//! Top-level emulator: owns the [`Vpu`] and the [`Machine`] as siblings and
//! drives the run loop.

use std::time::{Duration, Instant};

use crate::bus::{Bus, Width};
use crate::machine::{Console, Machine};
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

impl std::fmt::Debug for Hex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

impl Emulator {
    pub fn new(machine: Machine, entry: u32) -> Emulator {
        Emulator {
            cpu: Vpu::new(entry),
            cpu1: None,
            core1_entry: None,
            core1_release_armed: false,
            machine,
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
        let start = Instant::now();
        let mut console = Vec::new();
        let mut wall_check = 0u64;
        // UART output is echoed to stderr as it happens, so a run can be
        // watched instead of waiting for the summary at the end. Set
        // `RVF_LIVE_CONSOLE=0` to get the buffered-only behaviour back (the
        // summary still prints the whole console either way, but it is not
        // repeated once it has been streamed).
        let diag = crate::diag::DiagConfig::from_env();
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
        let mut irqtbl_n = 0u32;
        let mut tick_skips: u64 = 0;

        // RVF_PROF=1: cheap PC profiler. Bucket the core-0 PC into 256-byte
        // slots on every step and dump the hottest on exit — finds the loop
        // that is eating the step budget when a boot phase runs slow.
        let mut prof_hist: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
        // RVF_PROF_THREAD=1: same buckets, but keyed by the running ThreadX
        // thread (`_tx_thread_current_ptr`, `0x3EE35900`) as well, so "which
        // thread is spinning, and where" can be read off directly.
        let mut prof_thist: std::collections::HashMap<(u32, u32), u64> =
            std::collections::HashMap::new();

        // RVF_DBG_MAINSUS: catch the boot thread (0x3EF248C4) suspending — dump
        // the control-flow tail the one time it stops being the current thread
        // for good.
        let mut mainsus_done = false;
        let mut main_was_cur = false;

        // RVF_CZ_LOG=<n>: raise confzilla's own log level (byte at `0x3EE4ABD8`)
        // to <n> just before `gpioman_init` kicks off the schema walk
        // (`0x3ECC9DB0`, `bl 0x3EC89B38`), and dump the schema root descriptor
        // at `0x3EE19114` (its `.name` is patched to "pins_<variant>" at
        // runtime). confzilla is the FDT front end that is supposed to invoke
        // the `pin_config/pin` (`0x3ECC9DC4`) and `pin_defines/pin_define`
        // (`0x3ECC9EC4`) handlers which register the GPIO providers
        // (`[gp+807672/676/680]`); none of them fire, so gpioman reports
        // `error 1`. Its own diagnostics say why.
        let mut cz_probe_n = 0u32;
        let mut cz_match_n = 0u32;
        let mut cz_pool_n = 0u32;

        // RVF_HEARTBEAT=<n>: every <n> million retired instructions, print model
        // time, the running ThreadX thread and the PC. The one diagnostic that
        // says whether a stalled boot is wedged or merely slow.
        let mut next_beat = self.cpu.retired + diag.heartbeat;

        // RVF_TRAP=<hex>[,<hex>...]: print pc / lr / r0-r5 every time core 0
        // reaches one of these addresses. Generic "who calls this, with what"
        // probe - the linear disassembler can't xref (it desyncs on inline
        // data), so callers have to be found at runtime.
        let mut trap_hits: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
        // `RVF_TRAP_MAX=<n>`: how many hits of each trap address to print
        // (default 12). The totals are always reported at exit.
        // RVF_TRAP_FROM=<n>: ignore trap hits before <n> million retired
        // instructions, so the steady state can be sampled instead of only
        // early boot.

        // RVF_DBG_EVGET: log every distinct (event-group, caller) pair passed to
        // `_tx_event_flags_get` (`0x3EC3E3BE`), so the groups the boot actually
        // blocks on can be told apart from the ones a shim must not touch.
        // Hoisted out of the per-instruction loop: `std::env::var_os` is a
        // locking lookup over the whole environment and these were being
        // evaluated on every step, which dominated run time.
        let mut evget_seen: std::collections::HashSet<(u32, u32)> =
            std::collections::HashSet::new();

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
                let cur = self.machine.load(0x3EE3_5900, Width::Word).unwrap_or(0);
                let exec = self.machine.load(0x3EE3_5904, Width::Word).unwrap_or(0);
                eprintln!(
                    "[beat] retired={} model_us={} pc={pc_before:#010x} cur={cur:#010x} exec={exec:#010x} in_exc={} irq_en={} tick_due={}",
                    self.cpu.retired,
                    self.machine.systimer.now_us(),
                    self.cpu.in_exception,
                    self.cpu.irq_enabled(),
                    self.machine.systimer.tick_pending(),
                );
            }
            if diag.prof_thread {
                let cur = self.machine.load(0x3EE3_5900, Width::Word).unwrap_or(0);
                *prof_thist.entry((cur, pc_before & !0xFF)).or_insert(0) += 1;
            }
            if diag.prof {
                *prof_hist.entry(pc_before & !0xFF).or_insert(0) += 1;
            }

            // RVF_DBG_RESUME: log every _tx_thread_system_resume (0x3EC402D2)
            // and _tx_thread_system_suspend (0x3EC40516) — who resumes/suspends
            // which thread, to find what would wake the boot thread.
            if diag.dbg_resume
                && matches!(pc_before, 0x3EC4_02D2 | 0x3EC4_0516)
                && self.cpu.retired > 90_000_000
            {
                let kind = if pc_before == 0x3EC4_02D2 {
                    "resume"
                } else {
                    "suspend"
                };
                eprintln!(
                    "[{kind}] thread={:#x} lr={:#x} @retired={}",
                    self.cpu.regs.get(0),
                    self.cpu.regs.get(26),
                    self.cpu.retired
                );
            }
            if diag.dbg_evset
                && pc_before == 0x3EC3_E1BA
                && (self.cpu.regs.get(0) == 0x3EF0_5FEC
                    || self
                        .machine
                        .load(self.cpu.regs.get(0).wrapping_add(112), Width::Word)
                        .unwrap_or(0)
                        == 0x3EF0_5FEC)
            {
                let cf = &self.cpu.cf_trace;
                eprintln!(
                    "[evset] flags={:#x} lr={:#x} @retired={} cf-tail:",
                    self.cpu.regs.get(1),
                    self.cpu.regs.get(26),
                    self.cpu.retired
                );
                for &(from, to) in cf.iter().skip(cf.len().saturating_sub(24)) {
                    eprintln!("[evset]   {from:#010x} -> {to:#010x}");
                }
            }

            if diag.dbg_mainsus {
                let cur = self.machine.load(0x3EE3_5900, Width::Word).unwrap_or(0);
                if cur == 0x3EF2_48C4 {
                    main_was_cur = true;
                } else if main_was_cur {
                    main_was_cur = false;
                    if !mainsus_done {
                        let exec = self.machine.load(0x3EE3_5904, Width::Word).unwrap_or(0);
                        eprintln!(
                            "[mainsus] main switched out @ retired={} pc={pc_before:#x} exec={exec:#x}",
                            self.cpu.retired
                        );
                        let cf = &self.cpu.cf_trace;
                        for &(from, to) in cf.iter().skip(cf.len().saturating_sub(40)) {
                            eprintln!("[mainsus]   {from:#010x} -> {to:#010x}");
                        }
                        // stop after the switch-out that lands past ~140M
                        if self.cpu.retired > 140_000_000 {
                            mainsus_done = true;
                        }
                    }
                }
            }

            // `0x3EDA28D6` is start4's optimised `memcpy(r0=dst, r1=src,
            // r2=len)` — a leaf that returns via `bx r26`. Its bulk copy is a
            // VC4 vector loop (`0x3EDA28F2`+, class Vector80/48) the model's
            // scalar decoder can't execute, so it was silently skipping ~all of
            // every copy (only the ≤3-byte scalar tail ran) and corrupting
            // whatever it moved. Emulate the whole function here — pure memory
            // effect, no globals touched. `dst < src` in practice (overlapping
            // shift-down), which a forward byte copy handles correctly.
            if pc_before == 0x3EDA_28D6 {
                let dst = self.cpu.regs.get(0);
                let src = self.cpu.regs.get(1);
                let len = self.cpu.regs.get(2);
                for i in 0..len {
                    let b = self
                        .machine
                        .load(src.wrapping_add(i), Width::Byte)
                        .unwrap_or(0);
                    let _ = self.machine.store(dst.wrapping_add(i), Width::Byte, b);
                }
                self.cpu.regs.pc = self.cpu.regs.get(26);
            }

            // `0x3EDA2A00` is start4's `memmove(r0=dst, r1=src, r2=len)`, the
            // companion to the `memcpy` above and likewise a leaf returning via
            // `r26`. `if r0 > r1` it copies backwards in-place; otherwise it
            // tail-branches to the forward copier `0x3EDA28D6` at `0x3EDA2A04`.
            //
            // The backward path has three VC4 vector fast paths (32-byte
            // aligned, 16-byte aligned, and a 16-byte bulk chunk in the middle
            // of the byte-wise case) which the scalar decoder cannot execute, so
            // the model was running only the byte-wise edges and losing the
            // middle of every backward move. The relocatable heap moves blocks
            // through here, so a grown block came out with a hole in it.
            //
            // Emulate the whole function; overlap-correct in both directions.
            if pc_before == 0x3EDA_2A00 {
                let dst = self.cpu.regs.get(0);
                let src = self.cpu.regs.get(1);
                let len = self.cpu.regs.get(2);
                if dst > src {
                    for i in (0..len).rev() {
                        let b = self
                            .machine
                            .load(src.wrapping_add(i), Width::Byte)
                            .unwrap_or(0);
                        let _ = self.machine.store(dst.wrapping_add(i), Width::Byte, b);
                    }
                } else {
                    for i in 0..len {
                        let b = self
                            .machine
                            .load(src.wrapping_add(i), Width::Byte)
                            .unwrap_or(0);
                        let _ = self.machine.store(dst.wrapping_add(i), Width::Byte, b);
                    }
                }
                self.cpu.regs.pc = self.cpu.regs.get(26);
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
            if let Some(lvl) = diag.cz_log {
                // `cp_front_fdt_buffer` (`0x3EC89670`): `r10` = the FDT
                // buffer it was handed, `r0` = the byte-swapped magic it
                // just read from `[r10]`, which must be 0xD00DFEED. This
                // says directly whether the blob reached confzilla.
                // `FUN_0ecb6fd4(state, root_node, fields, ...)` - the
                // schema-tree matcher. It hashes `fields[0]` (FNV-1a) and
                // walks the sibling list from `root_node` comparing
                // `node[0x15]` (hash) then `strncmp(node->name, .., 31)`.
                // Dump what it is actually matching against.
                if pc_before == 0x3ECB_6FD4 && cz_match_n < 4 {
                    cz_match_n += 1;
                    let root = self.cpu.regs.get(1);
                    let fields = self.cpu.regs.get(2);
                    let rdstr = |m: &mut crate::machine::Machine, p: u32| {
                        let mut t = String::new();
                        for i in 0..32 {
                            match m.load(p + i, Width::Byte) {
                                Ok(0) | Err(_) => break,
                                Ok(c) => t.push(c as u8 as char),
                            }
                        }
                        t
                    };
                    let f0 = self.machine.load(fields, Width::Word).unwrap_or(0);
                    let want = rdstr(&mut self.machine, f0);
                    let mut chain = Vec::new();
                    let mut n = root;
                    for _ in 0..8 {
                        if n == 0 {
                            break;
                        }
                        let namep = self.machine.load(n, Width::Word).unwrap_or(0);
                        chain.push(format!(
                            "{n:#x}:{:?}/h={:#x}",
                            rdstr(&mut self.machine, namep),
                            self.machine.load(n + 0x54, Width::Word).unwrap_or(0)
                        ));
                        n = self.machine.load(n + 0x30, Width::Word).unwrap_or(0);
                    }
                    let mut words = Vec::new();
                    for i in 0..8 {
                        words.push(format!(
                            "{:08x}",
                            self.machine.load(root + i * 4, Width::Word).unwrap_or(0)
                        ));
                    }
                    let st = self.cpu.regs.get(0);
                    let mut sw = Vec::new();
                    for i in 0..13 {
                        sw.push(format!(
                            "{:08x}",
                            self.machine.load(st + i * 4, Width::Word).unwrap_or(0)
                        ));
                    }
                    // First three nodes of the pool (`state[1]`, stride 0x78).
                    let pool = self.machine.load(st + 4, Width::Word).unwrap_or(0);
                    let mut pn = Vec::new();
                    for i in 0..3u32 {
                        let nd = pool + i * 0x78;
                        let np = self.machine.load(nd, Width::Word).unwrap_or(0);
                        pn.push(format!("{nd:#x}:{:?}", rdstr(&mut self.machine, np)));
                    }
                    eprintln!(
                        "[cz] match want={want:?} root={root:#x} node[0..8]={} state={st:#x}[{}] pool={pool:#x} nodes=[{}]",
                        words.join(" "),
                        sw.join(" "),
                        pn.join(", ")
                    );
                }
                // The confzilla node pool is a relocatable-heap block.
                // `FUN_0ed5a494` unlocks it, `mem_resize_ex` (`0x3ED1FB7C`)
                // grows it, `FUN_0ed5a420` re-locks and re-bases every
                // node's internal pointers by (new_base - old_base). Trace
                // the base and node 0's `type` word across all three so it
                // is obvious where the contents are lost.
                if matches!(pc_before, 0x3ED5_A420 | 0x3ED5_A494 | 0x3ED1_FB7C) && cz_pool_n < 30 {
                    cz_pool_n += 1;
                    let (tag, st) = match pc_before {
                        0x3ED5_A420 => ("lock  ", self.cpu.regs.get(0)),
                        0x3ED5_A494 => ("unlock", self.cpu.regs.get(0)),
                        _ => ("resize", 0),
                    };
                    if st != 0 {
                        let base = self.machine.load(st + 4, Width::Word).unwrap_or(0);
                        eprintln!(
                            "[cz] {tag} state={st:#x} base={base:#x} oldbase={:#x} cap={} n0type={:#x} n0name={:#x}",
                            self.machine.load(st + 8, Width::Word).unwrap_or(0),
                            self.machine.load(st + 20, Width::Word).unwrap_or(0),
                            self.machine.load(base + 4, Width::Word).unwrap_or(0),
                            self.machine.load(base, Width::Word).unwrap_or(0),
                        );
                    } else {
                        eprintln!(
                            "[cz] {tag} handle={:#x} newsize={:#x} lr={:#x}",
                            self.cpu.regs.get(0),
                            self.cpu.regs.get(1),
                            self.cpu.regs.get(26)
                        );
                    }
                }
                // `FUN_0ed5a122(state, descriptor)` - the schema tree
                // builder. Dump the 44-byte source descriptor it is about
                // to copy, and `FUN_0ec89b38`'s stack template before it.
                if matches!(pc_before, 0x3ED5_A122 | 0x3EC8_9B38) && cz_probe_n < 14 {
                    cz_probe_n += 1;
                    let d = if pc_before == 0x3ED5_A122 {
                        self.cpu.regs.get(1)
                    } else {
                        self.cpu.regs.get(0)
                    };
                    let mut w = Vec::new();
                    for i in 0..11 {
                        w.push(format!(
                            "{:08x}",
                            self.machine.load(d + i * 4, Width::Word).unwrap_or(0)
                        ));
                    }
                    let namep = self.machine.load(d, Width::Word).unwrap_or(0);
                    let mut nm = String::new();
                    for i in 0..24 {
                        match self.machine.load(namep + i, Width::Byte) {
                            Ok(0) | Err(_) => break,
                            Ok(c) => nm.push(c as u8 as char),
                        }
                    }
                    // For the builder, r0 = the CP_STATE: show the node
                    // pool base and capacity, so a pool that moves under a
                    // relocatable-heap resize is visible.
                    let st = self.cpu.regs.get(0);
                    eprintln!(
                        "[cz] {} desc={d:#x} name={nm:?} pool={:#x} cap={} free={} [{}]",
                        if pc_before == 0x3ED5_A122 {
                            "build"
                        } else {
                            "b38  "
                        },
                        self.machine.load(st + 4, Width::Word).unwrap_or(0),
                        self.machine.load(st + 20, Width::Word).unwrap_or(0),
                        self.machine.load(st + 16, Width::Word).unwrap_or(0),
                        w.join(" ")
                    );
                }
                if pc_before == 0x3EC8_9684 {
                    // Raise both levels *here*, at `cp_front_fdt_buffer`
                    // entry: confzilla's own init stamps them back to 3
                    // after `gpioman_init` runs, so setting them earlier is
                    // undone. Level 4 turns on `cp_set_property: field not
                    // found` (the interesting one) without the per-FDT-token
                    // `cp parse_fdt_node ...` spam that level 5 adds.
                    let _ = self.machine.store(0x3EE4_ABD8, Width::Byte, lvl);
                    let _ = self.machine.store(0x3EE4_ABF0, Width::Byte, lvl);
                    let buf = self.cpu.regs.get(10);
                    let be32 = |m: &mut crate::machine::Machine, a: u32| -> u32 {
                        m.load(a, Width::Word).unwrap_or(0).swap_bytes()
                    };
                    let total = be32(&mut self.machine, buf + 4);
                    // Does the blob confzilla was handed actually contain
                    // the node the schema root names?
                    let needle = b"pins_4b";
                    let mut found = None;
                    let mut win = [0u8; 8];
                    let n = total.min(1 << 20);
                    for i in 0..n {
                        let c = self.machine.load(buf + i, Width::Byte).unwrap_or(0) as u8;
                        win.rotate_left(1);
                        win[7] = c;
                        if &win[1..8] == needle {
                            found = Some(buf + i - 6);
                            break;
                        }
                    }
                    eprintln!(
                        "[cz] fdt_buffer buf={buf:#010x} magic={:#010x} totalsize={total} pins_4b={found:#x?} be_lvl={} fe_lvl={} retired={}",
                        self.cpu.regs.get(0),
                        self.machine.load(0x3EE4_ABD8, Width::Byte).unwrap_or(0xff),
                        self.machine.load(0x3EE4_ABF0, Width::Byte).unwrap_or(0xff),
                        self.cpu.retired
                    );
                }
                if pc_before == 0x3ECC_9DB0 {
                    // confzilla has two log-level bytes: the back end
                    // (`cp_register_property_list` / `cp_set_property` /
                    // `cp_done`) uses `gp+294584` = `0x3EE4ABD8`, the FDT
                    // front end (`cp_front_fdt_buffer`) `gp+294608` =
                    // `0x3EE4ABF0`. Raise both.
                    let _ = self.machine.store(0x3EE4_ABD8, Width::Byte, lvl);
                    let _ = self.machine.store(0x3EE4_ABF0, Width::Byte, lvl);
                    let root = 0x3EE1_9114u32;
                    let namep = self.machine.load(root, Width::Word).unwrap_or(0);
                    let mut name = String::new();
                    for i in 0..40 {
                        match self.machine.load(namep + i, Width::Byte) {
                            Ok(0) | Err(_) => break,
                            Ok(c) => name.push(c as u8 as char),
                        }
                    }
                    eprintln!(
                        "[cz] schema root {root:#x} name={namep:#x} \"{name}\" type={:#x} children={:#x} arg={:#x}",
                        self.machine.load(root + 4, Width::Word).unwrap_or(0),
                        self.machine.load(root + 8, Width::Word).unwrap_or(0),
                        self.cpu.regs.get(0),
                    );
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
                    eprintln!(
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
            if pc_before == 0x3EC3_E3BE && diag.dbg_evget {
                let grp = self.cpu.regs.get(0);
                let lr = self.cpu.regs.get(26);
                if evget_seen.insert((grp, lr)) {
                    let magic = self.machine.load(grp, Width::Word).unwrap_or(0);
                    eprintln!(
                        "[evget] group={grp:#010x} req={:#x} magic={magic:#010x} lr={lr:#010x}",
                        self.cpu.regs.get(1)
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

            // Periodic timer interrupt. Real VC4 hardware raises an interrupt
            // when a system-timer compare matches the free-running counter and
            // that source is enabled; the model otherwise only fakes delivery
            // on the `sleep` instruction, so nothing preempts a thread that
            // busy-waits or loops on `msleep` (`do_step`, `0x3ED5766E`). Vector
            // through the firmware's own handler exactly as `Op::Sleep` does,
            // whenever a compare has fired, we are in thread context, and
            // interrupts are enabled.
            // Peek, don't consume: a tick that comes due while interrupts are
            // masked or an ISR is running must stay latched until it can be
            // delivered (real hardware holds the compare-match line asserted).
            // Consuming it here unconditionally dropped ~39/40 of the ticks
            // that came due anywhere other than the one `sleep` instruction in
            // ThreadX's idle loop — the scheduler then never woke a sleeping
            // thread and the boot wedged with interrupts disabled.
            // RVF_DBG_IRQTBL: at the generic per-source dispatcher
            // (`0x3EC3E9BC`) show where it reads the pending source from
            // (`[[r29+12]+4]`) and what the handler table at `gp+58004` holds
            // for the DMA sources (0x50..0x5F).
            // The vector entry carries a `0x0000` guard parcel that
            // `vector_irq` steps over, so the dispatcher is entered at +2.
            if diag.dbg_irqtbl && pc_before == 0x3EC3_E9BE && irqtbl_n < 4 {
                irqtbl_n += 1;
                let r29 = self.cpu.regs.get(29);
                let blk = self.machine.load(r29 + 12, Width::Word).unwrap_or(0);
                let pend = self.machine.corectl.peek_pending().unwrap_or(0);
                let tbl = self.cpu.regs.get(24).wrapping_add(58004);
                let mut h = Vec::new();
                for src in [64u32, 66, 76, 77, 78, 81, 95] {
                    h.push(format!(
                        "{src}:{:#x}",
                        self.machine.load(tbl + src * 4, Width::Word).unwrap_or(0)
                    ));
                }
                eprintln!(
                    "[irqtbl] r29={r29:#x} blk={blk:#x} pending={pend:#x} handlers[{}]",
                    h.join(" ")
                );
            }

            // The firmware raises an interrupt on a core in software by
            // setting its bit in that core's pending word (`0x7E002040` /
            // `+0x844`, `0x3ED01896`). start4 uses it for the clock service's
            // timer (source 66) and for ThreadX's inter-core reschedule IPI
            // (source 78 on core 0, 79 on core 1). Nothing modelled these, so
            // every software-posted interrupt was silently dropped.
            while let Some((core, src)) = self.machine.corectl.take_sw_raised() {
                if diag.dbg_swirq {
                    eprintln!(
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
                        eprintln!(
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
                    eprintln!(
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
                            eprintln!(
                                "[tick] #{tick_deliveries} slot={slot} vbase={vb:#x} handler={h:x?} resume={:#x} retired={} nest={:#x} cur={:#x} exec={:#x}",
                                self.cpu.pc(),
                                self.cpu.retired,
                                self.machine.load(0x3EE0_3E64, Width::Word).unwrap_or(0xdead),
                                self.machine.load(0x3EE3_5900, Width::Word).unwrap_or(0xdead),
                                self.machine.load(0x3EE3_5904, Width::Word).unwrap_or(0xdead),
                            );
                        }
                    }
                    self.cpu.vector_irq(&mut self.machine, slot);
                }
            }

            if self.machine.mmio_trace && !self.machine.mmio_events.is_empty() {
                for (addr, w, val, write) in self.machine.mmio_events.drain(..) {
                    eprintln!(
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
                use std::io::Write;
                let _ = std::io::stderr().write_all(&fresh);
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
                        eprintln!(
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
                eprintln!("[trap-total] {pc:#010x} {n}");
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
            eprintln!(
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
                    eprintln!("[irqtbl]   src {src} handler={h:#x} vector={v:#x}{direct}");
                }
            }
        }

        if let Ok(list) = std::env::var("RVF_DBG_TCB") {
            for t in list.split(',') {
                let Ok(tcb) = u32::from_str_radix(t.trim().trim_start_matches("0x"), 16) else {
                    continue;
                };
                let mut ld = |a: u32| self.machine.load(a, Width::Word).unwrap_or(0xdead_dead);
                let sp = ld(tcb.wrapping_add(8));
                let disc = ld(sp);
                let (kind, resume) = if disc == 1 {
                    ("irq", ld(sp.wrapping_add(4 * (1 + 8 + 16 + 1 + 1))))
                } else {
                    ("solicited", ld(sp.wrapping_add(4 * (1 + 8 + 10))))
                };
                eprintln!(
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
                        eprintln!("[tcb]     {a:#x}: {v:#010x}");
                        shown += 1;
                        if shown == 16 {
                            break;
                        }
                    }
                }
            }
        }

        if diag.prof {
            let mut v: Vec<_> = prof_hist.iter().map(|(&k, &n)| (k, n)).collect();
            v.sort_by_key(|a| std::cmp::Reverse(a.1));
            let total: u64 = v.iter().map(|(_, n)| n).sum();
            eprintln!("--- RVF_PROF: core-0 PC buckets (total {total}) ---");
            for (pc, n) in v.iter().take(25) {
                eprintln!(
                    "  {pc:#010x}  {n:>14}  {:5.1}%",
                    100.0 * *n as f64 / total as f64
                );
            }
        }
        if diag.prof_thread {
            let total: u64 = prof_thist.values().sum();
            let mut by_thread: std::collections::HashMap<u32, u64> =
                std::collections::HashMap::new();
            for (&(t, _), &n) in prof_thist.iter() {
                *by_thread.entry(t).or_insert(0) += n;
            }
            let mut threads: Vec<_> = by_thread.into_iter().collect();
            threads.sort_by_key(|a| std::cmp::Reverse(a.1));
            eprintln!("--- RVF_PROF_THREAD: core-0 time by ThreadX thread (total {total}) ---");
            for (t, n) in threads.iter().take(8) {
                eprintln!(
                    "  thread {t:#010x}  {n:>14}  {:5.1}%",
                    100.0 * *n as f64 / total as f64
                );
                let mut buckets: Vec<_> = prof_thist
                    .iter()
                    .filter(|((tt, _), _)| tt == t)
                    .map(|((_, pc), &c)| (*pc, c))
                    .collect();
                buckets.sort_by_key(|a| std::cmp::Reverse(a.1));
                for (pc, c) in buckets.iter().take(6) {
                    eprintln!(
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
            regs: std::array::from_fn(|i| self.cpu.regs.get(i)),
            core1_pc: self.cpu1.as_ref().map(|c| c.pc()),
            core1_retired: self.cpu1.as_ref().map(|c| c.retired),
            core1_end,
            phase_tags: self.machine.phase_tags.clone(),
            core1_release_never_resolved: self.core1_release_armed && self.cpu1.is_none(),
        }
    }
}
