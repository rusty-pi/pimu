//! BCM2711 (Raspberry Pi 4 / 400 / CM4) memory map, from the VPU's point of
//! view: the "legacy" peripheral alias at `0x7E00_0000`, which the ARM cores
//! see at `0xFE00_0000` in low-peripheral mode. All constants here are VPU-side
//! (`0x7Exx_xxxx`), as in the datasheet, and each modelled block takes its
//! window from its register spec in `specs/*.toml`, where the base and size
//! carry their provenance; the names here only keep
//! [`crate::machine::Machine`]'s decoder readable.
//!
//! One thing the model deliberately does not reproduce: the real AXI system can
//! return reads from *different* peripherals out of order, which is why the
//! datasheet asks ARM code for a barrier around any peripheral service routine
//! (§1.3). Here every access is in program order, so firmware that forgets a
//! barrier still works — a bug this bench cannot find.
use crate::spec;

pub const PERIPH_BASE: u32 = 0x7E00_0000;
pub const PERIPH_SIZE: u32 = 0x0200_0000; // 32 MiB window (0x7E00_0000..0x8000_0000)

pub const SYSTIMER_BASE: u32 = spec::systimer::BASE;
pub const SYSTIMER_SIZE: u32 = spec::systimer::SIZE;

pub const MCSYNC_BASE: u32 = spec::mcsync::BASE;
pub const MCSYNC_SIZE: u32 = spec::mcsync::SIZE;

/// VPU core-control block: each core's interrupt controller, exception-vector
/// base and start address.
pub const CORECTL_BASE: u32 = spec::corectl::BASE;
pub const CORECTL_SIZE: u32 = spec::corectl::SIZE;

pub const DMA4_BASE: u32 = spec::dma4::BASE;
pub const DMA4_SIZE: u32 = spec::dma4::SIZE;

/// Legacy DMA controller: 15 channels plus the global `INT_STATUS`/`ENABLE`
/// words at the top of the window. Decoded *after* [`DMA4_BASE`], so channel 11
/// keeps going to the 40-bit engine the bootloader uses.
pub const DMA_LEGACY_BASE: u32 = spec::dma::BASE;
pub const DMA_LEGACY_SIZE: u32 = spec::dma::SIZE;

/// The DMA controller start4's dmalib uses: its transfer queue runs on channel
/// 15, whose register block is outside [`BOOTBOX_BASE`]'s window.
pub const DMA_VPU_BASE: u32 = spec::dma_vpu::BASE;
pub const DMA_VPU_SIZE: u32 = spec::dma_vpu::SIZE;

/// Boot-info handoff doorbells: the bootloader stages a version/OTP block into
/// DRAM and rings these, whose control bits self-clear. Kept below DMA4.
pub const BOOTBOX_BASE: u32 = spec::bootbox::BASE;
pub const BOOTBOX_SIZE: u32 = spec::bootbox::SIZE;

/// Legacy SDRAM-controller register interface: DRAM timing words plus the
/// per-sub-controller lock/ready status the bootloader polls.
pub const SDC_BASE: u32 = spec::sdc::BASE;
pub const SDC_SIZE: u32 = spec::sdc::SIZE;

pub const SPI0_BASE: u32 = spec::spi0::BASE;
pub const SPI0_SIZE: u32 = spec::spi0::SIZE;

/// The BSC (I²C master) instance `start4.elf` uses for the board PMIC. The
/// other instances fall through to the stub — nothing on the boot path hangs
/// on them.
pub const BSC_PMIC_BASE: u32 = spec::bsc::PMIC_BASE;
pub const BSC_PMIC_SIZE: u32 = spec::bsc::SIZE;

pub const PWM0_BASE: u32 = spec::pwm::BASE;
pub const PWM1_BASE: u32 = spec::pwm::PWM1_BASE;
pub const PWM_SIZE: u32 = spec::pwm::SIZE;

pub const PCM_BASE: u32 = spec::pcm::BASE;
pub const PCM_SIZE: u32 = spec::pcm::SIZE;

pub const PACTL_BASE: u32 = spec::pactl::BASE;
pub const PACTL_SIZE: u32 = spec::pactl::SIZE;

/// BSC instance 0. Late in the boot start4 probes address `0x52` here for a
/// HAT / display EEPROM; nothing is attached on a bare board, so the transfer
/// must complete with `S.ERR`.
pub const BSC0_BASE: u32 = spec::bsc::BASE;
pub const BSC0_SIZE: u32 = spec::bsc::SIZE;

pub const OTP_BASE: u32 = spec::otp::BASE;
pub const OTP_SIZE: u32 = spec::otp::SIZE;

/// VCE (VideoCore vector/codec engine), the windows start4's own driver uses.
/// `0x7F13_0000` is not claimed — nothing is known about it — so it stays on the
/// stub and shows up in the run report.
pub const VCE_BASE: u32 = spec::vce::BASE;
pub const VCE_MEM_SIZE: u32 = spec::vce::SIZE;
pub const VCE_CTRL_BASE: u32 = spec::vce_ctrl::BASE;
pub const VCE_CTRL_SIZE: u32 = spec::vce_ctrl::SIZE;

/// AXI async slave bridges: the per-block stop/acknowledge handshake start4
/// runs before gating the V3D, ISP and H264 power domains.
pub const ASB_BASE: u32 = spec::asb::BASE;
pub const ASB_SIZE: u32 = spec::asb::SIZE;

pub const ARMCTRL_BASE: u32 = spec::armctrl::BASE;
pub const ARMCTRL_SIZE: u32 = spec::armctrl::SIZE;

/// The ARM <-> VideoCore doorbells and the VPU's view of them, VCHIQ's wake
/// path. Inside [`ARMCTRL_BASE`]'s and [`MBOX_BASE`]'s windows, so decoded
/// ahead of those.
pub const BELL_BASE: u32 = spec::bell::BASE;
pub const BELL_VPU_BASE: u32 = spec::bell::VPU_BASE;
pub const BELL_SIZE: u32 = spec::bell::SIZE;

pub const MBOX_BASE: u32 = spec::mbox::BASE;
pub const MBOX_SIZE: u32 = spec::mbox::SIZE;

pub const PM_BASE: u32 = spec::pm::BASE;
pub const PM_SIZE: u32 = spec::pm::SIZE;

pub const CM_BASE: u32 = spec::cm::BASE;
pub const CM_SIZE: u32 = spec::cm::SIZE;

pub const RNG_BASE: u32 = spec::rng::BASE;
pub const RNG_SIZE: u32 = spec::rng::SIZE;

/// AVS monitor: on-die temperature sensor and the rail monitors start4's DVFS
/// code reads. Carved out of [`CLKMON_BASE`], so it is decoded first.
pub const AVS_BASE: u32 = spec::avs::BASE;
pub const AVS_SIZE: u32 = spec::avs::SIZE;

/// Per-channel PVT monitors, eighteen channels `0x40` apart. Carved out of the
/// [`CLKMON_BASE`] window and decoded ahead of it — the magic at `+0x10` has to
/// survive, or start4 concludes the blocks are absent.
pub const PVT_BASE: u32 = spec::pvt::BASE;
pub const PVT_SIZE: u32 = spec::pvt::SIZE;

pub const CLKMON_BASE: u32 = spec::clkmon::BASE;
pub const CLKMON_SIZE: u32 = spec::clkmon::SIZE;

/// GPIO: pin functions, levels and pulls, and the undocumented word at `+0xD0`
/// that routes the SD card slot. See [`crate::periph::gpio`].
pub const GPIO_BASE: u32 = spec::gpio::BASE;
pub const GPIO_SIZE: u32 = spec::gpio::SIZE;

pub const UART0_BASE: u32 = spec::uart0::BASE;
pub const UART0_SIZE: u32 = spec::uart0::SIZE;

pub const AUX_BASE: u32 = spec::aux::BASE;
pub const AUX_SIZE: u32 = spec::aux::SIZE;

/// The legacy EMMC controller (`mmcnr@7e300000`): the WiFi SDIO host on a Pi 4,
/// and the SD host 2020-era bootcode uses.
pub const EMMC_BASE: u32 = spec::emmc::BASE;
pub const EMMC_SIZE: u32 = spec::emmc::SIZE;

pub const EMMC2_BASE: u32 = spec::emmc2::BASE;
pub const EMMC2_SIZE: u32 = spec::emmc2::SIZE;

pub const HVS_BASE: u32 = spec::hvs::BASE;
pub const HVS_SIZE: u32 = spec::hvs::SIZE;

/// The control block whose power acknowledge start4's USB power-on waits for,
/// with no timeout.
pub const USBR_BASE: u32 = spec::usbr::BASE;
pub const USBR_SIZE: u32 = spec::usbr::SIZE;

pub const DWC2_BASE: u32 = spec::dwc2::BASE;
pub const DWC2_SIZE: u32 = spec::dwc2::SIZE;

/// The BCM2711's own xHCI (`xhci@7e9c0000`), the other controller on the USB-C
/// port: what the bootloader boots as `BCM-USB-MSD` and `otg_mode=1` exposes.
pub const XHCI_OTG_BASE: u32 = spec::xhci_otg::BASE;
pub const XHCI_OTG_SIZE: u32 = spec::xhci_otg::SIZE;

/// The two HDMI controllers' core registers, with no monitor attached. start4
/// waits on the packet-RAM status when it stops its display, and 2020-era
/// bootcode on the FIFO recenter, both with no timeout.
pub const HDMI0_BASE: u32 = spec::hdmi::BASE;
pub const HDMI1_BASE: u32 = spec::hdmi::HDMI1_BASE;
pub const HDMI_SIZE: u32 = spec::hdmi::SIZE;

/// The two HDMI controllers' DDC I²C masters. They need a model of their own:
/// RAM-backed, an EDID read looks like 128 zero bytes and start4 retries.
pub const HDMI_DDC0_BASE: u32 = spec::hdmi_ddc::BASE;
pub const HDMI_DDC1_BASE: u32 = spec::hdmi_ddc::HDMI1_BASE;
pub const HDMI_DDC_SIZE: u32 = spec::hdmi_ddc::SIZE;

/// The same nodes' second `reg` window, the auto-i2c sequencers: start4
/// 1.20190925 to 1.20200601 run a list through HDMI0's at boot and wait for it
/// with no timeout.
pub const HDMI_AUTO_I2C0_BASE: u32 = spec::hdmi_auto_i2c::BASE;
pub const HDMI_AUTO_I2C1_BASE: u32 = spec::hdmi_auto_i2c::HDMI1_BASE;
pub const HDMI_AUTO_I2C_SIZE: u32 = spec::hdmi_auto_i2c::SIZE;

/// BCM2711 LPDDR4 controller and PHY, below the legacy peripheral window. Must
/// be decoded *before* the cache-alias fold, or the writes vanish into DRAM.
pub const SDRAMC_BASE: u32 = spec::sdramc::BASE;
pub const SDRAMC_SIZE: u32 = spec::sdramc::SIZE;

/// PCIe root complex (`pcie@7d500000`), which the VL805 xHCI sits behind. Like
/// [`SDRAMC_BASE`] and [`CLKMON_BASE`] it is below the `0x7E…` window, so it
/// must be decoded before the cache-alias fold or its registers land in DRAM.
pub const PCIE_BASE: u32 = spec::pcie::BASE;
pub const PCIE_SIZE: u32 = spec::pcie::SIZE;

pub const GENET_BASE: u32 = spec::genet::BASE;
pub const GENET_SIZE: u32 = spec::genet::SIZE;

pub const SDRAM_CACHED_BASE: u32 = 0x0000_0000;

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
    UnmappedPeripheral,
    Unmapped,
}
