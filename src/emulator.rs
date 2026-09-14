//! Top-level emulator: owns the [`Vpu`] and the [`Machine`] as siblings and
//! drives the run loop.

use std::collections::HashMap;
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
    /// Where core 0 entered `start4.elf`: the first instruction it fetched
    /// from the `0xC000_0000` uncached alias start4 runs in (the bootloader
    /// runs below it). Not always [`START4_ENTRY`]: for a USB mass-storage
    /// boot the bootloader places start4 1 MiB lower (`Starting start4.elf @
    /// 0xfeb00200`), and core 1 has to run the same trampoline.
    pub start4_entry: Option<u32>,
    /// Latched once the firmware signals it wants core 1 up (a code-address
    /// write to the CoreCtl run-state words). The actual spawn is deferred
    /// until the dispatch global is populated — see [`SMP_DISPATCH_GP_OFFSET`].
    core1_release_armed: bool,
    pub machine: Machine,
    /// Model the ARM: release core 0 when `arm_loader` writes the ARM control
    /// block, then run it in lock-step with the VPU ([`crate::arm`]). Always
    /// on for a boot since #52 – a boot that should end at the handover puts a
    /// kernel on the medium that parks the ARM; tests can still turn it off.
    pub arm_enabled: bool,
    /// ARM core 0, once released.
    pub arm: Option<crate::arm::ArmSide>,
    pub input: ConsoleInput,
    /// Take the steps no per-step check can act on through
    /// [`Self::fast_steps`], which skips those checks. On unless this is a
    /// `diag` build, whose diagnostics watch every step; tests turn it off to
    /// hold the two paths to the same run.
    pub fast_loop: bool,
    /// Core-0 steps [`Self::fast_steps`] took, over every run.
    pub fast_stepped: u64,
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
    /// Stop cleanly once the console has printed this text — a shell prompt,
    /// say, where a run that got there has nothing more to show.
    pub until: Option<String>,
}

impl Default for RunLimits {
    fn default() -> Self {
        RunLimits {
            max_steps: Some(5_000_000),
            max_wall: Some(Duration::from_secs(30)),
            stop_pc: None,
            idle_spin_limit: 0,
            silent_us: 0,
            until: None,
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
    /// The ARM core hit something the model does not do yet.
    ArmStopped(crate::arm::ArmStop),
    /// The console printed [`RunLimits::until`] — after the last line of any
    /// [`ConsoleInput::script`] went in.
    Until,
    /// The user ended an interactive session (`Ctrl-A x`).
    Quit,
}

/// What the run types into the serial console (#40, milestone 6).
#[derive(Default)]
pub struct ConsoleInput {
    /// Scripted input: each text is sent once the console has printed its
    /// prompt, in order, each prompt looked for only in what came out after
    /// the previous send. Keyed to the transcript, not to time, so a scripted
    /// session is as deterministic as the boot it follows.
    pub script: std::collections::VecDeque<(String, Vec<u8>)>,
    /// Interactive input from the host (`boot --stdin`).
    pub host: Option<crate::stdio::HostInput>,
}

/// Does `hay[from..]` contain `needle`, where `from` backs up far enough from
/// `seen` (what was already searched) to catch a match straddling the two, but
/// never before `floor`?
fn printed_since(hay: &[u8], seen: usize, floor: usize, needle: &str) -> bool {
    let needle = needle.as_bytes();
    let from = seen.saturating_sub(needle.len()).max(floor);
    !needle.is_empty() && hay[from..].windows(needle.len()).any(|w| w == needle)
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
    /// True if [`Self::console`] was already streamed to stdout as it was
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
    /// `start4.elf` boot-progress tags (`0xCEC0_2000`), in order. Empty
    /// unless built with `--features diag`.
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
        let mut cpu = Vpu::new(entry);
        cpu.version_value = machine.board().stepping.vpu_version();
        Emulator {
            cpu,
            cpu1: None,
            core1_entry: None,
            start4_entry: None,
            core1_release_armed: false,
            machine,
            arm_enabled: true,
            arm: None,
            input: ConsoleInput::default(),
            // `RVF_SLOW_LOOP=1`: every step through every check, to hold the
            // fast loop to the same run.
            fast_loop: !crate::diag::ON && std::env::var_os("RVF_SLOW_LOOP").is_none(),
            fast_stepped: 0,
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

    /// Core 1, at `entry`: the same silicon as core 0, so the same `version`
    /// apart from the core-id bit, and the same unimplemented-op and trace
    /// settings.
    fn spawn_core1(&mut self, entry: u32) {
        let mut c1 = Vpu::new(entry);
        c1.core_id = 1;
        c1.version_value = self.cpu.version_value;
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
        self.spawn_core1(entry);
    }

    pub fn set_console(&mut self, c: Console) {
        self.machine.console = c;
    }

    pub fn set_unimpl_policy(&mut self, p: UnimplPolicy) {
        self.cpu.on_unimpl = p;
    }

    pub fn run(&mut self, limits: &RunLimits) -> RunReport {
        let start = Instant::now();
        // UART output goes to stdout as it happens, so a run can be
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
        let mut st = RunState::new(self, limits, diag, start);

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

        // RVF_DBG_EVGET: log every distinct (event-group, caller) pair passed to
        // `_tx_event_flags_get` (`0x3EC3E3BE`), so the groups the boot actually
        // blocks on can be told apart from the ones a shim must not touch.
        // Hoisted out of the per-instruction loop: `std::env::var_os` is a
        // locking lookup over the whole environment and these were being
        // evaluated on every step, which dominated run time.

        let end = loop {
            if let Some(end) = self.slow_step(&mut st, limits) {
                break end;
            }
            if self.fast_loop {
                if let Some(end) = self.fast_steps(&mut st, limits) {
                    break end;
                }
            }
        };

        let RunState {
            diag,
            mut console,
            trap_hits,
            prof_hist,
            prof_thist,
            core1_end,
            ..
        } = st;
        console.extend_from_slice(&self.machine.take_console_output());

        // Only the first 12 hits of each `RVF_TRAP` address are printed, so
        // report the totals as well - the print cap otherwise makes every
        // busy address look like it ran exactly 12 times.
        if crate::diag::ON && !trap_hits.is_empty() {
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
        if crate::diag::ON && diag.dbg_irqtbl {
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

        if let (true, Ok(list)) = (crate::diag::ON, std::env::var("RVF_DBG_TCB")) {
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

        if crate::diag::ON && diag.prof {
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
        if crate::diag::ON && diag.prof_thread.is_some() {
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

    /// One step with every check the run loop makes, in order: the stop
    /// conditions and per-pc hooks before core 0's instruction, then
    /// [`Self::post_step`].
    fn slow_step(&mut self, st: &mut RunState, limits: &RunLimits) -> Option<RunEnd> {
        // Anything the checks act on from here on is seen by this step's
        // checks or by the next slow step's (see `fast_steps`).
        self.machine.wake = false;
        if limits.max_steps.is_some_and(|max| {
            self.cpu.retired + self.cpu1.as_ref().map_or(0, |c| c.retired) >= max
        }) {
            return Some(RunEnd::StepLimit);
        }
        if let Some(pc) = limits.stop_pc {
            if self.cpu.pc() == pc {
                return Some(RunEnd::StopPc(pc));
            }
        }

        // The firmware's trampoline writes each core's exception-vector base
        // to CoreCtl (0x7E00_2030 / 0x38); pick it up so `swi` diag.traps into
        // the firmware's own handler table. An explicit `--exc-vbase` wins.
        if self.cpu.exc_vbase == 0 && self.machine.corectl.vbase[0] != 0 {
            self.cpu.exc_vbase = self.machine.corectl.vbase[0];
        }

        let pc_before = self.cpu.pc();
        if self.start4_entry.is_none() && pc_before >= 0xC000_0000 {
            self.start4_entry = Some(pc_before);
        }

        if crate::diag::ON && st.diag.heartbeat != 0 && self.cpu.retired >= st.next_beat {
            st.next_beat = self.cpu.retired + st.diag.heartbeat;
            eprintln!(
                "[beat] retired={} model_us={} pc={pc_before:#010x} in_exc={} irq_en={} tick_due={}",
                self.cpu.retired,
                self.machine.systimer.now_us(),
                self.cpu.in_exception,
                self.cpu.irq_enabled(),
                self.machine.systimer.tick_pending(),
            );
        }
        if let Some(cur_ptr) = st.diag.prof_thread.filter(|_| crate::diag::ON) {
            let cur = self.machine.load(cur_ptr, Width::Word).unwrap_or(0);
            *st.prof_thist.entry((cur, pc_before & !0xFF)).or_insert(0) += 1;
        }
        if crate::diag::ON && st.diag.prof {
            *st.prof_hist.entry(pc_before & !0xFF).or_insert(0) += 1;
        }

        if let Some(from) = st.diag.mmio_from.filter(|_| crate::diag::ON) {
            if !self.machine.mmio_trace && pc_before == from {
                self.machine.mmio_trace = true;
            }
        }
        // `RVF_TRACE_ON_PC=<hex>`: arm the instruction trace the first time
        // core 0 reaches this address. The console-substring trigger cannot
        // reach a code path that runs after the firmware has stopped
        // printing — which is exactly where a wedged boot has to be read.
        if let Some(pc) = st.diag.trace_on_pc.filter(|_| crate::diag::ON) {
            if !self.cpu.trace && pc_before == pc {
                self.cpu.trace = true;
                self.cpu.trace_armed = true;
                self.cpu.trace_cf_only = st.diag.trace_cf;
                self.cpu.trace_cap = st.diag.trace_cap;
                if st.diag.trace_mmio {
                    self.machine.mmio_trace = true;
                }
            }
        }
        if crate::diag::ON
            && !st.diag.traps.is_empty()
            && self.cpu.retired >= st.diag.trap_from
            && st.diag.traps.contains(&pc_before)
        {
            let n = st.trap_hits.entry(pc_before).or_insert(0);
            *n += 1;
            if *n <= st.diag.trap_max {
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
        if crate::diag::ON {
            self.machine.watch_pc = pc_before;
        }
        let step = self.cpu.step(&mut self.machine);
        self.machine.tick(1);
        self.post_step(st, limits, pc_before, step, Resume::Core0)
    }

    /// Everything a step does after core 0's instruction and the tick that
    /// follows it: interrupt delivery, core 1, console input, the ARM, the
    /// console, and the stuck and busy-wait detectors. `resume` says which of
    /// the stages up to the ARM already ran (in [`Self::fast_steps`]).
    fn post_step(
        &mut self,
        st: &mut RunState,
        limits: &RunLimits,
        pc_before: u32,
        step: crate::vpu::Step,
        resume: Resume,
    ) -> Option<RunEnd> {
        if resume == Resume::Core0 {
            // The firmware raises an interrupt on a core in software by
            // setting its bit in that core's pending word (`0x7E002040` /
            // `+0x844`, `0x3ED01896`). start4 uses it for the clock service's
            // timer (source 66) and for ThreadX's inter-core reschedule IPI
            // (source 78 on core 0, 79 on core 1). Nothing modelled these, so
            // every software-posted interrupt was silently dropped.
            while let Some((core, src)) = self.machine.corectl.take_sw_raised() {
                if crate::diag::ON && st.diag.dbg_swirq {
                    eprintln!(
                        "[sw-irq] core {core} src {src} pc={:#x} retired={}",
                        self.cpu.pc(),
                        self.cpu.retired
                    );
                }
                if core == 0 {
                    // The generic dispatcher re-reads the source from
                    // CoreCtl `+0x04`; `take_pending_irq` presents it there
                    // when it is vectored.
                    self.machine.push_pending_irq(src);
                } else if let Some(c1) = self.cpu1.as_mut() {
                    if c1.exc_vbase != 0 {
                        c1.vector_irq(&mut self.machine, src);
                    }
                }
            }

            // A device-raised interrupt (DMA completion) takes the same
            // vectoring path as the tick, but is not gated on a compare match.
            if self.cpu.irq_enabled() && self.cpu.exc_vbase != 0 {
                if let Some(src) = self.machine.take_pending_irq() {
                    if crate::diag::ON && st.diag.dbg_tick {
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
            if crate::diag::ON
                && st.diag.dbg_tick
                && tick_due
                && self.cpu.exc_vbase != 0
                && !self.cpu.irq_enabled()
            {
                st.tick_skips += 1;
                if st.tick_skips <= 20 || st.tick_skips.is_multiple_of(100_000) {
                    eprintln!(
                        "[tick-skip #{}] in_exc={} irq_en={} pc={:#x} retired={}",
                        st.tick_skips,
                        self.cpu.in_exception,
                        self.cpu.irq_enabled(),
                        self.cpu.pc(),
                        self.cpu.retired,
                    );
                }
            }
            if tick_due && self.cpu.irq_enabled() && self.cpu.exc_vbase != 0 {
                if let Some(slot) = self.machine.timer_tick_slot() {
                    // Deliver now — consume the latched flag.
                    self.machine.systimer.take_tick_pending();
                    if crate::diag::ON && st.diag.dbg_tick {
                        st.tick_deliveries += 1;
                        if st.tick_deliveries <= 30 || st.tick_deliveries.is_multiple_of(500) {
                            let vb = self.cpu.exc_vbase;
                            let h = self.machine.load(vb.wrapping_add(slot * 4), Width::Word);
                            eprintln!(
                                "[tick] #{} slot={slot} vbase={vb:#x} handler={h:x?} resume={:#x} retired={}",
                                st.tick_deliveries,
                                self.cpu.pc(),
                                self.cpu.retired,
                            );
                        }
                    }
                    self.cpu.vector_irq(&mut self.machine, slot);
                }
            }

            if crate::diag::ON && self.machine.mmio_trace && !self.machine.mmio_events.is_empty() {
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
                return Some(RunEnd::Reset);
            }

            // Core 1 (re)enters at the shared start4 reset vector — where core 0
            // entered start4 — not core 0's `entry`, which on the EEPROM path is
            // the *bootcode*, long gone by the time start4 brings its sibling up.
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
                    let entry = self
                        .core1_entry
                        .or(self.start4_entry)
                        .unwrap_or(START4_ENTRY);
                    self.spawn_core1(entry);
                }
            }
            // Interleave one core-1 step per core-0 step over the shared bus.
            self.step_core1(st);
        }

        if resume <= Resume::Core1 {
            // Console input: keystrokes from the host, now and then (a channel
            // poll per step would cost more than the step), then whatever is on
            // the line into the receive FIFO at the modelled time.
            if let Some(host) = self.input.host.as_mut() {
                st.host_poll = st.host_poll.wrapping_add(1);
                if st.host_poll.is_multiple_of(1024) {
                    match host.poll() {
                        Some(crate::stdio::HostEvent::Bytes(b)) => self.machine.uart0.feed(&b),
                        Some(crate::stdio::HostEvent::Quit) => return Some(RunEnd::Quit),
                        None => {}
                    }
                }
            }
            self.machine.uart0.pump(self.machine.systimer.now_us());

            if let Some(end) = self.step_arm() {
                return Some(end);
            }
        }

        let fresh = self.machine.take_console_output();
        let had_output = !fresh.is_empty();
        if had_output {
            st.last_output_us = self.machine.systimer.now_us();
            st.last_output_retired = self.cpu.retired;
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
                .saturating_sub(st.last_output_us);
            let silent_retired = self.cpu.retired.saturating_sub(st.last_output_retired);
            if silent_us >= limits.silent_us && silent_retired >= SILENT_RETIRED {
                return Some(RunEnd::Stuck {
                    pc: Hex(self.cpu.pc()),
                    silent_us,
                    retired: silent_retired,
                });
            }
        }
        if st.diag.live_console && had_output {
            use std::io::Write;
            // The serial console is what a run is for, so it is stdout (#55),
            // flushed at once: a prompt does not end in a newline.
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&fresh);
            let _ = out.flush();
        }
        st.console.extend_from_slice(&fresh);
        if had_output {
            while let Some((prompt, text)) = self.input.script.front() {
                if !printed_since(&st.console, st.prompt_seen, st.prompt_floor, prompt) {
                    break;
                }
                self.machine.uart0.feed(text);
                self.input.script.pop_front();
                st.prompt_floor = st.console.len();
                st.prompt_seen = st.prompt_floor;
            }
            if let (Some(needle), true) = (&limits.until, self.input.script.is_empty()) {
                if printed_since(&st.console, st.prompt_seen, st.prompt_floor, needle) {
                    return Some(RunEnd::Until);
                }
            }
            st.prompt_seen = st.console.len();
        }
        // `RVF_TRACE_ON_CONSOLE=<substr>` arms the instruction trace the moment
        // that substring appears in the console — for pinning down a code path
        // by the log line that precedes it.
        if let Some(needle) = st
            .diag
            .trace_on_console
            .as_ref()
            .filter(|_| crate::diag::ON)
        {
            if !self.cpu.trace && st.console.len() > st.console_seen {
                let from = st.console_seen.saturating_sub(needle.len());
                if String::from_utf8_lossy(&st.console[from..]).contains(needle.as_str()) {
                    self.cpu.trace = true;
                    self.cpu.trace_armed = true;
                    self.cpu.trace_cf_only = st.diag.trace_cf;
                    self.cpu.trace_cap = st.diag.trace_cap;
                    // Also stream peripheral accesses while the trace is
                    // armed (RVF_TRACE_MMIO=1) — handy for pinning down an
                    // unmodelled block like the I2C BSC.
                    if st.diag.trace_mmio {
                        self.machine.mmio_trace = true;
                    }
                }
                st.console_seen = st.console.len();
            }
        }

        match step {
            crate::vpu::Step::Stopped => {
                let stop = self.cpu.stopped.clone().expect("stopped without reason");
                return Some(RunEnd::Halted(stop));
            }
            crate::vpu::Step::Ran => {}
        }
        // Core 1 halting does not stop core 0 — record it and carry on.

        if limits.idle_spin_limit > 0 {
            // A loop that keeps reading the free-running system timer is a
            // firmware `usleep` — time-bounded, so not a hung spin however
            // many iterations it takes. `max_steps` / `max_wall` still cap
            // a pathological one.
            let timer_polling = self.machine.systimer.clo_reads != st.clo_reads_at_cf;
            // Firmware busy-wait on the free-running counter: same edge,
            // timer advancing, nothing else changing, no output. Let it
            // build up, then skip the counter forward a slice at a time.
            if let Some(cf) = self.cpu.cf_last {
                let p = progress_count(&self.machine);
                // A firmware `udelay` (`while (CLO - start) < n`) spins the
                // same 2-instruction edge thousands of times per call and the
                // clock bring-up does hundreds of them — the model's biggest
                // time sink. Recognise it: the same taken edge, the counter
                // advancing, no console output. The periodic ThreadX tick ISR
                // fires in the middle of a long delay and does a *bounded*
                // amount of RAM traffic, so tolerate a small `progress` delta
                // (a real memcpy/memtest in the loop would blow past it) rather
                // than resetting.
                let prog_delta = p.wrapping_sub(st.progress_at_delay);
                if cf == st.delay_ff_cf && timer_polling && !had_output && prog_delta < 4_096 {
                    st.delay_ff += 1;
                    st.progress_at_delay = p;
                    if st.delay_ff >= 1_000 {
                        // Jump (not `skip_ahead`) so one long `udelay` clears
                        // in a few detections instead of being chopped at
                        // every tick deadline; `service_matches` collapses
                        // any ticks the jump skips to a single delivery.
                        self.machine.systimer.jump(50_000);
                        st.delay_ff = 0;
                    }
                } else {
                    st.delay_ff = 0;
                    st.delay_ff_cf = cf;
                    st.progress_at_delay = p;
                }
            }
            if let Some(cf) = self.cpu.cf_last {
                // A bare read-only poll counts as a spin, but firmware
                // delay/lock loops legitimately iterate 10k+ times before
                // giving up, so the threshold is generous.
                let progress = progress_count(&self.machine);
                let progressing = progress != st.progress_at_cf || timer_polling;
                if cf == st.last_cf && !had_output && !progressing {
                    st.cf_repeat += 1;
                    if st.cf_repeat >= 200_000 {
                        return Some(RunEnd::IdleSpin(cf.0));
                    }
                } else {
                    st.cf_repeat = 0;
                    st.last_cf = cf;
                    st.progress_at_cf = progress;
                }
            }
            st.clo_reads_at_cf = self.machine.systimer.clo_reads;

            st.w_lo = st.w_lo.min(pc_before);
            st.w_hi = st.w_hi.max(pc_before);
            st.w_output |= had_output;
            st.w_steps += 1;
            if st.w_steps >= st.win {
                let clo_delta = self
                    .machine
                    .systimer
                    .clo_reads
                    .wrapping_sub(st.clo_reads_at_window);
                let stalled = progress_count(&self.machine) == st.progress_at_window
                    && self.machine.systimer.clo_reads == st.clo_reads_at_window;
                if !st.w_output && stalled && st.w_hi.wrapping_sub(st.w_lo) <= 4096 {
                    return Some(RunEnd::IdleSpin(st.w_lo));
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
                let tight = st.w_hi.wrapping_sub(st.w_lo) <= 0x40;
                // Experimental `--boot-rom`: the maskROM's `udelay` helper sits
                // ~12 KB from its callers, so the window is wide and the CLO-read
                // ratio low — neither the tight nor the wide threshold below
                // catches it. Loosen it for that path only; a normal boot never
                // has the overlay set, so no golden is affected.
                // Experimental `--boot-rom`: the maskROM's `udelay` helper sits
                // ~12 KB from its callers, so the window is wide and the CLO-read
                // ratio low — the wide `win/8` threshold below never catches it.
                // Loosen it for that path only; a normal boot never has the
                // overlay set, so no golden is affected.
                let boot_rom = self.machine.executing_boot_rom();
                let ff = !st.w_output
                    && if tight {
                        clo_delta > st.win / 20
                    } else if boot_rom {
                        clo_delta > st.win / 64 && self.cpu.in_exception == 0
                    } else {
                        clo_delta > st.win / 8 && self.cpu.in_exception == 0
                    };
                if ff {
                    self.machine.systimer.jump(200_000);
                }
                if crate::diag::ON && st.diag.dbg_ff {
                    eprintln!(
                        "[ff] win close: clo_delta={clo_delta} w=[{:#x}..{:#x}] out={} exc={} ff={ff} @{}",
                        st.w_lo, st.w_hi, st.w_output, self.cpu.in_exception, self.cpu.retired
                    );
                }
                st.clo_reads_at_window = self.machine.systimer.clo_reads;
                st.w_lo = u32::MAX;
                st.w_hi = 0;
                st.w_steps = 0;
                st.w_output = false;
                st.progress_at_window = progress_count(&self.machine);
            }
        }

        st.wall_check += 1;
        if let Some(max) = limits.max_wall {
            if st.wall_check.is_multiple_of(65_536) && st.start.elapsed() >= max {
                return Some(RunEnd::TimeLimit);
            }
        }
        None
    }

    /// Core 1's step: one per core-0 step, over the shared bus.
    #[inline]
    fn step_core1(&mut self, st: &mut RunState) {
        if let Some(c1) = self.cpu1.as_mut() {
            if c1.exc_vbase == 0 && self.machine.corectl.vbase[1] != 0 {
                c1.exc_vbase = self.machine.corectl.vbase[1];
            }
            if !c1.is_stopped() && !c1.halted {
                let pc_before = c1.pc();
                if let crate::vpu::Step::Stopped = c1.step(&mut self.machine) {
                    st.core1_end = Some(RunEnd::Core1Halted(
                        c1.stopped.clone().expect("stop reason"),
                    ));
                }
                // Core 1's accesses under its own pc, or the next core-0
                // step prints them against core 0's.
                if crate::diag::ON && self.machine.mmio_trace {
                    for (addr, w, val, write) in self.machine.mmio_events.drain(..) {
                        eprintln!(
                            "mmio {:#010x}  {}{}  {:#010x} <- {:#0width$x}  core1",
                            pc_before,
                            if write { "W" } else { "R" },
                            w,
                            addr,
                            val,
                            width = (w as usize) * 2 + 2,
                        );
                    }
                }
            }
        }
    }

    /// The ARM: out of reset when `arm_loader` writes the ARM control block,
    /// then kept in step with the system timer.
    #[inline]
    fn step_arm(&mut self) -> Option<RunEnd> {
        if self.arm_enabled && self.arm.is_none() && self.machine.armctrl.take_release() {
            self.arm = Some(crate::arm::ArmSide::released(&mut self.machine));
            self.machine.defer_sleep = true;
        }
        if let Some(arm) = self.arm.as_mut() {
            // The VPU went to `sleep`: it wakes at its next compare, or when
            // the ARM writes something that interrupts it — a mailbox
            // request, most often — whichever comes first. So the ARM runs
            // up to that compare first, and the counter only moves as far as
            // it got (#53; `arm.rs`, "Time and scheduling").
            if let Some(to) = self.machine.sleep_to.take() {
                let us = arm.run_until_store(&mut self.machine, to);
                self.machine.wake_vpu_at(us);
            }
            arm.catch_up(&mut self.machine);
            if let Some(stop) = &arm.stopped {
                return Some(RunEnd::ArmStopped(stop.clone()));
            }
        }
        None
    }

    /// Run steps for as long as none of the run loop's per-step checks can
    /// act, doing only what every step has to do: core 0, the tick, core 1,
    /// the UART receiver, the ARM, and the detectors' running state.
    ///
    /// The guest sees exactly the same run as through [`Self::slow_step`]
    /// alone: the same instructions, retired at the same step, with every
    /// interrupt and time jump at the same point. What is skipped is only
    /// checks that are known to do nothing, and each is covered one of
    /// three ways:
    ///
    /// 1. By a flag raised at the event it depends on. A peripheral write, a
    ///    queued interrupt, a compare that fires or a reset coming due set
    ///    [`Machine::wake`]; core 0 entering or leaving an exception,
    ///    switching interrupts, sleeping, stopping or missing the decode cache
    ///    sets [`Vpu::event`]; a read of the system timer's counter shows in
    ///    its `clo_reads`. The step that raises one is finished by
    ///    [`Self::post_step`] from where it got to, and the step after it is
    ///    a slow one, so a check that runs before core 0's instruction (the
    ///    exception-vector pickup) sees it too.
    /// 2. By a budget, for the checks that come due by counting: the step
    ///    limit, the wall-clock check, host input, the detectors' window and
    ///    the console-silence watchdog. The budget stops short of the step
    ///    where any of them could fire.
    /// 3. By the pc, for the checks tied to an address: `stop_pc` and the
    ///    start4 entry. Such a pc ends the fast run before its instruction.
    ///
    /// A queued interrupt that only the interrupt-enable bit holds back is
    /// the one thing no flag covers, since any instruction can write `r30`,
    /// so it keeps the run on slow steps; so does core 1's release while it
    /// waits on a word in RAM.
    ///
    /// The detectors keep their running state exactly. On a step with no
    /// timer read and a decode-cache hit — which counts as a RAM read, so
    /// `progress` moved — both of their per-step updates take the "reset"
    /// branch, which is all this does.
    fn fast_steps(&mut self, st: &mut RunState, limits: &RunLimits) -> Option<RunEnd> {
        if self.machine.wake || self.cpu.is_stopped() {
            return None;
        }
        if self.cpu.exc_vbase != 0
            && (self.machine.irq_queued() || self.machine.systimer.tick_pending())
        {
            return None;
        }
        if self.cpu1.is_none() && self.core1_release_armed {
            return None;
        }
        let budget = self.fast_budget(st, limits);
        if budget == 0 {
            return None;
        }
        // The pcs a slow step has to handle before their instruction. A
        // `u32::MAX` that matches by accident only costs a slow step.
        let stop_pc = limits.stop_pc.unwrap_or(u32::MAX);
        let entry = if self.start4_entry.is_none() {
            0xC000_0000
        } else {
            u32::MAX
        };
        // Only a slow step feeds the receive line.
        let uart_busy = self.machine.uart0.rx_backlog() != 0;
        let host = self.input.host.is_some();
        let spin = limits.idle_spin_limit > 0;
        // Core 1 steps only while it runs. It can stop or sleep in here, but
        // only a slow step wakes it, and its vector-base pickup is done: the
        // register it reads only moves with a peripheral write.
        let mut core1_runs = self
            .cpu1
            .as_ref()
            .is_some_and(|c| !c.is_stopped() && !c.halted);
        // Only a register write releases the ARM, and that ends the run.
        let arm_on = self.arm.is_some();
        self.cpu.event = false;
        let mut n = 0u64;
        let end = 'run: {
            while n < budget {
                let pc = self.cpu.pc();
                if (pc == stop_pc) | (pc >= entry) {
                    break 'run None;
                }
                let step = self.cpu.step(&mut self.machine);
                n += 1;
                self.machine.tick(1);
                if self.machine.wake | self.cpu.event {
                    break 'run self.post_step(st, limits, pc, step, Resume::Core0);
                }
                if core1_runs {
                    self.step_core1(st);
                    if self.machine.wake {
                        break 'run self.post_step(st, limits, pc, step, Resume::Core1);
                    }
                    core1_runs = self
                        .cpu1
                        .as_ref()
                        .is_some_and(|c| !c.is_stopped() && !c.halted);
                }
                if host {
                    st.host_poll = st.host_poll.wrapping_add(1);
                }
                if uart_busy {
                    self.machine.uart0.pump(self.machine.systimer.now_us());
                }
                if arm_on {
                    if let Some(end) = self.step_arm() {
                        break 'run Some(end);
                    }
                    if self.machine.wake {
                        break 'run self.post_step(st, limits, pc, step, Resume::Arm);
                    }
                }
                if spin {
                    if self.machine.systimer.clo_reads != st.clo_reads_at_cf {
                        break 'run self.post_step(st, limits, pc, step, Resume::Arm);
                    }
                    if let Some(cf) = self.cpu.cf_last {
                        let p = progress_count(&self.machine);
                        st.delay_ff = 0;
                        st.delay_ff_cf = cf;
                        st.progress_at_delay = p;
                        st.cf_repeat = 0;
                        st.last_cf = cf;
                        st.progress_at_cf = p;
                    }
                    st.w_lo = st.w_lo.min(pc);
                    st.w_hi = st.w_hi.max(pc);
                    st.w_steps += 1;
                }
                st.wall_check += 1;
            }
            None
        };
        self.fast_stepped += n;
        end
    }

    /// How many steps [`Self::fast_steps`] may take before one of the
    /// counted checks could fire.
    fn fast_budget(&self, st: &RunState, limits: &RunLimits) -> u64 {
        let mut n = u64::MAX;
        if let Some(max) = limits.max_steps {
            // Each step retires at most one instruction per running core.
            let core1_runs = self
                .cpu1
                .as_ref()
                .is_some_and(|c| !c.is_stopped() && !c.halted);
            let done = self.cpu.retired + self.cpu1.as_ref().map_or(0, |c| c.retired);
            n = n.min(max.saturating_sub(done) / if core1_runs { 2 } else { 1 });
        }
        if limits.max_wall.is_some() {
            n = n.min(65_535 - st.wall_check % 65_536);
        }
        if self.input.host.is_some() {
            n = n.min(u64::from(1_023 - st.host_poll % 1_024));
        }
        if limits.idle_spin_limit > 0 {
            n = n.min(st.win - 1 - st.w_steps);
        }
        if limits.silent_us > 0 {
            // The watchdog needs both halves. In the `k`-th step from here
            // core 0 has retired at most `k` more instructions and the
            // counter has had exactly `k` more cycles (a jump is an event).
            let retired_left = SILENT_RETIRED
                .saturating_sub(self.cpu.retired.saturating_sub(st.last_output_retired));
            let cycles_left = self
                .machine
                .systimer
                .cycles_until(st.last_output_us.saturating_add(limits.silent_us));
            n = n.min(retired_left.max(cycles_left).saturating_sub(1));
        }
        n
    }
}

/// How many instructions the console-silence watchdog wants on top of its
/// modelled time ([`RunLimits::silent_us`]).
const SILENT_RETIRED: u64 = 20_000_000;

/// "Progress" = RAM stores + peripheral stores + RAM loads. A memset or
/// memcpy advances the stores; a DRAM memtest read-back advances the loads.
/// A poll loop our stubs never satisfy touches none of them (MMIO loads are
/// not counted), so it still trips.
fn progress_count(m: &Machine) -> u64 {
    m.ram_writes
        .wrapping_add(m.mmio_writes)
        .wrapping_add(m.ram_reads)
}

/// How far a step got before [`Emulator::fast_steps`] handed it to
/// [`Emulator::post_step`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Resume {
    /// Core 0 stepped and the clock ticked.
    Core0,
    /// Core 1 stepped as well.
    Core1,
    /// Console input and the ARM ran too.
    Arm,
}

/// What [`Emulator::run`] carries from one step to the next.
struct RunState {
    start: Instant,
    diag: crate::diag::DiagConfig,
    console: Vec<u8>,
    wall_check: u64,
    /// How much of the console `RVF_TRACE_ON_CONSOLE` has searched.
    console_seen: usize,
    /// Prompt / `until` search: how much of the console has been searched,
    /// and where the output after the last scripted send begins.
    prompt_seen: usize,
    prompt_floor: usize,
    host_poll: u32,

    // Spin detection: over a sliding window of steps, track the min/max PC
    // and whether any console output happened. If the PC stays within a
    // small window for the whole window length with no output, call it a
    // spin (a peripheral poll our stubs never satisfy).
    win: u64,
    // Console-silence watchdog: the modelled clock and the retired count at
    // the last byte the firmware printed.
    last_output_us: u64,
    last_output_retired: u64,
    w_lo: u32,
    w_hi: u32,
    w_steps: u64,
    w_output: bool,
    progress_at_window: u64,
    clo_reads_at_window: u64,
    // Fast path: the exact same taken transfer repeating is a tight spin —
    // *unless* memory traffic keeps advancing.
    last_cf: (u32, u32),
    cf_repeat: u64,
    progress_at_cf: u64,
    clo_reads_at_cf: u64,
    // Busy-wait fast-forward: a repeating control-flow edge that only
    // advances the system-timer counter is a firmware `usleep`
    // (`while now - start < N`). Count the iterations and jump the timer
    // ahead so a multi-millisecond delay doesn't eat the step budget.
    delay_ff: u64,
    delay_ff_cf: (u32, u32),
    progress_at_delay: u64,

    tick_deliveries: u64,
    tick_skips: u64,
    /// RVF_PROF=1: cheap PC profiler. Bucket the core-0 PC into 256-byte
    /// slots on every step and dump the hottest on exit — finds the loop
    /// that is eating the step budget when a boot phase runs slow.
    prof_hist: HashMap<u32, u64>,
    /// RVF_PROF_THREAD=1: same buckets, but keyed by the running ThreadX
    /// thread (`_tx_thread_current_ptr`, `0x3EE35900`) as well, so "which
    /// thread is spinning, and where" can be read off directly.
    prof_thist: HashMap<(u32, u32), u64>,
    /// RVF_HEARTBEAT=<n>: every <n> retired instructions, print model
    /// time, the running ThreadX thread and the PC. The one diagnostic that
    /// says whether a stalled boot is wedged or merely slow.
    next_beat: u64,
    /// RVF_TRAP=<hex>[,<hex>...]: print pc / lr / r0-r5 every time core 0
    /// reaches one of these addresses. Generic "who calls this, with what"
    /// probe - the linear disassembler can't xref (it desyncs on inline
    /// data), so callers have to be found at runtime. `RVF_TRAP_MAX=<n>`:
    /// how many hits of each trap address to print (default 12); the totals
    /// are always reported at exit. RVF_TRAP_FROM=<n>: ignore trap hits
    /// before <n> million retired instructions, so the steady state can be
    /// sampled instead of only early boot.
    trap_hits: HashMap<u32, u64>,
    core1_end: Option<RunEnd>,
}

impl RunState {
    fn new(
        emu: &Emulator,
        limits: &RunLimits,
        diag: crate::diag::DiagConfig,
        start: Instant,
    ) -> RunState {
        let m = &emu.machine;
        RunState {
            start,
            next_beat: emu.cpu.retired + diag.heartbeat,
            diag,
            console: Vec::new(),
            wall_check: 0,
            console_seen: 0,
            prompt_seen: 0,
            prompt_floor: 0,
            host_poll: 0,
            win: limits.idle_spin_limit.max(1),
            last_output_us: 0,
            last_output_retired: 0,
            w_lo: u32::MAX,
            w_hi: 0,
            w_steps: 0,
            w_output: false,
            progress_at_window: progress_count(m),
            clo_reads_at_window: m.systimer.clo_reads,
            last_cf: (u32::MAX, u32::MAX),
            cf_repeat: 0,
            progress_at_cf: progress_count(m),
            clo_reads_at_cf: m.systimer.clo_reads,
            delay_ff: 0,
            delay_ff_cf: (u32::MAX, u32::MAX),
            progress_at_delay: progress_count(m),
            tick_deliveries: 0,
            tick_skips: 0,
            prof_hist: HashMap::new(),
            prof_thist: HashMap::new(),
            trap_hits: HashMap::new(),
            core1_end: None,
        }
    }
}
