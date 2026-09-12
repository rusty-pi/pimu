//! The `Machine`: RAM + peripherals + address decode. Implements [`Bus`].

use crate::bus::{Bus, BusError, BusResult, MmioDevice, Width};
use crate::mem::Ram;
use crate::periph::{
    ArmCtrl, ArmLocal, Asb, Aux, Avs, BootBox, Bsc, ClkMon, ClockManager, ConfigOtp, CoreCtl, Dma4,
    Emmc2, Gic, HdmiDdc, Hvs, Mbox, McSync, Pl011, Pm, Rng, Sdc, Sdramc, Spi0, StubRegion,
    SysTimer, Vce,
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
    /// PCIe root complex (`0x7D50_0000`) — a register file only. The VL805
    /// xHCI controller behind it is not modelled; see `docs/usb-xhci.md`.
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
    /// SPI0 master (`0x7E20_4000`) — minimal model for the EEPROM bootloader.
    pub spi0: Spi0,
    /// BSC / I²C master at `0x7E20_5E00` + the board PMIC — start4 reads the
    /// PMIC over this on its way to bringing up the "external" GPIO pins.
    pub bsc_pmic: Bsc,
    /// The two HDMI connectors' DDC I²C masters, with no monitor on either.
    pub hdmi_ddc0: HdmiDdc,
    pub hdmi_ddc1: HdmiDdc,
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
    /// Legacy DMA controller (`0x7E00_7000`) — start4's bulk memory copies.
    pub dma_legacy: crate::periph::dma_legacy::DmaLegacy,
    /// The `0x7EE0_4100` DMA controller (channel 15 at `0x7EE0_5000`).
    pub dma_vpu: crate::periph::dma_legacy::DmaLegacy,
    /// DMA4 channel (`0x7E00_7B00`) — the bootloader scrubs / moves DRAM through
    /// it; [`Machine::store`] runs the control-block chain after a `CS` write.
    pub dma4: Dma4,
    /// EMMC2 SD host controller (`0x7E34_0000`).
    pub emmc2: Emmc2,
    /// HVS (`0x7E40_0000`) — display frame-swap registers auto-complete.
    pub hvs: Hvs,
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
    /// `RVF_DBG_DMA=1`: log every control block the DMA4 channel executes.
    dbg_dma: bool,
    /// Interrupt sources raised by peripherals, waiting to be vectored.
    pending_irqs: std::collections::VecDeque<u32>,
    /// Something happened that the run loop's per-step checks may have to
    /// act on: a peripheral register was written, an interrupt was queued, a
    /// compare fired, or a reset came due. The run loop clears it; while it
    /// stays clear, those checks have nothing to do (`Emulator::fast_steps`).
    pub wake: bool,
    pub watch_pc: u32,

    /// `start4.elf` logs boot progress by writing 4-char ASCII tags (`_msh`,
    /// `_osh`, `bfsp`, ...) to a register at `0xCEC0_2000`. We capture the
    /// sequence — it is the closest thing to an early-boot log before any UART
    /// is up.
    pub phase_tags: Vec<u32>,
}

/// `start4.elf` writes 4-char boot-progress tags to `0x?EC0_2000`. Direct-ELF
/// load runs it at `0xCEC0_0000` (tags `0xCEC0_2000`); the real bootloader
/// relocates it to `0xFEC0_0000` (tags `0xFEC0_2000`). Both share the low-26-bit
/// signature `0x02C0_2000` (each `0x?C00_0000` alias is 64 MiB), so match on
/// that regardless of which alias the write used.
const PHASE_TAG_SIG: u32 = 0x02C0_2000;

impl Machine {
    pub fn new(ram_bytes: usize) -> Machine {
        Machine {
            ram: Ram::new(map::SDRAM_CACHED_BASE, ram_bytes),
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
            bsc0: Bsc::empty("bsc0"),
            spi0: Spi0::new(),
            bsc_pmic: Bsc::new("bsc-pmic"),
            hdmi_ddc0: HdmiDdc::new("hdmi-ddc0"),
            hdmi_ddc1: HdmiDdc::new("hdmi-ddc1"),
            config_otp: ConfigOtp::new(),
            sdramc: Sdramc::new(),
            sdc: Sdc::new(),
            bootbox: BootBox::new(),
            dma4: Dma4::new(),
            dma_legacy: crate::periph::dma_legacy::DmaLegacy::new(),
            dma_vpu: crate::periph::dma_legacy::DmaLegacy::new_vpu(),
            emmc2: Emmc2::new(),
            hvs: Hvs::new(),
            periph_stub: StubRegion::new("periph-window"),
            console: Console::default(),
            stub_hits: 0,
            bus_errors: 0,
            ram_writes: 0,
            mmio_writes: 0,
            ram_reads: 0,
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
            dbg_dma: crate::diag::ON && std::env::var_os("RVF_DBG_DMA").is_some(),
            pending_irqs: std::collections::VecDeque::new(),
            wake: false,
            watch_pc: 0,
            phase_tags: Vec::new(),
        }
    }

    /// Plug GENET's cable into `backend`: the PHY sees a link partner, and
    /// frames flow between the DMA rings and `backend`.
    pub fn attach_net(&mut self, backend: Box<dyn crate::net::NetBackend>) {
        self.genet.phy.set_link(true);
        self.net = Some(backend);
    }

    /// Queue an interrupt source for delivery to core 0 on the next step.
    pub fn push_pending_irq(&mut self, src: u32) {
        self.pending_irqs.push_back(src);
        self.wake = true;
    }

    /// Is an interrupt source queued for core 0?
    pub fn irq_queued(&self) -> bool {
        !self.pending_irqs.is_empty()
    }

    /// Settle any I²C transfer whose time on the wire has elapsed.
    fn advance_i2c(&mut self) {
        let now = self.systimer.now_us();
        self.bsc_pmic.advance_to(now);
        self.bsc0.advance_to(now);
    }

    /// Settle the HDMI DDC masters, which time their transfers the same way.
    ///
    /// Unlike the BSCs these are advanced lazily, on the way into their own
    /// registers, rather than out of [`Machine::tick`]: nothing but the
    /// firmware's own status poll ever observes them, and two more calls on
    /// every single retired instruction cost a measurable few percent of the
    /// model's throughput.
    fn advance_hdmi_ddc(&mut self, addr: u32) {
        let in_window = |base: u32| addr >= base && addr < base + map::HDMI_DDC_SIZE;
        if in_window(map::HDMI_DDC0_BASE) || in_window(map::HDMI_DDC1_BASE) {
            let now = self.systimer.now_us();
            self.hdmi_ddc0.advance_to(now);
            self.hdmi_ddc1.advance_to(now);
        }
    }

    /// Mirror the core rail's PMIC setpoint into the AVS monitor before a read
    /// of that block.
    ///
    /// Channel 3 of the AVS monitor is a voltage sensor sitting on the SoC core
    /// rail, and the rail is driven over I²C by the `0x1E` PMIC (register
    /// `0x25`, 10 mV per step — see [`crate::periph::pmic`]). start4's DVFS
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
        // `0x3EC8C9F6`, the `0x1E` descriptor's raw-to-microvolts callback.
        let raw = self
            .bsc_pmic
            .slave()
            .and_then(|p| p.part(crate::periph::pmic::ADDR_CORE))
            .map(|part| part.reg(crate::periph::pmic::CORE_SETPOINT));
        if let Some(raw) = raw {
            self.avs.set_core_rail_uv(u32::from(raw) * 10_000);
        }
    }

    /// Advance time-based peripheral state by `cycles` VPU cycles.
    pub fn tick(&mut self, cycles: u64) {
        // Everything below is derived from the microsecond counter, so when it
        // has not moved there is nothing for any of it to do.
        if !self.systimer.advance(cycles) {
            return;
        }
        if self.systimer.take_fired() {
            self.wake = true;
        }
        self.pm.advance(self.systimer.now_us());
        if self.pm.reset_pending() {
            self.wake = true;
        }
        // A backend with its own clock (a host network) can deliver a frame
        // at any time, not only in reply to a register write.
        if self.net.is_some() {
            self.genet.service(&mut self.ram, &mut self.net);
        }
        // The I²C masters time their transfers in microseconds off the system
        // timer, so they stay in step with it across the run loop's `sleep`
        // fast-forward (which jumps the counter without retiring cycles).
        self.advance_i2c();
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
    }

    /// Drain and return whatever the console UART has transmitted.
    pub fn take_console_output(&mut self) -> Vec<u8> {
        match self.console {
            Console::Pl011 => self.uart0.take_output(),
            Console::MiniUart => self.aux.take_output(),
        }
    }

    /// Drain both UARTs regardless of console selection (for tracing).
    pub fn take_all_uart_output(&mut self) -> (Vec<u8>, Vec<u8>) {
        (self.uart0.take_output(), self.aux.take_output())
    }

    pub fn irq_pending(&self) -> bool {
        self.uart0.irq_pending() || self.aux.irq_pending()
    }

    /// The VPU addresses peripherals only through the `0x7E00_0000` window (no
    /// cache aliasing, unlike RAM) — plus the LPDDR4 controller/PHY, which is
    /// mapped just *below* that window at `0x7DC0_0000`. Both must be decoded
    /// before the cache-alias fold, or their (aliased) addresses land in DRAM.
    fn in_mmio(addr: u32) -> bool {
        (map::PERIPH_BASE..map::PERIPH_BASE + map::PERIPH_SIZE).contains(&addr)
            || (map::SDRAMC_BASE..map::SDRAMC_BASE + map::SDRAMC_SIZE).contains(&addr)
            || (map::CLKMON_BASE..map::CLKMON_BASE + map::CLKMON_SIZE).contains(&addr)
            || (map::PCIE_BASE..map::PCIE_BASE + map::PCIE_SIZE).contains(&addr)
            || (map::GENET_BASE..map::GENET_BASE + map::GENET_SIZE).contains(&addr)
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

    /// Fold the four VC4 cache aliases (`0x0`, `0x4000_0000`, `0x8000_0000`,
    /// `0xC000_0000`) of physical memory onto a single backing store. Before
    /// SDRAM training this backing *is* the ~128 KiB of L2-as-SRAM the bootcode
    /// runs from; afterwards it stands in for DRAM.
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
            self.mbox.now_us = self.systimer.now_us();
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
        if let Some(off) = hit(map::EMMC2_BASE, map::EMMC2_SIZE) {
            return Some((&mut self.emmc2, off));
        }
        if let Some(off) = hit(map::HVS_BASE, map::HVS_SIZE) {
            return Some((&mut self.hvs, off));
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
    /// `RVF_DBG_DMA=1`: trace every access to the legacy DMA controller window
    /// (`0x7E00_7000..0x7E00_8000`, 15 channels x 0x100). Only channel 11
    /// (DMA4, `0x7E00_7B00`) is modelled; start4's `dma_memcpy` uses one of the
    /// others, so those accesses currently fall through to the catch-all stub.
    fn dma_win_log(&self, rw: &str, addr: u32, value: u32) {
        if crate::diag::ON && self.dbg_dma && (0x7E00_7000..0x7E00_8000).contains(&addr) {
            let ch = (addr - 0x7E00_7000) / 0x100;
            eprintln!(
                "[dmawin] {rw} ch{ch} +{:#04x} ({addr:#x}) = {value:#x} pc={:#x}",
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
        let mut cb = cb_addr & 0x3FFF_FFFF;
        for _ in 0..4096 {
            if cb == 0 || !self.ram.contains(cb) {
                break;
            }
            let mut w = [0u32; 7];
            for (i, slot) in w.iter_mut().enumerate() {
                *slot = self.ram.load(cb + (i as u32) * 4, Width::Word).unwrap_or(0);
            }
            // Channel 15 of the `0x7EE0_4100` controller is a 40-bit ("dma40")
            // channel - `dma_memcpy` builds its CB with
            // `dma_transfer_setup_memcpy_vpu40` (`0x3EC99EE0`) - so it uses the
            // DMA4 control-block layout, not the legacy one:
            //   +0x00 TI  +0x04 SRC  +0x08 SRCI  +0x0C DEST  +0x10 DESTI
            //   +0x14 LEN +0x18 NEXT_CB(>>5)
            let d = if vpu {
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
            if crate::diag::ON && self.dbg_dma {
                eprintln!(
                    "[dma-legacy] ch{ch} cb={cb:#x} ti={:#x} src={:#x} dest={:#x} len={:#x} stride={:#x} next={:#x}",
                    d.ti, d.src, d.dest, d.len, d.stride, d.next
                );
            }
            let (rows, xlen) = d.rows();
            let (src_stride, dest_stride) = d.strides();
            let mut src = d.src & 0x3FFF_FFFF;
            let mut dest = d.dest & 0x3FFF_FFFF;
            for _ in 0..rows {
                for i in 0..xlen {
                    let sa = if vpu || d.src_inc() {
                        src.wrapping_add(i)
                    } else {
                        src
                    };
                    let da = if vpu || d.dest_inc() {
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

    /// One word to a 40-bit DMA4 address.
    fn dma40_store(&mut self, addr: u64, value: u32) {
        if addr >> 32 != 0
            && self
                .pcie
                .mmio_write(addr, Width::Word, value, &mut self.ram)
        {
            return;
        }
        let _ = self
            .ram
            .store((addr as u32) & 0x3FFF_FFFF, Width::Word, value);
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

            if crate::diag::ON && self.dbg_dma {
                eprintln!(
                    "[dma] cb={cb:#x} ti={:#x} src={src:#x} srci={srci:#x} dest={dest:#x} len={len:#x} next={next:#x}",
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
        let src = self.pending_irqs.pop_front()?;
        self.corectl.raise_source(src);
        Some(src)
    }

    fn take_tick_pending(&mut self) -> bool {
        self.systimer.take_tick_pending()
    }

    fn sleep_advance(&mut self) -> bool {
        let woke = self.systimer.wake_to_next_match().is_some();
        // The counter just jumped; anything timed against it has to catch up.
        self.advance_i2c();
        woke
    }

    /// Fetch an instruction straight out of RAM when it lives there.
    ///
    /// The generic path in [`Bus::read_insn`] costs a full address decode per
    /// halfword — two to five per instruction, across nearly two billion
    /// instructions a boot. Execution is essentially always out of RAM.
    fn read_insn(&mut self, pc: u32, out: &mut [u8; 10]) -> BusResult<u8> {
        if !Machine::in_mmio(pc) {
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
    fn code_gen(&mut self, pc: u32, cached: Option<u64>) -> Option<u64> {
        if Machine::in_mmio(pc) {
            return None;
        }
        let gen = self.ram.page_gen(Machine::fold_ram_addr(pc))?;
        if cached == Some(gen) {
            self.ram_reads = self.ram_reads.wrapping_add(1);
        }
        Some(gen)
    }

    fn timer_tick_slot(&mut self) -> Option<u32> {
        // Each system-timer compare channel is its own VPU interrupt source
        // (`SYS_IRQ_SRC + channel`), and start4's vector table has 128 entries /
        // 0x200 bytes: [0..15] are the per-priority stubs, [64..127] the direct
        // per-source handlers. Entry 64 is the ThreadX tick `0x3EC40B7C` (read
        // back off a running Pi 4, issue #7) — the only code that acks `CS.M0`,
        // re-arms `C0`, walks the 32-bucket timer wheel and calls
        // `_tx_thread_system_resume`. Entry 66 is the clock service's timeout
        // timer, which reaches `0x3ED6588A` through the generic dispatcher.
        //
        // So deliver the raw source, not the 4-bit priority
        // `enable_irq_source(64, prio)` recorded. Routing through the priority
        // stub reached `0x3ECB0BF0(slot)`, which does nothing for slot 1, so no
        // timed wait ever expired and every blocking `msleep` hung forever.
        //
        // A channel that has already matched is what we are delivering, and
        // under one-shot compares it disarms at the moment it fires — so check
        // "something is pending" first and only fall back to "something is
        // armed". Testing `any_armed()` alone dropped every one-shot match.
        if self.systimer.pending_channel().is_none() && !self.systimer.any_armed() {
            return None;
        }
        let ch = self.systimer.pending_channel().unwrap_or(0) as u32;
        let src = crate::periph::corectl::SYS_IRQ_SRC + ch;
        // The generic dispatcher `0x3EC3E9BC` does not take the source from the
        // vector number — it re-reads it from CoreCtl `+0x04` and indexes the
        // handler table at `gp+58004`. Entry 64 is a direct handler and never
        // looks, so leaving a stale pending value there would only shadow a
        // device interrupt's own source.
        if src != crate::periph::corectl::SYS_IRQ_SRC {
            self.corectl.raise_source(src);
        }
        Some(src)
    }

    fn load(&mut self, addr: u32, width: Width) -> BusResult<u32> {
        if !Machine::in_mmio(addr) {
            let phys = Machine::fold_ram_addr(addr);
            if self.ram.contains(phys) {
                self.ram_reads = self.ram_reads.wrapping_add(1);
                return self.ram.load(phys, width);
            }
        }
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

    fn store(&mut self, addr: u32, width: Width, value: u32) -> BusResult<()> {
        self.ram_writes = self.ram_writes.wrapping_add(1);
        if crate::diag::ON && !self.watch.is_empty() {
            let a = Machine::fold_ram_addr(addr) & !3;
            if self
                .watch
                .iter()
                .any(|&w| Machine::fold_ram_addr(w) & !3 == a)
            {
                eprintln!(
                    "[watch] pc={:#010x} store{} {:#010x} <- {:#x}",
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
                if addr & 0x03FF_FFFF == PHASE_TAG_SIG
                    && width == Width::Word
                    && self.phase_tags.last() != Some(&value)
                {
                    self.phase_tags.push(value);
                }
                return self.ram.store(phys, width, value);
            }
        }
        self.wake = true;
        self.mmio_writes = self.mmio_writes.wrapping_add(1);
        self.dma_win_log("wr", addr, value);
        self.advance_hdmi_ddc(addr);
        let trace = self.mmio_traced(addr);
        if let Some((dev, off)) = self.device_for(addr) {
            let r = dev.write(off, width, value);
            if trace {
                self.mmio_events
                    .push((addr, width.bytes() as u8, value, true));
            }
            if self.dma4.take_start() {
                self.run_dma4();
            }
            if let Some(ch) = self.dma_legacy.take_start() {
                self.run_dma_legacy(ch, false);
            }
            if let Some(ch) = self.dma_vpu.take_start() {
                self.run_dma_legacy(ch, true);
            }
            if self.emmc2.dma_pending() {
                self.emmc2.run_dma(&mut self.ram);
            }
            if self.genet.take_kick() {
                self.genet.service(&mut self.ram, &mut self.net);
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

#[cfg(test)]
mod tests {
    use super::dma_irq_source;

    /// The sources start4's dmalib registers `dma_interrupt` on, from its own
    /// source -> channel table (`gp+0x556b8`).
    #[test]
    fn dma_completion_sources_match_dmalib() {
        for (ch, src) in [(1, 81), (3, 83), (6, 86), (11, 89), (14, 92), (15, 95)] {
            assert_eq!(dma_irq_source(ch), src, "channel {ch}");
        }
    }
}
