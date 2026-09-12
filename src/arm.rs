//! The ARM side of the machine (#40): the BCM2711's four Cortex-A72 cores on
//! its ARM physical map, run in lock-step with the VPU.
//!
//! ## Release
//!
//! The cores stay in reset until `arm_loader` lets them go, which it does by
//! writing the ARM control block ([`crate::periph::armctrl`]). All four then
//! start the way the SoC starts them: PC 0, EL3, `DAIF` masked — in the
//! firmware's armstub ([`crate::armstub`]). Every core sets up its own banked
//! GIC state and drops to non-secure EL2; core 0 jumps to the kernel with
//! `x0` = the dtb, and cores 1..3 park in the stub, polling their spin-table
//! word until Linux writes an entry point there.
//!
//! ## Address map
//!
//! Low-peripheral mode, the one the handed-over device tree describes:
//!
//! ```text
//!   0x0_0000_0000 ..             RAM: the VPU's SDRAM, no aliases
//!   0x0_FC00_0000 .. 0xFF80_0000  peripherals; ARM 0xFC00_0000 + x is VPU
//!                                 bus address 0x7C00_0000 + x
//!   0x0_FF80_0000                 ARM local block (periph/armlocal.rs)
//!   0x0_FF84_0000                 GIC-400 (periph/gic.rs)
//!   0x6_0000_0000 .. 0x8_0000_0000  PCIe outbound window (periph/pcie.rs):
//!                                 the VL805's BAR0 wherever the root
//!                                 complex and the endpoint put it
//! ```
//!
//! Anything else is a bus abort. RAM accesses go straight to the backing
//! store rather than through [`Machine`]'s VPU address decode, so they do not
//! count towards the VPU run loop's progress heuristics.
//!
//! ## Time and scheduling
//!
//! One ARM instruction is one cycle at the nominal [`gentimer::ARM_HZ`], on
//! every core: the cores take turns, one instruction each per cycle in core
//! order, so they share one cycle count and read the same counter. After
//! every VPU step the run loop calls [`ArmSide::catch_up`], which runs the
//! ARM until it has had as many cycles as the system timer says have passed
//! since release: about 28 per VPU step, and a whole slice at once when the
//! VPU's `sleep` or `usleep` fast-forward jumps the counter. Either way a run
//! is a pure function of its inputs — the reproducibility the regression
//! bench depends on — and the ARM's clock never falls behind the VPU's.
//!
//! A core in `wfi` sits out its turns until the GIC signals it (masked or
//! not, as the architecture says); when all of them wait, time skips ahead
//! to the next generic-timer event inside the slice.
//!
//! A fast-forward slice runs with the VPU frozen, so a mailbox request the
//! ARM makes in one is seen by the VPU only when the slice ends (at most one
//! VPU timer interval late).
//!
//! ## Between cores
//!
//! - Exclusive monitor: a store by one core into the 64-byte granule another
//!   core has marked with `ldxr` clears that core's mark, so its `stxr`
//!   fails — the global monitor's part in a spinlock.
//! - TLB maintenance: every `TLBI` flushes all the cores' TLBs, which covers
//!   the broadcast (inner-shareable) forms; invalidating more is allowed.
//! - `wfe` waits for an event (ARM ARM D1.16): `sev` on any core, the
//!   global monitor clearing the core's mark (the exclusive monitor above),
//!   an interrupt line, or an exception return. It also gives up at the next
//!   generic-timer event-stream event if the stream is enabled, and after
//!   [`WFE_BACKSTOP`] if not. The architecture allows a `wfe` to complete
//!   early, so the backstop is always safe; it only bounds how long a wait
//!   nobody signals can take. Before this, `wfe` was a no-op, so cores parked
//!   in a holding pen (TF-A's, UEFI's) spun for the whole boot and the cycle
//!   loop never found every core asleep.
//!
//! Not yet: stage 2 translation (a core that sets `HCR_EL2.VM` stops with
//! [`ArmStop::Unsupported`]).

use crate::aarch64::{sysreg, Abort, Cpu, Exception, Memory, Step};
use crate::armstub::{self, Handoff};
use crate::bus::{Bus, MmioDevice, Width};
use crate::machine::Machine;
use crate::periph::gentimer::{self, GenericTimer, Reg, Which};
use crate::periph::{armlocal, gic};

/// The number of cores: BCM2711 has four A72s.
pub const CORES: usize = 4;

/// The peripheral window, and how far below it the VPU sees the same thing.
const PERIPH: std::ops::Range<u64> = 0xFC00_0000..0xFF80_0000;
const PERIPH_TO_BUS: u64 = 0x8000_0000;

/// The PCIe outbound window. What decodes in it — the VL805's BAR0, through
/// `CPU_2_PCIE_MEM_WIN0` — is up to the root complex (`periph/pcie.rs`).
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
    /// An instruction the interpreter does not implement yet.
    Unimplemented {
        core: usize,
        pc: u64,
        el: u32,
        insn: u32,
    },
    /// The guest enabled something not modelled yet.
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
    /// Waiting in `wfe`: the cycle it gives up and completes anyway — the
    /// next event-stream event, or [`WFE_BACKSTOP`] (module docs, "Between
    /// cores").
    pub wfe_until: Option<u64>,
    /// The first time the core's PC left the armstub: `(cycle, EL, PC, x0)`.
    /// For core 0 that is the kernel entry.
    pub entered: Option<(u64, u32, u64, u64)>,
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
        }
    }

    /// Bring this core's interrupt inputs up to date: the counter's rate,
    /// its timers' lines into its GIC PPIs, and the GIC's verdict onto it.
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
    /// The kernel command line, before and after [`armstub::BOOTARGS`] were
    /// added at release.
    pub bootargs: Option<Result<(String, String), String>>,
    /// The system timer, in ARM cycles, at the first [`Self::catch_up`].
    released_at: Option<u64>,
    /// Interrupt state has to be recomputed before the next step.
    dirty: bool,
    /// The earliest cycle a generic timer's output can rise.
    timer_due: u64,
    /// The [`SPIS`] lines into the GIC, as last seen.
    spis: [bool; SPIS.len()],
    /// Bit `id` set when core `id` takes its turn this cycle: not in `wfi`,
    /// or woken by a line that is up. A core's lines only move in
    /// [`Self::sync`] and its `waiting` only in its own step, so recomputing
    /// the bits there is exact, and the cycle loop visits just these cores —
    /// in the same order — instead of re-testing all four every cycle (#43).
    runnable: u32,
}

/// The device interrupt lines wired to the GIC: the mailbox, eMMC2, the two
/// GENET lines, the PL011, and the PCIe endpoint's INTA and MSI.
const SPIS: [u32; 7] = [
    gic::ID_MAILBOX,
    gic::ID_EMMC2,
    gic::ID_GENET_A,
    gic::ID_GENET_B,
    gic::ID_PL011,
    gic::ID_PCIE_INTA,
    gic::ID_PCIE_MSI,
];

fn spi_levels(m: &Machine) -> [bool; SPIS.len()] {
    let [genet_a, genet_b] = m.genet.irq_lines();
    [
        m.mbox.arm_irq_asserted(),
        m.emmc2.irq_asserted(),
        genet_a,
        genet_b,
        m.uart0.irq_line(),
        m.pcie.intx_line(),
        m.pcie.msi_line(),
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
        ArmSide {
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
        }
    }

    /// The cores as `arm_loader` releases them: read the armstub's hand-off
    /// words, and put the harness's kernel arguments into the device tree
    /// before the first instruction runs.
    pub fn released(m: &mut Machine) -> ArmSide {
        let mut arm = Self::new();
        if let Ok(h) = armstub::read_handoff(m) {
            arm.handoff = Some(h);
            // `RVF_BOOTARGS="initcall_debug nokaslr"`: more kernel arguments,
            // for a run that has to be read from the kernel's side.
            let extra = std::env::var("RVF_BOOTARGS").unwrap_or_default();
            let args: Vec<&str> = armstub::BOOTARGS
                .iter()
                .copied()
                .chain(extra.split_whitespace())
                .collect();
            arm.bootargs = Some(armstub::add_bootargs(m, h.dtb, &args));
        }
        arm
    }

    /// Run until the ARM has had every cycle of modelled time since release
    /// (module docs, "Time and scheduling").
    pub fn catch_up(&mut self, m: &mut Machine) {
        let now = m.systimer.cycles_at(gentimer::ARM_HZ);
        let since = now - *self.released_at.get_or_insert(now);
        if since > self.cycles {
            self.run(m, since - self.cycles);
        }
    }

    /// Bring every core's interrupt inputs up to date (module docs, "Time
    /// and scheduling"), and note when a timer next needs looking at.
    fn sync(&mut self, m: &mut Machine) {
        // Fresh, not the levels `run` saw: a core's own access (reading the
        // mailbox, acking a device) may just have dropped one.
        self.spis = spi_levels(m);
        for (&id, &level) in SPIS.iter().zip(&self.spis) {
            m.gic.set_spi_level(id, level);
        }
        let hz = m.arm_local.counter_hz().unwrap_or(0);
        let cycles = self.cycles;
        for (id, core) in self.cores.iter_mut().enumerate() {
            core.sync(m, id, cycles, hz);
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
        if !c.waiting || c.cpu.irq_line || c.cpu.fiq_line || wfe_done {
            self.runnable |= 1 << id;
        } else {
            self.runnable &= !(1 << id);
        }
    }

    /// Run for `budget` cycles, or until a core stops.
    pub fn run(&mut self, m: &mut Machine, budget: u64) {
        let end = self.cycles + budget;
        // The VPU side moves these lines, and it is frozen while this runs.
        if spi_levels(m) != self.spis {
            self.dirty = true;
        }
        while self.cycles < end && self.stopped.is_none() {
            // Interrupt state only moves when a core touches a device, the
            // GIC or a timer register, or a timer reaches its compare.
            if self.dirty || self.cycles >= self.timer_due {
                self.sync(m);
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
                if let Some(stop) = self.step_core(m, id) {
                    self.stopped = Some(stop);
                    break;
                }
                self.refresh_runnable(id);
            }
            if !awake {
                let wake = self.timer_due.min(end).max(self.cycles + 1);
                self.slept += wake - self.cycles;
                self.cycles = wake;
                continue;
            }
            self.cycles += 1;
        }
    }

    /// One instruction (or exception or interrupt entry) on core `id`, and
    /// what it means for the others (module docs, "Between cores").
    fn step_core(&mut self, m: &mut Machine, id: usize) -> Option<ArmStop> {
        let cycles = self.cycles;
        let core = &mut self.cores[id];
        let secure = core.cpu.el == 3 || core.cpu.sys.scr_el3 & sysreg::SCR_NS == 0;
        let pc = core.cpu.pc;
        let mut bus = ArmBus {
            m,
            timer: &mut core.timer,
            cycles,
            core: id,
            secure,
            written: None,
            io: false,
        };
        let step = core.cpu.step_system(&mut bus);
        let written = bus.written;
        self.dirty |= bus.io;
        let el = core.cpu.el;
        let mut wfe_until = None;
        match step {
            Step::Retired => core.insns += 1,
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
                // `RVF_DBG_ARM_EXC`: every synchronous exception a core takes
                // except `svc` (Linux's syscalls), with what the guest's own
                // handler will see in `ESR_ELx`/`FAR_ELx`.
                if dbg_arm_exc() && !matches!(e, Exception::Svc(_)) {
                    let t = el as usize;
                    eprintln!(
                        "[arm-exc] core {id} pc {pc:#x} -> EL{el} {e:?} esr {:#x} far {:#x}",
                        core.cpu.sys.esr[t], core.cpu.sys.far[t]
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
        for (k, other) in self.cores.iter_mut().enumerate() {
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
        None
    }
}

/// Where an ARM physical address lands.
enum Target {
    Ram(u32),
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
}

impl ArmBus<'_> {
    fn route(&self, addr: u64, size: u32, write: bool) -> Result<Target, Abort> {
        let abort = Abort { addr, write };
        let end = addr.checked_add(u64::from(size)).ok_or(abort)?;
        let within = |base: u32, len: u32| {
            let base = u64::from(base);
            (addr >= base && end <= base + u64::from(len)).then(|| (addr - base) as u32)
        };
        if end <= self.m.ram.len() as u64 {
            Ok(Target::Ram(addr as u32))
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
    fn read32(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
        let w = Self::width(size);
        let target = self.route(addr, size, false)?;
        self.io |= !matches!(target, Target::Ram(_));
        let r = match target {
            Target::Ram(a) => self.m.ram.load(self.m.ram.base() + a, w),
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

    fn write32(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
        let (w, v) = (Self::width(size), value as u32);
        let target = self.route(addr, size, true)?;
        self.io |= !matches!(target, Target::Ram(_));
        let r = match target {
            Target::Ram(a) => {
                let base = self.m.ram.base();
                self.m.ram.store(base + a, w, v)
            }
            Target::Periph(a) => {
                // `RVF_DBG_MBOX`: name what Linux asks the firmware for — the
                // first tag of each property request it posts, and its
                // first value words. The address on the wire is the bus
                // alias (`0xC000_0000 | phys`, module docs of `mbox`).
                if a == crate::periph::mbox::ARM_BASE + 0x20 && self.m.mbox.debug() {
                    let buf = self.m.ram.base() + (v & 0x3FFF_FFF0);
                    let word = |o: u32| self.m.ram.load(buf + o, Width::Word).unwrap_or(0);
                    eprintln!(
                        "[mbox] ARM request tag {:#010x} values {:#x} {:#x}",
                        word(8),
                        word(20),
                        word(24)
                    );
                }
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

/// How many cycles a `wfe` about to wait on `core` may take before it
/// completes on its own: until the next event-stream event if the stream
/// that applies at its EL is on (`CNTHCTL_EL2` at EL2, `CNTKCTL_EL1` below;
/// an event every `2^(EVNTI + 1)` counter ticks), else [`WFE_BACKSTOP`].
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

fn dbg_arm_exc() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RVF_DBG_ARM_EXC").is_some())
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

impl Memory for ArmBus<'_> {
    /// Straight out of RAM, where code runs, without `route`'s range checks:
    /// they were a measurable share of the Linux boot's host time (#43). The
    /// same access `read32` would make for a RAM target, which leaves `io`
    /// clear.
    fn fetch(&mut self, addr: u64) -> Result<u32, Abort> {
        if addr.saturating_add(4) <= self.m.ram.len() as u64 {
            return self
                .m
                .ram
                .load(self.m.ram.base() + addr as u32, Width::Word)
                .map_err(|_| Abort { addr, write: false });
        }
        self.read(addr, 4).map(|v| v as u32)
    }

    fn read(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
        if size == 8 {
            let lo = self.read32(addr, 4)?;
            let hi = self.read32(addr.wrapping_add(4), 4)?;
            return Ok(lo | (hi << 32));
        }
        self.read32(addr, size)
    }

    fn write(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
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

    /// Interpreter speed, without the VPU: `cargo test --release --lib
    /// arm::tests::speed -- --ignored --nocapture`.
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
        assert_eq!(arm.cores[0].cpu.sys.esr[3] >> 26, 0x25);
    }

    /// UEFI's xHCI driver reads the VL805's registers straight through the
    /// PCIe outbound window; it used to take an external abort at
    /// `0x6_0000_0000`. Outside BAR0 the window still aborts.
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

    /// Nothing signals a lone `wfe`: the core sleeps until the backstop, and
    /// the cycle loop skips the time instead of spinning through it.
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

    /// `sevl; wfe` is the idiom that primes a wait loop: the local event is
    /// consumed and the `wfe` does not wait.
    #[test]
    fn sevl_makes_the_next_wfe_complete_at_once() {
        let mut m = machine_with(&[SEVL, WFE, NOP, B_SELF]);
        let mut arm = ArmSide::with_cores(1);
        arm.run(&mut m, 3);
        assert_eq!(arm.cores[0].insns, 3);
        assert_eq!(arm.cores[0].cpu.pc, 12);
        assert!(!arm.cores[0].cpu.event);
    }

    /// A holding pen like TF-A's (`wfe; ldr; cbz`): the parked core sleeps
    /// until another core writes its release word and signals with `sev`.
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

    /// Without the `sev` the same pen stays asleep: a plain store to a
    /// location nobody holds exclusively is not an event.
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
}
