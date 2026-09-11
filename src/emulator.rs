//! Top-level emulator: owns the [`Vpu`] and the [`Machine`] as siblings and
//! drives the run loop.

use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use crate::bus::{Bus, Width};
use crate::diag_eprintln;
use crate::machine::{Console, Machine};
use crate::time::Stopwatch;
use crate::vpu::{Stop, UnimplPolicy, Vpu};

/// The VPU reset vector `start4.elf` is entered at (`.crypto` region). The
/// BCM2711 boot ROM releases both VPU cores here; the bootloader also jumps
/// here after loading the image. Used as core 1's default entry.
pub const START4_ENTRY: u32 = 0xFEC0_0200;

/// Offset of `start4.elf`'s ThreadX-SMP dispatch-module global within `.sdata`.
///
/// It holds a pointer to the per-core scheduler object once `_tx_thread_smp`
/// init has registered it. Core 1's very first instructions after the
/// trampoline (`0x3EC2_CC28` → `0x3ED6_50B4`) do `b *([[gp+3672]] + 24)`, so
/// releasing core 1 before this is populated jumps it through a null vtable.
/// The core-1 spawn is gated on it being set.
///
/// This is an offset from `gp`, not an absolute address, and it is resolved
/// against the live `gp` register — see [`Emulator::smp_dispatch_global`].
/// Hardcoding the absolute (`0x3EE0_3B78` in the build this was read from)
/// silently version-locked the model: a firmware bump that moves `.sdata`
/// leaves that address holding something else, and core 1 then either never
/// spawns or spawns at the wrong moment, with nothing reporting it. For a
/// bench whose whole purpose is diffing one firmware version against another
/// (#5, #25) that is the worst available failure mode.
const SMP_DISPATCH_GP_OFFSET: u32 = crate::firmware::addrs::SMP_DISPATCH_GP_OFFSET;

pub struct Emulator {
    pub cpu: Vpu,
    /// VPU core 1. `None` until `start4.elf`'s trampoline releases it by writing
    /// a start vector to the core-control block; then the run loop interleaves
    /// it with core 0 over the shared bus.
    pub cpu1: Option<Vpu>,
    /// Reset PC for VPU core 1 when the firmware releases it via the core-control
    /// block. Defaults to [`START4_ENTRY`] — the shared VPU reset vector the boot
    /// ROM releases *both* cores at; core 1 runs start4's trampoline from there
    /// and diverges on `version` bit 16. Override for tests / direct-load runs.
    pub core1_entry: Option<u32>,
    /// Latched once the firmware signals it wants core 1 up (a code-address
    /// write to the CoreCtl run-state words). The actual spawn is deferred
    /// until the dispatch global is populated — see [`SMP_DISPATCH_GP_OFFSET`].
    core1_release_armed: bool,
    pub machine: Machine,
    /// Diagnostics for [`Emulator::run`]. `None` means "ask the environment",
    /// which is what the hosted frontend wants and what every run has always
    /// done; a `no_std` frontend has no environment to ask, so it sets this.
    pub diag: Option<crate::diag::DiagConfig>,
}

/// Stopping conditions for [`Emulator::run`].
#[derive(Debug, Clone)]
pub struct RunLimits {
    /// Optional cap on retired instructions. `None` = run until the wall clock
    /// (or another stop condition) ends the run.
    pub max_steps: Option<u64>,
    /// Optional wall-clock cap.
    pub max_wall: Option<Duration>,
    /// Stop cleanly when the PC reaches this address (e.g. an ARM-handoff stub).
    pub stop_pc: Option<u32>,
    /// Stop if the PC revisits the same address this many steps in a row with no
    /// console output (tight spin / wfi-style wait). 0 disables.
    pub idle_spin_limit: u64,
    /// Stop once the firmware has printed nothing for this many microseconds of
    /// *modelled* time. A healthy boot logs continuously — the largest gap in
    /// `examples-on-real-hardware/vc4-boot.log` is about a second, and the
    /// model's own worst gap (the kernel load) is thirteen. Once the firmware
    /// wedges, output stops but modelled time keeps advancing, because `sleep`
    /// fast-forwards the system timer. That makes console silence a far better
    /// stuck-detector than any PC-window heuristic, which the ThreadX tick
    /// defeats by bumping the progress counters forever. 0 disables.
    pub silent_us: u64,
}

impl Default for RunLimits {
    fn default() -> Self {
        RunLimits {
            max_steps: Some(5_000_000),
            max_wall: Some(Duration::from_secs(30)),
            stop_pc: None,
            idle_spin_limit: 0,
            silent_us: 0,
        }
    }
}

/// A `u32` that debug-prints as hex. Addresses in a `RunEnd` are read by people
/// comparing them against a disassembly, and decimal is useless for that.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Hex(pub u32);

impl core::fmt::Debug for Hex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#010x}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    /// The core halted (swi/sleep/breakpoint).
    Halted(Stop),
    /// `stop_pc` reached.
    StopPc(u32),
    /// `max_steps` hit.
    StepLimit,
    /// `max_wall` elapsed.
    TimeLimit,
    /// Detected a tight spin with no output.
    IdleSpin(u32),
    /// The firmware stopped logging for [`RunLimits::silent_us`] of modelled
    /// time while still executing — wedged rather than merely slow.
    Stuck {
        pc: Hex,
        /// Microseconds of modelled time since the last console output.
        silent_us: u64,
        /// Instructions retired in that window.
        retired: u64,
    },
    /// VPU core 1 halted (swi/sleep/breakpoint/fault). Core 0 may still have
    /// been running; check the report's `pc` and `core1_pc`.
    Core1Halted(Stop),
    /// Firmware asked the SoC to reset (PM `RSTC`). The caller should re-run
    /// from a fresh machine seeded with the (possibly updated) flash image.
    Reset,
}

#[derive(Debug, Clone)]
pub struct RunReport {
    pub end: RunEnd,
    pub retired: u64,
    pub cycles: u64,
    pub skipped: u64,
    pub stub_hits: u64,
    pub bus_errors: u64,
    pub wall: Duration,
    pub pc: u32,
    /// Everything the console UART transmitted during the run.
    pub console: Vec<u8>,
    /// True if [`Self::console`] was already streamed to stderr as it was
    /// produced, so the summary does not need to repeat it.
    pub console_streamed: bool,
    /// Distinct unimplemented instructions encountered (reconnaissance).
    pub unimpl: Vec<crate::vpu::exec::UnimplHit>,
    /// Final register file (r0..r31) of core 0.
    pub regs: [u32; 32],
    /// Core 1 state, once it was released.
    pub core1_pc: Option<u32>,
    pub core1_retired: Option<u64>,
    pub core1_end: Option<RunEnd>,
    /// `start4.elf` boot-progress tags (`0xCEC0_2000`), in order.
    pub phase_tags: Vec<u32>,
    /// The firmware released core 1, but the ThreadX-SMP dispatch global never
    /// became non-zero, so core 1 was never spawned.
    ///
    /// Worth reporting rather than passing over in silence: the most likely
    /// cause is a firmware whose `.sdata` layout moved, which would make
    /// [`SMP_DISPATCH_GP_OFFSET`] point at the wrong word (#25).
    pub core1_release_never_resolved: bool,
}

/// What [`Emulator::diag`] falls back to: the `RVF_*` environment in a hosted
/// build, and the quiet defaults where there is no environment to read.
fn default_diag() -> crate::diag::DiagConfig {
    #[cfg(feature = "std")]
    {
        crate::diag::DiagConfig::from_env()
    }
    #[cfg(not(feature = "std"))]
    {
        crate::diag::DiagConfig::quiet()
    }
}

impl Emulator {
    pub fn new(machine: Machine, entry: u32) -> Emulator {
        Emulator {
            cpu: Vpu::new(entry),
            cpu1: None,
            core1_entry: None,
            core1_release_armed: false,
            machine,
            diag: None,
        }
    }

    /// Release VPU core 1 at `entry` (the shared trampoline), inheriting core 0's
    /// unimpl policy. Core 1 sets its own exception-vector base from the
    /// trampoline, so leave `exc_vbase` at 0 here.
    /// Has `_tx_thread_smp` init registered the per-core scheduler object yet?
    ///
    /// Resolved against the live `gp` rather than a baked-in address, so a
    /// firmware whose `.sdata` sits somewhere else still gates correctly. `gp`
    /// is zero until the firmware establishes it, and an unresolved gate reads
    /// as "not ready", which is the safe direction: core 1 stays parked rather
    /// than branching through a null vtable.
    fn smp_dispatch_ready(&mut self) -> bool {
        let gp = self.cpu.regs.get(crate::vpu::reg::GP);
        if gp == 0 {
            return false;
        }
        self.machine
            .load(gp.wrapping_add(SMP_DISPATCH_GP_OFFSET), Width::Word)
            .unwrap_or(0)
            != 0
    }

    fn spawn_core1(&mut self, entry: u32) {
        let mut c1 = Vpu::new(entry);
        c1.core_id = 1;
        c1.on_unimpl = self.cpu.on_unimpl;
        c1.trace = self.cpu.trace;
        c1.trace_cf_only = self.cpu.trace_cf_only;
        c1.trace_cap = self.cpu.trace_cap;
        c1.trace_from = self.cpu.trace_from;
        self.cpu1 = Some(c1);
    }

    /// Bring up VPU core 1 immediately at `entry` (same as core 0). The BCM2711
    /// boot ROM releases both VPU cores at `start4.elf`'s entry at once; they
    /// diverge on `version` bit 16 inside the trampoline.
    pub fn start_smp(&mut self, entry: u32) {
        let mut c1 = Vpu::new(entry);
        c1.core_id = 1;
        c1.on_unimpl = self.cpu.on_unimpl;
        c1.trace = self.cpu.trace;
        c1.trace_cf_only = self.cpu.trace_cf_only;
        c1.trace_cap = self.cpu.trace_cap;
        c1.trace_from = self.cpu.trace_from;
        self.cpu1 = Some(c1);
    }

    pub fn set_console(&mut self, c: Console) {
        self.machine.console = c;
    }

    pub fn set_unimpl_policy(&mut self, p: UnimplPolicy) {
        self.cpu.on_unimpl = p;
    }

    pub fn run(&mut self, limits: &RunLimits) -> RunReport {
        let start = Stopwatch::start();
        let mut console = Vec::new();
        let mut wall_check = 0u64;
        // UART output is echoed to stderr as it happens, so a run can be
        // watched instead of waiting for the summary at the end. Set
        // `RVF_LIVE_CONSOLE=0` to get the buffered-only behaviour back (the
        // summary still prints the whole console either way, but it is not
        // repeated once it has been streamed).
        let diag = self.diag.clone().unwrap_or_else(default_diag);
        // `RVF_MMIO_FROM=<hex>` arms `--trace-mmio`-style logging only once the
        // PC first reaches that address — lets you capture a late boot stage
        // (e.g. start4.elf) without drowning in the bootloader's MMIO.
        if diag.mmio_from.is_some() {
            self.machine.mmio_trace = false;
        }
        // `RVF_TRACE_ON_CONSOLE=<substr>` arms the instruction trace the moment
        // that substring appears in the console — for pinning down a code path
        // by the log line that precedes it.
        let mut console_seen = 0usize;

        // Spin detection: over a sliding window of steps, track the min/max PC
        // and whether any console output happened. If the PC stays within a
        // small window for the whole window length with no output, call it a
        // spin (a peripheral poll our stubs never satisfy).
        let win = limits.idle_spin_limit.max(1);
        // Console-silence watchdog: the modelled clock and the retired count at
        // the last byte the firmware printed.
        let mut last_output_us = 0u64;
        let mut last_output_retired = 0u64;
        let mut w_lo = u32::MAX;
        let mut w_hi = 0u32;
        let mut w_steps = 0u64;
        let mut w_output = false;
        // "Progress" = RAM stores + peripheral stores + RAM loads. A memset or
        // memcpy advances the stores; a DRAM memtest read-back advances the
        // loads. A poll loop our stubs never satisfy touches none of them
        // (MMIO loads are not counted), so it still trips.
        let progress_count = |m: &Machine| {
            m.ram_writes
                .wrapping_add(m.mmio_writes)
                .wrapping_add(m.ram_reads)
        };
        let mut progress_at_window = progress_count(&self.machine);
        let mut clo_reads_at_window = self.machine.systimer.clo_reads;
        // Fast path: the exact same taken transfer repeating is a tight spin —
        // *unless* memory traffic keeps advancing.
        let mut last_cf = (u32::MAX, u32::MAX);
        let mut cf_repeat = 0u64;
        let mut progress_at_cf = progress_count(&self.machine);
        let mut clo_reads_at_cf = self.machine.systimer.clo_reads;
        // Busy-wait fast-forward: a repeating control-flow edge that only
        // advances the system-timer counter is a firmware `usleep`
        // (`while now - start < N`). Count the iterations and jump the timer
        // ahead so a multi-millisecond delay doesn't eat the step budget.
        let mut delay_ff = 0u64;
        let mut delay_ff_cf = (u32::MAX, u32::MAX);
        let mut progress_at_delay = progress_count(&self.machine);

        // `RVF_MBOX_KICK` used to live here: from the idle loop it called the
        // lock-release `0x3ED651E6` on a hard-coded object address whenever that
        // word showed a queued waiter, on the theory that an unmodelled
        // "0x7EE0 mailbox completer" owed the release. The object it was pointed
        // at (`0xBEF6D458`, the uncached alias of a stack frame) is a dmalib
        // transfer control block - `dma_transfer_init` (`0x3EC996CE`) zeroes it
        // and `dma_transfer_queue_post` (`0x3EC99898`) marks it 1 - and the
        // release is the firmware's own `dma_chan_interrupt`, reached from
        // `dma_interrupt` (`0x3EC980E8`) on the channel's completion interrupt
        // (source `0x50 + channel`). That interrupt is modelled now, in
        // `Machine::run_dma_legacy`, so the firmware retires its own transfers:
        // `RVF_WATCH=0xbef6d458` shows the word cycling 0 -> 1 -> 0 through
        // `0x3ED651C8` / `0x3ED65208` for the whole boot and never parking on a
        // waiter pointer. The shim has been removed rather than left armed.

        let mut tick_deliveries: u64 = 0;
        let mut tick_skips: u64 = 0;

        // RVF_PROF=1: cheap PC profiler. Bucket the core-0 PC into 256-byte
        // slots on every step and dump the hottest on exit — finds the loop
        // that is eating the step budget when a boot phase runs slow.
        //
        // `BTreeMap`, not `HashMap`: `alloc` has no `HashMap`, and these three
        // maps are only touched when the switch that owns them is set, so the
        // lookup being a tree walk costs a normal run nothing.
        let mut prof_hist: alloc::collections::BTreeMap<u32, u64> =
            alloc::collections::BTreeMap::new();
        // RVF_PROF_THREAD=1: same buckets, but keyed by the running ThreadX
        // thread (`_tx_thread_current_ptr`, `0x3EE35900`) as well, so "which
        // thread is spinning, and where" can be read off directly.
        let mut prof_thist: alloc::collections::BTreeMap<(u32, u32), u64> =
            alloc::collections::BTreeMap::new();

        // RVF_DBG_MAINSUS: catch the boot thread (0x3EF248C4) suspending — dump
        // the control-flow tail the one time it stops being the current thread
        // for good.

        // RVF_CZ_LOG=<n>: raise confzilla's own log level (byte at `0x3EE4ABD8`)
        // to <n> just before `gpioman_init` kicks off the schema walk
        // (`0x3ECC9DB0`, `bl 0x3EC89B38`), and dump the schema root descriptor
        // at `0x3EE19114` (its `.name` is patched to "pins_<variant>" at
        // runtime). confzilla is the FDT front end that is supposed to invoke
        // the `pin_config/pin` (`0x3ECC9DC4`) and `pin_defines/pin_define`
        // (`0x3ECC9EC4`) handlers which register the GPIO providers
        // (`[gp+807672/676/680]`); none of them fire, so gpioman reports
        // `error 1`. Its own diagnostics say why.

        // RVF_HEARTBEAT=<n>: every <n> million retired instructions, print model
        // time, the running ThreadX thread and the PC. The one diagnostic that
        // says whether a stalled boot is wedged or merely slow.
        let mut next_beat = self.cpu.retired + diag.heartbeat;

        // RVF_TRAP=<hex>[,<hex>...]: print pc / lr / r0-r5 every time core 0
        // reaches one of these addresses. Generic "who calls this, with what"
        // probe - the linear disassembler can't xref (it desyncs on inline
        // data), so callers have to be found at runtime.
        let mut trap_hits: alloc::collections::BTreeMap<u32, u64> =
            alloc::collections::BTreeMap::new();
        // `RVF_TRAP_MAX=<n>`: how many hits of each trap address to print
        // (default 12). The totals are always reported at exit.
        // RVF_TRAP_FROM=<n>: ignore trap hits before <n> million retired
        // instructions, so the steady state can be sampled instead of only
        // early boot.

        // RVF_DBG_EVGET: log every distinct (event-group, caller) pair passed to
        // `_tx_event_flags_get` (`0x3EC3E3BE`), so the groups the boot actually
        // blocks on can be told apart from the ones a shim must not touch.
        // Hoisted out of the per-instruction loop: an environment lookup is a
        // locking scan over the whole environment and these were being
        // evaluated on every step, which dominated run time.

        let mut core1_end: Option<RunEnd> = None;
        let end = loop {
            if limits.max_steps.is_some_and(|max| {
                self.cpu.retired + self.cpu1.as_ref().map_or(0, |c| c.retired) >= max
            }) {
                break RunEnd::StepLimit;
            }
            if let Some(pc) = limits.stop_pc {
                if self.cpu.pc() == pc {
                    break RunEnd::StopPc(pc);
                }
            }

            // The firmware's trampoline writes each core's exception-vector base
            // to CoreCtl (0x7E00_2030 / 0x38); pick it up so `swi` diag.traps into
            // the firmware's own handler table. An explicit `--exc-vbase` wins.
            if self.cpu.exc_vbase == 0 && self.machine.corectl.vbase[0] != 0 {
                self.cpu.exc_vbase = self.machine.corectl.vbase[0];
            }

            let pc_before = self.cpu.pc();

            if diag.heartbeat != 0 && self.cpu.retired >= next_beat {
                next_beat = self.cpu.retired + diag.heartbeat;
                diag_eprintln!(
                    "[beat] retired={} model_us={} pc={pc_before:#010x} in_exc={} irq_en={} tick_due={}",
                    self.cpu.retired,
                    self.machine.systimer.now_us(),
                    self.cpu.in_exception,
                    self.cpu.irq_enabled(),
                    self.machine.systimer.tick_pending(),
                );
            }
            if let Some(cur_ptr) = diag.prof_thread {
                let cur = self.machine.load(cur_ptr, Width::Word).unwrap_or(0);
                *prof_thist.entry((cur, pc_before & !0xFF)).or_insert(0) += 1;
            }
            if diag.prof {
                *prof_hist.entry(pc_before & !0xFF).or_insert(0) += 1;
            }

            // `_tx_thread_schedule`'s *solicited* context restore (`0x3EC40034`
            // → `bx r26` at `0x3EC4003E`) resumes a thread that yielded via a
            // ThreadX call — it does NOT `rti`, so the model's `in_exception`
            // depth (bumped on the faked timer IRQ, dropped by `Op::Rti`) would
            // stay stuck above 0 after the tick ISR preempts into such a thread.
            // Rebalance it here: reaching this point means we are back in thread
            // context.
            if pc_before == crate::firmware::addrs::SOLICITED_RESTORE_PC {
                self.cpu.in_exception = 0;
            }

            if let Some(from) = diag.mmio_from {
                if !self.machine.mmio_trace && pc_before == from {
                    self.machine.mmio_trace = true;
                }
            }
            // `0x3ED7BD2C` = start4's `udelay(r1)` primitive: `start = CLO;
            // while (CLO - start) < r1 {}`. The clkm clock bring-up calls it
            // hundreds of times inside tight outer loops — each call is only
            // ~50 spin iterations so the generic delay-ff detector (which needs
            // ~1000 identical edges) never trips, and they add up to the
            // model's single biggest time sink (~68% of instructions in the
            // post-config window). A pure busy-wait on the free-running counter
            // is instantaneous in emulation: jump the timer straight to the
            // deadline on entry so the loop exits on its first read. Capped so a
            // garbage argument can't race sim-time away.
            // `0x3ED7BD3A` is the loop body, reached once `r3 = start = CLO`
            // has been captured (`0x3ED7BD36`) and `r1 = us` still holds the
            // requested delay. Jump the counter a full `us` past `start` so the
            // very next `(CLO - start) < us` check (`0x3ED7BD40`) fails and the
            // loop exits. (Jumping on the `0x3ED7BD2C` entry instead is a no-op
            // — `start` is captured *after* it, so the delta stays 0.)
            if pc_before == 0x3ED7_BD3A {
                let us = (self.cpu.regs.get(1) as u64).min(5_000_000);
                if us != 0 {
                    self.machine.systimer.jump(us);
                }
            }
            // `RVF_TRACE_ON_PC=<hex>`: arm the instruction trace the first time
            // core 0 reaches this address. The console-substring trigger cannot
            // reach a code path that runs after the firmware has stopped
            // printing — which is exactly where a wedged boot has to be read.
            if let Some(pc) = diag.trace_on_pc {
                if !self.cpu.trace && pc_before == pc {
                    self.cpu.trace = true;
                    self.cpu.trace_armed = true;
                    self.cpu.trace_cf_only = diag.trace_cf;
                    self.cpu.trace_cap = diag.trace_cap;
                    if diag.trace_mmio {
                        self.machine.mmio_trace = true;
                    }
                }
            }
            if !diag.traps.is_empty()
                && self.cpu.retired >= diag.trap_from
                && diag.traps.contains(&pc_before)
            {
                let n = trap_hits.entry(pc_before).or_insert(0);
                *n += 1;
                if *n <= diag.trap_max {
                    diag_eprintln!(
                        "[trap] {pc_before:#010x} #{n} lr={:#010x} r0={:#x} r1={:#x} r2={:#x} r3={:#x} r4={:#x} r5={:#x} r6={:#x} r7={:#x} sp={:#x} retired={}",
                        self.cpu.regs.get(26),
                        self.cpu.regs.get(0),
                        self.cpu.regs.get(1),
                        self.cpu.regs.get(2),
                        self.cpu.regs.get(3),
                        self.cpu.regs.get(4),
                        self.cpu.regs.get(5),
                        self.cpu.regs.get(6),
                        self.cpu.regs.get(7),
                        self.cpu.regs.get(25),
                        self.cpu.retired
                    );
                }
            }
            self.machine.watch_pc = pc_before;
            let exc_depth_before = self.cpu.in_exception;
            let step = self.cpu.step(&mut self.machine);
            self.machine.tick(1);

            // start4's interrupt entry (`0x3ED18004`) bumps a nesting counter
            // (`[gp+4420]`) and indexes a per-nesting IRQ record by it; the
            // matching decrement lives in `_tx_thread_context_restore`, which
            // the model's `rti` shortcut skips. Left uncorrected the counter
            // grows without bound and the record index walks off into garbage
            // after a few interrupts. Undo one increment each time an `rti`
            // unwinds a faked interrupt.
            if self.cpu.in_exception < exc_depth_before {
                // Resolved against the live `gp`, not pinned: see
                // `firmware::addrs` for why an absolute here would version-lock
                // the model (#25).
                let nest = self
                    .cpu
                    .regs
                    .get(crate::vpu::reg::GP)
                    .wrapping_add(crate::firmware::addrs::IRQ_NEST_GP_OFFSET);
                if let Ok(n) = self.machine.load(nest, Width::Word) {
                    if n > 0 && n != 0xFFFF_FFFF {
                        let _ = self.machine.store(nest, Width::Word, n - 1);
                    }
                }
            }

            // The firmware raises an interrupt on a core in software by
            // setting its bit in that core's pending word (`0x7E002040` /
            // `+0x844`, `0x3ED01896`). start4 uses it for the clock service's
            // timer (source 66) and for ThreadX's inter-core reschedule IPI
            // (source 78 on core 0, 79 on core 1). Nothing modelled these, so
            // every software-posted interrupt was silently dropped.
            while let Some((core, src)) = self.machine.corectl.take_sw_raised() {
                if diag.dbg_swirq {
                    diag_eprintln!(
                        "[sw-irq] core {core} src {src} pc={:#x} retired={}",
                        self.cpu.pc(),
                        self.cpu.retired
                    );
                }
                if core == 0 {
                    // The generic dispatcher re-reads the source from
                    // CoreCtl `+0x04`, so present it there as well as
                    // queueing the vectoring.
                    self.machine.corectl.raise_source(src);
                    self.machine.push_pending_irq(src);
                } else if let Some(c1) = self.cpu1.as_mut() {
                    if c1.exc_vbase != 0 {
                        c1.vector_irq(&mut self.machine, src);
                    }
                }
            }

            // A device-raised interrupt (DMA completion) takes the same
            // vectoring path as the tick, but is not gated on a compare match.
            if self.cpu.in_exception == 0 && self.cpu.irq_enabled() && self.cpu.exc_vbase != 0 {
                if let Some(src) = self.machine.take_pending_irq() {
                    if diag.dbg_tick {
                        diag_eprintln!(
                            "[irq] src={src} pc={:#x} retired={}",
                            self.cpu.pc(),
                            self.cpu.retired
                        );
                    }
                    self.cpu.vector_irq(&mut self.machine, src);
                }
            }
            let tick_due = self.machine.systimer.tick_pending();
            if diag.dbg_tick
                && tick_due
                && self.cpu.exc_vbase != 0
                && (self.cpu.in_exception != 0 || !self.cpu.irq_enabled())
            {
                tick_skips += 1;
                if tick_skips <= 20 || tick_skips.is_multiple_of(100_000) {
                    diag_eprintln!(
                        "[tick-skip #{tick_skips}] in_exc={} irq_en={} pc={:#x} retired={}",
                        self.cpu.in_exception,
                        self.cpu.irq_enabled(),
                        self.cpu.pc(),
                        self.cpu.retired,
                    );
                }
            }
            if tick_due
                && self.cpu.in_exception == 0
                && self.cpu.irq_enabled()
                && self.cpu.exc_vbase != 0
            {
                if let Some(slot) = self.machine.timer_tick_slot() {
                    // Deliver now — consume the latched flag.
                    self.machine.systimer.take_tick_pending();
                    if diag.dbg_tick {
                        tick_deliveries += 1;
                        if tick_deliveries <= 30 || tick_deliveries.is_multiple_of(500) {
                            let vb = self.cpu.exc_vbase;
                            let h = self.machine.load(vb.wrapping_add(slot * 4), Width::Word);
                            diag_eprintln!(
                                "[tick] #{tick_deliveries} slot={slot} vbase={vb:#x} handler={h:x?} resume={:#x} retired={} nest={:#x}",
                                self.cpu.pc(),
                                self.cpu.retired,
                                {
                                    let nest = self
                                        .cpu
                                        .regs
                                        .get(crate::vpu::reg::GP)
                                        .wrapping_add(crate::firmware::addrs::IRQ_NEST_GP_OFFSET);
                                    self.machine.load(nest, Width::Word).unwrap_or(0xdead)
                                },
                            );
                        }
                    }
                    self.cpu.vector_irq(&mut self.machine, slot);
                }
            }

            if self.machine.mmio_trace && !self.machine.mmio_events.is_empty() {
                for (addr, w, val, write) in self.machine.mmio_events.drain(..) {
                    diag_eprintln!(
                        "mmio {:#010x}  {}{}  {:#010x} <- {:#0width$x}",
                        pc_before,
                        if write { "W" } else { "R" },
                        w,
                        addr,
                        val,
                        width = (w as usize) * 2 + 2,
                    );
                }
            }

            // Firmware asked for a SoC reset (PM RSTC) — stop so the caller can
            // re-run from a fresh machine seeded with the updated flash.
            if self.machine.pm.take_reset() {
                break RunEnd::Reset;
            }

            // Core 1 (re)enters at the shared start4 reset vector, not core 0's
            // `entry` — which on the EEPROM path is the *bootcode*, long gone by
            // the time start4 brings its sibling up.
            //
            // The CoreCtl run-state write the model keys on also overlaps the
            // interrupt-priority words, so it fires early (during driver
            // bring-up) — well before start4's ThreadX-SMP init registers the
            // per-core scheduler object that core 1 immediately dereferences.
            // Latch the intent, but defer the spawn until that object exists.
            if self.cpu1.is_none() {
                if self.machine.corectl.take_core1_release() {
                    self.core1_release_armed = true;
                }
                if self.core1_release_armed && self.smp_dispatch_ready() {
                    let entry = self.core1_entry.unwrap_or(START4_ENTRY);
                    self.spawn_core1(entry);
                }
            }
            // Interleave one core-1 step per core-0 step over the shared bus.
            if let Some(c1) = self.cpu1.as_mut() {
                if c1.exc_vbase == 0 && self.machine.corectl.vbase[1] != 0 {
                    c1.exc_vbase = self.machine.corectl.vbase[1];
                }
                if !c1.is_stopped() && !c1.halted {
                    if let crate::vpu::Step::Stopped = c1.step(&mut self.machine) {
                        core1_end = Some(RunEnd::Core1Halted(
                            c1.stopped.clone().expect("stop reason"),
                        ));
                    }
                }
            }

            let fresh = self.machine.take_console_output();
            let had_output = !fresh.is_empty();
            if had_output {
                last_output_us = self.machine.systimer.now_us();
                last_output_retired = self.cpu.retired;
            } else if limits.silent_us > 0 {
                // Wedged, not merely slow: the firmware has printed nothing for
                // a long stretch of *modelled* time and is still burning
                // instructions. Both halves matter — modelled time alone would
                // trip on a legitimate long delay that `sleep` fast-forwards
                // through in a handful of instructions, and instructions alone
                // would trip on a busy stretch that simply has nothing to say.
                let silent_us = self
                    .machine
                    .systimer
                    .now_us()
                    .saturating_sub(last_output_us);
                let silent_retired = self.cpu.retired.saturating_sub(last_output_retired);
                if silent_us >= limits.silent_us && silent_retired >= 20_000_000 {
                    break RunEnd::Stuck {
                        pc: Hex(self.cpu.pc()),
                        silent_us,
                        retired: silent_retired,
                    };
                }
            }
            if diag.live_console && had_output {
                crate::diag::emit_console(&fresh);
            }
            console.extend_from_slice(&fresh);
            if let Some(needle) = &diag.trace_on_console {
                if !self.cpu.trace && console.len() > console_seen {
                    let from = console_seen.saturating_sub(needle.len());
                    if String::from_utf8_lossy(&console[from..]).contains(needle.as_str()) {
                        self.cpu.trace = true;
                        self.cpu.trace_armed = true;
                        self.cpu.trace_cf_only = diag.trace_cf;
                        self.cpu.trace_cap = diag.trace_cap;
                        // Also stream peripheral accesses while the trace is
                        // armed (RVF_TRACE_MMIO=1) — handy for pinning down an
                        // unmodelled block like the I2C BSC.
                        if diag.trace_mmio {
                            self.machine.mmio_trace = true;
                        }
                    }
                    console_seen = console.len();
                }
            }

            match step {
                crate::vpu::Step::Stopped => {
                    let stop = self.cpu.stopped.clone().expect("stopped without reason");
                    break RunEnd::Halted(stop);
                }
                crate::vpu::Step::Ran => {}
            }
            // Core 1 halting does not stop core 0 — record it and carry on.

            if limits.idle_spin_limit > 0 {
                // A loop that keeps reading the free-running system timer is a
                // firmware `usleep` — time-bounded, so not a hung spin however
                // many iterations it takes. `max_steps` / `max_wall` still cap
                // a pathological one.
                let timer_polling = self.machine.systimer.clo_reads != clo_reads_at_cf;
                // Firmware busy-wait on the free-running counter: same edge,
                // timer advancing, nothing else changing, no output. Let it
                // build up, then skip the counter forward a slice at a time.
                if let Some(cf) = self.cpu.cf_last {
                    let p = progress_count(&self.machine);
                    // Firmware `udelay` (e.g. `0x3ED7BD2C`: `while (CLO - start)
                    // < n`) spins the same 2-instruction edge thousands of times
                    // per call and the clock bring-up does hundreds of them —
                    // the model's biggest time sink. Recognise it: the same
                    // taken edge, the counter advancing, no console output. The
                    // periodic ThreadX tick ISR fires in the middle of a long
                    // delay and does a *bounded* amount of RAM traffic, so
                    // tolerate a small `progress` delta (a real memcpy/memtest
                    // in the loop would blow past it) rather than resetting.
                    let prog_delta = p.wrapping_sub(progress_at_delay);
                    if cf == delay_ff_cf && timer_polling && !had_output && prog_delta < 4_096 {
                        delay_ff += 1;
                        progress_at_delay = p;
                        if delay_ff >= 1_000 {
                            // Jump (not `skip_ahead`) so one long `udelay` clears
                            // in a few detections instead of being chopped at
                            // every tick deadline; `service_matches` collapses
                            // any ticks the jump skips to a single delivery.
                            self.machine.systimer.jump(50_000);
                            delay_ff = 0;
                        }
                    } else {
                        delay_ff = 0;
                        delay_ff_cf = cf;
                        progress_at_delay = p;
                    }
                }
                if let Some(cf) = self.cpu.cf_last {
                    // A bare read-only poll counts as a spin, but firmware
                    // delay/lock loops legitimately iterate 10k+ times before
                    // giving up, so the threshold is generous.
                    let progress = progress_count(&self.machine);
                    let progressing = progress != progress_at_cf || timer_polling;
                    if cf == last_cf && !had_output && !progressing {
                        cf_repeat += 1;
                        if cf_repeat >= 200_000 {
                            break RunEnd::IdleSpin(cf.0);
                        }
                    } else {
                        cf_repeat = 0;
                        last_cf = cf;
                        progress_at_cf = progress;
                    }
                }
                clo_reads_at_cf = self.machine.systimer.clo_reads;

                w_lo = w_lo.min(pc_before);
                w_hi = w_hi.max(pc_before);
                w_output |= had_output;
                w_steps += 1;
                if w_steps >= win {
                    let clo_delta = self
                        .machine
                        .systimer
                        .clo_reads
                        .wrapping_sub(clo_reads_at_window);
                    let stalled = progress_count(&self.machine) == progress_at_window
                        && self.machine.systimer.clo_reads == clo_reads_at_window;
                    if !w_output && stalled && w_hi.wrapping_sub(w_lo) <= 4096 {
                        break RunEnd::IdleSpin(w_lo);
                    }
                    // A window that spent >1/8 of its instructions reading the
                    // free-running counter, with no console output, is
                    // dominated by a firmware `usleep(n)`. The plain delay-ff
                    // path above stalls on it because the periodic tick ISR
                    // keeps bumping `progress` and briefly widening the PC
                    // range. Jump the counter forward so a multi-100 ms
                    // rail-settle delay (PMIC bring-up does several) doesn't run
                    // in real time.
                    // A tight PC window (a single small loop) that spent the
                    // whole window polling the free-running counter, no console
                    // output, is a firmware `usleep(n)` — jump the counter
                    // forward so it doesn't run in real time. A wider window
                    // (loop punctuated by a tick ISR) needs a firmer CLO-read
                    // ratio to be sure it isn't doing real work.
                    let tight = w_hi.wrapping_sub(w_lo) <= 0x40;
                    let ff = !w_output
                        && if tight {
                            clo_delta > win / 20
                        } else {
                            clo_delta > win / 8 && self.cpu.in_exception == 0
                        };
                    if ff {
                        self.machine.systimer.jump(200_000);
                    }
                    if diag.dbg_ff {
                        diag_eprintln!(
                            "[ff] win close: clo_delta={clo_delta} w=[{w_lo:#x}..{w_hi:#x}] out={w_output} exc={} ff={ff} @{}",
                            self.cpu.in_exception, self.cpu.retired
                        );
                    }
                    clo_reads_at_window = self.machine.systimer.clo_reads;
                    w_lo = u32::MAX;
                    w_hi = 0;
                    w_steps = 0;
                    w_output = false;
                    progress_at_window = progress_count(&self.machine);
                }
            }

            wall_check += 1;
            if let Some(max) = limits.max_wall {
                if wall_check.is_multiple_of(65_536) && start.elapsed() >= max {
                    break RunEnd::TimeLimit;
                }
            }
        };

        console.extend_from_slice(&self.machine.take_console_output());

        // Only the first 12 hits of each `RVF_TRAP` address are printed, so
        // report the totals as well - the print cap otherwise makes every
        // busy address look like it ran exactly 12 times.
        if !trap_hits.is_empty() {
            let mut totals: Vec<(u32, u64)> = trap_hits.into_iter().collect();
            totals.sort_unstable();
            for (pc, n) in totals {
                diag_eprintln!("[trap-total] {pc:#010x} {n}");
            }
        }

        // `RVF_DBG_TCB=<hex>[,<hex>...]`: at exit, decode each ThreadX thread's
        // saved context and report the pc it is parked at. `[tcb+8]` is the
        // saved stack pointer and the word at it is the frame discriminator
        // (`_tx_thread_schedule`, `0x3EC4002C`): 1 = an interrupt frame
        // `[1][r16-r23][r0-r15][lr][SR][PC]`, 0 = a solicited frame
        // `[0][r16-r23][r6-r15][lr]` whose `lr` is the resume address. That pc
        // is the answer to "what is this thread blocked on".
        // `RVF_DBG_IRQTBL=1`: dump the per-source handler table at `gp+58004`
        // at exit. The generic dispatcher (`0x3EC3E9BC`) indexes it with the
        // source number to find the ISR, so a zero entry means "this source is
        // never handled" even if `enable_irq_source` turned it on.
        if diag.dbg_irqtbl {
            let tbl = self.cpu.regs.get(24).wrapping_add(58004);
            let vb = self.cpu.exc_vbase;
            diag_eprintln!(
                "[irqtbl] gp={:#x} table={tbl:#x} vbase={vb:#x}",
                self.cpu.regs.get(24)
            );
            // Two dispatch routes exist. The vector table's [64..127] entries are
            // direct per-source handlers (source 64 = the ThreadX tick
            // `0x3EC40B7C`); everything else points at the generic dispatcher
            // `0x3EC3E9BC`, which indexes the handler table by source. A source
            // whose *handler-table* slot is 0 is not broken — `0x3ED656A8`
            // refuses to register on such a slot — it is dispatched directly.
            for src in 64u32..128 {
                let h = self
                    .machine
                    .load(tbl.wrapping_add(src * 4), Width::Word)
                    .unwrap_or(0);
                let v = self
                    .machine
                    .load(vb.wrapping_add(src * 4), Width::Word)
                    .unwrap_or(0);
                if h != 0 || v != 0 {
                    let direct = if v != 0 && v & !1 != 0x3EC3_E9BC {
                        "  <- direct vector"
                    } else {
                        ""
                    };
                    diag_eprintln!("[irqtbl]   src {src} handler={h:#x} vector={v:#x}{direct}");
                }
            }
        }

        for tcb in crate::diag::hex_list("RVF_DBG_TCB") {
            let mut ld = |a: u32| self.machine.load(a, Width::Word).unwrap_or(0xdead_dead);
            let sp = ld(tcb.wrapping_add(8));
            let disc = ld(sp);
            let (kind, resume) = if disc == 1 {
                ("irq", ld(sp.wrapping_add(4 * (1 + 8 + 16 + 1 + 1))))
            } else {
                ("solicited", ld(sp.wrapping_add(4 * (1 + 8 + 10))))
            };
            diag_eprintln!(
                    "[tcb] {tcb:#x} id={:#x} run_count={} sp={sp:#x} disc={disc} {kind} resume={resume:#x}",
                    ld(tcb),
                    ld(tcb.wrapping_add(4)),
                );
            // Everything above the saved frame is the suspended function's
            // own stack; scan it for words that look like start4 text and
            // print them as a rough backtrace. `resume` alone is always the
            // return out of `_tx_thread_system_suspend`, which says nothing
            // about *what* the thread is waiting for.
            let frame = 4
                * (if disc == 1 {
                    1 + 8 + 16 + 1 + 1 + 1
                } else {
                    1 + 8 + 10 + 1
                });
            let mut shown = 0;
            for i in 0..192u32 {
                let a = sp.wrapping_add(frame + 4 * i);
                let v = ld(a);
                if (0x3EC0_0000..0x3EE0_0000).contains(&v) && v & 1 == 0 {
                    diag_eprintln!("[tcb]     {a:#x}: {v:#010x}");
                    shown += 1;
                    if shown == 16 {
                        break;
                    }
                }
            }
        }

        if diag.prof {
            let mut v: Vec<_> = prof_hist.iter().map(|(&k, &n)| (k, n)).collect();
            v.sort_by_key(|a| core::cmp::Reverse(a.1));
            let total: u64 = v.iter().map(|(_, n)| n).sum();
            diag_eprintln!("--- RVF_PROF: core-0 PC buckets (total {total}) ---");
            for (pc, n) in v.iter().take(25) {
                diag_eprintln!(
                    "  {pc:#010x}  {n:>14}  {:5.1}%",
                    100.0 * *n as f64 / total as f64
                );
            }
        }
        if diag.prof_thread.is_some() {
            let total: u64 = prof_thist.values().sum();
            let mut by_thread: alloc::collections::BTreeMap<u32, u64> =
                alloc::collections::BTreeMap::new();
            for (&(t, _), &n) in prof_thist.iter() {
                *by_thread.entry(t).or_insert(0) += n;
            }
            let mut threads: Vec<_> = by_thread.into_iter().collect();
            threads.sort_by_key(|a| core::cmp::Reverse(a.1));
            diag_eprintln!(
                "--- RVF_PROF_THREAD: core-0 time by ThreadX thread (total {total}) ---"
            );
            for (t, n) in threads.iter().take(8) {
                diag_eprintln!(
                    "  thread {t:#010x}  {n:>14}  {:5.1}%",
                    100.0 * *n as f64 / total as f64
                );
                let mut buckets: Vec<_> = prof_thist
                    .iter()
                    .filter(|((tt, _), _)| tt == t)
                    .map(|((_, pc), &c)| (*pc, c))
                    .collect();
                buckets.sort_by_key(|a| core::cmp::Reverse(a.1));
                for (pc, c) in buckets.iter().take(6) {
                    diag_eprintln!(
                        "      {pc:#010x}  {c:>14}  {:5.1}%",
                        100.0 * *c as f64 / *n as f64
                    );
                }
            }
        }

        let mut unimpl = self.cpu.unimpl.clone();
        unimpl.sort_by(|a, b| b.count.cmp(&a.count).then(a.pc.cmp(&b.pc)));

        RunReport {
            end,
            retired: self.cpu.retired,
            cycles: self.cpu.cycles,
            skipped: self.cpu.skipped,
            stub_hits: self.machine.stub_hits,
            bus_errors: self.machine.bus_errors,
            wall: start.elapsed(),
            pc: self.cpu.pc(),
            console,
            console_streamed: diag.live_console,
            unimpl,
            regs: core::array::from_fn(|i| self.cpu.regs.get(i)),
            core1_pc: self.cpu1.as_ref().map(|c| c.pc()),
            core1_retired: self.cpu1.as_ref().map(|c| c.retired),
            core1_end,
            phase_tags: self.machine.phase_tags.clone(),
            core1_release_never_resolved: self.core1_release_armed && self.cpu1.is_none(),
        }
    }
}
