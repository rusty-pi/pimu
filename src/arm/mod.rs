//! The ARM side of the machine: the BCM2711's four Cortex-A72 cores on its ARM
//! physical map, run in lock-step with the VPU.
//!
//! The cores stay in reset until `arm_loader` releases them through the ARM
//! control block ([`crate::periph::armctrl`]). All four then start as the SoC
//! starts them — PC 0, EL3, `DAIF` masked — in the firmware's armstub
//! ([`crate::armstub`]): each sets up its own banked GIC state and drops to
//! non-secure EL2, core 0 enters the kernel with `x0` = the dtb, and cores 1..3
//! park on their spin-table word.
//!
//! ## Address map
//!
//! Low-peripheral mode, the one the handed-over device tree describes:
//!
//! ```text
//!   0x0_0000_0000 ..             RAM: the VPU's SDRAM, no aliases
//!   0x0_FC00_0000 .. 0xFF80_0000  peripherals; ARM 0xFC00_0000 + x is VPU
//!                                 bus address 0x7C00_0000 + x
//!   0x0_FF80_0000                 ARM local block (`src/periph/armlocal.rs`)
//!   0x0_FF84_0000                 GIC-400 (`src/periph/gic.rs`)
//!   0x6_0000_0000 .. 0x8_0000_0000  PCIe outbound window (`src/periph/pcie.rs`)
//! ```
//!
//! Anything else is a bus abort. RAM accesses go straight to the backing store
//! rather than through [`Machine`]'s VPU decode, so they do not count towards
//! the VPU run loop's progress heuristics.
//!
//! ## Time and scheduling
//!
//! One ARM instruction is one cycle at the nominal [`gentimer::ARM_HZ`] on every
//! core: the cores take turns in core order, share one cycle count and read the
//! same counter. After every VPU step [`ArmSide::catch_up`] runs the ARM until
//! it has had as many cycles as the system timer says have passed. A run is
//! therefore a pure function of its inputs — the reproducibility the regression
//! bench depends on — and the ARM's clock never falls behind the VPU's.
//!
//! A core in `wfi` sits out its turns until the GIC signals it; when all of them
//! wait, time skips to the next generic-timer event inside the slice.
//!
//! A fast-forward slice runs with the VPU frozen. For `sleep` the ARM runs the
//! slice *before* the counter moves ([`ArmSide::run_until_store`]) and stops
//! after the first cycle in which a core writes a VPU-side peripheral: that
//! write — usually a mailbox request — is what would interrupt the VPU, so the
//! counter moves only that far and the VPU wakes to it on time. Without it a
//! request made while the VPU sleeps waits the whole slice, 1.9 ms on average,
//! and UEFI's SD card reads crawl.
//!
//! ## Going faster, and the invariants that keep it honest
//!
//! Each of these is a shortcut whose observable result must match the plain
//! path; each has a `PIMU_*` switch that turns it off for comparison.
//!
//! - **Bursts** (`PIMU_NO_BURST`): when one core is the only one taking turns,
//!   [`ArmSide::burst`] steps it without the cycle loop's per-cycle checks. A
//!   step that touches a device, stores where another core holds an exclusive
//!   mark, changes state others depend on ([`Cpu::shared_effects`]) or does not
//!   retire ends the burst and is finished the ordinary way.
//! - **Straight-line runs** (`PIMU_NO_STRAIGHT`, measured by
//!   `PIMU_ARM_BLOCKS`): inside a burst, the instructions between two control
//!   transfers run off the page the first came from, because nothing they could
//!   observe can change over such a run — interrupt lines only move in
//!   [`ArmSide::sync`]; the EL and `SCR_EL3`/`HCR_EL2` a stage-2 guard reads
//!   cannot move without adding to [`Cpu::shared_effects`], which is compared
//!   every step; and a `TLBI` that would drop the page's translation ends the
//!   run by the same compare.
//! - **Parking** (`PIMU_NO_PARK`, `src/arm/park.rs`): a polling loop — UEFI's
//!   mailbox driver and its `Stall` are most of a UEFI boot's host time — sits
//!   out its turns like a `wfi` sleeper and is rebuilt exactly where it would
//!   have ended. A watched loop must repeat the same PCs and reads, store
//!   nothing, change nothing beyond the general registers and flags
//!   ([`Cpu::effects`]), and read only RAM, the counter and registers
//!   [`Machine::peek`] can read without side effects. The exit search doubles
//!   and halves, which assumes an exit condition stays true once true — hence a
//!   moved-on register must count *down* to zero, since one counting up is
//!   often compared for equality and would be stepped over.
//! - **SHA-256 skipping** (`PIMU_NO_SHA_SKIP`, `src/arm/sha.rs`): UEFI hashes
//!   the whole image before starting a kernel — 89.7 MB for the mkosi UKI — so
//!   a recognised compression loop has its middle blocks hashed natively and
//!   comes out with the registers, memory and cycle count it would have had.
//!   Recognition is by what the loop does, not by its code, and the skip is
//!   bounded to a lone runnable core with no exclusive mark or takeable
//!   interrupt, inside the slice and before the next timer event.
//!
//! Skipped instructions always count as executed.
//!
//! ## Between cores
//!
//! - Exclusive monitor: a store into the 64-byte granule another core marked
//!   with `ldxr` clears that core's mark, so its `stxr` fails.
//! - TLB maintenance: every `TLBI` flushes all cores' TLBs, which covers the
//!   broadcast forms; invalidating more is allowed.
//! - `wfe` (ARM ARM D1.16) ends on `sev`, a cleared monitor mark, an interrupt
//!   line or an exception return, at the next event-stream event, or after
//!   [`WFE_BACKSTOP`]. The architecture allows an early completion, so the
//!   backstop is always safe; treating `wfe` as a no-op instead would leave
//!   TF-A's and UEFI's holding pens spinning for the whole boot.
//!
//! Not modelled: stage 2 translation (a core that sets `HCR_EL2.VM` stops with
//! [`ArmStop::Unsupported`]).

use crate::aarch64::{sysreg, Abort, Cpu, Exception, Memory, Step};
use crate::armstub::{self, Handoff};
use crate::bus::{Bus, MmioDevice, Width};
use crate::log::{Channel, Log};
use crate::machine::Machine;
use crate::periph::gentimer::{self, GenericTimer, Reg, Which};
use crate::periph::{armlocal, gic};

pub mod blocks;
mod park;
mod sha;

/// The number of cores: BCM2711 has four A72s.
pub const CORES: usize = 4;

/// The peripheral window, and how far below it the VPU sees the same thing.
const PERIPH: std::ops::Range<u64> = 0xFC00_0000..0xFF80_0000;
/// Where the DRAM the peripherals shadow gives way to them, and where the rest
/// comes back at 4 GB — which is why the firmware's `/memory@0` splits there.
const RAM_LOW_END: u64 = PERIPH.start;
const RAM_HIGH_BASE: u64 = 0x1_0000_0000;
const PERIPH_TO_BUS: u64 = 0x8000_0000;

/// The PCIe outbound window; what decodes in it is up to the root complex.
const PCIE: std::ops::Range<u64> = 0x6_0000_0000..0x8_0000_0000;

/// The longest a `wfe` waits when nothing else ends it: 1 ms of modelled time.
const WFE_BACKSTOP: u64 = gentimer::ARM_HZ / 1000;

/// `CNTKCTL_EL1` / `CNTHCTL_EL2`: the event stream's enable, and which counter
/// bit it follows.
const EVNTEN: u64 = 1 << 2;
const CNTKCTL_EL1: u32 = sysreg::key(3, 0, 14, 1, 0);
const CNTHCTL_EL2: u32 = sysreg::key(3, 4, 14, 1, 0);

/// The armstub lives at physical 0 and is well under a page; a core whose PC
/// gets past this has left it.
const STUB_END: u64 = 0x1000;

/// Why the ARM stopped. The cores never stop by themselves; these are gaps in
/// the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmStop {
    /// An instruction the interpreter does not implement.
    Unimplemented {
        core: usize,
        pc: u64,
        el: u32,
        insn: u32,
    },
    /// The guest enabled something not modelled.
    Unsupported {
        core: usize,
        pc: u64,
        el: u32,
        what: &'static str,
    },
}

/// One core and what it has done since release.
pub struct Core {
    pub cpu: Cpu,
    pub timer: GenericTimer,
    pub insns: u64,
    pub exceptions: u64,
    pub interrupts: u64,
    /// In `wfi`, waiting for an interrupt, or in `wfe` (then
    /// [`Self::wfe_until`] is set).
    pub waiting: bool,
    /// Waiting in `wfe`: the cycle it completes anyway.
    pub wfe_until: Option<u64>,
    /// The first time the core's PC left the armstub: `(cycle, EL, PC, x0)`.
    /// For core 0 that is the kernel entry.
    pub entered: Option<(u64, u32, u64, u64)>,
    /// Looks for a busy-wait loop to park the core in.
    detect: park::Detector,
    /// The loop the core is parked in.
    park: Option<Box<park::Park>>,
    /// The SHA-256 block loop the core was last fitted to.
    sha: Option<Box<sha::Loop>>,
    pub sha_loops: u64,
    pub sha_blocks: u64,
    /// `PIMU_ARM_BLOCKS=1`: the straight-line runs this core executed.
    pub blocks: Option<Box<blocks::Blocks>>,
}

impl Core {
    fn new(id: usize) -> Core {
        Core {
            cpu: Cpu::with_id(id as u32),
            timer: GenericTimer::new(),
            insns: 0,
            exceptions: 0,
            interrupts: 0,
            waiting: false,
            wfe_until: None,
            entered: None,
            detect: park::Detector::default(),
            park: None,
            sha: None,
            sha_loops: 0,
            sha_blocks: 0,
            blocks: (crate::diag::ON && std::env::var_os("PIMU_ARM_BLOCKS").is_some())
                .then(Box::<blocks::Blocks>::default),
        }
    }

    /// Bring this core's interrupt inputs up to date.
    fn sync(&mut self, m: &mut Machine, id: usize, cycles: u64, hz: u64) {
        self.timer.set_hz(cycles, hz);
        for w in Which::ALL {
            m.gic
                .set_ppi_level(id, w.intid(), self.timer.line(w, cycles));
        }
        let s = m.gic.signal(id);
        self.cpu.irq_line = s.is_some_and(|s| !s.fiq);
        self.cpu.fiq_line = s.is_some_and(|s| s.fiq);
    }
}

/// The ARM cores and what they have done since release.
pub struct ArmSide {
    pub cores: Vec<Core>,
    /// Modelled cycles since release.
    pub cycles: u64,
    /// The part of [`Self::cycles`] every core spent asleep.
    pub slept: u64,
    pub stopped: Option<ArmStop>,
    /// What `arm_loader` left in the armstub, read at release.
    pub handoff: Option<Handoff>,
    /// The kernel command line, before and after the extra arguments.
    pub bootargs: Option<Result<(String, String), String>>,
    /// The system timer, in ARM cycles, at the first [`Self::catch_up`].
    released_at: Option<u64>,
    /// Interrupt state has to be recomputed before the next step.
    dirty: bool,
    /// The earliest cycle a generic timer's output can rise.
    timer_due: u64,
    /// The [`SPIS`] lines into the GIC, as last seen.
    spis: [bool; SPIS.len()],
    /// Bit `id` set when core `id` takes its turn this cycle. Lines only move
    /// in [`Self::sync`] and `waiting` only in a core's own step, so keeping
    /// this exact lets the cycle loop visit just these cores, in order.
    runnable: u32,
    /// `PIMU_ARM_PROF`: steps per `(core, EL, 256-byte PC bucket)`.
    pub prof: Option<std::collections::HashMap<(usize, u32, u64), u64>>,
    /// `PIMU_ARM_PROF=<us>`: the model time the profile starts at, until it
    /// has.
    prof_from: Option<u64>,
    /// Bit `id` set while core `id` is parked, and when one is next due back.
    parked: u32,
    park_due: u64,
    /// Parked cores an input change woke in the middle of a cycle, still to
    /// take their turn in it.
    woke: u32,
    /// `PIMU_NO_PARK=1` turns parking off.
    park_on: bool,
    /// `PIMU_NO_BURST=1` takes a lone core through the cycle loop too
    /// ([`Self::burst`]).
    burst_on: bool,
    /// `PIMU_NO_SHA_SKIP=1` runs SHA-256 block loops block by block too.
    sha_on: bool,
    /// `PIMU_NO_STRAIGHT=1` steps a burst one instruction at a time.
    straight_on: bool,
    /// `PIMU_ARM_BLOCKS=1` counts straight-line runs; only [`Self::step_core`]
    /// sees them, so it takes the cores off the burst path.
    blocks_on: bool,
    /// [`Self::run_until_store`]'s stop condition, and whether it has fired.
    stop_on_store: bool,
    stored: bool,
    /// Where [`Channel::ArmExc`] goes: the machine's, taken at release.
    log: Log,
}

/// The device interrupt lines wired to the GIC: the mailbox, VCHIQ's
/// doorbell, eMMC2 (which the legacy EMMC shares), the two GENET lines, the
/// PL011, the AUX block's mini-UART, the PCIe endpoint's INTA and MSI, the
/// USB-C port's own xHCI, the GPIO block's four (a bank each, the third-bank
/// output that mirrors bank 1's, and the one either bank raises), and the
/// legacy DMA controller's nine.
const SPIS: [u32; 23] = [
    gic::ID_MAILBOX,
    gic::ID_DOORBELL0,
    gic::ID_EMMC2,
    gic::ID_GENET_A,
    gic::ID_GENET_B,
    gic::ID_PL011,
    gic::ID_AUX,
    gic::ID_PCIE_INTA,
    gic::ID_PCIE_MSI,
    gic::ID_XHCI_OTG,
    gic::ID_GPIO_BANK0,
    gic::ID_GPIO_BANK1,
    gic::ID_GPIO_BANK1_MIRROR,
    gic::ID_GPIO_ANY,
    gic::ID_DMA[0],
    gic::ID_DMA[1],
    gic::ID_DMA[2],
    gic::ID_DMA[3],
    gic::ID_DMA[4],
    gic::ID_DMA[5],
    gic::ID_DMA[6],
    gic::ID_DMA[7],
    gic::ID_DMA[8],
];

fn spi_levels(m: &Machine) -> [bool; SPIS.len()] {
    let [genet_a, genet_b] = m.genet.irq_lines();
    let [gpio0, gpio1] = m.gpio.irq_lines();
    let dma = m.dma_legacy.irq_lines();
    [
        m.mbox.arm_irq_asserted(),
        m.bell.arm_irq_asserted(),
        m.emmc2.irq_asserted() || m.emmc.irq_asserted(),
        genet_a,
        genet_b,
        m.uart0.irq_line(),
        m.aux.irq_line(),
        m.pcie.intx_line(),
        m.pcie.msi_line(),
        m.xhci_otg.irq_asserted(),
        gpio0,
        gpio1,
        gpio1,
        gpio0 || gpio1,
        dma[0],
        dma[1],
        dma[2],
        dma[3],
        dma[4],
        dma[5],
        dma[6],
        dma[7],
        dma[8],
    ]
}

impl Default for ArmSide {
    fn default() -> Self {
        Self::new()
    }
}

impl ArmSide {
    /// All the cores in their reset state, not yet looking at any hand-off.
    pub fn new() -> ArmSide {
        Self::with_cores(CORES)
    }

    /// The first `n` cores only.
    pub fn with_cores(n: usize) -> ArmSide {
        let mut arm = ArmSide {
            cores: (0..n).map(Core::new).collect(),
            cycles: 0,
            slept: 0,
            stopped: None,
            handoff: None,
            bootargs: None,
            released_at: None,
            dirty: true,
            timer_due: 0,
            spis: [false; SPIS.len()],
            runnable: (1 << n) - 1,
            prof: None,
            prof_from: std::env::var("PIMU_ARM_PROF")
                .ok()
                .map(|v| v.parse().unwrap_or(0)),
            parked: 0,
            park_due: u64::MAX,
            woke: 0,
            park_on: std::env::var_os("PIMU_NO_PARK").is_none(),
            burst_on: std::env::var_os("PIMU_NO_BURST").is_none(),
            sha_on: std::env::var_os("PIMU_NO_SHA_SKIP").is_none(),
            straight_on: std::env::var_os("PIMU_NO_STRAIGHT").is_none(),
            blocks_on: crate::diag::ON && std::env::var_os("PIMU_ARM_BLOCKS").is_some(),
            stop_on_store: false,
            stored: false,
            log: Log::default(),
        };
        arm.set_detectors();
        arm
    }

    /// Tell every core's loop detector what to look for.
    fn set_detectors(&mut self) {
        for c in &mut self.cores {
            c.detect.park_on = self.park_on;
            c.detect.sha_on = self.sha_on;
        }
    }

    /// The cores as `arm_loader` releases them: read the armstub's hand-off
    /// words and put the harness's kernel arguments into the device tree.
    pub fn released(m: &mut Machine) -> ArmSide {
        let mut arm = Self::new();
        arm.log = m.log.clone();
        if let Ok(h) = armstub::read_handoff(m) {
            arm.handoff = Some(h);
            // `PIMU_BOOTARGS="initcall_debug nokaslr"`: more kernel arguments,
            // for a run that has to be read from the kernel's side.
            let extra = std::env::var("PIMU_BOOTARGS").unwrap_or_default();
            let args: Vec<&str> = armstub::BOOTARGS
                .iter()
                .copied()
                .chain(extra.split_whitespace())
                .collect();
            arm.bootargs = Some(armstub::add_bootargs(m, h.dtb, &args));
        }
        arm
    }

    /// Run until the ARM has had every cycle of modelled time since release.
    pub fn catch_up(&mut self, m: &mut Machine) {
        let now = m.systimer.cycles_at(gentimer::ARM_HZ);
        let since = now - *self.released_at.get_or_insert(now);
        if since > self.cycles {
            self.run(m, since - self.cycles);
        }
    }

    /// Run the ARM up to `until_us`, the VPU's next compare, stopping after
    /// the first cycle in which a core writes a VPU-side peripheral. Returns
    /// the microsecond the VPU wakes in.
    pub fn run_until_store(&mut self, m: &mut Machine, until_us: u64) -> u64 {
        const PER_US: u64 = gentimer::ARM_HZ / 1_000_000;
        let now = m.systimer.cycles_at(gentimer::ARM_HZ);
        let released = *self.released_at.get_or_insert(now);
        let end = (until_us * PER_US).saturating_sub(released);
        if end <= self.cycles {
            return until_us;
        }
        self.stop_on_store = true;
        self.stored = false;
        self.run(m, end - self.cycles);
        self.stop_on_store = false;
        if !std::mem::take(&mut self.stored) {
            return until_us;
        }
        (released + self.cycles).div_ceil(PER_US).min(until_us)
    }

    /// Bring every core's interrupt inputs up to date, and note when a timer
    /// next needs looking at.
    fn sync(&mut self, m: &mut Machine) {
        // Fresh, not the levels `run` saw: a core's own access may have just
        // dropped one.
        self.spis = spi_levels(m);
        for (&id, &level) in SPIS.iter().zip(&self.spis) {
            m.gic.set_spi_level(id, level);
        }
        let hz = m.arm_local.counter_hz().unwrap_or(0);
        let cycles = self.cycles;
        // A parked core is rebuilt reading the counter at the rate it parked
        // under: a new rate ends the park first.
        if self.parked != 0
            && self
                .cores
                .iter()
                .any(|c| c.park.is_some() && c.timer.hz() != hz)
        {
            self.unpark_if(m, |_, _| true, cycles);
        }
        for (id, core) in self.cores.iter_mut().enumerate() {
            core.sync(m, id, cycles, hz);
        }
        // A line the core can take ends its park: it takes it this cycle.
        let mut ids = self.parked;
        while ids != 0 {
            let id = ids.trailing_zeros() as usize;
            ids &= ids - 1;
            let c = &self.cores[id].cpu;
            if (c.irq_line && c.can_take_interrupt(false))
                || (c.fiq_line && c.can_take_interrupt(true))
            {
                self.unpark(m, id, cycles);
            }
        }
        self.timer_due = self
            .cores
            .iter()
            .flat_map(|c| [c.timer.next_event(cycles), c.wfe_until])
            .flatten()
            .min()
            .unwrap_or(u64::MAX);
        for id in 0..self.cores.len() {
            self.refresh_runnable(id);
        }
        self.dirty = false;
    }

    /// Recompute core `id`'s bit in [`Self::runnable`].
    #[inline]
    fn refresh_runnable(&mut self, id: usize) {
        let c = &self.cores[id];
        let wfe_done = c.wfe_until.is_some_and(|t| c.cpu.event || t <= self.cycles);
        if c.park.is_none() && (!c.waiting || c.cpu.irq_line || c.cpu.fiq_line || wfe_done) {
            self.runnable |= 1 << id;
        } else {
            self.runnable &= !(1 << id);
        }
    }

    /// Bring parked core `id` up to date as of cycle `t` and let it run.
    fn unpark(&mut self, m: &Machine, id: usize, t: u64) {
        let core = &mut self.cores[id];
        let Some(p) = core.park.take() else {
            return;
        };
        debug_assert!(t <= p.until, "core {id} resumed past its park");
        let n = p.resume(&mut core.cpu, m, &core.timer, t);
        core.insns += n;
        if let Some(prof) = &mut self.prof {
            let (len, el) = (p.pcs().len() as u64, core.cpu.el);
            for (i, &pc) in p.pcs().iter().enumerate() {
                let k = n / len + u64::from((i as u64) < n % len);
                *prof.entry((id, el, pc & !0xFF)).or_default() += k;
            }
        }
        self.parked &= !(1 << id);
        self.park_due = self
            .cores
            .iter()
            .filter_map(|c| c.park.as_ref().map(|p| p.until))
            .min()
            .unwrap_or(u64::MAX);
        self.refresh_runnable(id);
    }

    /// Unpark, as of cycle `t`, every parked core `wake` picks.
    fn unpark_if(&mut self, m: &Machine, wake: impl Fn(&park::Park, &Machine) -> bool, t: u64) {
        let mut ids = self.parked;
        while ids != 0 {
            let id = ids.trailing_zeros() as usize;
            ids &= ids - 1;
            if self.cores[id].park.as_ref().is_some_and(|p| wake(p, m)) {
                self.unpark(m, id, t);
            }
        }
    }

    /// After core `by` stepped in cycle `t`, unpark the cores whose inputs the
    /// step changed. One later in core order still has its turn this cycle,
    /// which the returned bits ask for.
    fn check_parked(&mut self, m: &Machine, by: usize, t: u64) -> u32 {
        let mut woke = 0;
        let mut ids = self.parked;
        while ids != 0 {
            let id = ids.trailing_zeros() as usize;
            ids &= ids - 1;
            if !self.cores[id]
                .park
                .as_ref()
                .is_some_and(|p| p.inputs_changed(m))
            {
                continue;
            }
            if id > by {
                self.unpark(m, id, t);
                woke |= 1 << id;
            } else {
                self.unpark(m, id, t + 1);
            }
        }
        woke
    }

    /// Bring every parked core up to date, for a report.
    pub fn settle(&mut self, m: &Machine) {
        let t = self.cycles;
        self.unpark_if(m, |_, _| true, t);
    }

    /// Run for `budget` cycles, or until a core stops.
    pub fn run(&mut self, m: &mut Machine, budget: u64) {
        let end = self.cycles + budget;
        if self.prof_from.is_some_and(|t| m.systimer.now_us() >= t) {
            self.prof = Some(Default::default());
            self.prof_from = None;
        }
        // The VPU side moves these lines, and it is frozen while this runs.
        if spi_levels(m) != self.spis {
            self.dirty = true;
        }
        // It may also have answered what a parked core waits for.
        if self.parked != 0 {
            let t = self.cycles;
            self.unpark_if(m, |p, m| p.inputs_changed(m), t);
        }
        while self.cycles < end && self.stopped.is_none() {
            // Interrupt state only moves when a core touches a device, the
            // GIC or a timer register, or a timer reaches its compare.
            if self.dirty || self.cycles >= self.timer_due {
                self.sync(m);
            }
            if self.cycles >= self.park_due {
                let t = self.cycles;
                self.unpark_if(m, |p, _| p.until <= t, t);
            }
            // A lone core back at the head of its SHA-256 loop: hash what the
            // time up to the next thing due has room for.
            if self.parked == 0 && self.runnable.is_power_of_two() {
                let id = self.runnable.trailing_zeros() as usize;
                if self.cores[id].detect.sha_stop {
                    let limit = end.min(self.timer_due).min(self.park_due);
                    self.sha_skip(m, id, limit);
                }
            }
            // One core takes turns and no other is parked: until something is
            // due, the cycles below would visit just it.
            if self.burst_on
                && self.runnable.is_power_of_two()
                && self.parked == 0
                && self.prof.is_none()
                && !self.blocks_on
            {
                let id = self.runnable.trailing_zeros() as usize;
                let limit = end.min(self.timer_due).min(self.park_due);
                if limit > self.cycles && !self.cores[id].detect.watching() {
                    self.stopped = self.burst(m, id, limit);
                    if self.stop_on_store && self.stored {
                        break;
                    }
                    continue;
                }
            }
            let mut awake = false;
            let mut ids = self.runnable;
            while ids != 0 {
                let id = ids.trailing_zeros() as usize;
                ids &= ids - 1;
                // Runnable and waiting means a line is up, or a `wfe` saw its
                // event or ran out: it wakes. A `wfe` that completes consumes
                // the event.
                let core = &mut self.cores[id];
                core.waiting = false;
                if core.wfe_until.take().is_some() {
                    core.cpu.event = false;
                }
                awake = true;
                match self.step_core(m, id) {
                    // Nothing changed that the runnable bits or the parked
                    // cores depend on.
                    Turn::Plain => {}
                    Turn::Full => {
                        self.refresh_runnable(id);
                        ids |= std::mem::take(&mut self.woke);
                    }
                    Turn::Stop(stop) => {
                        self.stopped = Some(stop);
                        break;
                    }
                }
            }
            if !awake {
                let wake = self.timer_due.min(self.park_due).min(end);
                let wake = wake.max(self.cycles + 1);
                // A parked core is running, as far as the guest can tell.
                if self.parked == 0 {
                    self.slept += wake - self.cycles;
                }
                self.cycles = wake;
                continue;
            }
            self.cycles += 1;
            if self.stop_on_store && self.stored {
                break;
            }
        }
    }

    /// One instruction, exception or interrupt entry on core `id`.
    fn step_core(&mut self, m: &mut Machine, id: usize) -> Turn {
        let cycles = self.cycles;
        let track = self.park_on || self.sha_on;
        let core = &mut self.cores[id];
        let secure = core.cpu.el == 3 || core.cpu.sys.scr_el3 & sysreg::SCR_NS == 0;
        let pc = core.cpu.pc;
        if let Some(p) = &mut self.prof {
            *p.entry((id, core.cpu.el, pc & !0xFF)).or_default() += 1;
        }
        let watching = core.detect.watching();
        let effects = core.cpu.shared_effects;
        let mut bus = ArmBus {
            m: &mut *m,
            timer: &mut core.timer,
            cycles,
            core: id,
            secure,
            written: None,
            io: false,
            log: watching.then_some(&mut core.detect.log),
            stores: watching.then_some(&mut core.detect.stores),
            released_at: self.released_at.unwrap_or(0),
            periph_store: false,
        };
        let step = core.cpu.step_system(&mut bus);
        let done = Stepped {
            step,
            pc,
            written: bus.written,
            io: bus.io,
            periph_store: bus.periph_store,
        };
        if crate::diag::ON {
            if let Some(b) = &mut core.blocks {
                // The fetch hint is the page this instruction came from, so the
                // physical PC costs no second translation. A step that faulted
                // before fetching leaves a stale hint, but does not retire.
                let retired = matches!(done.step, Step::Retired);
                let (pa, el) = match core.cpu.tlb.fetch_hint() {
                    Some((_, el, page)) => (page | (pc & 0xFFF), el),
                    None => (pc, core.cpu.el),
                };
                b.step(pa, el, retired, core.cpu.pc == pc.wrapping_add(4));
            }
        }
        // A plain instruction on registers and RAM needs nothing of
        // [`Self::after_step`] but the count — the same test [`Self::burst`]
        // makes.
        let plain = matches!(done.step, Step::Retired)
            && !done.io
            && !watching
            && core.cpu.shared_effects == effects
            && core.entered.is_some();
        if plain
            && (done.written.is_none()
                || (self.parked == 0
                    && !self
                        .cores
                        .iter()
                        .enumerate()
                        .any(|(k, c)| k != id && c.cpu.marked())))
        {
            let core = &mut self.cores[id];
            core.insns += 1;
            if track {
                let park = core
                    .detect
                    .retired(&core.cpu, cycles, pc, done.written.is_some());
                debug_assert!(!park);
            }
            return Turn::Plain;
        }
        match self.after_step(m, id, done) {
            Some(stop) => Turn::Stop(stop),
            None => Turn::Full,
        }
    }

    /// Core `id` alone, nothing due before `limit`: step it without the cycle
    /// loop's checks for as long as each step leaves the other cores and the
    /// interrupt state alone. The first that does not ends the burst and is
    /// finished the ordinary way.
    fn burst(&mut self, m: &mut Machine, id: usize, limit: u64) -> Option<ArmStop> {
        // Another core's exclusive mark is the one thing a plain store can
        // change; nobody else runs, so no mark can appear meanwhile.
        let marked = self
            .cores
            .iter()
            .enumerate()
            .any(|(k, c)| k != id && c.cpu.marked());
        let track = self.park_on || self.sha_on;
        let released_at = self.released_at.unwrap_or(0);
        let mut cycles = self.cycles;
        let core = &mut self.cores[id];
        core.waiting = false;
        if core.wfe_until.take().is_some() {
            core.cpu.event = false;
        }
        let Core {
            cpu,
            timer,
            detect,
            insns,
            entered,
            ..
        } = core;
        // State other cores or the run loop depend on (`Cpu::shared_effects`)
        // may change what the next step is. A line this core's own `msr daif`
        // unmasks needs no stop: lines only move when a device is touched.
        let effects = cpu.shared_effects;
        let mut bus = ArmBus {
            m: &mut *m,
            timer,
            cycles,
            core: id,
            secure: cpu.el == 3 || cpu.sys.scr_el3 & sysreg::SCR_NS == 0,
            written: None,
            io: false,
            log: None,
            stores: None,
            released_at,
            periph_store: false,
        };
        let last = 'burst: loop {
            // A straight-line run off one page: the checks around a step are
            // made once instead of once per instruction. The module docs give
            // the invariants that make that sound. Mean run: 5.3 instructions
            // on `linux`, 9.6 on the mkosi boot.
            if self.straight_on && !cpu.irq_line && !cpu.fiq_line && entered.is_some() {
                if let Some((va_page, el, pa_page)) = cpu.tlb.fetch_hint() {
                    // The alignment test is hoisted too: a PC that starts
                    // aligned and only advances by 4 stays aligned.
                    if el == cpu.el
                        && va_page == cpu.pc & !0xFFF
                        && cpu.pc & 3 == 0
                        && !detect.watching()
                    {
                        loop {
                            let pc = cpu.pc;
                            if cycles >= limit || pc & !0xFFF != va_page {
                                break;
                            }
                            let Ok(insn) = bus.fetch(pa_page | (pc & 0xFFF)) else {
                                break;
                            };
                            bus.cycles = cycles;
                            bus.written = None;
                            // What `step_system` does with an exception the
                            // instruction raised; the interrupt tests in
                            // front of it are the ones hoisted out.
                            let step = match cpu.step_fetched(insn, &mut bus) {
                                Step::Exception(e) => {
                                    cpu.take_sync(e);
                                    Step::Took(e)
                                }
                                s => s,
                            };
                            let wrote = bus.written.is_some();
                            if !matches!(step, Step::Retired)
                                || bus.io
                                || cpu.shared_effects != effects
                                || (wrote && marked)
                            {
                                break 'burst Some(Stepped {
                                    step,
                                    pc,
                                    written: bus.written,
                                    io: bus.io,
                                    periph_store: bus.periph_store,
                                });
                            }
                            *insns += 1;
                            cycles += 1;
                            if cpu.pc != pc.wrapping_add(4) {
                                // The transfer that ends the run, and the one
                                // step of it the detector can have work for.
                                if track {
                                    let park = detect.retired(cpu, cycles - 1, pc, wrote);
                                    debug_assert!(!park);
                                }
                                break;
                            }
                        }
                        if cycles >= limit || detect.watching() {
                            break None;
                        }
                        continue;
                    }
                }
            }
            bus.cycles = cycles;
            bus.written = None;
            let pc = cpu.pc;
            let step = cpu.step_system(&mut bus);
            let wrote = bus.written.is_some();
            if !matches!(step, Step::Retired)
                || bus.io
                || cpu.shared_effects != effects
                || (wrote && marked)
            {
                break Some(Stepped {
                    step,
                    pc,
                    written: bus.written,
                    io: bus.io,
                    periph_store: bus.periph_store,
                });
            }
            *insns += 1;
            // Not watching a loop yet, so this only counts backward jumps.
            if track {
                let park = detect.retired(cpu, cycles, pc, wrote);
                debug_assert!(!park);
            }
            if entered.is_none() && cpu.pc >= STUB_END {
                *entered = Some((cycles, cpu.el, cpu.pc, cpu.x[0]));
            }
            cycles += 1;
            if cycles >= limit || detect.watching() {
                break None;
            }
        };
        self.cycles = cycles;
        let done = last?;
        let stop = self.after_step(m, id, done);
        self.refresh_runnable(id);
        self.cycles += 1;
        stop
    }

    /// Core `id` back at the head of its SHA-256 loop: skip the passes the
    /// cycles up to `limit` have room for, or forget the loop.
    fn sha_skip(&mut self, m: &mut Machine, id: usize, limit: u64) {
        let marked = self
            .cores
            .iter()
            .enumerate()
            .any(|(k, c)| k != id && c.cpu.marked());
        let cycles = self.cycles;
        let core = &mut self.cores[id];
        core.detect.sha_stop = false;
        let Some(l) = core.sha.as_ref() else {
            return;
        };
        let c = &core.cpu;
        let takes = (c.irq_line && c.can_take_interrupt(false))
            || (c.fiq_line && c.can_take_interrupt(true));
        if marked || takes || core.detect.watching() || limit <= cycles {
            return;
        }
        match l.skip(&mut core.cpu, m, limit - cycles) {
            Ok((n, k)) => {
                self.cycles += n;
                core.insns += n;
                core.sha_blocks += k;
            }
            Err(()) => {
                core.sha = None;
                core.detect.sha_head = None;
            }
        }
    }

    /// What one step on core `id` in cycle [`Self::cycles`] means for the
    /// run loop and the other cores.
    fn after_step(&mut self, m: &mut Machine, id: usize, done: Stepped) -> Option<ArmStop> {
        let cycles = self.cycles;
        let Stepped {
            step,
            pc,
            written,
            io,
            periph_store,
        } = done;
        self.dirty |= io;
        self.stored |= periph_store;
        let core = &mut self.cores[id];
        let el = core.cpu.el;
        let mut wfe_until = None;
        if !matches!(step, Step::Retired) {
            core.detect.interrupted();
        }
        match step {
            Step::Retired => {
                core.insns += 1;
                if (self.park_on || self.sha_on)
                    && core
                        .detect
                        .retired(&core.cpu, cycles, pc, written.is_some())
                {
                    if let Some(p) = core.detect.park(&mut core.cpu, m, &core.timer) {
                        self.park_due = self.park_due.min(p.until);
                        self.parked |= 1 << id;
                        core.park = Some(p);
                    }
                }
                // Two passes of a long loop recorded: is it a SHA-256 block
                // loop? The core is at its head, so the run loop can skip
                // right away.
                if core.detect.sha.ready() {
                    core.sha = core.detect.sha.fit(&core.cpu, m);
                    core.detect.sha_head = core.sha.as_ref().map(|l| l.entry);
                    core.detect.sha_stop = core.sha.is_some();
                    core.sha_loops += u64::from(core.sha.is_some());
                }
            }
            Step::Wfe => {
                core.insns += 1;
                core.waiting = true;
                let until = cycles + wfe_timeout(core);
                core.wfe_until = Some(until);
                wfe_until = Some(until);
            }
            Step::Wfi => {
                core.insns += 1;
                core.waiting = true;
            }
            Step::Took(e) => {
                core.exceptions += 1;
                // `--log arm-exc`: every synchronous exception but `svc`.
                if self.log.on(Channel::ArmExc) && !matches!(e, Exception::Svc(_)) {
                    let t = el as usize;
                    // An external abort: nothing answered at this physical
                    // address, the one to look up in the memory map.
                    let pa = match &e {
                        Exception::DataAbort { fsc, .. } | Exception::InsnAbort { fsc, .. }
                            if *fsc == crate::aarch64::mmu::FSC_EXTERNAL =>
                        {
                            format!(" pa {:#x}", core.cpu.abort_pa)
                        }
                        _ => String::new(),
                    };
                    crate::log!(
                        self.log,
                        Channel::ArmExc,
                        "core {id} pc {pc:#x} -> EL{el} {e:?} esr {:#x} far {:#x}{pa}",
                        core.cpu.sys.esr[t],
                        core.cpu.sys.far[t]
                    );
                }
            }
            Step::Interrupt { .. } => core.interrupts += 1,
            Step::Unimplemented(insn) => {
                return Some(ArmStop::Unimplemented {
                    core: id,
                    pc,
                    el,
                    insn,
                })
            }
            Step::Unsupported(what) => {
                return Some(ArmStop::Unsupported {
                    core: id,
                    pc,
                    el,
                    what,
                })
            }
            Step::Exception(_) => unreachable!("step_system takes exceptions"),
        }
        if core.entered.is_none() && core.cpu.pc >= STUB_END {
            core.entered = Some((cycles, el, core.cpu.pc, core.cpu.x[0]));
        }
        let broadcast = std::mem::take(&mut core.cpu.tlb.broadcast);
        let sev = std::mem::take(&mut core.cpu.sev);
        // Sleepers an event just reached: their runnable bits need a look.
        let mut signalled = 0u32;
        // Most steps have nothing to tell the other cores.
        let others = if written.is_some() || sev || broadcast {
            self.cores.len()
        } else {
            0
        };
        for (k, other) in self.cores.iter_mut().enumerate().take(others) {
            if k == id {
                continue;
            }
            let mut event = false;
            if let Some((lo, hi)) = written {
                event |= other.cpu.snoop_write(lo, hi);
            }
            if sev {
                other.cpu.event = true;
                event = true;
            }
            if event && other.waiting {
                signalled |= 1 << k;
            }
            if broadcast {
                other.cpu.tlb.flush();
            }
        }
        while signalled != 0 {
            let k = signalled.trailing_zeros() as usize;
            signalled &= signalled - 1;
            self.refresh_runnable(k);
        }
        if let Some(until) = wfe_until {
            self.timer_due = self.timer_due.min(until);
        }
        // A store, or a device access with a side effect, may have changed
        // what a parked core waits on.
        if self.parked != 0 && (written.is_some() || io) {
            self.woke |= self.check_parked(m, id, cycles);
        }
        None
    }
}

/// How a core's turn in the cycle loop went.
enum Turn {
    /// A plain instruction: nobody else needs to hear of it.
    Plain,
    /// It went through [`ArmSide::after_step`]: the runnable bits and the
    /// parked cores need a look.
    Full,
    Stop(ArmStop),
}

/// One step of a core, as [`ArmSide::after_step`] needs it: what the step
/// did, from which PC, and what its bus saw.
struct Stepped {
    step: Step,
    pc: u64,
    written: Option<(u64, u64)>,
    io: bool,
    periph_store: bool,
}

/// Where an ARM physical address lands.
enum Target {
    /// An offset into DRAM — up to 8 GB, so not a 32-bit address.
    Ram(u64),
    /// A VPU bus address in the peripheral window.
    Periph(u32),
    Local(u32),
    Gic(u32),
    /// A CPU-physical address in the PCIe outbound window.
    Pcie(u64),
}

/// The ARM's view of the machine for one step.
struct ArmBus<'a> {
    m: &'a mut Machine,
    timer: &'a mut GenericTimer,
    cycles: u64,
    /// Which core is accessing, for the GIC's per-CPU views.
    core: usize,
    /// The access's security state, for the GIC's banked views.
    secure: bool,
    /// The physical range the step wrote to, for the other cores'
    /// exclusive monitors.
    written: Option<(u64, u64)>,
    /// The step touched something besides RAM (a device, the GIC, a timer
    /// register), so interrupt state may have moved.
    io: bool,
    /// Where the step's data reads go while the core's loop is watched.
    log: Option<&'a mut Vec<park::Read>>,
    /// Where its stores go then, for a SHA-256 recording.
    stores: Option<&'a mut Vec<park::Read>>,
    /// The system timer at release; `released_at + cycles` is the ARM's clock.
    released_at: u64,
    /// The step wrote a VPU-side peripheral: something the VPU may wake for.
    periph_store: bool,
}

impl ArmBus<'_> {
    #[inline]
    fn route(&self, addr: u64, size: u32, write: bool) -> Result<Target, Abort> {
        let abort = Abort { addr, write };
        let end = addr.checked_add(u64::from(size)).ok_or(abort)?;
        let within = |base: u32, len: u32| {
            let base = u64::from(base);
            (addr >= base && end <= base + u64::from(len)).then(|| (addr - base) as u32)
        };
        if end <= self.m.ram.len() as u64 && (end <= RAM_LOW_END || addr >= RAM_HIGH_BASE) {
            Ok(Target::Ram(addr))
        } else if PERIPH.contains(&addr) && end <= PERIPH.end {
            Ok(Target::Periph((addr - PERIPH_TO_BUS) as u32))
        } else if let Some(off) = within(armlocal::BASE, armlocal::SIZE) {
            Ok(Target::Local(off))
        } else if let Some(off) = within(gic::BASE, gic::SIZE) {
            Ok(Target::Gic(off))
        } else if PCIE.contains(&addr) && end <= PCIE.end {
            Ok(Target::Pcie(addr))
        } else {
            Err(abort)
        }
    }

    fn width(size: u32) -> Width {
        match size {
            1 => Width::Byte,
            2 => Width::Half,
            _ => Width::Word,
        }
    }

    fn accessor(&self) -> gic::Accessor {
        gic::Accessor {
            cpu: self.core,
            secure: self.secure,
        }
    }

    /// An access of at most 4 bytes.
    #[inline(always)]
    fn read32(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
        let w = Self::width(size);
        let target = self.route(addr, size, false)?;
        self.io |= !matches!(target, Target::Ram(_));
        let r = match target {
            Target::Ram(off) => {
                if self.m.ram.coherency.is_on() {
                    self.m.ram.coherency.read_by(
                        off as u32,
                        w.bytes(),
                        crate::coherency::Master::Arm,
                    );
                }
                self.m.ram.load_at(off, w)
            }
            Target::Periph(a) => self.m.load(a, w),
            Target::Local(o) => self.m.arm_local.read(o, w),
            Target::Gic(o) => {
                let acc = self.accessor();
                self.m.gic.read_as(acc, o, w)
            }
            Target::Pcie(a) => {
                return self
                    .m
                    .pcie
                    .mmio_read(a, w)
                    .map(u64::from)
                    .ok_or(Abort { addr, write: false })
            }
        };
        r.map(u64::from).map_err(|_| Abort { addr, write: false })
    }

    #[inline(always)]
    fn write32(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
        let (w, v) = (Self::width(size), value as u32);
        let target = self.route(addr, size, true)?;
        self.io |= !matches!(target, Target::Ram(_));
        let r = match target {
            Target::Ram(off) => self.m.ram.store_at(off, w, v),
            Target::Periph(a) => {
                // `--log mbox`: name what Linux asks the firmware for — the
                // first tag of each property request it posts, and its
                // first value words. The address on the wire is the bus
                // alias (`0xC000_0000 | phys`, module docs of `mbox`).
                if a == crate::spec::mbox::BASE + crate::spec::mbox::DATA1
                    && self.m.log.on(Channel::Mbox)
                {
                    let buf = self.m.ram.base() + (v & 0x3FFF_FFF0);
                    let word = |o: u32| self.m.ram.load(buf + o, Width::Word).unwrap_or(0);
                    // Inside a fast-forward slice the timer is already at the
                    // slice's end, and so is the ARM's own clock.
                    let arm_us = (self.released_at + self.cycles) / (gentimer::ARM_HZ / 1_000_000);
                    crate::log!(
                        self.m.log,
                        Channel::Mbox,
                        "ARM request (ARM at {arm_us} us) tag {:#010x} values {:#x} {:#x}",
                        word(8),
                        word(20),
                        word(24)
                    );
                }
                self.periph_store = true;
                self.m.store(a, w, v)
            }
            Target::Local(o) => self.m.arm_local.write(o, w, v),
            Target::Gic(o) => {
                let acc = self.accessor();
                self.m.gic.write_as(acc, o, w, v)
            }
            Target::Pcie(a) => {
                let m = &mut *self.m;
                return match m.pcie.mmio_write(a, w, v, &mut m.ram) {
                    true => Ok(()),
                    false => Err(Abort { addr, write: true }),
                };
            }
        };
        r.map_err(|_| Abort { addr, write: true })
    }
}

/// When a `wfe` on `core` completes on its own: the next event-stream event if
/// the stream for its EL is on, else [`WFE_BACKSTOP`].
fn wfe_timeout(core: &Core) -> u64 {
    let ctl = match core.cpu.el {
        0 | 1 => CNTKCTL_EL1,
        2 => CNTHCTL_EL2,
        _ => return WFE_BACKSTOP,
    };
    let v = core.cpu.sys.plain.get(&ctl).copied().unwrap_or(0);
    let hz = core.timer.hz();
    if v & EVNTEN == 0 || hz == 0 {
        return WFE_BACKSTOP;
    }
    let ticks = 2u64 << ((v >> 4) & 0xF);
    (ticks * gentimer::ARM_HZ / hz).clamp(1, WFE_BACKSTOP)
}

fn timer_reg(key: u32) -> Option<Reg> {
    Reg::decode(
        key >> 14,
        (key >> 11) & 7,
        (key >> 7) & 15,
        (key >> 3) & 15,
        key & 7,
    )
}

impl ArmBus<'_> {
    /// An instruction fetch from outside RAM; out of line to keep the common
    /// step's register pressure down.
    #[inline(never)]
    fn fetch_device(&mut self, addr: u64) -> Result<u32, Abort> {
        self.read(addr, 4).map(|v| v as u32)
    }
}

impl Memory for ArmBus<'_> {
    /// Straight out of RAM without `route`'s range checks, which are a
    /// measurable share of the Linux boot's host time.
    #[inline]
    fn fetch(&mut self, addr: u64) -> Result<u32, Abort> {
        let end = addr.saturating_add(4);
        if end <= self.m.ram.len() as u64 && (end <= RAM_LOW_END || addr >= RAM_HIGH_BASE) {
            return self
                .m
                .ram
                .load_at(addr, Width::Word)
                .map_err(|_| Abort { addr, write: false });
        }
        self.fetch_device(addr)
    }

    #[inline]
    fn read(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
        let value = if size == 8 {
            let lo = self.read32(addr, 4)?;
            let hi = self.read32(addr.wrapping_add(4), 4)?;
            lo | (hi << 32)
        } else {
            self.read32(addr, size)?
        };
        if let Some(log) = &mut self.log {
            log.push(park::Read { addr, size, value });
        }
        Ok(value)
    }

    #[inline]
    fn write(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
        if let Some(stores) = &mut self.stores {
            stores.push(park::Read { addr, size, value });
        }
        let end = addr.saturating_add(u64::from(size));
        self.written = Some(match self.written {
            Some((lo, hi)) => (lo.min(addr), hi.max(end)),
            None => (addr, end),
        });
        if size == 8 {
            self.write32(addr, 4, value)?;
            return self.write32(addr.wrapping_add(4), 4, value >> 32);
        }
        self.write32(addr, size, value)
    }

    fn sysreg_read(&mut self, key: u32) -> Option<u64> {
        timer_reg(key).map(|r| self.timer.read(r, self.cycles))
    }

    fn sysreg_write(&mut self, key: u32, value: u64) -> bool {
        match timer_reg(key) {
            Some(r) => {
                self.timer.write(r, self.cycles, value);
                self.io = true;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine_with(code: &[u32]) -> Machine {
        let mut m = Machine::new(1 << 20);
        for (i, w) in code.iter().enumerate() {
            let base = m.ram.base();
            m.ram.store(base + 4 * i as u32, Width::Word, *w).unwrap();
        }
        m
    }

    /// `movz`/`movk` a 32-bit constant into `xd`.
    fn mov32(rd: u32, v: u32) -> [u32; 2] {
        [
            0xD280_0000 | ((v & 0xFFFF) << 5) | rd,
            0xF2A0_0000 | ((v >> 16) << 5) | rd,
        ]
    }

    #[test]
    fn the_peripheral_window_reaches_the_shared_pl011() {
        // x1 = 0xFE201000 (PL011 DR); x0 = 'A'; str w0, [x1]; b .
        let mut code = mov32(1, 0xFE20_1000).to_vec();
        code.extend([0xD280_0820, 0xB900_0020, 0x1400_0000]);
        let mut m = machine_with(&code);
        let mut arm = ArmSide::with_cores(1);
        arm.run(&mut m, 10);
        assert_eq!(arm.stopped, None);
        assert_eq!(m.uart0.take_output(), b"A");
    }

    #[test]
    fn gic_and_generic_timer_answer() {
        // x1 = GICD_TYPER; ldr w2, [x1]; x3 = ARM local; str wzr,[x3];
        // w4 = 0x80000000, str w4, [x3, #8] (prescaler); nop x10; mrs x5,
        // cntpct_el0; b .
        let mut code = mov32(1, 0xFF84_1004).to_vec();
        code.push(0xB940_0022);
        code.extend(mov32(3, 0xFF80_0000));
        code.push(0xB900_007F);
        code.extend(mov32(4, 0x8000_0000));
        code.push(0xB900_0864);
        code.extend([0xD503_201F; 10]);
        code.extend([0xD53B_E025, 0x1400_0000]);
        let mut m = machine_with(&code);
        let mut arm = ArmSide::with_cores(1);
        arm.run(&mut m, 40);
        assert_eq!(arm.stopped, None);
        assert_eq!(
            arm.cores[0].cpu.x[2], 0xFC67,
            "GICD_TYPER as measured on the board"
        );
        // The counter started when the prescaler was written, and has run
        // at 54 MHz on a 1.5 GHz clock since: ~11 cycles -> 0 ticks, so
        // just check it is not running wild.
        assert!(arm.cores[0].cpu.x[5] < 10);
    }

    #[test]
    #[ignore]
    fn speed() {
        // x1 = 0x1000; 1: ldr x2, [x1]; add x2, x2, #1; str x2, [x1]; b 1b
        let mut m = machine_with(&[
            0xD282_0001,
            0xF940_0022,
            0x9100_0442,
            0xF900_0022,
            0x17FF_FFFD,
        ]);
        for cores in [1, CORES] {
            let mut arm = ArmSide::with_cores(cores);
            let t = std::time::Instant::now();
            arm.run(&mut m, 20_000_000);
            let insns: u64 = arm.cores.iter().map(|c| c.insns).sum();
            let s = t.elapsed().as_secs_f64();
            println!(
                "{cores} core(s): {insns} instructions in {s:.2} s = {:.1} M/s",
                insns as f64 / s / 1e6
            );
        }
    }

    #[test]
    fn every_core_runs_and_knows_which_it_is() {
        // mrs x0, mpidr_el1; and x0, x0, #0xff; add x0, x0, #1;
        // lsl x1, x0, #3; add x1, x1, #0x100; str x0, [x1]; b .
        let mut m = machine_with(&[
            0xD538_00A0,
            0x9240_1C00,
            0x9100_0400,
            0xD37D_F001,
            0x9104_0021,
            0xF900_0020,
            0x1400_0000,
        ]);
        let mut arm = ArmSide::new();
        arm.run(&mut m, 20);
        assert_eq!(arm.stopped, None);
        for id in 0..CORES as u32 {
            let slot = m.ram.base() + 0x108 + 8 * id;
            assert_eq!(m.ram.load(slot, Width::Word), Ok(id + 1));
        }
    }

    #[test]
    fn another_cores_store_breaks_an_exclusive_pair() {
        // mrs x0, mpidr_el1; and x0, x0, #0xff; cbnz x0, 1f;
        // ldxr x2, [x1]; stxr w3, x2, [x1]; b .
        // 1: str x0, [x1]; b .
        // Core 1's store lands in the cycle between core 0's ldxr and stxr.
        let code = [
            0xD538_00A0,
            0x9240_1C00,
            0xB500_0080,
            0xC85F_7C22,
            0xC803_7C22,
            0x1400_0000,
            0xF900_0020,
            0x1400_0000,
        ];
        for (cores, failed) in [(1, 0), (2, 1)] {
            let mut m = machine_with(&code);
            let mut arm = ArmSide::with_cores(cores);
            for c in &mut arm.cores {
                c.cpu.x[1] = 0x800;
            }
            arm.run(&mut m, 8);
            assert_eq!(arm.cores[0].cpu.x[3], failed, "{cores} core(s)");
        }
    }

    #[test]
    fn the_arm_keeps_up_with_the_system_timer() {
        // b . — always busy.
        let mut m = machine_with(&[0x1400_0000]);
        let mut arm = ArmSide::with_cores(1);
        arm.catch_up(&mut m);
        assert_eq!(arm.cycles, 0);
        // 54 VPU cycles = 1 µs = 1500 ARM cycles.
        m.tick(54);
        arm.catch_up(&mut m);
        assert_eq!(arm.cycles, 1500);
        // A `sleep`-style jump of 1 ms: the ARM gets the whole of it.
        m.systimer.jump(1000);
        arm.catch_up(&mut m);
        assert_eq!(arm.cycles, 1500 + 1_500_000);
    }

    #[test]
    fn a_stray_address_aborts_into_the_guest() {
        // ldr x0, [x1] with x1 = 0x1_0000_0000: nothing there. VBAR_EL3 = 0
        // so the sync vector (current EL, SP_ELx) is at 0x200.
        let mut m = machine_with(&[0xF940_0020]);
        let mut arm = ArmSide::with_cores(1);
        arm.cores[0].cpu.x[1] = 0x1_0000_0000;
        arm.run(&mut m, 1);
        assert_eq!(arm.cores[0].exceptions, 1);
        assert_eq!(arm.cores[0].cpu.pc, 0x200);
        assert_eq!(arm.cores[0].cpu.sys.far[3], 0x1_0000_0000);
        assert_eq!(arm.cores[0].cpu.abort_pa, 0x1_0000_0000);
        assert_eq!(arm.cores[0].cpu.sys.esr[3] >> 26, 0x25);
    }

    #[test]
    fn the_arm_reaches_the_vl805_through_the_pcie_window() {
        use crate::periph::pcie;
        // ldr w0, [x1]
        let mut m = machine_with(&[0xB940_0020, 0xB940_0020]);
        for (off, v) in [
            (0x9210, 3),           // RGR1_SW_INIT_1: bridge + PERST# in reset...
            (0x9210, 0),           // ...and out: the link trains
            (0x400C, 0x8000_0000), // MEM_WIN0_LO: bus side
            (0x4010, 0),
            (0x4070, 0x3FF0_0000), // BASE_LIMIT: 1 GiB
            (0x4080, 6),           // BASE_HI / LIMIT_HI: at 0x6_0000_0000
            (0x4084, 6),
            (0x9000, 1 << 20),     // EXT_CFG_INDEX: bus 1, the VL805
            (0x8010, 0x8000_0000), // BAR0
            (0x8014, 0),
            (0x8004, 0x0146), // memory space + bus master
        ] {
            m.store(pcie::BASE + off, Width::Word, v).unwrap();
        }
        let caps = m.pcie.mmio_read(0x6_0000_0000, Width::Word).unwrap();
        assert_ne!(caps, 0);
        let mut arm = ArmSide::with_cores(1);
        arm.cores[0].cpu.x[1] = 0x6_0000_0000;
        arm.run(&mut m, 1);
        assert_eq!(arm.cores[0].exceptions, 0);
        assert_eq!(arm.cores[0].cpu.x[0], u64::from(caps));
        arm.cores[0].cpu.x[1] = 0x6_3000_0000;
        arm.run(&mut m, 1);
        assert_eq!(arm.cores[0].exceptions, 1);
        assert_eq!(arm.cores[0].cpu.sys.far[3], 0x6_3000_0000);
    }

    const WFE: u32 = 0xD503_205F;
    const SEV: u32 = 0xD503_209F;
    const SEVL: u32 = 0xD503_20BF;
    const NOP: u32 = 0xD503_201F;
    const B_SELF: u32 = 0x1400_0000;

    #[test]
    fn an_unsignalled_wfe_sleeps_until_the_backstop() {
        let mut m = machine_with(&[WFE, B_SELF]);
        let mut arm = ArmSide::with_cores(1);
        arm.run(&mut m, WFE_BACKSTOP - 1);
        assert_eq!(arm.cores[0].insns, 1);
        assert_eq!(arm.cores[0].cpu.pc, 4);
        assert!(arm.slept >= WFE_BACKSTOP - 2);
        arm.run(&mut m, 10);
        assert!(arm.cores[0].insns > 1, "the backstop ends the wait");
    }

    #[test]
    fn sevl_makes_the_next_wfe_complete_at_once() {
        let mut m = machine_with(&[SEVL, WFE, NOP, B_SELF]);
        let mut arm = ArmSide::with_cores(1);
        arm.run(&mut m, 3);
        assert_eq!(arm.cores[0].insns, 3);
        assert_eq!(arm.cores[0].cpu.pc, 12);
        assert!(!arm.cores[0].cpu.event);
    }

    #[test]
    fn a_parked_core_wakes_on_sev() {
        let mut code = vec![NOP; 0x40 + 4];
        // Core 0: movz x1, #1; str x1, [x0]; sev; b .
        code[..4].copy_from_slice(&[0xD280_0021, 0xF900_0001, SEV, B_SELF]);
        // Core 1 at 0x100: wfe; ldr x1, [x0]; cbz x1, 0x100; b .
        code[0x40..].copy_from_slice(&[WFE, 0xF940_0001, 0xB4FF_FFC1, B_SELF]);
        let mut m = machine_with(&code);
        let mut arm = ArmSide::with_cores(2);
        for c in &mut arm.cores {
            c.cpu.x[0] = 0x800;
        }
        arm.cores[1].cpu.pc = 0x100;
        // Cycle 0 core 1 waits; cycle 2 core 0 signals; cycles 3..5 core 1
        // runs `ldr`, `cbz` and lands on its `b .`.
        arm.run(&mut m, 6);
        assert_eq!(arm.cores[1].cpu.pc, 0x10C, "released");
        assert_eq!(arm.cores[1].insns, 4);
    }

    #[test]
    fn a_parked_core_ignores_a_store_without_sev() {
        let mut code = vec![NOP; 0x40 + 4];
        code[..4].copy_from_slice(&[0xD280_0021, 0xF900_0001, B_SELF, B_SELF]);
        code[0x40..].copy_from_slice(&[WFE, 0xF940_0001, 0xB4FF_FFC1, B_SELF]);
        let mut m = machine_with(&code);
        let mut arm = ArmSide::with_cores(2);
        for c in &mut arm.cores {
            c.cpu.x[0] = 0x800;
        }
        arm.cores[1].cpu.pc = 0x100;
        arm.run(&mut m, 1000);
        assert_eq!(arm.cores[1].insns, 1);
        assert_eq!(arm.cores[1].cpu.pc, 0x104);
    }

    /// Parked or not, in bursts or not, `drive` must see the same run.
    fn parks_exactly(code: &[u32], cores: usize, drive: impl Fn(&mut ArmSide, &mut Machine)) {
        let run = |park: bool, burst: bool| {
            let mut m = machine_with(code);
            let mut arm = ArmSide::with_cores(cores);
            arm.park_on = park;
            arm.burst_on = burst;
            arm.set_detectors();
            drive(&mut arm, &mut m);
            arm.settle(&m);
            arm
        };
        let reference = run(false, false);
        for (park, burst) in [(true, true), (false, true), (true, false)] {
            let other = run(park, burst);
            assert_eq!(other.cycles, reference.cycles, "park {park} burst {burst}");
            for (id, (a, b)) in other.cores.iter().zip(&reference.cores).enumerate() {
                let state = |c: &Core| (c.cpu.pc, c.cpu.x, c.cpu.nzcv, c.insns, c.entered);
                assert_eq!(state(a), state(b), "core {id}, park {park} burst {burst}");
            }
        }
    }

    #[test]
    fn a_lone_core_bursts_past_another_cores_mark() {
        // mrs x0, mpidr_el1; and x0, x0, #0xff; cbnz x0, 1f;
        // x1 = 0x800; x2 = 0x2000; x3 = 100;
        // 0: str x3, [x2]; ldr x4, [x2]; add x5, x5, x4; subs x3, x3, #1;
        //    b.ne 0b; str x5, [x1]; b .
        // 1: x1 = 0x800; 2: ldxr x2, [x1]; cbnz x2, 3f; wfe; b 2b;
        // 3: add x6, x6, #1; b .
        let code = [
            0xD538_00A0,
            0x9240_1C00,
            0xB500_0160,
            0xD281_0001,
            0xD284_0002,
            0xD280_0C83,
            0xF900_0043,
            0xF940_0044,
            0x8B04_00A5,
            0xF100_0463,
            0x54FF_FF81,
            0xF900_0025,
            B_SELF,
            0xD281_0001,
            0xC85F_7C22,
            0xB500_0062,
            0xD503_205F,
            0x17FF_FFFD,
            0x9100_04C6,
            B_SELF,
        ];
        parks_exactly(&code, 2, |arm, m| {
            arm.run(m, 2_000);
            assert_eq!(arm.cores[0].cpu.x[5], 5050);
            assert_eq!(arm.cores[1].cpu.x[6], 1, "woken by the store");
        });
    }

    #[test]
    fn a_counter_delay_parks_and_ends_on_the_same_cycle() {
        // Start the counter (as above); mrs x1, cntpct_el0; add x2, x1,
        // #1000; 1: mrs x3, cntpct_el0; cmp x3, x2; b.lo 1b; add x5, x5, #1;
        // b .
        let mut code = mov32(3, 0xFF80_0000).to_vec();
        code.push(0xB900_007F);
        code.extend(mov32(4, 0x8000_0000));
        code.push(0xB900_0864);
        code.extend([0xD53B_E021, 0x910F_A022, 0xD53B_E023, 0xEB02_007F]);
        code.extend([0x54FF_FFC3, 0x9100_04A5, B_SELF]);
        parks_exactly(&code, 1, |arm, m| {
            arm.run(m, 20_000);
            assert_eq!(arm.cores[0].park.is_some(), arm.park_on);
            arm.run(m, 20_000);
            assert_eq!(arm.cores[0].cpu.x[5], 1, "left the loop");
        });
    }

    #[test]
    fn a_mailbox_poll_parks_until_the_vpu_answers() {
        // x1 = the ARM's mailbox 0 status; movz x19, #0x10, lsl #16;
        // 1: ldr w0, [x1]; cbz w0, 2f; subs x19, x19, #1; b.ne 1b; 2: b .
        let mut code = mov32(1, 0xFE00_B898).to_vec();
        code.extend([0xD2A0_0213, 0xB940_0020, 0x3400_0060, 0xF100_0673]);
        code.extend([0x54FF_FFA1, B_SELF]);
        parks_exactly(&code, 1, |arm, m| {
            arm.run(m, 5000);
            assert_eq!(arm.cores[0].park.is_some(), arm.park_on);
            m.mbox
                .write(crate::spec::mbox::DATA0_STRIDE, Width::Word, 0x1234_5678)
                .unwrap();
            arm.run(m, 100);
            assert_eq!(arm.cores[0].cpu.pc, 0x1C, "saw the answer");
        });
    }

    #[test]
    fn a_count_up_loop_runs_to_its_limit() {
        // 1: add w3, w3, #1; cmp w3, #200; b.ne 1b; add x5, x5, #1; b .
        let code = [0x1100_0463, 0x7103_207F, 0x54FF_FFC1, 0x9100_04A5, B_SELF];
        parks_exactly(&code, 1, |arm, m| {
            arm.run(m, 2000);
            assert_eq!(arm.cores[0].cpu.x[5], 1, "left the loop");
        });
    }

    #[test]
    fn a_sleeping_vpu_wakes_at_the_arms_mailbox_write() {
        // movz x2, #1500; 1: subs x2, x2, #1; b.ne 1b; x1 = the ARM's
        // mailbox 1 write register; str w3, [x1]; b .
        let mut code = vec![0xD280_BB82, 0xF100_0442, 0x54FF_FFE1];
        code.extend(mov32(1, 0xFE00_B8A0));
        code.extend([0xB900_0023, B_SELF]);
        let mut m = machine_with(&code);
        let mut arm = ArmSide::with_cores(1);
        let until = m.systimer.now_us() + 1000;
        let woke = arm.run_until_store(&mut m, until);
        // 3000-odd cycles of countdown at 1500 per microsecond.
        assert!(woke <= 3, "woke at {woke} us");
        assert!(m.recheck, "the request reached the mailbox");
        assert_eq!(arm.cores[0].cpu.pc, 0x18, "stopped right after the store");
        // Nothing more to write: this time the sleep runs to its compare.
        assert_eq!(arm.run_until_store(&mut m, until), until);
    }

    #[test]
    fn a_ram_poll_parks_until_another_core_stores() {
        let mut code = vec![NOP; 0x40 + 4];
        // Writer: movz x2, #3000; 1: subs x2, x2, #1; b.ne 1b; movz x1, #1;
        // str x1, [x0]; b .
        code[..6].copy_from_slice(&[
            0xD281_7702,
            0xF100_0442,
            0x54FF_FFE1,
            0xD280_0021,
            0xF900_0001,
            B_SELF,
        ]);
        // Poller at 0x100: 1: ldr x1, [x0]; cbz x1, 1b; add x5, x5, #1; b .
        code[0x40..].copy_from_slice(&[0xF940_0001, 0xB4FF_FFE1, 0x9100_04A5, B_SELF]);
        for (writer, poller) in [(0, 1), (1, 0)] {
            parks_exactly(&code, 2, |arm, m| {
                for c in &mut arm.cores {
                    c.cpu.x[0] = 0x800;
                }
                arm.cores[poller].cpu.pc = 0x100;
                arm.cores[writer].cpu.pc = 0;
                arm.run(m, 1000);
                assert_eq!(arm.cores[poller].park.is_some(), arm.park_on);
                arm.run(m, 9000);
                assert_eq!(arm.cores[poller].cpu.x[5], 1, "saw the flag");
            });
        }
    }

    /// `sha256_blocks(state, data, blocks, k)` from `testdata/arm/
    /// sha256_blocks.rs`, built the way its header says: a textbook SHA-256
    /// block loop that reads the state back from memory every block.
    const SHA_MEM: [u32; 133] = [
        0xd37a_e448,
        0xb400_1068,
        0xd102_c3ff,
        0xa905_7bfd,
        0xa906_6ffc,
        0xa907_67fa,
        0xa908_5ff8,
        0xa909_57f6,
        0xa90a_4ff4,
        0x6f00_e400,
        0x8b08_0028,
        0x9100_0429,
        0x9100_43ea,
        0xf900_07e8,
        0x1400_0016,
        0x0b11_0371,
        0x0b12_02b2,
        0x0b10_00d0,
        0x0b0c_02cc,
        0xb900_0011,
        0x9101_0021,
        0xb900_0412,
        0x0b0b_008b,
        0x9101_0129,
        0xb900_0810,
        0xb900_0c0c,
        0x0b0d_032c,
        0xb900_100c,
        0x0b0e_026c,
        0xb900_140c,
        0x0b0f_004c,
        0xb900_180c,
        0xf940_07e8,
        0xb900_1c0b,
        0xeb08_003f,
        0x5400_0b40,
        0xb940_0011,
        0xb940_0412,
        0xaa1f_03e5,
        0xb940_0810,
        0xb940_0c0c,
        0xaa09_03e7,
        0xb940_100d,
        0xb940_140e,
        0x2a12_03f8,
        0xb940_180f,
        0xb940_1c0b,
        0x2a10_03e6,
        0x2a0c_03f4,
        0x2a0e_03fa,
        0x2a11_03fb,
        0x2a0f_03e2,
        0x2a0b_03f7,
        0x2a0d_03f9,
        0xad00_83e0,
        0xad01_83e0,
        0x1400_0032,
        0x9240_0f19,
        0x1100_38ba,
        0x1100_24bd,
        0xb879_7959,
        0x9240_0f5a,
        0x9240_0cbe,
        0xb87a_795a,
        0x9240_0fbd,
        0xb87e_7948,
        0x1399_1f3b,
        0xb87d_795d,
        0x139a_475c,
        0x4ad9_4b7b,
        0x0b1d_0108,
        0x4ada_4f9c,
        0x4a59_0f79,
        0x4a5a_2b9a,
        0x0b08_0328,
        0x0b1a_0119,
        0xb83e_7959,
        0x1393_1a68,
        0x0a13_005a,
        0x0a33_009b,
        0x1395_0abc,
        0x2a1b_035a,
        0xb865_7865,
        0x4ad3_2d08,
        0x0b1a_02f7,
        0x4a16_00db,
        0x4ad5_379a,
        0x0a15_037b,
        0x9100_10e7,
        0x4ad3_6508,
        0xf101_031f,
        0x4ad5_5b5a,
        0x0b17_0108,
        0x0a16_00d7,
        0x4a17_0377,
        0x0b05_0108,
        0xaa18_03e5,
        0x0b1a_02f7,
        0x0b19_0108,
        0x2a15_03f8,
        0x0b14_0119,
        0x0b08_02fb,
        0x2a16_03f4,
        0x2a13_03fa,
        0x2a04_03f7,
        0x54ff_f4c0,
        0x2a19_03f3,
        0x2a1b_03f5,
        0x2a02_03e4,
        0x2a1a_03e2,
        0xf100_3cbf,
        0x2a06_03f6,
        0x2a18_03e6,
        0x9100_04b8,
        0x54ff_f8e8,
        0x385f_f0f9,
        0x3940_00fa,
        0x3940_04fb,
        0x2a1a_2339,
        0x3940_08fa,
        0x2a1b_4339,
        0x2a1a_6339,
        0x5ac0_0b39,
        0xb825_7959,
        0x17ff_ffd1,
        0xa94a_4ff4,
        0xa949_57f6,
        0xa948_5ff8,
        0xa947_67fa,
        0xa946_6ffc,
        0xa945_7bfd,
        0x9102_c3ff,
        0xd65f_03c0,
    ];

    /// `sha256_blocks_regs` from the same file: the state stays in registers
    /// from block to block and is only stored, the way OpenSSL's assembly
    /// does it.
    const SHA_REGS: [u32; 133] = [
        0xb940_0008,
        0xb940_0409,
        0xd37a_e450,
        0xb940_080a,
        0xb940_0c0b,
        0xb940_100c,
        0xb940_140d,
        0xb940_180e,
        0xb940_1c0f,
        0xb400_0f70,
        0xd102_c3ff,
        0xa905_7bfd,
        0xa906_6ffc,
        0xa907_67fa,
        0xa908_5ff8,
        0xa909_57f6,
        0xa90a_4ff4,
        0x6f00_e400,
        0x8b10_0030,
        0x9100_0431,
        0x9100_43f2,
        0xf900_07f0,
        0x1400_0016,
        0x0b08_0368,
        0x0b09_02a9,
        0x0b0a_00ca,
        0x0b0b_02cb,
        0x0b0c_032c,
        0x0b0d_00ad,
        0x0b0e_004e,
        0xb900_0008,
        0x9101_0021,
        0xb900_0409,
        0x0b0f_008f,
        0x9101_0231,
        0xb900_080a,
        0xb900_0c0b,
        0xb900_100c,
        0xb900_140d,
        0xb900_180e,
        0xf940_07f0,
        0xb900_1c0f,
        0xeb10_003f,
        0x5400_0a40,
        0xaa1f_03f3,
        0xaa11_03e7,
        0x2a09_03f8,
        0x2a0a_03e6,
        0x2a0b_03f4,
        0x2a0d_03fa,
        0x2a0e_03e2,
        0x2a0f_03f7,
        0x2a08_03fb,
        0x2a0c_03f9,
        0xad00_83e0,
        0xad01_83e0,
        0x1400_0032,
        0x9240_0f19,
        0x1100_3a7a,
        0x1100_267d,
        0xb879_7a59,
        0x9240_0f5a,
        0x9240_0e7e,
        0xb87a_7a5a,
        0x9240_0fbd,
        0xb87e_7a50,
        0x1399_1f3b,
        0xb87d_7a5d,
        0x139a_475c,
        0x4ad9_4b7b,
        0x0b1d_0210,
        0x4ada_4f9c,
        0x4a59_0f79,
        0x4a5a_2b9a,
        0x0b10_0330,
        0x0b1a_0219,
        0xb83e_7a59,
        0x1385_18b0,
        0x0a05_005a,
        0x0a25_009b,
        0x1395_0abc,
        0x2a1b_035a,
        0xb873_7873,
        0x4ac5_2e10,
        0x0b1a_02f7,
        0x4a16_00db,
        0x4ad5_379a,
        0x0a15_037b,
        0x9100_10e7,
        0x4ac5_6610,
        0xf101_031f,
        0x4ad5_5b5a,
        0x0b17_0210,
        0x0a16_00d7,
        0x4a17_0377,
        0x0b13_0210,
        0xaa18_03f3,
        0x0b1a_02f7,
        0x0b19_0210,
        0x2a15_03f8,
        0x0b14_0219,
        0x0b10_02fb,
        0x2a16_03f4,
        0x2a05_03fa,
        0x2a04_03f7,
        0x54ff_f5c0,
        0x2a19_03e5,
        0x2a1b_03f5,
        0x2a02_03e4,
        0x2a1a_03e2,
        0xf100_3e7f,
        0x2a06_03f6,
        0x2a18_03e6,
        0x9100_0678,
        0x54ff_f8e8,
        0x385f_f0f9,
        0x3940_00fa,
        0x3940_04fb,
        0x2a1a_2339,
        0x3940_08fa,
        0x2a1b_4339,
        0x2a1a_6339,
        0x5ac0_0b39,
        0xb833_7a59,
        0x17ff_ffd1,
        0xa94a_4ff4,
        0xa949_57f6,
        0xa948_5ff8,
        0xa947_67fa,
        0xa946_6ffc,
        0xa945_7bfd,
        0x9102_c3ff,
        0xd65f_03c0,
    ];

    const SHA_IV: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    /// Run `func(0x8000, 0x10000, blocks, 0x9000)` at 0x1000 from a driver
    /// at 0 that then spins at 0x30, `slice` cycles a call to `run`, with
    /// the SHA-256 skip on and off (module docs, "SHA-256 loops"): the cores,
    /// the cycle count and every byte of RAM have to come out the same.
    /// Returns the hash the loop left at 0x8000 and how many blocks the
    /// skip hashed.
    fn hashes_exactly(func: &[u32], k: &[u32; 64], blocks: u32, slice: u64) -> ([u32; 8], u64) {
        let mut code = [
            mov32(0, 0x8000),
            mov32(1, 0x1_0000),
            mov32(2, blocks),
            mov32(3, 0x9000),
            mov32(4, 0xF000),
        ]
        .concat();
        // mov sp, x4; bl 0x1000; b .
        code.extend([0x9100_009F, 0x9400_03F5, B_SELF]);
        let data: Vec<u8> = (0..64 * blocks).map(|i| (i * 7 + 3) as u8).collect();
        let run = |sha: bool| {
            let mut m = machine_with(&code);
            let base = m.ram.base();
            let words = func.iter().map(|&w| (0x1000, w));
            let words = words.enumerate().map(|(i, (a, w))| (a + 4 * i as u32, w));
            let consts = SHA_IV
                .iter()
                .enumerate()
                .map(|(i, &w)| (0x8000 + 4 * i as u32, w));
            let consts = consts.chain(
                k.iter()
                    .enumerate()
                    .map(|(i, &w)| (0x9000 + 4 * i as u32, w)),
            );
            for (a, w) in words.chain(consts) {
                m.ram.store(base + a, Width::Word, w).unwrap();
            }
            for (i, &b) in data.iter().enumerate() {
                let a = base + 0x1_0000 + i as u32;
                m.ram.store(a, Width::Byte, u32::from(b)).unwrap();
            }
            let mut arm = ArmSide::with_cores(1);
            arm.sha_on = sha;
            arm.set_detectors();
            while arm.cores[0].cpu.pc != 0x30 && arm.cycles < 50_000_000 {
                arm.run(&mut m, slice);
            }
            arm.settle(&m);
            assert_eq!(arm.stopped, None);
            (arm, m)
        };
        let (off, m_off) = run(false);
        let (on, m_on) = run(true);
        assert_eq!(off.cores[0].cpu.pc, 0x30, "the loop finished");
        assert_eq!(off.cores[0].sha_blocks, 0);
        assert_eq!(on.cycles, off.cycles);
        let state = |c: &Core| (c.cpu.pc, c.cpu.x, c.cpu.sp_el, c.cpu.nzcv, c.cpu.v, c.insns);
        assert_eq!(state(&on.cores[0]), state(&off.cores[0]));
        assert!(m_on.ram.as_slice() == m_off.ram.as_slice(), "RAM differs");
        let hash = std::array::from_fn(|i| {
            m_on.ram
                .load(m_on.ram.base() + 0x8000 + 4 * i as u32, Width::Word)
                .unwrap()
        });
        (hash, on.cores[0].sha_blocks)
    }

    fn sha256_of(blocks: u32) -> [u32; 8] {
        let mut state = SHA_IV;
        for n in 0..blocks {
            let block = std::array::from_fn(|i| ((64 * n + i as u32) * 7 + 3) as u8);
            sha::compress(&mut state, &block);
        }
        state
    }

    #[test]
    fn a_sha256_block_loop_is_hashed_natively_and_exactly() {
        for (func, blocks, slice) in [
            (&SHA_MEM, 3000, 5_000_000),
            (&SHA_MEM, 700, 50_000),
            (&SHA_REGS, 3000, 5_000_000),
            (&SHA_REGS, 700, 50_000),
        ] {
            let (hash, skipped) = hashes_exactly(func, &sha::K, blocks, slice);
            assert_eq!(hash, sha256_of(blocks));
            assert!(
                skipped > blocks as u64 / 2,
                "{skipped} of {blocks} blocks skipped"
            );
        }
    }

    #[test]
    fn a_loop_that_is_almost_sha256_runs_block_by_block() {
        let mut k = sha::K;
        k[40] ^= 1;
        let (hash, skipped) = hashes_exactly(&SHA_MEM, &k, 300, 5_000_000);
        assert_ne!(hash, sha256_of(300));
        assert_eq!(skipped, 0);
    }
}
