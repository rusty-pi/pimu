//! Peripheral-model tests.
//!
//! The recurring failure mode in this project is not a peripheral that behaves
//! subtly wrong — it is a register that reads back 0. Firmware reads 0 from an
//! identification or status register as "this chip is absent" and silently
//! skips a whole subsystem, and the symptom surfaces hundreds of millions of
//! instructions later as a `bl <null>`. These tests pin the register values
//! that the boot is known to gate on.

use rpi_virt_fw::bus::{Bus, MmioDevice, Width};
use rpi_virt_fw::periph::Spi0;
use rpi_virt_fw::soc::bcm2711 as map;
use rpi_virt_fw::soc::{Board, Stepping};
use rpi_virt_fw::Machine;

fn machine() -> Machine {
    Machine::new(1024 * 1024)
}

/// `SCALER_DISPID` at `0x7E40_0008`. start4's display bring-up compares it
/// against `0x64647276` and bails out immediately otherwise, which skips
/// `hdmi_init` and leaves the HDMI provider array null (commit `b21bc55`, #13).
/// The value is read off a running Pi 4's `hvs_regs`, not guessed.
#[test]
fn scaler_dispid_identifies_the_hvs() {
    let mut m = machine();
    assert_eq!(m.load32(map::HVS_BASE + 0x08).unwrap(), 0x6464_7276);

    // Read-only: the firmware writes all over this block.
    m.store32(map::HVS_BASE + 0x08, 0).unwrap();
    assert_eq!(
        m.load32(map::HVS_BASE + 0x08).unwrap(),
        0x6464_7276,
        "SCALER_DISPID must not be clobberable"
    );
}

/// The HVS frame-swap wait polls `current` (`+0x30 + 4*chan`) until it matches
/// `requested` (`+0x20 + 4*chan`). With no display behind it, `current` has to
/// mirror `requested` or every swap burns its full 100 x 1 ms timeout.
#[test]
fn hvs_frame_swap_completes_immediately() {
    let mut m = machine();
    for chan in 0..4u32 {
        m.store32(map::HVS_BASE + 0x20 + 4 * chan, 0x1234 + chan)
            .unwrap();
        assert_eq!(
            m.load32(map::HVS_BASE + 0x30 + 4 * chan).unwrap(),
            0x1234 + chan,
            "channel {chan} swap never reports complete"
        );
    }
}

/// start4's USB power-on (#49): request power at `0x7E80_8008` bit 2, wait for
/// both acknowledge bits at `+0x20`, then reset the DWC2 core and flush its
/// FIFOs, spinning on each `GRSTCTL` bit. None of those waits has a timeout, so
/// a wrong answer parks the `SET_POWER_STATE` handler for good and every later
/// property request goes unanswered. The values are what a
/// Raspberry Pi 4B d03115 reads before and after the same request.
#[test]
fn usb_power_on_handshake_completes() {
    let mut m = machine();
    let (ctrl, status) = (map::HD_BASE + 0x08, map::HD_BASE + 0x20);
    assert_eq!(m.load32(ctrl).unwrap(), 0x3);
    assert_eq!(m.load32(status).unwrap(), 0x0);

    let idle = m.load32(ctrl).unwrap();
    m.store32(ctrl, idle | 4).unwrap();
    assert_eq!(m.load32(status).unwrap(), 0x3, "power never acknowledged");
    m.store32(ctrl, idle).unwrap();
    assert_eq!(m.load32(status).unwrap(), 0x0);

    let grstctl = map::DWC2_BASE + 0x10;
    for (write, bit) in [(0x1, 0x1), (0x2, 0x2), (0x420, 0x20), (0x10, 0x10)] {
        m.store32(grstctl, write).unwrap();
        assert_eq!(
            m.load32(grstctl).unwrap() & bit,
            0,
            "GRSTCTL {write:#x} never completes"
        );
    }
    assert_eq!(m.load32(grstctl).unwrap(), 0x8000_0000);
}

/// `GSNPSID` names the core: OTG 2.80a on the reference board. Linux's dwc2
/// refuses a core whose id lacks the `0x4F54` prefix, and UEFI's
/// `DwUsbHostDxe` sizes its channel loops from `GHWCFG2`, where 0 reads as one
/// channel. The board has eight.
#[test]
fn dwc2_identifies_itself() {
    let mut m = machine();
    assert_eq!(m.load32(map::DWC2_BASE + 0x40).unwrap(), 0x4F54_280A);
    let hwcfg2 = m.load32(map::DWC2_BASE + 0x48).unwrap();
    assert_eq!((hwcfg2 >> 14 & 0xF) + 1, 8);
}

/// What UEFI's `DwUsbHostDxe` does once USB has power (#49): halt every host
/// channel and wait up to ten seconds, polled, for `CHENA` to clear; check
/// `GINTSTS.CURMOD` before powering the root port; then read the port status.
/// Storage that kept `CHENA` set cost ten guest seconds per channel.
#[test]
fn dwc2_host_channels_halt_and_the_port_is_empty() {
    let mut m = machine();
    let base = map::DWC2_BASE;
    m.store32(base + 0x0C, 0x2040_2700).unwrap();
    assert_eq!(m.load32(base + 0x14).unwrap() & 1, 1, "not in host mode");

    for ch in 0..8 {
        let (hcchar, hcint) = (base + 0x500 + ch * 0x20, base + 0x508 + ch * 0x20);
        m.store32(hcchar, 0x4000_0000).unwrap();
        m.store32(hcchar, 0xC000_0000).unwrap();
        assert_eq!(
            m.load32(hcchar).unwrap() & 0x8000_0000,
            0,
            "channel {ch} never halts"
        );
        assert_eq!(m.load32(hcint).unwrap(), 0x2, "no CHHLTD on channel {ch}");
        m.store32(hcint, 0x2).unwrap();
        assert_eq!(m.load32(hcint).unwrap(), 0);
    }

    // Port power sticks; nothing is connected, and writing the change bits or
    // PRTENA does not make it look otherwise.
    m.store32(base + 0x440, 0x1000 | 0x2F).unwrap();
    assert_eq!(m.load32(base + 0x440).unwrap(), 0x1000);

    m.store32(base + 0x0C, 0x4040_2700).unwrap();
    assert_eq!(m.load32(base + 0x14).unwrap() & 1, 0, "forced device mode");
}

const CS: u32 = 0x00;
const FIFO: u32 = 0x04;
const CS_TA: u32 = 1 << 7;
const CS_CLEAR_RX: u32 = 1 << 5;
const CS_CLEAR_TX: u32 = 1 << 4;
const CS_DONE: u32 = 1 << 16;
const CS_RXD: u32 = 1 << 17;

/// SPI0 `CS.DONE` reflects the TX side only. Gating it on an empty RX FIFO
/// broke start4's EEPROM section scanner, which clocks a block and then checks
/// `DONE` while the received bytes are still queued — reading `DONE = 0` there
/// is a transfer error to the firmware, and the whole scan failed.
#[test]
fn spi0_done_is_tx_side_only() {
    let mut spi = Spi0::new();
    spi.attach_flash(vec![0xAA; 0x1000]);

    spi.write(CS, Width::Word, CS_TA | CS_CLEAR_RX | CS_CLEAR_TX)
        .unwrap();
    for byte in [0x03, 0x00, 0x00, 0x10, 0xFF] {
        spi.write(FIFO, Width::Word, byte).unwrap();
    }

    let cs = spi.read(CS, Width::Word).unwrap();
    assert_ne!(cs & CS_RXD, 0, "received bytes are waiting");
    assert_ne!(
        cs & CS_DONE,
        0,
        "DONE must be set with the transfer active, RX FIFO or not"
    );

    // And the read command actually returns flash contents.
    for _ in 0..4 {
        spi.read(FIFO, Width::Word).unwrap(); // command + address echoes
    }
    assert_eq!(spi.read(FIFO, Width::Word).unwrap(), 0xAA);
}

/// Core 1's copies of the core-control registers live at `+0x800`. The window
/// was mapped 0x100 bytes wide, so every one of them fell through to the
/// catch-all peripheral stub and core 1 looked like it enabled no interrupts at
/// all (commit `7bd21a3`).
#[test]
fn core1_registers_are_inside_the_corectl_window() {
    // The window must cover core 1's block at +0x800.
    const _: () = assert!(map::CORECTL_SIZE > 0x844);

    let mut m = machine();
    let before = m.stub_hits;
    for off in [0x800, 0x810, 0x81C, 0x830, 0x840, 0x844] {
        m.store32(map::CORECTL_BASE + off, 0x0001_4000).unwrap();
        m.load32(map::CORECTL_BASE + off).unwrap();
        assert_eq!(
            m.stub_hits, before,
            "core 1's register +{off:#x} fell through to the peripheral stub"
        );
    }

    // Core 0's vector base is the one register whose value the model keeps.
    m.store32(map::CORECTL_BASE + 0x30, 0x0002_8000).unwrap();
    assert_eq!(m.corectl.vbase[0], 0x0002_8000);
}

/// Core 1's vector base is one 0x800 stride above core 0's, like every other
/// register in this block. A boot trace of the core-control writes shows the
/// pair `+0x30` and `+0x830` taking the same base (`0xFEC01E00`) and nothing
/// ever writing `+0x38`; the old `+0x38` guess left `vbase[1]` at 0, so core 1
/// could never be vectored (commit `06a8447`).
#[test]
fn core1_vector_base_is_recorded_from_the_strided_offset() {
    let mut m = machine();

    m.store32(map::CORECTL_BASE + 0x830, 0xFEC0_1E00).unwrap();
    assert_eq!(m.corectl.vbase[1], 0xFEC0_1E00);

    // `+0x38` is not the vector base and must not be mistaken for it.
    let mut m = machine();
    m.store32(map::CORECTL_BASE + 0x38, 0xFEC0_1E00).unwrap();
    assert_eq!(m.corectl.vbase[1], 0);
}

/// start4 raises an interrupt on a core by setting its bit in that core's
/// pending word (`+0x40` for sources 64..95, `+0x840` for core 1). Dropping
/// those writes silently starves every software-posted interrupt — the clock
/// service re-posts its own source 66 that way on every run.
#[test]
fn software_posted_interrupts_are_queued_per_core() {
    let mut m = machine();

    m.store32(map::CORECTL_BASE + 0x40, 1 << 2).unwrap(); // source 64 + 2
    m.store32(map::CORECTL_BASE + 0x840, 1 << 15).unwrap(); // core 1, source 79

    assert_eq!(m.corectl.take_sw_raised(), Some((0, 66)));
    assert_eq!(m.corectl.take_sw_raised(), Some((1, 79)));
    assert_eq!(m.corectl.take_sw_raised(), None);

    // Clearing a bit is the ISR's acknowledge, not a new interrupt.
    m.store32(map::CORECTL_BASE + 0x40, 0).unwrap();
    assert_eq!(m.corectl.take_sw_raised(), None);
}

/// Each bank has its own `IRQ_PENDING` (`+0x04` / `+0x804`): start4's
/// dispatcher runs on both cores and reads it through its own bank pointer.
/// It reads `VALID` (bit 8) with the source minus 64, then 0 (#80).
#[test]
fn irq_pending_is_per_core_and_read_to_clear() {
    let mut m = machine();

    m.corectl.raise_source(1, 79);
    assert_eq!(
        m.load32(map::CORECTL_BASE + 0x04).unwrap(),
        0,
        "core 0's copy"
    );
    assert_eq!(m.load32(map::CORECTL_BASE + 0x804).unwrap(), 0x100 | 15);
    assert_eq!(
        m.load32(map::CORECTL_BASE + 0x804).unwrap(),
        0,
        "read-to-clear"
    );

    m.corectl.raise_source(0, 66);
    assert_eq!(
        m.load32(map::CORECTL_BASE + 0x804).unwrap(),
        0,
        "core 1's copy"
    );
    assert_eq!(m.load32(map::CORECTL_BASE + 0x04).unwrap(), 0x100 | 2);
}

/// `enable_irq_source(src, prio)` packs a 4-bit field per source into the words
/// at `+0x10`: `word = (src >> 3) & 3`, `field = (src & 7) * 4`. The tick only
/// gets delivered if that decode round-trips.
#[test]
fn interrupt_priority_fields_round_trip() {
    let mut m = machine();

    // enable_irq_source(64, 1) and enable_irq_source(66, 3).
    m.store32(map::CORECTL_BASE + 0x10, 1 | (3 << 8)).unwrap();

    assert_eq!(m.corectl.irq_priority(0, 64), 1);
    assert_eq!(m.corectl.irq_priority(0, 66), 3);
    assert_eq!(m.corectl.irq_priority(0, 65), 0, "unenabled source");
    assert_eq!(m.corectl.irq_priority(0, 72), 0, "next word along");

    // Core 1's bank is 0x800 up, and its own: enable_irq_source(79, 1) there.
    m.store32(map::CORECTL_BASE + 0x814, 1 << 28).unwrap();
    assert_eq!(m.corectl.irq_priority(1, 79), 1);
    assert_eq!(m.corectl.irq_priority(0, 79), 0, "core 0's copy");
}

// ---------------------------------------------------------------------------
// Board PMICs on the BSC at 0x7E20_5E00 (#4).
// ---------------------------------------------------------------------------

const BSC_C: u32 = 0x00;
const BSC_S: u32 = 0x04;
const BSC_DLEN: u32 = 0x08;
const BSC_A: u32 = 0x0C;
const BSC_FIFO: u32 = 0x10;

const C_READ: u32 = 1 << 0;
const C_ST: u32 = 1 << 7;
const C_I2CEN: u32 = 1 << 15;
const S_DONE: u32 = 1 << 1;
const S_ERR: u32 = 1 << 8;

/// Let a transfer settle: `S.DONE` only lands once the bytes have had time to
/// clock out at the bus speed `DIV` asks for. 10 ms of simulated time is more
/// than any transfer here needs.
fn settle(m: &mut Machine) {
    m.tick(10_000 * 54); // 54 VPU cycles per microsecond
}

/// `read(reg)` the way start4's BSC transport does it when `cfg[8] & 2` is set
/// (the `0x1B` path): one-byte write phase, FIFO fed straight after `ST`, then
/// a separate read phase.
fn pmic_read(m: &mut Machine, addr: u8, reg: u8) -> u8 {
    let base = map::BSC_PMIC_BASE;
    m.store32(base + BSC_A, addr as u32).unwrap();
    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST).unwrap();
    m.store32(base + BSC_FIFO, reg as u32).unwrap();
    settle(m);
    m.store32(base + BSC_S, S_DONE | S_ERR).unwrap();

    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST | C_READ).unwrap();
    settle(m);
    let v = m.load32(base + BSC_FIFO).unwrap() as u8;
    m.store32(base + BSC_S, S_DONE | S_ERR).unwrap();
    v
}

/// The same read with the read phase programmed *before* the register byte is
/// pushed. Arming the read switches the FIFO to the receive path, so the byte
/// pushed afterwards is never a register select — it comes back as the read's
/// data.
fn pmic_read_late_fifo(m: &mut Machine, addr: u8, reg: u8) -> u8 {
    let base = map::BSC_PMIC_BASE;
    m.store32(base + BSC_A, addr as u32).unwrap();
    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST).unwrap();
    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST | C_READ).unwrap();
    m.store32(base + BSC_FIFO, reg as u32).unwrap();
    settle(m);
    let v = m.load32(base + BSC_FIFO).unwrap() as u8;
    m.store32(base + BSC_S, S_DONE | S_ERR).unwrap();
    v
}

fn pmic_write(m: &mut Machine, addr: u8, reg: u8, value: u8) {
    let base = map::BSC_PMIC_BASE;
    m.store32(base + BSC_A, addr as u32).unwrap();
    m.store32(base + BSC_DLEN, 2).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST).unwrap();
    m.store32(base + BSC_FIFO, reg as u32).unwrap();
    m.store32(base + BSC_FIFO, value as u32).unwrap();
    settle(m);
    m.store32(base + BSC_S, S_DONE | S_ERR).unwrap();
}

/// A register byte pushed after the read is armed is read straight back, and a
/// register byte written out first selects the register. start4's transport
/// (`0x3ECF0ED0`) writes `C.ST` before it feeds the FIFO, which is fine on its
/// own — the write stalls with `S.TA` until the byte lands — but arming the
/// read on top of that stalled write turns the FIFO into the receive path.
///
/// Measured on a 4B rev 1.5: every PMIC register answered with its own number
/// until `rpi-unboxed` 65a4b8c/f20137b wrote the register out and waited for
/// `DONE` before arming the read. 0x1B reg 0x09 then read 40, which
/// `vcgencmd measure_volts sdram_c` confirms as 1.1 V.
#[test]
fn an_armed_read_returns_the_register_byte_not_the_register() {
    let mut m = machine();
    // 0x1B reg 0x09 is the SDRAM rail setpoint, shared by rails 2, 3 and 4.
    assert_eq!(pmic_read(&mut m, 0x1B, 0x09), 40);
    assert_eq!(pmic_read_late_fifo(&mut m, 0x1B, 0x09), 0x09);
    // 0x1E reg 0x25 is the SoC core rail setpoint.
    assert_eq!(pmic_read_late_fifo(&mut m, 0x1E, 0x25), 0x25);
    assert_eq!(pmic_read(&mut m, 0x1E, 0x25), 85);
}

/// Seeded setpoints must decode, through the firmware's own conversion, to the
/// voltages a real d03115 reports.
#[test]
fn pmic_setpoints_decode_to_the_real_boards_voltages() {
    let mut m = machine();

    // 0x1B rails 2..4 (`0x3EC8C710`): raw * 5_000 + 900_000 µV.
    let sdram = pmic_read(&mut m, 0x1B, 0x09) as u32 * 5_000 + 900_000;
    assert_eq!(
        sdram, 1_100_000,
        "vcgencmd measure_volts sdram_c on a Raspberry Pi 4B d03115"
    );

    // 0x1E rail 1 (`0x3EC8C9F6`): raw * 10_000 µV, within the descriptor's
    // 0.3 V..1.9 V range.
    let core = pmic_read(&mut m, 0x1E, 0x25) as u32 * 10_000;
    assert!(
        (300_000..=1_900_000).contains(&core),
        "core setpoint {core} outside the descriptor range"
    );
}

/// A setpoint write is read back, and latches the "voltage settled" bit each
/// part's post-set callback polls: `0x1B` reg 0x00 bit 4 (`0x3EC8C746`) and
/// `0x1E` reg 0x02 bit 3 (`0x3EC8C9FC`). Those loops have no timeout, so a bit
/// that never sets hangs the boot outright.
#[test]
fn pmic_setpoint_write_latches_the_settled_bit() {
    let mut m = machine();

    // 0x6E = 110 -> 1.10 V, the top of the band this boot's DVFS actually uses.
    pmic_write(&mut m, 0x1E, 0x25, 0x6E);
    assert_eq!(pmic_read(&mut m, 0x1E, 0x25), 0x6E);
    assert_ne!(pmic_read(&mut m, 0x1E, 0x02) & 0x08, 0, "0x1E settled bit");

    pmic_write(&mut m, 0x1B, 0x09, 40);
    assert_ne!(pmic_read(&mut m, 0x1B, 0x00) & 0x10, 0, "0x1B settled bit");
}

/// The two parts are separate register files even though they share a bus: a
/// write to one must not show up in the other.
#[test]
fn pmic_addresses_are_separate_register_files() {
    let mut m = machine();
    pmic_write(&mut m, 0x1E, 0x40, 0x5A);
    assert_eq!(pmic_read(&mut m, 0x1E, 0x40), 0x5A);
    assert_eq!(pmic_read(&mut m, 0x1B, 0x40), 0x00);
}

/// A 4B up to rev 1.4 has one PMIC, at `0x1D`, in place of rev 1.5's pair
/// (#78). Its setpoints decode, through `0x3EDD259A`, to the same voltages;
/// its settled bit is reg `0x1A` bit 4 (`0x3EDD25AE`); it passes the check
/// `pmic_get_voltage` makes of it, `0x0F == 0x14 ^ 0xAD`; and its status poll
/// (`0x3EDD23F4`) reads good input power, `0x1A & 0x60 == 0x20`.
#[test]
fn a_rev_1_2_board_has_the_one_0x1d_pmic() {
    let mut m = machine();
    m.set_board(Board::for_stepping(Stepping::B0));
    let pmic = m.bsc_pmic.slave().unwrap();
    assert!(pmic.responds_to(0x1D));
    assert!(!pmic.responds_to(0x1B) && !pmic.responds_to(0x1E));

    assert_eq!(
        pmic_read(&mut m, 0x1D, 0x1A) & 0x60,
        0x20,
        "0x1D power good"
    );
    assert_eq!(pmic_read(&mut m, 0x1D, 0x13) as u32 * 6_250, 1_100_000);
    let core = pmic_read(&mut m, 0x1D, 0x14);
    assert_eq!(pmic_read(&mut m, 0x1D, 0x0F), core ^ 0xAD);

    pmic_write(&mut m, 0x1D, 0x14, 0x90);
    assert_eq!(pmic_read(&mut m, 0x1D, 0x14), 0x90);
    assert_ne!(pmic_read(&mut m, 0x1D, 0x1A) & 0x10, 0, "0x1D settled bit");
    assert_eq!(
        m.bsc_pmic.slave().unwrap().core_rail_uv(),
        Some(0x90 * 6_250)
    );
}

/// The FXL6408 GPIO expander shares the bus at `0x43`; start4 probes it by
/// reading register `0x01` (any value will do for the firmware, but it is the
/// Fairchild manufacturer id on the real part).
#[test]
fn gpio_expander_answers_on_the_pmic_bus() {
    let mut m = machine();
    assert_eq!(pmic_read(&mut m, 0x43, 0x01) >> 5, 0b101);
}

/// Nothing else is on this bus. start4 probes a handful of other addresses on
/// it — `0x10`, where its other expander driver (a GreenPAK) looks, among
/// them; an unACKed transfer has to complete `DONE | ERR`, not spin.
#[test]
fn pmic_bus_nacks_every_other_address() {
    let mut m = machine();
    let base = map::BSC_PMIC_BASE;
    m.store32(base + BSC_A, 0x10).unwrap();
    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST | C_READ).unwrap();
    settle(&mut m);
    let s = m.load32(base + BSC_S).unwrap();
    assert_ne!(s & S_DONE, 0, "transfer must complete");
    assert_ne!(s & S_ERR, 0, "unACKed address must raise ERR");
}

/// `S.TA` spans the transfer, `S.DONE` lands only at the end of it, and
/// neither is a function of how many instructions the firmware happens to
/// retire in between. start4's transport (`0x3ECF0ED0`) spins on
/// `S & (TA | ERR)` with **no timeout** between writing `C.ST` and pushing the
/// data byte (#17), so a `TA` that lasts a fixed number of ticks — 96, as it
/// was — can lapse before the firmware ever reads `S` and hang the boot for
/// good. Real hardware holds `TA` from `ST` until the last bit is clocked.
#[test]
fn bsc_transfer_active_spans_the_whole_transfer() {
    let mut m = machine();
    let base = map::BSC_PMIC_BASE;
    const S_TA: u32 = 1 << 0;

    m.store32(base + BSC_A, 0x1E).unwrap();
    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST).unwrap();

    // Stalled on an empty FIFO: active, and staying active however long the
    // firmware takes to get round to it — 10 ms here, ~540 000 instructions.
    for _ in 0..1000 {
        m.tick(10 * 54);
        let s = m.load32(base + BSC_S).unwrap();
        assert_ne!(s & S_TA, 0, "TA must be held while the FIFO is empty");
        assert_eq!(s & S_DONE, 0, "nothing has been transferred yet");
    }

    // Feed it: the byte is still on the wire, so the transfer is not over.
    m.store32(base + BSC_FIFO, 0x25).unwrap();
    let s = m.load32(base + BSC_S).unwrap();
    assert_ne!(s & S_TA, 0, "the last byte is still clocking out");
    assert_eq!(s & S_DONE, 0, "DONE must not land inside the FIFO write");

    // At 100 kHz (DIV defaults to 5000 against the 500 MHz core clock) that
    // byte takes 90 µs. Well short of it, nothing has changed.
    m.tick(50 * 54);
    assert_eq!(
        m.load32(base + BSC_S).unwrap() & S_DONE,
        0,
        "DONE cannot precede the bits going out"
    );

    settle(&mut m);
    let s = m.load32(base + BSC_S).unwrap();
    assert_eq!(s & S_TA, 0, "transfer over");
    assert_ne!(s & S_DONE, 0, "DONE latches at the end");
    assert_eq!(s & S_ERR, 0, "the PMIC acknowledged its address");
    assert_eq!(m.load32(base + BSC_DLEN).unwrap(), 0, "all bytes sent");
}

/// The VCE launch handshake, as `vce_run_start` / `vce_run_complete` /
/// `vce_clear_interrupt` drive it (`vcfw/drivers/chip/vciv/2708/vce.c`).
///
/// With the block unmapped, `STATUS` read back 0 forever and the boot stalled
/// one step short of `arm_loader`, printing "VCE taking >1s to run".
#[test]
fn vce_launch_completes_and_raises_its_interrupt() {
    use rpi_virt_fw::periph::vce;

    let mut m = machine();
    let ctrl = map::VCE_CTRL_BASE;

    // Idle: no interrupt pending, and the two bits `vce_obtain_semaphore` in
    // start4db asserts are clear must read clear.
    assert_eq!(m.load32(ctrl).unwrap(), 0);
    assert!(!m.vce.irq_asserted());

    // A launch the way `vce_run_start` does it for the codec licence check:
    // flags 0xC0000000, so the endcode field is 0 and ENDCODE_ENABLE is not
    // written at all.
    m.store32(ctrl + 0x08, 0x1234).unwrap(); // start pc
    m.store32(ctrl + 0x24, 0xFF).unwrap(); // INTCLR
    m.store32(ctrl + 0x20, 1).unwrap(); // RUN

    let status = m.load32(ctrl).unwrap();
    assert_eq!(
        status >> 16 & 0x1F,
        0,
        "vce_run_complete requires the endcode it asked for"
    );
    assert_ne!(status & (1 << 31), 0, "completion must flag an interrupt");
    assert_eq!(
        m.load32(ctrl + 0x30).unwrap(),
        0,
        "BAD_ADDR must stay clear"
    );
    assert_eq!(m.load32(ctrl + 0x08).unwrap(), 0x1234, "PC0 reads back");
    assert!(
        m.vce.irq_asserted(),
        "source {} has to be driven or the event flag is never set",
        vce::IRQ_SRC
    );

    // `vce_clear_interrupt` writes bit 31 and then asserts it reads back clear.
    m.store32(ctrl + 0x24, 0x8000_0000).unwrap();
    assert_eq!(m.load32(ctrl).unwrap() & (1 << 31), 0);
    assert!(!m.vce.irq_asserted());
    // The endcode survives the ack: `vce_run_complete` reads it afterwards.
    assert_eq!(m.load32(ctrl).unwrap() >> 16 & 0x1F, 0);
}

/// A launch that asks for a non-zero endcode has to get that endcode back, or
/// `vce_run_complete` logs "unexpected endcode" and retries forever. The only
/// record of what was asked for is `ENDCODE_ENABLE`, which `vce_run_start`
/// writes as `(1 << endcode) | 0x20` — bit 5 being the clock-stall condition
/// the interrupt handler services itself.
#[test]
fn vce_reports_the_endcode_the_launch_armed() {
    let mut m = machine();
    let ctrl = map::VCE_CTRL_BASE;

    m.store32(ctrl + 0x24, 0xFF).unwrap();
    m.store32(ctrl + 0x28, (1 << 3) | 0x20).unwrap();
    m.store32(ctrl + 0x20, 1).unwrap();
    assert_eq!(m.load32(ctrl).unwrap() >> 16 & 0x1F, 3);

    // The next launch does not re-arm the mask, so it means endcode 0 again —
    // a stale mask must not leak into it.
    m.store32(ctrl + 0x24, 0xFF).unwrap();
    m.store32(ctrl + 0x20, 1).unwrap();
    assert_eq!(m.load32(ctrl).unwrap() >> 16 & 0x1F, 0);
}

/// The codec licence check reads its answer out of VCE register 2. The compute
/// core is not emulated, so a completed run leaves the register file zeroed,
/// which is "this key does not match": the reference Pi 4 has OTP rows 45 and 46
/// blank and reports `MPG2=disabled` / `WVC1=disabled`. Before the run the
/// register file is plain storage — `vce_launch_prerun` sets register 0 and
/// asserts it reads back.
#[test]
fn vce_register_file_round_trips_but_a_run_consumes_it() {
    let mut m = machine();
    let regs = map::VCE_BASE + 0x2_0000;

    m.store32(regs + 4 * 2, 0x137A_FEDA ^ 0x4D50_4732).unwrap();
    assert_eq!(m.load32(regs + 4 * 2).unwrap(), 0x137A_FEDA ^ 0x4D50_4732);

    m.store32(map::VCE_CTRL_BASE + 0x20, 1).unwrap();
    assert_eq!(
        m.load32(regs + 4 * 2).unwrap(),
        0,
        "getreg(2) == 0 is 'licence key does not match', which is the measured \
         answer for a board with blank OTP rows 45/46"
    );
}

/// Program and data memory are real storage: `vce_loadprogram` memcpys into
/// `0x7F11_0000` and `vce_launch_complete` copies results back out of
/// `0x7F10_0000`, a byte at a time when the host buffer is unaligned.
#[test]
fn vce_program_and_data_memory_are_writable() {
    let mut m = machine();
    m.store32(map::VCE_BASE + 0x1_0000, 0xDEAD_BEEF).unwrap();
    assert_eq!(m.load32(map::VCE_BASE + 0x1_0000).unwrap(), 0xDEAD_BEEF);

    m.store(map::VCE_BASE + 0x0104, Width::Byte, 0xA5).unwrap();
    assert_eq!(
        m.load(map::VCE_BASE + 0x0104, Width::Byte).unwrap(),
        0xA5,
        "data memory must take byte stores"
    );
    // Separate windows, not aliases of each other.
    assert_eq!(m.load32(map::VCE_BASE).unwrap(), 0);
}

/// The ASB bridge handshake at `0x7E00_A000`. start4's power-domain switch
/// (`FUN_0ED54E40`) sets `ASB_REQ_STOP` on both bridges of a pair and then
/// spins until each answers with `ASB_ACK`; the release path clears `REQ_STOP`
/// and spins until `ACK` goes away. With the block unmapped, `ACK` read back 0
/// forever and the boot hung at `0x3ED550A8` on the H264 bridge, just after
/// `uart: Baud rate change done`.
#[test]
fn asb_ack_follows_the_stop_request() {
    let mut m = machine();

    // Every `*_CTRL` register: V3D S/M, ISP S/M, H264 S/M.
    for off in [0x08, 0x0C, 0x10, 0x14, 0x18, 0x1C] {
        let reg = map::ASB_BASE + off;

        // Out of reset the bridge is running: no request, no acknowledge.
        let v = m.load32(reg).unwrap();
        assert_eq!(v & 0b11, 0, "{reg:#x} must come up running");

        // Stop: request, then the acknowledge the firmware polls for.
        let v = m.load32(reg).unwrap() | 1;
        m.store32(reg, v).unwrap();
        assert_ne!(
            m.load32(reg).unwrap() & 0b10,
            0,
            "{reg:#x} never acknowledged the stop request"
        );

        // Release: clear the request, and the acknowledge has to drop.
        let v = m.load32(reg).unwrap() & !1;
        m.store32(reg, v).unwrap();
        assert_eq!(
            m.load32(reg).unwrap() & 0b10,
            0,
            "{reg:#x} never dropped its acknowledge"
        );
    }
}

/// The bridges are independent: stopping the H264 pair must not report the ISP
/// or V3D pair as stopped, or a later release would spin on the wrong bridge.
#[test]
fn asb_bridges_are_independent() {
    let mut m = machine();

    m.store32(map::ASB_BASE + 0x18, 1).unwrap();
    assert_eq!(m.load32(map::ASB_BASE + 0x18).unwrap() & 0b11, 0b11);
    for off in [0x08, 0x0C, 0x10, 0x14, 0x1C] {
        assert_eq!(
            m.load32(map::ASB_BASE + off).unwrap() & 0b11,
            0,
            "bridge +{off:#x} moved with the H264 slave bridge"
        );
    }
}

/// `ASB_AXI_BRDG_ID` (`+0x20`) reads `"brdg"`. Linux's `bcm2835_power_probe`
/// refuses to bind when it does not, and this project's recurring failure mode
/// is an identification register that reads back 0.
#[test]
fn asb_identifies_itself_as_a_bridge() {
    let mut m = machine();
    assert_eq!(m.load32(map::ASB_BASE + 0x20).unwrap(), 0x6272_6467);
}

/// The PCIe root complex is decoded as MMIO, not folded onto DRAM at
/// `0x3D50_xxxx` — stage 0 of #18. The rest of the PCIe behaviour is tested
/// next to the model in `src/periph/pcie.rs`, where the reset sequence can be
/// driven directly.
#[test]
fn pcie_window_is_mmio_not_dram() {
    // Big enough that the DRAM alias of the PCIe window, `addr & 0x3FFF_FFFF`,
    // is backed memory — which is exactly what used to swallow these writes.
    let mut m = Machine::new(0x3D51_0000);
    m.store32(0x3D50_9210, 0xDEAD_BEEF).unwrap();
    m.store32(map::PCIE_BASE + 0x9210, 0x3).unwrap();
    assert_eq!(m.load32(0x3D50_9210).unwrap(), 0xDEAD_BEEF);
    assert_eq!(m.load32(map::PCIE_BASE + 0x9210).unwrap(), 0x3);
}

/// The VL805 is soldered to every Pi 4B, so the link trains as soon as the
/// bootloader releases PERST# and the config router answers for bus 1.
/// `MISC_PCIE_STATUS` then reads `0xB0` — `PHYLINKUP | DL_ACTIVE | port RC` —
/// which is what stops `PCIe timeout: 0x00000000` being printed.
#[test]
fn pcie_link_trains_and_finds_the_vl805() {
    let mut m = machine();
    // bootcode parks the block in reset; the bootloader releases bridge then
    // PERST#.
    m.store32(map::PCIE_BASE + 0x9210, 0x3).unwrap();
    m.store32(map::PCIE_BASE + 0x9210, 0x1).unwrap();
    m.store32(map::PCIE_BASE + 0x9210, 0x0).unwrap();
    assert_eq!(m.load32(map::PCIE_BASE + 0x4068).unwrap(), 0xB0);
    m.store32(map::PCIE_BASE + 0x9000, 1 << 20).unwrap();
    assert_eq!(m.load32(map::PCIE_BASE + 0x8000).unwrap(), 0x3483_1106);
}

/// The whole of the path the bootloader's `xHC0 ver:` line comes out of, driven
/// through the bus the way the firmware drives it: enumerate the endpoint,
/// program the outbound window, then read BAR0 with a 40-bit DMA4 transfer
/// (`0x0008B42C` builds the control block, `0x000A701E` reads the bounce
/// buffer). The source word is the one `--log dma` shows the real firmware
/// using — `src = 0x0200_0004`, `srci = 0x1006`, i.e. `0x6_0200_0004`.
///
/// Before the 40-bit address was honoured this read landed in DRAM, every
/// capability register came back 0, and the bring-up hung at `0x000AA3C0`.
#[test]
fn xhci_capability_registers_arrive_by_forty_bit_dma() {
    let mut m = machine();
    // Link up, then assign BAR0 and enable memory decoding (0x000A6918).
    m.store32(map::PCIE_BASE + 0x9210, 0x3).unwrap();
    m.store32(map::PCIE_BASE + 0x9210, 0x0).unwrap();
    m.store32(map::PCIE_BASE + 0x9000, 1 << 20).unwrap();
    m.store32(map::PCIE_BASE + 0x8010, 0x8200_0000).unwrap();
    m.store32(map::PCIE_BASE + 0x8014, 0).unwrap();
    m.store32(map::PCIE_BASE + 0x8004, 0x0146).unwrap();
    // CPU_2_PCIE_MEM_WIN0 (0x000A725C..0x000A72F0).
    m.store32(map::PCIE_BASE + 0x400C, 0x8000_0000).unwrap();
    m.store32(map::PCIE_BASE + 0x4010, 0).unwrap();
    m.store32(map::PCIE_BASE + 0x4070, 0x3FF0_0000).unwrap();
    m.store32(map::PCIE_BASE + 0x4080, 6).unwrap();
    m.store32(map::PCIE_BASE + 0x4084, 6).unwrap();

    let cb = 0x2_0000u32;
    let dst = 0x3_0000u32;
    for (off, w) in [
        (0x00, 0),           // TI
        (0x04, 0x0200_0004), // SRC low
        (0x08, 0x0000_1006), // SRC info: INC | address bits [39:32] = 6
        (0x0C, dst),         // DEST low
        (0x10, 0x0000_1000), // DEST info: INC, high bits 0
        (0x14, 4),           // LEN
        (0x18, 0),           // NEXT
    ] {
        m.store32(cb + off, w).unwrap();
    }
    m.store32(map::DMA4_BASE + 0x04, cb >> 5).unwrap();
    m.store32(map::DMA4_BASE, 1).unwrap(); // ACTIVE

    assert_eq!(
        m.load32(dst).unwrap(),
        0x0500_0420,
        "HCSPARAMS1 as measured on a Raspberry Pi 4B d03115: MaxSlots 32, MaxIntrs 4, MaxPorts 5"
    );
    // END set, ACTIVE and ERROR clear — what 0x0008B3F4 polls for.
    let cs = m.load32(map::DMA4_BASE).unwrap();
    assert_eq!(cs & 0x403, 0x2);
}

/// The PVT magic at `0x7D5D_8010 + ch*0x40`. `FUN_0ec300fa` reads `+0x1C` only
/// when this equals `0x7FFF50CF`, and returns zeros for both halves otherwise.
/// Every one of the eighteen channels carries it on a real Pi 4, read through
/// `/dev/mem` — the same class of trap as `SCALER_DISPID` above (#1 point 3),
/// so it is pinned per channel rather than only for channel 0.
#[test]
fn pvt_channels_all_carry_the_magic() {
    let mut m = machine();
    for ch in 0..18 {
        let base = map::PVT_BASE + ch * 0x40;
        assert_eq!(
            m.load32(base + 0x10).unwrap(),
            0x7FFF_50CF,
            "channel {ch} magic"
        );
        // Read-only: a stray write must not make the block look absent.
        m.store32(base + 0x10, 0).unwrap();
        assert_eq!(m.load32(base + 0x10).unwrap(), 0x7FFF_50CF);
        // And the channel reads back its own index at +0x00.
        assert_eq!(m.load32(base).unwrap(), ch, "channel {ch} index");
    }
}

/// `FUN_0ec300fa` splits `+0x1C` into two 16-bit halves and zeroes either half
/// that reads below 10, which skips the adaptive correction in `FUN_0ec303e8`.
/// The measured values clear that floor on every channel.
#[test]
fn pvt_readings_clear_the_firmwares_floor() {
    let mut m = machine();
    for ch in 0..18 {
        let v = m.load32(map::PVT_BASE + ch * 0x40 + 0x1C).unwrap();
        assert!(v >> 16 >= 10, "channel {ch} high half {:#x}", v >> 16);
        assert!(v & 0xFFFF >= 10, "channel {ch} low half {:#x}", v & 0xFFFF);
    }
}

/// `+0x14` / `+0x18` are writable thresholds, but `FUN_0ec30276` can read them
/// before anything has written them, so they are seeded from hardware.
#[test]
fn pvt_thresholds_are_seeded_then_writable() {
    let mut m = machine();
    assert_eq!(m.load32(map::PVT_BASE + 0x14).unwrap(), 0x0364_0340);
    assert_eq!(m.load32(map::PVT_BASE + 0x18).unwrap(), 0x0648_0624);

    m.store32(map::PVT_BASE + 0x14, 0x1234_5678).unwrap();
    assert_eq!(m.load32(map::PVT_BASE + 0x14).unwrap(), 0x1234_5678);
    // Channel 1 is unaffected by a write to channel 0.
    assert_eq!(m.load32(map::PVT_BASE + 0x40 + 0x14).unwrap(), 0x0364_0340);
}

/// The six AVS result channels each report their own count. Before this they
/// all answered with one of two values, so a rail read was indistinguishable
/// from a temperature read (#1 point 2).
#[test]
fn avs_channels_report_distinct_counts() {
    let mut m = machine();
    let counts: Vec<u32> = (0..6)
        .map(|ch| m.load32(map::AVS_BASE + 0x200 + ch * 4).unwrap() & 0x3FF)
        .collect();
    // Channel 3 is the core rail, and it reports whatever the PMIC is set to
    // rather than a fixed count: 692 decodes back to the 0.85 V the `0x1E`
    // setpoint is seeded with. The others are the measured constants.
    assert_eq!(counts, vec![752, 2, 669, 692, 2, 841]);

    // `FUN_0ed603e2` accepts a sample only with both bit 10 and bit 16 set.
    for ch in 0..6 {
        let v = m.load32(map::AVS_BASE + 0x200 + ch * 4).unwrap();
        assert!(v & (1 << 10) != 0 && v & (1 << 16) != 0, "channel {ch}");
    }
}

/// The AVS block is 36 channels wide at `+0x220`, not 24: `FUN_0ec5f2c0`
/// accepts a channel up to 0x23 and hands it straight to `FUN_0ec3007a`, which
/// spins ten times on a channel that does not report "settled". Channels
/// 0x20..0x23 settle with a count of 0 on hardware, which is not the same thing
/// as never settling.
#[test]
fn avs_rail_monitors_cover_every_channel_the_sensor_api_accepts() {
    let mut m = machine();
    for ch in 0..0x24u32 {
        let v = m.load32(map::AVS_BASE + 0x220 + ch * 4).unwrap();
        assert_ne!(v & (1 << 16), 0, "channel {ch:#x} must report settled");
    }
    assert_eq!(m.load32(map::AVS_BASE + 0x220).unwrap() & 0x7FFF, 0x07FD);
    assert_eq!(
        m.load32(map::AVS_BASE + 0x220 + 0x23 * 4).unwrap() & 0x7FFF,
        0
    );
}

/// Channel 3 measures the SoC core rail, so it has to follow the rail. start4's
/// DVFS calibration (`FUN_0ec303e8`) programs two voltages and gives up on the
/// whole scan unless the sensor reports at least 10 mV between them; a fixed
/// count reads as a rail that does not respond.
#[test]
fn avs_core_channel_follows_the_pmic_setpoint() {
    let mut m = machine();
    let count = |m: &mut Machine| m.load32(map::AVS_BASE + 0x200 + 3 * 4).unwrap() & 0x3FF;
    // start4 decodes the count as `(count * 100571) >> 13` tenths of a mV.
    let tenths = |c: u32| (c * 100_571) >> 13;

    pmic_write(&mut m, 0x1E, 0x25, 0x54); // 0.84 V
    let low = count(&mut m);
    pmic_write(&mut m, 0x1E, 0x25, 0x6E); // 1.10 V
    let high = count(&mut m);

    // One count is ~1.2 mV, so the round trip is exact to within a count.
    assert!(
        tenths(low).abs_diff(8400) <= 12,
        "0.84 V decoded back as {}",
        tenths(low)
    );
    assert!(
        tenths(high).abs_diff(11000) <= 12,
        "1.10 V decoded back as {}",
        tenths(high)
    );
    assert!(tenths(high) - tenths(low) >= 100, "rail must move >= 10 mV");
}

/// `+0x03C` is an active-high disable mask: `FUN_0ed6040e` writes
/// `~(1 << ch) & 0x7F` to leave only `ch` unmasked, and `0` to unmask
/// everything. A masked channel must not report a valid, settled sample.
#[test]
fn avs_disable_mask_gates_the_other_channels() {
    let mut m = machine();
    // Select channel 3 the way `FUN_0ed6040e` does.
    m.store32(map::AVS_BASE + 0x03C, !(1u32 << 3) & 0x7F)
        .unwrap();
    assert_eq!(
        m.load32(map::AVS_BASE + 0x200 + 3 * 4).unwrap() & 0x3FF,
        692
    );
    for ch in [0, 1, 2, 4, 5] {
        assert_eq!(
            m.load32(map::AVS_BASE + 0x200 + ch * 4).unwrap(),
            0,
            "channel {ch} should be masked off"
        );
    }

    // Restoring 0 unmasks everything, which is the state real hardware idles
    // in — all six channels were read live with this register at 0.
    m.store32(map::AVS_BASE + 0x03C, 0).unwrap();
    for ch in 0..6 {
        assert!(m.load32(map::AVS_BASE + 0x200 + ch * 4).unwrap() & (1 << 10) != 0);
    }
}

// ---------------------------------------------------------------------------
// HDMI DDC I²C masters (`0x7EF0_4500` / `0x7EF0_9500`), issue #15.
// ---------------------------------------------------------------------------

const DDC_CHIP_ADDRESS: u32 = 0x00;
const DDC_DATA_IN: u32 = 0x04;
const DDC_CNT: u32 = 0x24;
const DDC_CTL: u32 = 0x28;
const DDC_IIC_ENABLE: u32 = 0x2C;
const DDC_DATA_OUT: u32 = 0x30;
const DDC_CTLHI: u32 = 0x50;

const DDC_EN_ENABLE: u32 = 1 << 0;
const DDC_EN_INTRP: u32 = 1 << 1;
const DDC_EN_NOACK: u32 = 1 << 2;
const DDC_EN_NOSTOP: u32 = 1 << 4;
const DDC_EN_NOSTART: u32 = 1 << 5;
const DDC_EN_RESTART: u32 = 1 << 6;

/// Start a transfer the way start4's driver does (`0x3ECE69E2` /
/// `0x3ECE6DEC`): address, clear `CTLHI.IGNORE_ACK`, count, direction, then
/// `IIC_ENABLE` with `ENABLE | INTRP` and the start/stop flags for this chunk.
fn ddc_start(m: &mut Machine, base: u32, addr: u32, read: bool, count: u32, flags: u32) {
    m.store32(base + DDC_CHIP_ADDRESS, addr << 1 | u32::from(read))
        .unwrap();
    let ctlhi = m.load32(base + DDC_CTLHI).unwrap();
    m.store32(base + DDC_CTLHI, ctlhi & !2).unwrap();
    m.store32(base + DDC_CNT, count & 0x3F).unwrap();
    let ctl = m.load32(base + DDC_CTL).unwrap();
    m.store32(base + DDC_CTL, (ctl & !3) | u32::from(read))
        .unwrap();
    m.store32(base + DDC_IIC_ENABLE, flags | DDC_EN_ENABLE | DDC_EN_INTRP)
        .unwrap();
}

/// The driver's completion wait (`0x3ECE6D5C`): poll `INTRP`, then look at
/// `NOACK`. Returns the final `IIC_ENABLE`, or `None` on the 100 ms timeout.
fn ddc_wait(m: &mut Machine, base: u32) -> Option<u32> {
    for _ in 0..20 {
        let v = m.load32(base + DDC_IIC_ENABLE).unwrap();
        if v & DDC_EN_INTRP != 0 {
            return Some(v);
        }
        m.tick(5000 * 54); // the driver's 5 ms between polls
    }
    None
}

/// Release the bus the way the driver does after every transfer.
fn ddc_stop(m: &mut Machine, base: u32) {
    m.store32(base + DDC_CNT, 0).unwrap();
    m.store32(base + DDC_IIC_ENABLE, 0).unwrap();
}

/// Nothing is plugged into either HDMI connector on the reference board
/// (a Raspberry Pi 4B d03115 reports both `card1-HDMI-A-*/status` as
/// `disconnected`), so the EDID EEPROM's address goes unacknowledged and the
/// transfer has to complete `INTRP | NOACK`. That is the whole point of the
/// block: with `IIC_ENABLE` RAM-backing on the catch-all stub it read back the
/// `ENABLE | INTRP` the driver had just written, so every read looked like an
/// instant, successful transfer of 32 zero bytes — start4 failed the EDID
/// checksum, never bumped its attempt counter, and re-read EDID forever (#15).
#[test]
fn hdmi_ddc_nacks_when_no_monitor_answers() {
    for base in [map::HDMI_DDC0_BASE, map::HDMI_DDC1_BASE] {
        let mut m = machine();

        // Write phase: the EDID byte offset to start reading from.
        m.store32(base + DDC_DATA_IN, 0).unwrap();
        ddc_start(&mut m, base, 0x50, false, 1, DDC_EN_NOSTOP | DDC_EN_RESTART);
        let v = ddc_wait(&mut m, base).expect("the transfer must complete");
        assert_ne!(v & DDC_EN_NOACK, 0, "an empty bus cannot acknowledge");
        ddc_stop(&mut m, base);

        // Read phase.
        ddc_start(&mut m, base, 0x50, true, 32, DDC_EN_NOSTOP);
        let v = ddc_wait(&mut m, base).expect("the transfer must complete");
        assert_ne!(v & DDC_EN_NOACK, 0, "an empty bus cannot acknowledge");
        for i in 0..8 {
            assert_eq!(
                m.load32(base + DDC_DATA_OUT + 4 * i).unwrap(),
                0,
                "a NAKed read returns nothing"
            );
        }
        ddc_stop(&mut m, base);
    }
}

/// `INTRP` means "the bytes have been clocked out", so it cannot be set
/// already inside the register write that starts the transfer, however few
/// instructions the firmware retires before it looks. 32 bytes at the 97.5 kHz
/// the device tree gives for this bus take ~3 ms.
#[test]
fn hdmi_ddc_completion_waits_for_the_wire() {
    let base = map::HDMI_DDC0_BASE;
    let mut m = machine();

    ddc_start(&mut m, base, 0x50, true, 32, DDC_EN_NOSTOP);
    assert_eq!(
        m.load32(base + DDC_IIC_ENABLE).unwrap() & DDC_EN_INTRP,
        0,
        "INTRP must not land inside the write that started the transfer"
    );
    m.tick(1000 * 54); // 1 ms — a third of the way through
    assert_eq!(
        m.load32(base + DDC_IIC_ENABLE).unwrap() & DDC_EN_INTRP,
        0,
        "INTRP cannot precede the bits going out"
    );
    m.tick(3000 * 54);
    assert_ne!(
        m.load32(base + DDC_IIC_ENABLE).unwrap() & DDC_EN_INTRP,
        0,
        "INTRP must latch once the transfer is over"
    );

    // Disabling the master drops the status again.
    ddc_stop(&mut m, base);
    assert_eq!(m.load32(base + DDC_IIC_ENABLE).unwrap(), 0);
}

/// With a monitor on the bus the same sequence reads its EDID back: a one-byte
/// write phase sets the EEPROM's address pointer and the read chunks that
/// follow walk on from it, four bytes per `DATA_OUT` register, little-endian.
/// Nothing on the boot path attaches one yet — this pins the transport for the
/// HDMI mode-set work that would.
#[test]
fn hdmi_ddc_reads_an_attached_edid() {
    use rpi_virt_fw::periph::HdmiDdc;

    let edid: Vec<u8> = (0..128u32).map(|i| (i * 7 + 1) as u8).collect();
    let base = map::HDMI_DDC0_BASE;
    let mut m = machine();
    m.hdmi_ddc0 = HdmiDdc::new("hdmi-ddc0").with_edid(edid.clone());

    m.store32(base + DDC_DATA_IN, 0).unwrap();
    ddc_start(&mut m, base, 0x50, false, 1, DDC_EN_NOSTOP | DDC_EN_RESTART);
    let v = ddc_wait(&mut m, base).expect("the transfer must complete");
    assert_eq!(v & DDC_EN_NOACK, 0, "a monitor acknowledges its address");
    ddc_stop(&mut m, base);

    let mut got = Vec::new();
    for chunk in 0..4 {
        let flags = if chunk == 0 {
            DDC_EN_NOSTOP
        } else {
            DDC_EN_NOSTOP | DDC_EN_NOSTART
        };
        ddc_start(&mut m, base, 0x50, true, 32, flags);
        let v = ddc_wait(&mut m, base).expect("the transfer must complete");
        assert_eq!(v & DDC_EN_NOACK, 0, "a monitor acknowledges its address");
        for i in 0..8 {
            got.extend_from_slice(&m.load32(base + DDC_DATA_OUT + 4 * i).unwrap().to_le_bytes());
        }
        ddc_stop(&mut m, base);
    }
    assert_eq!(got, edid, "the whole block, in order");

    // The other connector still has nothing on it.
    let base1 = map::HDMI_DDC1_BASE;
    ddc_start(&mut m, base1, 0x50, true, 32, DDC_EN_NOSTOP);
    let v = ddc_wait(&mut m, base1).expect("the transfer must complete");
    assert_ne!(v & DDC_EN_NOACK, 0, "HDMI1 has no monitor");
}

// ---------------------------------------------------------------------------
// GPIO: the pins a master's pads are on decide what it reaches (#115).
// ---------------------------------------------------------------------------

const GPFSEL0: u32 = map::GPIO_BASE;
const GPFSEL4: u32 = map::GPIO_BASE + 0x10;

/// GPIO 0 and 1 on ALT0 — I²C 0 out to the 40-pin header.
const HEADER_I2C: u32 = 0b100 | 0b100 << 3;
/// GPIO 40..43 on ALT4 — SPI0 on the boot flash.
const FLASH_SPI: u32 = 0b011 | 0b011 << 3 | 0b011 << 6 | 0b011 << 9;

/// Read `len` bytes from `addr` on the I²C master at `base`, and say whether
/// the address was acknowledged.
fn i2c_read(m: &mut Machine, base: u32, addr: u8, len: u32) -> Option<Vec<u8>> {
    m.store32(base + BSC_A, addr as u32).unwrap();
    m.store32(base + BSC_DLEN, len).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST | C_READ).unwrap();
    settle(m);
    let s = m.load32(base + BSC_S).unwrap();
    let mut got = Vec::new();
    for _ in 0..len {
        got.push(m.load32(base + BSC_FIFO).unwrap() as u8);
    }
    m.store32(base + BSC_S, S_DONE | S_ERR).unwrap();
    (s & S_ERR == 0).then_some(got)
}

/// A HAT's ID EEPROM is on the header pins, so it answers only while I²C 0 is
/// muxed there. start4 probes it that way — `GPFSEL0` `0x4`, then `0x24`, then
/// back to inputs — and runs the same master on GPIO 44/45 for the camera and
/// display, where no HAT is: a probe made from there must find nothing.
#[test]
fn the_hat_eeprom_answers_only_on_the_header_pins() {
    let mut m = machine();
    m.bsc0
        .attach_eeprom(rpi_virt_fw::periph::hat::HatEeprom::new(
            b"R-Pi\x01\x00\x02\x00".to_vec(),
        ));

    // Out of reset every pin is an input: the master's pads are elsewhere.
    assert_eq!(i2c_read(&mut m, map::BSC0_BASE, 0x50, 4), None);

    m.store32(GPFSEL0, HEADER_I2C).unwrap();
    assert_eq!(
        i2c_read(&mut m, map::BSC0_BASE, 0x50, 4).as_deref(),
        Some(&b"R-Pi"[..]),
        "the header bus reaches the HAT"
    );

    // And back to inputs, as start4 leaves them after the probe.
    m.store32(GPFSEL0, 0).unwrap();
    assert_eq!(i2c_read(&mut m, map::BSC0_BASE, 0x50, 4), None);
}

/// The PMIC bus is not on the header: its instance is unaffected by the pins.
#[test]
fn the_pmic_bus_does_not_care_what_the_pins_do() {
    let mut m = machine();
    assert_eq!(pmic_read(&mut m, 0x43, 0x01) >> 5, 0b101);
    m.store32(GPFSEL0, HEADER_I2C).unwrap();
    assert_eq!(pmic_read(&mut m, 0x43, 0x01) >> 5, 0b101);
}

/// GPIO 40..43 are the boot flash on ALT4 and PWM audio plus the activity LED
/// otherwise, so a flash session only reads the image while the four pins are
/// on ALT4. Both EEPROM stages and start4 move them there and back around
/// every session (`specs/spi0.toml`).
#[test]
fn spi0_reads_the_flash_only_while_its_pins_are_on_alt4() {
    const CS_TA: u32 = 1 << 7;
    let mut m = machine();
    m.spi0.attach_flash(vec![0xAA; 0x1000]);

    let read_byte = |m: &mut Machine| {
        let base = map::SPI0_BASE;
        m.store32(base, CS_TA).unwrap();
        for byte in [0x03, 0x00, 0x00, 0x00, 0xFF] {
            m.store32(base + 0x04, byte).unwrap();
        }
        for _ in 0..4 {
            m.load32(base + 0x04).unwrap(); // command + address echoes
        }
        let v = m.load32(base + 0x04).unwrap() as u8;
        m.store32(base, 0).unwrap();
        v
    };

    // Pins still inputs: the bytes go nowhere and MISO idles high.
    assert_eq!(read_byte(&mut m), 0xFF);

    m.store32(GPFSEL4, FLASH_SPI).unwrap();
    assert_eq!(read_byte(&mut m), 0xAA, "ALT4 puts the pads on the flash");

    // GPIO 42 back to the activity LED (an output) ends the session's pads.
    m.store32(GPFSEL4, FLASH_SPI & !(0b111 << 6) | 0b001 << 6)
        .unwrap();
    assert_eq!(read_byte(&mut m), 0xFF);
}

/// The block's two interrupt lines follow `GPEDS`, and a detector watches the
/// pad whatever drives it — here the activity LED the firmware drives itself.
#[test]
fn a_gpio_edge_raises_the_banks_line() {
    const GPSET1: u32 = map::GPIO_BASE + 0x20;
    const GPCLR1: u32 = map::GPIO_BASE + 0x2C;
    const GPEDS1: u32 = map::GPIO_BASE + 0x44;
    const GPREN1: u32 = map::GPIO_BASE + 0x50;
    const LED: u32 = 1 << 10; // GPIO 42

    let mut m = machine();
    m.store32(GPFSEL4, 0b001 << 6).unwrap(); // GPIO 42 an output
    assert_eq!(m.gpio.irq_lines(), [false, false]);

    // No detector enabled: driving the pin latches nothing.
    m.store32(GPSET1, LED).unwrap();
    assert_eq!(m.load32(GPEDS1).unwrap(), 0);
    m.store32(GPCLR1, LED).unwrap();

    m.store32(GPREN1, LED).unwrap();
    m.store32(GPSET1, LED).unwrap();
    assert_eq!(m.load32(GPEDS1).unwrap(), LED, "the rising edge latched");
    assert_eq!(m.gpio.irq_lines(), [false, true]);

    m.store32(GPEDS1, LED).unwrap();
    assert_eq!(m.load32(GPEDS1).unwrap(), 0, "write 1 clears it");
    assert_eq!(m.gpio.irq_lines(), [false, false]);
}
