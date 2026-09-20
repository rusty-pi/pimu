//! The `Machine`: RAM + peripherals + address decode. Implements [`Bus`].

use crate::bus::{Bus, BusError, BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};
use crate::mem::Ram;
use crate::periph::gpio;
use crate::periph::hdmi_ddc::AUTO_WINDOW;
use crate::periph::{
    ArmCtrl, ArmLocal, Asb, Aux, Avs, BootBox, Bsc, ClkMon, ClockManager, ConfigOtp, CoreCtl, Dma4,
    Dwc2, Emmc2, Gic, Gpio, Hd, Hdmi, HdmiDdc, Hvs, Mbox, McSync, Pl011, Pm, Rng, Sdc, Sdramc,
    Spi0, StubRegion, SysTimer, Vce, XhciOtg,
};
use crate::soc::bcm2711 as map;

/// Which UART the harness captures as "the console".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Console {
    #[default]
    Pl011,
    MiniUart,
}

pub struct Machine {
    pub ram: Ram,
    /// Experimental: a real BCM2711 boot-ROM image mapped read-only at
    /// `0x6000_0000` (`--boot-rom`). When set, reads and instruction fetches in
    /// `[base, base+len)` are served from these bytes instead of the DRAM alias,
    /// so the VPU can execute the maskROM from its reset vector. Writes and every
    /// address outside the range fall through to normal decoding, so the ROM's
    /// scratch (above the code region) and its staging at `0x8000_0000` still
    /// land in RAM. `None` on every normal boot — see `firmware::bootrom`.
    boot_rom: Option<(u32, u32, Vec<u8>)>,
    /// The L2 the bootcode runs out of, until its flush ([`crate::l2`], #70).
    pub l2: crate::l2::CacheAsRam,
    pub systimer: SysTimer,
    pub uart0: Pl011,
    pub aux: Aux,
    /// The ARM property mailbox (`0x7E00_B880`). Idle during a normal boot —
    /// the firmware only services it once an ARM is running.
    pub mbox: Mbox,
    /// The ARM control block below the mailboxes (`0x7E00_B000`): where
    /// `arm_loader` releases the ARM.
    pub armctrl: ArmCtrl,
    /// ARM-only blocks, reached from [`crate::arm`], never from the VPU: the
    /// ARM-local block (`0xFF80_0000`) and the GIC-400 (`0xFF84_0000`).
    pub arm_local: ArmLocal,
    pub gic: Gic,
    /// Inter-core sync block (`0x7E00_0000`) — stubbed as auto-acknowledged.
    pub mcsync: McSync,
    /// VPU core-control block (`0x7E00_2000`) — brings up VPU core 1.
    pub corectl: CoreCtl,
    /// Power-management block (`0x7E10_0000`) — SoC reset / watchdog.
    pub pm: Pm,
    /// Clock manager (`0x7E10_1000`) — PLL locks always report ready.
    pub clockman: ClockManager,
    /// VPU clock block (`0x7D5D_0000`) — PLLs + frequency monitors.
    pub clkmon: ClkMon,
    /// AVS monitor (`0x7D5D_2000`) — temperature and rail monitors.
    pub avs: Avs,
    /// PVT monitors (`0x7D5D_8000`) — eighteen per-channel process / voltage /
    /// temperature sensors, each gated on a magic at `+0x10`.
    pub pvt: crate::periph::Pvt,
    /// AXI async slave bridges (`0x7E00_A000`) — the stop/acknowledge handshake
    /// start4 runs before gating the V3D / ISP / H264 power domains.
    pub asb: Asb,
    /// PCIe root complex (`0x7D50_0000`). The VL805 xHCI controller behind it
    /// is [`crate::periph::Vl805`], reached through this block's
    /// `EXT_CFG_DATA` and its outbound window.
    pub pcie: crate::periph::pcie::Pcie,
    /// GENET v5 Ethernet MAC (`0x7D58_0000`) with the BCM54213PE PHY on its
    /// MDIO bus; see [`crate::periph::genet`].
    pub genet: crate::periph::Genet,
    /// What GENET's cable is plugged into, if anything ([`Machine::attach_net`]).
    pub net: Option<Box<dyn crate::net::NetBackend>>,
    /// Hardware RNG (`0x7E10_4000`), an RNG200 — start4 and Linux both read it.
    pub rng: Rng,
    /// VCE vector/codec engine (`0x7F10_0000`) — the codec licence check
    /// launches a program on it and waits for interrupt source 68.
    pub vce: Vce,
    /// BSC instance 0 (`0x7E20_5000`) — nothing attached; probes go unACKed.
    pub bsc0: Bsc,
    /// The GPIO block (`0x7E20_0000`): pin functions, levels and pulls, and
    /// the mux word [`Self::route_sd_slot`] follows.
    pub gpio: Gpio,
    /// SPI0 master (`0x7E20_4000`) — minimal model for the EEPROM bootloader.
    pub spi0: Spi0,
    /// BSC / I²C master at `0x7E20_5E00` + the board PMIC — start4 reads the
    /// PMIC over this on its way to bringing up the "external" GPIO pins.
    pub bsc_pmic: Bsc,
    /// The two HDMI connectors' DDC I²C masters, with no monitor on either.
    pub hdmi_ddc0: HdmiDdc,
    pub hdmi_ddc1: HdmiDdc,
    /// The two HDMI controllers' core registers — the packet RAM start4
    /// sends AV mute through when it stops its display (#61).
    pub hdmi0: Hdmi,
    pub hdmi1: Hdmi,
    /// Which board this is and which BCM2711 stepping it carries. Changed only
    /// through [`Machine::set_board`], which keeps OTP row 30 in step.
    board: crate::soc::Board,
    /// Always-on config / OTP engine (`0x7E20_F000`) — board identity reads.
    pub config_otp: ConfigOtp,
    /// LPDDR4 controller + PHY (`0x7DC0_0000`, below the peripheral window) —
    /// the `init_sdram_*` training path drives this.
    pub sdramc: Sdramc,
    /// Legacy SDRAM-controller register interface (`0x7E00_1000`) — DRAM timing
    /// table plus lock/ready bits polled after PHY training.
    pub sdc: Sdc,
    /// Boot-info handoff doorbell (`0x7EE0_2000`).
    pub bootbox: BootBox,
    /// Scalar VPU accesses that are not naturally aligned, which the model
    /// performs and the core cannot (`--check-alignment`).
    pub alignment: crate::align::Alignment,
    /// Legacy DMA controller (`0x7E00_7000`) — start4's bulk memory copies.
    pub dma_legacy: crate::periph::dma_legacy::DmaLegacy,
    /// The `0x7EE0_4100` DMA controller (channel 15 at `0x7EE0_5000`).
    pub dma_vpu: crate::periph::dma_legacy::DmaLegacy,
    /// DMA4 channel (`0x7E00_7B00`) — the bootloader scrubs / moves DRAM through
    /// it; [`Machine::store`] runs the control-block chain after a `CS` write.
    pub dma4: Dma4,
    /// The legacy EMMC controller (`0x7E30_0000`) — the SD host of 2020-era
    /// bootcode. The card is on its bus only while [`Self::sd_slot_legacy`].
    pub emmc: Emmc2,
    /// Bit 1 of the SD-slot mux word at GPIO `+0xD0` (`0x7E20_00D0`): the card
    /// is routed to the legacy EMMC instead of EMMC2 (#66). See
    /// [`Self::route_sd_slot`].
    pub sd_slot_legacy: bool,
    /// EMMC2 SD host controller (`0x7E34_0000`).
    pub emmc2: Emmc2,
    /// HVS (`0x7E40_0000`) — display frame-swap registers auto-complete.
    pub hvs: Hvs,
    /// The control block at `0x7E80_8000` — the power acknowledge start4's USB
    /// power-on waits for.
    pub hd: Hd,
    /// DWC2 USB OTG controller (`0x7E98_0000`) — reset when USB power comes on.
    pub dwc2: Dwc2,
    /// The BCM2711's own xHCI (`0x7E9C_0000`) — the USB-C port as a USB 2.0
    /// host, which `--otg` plugs a stick into (#113).
    pub xhci_otg: XhciOtg,
    /// Catch-all for the rest of the peripheral window.
    pub periph_stub: StubRegion,
    pub console: Console,

    /// Total peripheral accesses that missed a real device (fell through to the
    /// stub). A quick health signal for how much firmware behaviour is faked.
    pub stub_hits: u64,
    pub bus_errors: u64,
    /// Total store operations (any address). A liveness signal for the run loop:
    /// a loop that keeps writing memory is making progress, not spinning.
    pub ram_writes: u64,
    pub mmio_writes: u64,
    /// Loads that resolved to RAM. Lets the run loop tell a bounded memory scan
    /// (a DRAM memtest read-back walks fresh addresses, no writes) from a hung
    /// poll (re-reads one MMIO register forever).
    pub ram_reads: u64,
    /// Loads off the RAM path, the system timer's included. The run
    /// loop's busy-wait detector compares it with the timer's `clo_reads`: a
    /// `udelay` reads nothing else.
    pub mmio_reads: u64,

    /// When set, every peripheral (non-RAM) access is appended to `mmio_events`
    /// as `(addr, width_bytes, value, is_write)`. The run loop drains and prints
    /// it tagged with the current PC. A reconnaissance aid for unmodelled blocks.
    pub mmio_trace: bool,
    /// Optional `[lo, hi)` address filter for `mmio_trace`. Tracing every
    /// peripheral access across a whole boot buries the one block under
    /// investigation in millions of unrelated lines (and costs more time than
    /// the wall-clock budget has); with this set only accesses inside the
    /// range are recorded. `RVF_TRACE_MMIO=<lo>-<hi>` sets it.
    pub mmio_trace_range: Option<(u32, u32)>,
    pub mmio_events: Vec<(u32, u8, u32, bool)>,

    /// Reconnaissance aid: when `RVF_WATCH=<hex>[,<hex>...]` is set, every store
    /// whose word-aligned address matches one of them is logged to stderr tagged
    /// with the current PC (`watch_pc`, refreshed by the run loop each step).
    /// Complements `mmio_trace` for pinning down who writes a given RAM word.
    pub watch: Vec<u32>,
    /// Where the machine's channels go ([`Self::set_log`]): [`Channel::Dma`]
    /// is the machine's own, the rest belong to the devices it hands a clone.
    pub log: Log,
    /// Interrupt sources raised by peripherals, waiting to be vectored. A
    /// source stays here until core 0's CoreCtl bank enables it.
    pending_irqs: std::collections::VecDeque<u32>,
    /// The same for core 1: sources start4 raised for it in software.
    pending_irqs1: std::collections::VecDeque<u32>,
    /// Something happened that the run loop's per-step checks may have to
    /// act on: a peripheral register was written, an interrupt was queued, a
    /// compare fired, or a reset came due. The run loop clears it; while it
    /// stays clear, those checks have nothing to do (`Emulator::fast_steps`).
    /// `Vpu::recheck` is the same flag for the core's own state.
    pub recheck: bool,
    /// The ARM runs alongside: a VPU `sleep` leaves its jump to the next
    /// compare in [`Self::sleep_to`] for `Emulator::step_arm`, which moves
    /// the counter only as far as the first ARM write the VPU would wake for
    /// (#53).
    pub defer_sleep: bool,
    pub sleep_to: Option<u64>,
    pub watch_pc: u32,
    /// Which VPU core `watch_pc` belongs to.
    pub watch_core: u32,

    /// `start4.elf` logs boot progress by writing 4-char ASCII tags (`_msh`,
    /// `_osh`, `bfsp`, ...) to a register at `0xCEC0_2000`. We capture the
    /// sequence — it is the closest thing to an early-boot log before any UART
    /// is up. Only a `--features diag` build does: the address is start4's, and
    /// outside diagnostics the model knows nothing about start4 (#25).
    pub phase_tags: Vec<u32>,
}

/// `start4.elf` writes 4-char boot-progress tags to `0x?EC0_2000`. Direct-ELF
/// load runs it at `0xCEC0_0000` (tags `0xCEC0_2000`); the real bootloader
/// relocates it to `0xFEC0_0000` (tags `0xFEC0_2000`). Both share the low-26-bit
/// signature `0x02C0_2000` (each `0x?C00_0000` alias is 64 MiB), so match on
/// that regardless of which alias the write used.
const PHASE_TAG_SIG: u32 = 0x02C0_2000;

/// The function-select registers of the GPIO block: a write to one of them can
/// move a peripheral's pads (see [`Machine::route_gpio_pins`]).
const GPFSEL_WINDOW: std::ops::Range<u32> = {
    let base = map::GPIO_BASE + crate::spec::gpio::GPFSEL;
    base..base + crate::spec::gpio::GPFSEL_COUNT * crate::spec::gpio::GPFSEL_STRIDE
};

/// The SD-slot mux word in the GPIO block, and the bit that routes the card to
/// the legacy EMMC (see [`Machine::route_sd_slot`]).
const SD_SLOT_MUX: u32 = map::GPIO_BASE + crate::spec::gpio::PIN_MUX;
const SD_SLOT_MUX_LEGACY: u32 = crate::spec::gpio::PIN_MUX_SD_LEGACY_MASK;

/// The L2's maintenance port (`specs/bootbox.toml`): the bootcode's flush
/// ends its cache-as-RAM window ([`crate::l2`], #70).
const L2_CTRL: u32 = map::BOOTBOX_BASE + crate::spec::bootbox::L2_CTRL;
const L2_FLUSH: u32 = crate::spec::bootbox::L2_CTRL_FLUSH_MASK;

/// The 64 MiB every peripheral window the VPU decodes lies in.
const MMIO_WINDOW: std::ops::Range<u32> = 0x7C00_0000..0x8000_0000;
const _: () = {
    let windows = [
        (map::PERIPH_BASE, map::PERIPH_SIZE),
        (map::SDRAMC_BASE, map::SDRAMC_SIZE),
        (map::CLKMON_BASE, map::CLKMON_SIZE),
        (map::PCIE_BASE, map::PCIE_SIZE),
        (map::GENET_BASE, map::GENET_SIZE),
    ];
    let mut i = 0;
    while i < windows.len() {
        let (base, size) = windows[i];
        assert!(base >= MMIO_WINDOW.start && size <= MMIO_WINDOW.end - base);
        i += 1;
    }
};

impl Machine {
    pub fn new(ram_bytes: usize) -> Machine {
        Machine {
            ram: Ram::new(map::SDRAM_CACHED_BASE, ram_bytes),
            boot_rom: None,
            l2: Default::default(),
            systimer: SysTimer::new(),
            uart0: Pl011::new(),
            aux: Aux::new(),
            mbox: Mbox::new(),
            armctrl: ArmCtrl::new(),
            arm_local: ArmLocal::new(),
            gic: Gic::new(),
            mcsync: McSync::new(),
            corectl: CoreCtl::new(),
            pm: Pm::new(),
            clockman: ClockManager::new(),
            clkmon: ClkMon::new(),
            avs: Avs::new(),
            pvt: crate::periph::Pvt::new(),
            asb: Asb::new(),
            pcie: crate::periph::pcie::Pcie::new(),
            genet: crate::periph::Genet::new(),
            net: None,
            rng: Rng::new(),
            vce: Vce::new(),
            bsc0: {
                // Every pin is an input out of reset, so no master has its
                // pads until the firmware says so (`route_gpio_pins`).
                let mut bsc0 = Bsc::empty("bsc0");
                bsc0.set_pins(false);
                bsc0
            },
            gpio: Gpio::new(),
            spi0: {
                let mut spi0 = Spi0::new();
                spi0.set_pins(false);
                spi0
            },
            bsc_pmic: Bsc::new("bsc-pmic"),
            hdmi_ddc0: HdmiDdc::new("hdmi-ddc0"),
            hdmi_ddc1: HdmiDdc::new("hdmi-ddc1"),
            hdmi0: Hdmi::new("hdmi0"),
            hdmi1: Hdmi::new("hdmi1"),
            board: crate::soc::Board::default(),
            config_otp: ConfigOtp::new(),
            sdramc: Sdramc::new(),
            sdc: Sdc::new(),
            bootbox: BootBox::new(),
            alignment: crate::align::Alignment::off(),
            dma4: Dma4::new(),
            dma_legacy: crate::periph::dma_legacy::DmaLegacy::new(),
            dma_vpu: crate::periph::dma_legacy::DmaLegacy::new_vpu(),
            emmc: Emmc2::new_legacy(),
            sd_slot_legacy: false,
            emmc2: Emmc2::new(),
            hvs: Hvs::new(),
            hd: Hd::new(),
            dwc2: Dwc2::new(),
            xhci_otg: XhciOtg::new(),
            periph_stub: StubRegion::new("periph-window"),
            console: Console::default(),
            stub_hits: 0,
            bus_errors: 0,
            ram_writes: 0,
            mmio_writes: 0,
            ram_reads: 0,
            mmio_reads: 0,
            mmio_trace: false,
            mmio_trace_range: None,
            mmio_events: Vec::new(),
            watch: std::env::var("RVF_WATCH")
                .ok()
                .filter(|_| crate::diag::ON)
                .map(|v| {
                    v.split(',')
                        .filter_map(|t| {
                            u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok()
                        })
                        .map(|a| a & !3)
                        .collect()
                })
                .unwrap_or_default(),
            log: Log::default(),
            pending_irqs: std::collections::VecDeque::new(),
            pending_irqs1: std::collections::VecDeque::new(),
            recheck: false,
            defer_sleep: false,
            sleep_to: None,
            watch_pc: 0,
            watch_core: 0,
            phase_tags: Vec::new(),
        }
    }

    /// The board this machine is, and the stepping on it.
    pub fn board(&self) -> crate::soc::Board {
        self.board
    }

    /// Make this machine `board`: OTP row 30 takes its revision code, the
    /// PMIC bus the PMICs it has fitted, and cores an
    /// [`crate::emulator::Emulator`] builds afterwards its stepping's
    /// `version`. Call it before anything runs; firmware reads the revision
    /// once, early.
    pub fn set_board(&mut self, board: crate::soc::Board) {
        self.board = board;
        self.config_otp.set(30, board.revision);
        // The DRAM parts are the ones a board of that size is fitted with, so
        // the firmware trains and publishes the memory the revision claims.
        self.sdc = Sdc::with_dram(crate::periph::sdc::Dram::for_memory(board.memory_bytes()));
        self.bsc_pmic
            .fit_pmics(crate::periph::Pmic::for_board(board));
        self.gpio.fit_board(board);
    }

    /// Send the machine's channels to `log` (#95): keep it for the machine's
    /// own, and hand every device that logs a clone.
    pub fn set_log(&mut self, log: Log) {
        self.systimer.set_log(log.clone());
        self.mbox.log = log.clone();
        self.corectl.log = log.clone();
        self.pcie.set_log(log.clone());
        self.spi0.log = log.clone();
        self.gpio.log = log.clone();
        self.bsc_pmic.set_log(log.clone());
        self.config_otp.log = log.clone();
        self.emmc.log = log.clone();
        self.emmc2.log = log.clone();
        self.dwc2.log = log.clone();
        self.xhci_otg.set_log(log.clone());
        self.log = log;
    }

    /// Plug GENET's cable into `backend`: the PHY sees a link partner, and
    /// frames flow between the DMA rings and `backend`.
    pub fn attach_net(&mut self, backend: Box<dyn crate::net::NetBackend>) {
        self.genet.phy.set_link(true);
        self.net = Some(backend);
    }

    /// Queue an interrupt source for core 0. It is delivered on the next step
    /// if core 0's CoreCtl bank enables it, and waits for that otherwise.
    pub fn push_pending_irq(&mut self, src: u32) {
        self.pending_irqs.push_back(src);
        self.recheck = true;
    }

    /// Is an interrupt source queued for core 0 that its CoreCtl bank enables?
    pub fn irq_queued(&self) -> bool {
        self.pending_irqs
            .iter()
            .any(|&src| self.corectl.irq_priority(0, src) != 0)
    }

    /// Queue an interrupt source start4 raised for core 1. Like core 0's, it
    /// waits until core 1's CoreCtl bank enables it, and until core 1 can take
    /// it (`Emulator::step_core1`), instead of being lost.
    pub fn push_core1_irq(&mut self, src: u32) {
        self.pending_irqs1.push_back(src);
        self.recheck = true;
    }

    /// Take the first source queued for core 1 that core 1's bank enables.
    pub fn take_core1_irq(&mut self) -> Option<u32> {
        let i = self
            .pending_irqs1
            .iter()
            .position(|&src| self.corectl.irq_priority(1, src) != 0)?;
        self.pending_irqs1.remove(i)
    }

    /// The lowest system-timer channel whose compare has fired and whose
    /// source, `SYS_IRQ_SRC + channel`, core 0's CoreCtl bank enables. A
    /// channel that fired with its source disabled stays latched, and doesn't
    /// hold up the channels above it.
    fn timer_channel_due(&self) -> Option<u8> {
        if !self.systimer.tick_pending() {
            return None;
        }
        (0..4u8).find(|&c| {
            self.systimer.channel_pending(c)
                && self
                    .corectl
                    .irq_priority(0, crate::periph::corectl::SYS_IRQ_SRC + c as u32)
                    != 0
        })
    }

    /// Has a compare fired whose interrupt core 0 can take?
    pub fn timer_irq_due(&self) -> bool {
        self.timer_channel_due().is_some()
    }

    /// Settle any I²C transfer whose time on the wire has elapsed.
    fn advance_i2c(&mut self) {
        let now = self.systimer.now_us();
        self.bsc_pmic.advance_to(now);
        self.bsc0.advance_to(now);
    }

    /// Let the SD hosts deliver a PIO block that has come due, for a driver
    /// that waits for the interrupt rather than polling (#109).
    fn advance_sd(&mut self) {
        let now = self.systimer.now_us();
        self.emmc2.advance_to(now);
        self.emmc.advance_to(now);
    }

    /// Let the PCIe endpoint's clock catch up, so a USB3 link it is training
    /// comes up on time even while nothing polls it.
    fn advance_pcie(&mut self) {
        let now = self.systimer.now_us();
        self.with_dma_master("the xHCI / VL805", |m| m.pcie.advance_to(now, &mut m.ram));
    }

    /// Settle the HDMI DDC masters, which time their transfers the same way.
    ///
    /// Unlike the BSCs these are advanced lazily, on the way into their own
    /// registers, rather than out of [`Machine::tick`]: nothing but the
    /// firmware's own status poll ever observes them, and two more calls on
    /// every single retired instruction cost a measurable few percent of the
    /// model's throughput.
    fn advance_hdmi_ddc(&mut self, addr: u32) {
        let in_window = |base: u32, size: u32| addr >= base && addr < base + size;
        if in_window(map::HDMI_DDC0_BASE, map::HDMI_DDC_SIZE)
            || in_window(map::HDMI_DDC1_BASE, map::HDMI_DDC_SIZE)
            || in_window(map::HDMI_AUTO_I2C0_BASE, map::HDMI_AUTO_I2C_SIZE)
            || in_window(map::HDMI_AUTO_I2C1_BASE, map::HDMI_AUTO_I2C_SIZE)
        {
            let now = self.systimer.now_us();
            self.hdmi_ddc0.advance_to(now);
            self.hdmi_ddc1.advance_to(now);
        }
    }

    /// Mirror the core rail's PMIC setpoint into the AVS monitor before a read
    /// of that block.
    ///
    /// Channel 3 of the AVS monitor is a voltage sensor sitting on the SoC core
    /// rail, and the rail is driven over I²C by a board PMIC (the `0x1E` one,
    /// or the `0x1D` one on a 4B rev 1.1/1.2 — see [`crate::periph::pmic`]).
    /// start4's DVFS
    /// calibration `FUN_0ec303e8` programs two voltages an appreciable step
    /// apart and requires the sensor to report a difference of at least 10 mV
    /// between them; a channel that answers with one fixed count reads as a
    /// rail that does not respond, and the calibration gives up. The two
    /// devices are in different windows, so the tie between them has to be made
    /// here.
    fn sync_avs_core_rail(&mut self, addr: u32) {
        if !(map::AVS_BASE..map::AVS_BASE + map::AVS_SIZE).contains(&addr) {
            return;
        }
        if let Some(uv) = self.bsc_pmic.slave().and_then(|p| p.core_rail_uv()) {
            self.avs.set_core_rail_uv(uv);
        }
    }

    /// Advance time-based peripheral state by `cycles` VPU cycles.
    ///
    /// Inline, because the run loop calls it on every step and on 53 of every
    /// 54 there is nothing to do past the first comparison.
    #[inline]
    pub fn tick(&mut self, cycles: u64) {
        // Everything below is derived from the microsecond counter, so when it
        // has not moved there is nothing for any of it to do.
        if self.systimer.advance(cycles) {
            self.tick_us();
        }
    }

    /// The rest of [`Self::tick`], for a step that moved the microsecond count.
    #[inline(never)]
    fn tick_us(&mut self) {
        if self.systimer.take_fired() {
            self.recheck = true;
        }
        self.pm.advance(self.systimer.now_us());
        if self.pm.reset_pending() {
            self.recheck = true;
        }
        // A backend with its own clock (a host network) can deliver a frame
        // at any time, not only in reply to a register write.
        if self.net.is_some() {
            let now = self.systimer.now_us();
            self.with_dma_master("the GENET", |m| {
                m.genet.service(now, &mut m.ram, &mut m.net)
            });
        }
        // The I²C masters time their transfers in microseconds off the system
        // timer, so they stay in step with it across the run loop's `sleep`
        // fast-forward (which jumps the counter without retiring cycles).
        self.advance_i2c();
        self.advance_sd();
        self.advance_pcie();
        // The RNG holds its line asserted while an enabled `INT_STATUS` bit is
        // set; start4's handler for source 125 disables the FIFO interrupt
        // again and releases the gate its read op waits on. Only ever keep one
        // delivery outstanding.
        let src = crate::periph::rng::IRQ_SRC;
        if self.rng.irq_asserted() && !self.pending_irqs.contains(&src) {
            self.push_pending_irq(src);
        }
        // The mailbox holds source 94 asserted while a request is queued for
        // the firmware and its driver has armed the interrupt. `0x3EC58302`
        // reads the pending word, dispatches to the registered callback, and
        // the `mbox_read` task takes the message off the FIFO.
        let src = crate::periph::mbox::IRQ_SRC;
        if self.mbox.irq_asserted() && !self.pending_irqs.contains(&src) {
            self.push_pending_irq(src);
        }
        // The VCE holds source 68 asserted from the moment a launch completes
        // until start4's handler (`0x3ED9D1EA`) acks it through `INTCLR`; that
        // handler is what sets the event flag `vce_run` is waiting on.
        let src = crate::periph::vce::IRQ_SRC;
        if self.vce.irq_asserted() && !self.pending_irqs.contains(&src) {
            self.push_pending_irq(src);
        }
        self.advance_hvs();
    }

    /// Let the HVS finish the frames the counter has reached, and hold source
    /// 97 asserted while an end-of-frame flag it interrupts for is set.
    /// start4's handler (`0x3ECEED5C`) clears the flag, and it is also what
    /// completes a display pause — `NOTIFY_DISPLAY_DONE` waits on one (#61).
    fn advance_hvs(&mut self) {
        self.hvs.advance_to(self.systimer.now_us());
        let src = crate::periph::hvs::IRQ_SRC;
        if self.hvs.irq_asserted() && !self.pending_irqs.contains(&src) {
            self.push_pending_irq(src);
        }
    }

    /// Drain and return whatever the console UART has transmitted.
    pub fn take_console_output(&mut self) -> Vec<u8> {
        match self.console {
            Console::Pl011 => self.uart0.take_output(),
            Console::MiniUart => self.aux.take_output(),
        }
    }

    pub fn irq_pending(&self) -> bool {
        self.uart0.irq_pending() || self.aux.irq_pending()
    }

    /// The VPU addresses peripherals only through the `0x7E00_0000` window (no
    /// cache aliasing, unlike RAM) — plus the LPDDR4 controller/PHY, which is
    /// mapped just *below* that window at `0x7DC0_0000`. Both must be decoded
    /// before the cache-alias fold, or their (aliased) addresses land in DRAM.
    #[inline]
    fn in_mmio(addr: u32) -> bool {
        // Every window below is inside `MMIO_WINDOW`, and nearly every access
        // (RAM) is outside it.
        addr >> 26 == MMIO_WINDOW.start >> 26
            && ((map::PERIPH_BASE..map::PERIPH_BASE + map::PERIPH_SIZE).contains(&addr)
                || (map::SDRAMC_BASE..map::SDRAMC_BASE + map::SDRAMC_SIZE).contains(&addr)
                || (map::CLKMON_BASE..map::CLKMON_BASE + map::CLKMON_SIZE).contains(&addr)
                || (map::PCIE_BASE..map::PCIE_BASE + map::PCIE_SIZE).contains(&addr)
                || (map::GENET_BASE..map::GENET_BASE + map::GENET_SIZE).contains(&addr))
    }

    /// Map a real boot-ROM image at `0x6000_0000` so the VPU can execute the
    /// maskROM from its reset vector (experimental `--boot-rom`). Only the
    /// code+rodata region is overlaid — the salt and SHA constants live there,
    /// while the ROM's BSS/scratch above it and its bootcode staging at
    /// `0x8000_0000` must stay writable DRAM. See the [`boot_rom`](Self::boot_rom)
    /// field.
    pub fn attach_boot_rom(&mut self, bytes: Vec<u8>) {
        const BASE: u32 = 0x6000_0000;
        const CODE_LEN: u32 = 0x8000;
        let len = (bytes.len() as u32).min(CODE_LEN);
        self.boot_rom = Some((BASE, BASE + len, bytes));
        // The ROM stages the bootcode into the L2 with ordinary stores.
        self.l2.hold(0, 0);
    }

    /// If a boot-ROM overlay covers `addr`, the byte offset into its image.
    #[inline]
    fn boot_rom_at(&self, addr: u32) -> Option<usize> {
        let (base, end, _) = self.boot_rom.as_ref()?;
        (*base..*end)
            .contains(&addr)
            .then(|| (addr - *base) as usize)
    }

    /// Read `width` bytes little-endian out of the boot-ROM overlay.
    fn boot_rom_load(&self, off: usize, width: Width) -> u32 {
        let bytes = &self.boot_rom.as_ref().expect("overlay present").2;
        let mut v = 0u32;
        for i in 0..width.bytes() as usize {
            v |= u32::from(bytes.get(off + i).copied().unwrap_or(0)) << (8 * i);
        }
        v
    }

    /// Should an access to `addr` be recorded in `mmio_events`? True when the
    /// trace is on and `addr` passes `mmio_trace_range`, if one is set.
    fn mmio_traced(&self, addr: u32) -> bool {
        crate::diag::ON
            && self.mmio_trace
            && self
                .mmio_trace_range
                .is_none_or(|(lo, hi)| (lo..hi).contains(&addr))
    }

    /// Run `f` with the tracker told that this peripheral, not the VPU, is
    /// the one using memory: it reads and writes behind the caches. Nests —
    /// a DMA engine's own doorbell can set another one going.
    #[inline]
    fn with_dma_master(&mut self, who: &'static str, f: impl FnOnce(&mut Machine)) {
        if !self.ram.coherency.is_on() {
            f(self);
            return;
        }
        self.with_master(crate::coherency::Master::Dma(who), f);
    }

    /// The same for an engine on the VPU's side of the L2.
    #[inline]
    fn with_vc4_dma_master(&mut self, who: &'static str, f: impl FnOnce(&mut Machine)) {
        self.with_master(crate::coherency::Master::Vc4Dma(who), f);
    }

    #[inline]
    fn with_master(&mut self, who: crate::coherency::Master, f: impl FnOnce(&mut Machine)) {
        if !self.ram.coherency.is_on() {
            f(self);
            return;
        }
        let was = self.ram.coherency.masters();
        self.ram.coherency.set_master(who);
        f(self);
        self.ram.coherency.set_masters(was.0, was.1);
    }

    /// What a legacy-DMA bus address means for the caches. That engine sits
    /// behind the L2, which is where a cached VPU access lands as well, so a
    /// transfer only goes past the caches when its address is in the alias
    /// that bypasses them (`0xC000_0000`). Stock's `dma_memcpy` copies
    /// megabytes through the `0x0` alias and reads them straight back cached,
    /// which is only sound because of this.
    #[inline]
    fn dma_master(addr: u32, who: &'static str) -> crate::coherency::Master {
        if addr >> 30 == 3 {
            crate::coherency::Master::Dma(who)
        } else {
            crate::coherency::Master::Vc4Dma(who)
        }
    }

    /// Fold the four VC4 cache aliases (`0x0`, `0x4000_0000`, `0x8000_0000`,
    /// `0xC000_0000`) of physical memory onto a single backing store. Before
    /// SDRAM training this backing *is* the L2-as-SRAM the bootcode runs from;
    /// afterwards it stands in for DRAM. Until the bootcode flushes the L2, an
    /// uncached write does not reach the lines it holds ([`crate::l2`]).
    fn fold_ram_addr(addr: u32) -> u32 {
        addr & 0x3FFF_FFFF
    }

    /// Resolve an address to `(device, offset)`, or `None` for RAM / unmapped.
    fn device_for(&mut self, addr: u32) -> Option<(&mut dyn MmioDevice, u32)> {
        let a = addr;
        let hit = |base: u32, size: u32| {
            if a >= base && a < base + size {
                Some(a - base)
            } else {
                None
            }
        };

        if let Some(off) = hit(map::SYSTIMER_BASE, map::SYSTIMER_SIZE) {
            return Some((&mut self.systimer, off));
        }
        if let Some(off) = hit(map::UART0_BASE, map::UART0_SIZE) {
            return Some((&mut self.uart0, off));
        }
        if let Some(off) = hit(map::AUX_BASE, map::AUX_SIZE) {
            return Some((&mut self.aux, off));
        }
        if let Some(off) = hit(map::MBOX_BASE, map::MBOX_SIZE) {
            return Some((&mut self.mbox, off));
        }
        if let Some(off) = hit(map::ARMCTRL_BASE, map::ARMCTRL_SIZE) {
            return Some((&mut self.armctrl, off));
        }
        if let Some(off) = hit(map::MCSYNC_BASE, map::MCSYNC_SIZE) {
            return Some((&mut self.mcsync, off));
        }
        if let Some(off) = hit(map::CORECTL_BASE, map::CORECTL_SIZE) {
            return Some((&mut self.corectl, off));
        }
        if let Some(off) = hit(map::SDC_BASE, map::SDC_SIZE) {
            return Some((&mut self.sdc, off));
        }
        if let Some(off) = hit(map::DMA4_BASE, map::DMA4_SIZE) {
            return Some((&mut self.dma4, off));
        }
        if let Some(off) = hit(map::DMA_LEGACY_BASE, map::DMA_LEGACY_SIZE) {
            return Some((&mut self.dma_legacy, off));
        }
        if let Some(off) = hit(map::DMA_VPU_BASE, map::DMA_VPU_SIZE) {
            return Some((&mut self.dma_vpu, off));
        }
        if let Some(off) = hit(map::EMMC_BASE, map::EMMC_SIZE) {
            return Some((&mut self.emmc, off));
        }
        if let Some(off) = hit(map::EMMC2_BASE, map::EMMC2_SIZE) {
            return Some((&mut self.emmc2, off));
        }
        if let Some(off) = hit(map::HVS_BASE, map::HVS_SIZE) {
            return Some((&mut self.hvs, off));
        }
        if let Some(off) = hit(map::HD_BASE, map::HD_SIZE) {
            return Some((&mut self.hd, off));
        }
        if let Some(off) = hit(map::DWC2_BASE, map::DWC2_SIZE) {
            return Some((&mut self.dwc2, off));
        }
        if let Some(off) = hit(map::XHCI_OTG_BASE, map::XHCI_OTG_SIZE) {
            return Some((&mut self.xhci_otg, off));
        }
        if let Some(off) = hit(map::BOOTBOX_BASE, map::BOOTBOX_SIZE) {
            return Some((&mut self.bootbox, off));
        }
        if let Some(off) = hit(map::PM_BASE, map::PM_SIZE) {
            return Some((&mut self.pm, off));
        }
        if let Some(off) = hit(map::CM_BASE, map::CM_SIZE) {
            return Some((&mut self.clockman, off));
        }
        if let Some(off) = hit(map::RNG_BASE, map::RNG_SIZE) {
            return Some((&mut self.rng, off));
        }
        // Both VCE windows address the one device; the control block keeps its
        // aperture-relative offset (`0x4_0000`).
        if hit(map::VCE_BASE, map::VCE_MEM_SIZE).is_some()
            || hit(map::VCE_CTRL_BASE, map::VCE_CTRL_SIZE).is_some()
        {
            return Some((&mut self.vce, a - map::VCE_BASE));
        }
        if let Some(off) = hit(map::AVS_BASE, map::AVS_SIZE) {
            return Some((&mut self.avs, off));
        }
        if let Some(off) = hit(map::PVT_BASE, map::PVT_SIZE) {
            return Some((&mut self.pvt, off));
        }
        if let Some(off) = hit(map::ASB_BASE, map::ASB_SIZE) {
            return Some((&mut self.asb, off));
        }
        if let Some(off) = hit(map::CLKMON_BASE, map::CLKMON_SIZE) {
            return Some((&mut self.clkmon, off));
        }
        if let Some(off) = hit(map::SPI0_BASE, map::SPI0_SIZE) {
            return Some((&mut self.spi0, off));
        }
        if let Some(off) = hit(map::GPIO_BASE, map::GPIO_SIZE) {
            return Some((&mut self.gpio, off));
        }
        if let Some(off) = hit(map::BSC0_BASE, map::BSC0_SIZE) {
            return Some((&mut self.bsc0, off));
        }
        if let Some(off) = hit(map::BSC_PMIC_BASE, map::BSC_PMIC_SIZE) {
            return Some((&mut self.bsc_pmic, off));
        }
        if let Some(off) = hit(map::HDMI_DDC0_BASE, map::HDMI_DDC_SIZE) {
            return Some((&mut self.hdmi_ddc0, off));
        }
        if let Some(off) = hit(map::HDMI_DDC1_BASE, map::HDMI_DDC_SIZE) {
            return Some((&mut self.hdmi_ddc1, off));
        }
        if let Some(off) = hit(map::HDMI_AUTO_I2C0_BASE, map::HDMI_AUTO_I2C_SIZE) {
            return Some((&mut self.hdmi_ddc0, AUTO_WINDOW + off));
        }
        if let Some(off) = hit(map::HDMI_AUTO_I2C1_BASE, map::HDMI_AUTO_I2C_SIZE) {
            return Some((&mut self.hdmi_ddc1, AUTO_WINDOW + off));
        }
        if let Some(off) = hit(map::HDMI0_BASE, map::HDMI_SIZE) {
            return Some((&mut self.hdmi0, off));
        }
        if let Some(off) = hit(map::HDMI1_BASE, map::HDMI_SIZE) {
            return Some((&mut self.hdmi1, off));
        }
        if let Some(off) = hit(map::OTP_BASE, map::OTP_SIZE) {
            return Some((&mut self.config_otp, off));
        }
        if let Some(off) = hit(map::SDRAMC_BASE, map::SDRAMC_SIZE) {
            return Some((&mut self.sdramc, off));
        }
        if let Some(off) = hit(map::PCIE_BASE, map::PCIE_SIZE) {
            return Some((&mut self.pcie, off));
        }
        if let Some(off) = hit(map::GENET_BASE, map::GENET_SIZE) {
            return Some((&mut self.genet, off));
        }

        if (map::PERIPH_BASE..map::PERIPH_BASE + map::PERIPH_SIZE).contains(&a) {
            self.stub_hits += 1;
            return Some((&mut self.periph_stub, a - map::PERIPH_BASE));
        }
        None
    }

    /// Run the DMA4 control-block chain the channel was just armed with. A word
    /// in RAM (`+0x08` `SRCI` bit 12 = "source increments"): clear ⇒ fill `DEST`
    /// with the single word at `SRC` (`SRC == 0` ⇒ zero-fill scrub); set ⇒ copy
    /// `SRC`→`DEST`. Both addresses are 40 bits wide — `SRCI`/`DESTI` bits
    /// `[7:0]` carry bits `[39:32]` — and an address the PCIe outbound window
    /// covers reaches the VL805's registers instead of DRAM.
    /// [`Channel::Dma`]: every access to the legacy DMA controller window
    /// (`0x7E00_7000..0x7E00_8000`, 15 channels x 0x100). Only channel 11
    /// (DMA4, `0x7E00_7B00`) is modelled; start4's `dma_memcpy` uses one of the
    /// others, so those accesses currently fall through to the catch-all stub.
    fn dma_win_log(&self, rw: &str, addr: u32, value: u32) {
        if crate::diag::ON && (0x7E00_7000..0x7E00_8000).contains(&addr) {
            let ch = (addr - 0x7E00_7000) / 0x100;
            crate::log!(
                self.log,
                Channel::Dma,
                "window {rw} ch{ch} +{:#04x} ({addr:#x}) = {value:#x} pc={:#x}",
                (addr - 0x7E00_7000) % 0x100,
                self.watch_pc
            );
        }
    }

    /// Execute the control-block chain armed on legacy DMA channel `ch`.
    ///
    /// CB layout (32 bytes): `+0x00 TI  +0x04 SOURCE_AD  +0x08 DEST_AD
    /// +0x0C TXFR_LEN  +0x10 STRIDE  +0x14 NEXTCONBK`. Bus addresses are folded
    /// onto flat DRAM the same way the CPU's are.
    fn run_dma_legacy(&mut self, ch: usize, vpu: bool) {
        use crate::periph::dma_legacy::DmaLegacy;

        let raw = if vpu {
            self.dma_vpu.conblk_ad(ch)
        } else {
            self.dma_legacy.conblk_ad(ch)
        };
        // `dma_chain_start` (`0x3EC97544`) writes `CONBLK_AD` two ways:
        //
        //   legacy channel: *(base + ch*0x100 + 4) = cb            (raw pointer)
        //   40-bit channel: cb>>30 == 3 ? (cb & 0x3fffffff) >> 5
        //                               : (cb >> 5) | 0x20000000
        //
        // Both shifted forms leave the top two bits clear, and a raw VC4
        // pointer always has an alias in them, so that is the discriminator.
        // Shifting back by 5 restores the alias bits for the `| 0x20000000`
        // form (0x25F7B6A5 << 5 == 0xBEF6D4A0).
        let cb_addr = if raw >> 30 != 0 { raw } else { raw << 5 };
        let who = if vpu { "the VPU DMA" } else { "the legacy DMA" };
        // The control block comes through the same alias its address is in.
        let fetch = Machine::dma_master(cb_addr, who);
        let mut cb = cb_addr & 0x3FFF_FFFF;
        for _ in 0..4096 {
            if cb == 0 || !self.ram.contains(cb) {
                break;
            }
            self.ram.coherency.set_master(fetch);
            let mut w = [0u32; 7];
            for (i, slot) in w.iter_mut().enumerate() {
                *slot = self.ram.load(cb + (i as u32) * 4, Width::Word).unwrap_or(0);
            }
            // Channel 15 of the `0x7EE0_4100` controller is a 40-bit ("dma40")
            // channel on C0 and a legacy one on B0 (#77). On C0 `dma_memcpy`
            // builds its CB with `dma_transfer_setup_memcpy_vpu40`
            // (`0x3EC99EE0`), in the DMA4 control-block layout:
            //   +0x00 TI  +0x04 SRC  +0x08 SRCI  +0x0C DEST  +0x10 DESTI
            //   +0x14 LEN +0x18 NEXT_CB(>>5)
            // On B0 it uses `dma_transfer_setup_memcpy` (`0x3EC99A7C`) and the
            // legacy layout, increments and all.
            let dma40 = vpu && self.board.stepping.dma_channel_15_is_40_bit();
            let d = if dma40 {
                crate::periph::dma_legacy::Cb {
                    ti: w[0],
                    src: w[1],
                    dest: w[3],
                    len: w[5],
                    stride: 0,
                    next: w[6] << 5,
                }
            } else {
                DmaLegacy::decode_cb([w[0], w[1], w[2], w[3], w[4], w[5]])
            };
            if crate::diag::ON {
                crate::log!(
                    self.log,
                    Channel::Dma,
                    "legacy ch{ch} cb={cb:#x} ti={:#x} src={:#x} dest={:#x} len={:#x} stride={:#x} next={:#x}",
                    d.ti, d.src, d.dest, d.len, d.stride, d.next
                );
            }
            // Each end of the copy is on whichever side of the L2 its own bus
            // address says.
            self.ram.coherency.set_masters(
                Machine::dma_master(d.src, who),
                Machine::dma_master(d.dest, who),
            );
            let (rows, xlen) = d.rows();
            let (src_stride, dest_stride) = d.strides();
            let mut src = d.src & 0x3FFF_FFFF;
            let mut dest = d.dest & 0x3FFF_FFFF;
            for _ in 0..rows {
                for i in 0..xlen {
                    let sa = if dma40 || d.src_inc() {
                        src.wrapping_add(i)
                    } else {
                        src
                    };
                    let da = if dma40 || d.dest_inc() {
                        dest.wrapping_add(i)
                    } else {
                        dest
                    };
                    let b = self.ram.load(sa & 0x3FFF_FFFF, Width::Byte).unwrap_or(0);
                    let _ = self.ram.store(da & 0x3FFF_FFFF, Width::Byte, b);
                }
                // 2D mode advances by the row length plus the signed stride.
                src = src.wrapping_add(xlen).wrapping_add(src_stride as u32);
                dest = dest.wrapping_add(xlen).wrapping_add(dest_stride as u32);
            }
            cb = d.next & 0x3FFF_FFFF;
        }
        self.ram.coherency.set_master(crate::coherency::Master::Vpu);
        if vpu {
            self.dma_vpu.finish(ch);
        } else {
            self.dma_legacy.finish(ch);
        }
        // Completion interrupt. `dma_interrupt` (`0x3EC980E8`) maps the source
        // back to a channel and `dma_chan_interrupt` then retires the transfer,
        // signals its waiter and starts the next one in the queue.
        let src = dma_irq_source(ch);
        self.push_pending_irq(src);
    }

    /// One word from a 40-bit DMA4 address: endpoint MMIO if the PCIe root
    /// complex's outbound window covers it, DRAM otherwise.
    fn dma40_load(&mut self, addr: u64) -> u32 {
        // The window the firmware programs starts at `0x6_0000_0000`, so a
        // plain 32-bit address can only be DRAM. Checking that first keeps the
        // multi-megabyte DRAM scrubs off the translation path.
        if addr >> 32 != 0 {
            if let Some(v) = self.pcie.mmio_read(addr, Width::Word) {
                return v;
            }
        }
        self.ram
            .load((addr as u32) & 0x3FFF_FFFF, Width::Word)
            .unwrap_or(0)
    }

    /// One word to a 40-bit DMA4 address. Both what the engine itself writes
    /// and what the controller behind the window writes (a doorbell here sets
    /// an xHCI transfer going) come from behind the VPU's caches.
    fn dma40_store(&mut self, addr: u64, value: u32) {
        self.with_dma_master("the 40-bit DMA / xHCI", |m| {
            if addr >> 32 != 0 && m.pcie.mmio_write(addr, Width::Word, value, &mut m.ram) {
                return;
            }
            let _ = m.ram.store((addr as u32) & 0x3FFF_FFFF, Width::Word, value);
        });
    }

    fn run_dma4(&mut self) {
        const S_INC: u32 = 1 << 12;
        /// `SRC_INFO` / `DEST_INFO` bits `[7:0]` are address bits `[39:32]` —
        /// the whole point of the 40-bit channel, and how the firmware reaches
        /// the PCIe outbound window at `0x6_0000_0000` from a 32-bit core.
        const ADDR_HI: u32 = 0xFF;
        let rd = |ram: &Ram, addr: u32| ram.load(addr & 0x3FFF_FFFF, Width::Word).unwrap_or(0);

        let mut cb = self.dma4.cb_addr() & 0x3FFF_FFFF;
        let mut interrupt = false;
        for _ in 0..4096 {
            if cb == 0 || !self.ram.contains(cb) {
                break;
            }
            let src = rd(&self.ram, cb + 0x04);
            let srci = rd(&self.ram, cb + 0x08);
            let dest = rd(&self.ram, cb + 0x0C);
            let desti = rd(&self.ram, cb + 0x10);
            let len = rd(&self.ram, cb + 0x14);
            let next = rd(&self.ram, cb + 0x18);
            interrupt |= rd(&self.ram, cb) & crate::periph::dma4::TI_INTEN != 0;
            let src40 = (((srci & ADDR_HI) as u64) << 32) | src as u64;
            let dest40 = (((desti & ADDR_HI) as u64) << 32) | dest as u64;

            if crate::diag::ON {
                crate::log!(
                    self.log,
                    Channel::Dma,
                    "cb={cb:#x} ti={:#x} src={src:#x} srci={srci:#x} dest={dest:#x} len={len:#x} next={next:#x}",
                    rd(&self.ram, cb)
                );
            }
            let fill = srci & S_INC == 0;
            let fill_word = if src40 == 0 {
                0
            } else {
                self.dma40_load(src40)
            };
            let mut off = 0u32;
            while off < len {
                let v = if fill {
                    fill_word
                } else {
                    self.dma40_load(src40 + off as u64)
                };
                self.dma40_store(dest40 + off as u64, v);
                off = off.wrapping_add(4);
            }
            cb = (next << 5) & 0x3FFF_FFFF;
        }
        self.dma4.finish(interrupt);
        // DMA4 is channel 11 of the `0x7E00_7000` controller. The bootloader
        // polls (`TI` = 0, no INTEN); start4's dmalib asks for the interrupt
        // and starts its next chain from it.
        if interrupt {
            self.push_pending_irq(dma_irq_source(11));
        }
    }
}

/// The VPU interrupt source a DMA channel's completion raises.
///
/// start4's dmalib keeps its own source -> channel table (`gp+0x556b8`, read
/// by `dma_interrupt`) and registers `dma_interrupt` on exactly the sources it
/// uses: 81, 83, 86, 89, 92 and 95 for channels 1, 3, 6, 11, 14 and 15. That
/// is 80 + channel for the low channels, 78 + channel for the DMA4 channels
/// 11..14, and 95 for the VPU's channel 15 — the same wiring Linux's
/// `bcm2711.dtsi` gives the controller as far as I know, where channels 7/8
/// and 9/10 share a line each (not verified here: nothing on this bench uses
/// them).
fn dma_irq_source(ch: usize) -> u32 {
    match ch {
        0..=6 => 80 + ch as u32,
        7 | 8 => 87,
        9 | 10 => 88,
        11..=14 => 78 + ch as u32,
        _ => 95,
    }
}

impl Machine {
    /// [`Bus::load`] off the RAM path: a peripheral, or nothing. Kept out of
    /// line so that the RAM path inlines into the cores' executors.
    #[inline(never)]
    fn load_device(&mut self, addr: u32, width: Width) -> BusResult<u32> {
        self.mmio_reads = self.mmio_reads.wrapping_add(1);
        self.advance_hdmi_ddc(addr);
        self.sync_avs_core_rail(addr);
        let trace = self.mmio_traced(addr);
        if let Some((dev, off)) = self.device_for(addr) {
            let v = dev.read(off, width);
            let got = *v.as_ref().unwrap_or(&0);
            self.dma_win_log("rd", addr, got);
            if trace {
                self.mmio_events
                    .push((addr, width.bytes() as u8, got, false));
            }
            return v;
        }
        self.bus_errors += 1;
        Err(BusError::Unmapped {
            addr,
            width,
            write: false,
        })
    }

    /// The end of a VPU `sleep` that [`Bus::sleep_advance`] left to the ARM:
    /// the counter moves on to `us`, firing the compares it reaches, and what
    /// is timed against it catches up.
    pub fn wake_vpu_at(&mut self, us: u64) {
        self.systimer.advance_to(us);
        self.advance_i2c();
        self.advance_sd();
        self.advance_pcie();
        self.advance_hvs();
    }

    /// Where a VPU `sleep` ends: at the next compare, or at the next end of
    /// frame the HVS interrupts it for, whichever comes first.
    fn next_wake(&self) -> Option<u64> {
        match (self.systimer.next_deadline(), self.hvs.deadline()) {
            (Some(t), Some(frame)) => Some(t.min(frame)),
            (t, frame) => t.or(frame),
        }
    }

    /// The value a read of device register `addr` would return, for the
    /// registers where a read has no side effect; `None` for the rest. So far
    /// only the mailbox's, which is what UEFI busy-waits on (`arm/mod.rs`,
    /// "Busy-wait loops").
    pub fn peek(&self, addr: u32) -> Option<u32> {
        let off = addr.checked_sub(map::MBOX_BASE)?;
        if off >= map::MBOX_SIZE {
            return None;
        }
        self.mbox.peek(off)
    }

    /// A write to the SD-slot mux word (GPIO `+0xD0`): bit 1 set routes the
    /// card to the legacy EMMC, clear to EMMC2.
    ///
    /// From the traces (#66): 2020-era bootcode (pieeprom-2020-09-03, pc
    /// `0x8000f60a`) writes `0x2` there right before its SD init on
    /// `0x7E30_0000` and never touches EMMC2; the 2026 bootcode never writes it
    /// and boots from EMMC2; start4 writes 0 and then sets bit 0, and start4db's
    /// decompile clears bit 1 explicitly (`_DAT_7e2000d0 & 0xfffffffd`) before
    /// it uses EMMC2. What bit 0 does is not known; [`crate::periph::gpio`]
    /// holds the word, and routing the card between two controllers is the
    /// machine's, since a device never reaches another.
    fn route_sd_slot(&mut self, value: u32) {
        let legacy = value & SD_SLOT_MUX_LEGACY != 0;
        if legacy == self.sd_slot_legacy {
            return;
        }
        self.sd_slot_legacy = legacy;
        let (from, to) = if legacy {
            (&mut self.emmc2, &mut self.emmc)
        } else {
            (&mut self.emmc, &mut self.emmc2)
        };
        to.put_card(from.take_card());
    }

    /// Put each master's pads where the pin functions say they are. The pins
    /// are the GPIO block's and the masters are their own devices, so the
    /// machine is what joins them, as it does for the SD slot.
    ///
    /// SPI0 reaches the boot flash only while GPIO 40..43 are on ALT4: every
    /// EEPROM-stage flash session moves them there and back
    /// (`specs/spi0.toml`), and in between the pins are PWM audio and the
    /// activity LED. I²C 0 reaches the 40-pin header — a HAT's ID EEPROM —
    /// only while GPIO 0/1 are on ALT0; start4's own probe sets them
    /// (`GPFSEL0` `0x4` then `0x24`) and puts them back afterwards, and its
    /// camera and display probes run the same master on GPIO 44/45, where no
    /// HAT is.
    fn route_gpio_pins(&mut self) {
        let alt = |gpio: &Gpio, pin, n| gpio.function(pin) == gpio::Function::Alt(n);
        let flash = (40..=43).all(|pin| alt(&self.gpio, pin, 4));
        self.spi0.set_pins(flash);
        let header = alt(&self.gpio, 0, 0) && alt(&self.gpio, 1, 0);
        self.bsc0.set_pins(header);
    }

    /// [`Bus::store`] off the RAM path; see [`Self::load_device`].
    #[inline(never)]
    fn store_device(&mut self, addr: u32, width: Width, value: u32) -> BusResult<()> {
        self.recheck = true;
        self.mmio_writes = self.mmio_writes.wrapping_add(1);
        self.dma_win_log("wr", addr, value);
        self.advance_hdmi_ddc(addr);
        if addr == SD_SLOT_MUX && width == Width::Word {
            self.route_sd_slot(value);
        }
        if addr == L2_CTRL && value & L2_FLUSH != 0 {
            let (first, last) = (
                self.bootbox.word(crate::spec::bootbox::L2_FLUSH_START),
                self.bootbox.word(crate::spec::bootbox::L2_FLUSH_END),
            );
            self.l2.flush(first, last);
            self.ram.coherency.flushed(first, last);
        }
        let trace = self.mmio_traced(addr);
        if let Some((dev, off)) = self.device_for(addr) {
            let r = dev.write(off, width, value);
            if trace {
                self.mmio_events
                    .push((addr, width.bytes() as u8, value, true));
            }
            if let Some(buf) = self.mbox.take_property_reply() {
                let ram = &self.ram;
                self.mbox.property.record(|o| {
                    ram.load(Machine::fold_ram_addr(buf.wrapping_add(o)), Width::Word)
                        .unwrap_or(0)
                });
            }
            if self.dma4.take_start() {
                // The 40-bit engine takes CPU-physical addresses, with no
                // alias bits to say where its accesses land. It is behind the
                // L2: stock writes its control block through `0x8000_0000`
                // and starts the channel without flushing.
                self.with_vc4_dma_master("the 40-bit DMA", |m| m.run_dma4());
            }
            if let Some(ch) = self.dma_legacy.take_start() {
                self.run_dma_legacy(ch, false);
            }
            if let Some(ch) = self.dma_vpu.take_start() {
                self.run_dma_legacy(ch, true);
            }
            if self.emmc2.dma_pending() {
                self.with_dma_master("the EMMC2 DMA", |m| m.emmc2.run_dma(&mut m.ram));
            }
            // A register write on the USB-C port's xHCI can run a ring, which
            // needs DRAM the device itself has no view of (`xhci_otg`).
            if self.xhci_otg.write_pending() {
                self.with_dma_master("the OTG xHCI", |m| m.xhci_otg.run_pending(&mut m.ram));
            }
            if self.genet.take_kick() {
                let now = self.systimer.now_us();
                self.with_dma_master("the GENET", |m| {
                    m.genet.service(now, &mut m.ram, &mut m.net)
                });
            }
            if GPFSEL_WINDOW.contains(&addr) {
                self.route_gpio_pins();
            }
            return r;
        }
        self.mmio_writes = self.mmio_writes.wrapping_sub(1);
        self.bus_errors += 1;
        Err(BusError::Unmapped {
            addr,
            width,
            write: true,
        })
    }
}

impl Bus for Machine {
    fn take_pending_irq(&mut self) -> Option<u32> {
        // Present the source at CoreCtl `+0x04` now, as it is vectored, not
        // when it was queued: the generic dispatcher (`0x3EC3E9BC`) reads it a
        // dozen instructions into its entry, and a source queued in between
        // used to overwrite the one being taken. That is how a system-timer
        // C2 match (source 66, the clock service's timeouts) got dispatched
        // as a mailbox interrupt during Linux's boot, leaving the clock
        // service waiting on a timer that had already fired — every later
        // `msleep` in the firmware then hung, starting with the SD card
        // power-off Linux asks for on its way to reboot.
        //
        // The controller only takes a source whose enable field is non-zero;
        // the others stay queued until the firmware enables them. In the HTTP
        // boot the HVS interrupt (97) is already asserted when start4 starts,
        // and start4 never enables that source there. It used to be taken off
        // the queue anyway, before start4 had enabled any source; only its
        // still-empty vector-table entry kept it from running.
        let i = self
            .pending_irqs
            .iter()
            .position(|&src| self.corectl.irq_priority(0, src) != 0)?;
        let src = self.pending_irqs.remove(i)?;
        self.corectl.raise_source(0, src);
        Some(src)
    }

    fn take_tick_pending(&mut self) -> bool {
        match self.timer_channel_due() {
            Some(c) => self.systimer.take_channel(c),
            None => false,
        }
    }

    fn sleep_advance(&mut self) -> bool {
        // With the ARM running, the jump waits for it (`Emulator::step_arm`,
        // `Self::wake_vpu_at`): an ARM write that interrupts the VPU has to
        // land before the counter moves past it (#53).
        if self.defer_sleep {
            self.sleep_to = self.next_wake();
            return self.sleep_to.is_some();
        }
        // The HVS can end a frame it interrupts for before the next compare.
        let woke = match (self.systimer.next_deadline(), self.hvs.deadline()) {
            (next, Some(frame)) if next.is_none_or(|t| frame < t) => {
                self.systimer.advance_to(frame);
                true
            }
            _ => self.systimer.wake_to_next_match().is_some(),
        };
        // The counter just jumped; anything timed against it has to catch up.
        self.advance_i2c();
        self.advance_pcie();
        self.advance_hvs();
        woke
    }

    /// Fetch an instruction straight out of RAM when it lives there.
    ///
    /// The generic path in [`Bus::read_insn`] costs a full address decode per
    /// halfword — two to five per instruction, across nearly two billion
    /// instructions a boot. Execution is essentially always out of RAM.
    fn read_insn(&mut self, pc: u32, out: &mut [u8; 10]) -> BusResult<u8> {
        if let Some(off) = self.boot_rom_at(pc) {
            let bytes = &self.boot_rom.as_ref().expect("overlay present").2;
            let p0 = u16::from_le_bytes([
                bytes.get(off).copied().unwrap_or(0),
                bytes.get(off + 1).copied().unwrap_or(0),
            ]);
            let len = crate::vpu::length::insn_len_bytes(p0);
            for (i, slot) in out.iter_mut().take(len as usize).enumerate() {
                *slot = bytes.get(off + i).copied().unwrap_or(0);
            }
            self.ram_reads = self.ram_reads.wrapping_add(1);
            return Ok(len);
        }
        // An uncached fetch from a line the L2 holds takes the slow path.
        if !Machine::in_mmio(pc) && !self.l2.diverts(pc, Machine::fold_ram_addr(pc)) {
            let phys = Machine::fold_ram_addr(pc);
            if let Ok(head) = self.ram.read_slice(phys, 2) {
                let p0 = u16::from_le_bytes([head[0], head[1]]);
                let len = crate::vpu::length::insn_len_bytes(p0);
                if let Ok(bytes) = self.ram.read_slice(phys, len as usize) {
                    self.ram_reads = self.ram_reads.wrapping_add(1);
                    out[..len as usize].copy_from_slice(bytes);
                    return Ok(len);
                }
            }
        }
        let p0 = self.load(pc, Width::Half)? as u16;
        let len = crate::vpu::length::insn_len_bytes(p0);
        out[0..2].copy_from_slice(&p0.to_le_bytes());
        let mut i = 2u32;
        while i < len as u32 {
            let h = self.load(pc.wrapping_add(i), Width::Half)? as u16;
            out[i as usize..i as usize + 2].copy_from_slice(&h.to_le_bytes());
            i += 2;
        }
        Ok(len)
    }

    /// Same condition as `read_insn`'s RAM fast path. A hit counts as the RAM
    /// read that fetch would have made: `ram_reads` feeds the run loop's
    /// progress heuristics, and a decode cache must not change what they see.
    #[inline]
    fn code_gen(&mut self, pc: u32, cached: Option<u64>) -> Option<u64> {
        if self.boot_rom_at(pc).is_some() {
            // The ROM overlay is immutable, so a fixed generation lets the decode
            // cache keep its instructions.
            const ROM_GEN: u64 = u64::MAX;
            if cached == Some(ROM_GEN) {
                self.ram_reads = self.ram_reads.wrapping_add(1);
            }
            return Some(ROM_GEN);
        }
        if Machine::in_mmio(pc) || self.l2.diverts(pc, Machine::fold_ram_addr(pc)) {
            return None;
        }
        let gen = self.ram.page_gen(Machine::fold_ram_addr(pc))?;
        if cached == Some(gen) {
            self.ram_reads = self.ram_reads.wrapping_add(1);
        }
        Some(gen)
    }

    fn timer_tick_slot(&mut self) -> Option<u32> {
        // The VPU vectors an interrupt through the table entry of its interrupt
        // number: 0-31 are exceptions, 32-63 `swi`, 64-127 the external sources
        // (hermanhermitage's VideoCore IV programmers manual). Each system-timer
        // compare channel is its own source, `SYS_IRQ_SRC + channel`, so that is
        // the vector. The 4-bit field the firmware writes per source is its
        // enable and priority, not a vector: a model that vectored through it
        // landed in start4's exception stubs, and no timed wait ever expired.
        //
        // The channel is the lowest one that has matched and whose source core
        // 0's CoreCtl bank enables, the one `take_tick_pending` then takes. It
        // goes by the latched match, never by what is still armed: a one-shot
        // compare disarms at the moment it fires, and testing `any_armed()`
        // dropped every one-shot match.
        let ch = self.timer_channel_due()? as u32;
        let src = crate::periph::corectl::SYS_IRQ_SRC + ch;
        // The generic dispatcher `0x3EC3E9BC` does not take the source from the
        // vector number — it re-reads it from CoreCtl `+0x04` and indexes the
        // handler table at `gp+58004`. Entry 64 is a direct handler and never
        // looks, so leaving a stale pending value there would only shadow a
        // device interrupt's own source.
        if src != crate::periph::corectl::SYS_IRQ_SRC {
            self.corectl.raise_source(0, src);
        }
        Some(src)
    }

    #[inline]
    fn load(&mut self, addr: u32, width: Width) -> BusResult<u32> {
        if self.alignment.is_on() {
            self.alignment.note(addr, width, self.watch_pc, false);
        }
        if let Some(off) = self.boot_rom_at(addr) {
            self.ram_reads = self.ram_reads.wrapping_add(1);
            return Ok(self.boot_rom_load(off, width));
        }
        if !Machine::in_mmio(addr) {
            let phys = Machine::fold_ram_addr(addr);
            if self.ram.contains(phys) {
                self.ram_reads = self.ram_reads.wrapping_add(1);
                if self.ram.coherency.is_on() {
                    if addr >> 30 == 3 {
                        self.ram.coherency.read_by(
                            phys,
                            width.bytes(),
                            crate::coherency::Master::VpuUncached,
                        );
                    } else {
                        let pc = self.watch_pc;
                        self.ram.coherency.read_cached(phys, width.bytes(), pc);
                    }
                }
                if self.l2.covers(phys) {
                    return self.l2.load(&self.ram, addr, phys, width);
                }
                return self.ram.load(phys, width);
            }
        }
        self.load_device(addr, width)
    }

    #[inline]
    fn store(&mut self, addr: u32, width: Width, value: u32) -> BusResult<()> {
        if self.alignment.is_on() {
            self.alignment.note(addr, width, self.watch_pc, true);
        }
        self.ram_writes = self.ram_writes.wrapping_add(1);
        if crate::diag::ON && !self.watch.is_empty() {
            let a = Machine::fold_ram_addr(addr) & !3;
            if self
                .watch
                .iter()
                .any(|&w| Machine::fold_ram_addr(w) & !3 == a)
            {
                eprintln!(
                    "[watch] core{} pc={:#010x} store{} {:#010x} <- {:#x}",
                    self.watch_core,
                    self.watch_pc,
                    width.bytes() * 8,
                    addr,
                    value
                );
            }
        }
        if !Machine::in_mmio(addr) {
            let phys = Machine::fold_ram_addr(addr);
            if self.ram.contains(phys) {
                // Bits 31:30 pick the alias; only `0b11` bypasses the caches.
                if self.ram.coherency.is_on() && addr >> 30 != 3 {
                    self.ram.coherency.wrote_cached(phys, width.bytes());
                }
                if crate::diag::ON
                    && addr & 0x03FF_FFFF == PHASE_TAG_SIG
                    && width == Width::Word
                    && self.phase_tags.last() != Some(&value)
                {
                    self.phase_tags.push(value);
                }
                if self.l2.covers(phys) {
                    return self.l2.store(&mut self.ram, addr, phys, width, value);
                }
                return self.ram.store(phys, width, value);
            }
        }
        self.store_device(addr, width, value)
    }
}

#[cfg(test)]
mod tests {
    use super::dma_irq_source;
    use super::{map, Machine, L2_CTRL, SD_SLOT_MUX};
    use crate::bus::Bus;

    /// 2020-era bootcode writes `0x2` to the mux before its SD init on the
    /// legacy EMMC; start4 writes 0 before it uses EMMC2 (#66).
    #[test]
    fn the_sd_slot_mux_moves_the_card_between_hosts() {
        let mut m = Machine::new(1 << 20);
        m.emmc2.insert_card(vec![0; 512 * 16]);
        m.store32(SD_SLOT_MUX, 0x2).unwrap();
        assert!(m.emmc.has_card() && !m.emmc2.has_card());
        // Bit 0 alone leaves the card on EMMC2's side.
        m.store32(SD_SLOT_MUX, 0x1).unwrap();
        assert!(!m.emmc.has_card() && m.emmc2.has_card());
        m.store32(SD_SLOT_MUX, 0x0).unwrap();
        assert!(!m.emmc.has_card() && m.emmc2.has_card());
    }

    /// 2022-04-26 bootcode keeps its config in the L2 at `0x8001_8020` and
    /// loads a file to `0xC001_8000` across it; the flush its stub does before
    /// bootmain ends that (#70).
    #[test]
    fn the_l2_keeps_the_bootcode_s_lines_until_its_flush() {
        let mut m = Machine::new(1 << 20);
        m.l2.hold(0, 0x100);
        m.store32(0x8001_80C8, 0xF41).unwrap();
        m.store32(0xC001_80C8, 0).unwrap();
        assert_eq!(m.load32(0x8001_80C8), Ok(0xF41));

        m.store32(map::BOOTBOX_BASE + 0x1004, 0).unwrap();
        m.store32(map::BOOTBOX_BASE + 0x1008, 0x0FFF_FFE0).unwrap();
        m.store32(L2_CTRL, 0x14).unwrap();
        m.store32(0xC001_80C8, 0).unwrap();
        assert_eq!(m.load32(0x8001_80C8), Ok(0));
    }

    /// Channel 15 reads a DMA4-layout control block on C0 and a legacy one on
    /// B0, the layouts start4 builds for each (#77).
    #[test]
    fn channel_15_takes_the_control_blocks_of_its_stepping() {
        use crate::soc::{Board, Stepping};
        const CS: u32 = 0x7EE0_5000;
        const CB: u32 = 0x1000;
        const SRC: u32 = 0x2000;
        const DEST: u32 = 0x3000;
        for stepping in [Stepping::B0, Stepping::C0] {
            let mut m = Machine::new(1 << 20);
            m.set_board(Board::for_stepping(stepping));
            for i in 0..16 {
                m.store32(SRC + i * 4, 0x0101_0101 * (i + 1)).unwrap();
            }
            let cb: [u32; 7] = match stepping {
                // TI, SOURCE_AD, DEST_AD, TXFR_LEN, STRIDE, NEXTCONBK
                Stepping::B0 => [0xF331, 0x8000_0000 | SRC, 0x8000_0000 | DEST, 64, 0, 0, 0],
                // TI, SRC, SRCI, DEST, DESTI, LEN, NEXT_CB
                Stepping::C0 => [0, SRC, 0, DEST, 0, 64, 0],
            };
            for (i, w) in cb.iter().enumerate() {
                m.store32(CB + i as u32 * 4, *w).unwrap();
            }
            m.store32(CS, 1).unwrap();
            m.store32(CS + 4, 0x8000_0000 | CB).unwrap();
            for i in 0..16 {
                assert_eq!(
                    m.load32(DEST + i * 4),
                    Ok(0x0101_0101 * (i + 1)),
                    "{stepping}"
                );
            }
        }
    }

    /// The sources start4's dmalib registers `dma_interrupt` on, from its own
    /// source -> channel table (`gp+0x556b8`).
    #[test]
    fn dma_completion_sources_match_dmalib() {
        for (ch, src) in [(1, 81), (3, 83), (6, 86), (11, 89), (14, 92), (15, 95)] {
            assert_eq!(dma_irq_source(ch), src, "channel {ch}");
        }
    }

    /// A queued source goes out once core 0's CoreCtl bank enables it, and
    /// until then it doesn't hold up the enabled sources behind it (#25).
    #[test]
    fn a_queued_source_waits_for_its_corectl_enable() {
        const IRQ_PRIO: u32 = 0x7E00_2010;
        let mut m = Machine::new(1 << 20);
        m.push_pending_irq(97);
        m.push_pending_irq(94);
        assert!(!m.irq_queued());
        assert_eq!(m.take_pending_irq(), None);
        // Source 94: word 3, bits 24..28.
        m.store32(IRQ_PRIO + 0xC, 0x0100_0000).unwrap();
        assert!(m.irq_queued());
        assert_eq!(m.take_pending_irq(), Some(94));
        assert_eq!(m.take_pending_irq(), None);
        // Source 97: word 4, bits 4..8, not source 65's field in word 0.
        m.store32(IRQ_PRIO, 0x10).unwrap();
        assert_eq!(m.take_pending_irq(), None);
        m.store32(IRQ_PRIO + 0x10, 0x10).unwrap();
        assert_eq!(m.take_pending_irq(), Some(97));
        assert!(!m.irq_queued());
    }

    /// A compare whose source core 0 hasn't enabled stays latched, and the
    /// enabled channel above it goes out first (#80).
    #[test]
    fn a_timer_match_waits_for_its_source_s_enable() {
        const IRQ_PRIO: u32 = 0x7E00_2010;
        const C0: u32 = 0x7E00_300C;
        let mut m = Machine::new(1 << 20);
        m.store32(C0, 10).unwrap();
        m.store32(C0 + 8, 20).unwrap();
        m.systimer.advance_to(30);
        assert!(m.systimer.tick_pending());
        assert!(!m.timer_irq_due());
        assert_eq!(m.timer_tick_slot(), None);
        // Source 66, channel 2: word 0, bits 8..12.
        m.store32(IRQ_PRIO, 0x100).unwrap();
        assert_eq!(m.timer_tick_slot(), Some(66));
        assert!(m.take_tick_pending());
        assert!(!m.timer_irq_due() && m.systimer.tick_pending());
        // Source 64, channel 0: bits 0..4.
        m.store32(IRQ_PRIO, 0x101).unwrap();
        assert_eq!(m.timer_tick_slot(), Some(64));
        assert!(m.take_tick_pending());
        assert!(!m.systimer.tick_pending());
    }
}
