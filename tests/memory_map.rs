//! Address-decode tests.
//!
//! A peripheral window that is mapped too small, or shadowed by a window
//! decoded before it, does not fail loudly: the access falls through to the
//! catch-all stub, reads back 0 and the firmware carries on with a wrong
//! answer. `CoreCtl` was mapped 0x100 bytes when core 1's registers live at
//! `+0x800` (commit `7bd21a3`); the DMA and SDRAM-controller windows have the
//! same shape of overlap. These tests assert the decode directly, using
//! `Machine::stub_hits` as the "did this reach a real device" signal.

use rpi_virt_fw::bus::Bus;
use rpi_virt_fw::soc::bcm2711 as map;
use rpi_virt_fw::Machine;

fn machine() -> Machine {
    Machine::new(1024 * 1024)
}

/// Addresses that must reach a modelled device, with a note on why each one
/// matters. Offsets are chosen to be side-effect free on read.
const MODELLED: &[(&str, u32)] = &[
    ("mcsync", map::MCSYNC_BASE),
    ("sdc", map::SDC_BASE),
    ("corectl core 0", map::CORECTL_BASE),
    ("corectl core 0 irq priority", map::CORECTL_BASE + 0x10),
    ("corectl core 1 irq priority", map::CORECTL_BASE + 0x810),
    ("corectl core 1 vector base", map::CORECTL_BASE + 0x830),
    ("corectl core 1 pending", map::CORECTL_BASE + 0x840),
    ("systimer", map::SYSTIMER_BASE),
    ("legacy dma ch0", map::DMA_LEGACY_BASE),
    ("legacy dma ch14", map::DMA_LEGACY_BASE + 14 * 0x100),
    ("dma4", map::DMA4_BASE),
    ("pm", map::PM_BASE),
    ("clock manager", map::CM_BASE),
    ("clock manager top", map::CM_BASE + map::CM_SIZE - 4),
    ("uart0", map::UART0_BASE),
    ("spi0", map::SPI0_BASE),
    ("bsc pmic", map::BSC_PMIC_BASE),
    ("config/otp fifo", map::OTP_BASE),
    ("aux mini-uart", map::AUX_BASE + 0x40),
    ("emmc CLOCK_CONTROL", map::EMMC_BASE + 0x2C),
    ("emmc2", map::EMMC2_BASE),
    ("hvs", map::HVS_BASE),
    ("usb power acknowledge", map::USBR_BASE + 0x20),
    ("dwc2 GRSTCTL", map::DWC2_BASE + 0x10),
    ("xhci_otg HCSPARAMS1", map::XHCI_OTG_BASE + 0x04),
    ("xhci_otg PORTSC", map::XHCI_OTG_BASE + 0x420),
    ("bootbox", map::BOOTBOX_BASE),
    ("vpu dma ch15", map::DMA_VPU_BASE + 0xF00),
    ("sdram controller", map::SDRAMC_BASE),
    ("sdram phy lane 3", map::SDRAMC_BASE + 0x30000),
    ("gpio", map::GPIO_BASE),
    ("gpio PUP_PDN3", map::GPIO_BASE + 0xF0),
    ("clkmon", map::CLKMON_BASE),
    ("avs", map::AVS_BASE),
];

#[test]
fn modelled_windows_do_not_fall_through_to_the_stub() {
    let mut m = machine();
    for (name, addr) in MODELLED {
        let before = m.stub_hits;
        m.load32(*addr)
            .unwrap_or_else(|e| panic!("{name} ({addr:#x}): {e}"));
        assert_eq!(
            m.stub_hits, before,
            "{name} ({addr:#x}) fell through to the peripheral stub"
        );
    }
}

/// The stub is still reachable — it is how unmodelled blocks stay harmless, and
/// `stub_hits` is only a useful health signal if it really counts them.
#[test]
fn unmodelled_peripherals_still_reach_the_stub() {
    let mut m = machine();
    let before = m.stub_hits;
    // SMI (`0x7E60_0000`): in the window, in the device tree, and nothing
    // models it. PWM used to be the example here, until #131 modelled it.
    m.load32(0x7E60_0000).unwrap();
    assert_eq!(m.stub_hits, before + 1, "the SMI is not modelled");
}

/// The AVS monitor is carved out of the middle of the VPU clock-block window,
/// so it only answers if it is decoded first. Same class of bug as a too-small
/// window: the access lands somewhere plausible and reads back the wrong thing.
#[test]
fn avs_is_carved_out_of_the_clkmon_window() {
    // The AVS window has to sit inside the clkmon one for this test to mean
    // anything.
    const _: () = assert!(
        map::AVS_BASE > map::CLKMON_BASE
            && map::AVS_BASE + map::AVS_SIZE <= map::CLKMON_BASE + map::CLKMON_SIZE
    );

    let mut m = machine();
    // Channel 0 is the temperature sensor: bit 10 valid, bit 16 settled,
    // bits [9:0] the count. start4 spins until both bits are set, so a window
    // answered by the clock block (or the stub) parks the DVFS code forever.
    let temp = m.load32(map::AVS_BASE + 0x200).unwrap();
    assert_ne!(temp & (1 << 10), 0, "temperature reading not valid");
    assert_ne!(temp & (1 << 16), 0, "temperature reading not settled");
    // 410040 - 487 * count, in millidegrees — a plausible idle Pi 4.
    let milli_c = 410040 - 487 * (temp & 0x3FF) as i32;
    assert!(
        (20_000..70_000).contains(&milli_c),
        "implausible die temperature {milli_c} m°C"
    );

    // The per-rail monitors: start4 skips every channel reading back 0 and
    // gives up on the voltage calculation if they all do.
    for ch in 0..0x18u32 {
        let rail = m.load32(map::AVS_BASE + 0x220 + 4 * ch).unwrap();
        assert_ne!(rail & 0x7FFF, 0, "rail monitor {ch} reads as absent");
        assert_ne!(rail & (1 << 16), 0, "rail monitor {ch} never settles");
    }
}

/// The LPDDR4 controller sits *below* the `0x7E00_0000` peripheral window, at an
/// address that folds onto DRAM under the VC4 cache-alias mask. If it is not
/// decoded before that fold, every training write vanishes into RAM.
#[test]
fn the_sdram_controller_is_not_folded_onto_dram() {
    let mut m = machine();
    for addr in [
        map::SDRAMC_BASE + 0x10,
        map::SDRAMC_BASE + 0x400,
        map::CLKMON_BASE + 0x40,
    ] {
        let before = (m.mmio_writes, m.stub_hits);
        m.store32(addr, 0xDEAD_BEEF).unwrap();
        assert_eq!(
            m.mmio_writes,
            before.0 + 1,
            "the write to {addr:#x} was folded onto DRAM at {:#x}",
            addr & 0x3FFF_FFFF
        );
        assert_eq!(m.stub_hits, before.1, "{addr:#x} reached only the stub");
    }
}

/// RAM is reachable through all four VC4 cache aliases and they must share one
/// backing store — the firmware writes a structure through the uncached alias
/// and reads it back through the cached one constantly.
#[test]
fn ram_aliases_share_one_backing_store() {
    let mut m = machine();
    m.store32(0x0002_0000, 0x1234_5678).unwrap();
    for alias in [0x0000_0000u32, 0x4000_0000, 0x8000_0000, 0xC000_0000] {
        assert_eq!(
            m.load32(alias + 0x0002_0000).unwrap(),
            0x1234_5678,
            "alias {alias:#x} is not the same memory"
        );
    }
}
