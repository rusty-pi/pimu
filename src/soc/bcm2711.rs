//! BCM2711 (Raspberry Pi 4 / 400 / CM4) memory map, from the VPU's point of view.
//!
//! The VPU addresses peripherals through the "legacy" alias at `0x7E00_0000`,
//! which the ARM cores see at `0xFE00_0000` in low-peripheral mode. All the
//! constants here are VPU-side (`0x7Exx_xxxx`).
//!
//! Every modelled block takes its window from its register spec in
//! `specs/*.toml` (#39), where the base and size carry their provenance; the
//! names here only keep [`crate::machine::Machine`]'s decoder readable.

use crate::spec;

/// Base of the peripheral window as seen by the VPU.
pub const PERIPH_BASE: u32 = 0x7E00_0000;
pub const PERIPH_SIZE: u32 = 0x0200_0000; // 32 MiB window (0x7E00_0000..0x8000_0000)

/// System timer (1 MHz free-running).
pub const SYSTIMER_BASE: u32 = spec::systimer::BASE;
pub const SYSTIMER_SIZE: u32 = spec::systimer::SIZE;

/// Multicore-sync block (`0x7E00_0000`): inter-core doorbells / semaphores.
pub const MCSYNC_BASE: u32 = spec::mcsync::BASE;
pub const MCSYNC_SIZE: u32 = spec::mcsync::SIZE;

/// VPU core-control block (`0x7E00_2000`): per-core start vectors and run-state.
/// `start4.elf` releases VPU core 1 through here.
pub const CORECTL_BASE: u32 = spec::corectl::BASE;
pub const CORECTL_SIZE: u32 = spec::corectl::SIZE;

/// DMA4 ("dma40") channel the main bootloader uses to scrub / move DRAM.
pub const DMA4_BASE: u32 = spec::dma4::BASE;
pub const DMA4_SIZE: u32 = spec::dma4::SIZE;

/// Legacy DMA controller: 15 channels x 0x100 plus the global INT_STATUS /
/// ENABLE words at the top of the window. start4 copies >= 1024 bytes through
/// here (`dma_memcpy`). Decoded *after* [`DMA4_BASE`], so channel 11 keeps
/// going to the 40-bit engine the bootloader uses.
pub const DMA_LEGACY_BASE: u32 = spec::dma::BASE;
pub const DMA_LEGACY_SIZE: u32 = spec::dma::SIZE;

/// The DMA controller start4's dmalib actually uses: `dma_set_cs` /
/// `dma_chain_start` pick `base = ch < 15 ? 0x7E007000 : 0x7EE04100`, and the
/// transfer queue runs on channel 15 — register block `0x7EE0_5000`. Outside
/// [`BOOTBOX_BASE`]'s 0x4000 window, so it used to land in the catch-all stub.
pub const DMA_VPU_BASE: u32 = spec::dma_vpu::BASE;
pub const DMA_VPU_SIZE: u32 = spec::dma_vpu::SIZE;

/// Boot-info handoff doorbells (`0x7EE0_0000` region: `0x7EE0_1000`,
/// `0x7EE0_2000`, `0x7EE0_2100`) — the bootloader stages a version/OTP block
/// into DRAM and rings these; their control bits self-clear. Kept below DMA4
/// at `0x7EE0_5000`.
pub const BOOTBOX_BASE: u32 = spec::bootbox::BASE;
pub const BOOTBOX_SIZE: u32 = spec::bootbox::SIZE;

/// Legacy SDRAM-controller register interface (`0x7E00_1000`): DRAM timing
/// words plus per-sub-controller lock/ready status the bootloader polls.
pub const SDC_BASE: u32 = spec::sdc::BASE;
pub const SDC_SIZE: u32 = spec::sdc::SIZE;

/// SPI0 master (`0x7E20_4000`).
pub const SPI0_BASE: u32 = spec::spi0::BASE;
pub const SPI0_SIZE: u32 = spec::spi0::SIZE;

/// BSC (I²C master) instance `start4.elf` uses for the board PMIC (slave
/// `0x1B`), at `0x7E20_5E00`. The other BSC instances (`0x7E80_4000`, …)
/// still fall through to the stub — nothing on the boot path hangs on them.
pub const BSC_PMIC_BASE: u32 = spec::bsc::PMIC_BASE;
pub const BSC_PMIC_SIZE: u32 = spec::bsc::SIZE;

/// BSC instance 0 (`0x7E20_5000`). start4's I²C driver picks its base from the
/// bus id — 8 is the PMIC bus above, 0 is this one, anything else is
/// `0x7E80_3000 + id * 0x1000` (`FUN_0ecf0ed0`). Late in the boot it probes
/// address `0x52` here for a HAT / display EEPROM; nothing is attached on a
/// bare board, so the transfer must complete with `S.ERR`.
pub const BSC0_BASE: u32 = spec::bsc::BASE;
pub const BSC0_SIZE: u32 = spec::bsc::SIZE;

/// The always-on config / OTP engine at `0x7E20_F000` the EEPROM bootloader
/// reads board identity through.
pub const OTP_BASE: u32 = spec::otp::BASE;
pub const OTP_SIZE: u32 = spec::otp::SIZE;

/// VCE (VideoCore vector/codec engine). Two windows, both taken from start4's
/// own driver (`vcfw/drivers/chip/vciv/2708/vce.c`): data memory at
/// `0x7F10_0000`, program memory at `0x7F11_0000` and the register file at
/// `0x7F12_0000` form one contiguous aperture, and the control block sits apart
/// at `0x7F14_0000`. `0x7F13_0000` is not claimed — nothing is known about it,
/// so it stays on the stub where it still shows up in the run report.
pub const VCE_BASE: u32 = spec::vce::BASE;
pub const VCE_MEM_SIZE: u32 = spec::vce::SIZE;
pub const VCE_CTRL_BASE: u32 = spec::vce_ctrl::BASE;
pub const VCE_CTRL_SIZE: u32 = spec::vce_ctrl::SIZE;

/// AXI async slave bridges (`0x7E00_A000`): the per-block stop/acknowledge
/// handshake start4 runs before gating the V3D, ISP and H264 power domains.
/// Named by Linux's `drivers/pmdomain/bcm/bcm2835-power.c`.
pub const ASB_BASE: u32 = spec::asb::BASE;
pub const ASB_SIZE: u32 = spec::asb::SIZE;

/// ARM control block (`0x7E00_B000`) up to the mailboxes: where `arm_loader`
/// releases the ARM.
pub const ARMCTRL_BASE: u32 = spec::armctrl::BASE;
pub const ARMCTRL_SIZE: u32 = spec::armctrl::SIZE;

/// The ARM <-> VideoCore mailboxes, `0x7E00_B880`: the ARM's view, the
/// interrupt block and the VPU's view.
pub const MBOX_BASE: u32 = spec::mbox::BASE;
pub const MBOX_SIZE: u32 = spec::mbox::SIZE;

/// Power-management block (`0x7E10_0000`): reset control + watchdog.
pub const PM_BASE: u32 = spec::pm::BASE;
pub const PM_SIZE: u32 = spec::pm::SIZE;

/// Clock manager (`0x7E10_1000`).
pub const CM_BASE: u32 = spec::cm::BASE;
pub const CM_SIZE: u32 = spec::cm::SIZE;

/// Hardware RNG at `0x7E10_4000` (`rng@7e104000`, `brcm,bcm2711-rng200`).
pub const RNG_BASE: u32 = spec::rng::BASE;
pub const RNG_SIZE: u32 = spec::rng::SIZE;

/// AVS monitor at `0x7D5D_2000` — on-die temperature sensor and the
/// ring-oscillator / rail monitors start4's DVFS code reads. Carved out of the
/// [`CLKMON_BASE`] window, so it must be decoded first.
pub const AVS_BASE: u32 = spec::avs::BASE;
pub const AVS_SIZE: u32 = spec::avs::SIZE;

/// Per-channel PVT monitors at `0x7D5D_8000`, eighteen channels `0x40` apart.
/// Also carved out of the [`CLKMON_BASE`] window and decoded ahead of it — the
/// magic at `+0x10` has to survive, or start4 concludes the blocks are absent.
pub const PVT_BASE: u32 = spec::pvt::BASE;
pub const PVT_SIZE: u32 = spec::pvt::SIZE;

/// VPU clock block (PLLs + frequency monitors) at `0x7D5D_0000` — outside the
/// `0x7E…` legacy window. start4's clock manager uses it on BCM2711.
pub const CLKMON_BASE: u32 = spec::clkmon::BASE;
pub const CLKMON_SIZE: u32 = spec::clkmon::SIZE;

/// GPIO (`0x7E20_0000`). Not modelled, so it has no spec and stays on the
/// catch-all stub.
pub const GPIO_BASE: u32 = 0x7E20_0000;
pub const GPIO_SIZE: u32 = 0x1000;

/// PL011 UART0 (`0x7E20_1000`). Primary firmware debug console when
/// `BOOT_UART=1` and the console is routed to the PL011.
pub const UART0_BASE: u32 = spec::uart0::BASE;
pub const UART0_SIZE: u32 = spec::uart0::SIZE;

/// AUX peripheral: mini-UART + two SPI masters (`0x7E21_5000`). The mini-UART at
/// offset `0x40` is the other common early console.
pub const AUX_BASE: u32 = spec::aux::BASE;
pub const AUX_SIZE: u32 = spec::aux::SIZE;

/// EMMC2 (SD card controller used for boot on Pi 4), `0x7E34_0000`.
pub const EMMC2_BASE: u32 = spec::emmc2::BASE;
pub const EMMC2_SIZE: u32 = spec::emmc2::SIZE;

/// HVS (Hardware Video Scaler), `0x7E40_0000` — the bootloader's diagnostic
/// display path polls its per-channel frame-swap registers.
pub const HVS_BASE: u32 = spec::hvs::BASE;
pub const HVS_SIZE: u32 = spec::hvs::SIZE;

/// The control block at `0x7E80_8000` whose power acknowledge start4's USB
/// power-on waits for, with no timeout (`src/periph/hd.rs`, #49).
pub const HD_BASE: u32 = spec::hd::BASE;
pub const HD_SIZE: u32 = spec::hd::SIZE;

/// DesignWare USB 2.0 OTG controller (`usb@7e980000`), the USB-C port. start4
/// resets it when USB power comes on (`src/periph/dwc2.rs`, #49).
pub const DWC2_BASE: u32 = spec::dwc2::BASE;
pub const DWC2_SIZE: u32 = spec::dwc2::SIZE;

/// The two HDMI controllers' DDC I²C masters (`i2c@7ef04500`, `i2c@7ef09500`,
/// `brcm,bcm2711-hdmi-i2c`) — the buses a monitor's EDID EEPROM sits on. Left
/// on the catch-all stub they RAM-back, so every EDID read looked like a
/// successful transfer of 128 zero bytes and start4 retried it forever
/// (`src/periph/hdmi_ddc.rs`).
pub const HDMI_DDC0_BASE: u32 = spec::hdmi_ddc::BASE;
pub const HDMI_DDC1_BASE: u32 = spec::hdmi_ddc::HDMI1_BASE;
pub const HDMI_DDC_SIZE: u32 = spec::hdmi_ddc::SIZE;

/// BCM2711 LPDDR4 controller + PHY, mapped *below* the legacy peripheral
/// window: the `init_sdram_*` path pokes `0x7DC2_0000` (command/status at
/// `+0x10`..`+0x28`) and per-byte-lane PHY blocks at `0x7DC2_0400`,
/// `+0x0600`, `+0x0A00`, `+0x0C00`, plus more blocks at `0x7DC3_0000` /
/// `0x7DC3_4000` / `0x7DC3_8000`. This window must be decoded *before* the
/// cache-alias fold, or the writes vanish into DRAM at `0x3DC2_0000`.
pub const SDRAMC_BASE: u32 = spec::sdramc::BASE;
pub const SDRAMC_SIZE: u32 = spec::sdramc::SIZE;

/// PCIe root complex (`pcie@7d500000`) — the BCM2711 block the VL805 xHCI
/// controller sits behind. Like [`SDRAMC_BASE`] and [`CLKMON_BASE`] it lives
/// *below* the `0x7E…` peripheral window, so it must be decoded before the
/// cache-alias fold or its registers land in DRAM at `0x3D50_0000`. See
/// `docs/usb-xhci.md`.
pub const PCIE_BASE: u32 = spec::pcie::BASE;
pub const PCIE_SIZE: u32 = spec::pcie::SIZE;

/// GENET v5 Ethernet MAC (`ethernet@7d580000`) with its UniMAC MDIO bus at
/// `+0xE14`. Below the `0x7E…` window like [`PCIE_BASE`], so decoded ahead of
/// the cache-alias fold. See [`crate::periph::genet`].
pub const GENET_BASE: u32 = spec::genet::BASE;
pub const GENET_SIZE: u32 = spec::genet::SIZE;

/// Main SDRAM as seen by the VPU (cached alias at 0, uncached at 0xC000_0000).
pub const SDRAM_CACHED_BASE: u32 = 0x0000_0000;
pub const SDRAM_UNCACHED_BASE: u32 = 0xC000_0000;

/// Where the boot ROM parks the second-stage bootloader extracted from
/// `pieeprom.bin` before jumping to it. This is an approximation for the model;
/// the real BCM2711 boot ROM runs the recovery/bootloader from L2-as-SRAM.
///
/// TODO(pieeprom milestone): confirm the real load/entry address.
pub const BOOTLOADER_LOAD_ADDR: u32 = 0x6000_0000;

/// Region kinds the [`Machine`](crate::machine::Machine) address decoder knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Sdram,
    SysTimer,
    ArmCtrl,
    Mailbox,
    ClockManager,
    Gpio,
    Uart0,
    Aux,
    Emmc2,
    /// Somewhere inside the peripheral window but not a device we model yet.
    UnmappedPeripheral,
    /// Outside anything we know.
    Unmapped,
}
