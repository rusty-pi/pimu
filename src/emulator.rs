//! Top-level emulator: owns the [`Vpu`] and the [`Machine`], and drives the run loop.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::bus::{Bus, Width};
use crate::log::Channel;
use crate::machine::Machine;
use crate::vpu::{Stop, UnimplPolicy, Vpu};

pub struct Emulator {
    pub cpu: Vpu,
    /// VPU core 1, `None` until the firmware writes its core-control `WAKEUP`.
    pub cpu1: Option<Vpu>,
    pub machine: Machine,
    /// Release ARM core 0 when `arm_loader` writes the ARM control block, then run it
    /// in lock-step with the VPU ([`crate::arm`]). Always on for a boot.
    pub arm_enabled: bool,
    pub arm: Option<crate::arm::ArmSide>,
    pub input: ConsoleInput,
    /// Stream the console to stdout as the run goes (`PIMU_LIVE_CONSOLE=0` to stop).
    pub stream_console: bool,
    /// Use [`Self::fast_steps`]; off in a `diag` build, whose diagnostics watch
    /// every step.
    pub fast_loop: bool,
    pub fast_stepped: u64,
}

/// Stopping conditions for [`Emulator::run`].
#[derive(Debug, Clone)]
pub struct RunLimits {
    /// Cap on retired instructions; `None` runs until another condition ends the run.
    pub max_steps: Option<u64>,
    pub max_wall: Option<Duration>,
    pub stop_pc: Option<u32>,
    /// Stop on this many steps at one PC with no console output; 0 disables.
    pub idle_spin_limit: u64,
    /// Stop once the firmware has printed nothing for this many microseconds of
    /// *modelled* time. A wedged firmware stops printing while modelled time keeps
    /// advancing, which makes silence a far better stuck-detector than a PC-window
    /// heuristic that the ThreadX tick defeats. For scale, the largest gap in a
    /// start4 log from a Raspberry Pi 4B d03115 is about a second and the model's own
    /// worst (the kernel load) thirteen. 0 disables.
    pub silent_us: u64,
    /// Stop once the console has printed this text — a shell prompt, say.
    pub until: Option<String>,
    /// Hold the run to this multiple of real time: whenever the modelled clock is
    /// ahead of the host's, the loop sleeps the difference away. `None` runs as fast
    /// as the host manages, which is what a regression run and CI want.
    pub speed: Option<f64>,
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
            speed: None,
        }
    }
}

/// A `u32` that debug-prints as hex, to be read against a disassembly.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Hex(pub u32);

impl std::fmt::Debug for Hex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#010x}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    Halted(Stop),
    StopPc(u32),
    StepLimit,
    TimeLimit,
    IdleSpin(u32),
    /// The firmware stopped logging for [`RunLimits::silent_us`] while still running.
    Stuck {
        pc: Hex,
        silent_us: u64,
        retired: u64,
    },
    /// VPU core 1 halted; core 0 may still have been running.
    Core1Halted(Stop),
    /// Firmware asked the SoC to reset; the caller re-runs from a fresh machine.
    Reset,
    ArmStopped(crate::arm::ArmStop),
    Until,
    Quit,
}

#[derive(Default)]
pub struct ConsoleInput {
    /// Scripted input, each text sent once its prompt appears in what came out after
    /// the previous send — keyed to the transcript, so it is as deterministic as the
    /// boot it follows.
    pub script: std::collections::VecDeque<(String, Vec<u8>)>,
    pub host: Option<crate::stdio::HostInput>,
}

/// Does `hay` contain `needle` at or after `seen`, never looking before `floor`?
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
    /// How much of [`Self::wall`] went on real-time pacing ([`RunLimits::speed`]).
    pub slept: Duration,
    pub pc: u32,
    pub console: Vec<u8>,
    pub console_streamed: bool,
    pub unimpl: Vec<crate::vpu::exec::UnimplHit>,
    pub regs: [u32; 32],
    pub core1_pc: Option<u32>,
    pub core1_retired: Option<u64>,
    pub core1_end: Option<RunEnd>,
    /// `start4.elf` boot-progress tags; empty unless built with `--features diag`.
    pub phase_tags: Vec<u32>,
}

impl Emulator {
    pub fn new(machine: Machine, entry: u32) -> Emulator {
        let mut cpu = Vpu::new(entry);
        cpu.version_value = machine.board().stepping.vpu_version();
        cpu.log = machine.log.clone();
        Emulator {
            cpu,
            cpu1: None,
            machine,
            arm_enabled: true,
            arm: None,
            input: ConsoleInput::default(),
            stream_console: true,
            // `PIMU_SLOW_LOOP=1` puts every step through every check.
            fast_loop: !crate::diag::ON && std::env::var_os("PIMU_SLOW_LOOP").is_none(),
            fast_stepped: 0,
        }
    }

    fn spawn_core1(&mut self, entry: u32) {
        if crate::diag::ON {
            eprintln!("[core1] released at {entry:#010x}");
        }
        let mut c1 = Vpu::new(entry);
        c1.core_id = 1;
        c1.version_value = self.cpu.version_value;
        c1.on_unimpl = self.cpu.on_unimpl;
        c1.trace = self.cpu.trace;
        c1.trace_cf_only = self.cpu.trace_cf_only;
        c1.trace_cap = self.cpu.trace_cap;
        c1.trace_from = self.cpu.trace_from;
        c1.log = self.cpu.log.clone();
        self.cpu1 = Some(c1);
    }

    /// Bring up VPU core 1 now, for payloads that run both cores (`--smp`, tests).
    pub fn start_smp(&mut self, entry: u32) {
        self.spawn_core1(entry);
    }

    pub fn set_unimpl_policy(&mut self, p: UnimplPolicy) {
        self.cpu.on_unimpl = p;
    }

    pub fn run(&mut self, limits: &RunLimits) -> RunReport {
        let start = Instant::now();
        let mut diag = crate::diag::DiagConfig::from_env();
        diag.live_console &= self.stream_console;
        // `PIMU_MMIO_FROM=<hex>` arms the trace only once the PC reaches that
        // address, so a late boot stage can be captured on its own.
        if diag.mmio_from.is_some() {
            self.machine.mmio_trace = false;
        }
        let mut st = RunState::new(self, limits, diag, start);

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
            slept: st_slept,
            ..
        } = st;
        console.extend_from_slice(&self.machine.take_console_output());

        // The print cap would otherwise make every busy address look like it
        // ran exactly 12 times.
        if crate::diag::ON && !trap_hits.is_empty() {
            let mut totals: Vec<(u32, u64)> = trap_hits.into_iter().collect();
            totals.sort_unstable();
            for (pc, n) in totals {
                eprintln!("[trap-total] {pc:#010x} {n}");
            }
        }

        // `--log irqtbl`: the per-source handler table the generic dispatcher
        // indexes, where a zero entry means the source is never handled.
        if crate::diag::ON && self.machine.log.on(Channel::IrqTbl) {
            let tbl = self.cpu.regs.get(24).wrapping_add(58004);
            let vb = self.cpu.exc_vbase;
            crate::log!(
                self.machine.log,
                Channel::IrqTbl,
                "gp={:#x} table={tbl:#x} vbase={vb:#x}",
                self.cpu.regs.get(24)
            );
            // A vector-table entry is either a direct handler or the generic
            // dispatcher, so a zero handler slot means direct, not broken.
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
                    crate::log!(
                        self.machine.log,
                        Channel::IrqTbl,
                        "  src {src} handler={h:#x} vector={v:#x}{direct}"
                    );
                }
            }
        }

        if crate::diag::ON {
            for &tcb in &diag.tcbs {
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
                // `resume` is only the return out of
                // `_tx_thread_system_suspend`, so scan the stack above the frame
                // for text addresses as a rough backtrace of what it waits on.
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
            eprintln!("--- PIMU_PROF: core-0 PC buckets (total {total}) ---");
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
            eprintln!("--- PIMU_PROF_THREAD: core-0 time by ThreadX thread (total {total}) ---");
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
            slept: st_slept,
            pc: self.cpu.pc(),
            console,
            console_streamed: diag.live_console,
            unimpl,
            regs: std::array::from_fn(|i| self.cpu.regs.get(i)),
            core1_pc: self.cpu1.as_ref().map(|c| c.pc()),
            core1_retired: self.cpu1.as_ref().map(|c| c.retired),
            core1_end,
            phase_tags: self.machine.phase_tags.clone(),
        }
    }

    /// One step with every check the run loop makes, in order.
    fn slow_step(&mut self, st: &mut RunState, limits: &RunLimits) -> Option<RunEnd> {
        self.machine.recheck = false;
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

        // The core re-reads its vector base from CoreCtl on every exception, so
        // each write moves it; `--exc-vbase` stands until the first.
        if let Some(vbase) = self.machine.corectl.take_vbase(0) {
            self.cpu.exc_vbase = vbase;
        }

        let pc_before = self.cpu.pc();

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
        // `PIMU_TRACE_ON_PC=<hex>`: for the code paths a console trigger cannot
        // reach, because the firmware has already stopped printing.
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
            self.machine.watch_core = 0;
        }
        let step = self.cpu.step(&mut self.machine);
        self.machine.tick(1);
        self.post_step(st, limits, pc_before, step, Resume::Core0)
    }

    /// Everything after core 0's instruction and its tick: interrupt delivery, core 1,
    /// console input, the ARM, the console, and the detectors. `resume` says which of
    /// the stages up to the ARM already ran.
    fn post_step(
        &mut self,
        st: &mut RunState,
        limits: &RunLimits,
        pc_before: u32,
        step: crate::vpu::Step,
        resume: Resume,
    ) -> Option<RunEnd> {
        if resume == Resume::Core0 {
            // The firmware raises interrupts in software through a core's
            // pending word: the clock service's timer, and ThreadX's IPI.
            while let Some((core, src)) = self.machine.corectl.take_sw_raised() {
                if crate::diag::ON {
                    crate::log!(
                        self.machine.log,
                        Channel::SwIrq,
                        "core {core} src {src} pc={:#x} retired={}",
                        self.cpu.pc(),
                        self.cpu.retired
                    );
                }
                if core == 0 {
                    self.machine.push_pending_irq(src);
                } else {
                    // Queued, not vectored: core 1 may have interrupts off or
                    // not be running yet, and the source must not be lost.
                    self.machine.push_core1_irq(src);
                }
            }

            // A device interrupt vectors like the tick, ungated by a compare.
            if self.cpu.irq_enabled() && self.cpu.exc_vbase != 0 {
                if let Some(src) = self.machine.take_pending_irq() {
                    if crate::diag::ON {
                        crate::log!(
                            self.machine.log,
                            Channel::Tick,
                            "irq src={src} pc={:#x} retired={}",
                            self.cpu.pc(),
                            self.cpu.retired
                        );
                    }
                    self.cpu.vector_irq(&mut self.machine, src);
                }
            }
            let tick_due = self.machine.timer_irq_due();
            if crate::diag::ON
                && self.machine.log.on(Channel::Tick)
                && tick_due
                && self.cpu.exc_vbase != 0
                && !self.cpu.irq_enabled()
            {
                st.tick_skips += 1;
                if st.tick_skips <= 20 || st.tick_skips.is_multiple_of(100_000) {
                    crate::log!(
                        self.machine.log,
                        Channel::Tick,
                        "skip #{} in_exc={} irq_en={} pc={:#x} retired={}",
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
                    self.machine.take_tick_pending();
                    if crate::diag::ON && self.machine.log.on(Channel::Tick) {
                        st.tick_deliveries += 1;
                        if st.tick_deliveries <= 30 || st.tick_deliveries.is_multiple_of(500) {
                            let vb = self.cpu.exc_vbase;
                            let h = self.machine.load(vb.wrapping_add(slot * 4), Width::Word);
                            crate::log!(
                                self.machine.log,
                                Channel::Tick,
                                "#{} slot={slot} vbase={vb:#x} handler={h:x?} resume={:#x} retired={}",
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

            // Stop so the caller can re-run from a machine seeded with the
            // updated flash.
            if self.machine.pm.take_reset() {
                return Some(RunEnd::Reset);
            }

            // The model never powers core 1 down, so a second WAKEUP write
            // finds it running and has nothing to do.
            if let Some(entry) = self.machine.corectl.take_core1_wake() {
                if self.cpu1.is_none() {
                    self.spawn_core1(entry);
                }
            }
            self.step_core1(st);
        }

        if resume <= Resume::Core1 {
            // Host keystrokes, polled now and then: a channel poll per step
            // would cost more than the step itself.
            if let Some(host) = self.input.host.as_mut() {
                st.host_poll = st.host_poll.wrapping_add(1);
                if st.host_poll.is_multiple_of(1024) {
                    match host.poll() {
                        Some(crate::stdio::HostEvent::Bytes(b)) => self.machine.console_feed(&b),
                        Some(crate::stdio::HostEvent::Quit) => return Some(RunEnd::Quit),
                        None => {}
                    }
                }
            }
            self.machine.console_pump(self.machine.systimer.now_us());

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
            // Both halves matter: modelled time alone trips on a long `sleep`,
            // instructions alone on a busy stretch with nothing to say.
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
            // Flushed at once: a prompt does not end in a newline.
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
                self.machine.console_feed(text);
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
        // `PIMU_TRACE_ON_CONSOLE=<substr>` arms the trace from the log line
        // that precedes the code path being pinned down.
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
                    // `PIMU_TRACE_MMIO=1` also streams peripheral accesses.
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

        if limits.idle_spin_limit > 0 {
            // A loop reading the free-running timer is a `usleep`: time-bounded,
            // so not a hung spin however many iterations it takes.
            let timer_polling = self.machine.systimer.clo_reads != st.clo_reads_at_cf;
            self.busy_wait_ff(st, pc_before, had_output, timer_polling);
            if let Some(cf) = self.cpu.cf_last {
                // Firmware delay and lock loops legitimately iterate 10k+ times,
                // so the threshold is generous.
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
                let stalled = progress_count(&self.machine) == st.progress_at_window
                    && self.machine.systimer.clo_reads == st.clo_reads_at_window;
                if !st.w_output && stalled && st.w_hi.wrapping_sub(st.w_lo) <= 4096 {
                    return Some(RunEnd::IdleSpin(st.w_lo));
                }
                st.clo_reads_at_window = self.machine.systimer.clo_reads;
                st.w_lo = u32::MAX;
                st.w_hi = 0;
                st.w_steps = 0;
                st.w_output = false;
                st.progress_at_window = progress_count(&self.machine);
            }
        }

        if let Some(speed) = limits.speed {
            self.pace(st, speed);
        }

        st.wall_check += 1;
        if let Some(max) = limits.max_wall {
            // Pacing sleep does not count: the budget bounds the work, not the
            // real time a paced run is meant to take.
            if st.wall_check.is_multiple_of(65_536)
                && st.start.elapsed().saturating_sub(st.slept) >= max
            {
                return Some(RunEnd::TimeLimit);
            }
        }
        None
    }

    /// Hold the modelled clock to `speed` times real time, sleeping off whatever
    /// lead it has built up. Called on a slow step, and only once the counter has
    /// passed [`PACE_QUANTUM_US`], so the ordinary step pays a single compare.
    ///
    /// Most of a boot is the other way round — the model is slower than the board it
    /// models — and a lead is built only where the run loop jumps the counter (an
    /// idle `sleep`, a `wfi`, a fast-forwarded delay), which is exactly where real
    /// hardware waits too. Time the model has fallen behind is never banked as credit
    /// to run free with later: the baseline moves up instead, so a slow stretch
    /// cannot buy an unthrottled one.
    fn pace(&mut self, st: &mut RunState, speed: f64) {
        let now_us = self.machine.systimer.now_us();
        if now_us < st.pace_next_us {
            return;
        }
        st.pace_next_us = now_us.saturating_add(PACE_QUANTUM_US);
        let modelled = (now_us.saturating_sub(st.pace_base_us) as f64 / speed) as u64;
        match Duration::from_micros(modelled).checked_sub(st.pace_base.elapsed()) {
            Some(lead) if !lead.is_zero() => {
                // Capped, so host input and a Ctrl-A x are still answered promptly.
                let nap = lead.min(PACE_MAX_SLEEP);
                std::thread::sleep(nap);
                st.slept += nap;
            }
            _ => {
                st.pace_base_us = now_us;
                st.pace_base = Instant::now();
            }
        }
    }

    /// Fast-forward a firmware busy-wait on the free-running counter, a `udelay`
    /// (`start = CLO; while (CLO - start) < n`): the firmware makes thousands of
    /// calls, each spinning a 3- or 4-instruction loop for the whole delay. The wait
    /// is recognised by its counter reads — same instruction, same control transfer
    /// before it, nothing else in between — and only one still going after 1000 of
    /// them is jumped, by as long as it has taken so far, up to 50 ms. A long delay
    /// clears in a few dozen doublings, overshooting by less than 2x, while a fixed
    /// jump would turn a train of `udelay(1)` calls into seconds. The ThreadX tick
    /// fires mid-delay and its handler reads peripherals itself, so once
    /// `in_exception` has moved, a few reads are passed over and the loop's next read
    /// need only match the instruction with a small `progress` delta.
    fn busy_wait_ff(&mut self, st: &mut RunState, pc: u32, had_output: bool, timer_read: bool) {
        /// Counter reads passed over after an exception: a thread switch out of the
        /// handler never comes back to the loop.
        const HANDLER_READS: u32 = 16;
        if self.cpu.in_exception != st.delay_ff_exc {
            st.delay_ff_exc = self.cpu.in_exception;
            st.delay_ff_irq = true;
        }
        if had_output {
            st.delay_ff = 0;
            st.delay_ff_pc = u32::MAX;
            return;
        }
        if !timer_read {
            return;
        }
        let m = &mut self.machine;
        let cf = self.cpu.cf_last.unwrap_or((u32::MAX, u32::MAX));
        let now = m.systimer.now_us();
        let p = progress_count(m);
        let small = p.wrapping_sub(st.progress_at_delay) < 4_096;
        let reads = m.mmio_reads.wrapping_sub(st.mmio_reads_at_delay);
        let timer_only = reads == m.systimer.clo_reads.wrapping_sub(st.clo_reads_at_delay);
        let spin = pc == st.delay_ff_pc
            && small
            && (st.delay_ff_irq || (cf == st.delay_ff_cf && timer_only));
        if spin {
            st.delay_ff += 1;
            if st.delay_ff >= 1_000 {
                // Jump rather than step, so one long `udelay` is not chopped at
                // every tick deadline.
                let waited = now.saturating_sub(st.delay_ff_start).clamp(1, 50_000);
                if crate::diag::ON {
                    crate::log!(m.log, Channel::Ff, "pc={pc:#x} jump={waited} us");
                }
                m.systimer.jump(waited);
                st.delay_ff = 0;
            }
        } else if st.delay_ff_irq && st.delay_ff_handler_reads < HANDLER_READS {
            // Likely the handler's own read: leave the run and its snapshots.
            st.delay_ff_handler_reads += 1;
            return;
        } else {
            st.delay_ff = 0;
            st.delay_ff_pc = pc;
            st.delay_ff_cf = cf;
            st.delay_ff_start = now;
        }
        st.progress_at_delay = p;
        st.mmio_reads_at_delay = m.mmio_reads;
        st.clo_reads_at_delay = m.systimer.clo_reads;
        st.delay_ff_irq = false;
        st.delay_ff_handler_reads = 0;
    }

    /// Core 1's step: one per core-0 step, over the shared bus.
    #[inline]
    fn step_core1(&mut self, st: &mut RunState) {
        if let Some(c1) = self.cpu1.as_mut() {
            if let Some(vbase) = self.machine.corectl.take_vbase(1) {
                c1.exc_vbase = vbase;
            }
            // Taken once core 1's bank enables it and core 1 can take it: with
            // interrupts on, or asleep in `sleep`, which takes one regardless.
            if c1.exc_vbase != 0 && !c1.is_stopped() && (c1.halted || c1.irq_enabled()) {
                if let Some(src) = self.machine.take_core1_irq() {
                    // Presented at `IRQ_PENDING` as it is vectored, as core 0's.
                    self.machine.corectl.raise_source(1, src);
                    if c1.halted {
                        c1.vector_irq_forced(&mut self.machine, src);
                    } else {
                        c1.vector_irq(&mut self.machine, src);
                    }
                }
            }
            if !c1.is_stopped() && !c1.halted {
                let pc_before = c1.pc();
                if crate::diag::ON {
                    self.machine.watch_pc = pc_before;
                    self.machine.watch_core = 1;
                }
                if let crate::vpu::Step::Stopped = c1.step(&mut self.machine) {
                    st.core1_end = Some(RunEnd::Core1Halted(
                        c1.stopped.clone().expect("stop reason"),
                    ));
                }
                // Drain here, or the next core-0 step prints these under its pc.
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

    /// The ARM: out of reset when `arm_loader` writes the ARM control block.
    #[inline]
    fn step_arm(&mut self) -> Option<RunEnd> {
        if self.arm_enabled && self.arm.is_none() && self.machine.armctrl.take_release() {
            self.arm = Some(crate::arm::ArmSide::released(&mut self.machine));
            self.machine.defer_sleep = true;
        }
        if let Some(arm) = self.arm.as_mut() {
            // A sleeping VPU wakes at its next compare or at an interrupting ARM
            // write, so the ARM runs first and the counter moves only as far as
            // it got (`arm/mod.rs`, "Time and scheduling").
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

    /// Run steps for as long as none of the run loop's per-step checks can act, doing
    /// only what every step must: core 0, the tick, core 1, the UART receiver, the
    /// ARM, and the detectors' running state.
    ///
    /// The guest sees exactly the run it would through [`Self::slow_step`] alone:
    /// same instructions, same step numbers, every interrupt and time jump at the
    /// same point. Only checks known to do nothing are skipped, each covered by a
    /// flag raised at the event it depends on ([`Machine::recheck`], [`Vpu::recheck`],
    /// the timer's `clo_reads`), by a budget for those that come due by counting, or
    /// by the pc for those tied to an address. A step raising a flag is finished by
    /// [`Self::post_step`] and followed by a slow step, so a check that runs before
    /// core 0's instruction sees it too. A queued interrupt held back only by the
    /// interrupt-enable bit is the one thing no flag covers, since any instruction
    /// can write `r30`, so it keeps the run on slow steps.
    fn fast_steps(&mut self, st: &mut RunState, limits: &RunLimits) -> Option<RunEnd> {
        if self.machine.recheck || self.cpu.is_stopped() {
            return None;
        }
        if self.cpu.exc_vbase != 0 && (self.machine.irq_queued() || self.machine.timer_irq_due()) {
            return None;
        }
        let budget = self.fast_budget(st, limits);
        if budget == 0 {
            return None;
        }
        // A `u32::MAX` that matches by accident only costs a slow step.
        let stop_pc = limits.stop_pc.unwrap_or(u32::MAX);
        let uart_busy = self.machine.console_rx_backlog() != 0;
        let host = self.input.host.is_some();
        let spin = limits.idle_spin_limit > 0;
        // Core 1 can stop or sleep in here, but only a slow step wakes it.
        let mut core1_runs = self
            .cpu1
            .as_ref()
            .is_some_and(|c| !c.is_stopped() && !c.halted);
        let arm_on = self.arm.is_some();
        self.cpu.recheck = false;
        let mut n = 0u64;
        let end = 'run: {
            while n < budget {
                let pc = self.cpu.pc();
                if pc == stop_pc {
                    break 'run None;
                }
                let step = self.cpu.step(&mut self.machine);
                n += 1;
                self.machine.tick(1);
                if self.machine.recheck | self.cpu.recheck {
                    break 'run self.post_step(st, limits, pc, step, Resume::Core0);
                }
                if core1_runs {
                    self.step_core1(st);
                    if self.machine.recheck {
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
                    self.machine.console_pump(self.machine.systimer.now_us());
                }
                if arm_on {
                    if let Some(end) = self.step_arm() {
                        break 'run Some(end);
                    }
                    if self.machine.recheck {
                        break 'run self.post_step(st, limits, pc, step, Resume::Arm);
                    }
                }
                if spin {
                    if self.machine.systimer.clo_reads != st.clo_reads_at_cf {
                        break 'run self.post_step(st, limits, pc, step, Resume::Arm);
                    }
                    if let Some(cf) = self.cpu.cf_last {
                        st.cf_repeat = 0;
                        st.last_cf = cf;
                        st.progress_at_cf = progress_count(&self.machine);
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

    /// How many steps [`Self::fast_steps`] may take before a counted check fires.
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
        if limits.speed.is_some() {
            n = n.min(
                self.machine
                    .systimer
                    .cycles_until(st.pace_next_us)
                    .saturating_sub(1),
            );
        }
        if limits.silent_us > 0 {
            // In the `k`-th step from here core 0 has retired at most `k` more
            // instructions and the counter has had exactly `k` more cycles.
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

/// How much modelled time passes between two real-time pacing checks.
const PACE_QUANTUM_US: u64 = 1_000;

/// The longest one pacing sleep lasts; a longer lead takes several.
const PACE_MAX_SLEEP: Duration = Duration::from_millis(20);

/// Instructions the console-silence watchdog wants on top of its modelled time.
const SILENT_RETIRED: u64 = 20_000_000;

/// RAM stores + peripheral stores + RAM loads: a memset advances the stores and a
/// memtest read-back the loads, while a poll our stubs never satisfy moves neither.
fn progress_count(m: &Machine) -> u64 {
    m.ram_writes
        .wrapping_add(m.mmio_writes)
        .wrapping_add(m.ram_reads)
}

/// How far a step got before [`Emulator::fast_steps`] handed it to `post_step`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Resume {
    Core0,
    Core1,
    Arm,
}

/// What [`Emulator::run`] carries from one step to the next.
struct RunState {
    start: Instant,
    diag: crate::diag::DiagConfig,
    console: Vec<u8>,
    wall_check: u64,
    console_seen: usize,
    /// How much of the console the prompt / `until` search has covered, and where the output after the last scripted send begins.
    prompt_seen: usize,
    prompt_floor: usize,
    host_poll: u32,
    /// Real-time pacing: the modelled clock and the host clock at the baseline, the
    /// modelled time of the next check, and how long the run has slept so far.
    pace_base_us: u64,
    pace_base: Instant,
    pace_next_us: u64,
    slept: Duration,

    // Spin detection: a PC inside a small range for a whole window, no output.
    win: u64,
    // The modelled clock and retired count at the last byte printed.
    last_output_us: u64,
    last_output_retired: u64,
    w_lo: u32,
    w_hi: u32,
    w_steps: u64,
    w_output: bool,
    progress_at_window: u64,
    clo_reads_at_window: u64,
    // The same taken transfer repeating is a tight spin, unless memory moves.
    last_cf: (u32, u32),
    cf_repeat: u64,
    progress_at_cf: u64,
    clo_reads_at_cf: u64,
    delay_ff: u64,
    delay_ff_pc: u32,
    delay_ff_cf: (u32, u32),
    /// The counter when the run of reads started: how long the wait has taken.
    delay_ff_start: u64,
    /// `in_exception` when last looked at, and whether it has moved since.
    delay_ff_exc: u32,
    delay_ff_irq: bool,
    delay_ff_handler_reads: u32,
    progress_at_delay: u64,
    mmio_reads_at_delay: u64,
    clo_reads_at_delay: u64,

    tick_deliveries: u64,
    tick_skips: u64,
    /// `PIMU_PROF=1`: 256-byte PC buckets, hottest dumped on exit.
    prof_hist: HashMap<u32, u64>,
    /// `PIMU_PROF_THREAD=1`: the same buckets keyed by ThreadX thread as well.
    prof_thist: HashMap<(u32, u32), u64>,
    /// `PIMU_HEARTBEAT=<n>`: every `<n>` retired instructions print model time, the
    /// running thread and the PC — what says whether a stall is wedged or merely slow.
    next_beat: u64,
    /// `PIMU_TRAP=<hex>[,<hex>...]`: print pc / lr / r0-r7 whenever core 0 reaches one
    /// of these addresses, since the linear disassembler cannot xref. `PIMU_TRAP_MAX`
    /// caps the prints per address (totals at exit); `PIMU_TRAP_FROM` arms it after
    /// so many million retired instructions.
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
            pace_base_us: m.systimer.now_us(),
            pace_base: start,
            pace_next_us: m.systimer.now_us(),
            slept: Duration::ZERO,
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
            delay_ff_pc: u32::MAX,
            delay_ff_cf: (u32::MAX, u32::MAX),
            delay_ff_start: 0,
            delay_ff_exc: emu.cpu.in_exception,
            delay_ff_irq: false,
            delay_ff_handler_reads: 0,
            progress_at_delay: progress_count(m),
            mmio_reads_at_delay: m.mmio_reads,
            clo_reads_at_delay: m.systimer.clo_reads,
            tick_deliveries: 0,
            tick_skips: 0,
            prof_hist: HashMap::new(),
            prof_thist: HashMap::new(),
            trap_hits: HashMap::new(),
            core1_end: None,
        }
    }
}
