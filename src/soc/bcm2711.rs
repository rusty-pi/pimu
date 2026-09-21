//! BCM2711 (Raspberry Pi 4 / 400 / CM4) memory map, from the VPU's point of view.
//!
//! The VPU addresses peripherals through the "legacy" alias at `0x7E00_0000`,
//! which the ARM cores see at `0xFE00_0000` in low-peripheral mode. All the
//! constants here are VPU-side (`0x7Exx_xxxx`).
//!
//! Those are the addresses the datasheet itself uses, and what it calls legacy
//! master addresses: the chip carries a 35-bit bus behind them, where a
//! peripheral at `0x7Enn_nnnn` really sits at `0x4_7Enn_nnnn` and RAM the DMA
//! engines reach through `0xC000_0000`..`0xFFFF_FFFF` is a movable 1 GB window
//! (`PAGE` / `PAGELITE`) onto 16 GB of SDRAM. The 40-bit DMA4 engines skip the
//! window and drive the wide bus themselves, which is how a 32-bit VPU reaches
//! the PCIe region (BCM2711 ARM Peripherals, §1.2).
//!
//! One thing the model does not reproduce, and does not need to: the real AXI
//! system can return reads from *different* peripherals out of order, which is
//! why the datasheet tells ARM code to put a memory barrier at the entry and
//! exit of any peripheral service routine (§1.3). Accesses to one peripheral
//! always stay in order. Here every access is in program order, so firmware
//! that forgets a barrier still works — a bug this bench cannot find, the same
//! way it cannot find a missing cache flush the L2 happens to forgive.
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

/// VPU core-control block (`0x7E00_2000`): each core's interrupt controller,
/// exception-vector base and start address.
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

/// PWM0 (`0x7E20_C000`) and PWM1 (`0x7E20_C800`): the pulse-width /
/// serialiser blocks. Nothing in a boot programs either.
pub const PWM0_BASE: u32 = spec::pwm::BASE;
pub const PWM1_BASE: u32 = spec::pwm::PWM1_BASE;
pub const PWM_SIZE: u32 = spec::pwm::SIZE;

/// PCM / I²S (`0x7E20_3000`): the one serial-audio interface.
pub const PCM_BASE: u32 = spec::pcm::BASE;
pub const PCM_SIZE: u32 = spec::pcm::SIZE;

/// `PACTL_CS` (`0x7E20_4E00`): which SPI, I²C or UART behind an ORed
/// interrupt line is the one asking.
pub const PACTL_BASE: u32 = spec::pactl::BASE;
pub const PACTL_SIZE: u32 = spec::pactl::SIZE;

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

/// GPIO (`0x7E20_0000`): pin functions, levels and pulls, and the undocumented
/// word at `+0xD0` that routes the SD card slot. See [`crate::periph::gpio`].
pub const GPIO_BASE: u32 = spec::gpio::BASE;
pub const GPIO_SIZE: u32 = spec::gpio::SIZE;

/// PL011 UART0 (`0x7E20_1000`). Primary firmware debug console when
/// `BOOT_UART=1` and the console is routed to the PL011.
pub const UART0_BASE: u32 = spec::uart0::BASE;
pub const UART0_SIZE: u32 = spec::uart0::SIZE;

/// AUX peripheral: mini-UART + two SPI masters (`0x7E21_5000`). The mini-UART at
/// offset `0x40` is the other common early console.
pub const AUX_BASE: u32 = spec::aux::BASE;
pub const AUX_SIZE: u32 = spec::aux::SIZE;

/// The legacy EMMC controller (`mmcnr@7e300000`), `0x7E30_0000`: the WiFi
/// SDIO host on a Pi 4, and the SD host 2020-era bootcode uses (#64).
pub const EMMC_BASE: u32 = spec::emmc::BASE;
pub const EMMC_SIZE: u32 = spec::emmc::SIZE;

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

/// The BCM2711's own xHCI controller (`xhci@7e9c0000`), the other controller
/// on the USB-C port: what the bootloader boots from as `BCM-USB-MSD`
/// (`BOOT_ORDER` digit `0x5`) and what `otg_mode=1` gives Linux
/// (`src/periph/xhci_otg.rs`, #113).
pub const XHCI_OTG_BASE: u32 = spec::xhci_otg::BASE;
pub const XHCI_OTG_SIZE: u32 = spec::xhci_otg::SIZE;

/// The two HDMI controllers' core registers (`hdmi@7ef00700`, `hdmi@7ef05700`,
/// reg-name "hdmi"), with no monitor attached. start4 waits on the packet-RAM
/// status when it stops its display (#61), and 2020-era bootcode on the FIFO
/// recenter at every boot (#63), both with no timeout (`src/periph/hdmi.rs`).
pub const HDMI0_BASE: u32 = spec::hdmi::BASE;
pub const HDMI1_BASE: u32 = spec::hdmi::HDMI1_BASE;
pub const HDMI_SIZE: u32 = spec::hdmi::SIZE;

/// The two HDMI controllers' DDC I²C masters (`i2c@7ef04500`, `i2c@7ef09500`,
/// `brcm,bcm2711-hdmi-i2c`) — the buses a monitor's EDID EEPROM sits on. Left
/// on the catch-all stub they RAM-back, so every EDID read looked like a
/// successful transfer of 128 zero bytes and start4 retried it forever
/// (`src/periph/hdmi_ddc.rs`).
pub const HDMI_DDC0_BASE: u32 = spec::hdmi_ddc::BASE;
pub const HDMI_DDC1_BASE: u32 = spec::hdmi_ddc::HDMI1_BASE;
pub const HDMI_DDC_SIZE: u32 = spec::hdmi_ddc::SIZE;

/// The same nodes' second `reg` window, the auto-i2c sequencers. start4
/// 1.20190925 to 1.20200601 run one list through HDMI0's at boot and wait for
/// it with no timeout, so on the catch-all stub they never started the ARM
/// (`src/periph/hdmi_ddc.rs`, #76).
pub const HDMI_AUTO_I2C0_BASE: u32 = spec::hdmi_auto_i2c::BASE;
pub const HDMI_AUTO_I2C1_BASE: u32 = spec::hdmi_auto_i2c::HDMI1_BASE;
pub const HDMI_AUTO_I2C_SIZE: u32 = spec::hdmi_auto_i2c::SIZE;

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
/// [`crate::periph::pcie`].
pub const PCIE_BASE: u32 = spec::pcie::BASE;
pub const PCIE_SIZE: u32 = spec::pcie::SIZE;

/// GENET v5 Ethernet MAC (`ethernet@7d580000`) with its UniMAC MDIO bus at
/// `+0xE14`. Below the `0x7E…` window like [`PCIE_BASE`], so decoded ahead of
/// the cache-alias fold. See [`crate::periph::genet`].
pub const GENET_BASE: u32 = spec::genet::BASE;
pub const GENET_SIZE: u32 = spec::genet::SIZE;

/// Main SDRAM as seen by the VPU (cached alias at 0, uncached at 0xC000_0000).
pub const SDRAM_CACHED_BASE: u32 = 0x0000_0000;

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
