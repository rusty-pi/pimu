//! BCM2711 (Raspberry Pi 4 / 400 / CM4) memory map, from the VPU's point of view.
//!
//! The VPU addresses peripherals through the "legacy" alias at `0x7E00_0000`,
//! which the ARM cores see at `0xFE00_0000` in low-peripheral mode. All the
//! constants here are VPU-side (`0x7Exx_xxxx`).
//!
//! Sources: BCM2711 ARM Peripherals datasheet; Raspberry Pi firmware
//! `hardware/` headers; `librerpi/rpi-open-firmware`.

/// Base of the peripheral window as seen by the VPU.
pub const PERIPH_BASE: u32 = 0x7E00_0000;
pub const PERIPH_SIZE: u32 = 0x0200_0000; // 32 MiB window (0x7E00_0000..0x8000_0000)

/// System timer (1 MHz free-running).
pub const SYSTIMER_BASE: u32 = 0x7E00_3000;
pub const SYSTIMER_SIZE: u32 = 0x1000;

/// Multicore-sync block (`0x7E00_0000`): inter-core doorbells / semaphores.
pub const MCSYNC_BASE: u32 = 0x7E00_0000;
pub const MCSYNC_SIZE: u32 = 0x1000;

/// VPU core-control block (`0x7E00_2000`): per-core start vectors and run-state.
/// `start4.elf` releases VPU core 1 through here.
pub const CORECTL_BASE: u32 = 0x7E00_2000;
pub const CORECTL_SIZE: u32 = 0x1000;

/// DMA4 ("dma40") channel the main bootloader uses to scrub / move DRAM.
pub const DMA4_BASE: u32 = 0x7E00_7B00;
pub const DMA4_SIZE: u32 = 0x100;

/// Legacy DMA controller: 15 channels x 0x100 plus the global INT_STATUS /
/// ENABLE words at the top of the window. start4 copies >= 1024 bytes through
/// here (`dma_memcpy`). Decoded *after* [`DMA4_BASE`], so channel 11 keeps
/// going to the 40-bit engine the bootloader uses.
pub const DMA_LEGACY_BASE: u32 = 0x7E00_7000;
pub const DMA_LEGACY_SIZE: u32 = 0x1000;

/// The DMA controller start4's dmalib actually uses: `dma_set_cs` /
/// `dma_chain_start` pick `base = ch < 15 ? 0x7E007000 : 0x7EE04100`, and the
/// transfer queue runs on channel 15 — register block `0x7EE0_5000`. Outside
/// [`BOOTBOX_BASE`]'s 0x4000 window, so it used to land in the catch-all stub.
pub const DMA_VPU_BASE: u32 = 0x7EE0_4100;
pub const DMA_VPU_SIZE: u32 = 0x1000;

/// Boot-info handoff doorbells (`0x7EE0_0000` region: `0x7EE0_1000`,
/// `0x7EE0_2000`, `0x7EE0_2100`) — the bootloader stages a version/OTP block
/// into DRAM and rings these; their control bits self-clear. Kept below DMA4
/// at `0x7EE0_5000`.
pub const BOOTBOX_BASE: u32 = 0x7EE0_0000;
pub const BOOTBOX_SIZE: u32 = 0x4000;

/// Legacy SDRAM-controller register interface (`0x7E00_1000`): DRAM timing
/// words plus per-sub-controller lock/ready status the bootloader polls.
pub const SDC_BASE: u32 = 0x7E00_1000;
pub const SDC_SIZE: u32 = 0x1000;

/// SPI0 master (`0x7E20_4000`).
pub const SPI0_BASE: u32 = 0x7E20_4000;
pub const SPI0_SIZE: u32 = 0x18;

/// BSC (I²C master) instance `start4.elf` uses for the board PMIC (slave
/// `0x1B`), at `0x7E20_5E00`. The other BSC instances (`0x7E20_5000`,
/// `0x7E80_4000`, …) still fall through to the stub — nothing on the boot path
/// hangs on them.
pub const BSC_PMIC_BASE: u32 = 0x7E20_5E00;
pub const BSC_PMIC_SIZE: u32 = 0x20;

/// Unmodelled FIFO/crypto block at `0x7E20_F000` the EEPROM bootloader polls.
pub const FIFO_STUB_BASE: u32 = 0x7E20_F000;
pub const FIFO_STUB_SIZE: u32 = 0x1000;

/// ARM control block: mailboxes, doorbells, IRQ routing (`0x7E00_B000`).
pub const ARMCTRL_BASE: u32 = 0x7E00_B000;
pub const ARMCTRL_SIZE: u32 = 0x1000;

/// VideoCore mailbox peripheral (property interface), `0x7E00_B880`.
pub const MBOX_BASE: u32 = 0x7E00_B880;
pub const MBOX_SIZE: u32 = 0x40;

/// Power-management block (`0x7E10_0000`): reset control + watchdog.
pub const PM_BASE: u32 = 0x7E10_0000;
pub const PM_SIZE: u32 = 0x1000;

/// Clock manager (`0x7E10_1000`).
pub const CM_BASE: u32 = 0x7E10_1000;
pub const CM_SIZE: u32 = 0x2000;

/// Hardware RNG at `0x7E10_4000` (`rng@7e104000`, `brcm,bcm2711-rng200`).
pub const RNG_BASE: u32 = 0x7E10_4000;
pub const RNG_SIZE: u32 = 0x28;

/// AVS monitor at `0x7D5D_2000` — on-die temperature sensor and the
/// ring-oscillator / rail monitors start4's DVFS code reads. Carved out of the
/// [`CLKMON_BASE`] window, so it must be decoded first. `reg = <0x7d5d2000
/// 0xf00>` in the Pi 4 device tree.
pub const AVS_BASE: u32 = 0x7D5D_2000;
pub const AVS_SIZE: u32 = 0x0000_0F00;

/// VPU clock block (PLLs + frequency monitors) at `0x7D5D_0000` — outside the
/// `0x7E…` legacy window. start4's clock manager uses it on BCM2711.
pub const CLKMON_BASE: u32 = 0x7D5D_0000;
pub const CLKMON_SIZE: u32 = 0x0001_0000;

/// GPIO (`0x7E20_0000`).
pub const GPIO_BASE: u32 = 0x7E20_0000;
pub const GPIO_SIZE: u32 = 0x1000;

/// PL011 UART0 (`0x7E20_1000`). Primary firmware debug console when
/// `BOOT_UART=1` and the console is routed to the PL011.
pub const UART0_BASE: u32 = 0x7E20_1000;
pub const UART0_SIZE: u32 = 0x1000;

/// AUX peripheral: mini-UART + two SPI masters (`0x7E21_5000`). The mini-UART at
/// offset `0x40` is the other common early console.
pub const AUX_BASE: u32 = 0x7E21_5000;
pub const AUX_SIZE: u32 = 0x100;

/// EMMC2 (SD card controller used for boot on Pi 4), `0x7E34_0000`.
pub const EMMC2_BASE: u32 = 0x7E34_0000;
pub const EMMC2_SIZE: u32 = 0x1000;

/// HVS (Hardware Video Scaler), `0x7E40_0000` — the bootloader's diagnostic
/// display path polls its per-channel frame-swap registers.
pub const HVS_BASE: u32 = 0x7E40_0000;
pub const HVS_SIZE: u32 = 0x1000;

/// BCM2711 LPDDR4 controller + PHY, mapped *below* the legacy peripheral
/// window: the `init_sdram_*` path pokes `0x7DC2_0000` (command/status at
/// `+0x10`..`+0x28`) and per-byte-lane PHY blocks at `0x7DC2_0400`,
/// `+0x0600`, `+0x0A00`, `+0x0C00`, plus more blocks at `0x7DC3_0000` /
/// `0x7DC3_4000` / `0x7DC3_8000`. This window must be decoded *before* the
/// cache-alias fold, or the writes vanish into DRAM at `0x3DC2_0000`.
pub const SDRAMC_BASE: u32 = 0x7DC0_0000;
pub const SDRAMC_SIZE: u32 = 0x0004_0000; // 0x7DC0_0000..0x7DC4_0000

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
