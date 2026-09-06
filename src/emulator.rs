//! Top-level emulator: owns the [`Vpu`] and the [`Machine`] as siblings and
//! drives the run loop.

use std::time::{Duration, Instant};

use crate::machine::{Console, Machine};
use crate::vpu::{Stop, UnimplPolicy, Vpu};

pub struct Emulator {
    pub cpu: Vpu,
    /// VPU core 1. `None` until `start4.elf`'s trampoline releases it by writing
    /// a start vector to the core-control block; then the run loop interleaves
    /// it with core 0 over the shared bus.
    pub cpu1: Option<Vpu>,
    /// Override for core 1's reset PC. `None` → core 1 starts at the address the
    /// firmware wrote to the core-control block (`0x7E00_2030`).
    pub core1_entry: Option<u32>,
    pub machine: Machine,
}

/// Stopping conditions for [`Emulator::run`].
#[derive(Debug, Clone)]
pub struct RunLimits {
    /// Hard cap on retired instructions. Always set something.
    pub max_steps: u64,
    /// Optional wall-clock cap.
    pub max_wall: Option<Duration>,
    /// Stop cleanly when the PC reaches this address (e.g. an ARM-handoff stub).
    pub stop_pc: Option<u32>,
    /// Stop if the PC revisits the same address this many steps in a row with no
    /// console output (tight spin / wfi-style wait). 0 disables.
    pub idle_spin_limit: u64,
}

impl Default for RunLimits {
    fn default() -> Self {
        RunLimits {
            max_steps: 5_000_000,
            max_wall: Some(Duration::from_secs(30)),
            stop_pc: None,
            idle_spin_limit: 0,
        }
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
    /// VPU core 1 halted (swi/sleep/breakpoint/fault). Core 0 may still have
    /// been running; check the report's `pc` and `core1_pc`.
    Core1Halted(Stop),
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
    /// Distinct unimplemented instructions encountered (reconnaissance).
    pub unimpl: Vec<crate::vpu::exec::UnimplHit>,
    /// Final register file (r0..r31) of core 0.
    pub regs: [u32; 32],
    /// Core 1 state, once it was released.
    pub core1_pc: Option<u32>,
    pub core1_retired: Option<u64>,
    pub core1_end: Option<RunEnd>,
}

impl Emulator {
    pub fn new(machine: Machine, entry: u32) -> Emulator {
        Emulator {
            cpu: Vpu::new(entry),
            cpu1: None,
            core1_entry: None,
            machine,
        }
    }

    /// Release VPU core 1 at `vec`, inheriting core 0's unimpl policy. Its
    /// exception-vector base is the start vector itself (that is what the
    /// trampoline hands the hardware).
    fn spawn_core1(&mut self, vec: u32) {
        // The firmware writes core 1's *vector base* to the core-control block;
        // the core itself resets to the shared ELF entry (the same trampoline
        // core 0 ran) and branches on `version` bit 16 from there.
        let entry = self.core1_entry.unwrap_or(vec);
        let mut c1 = Vpu::new(entry);
        c1.core_id = 1;
        c1.on_unimpl = self.cpu.on_unimpl;
        c1.exc_vbase = vec;
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

        // Spin detection: over a sliding window of steps, track the min/max PC
        // and whether any console output happened. If the PC stays within a
        // small window for the whole window length with no output, call it a
        // spin (a peripheral poll our stubs never satisfy).
        let win = limits.idle_spin_limit.max(1);
        let mut w_lo = u32::MAX;
        let mut w_hi = 0u32;
        let mut w_steps = 0u64;
        let mut w_output = false;
        let mut writes_at_window = self.machine.ram_writes;
        // Fast path: the exact same taken transfer repeating is a tight spin —
        // *unless* memory writes keep advancing (that's a memset/memcpy loop).
        let mut last_cf = (u32::MAX, u32::MAX);
        let mut cf_repeat = 0u64;
        let mut writes_at_cf = self.machine.ram_writes;

        let mut core1_end: Option<RunEnd> = None;
        let end = loop {
            if self.cpu.retired + self.cpu1.as_ref().map_or(0, |c| c.retired) >= limits.max_steps {
                break RunEnd::StepLimit;
            }
            if let Some(pc) = limits.stop_pc {
                if self.cpu.pc() == pc {
                    break RunEnd::StopPc(pc);
                }
            }

            let pc_before = self.cpu.pc();
            let step = self.cpu.step(&mut self.machine);
            self.machine.tick(1);

            // The trampoline (running on core 0) may have just set core 1's
            // vector base. Only actually bring core 1 up once we know where it
            // should reset to (`core1_entry` — its semantics are still being
            // reverse-engineered, so it is opt-in).
            if self.cpu1.is_none() && self.core1_entry.is_some() {
                if let Some(vec) = self.machine.corectl.take_core1_start() {
                    self.spawn_core1(vec);
                }
            }
            // Interleave one core-1 step per core-0 step over the shared bus.
            if let Some(c1) = self.cpu1.as_mut() {
                if !c1.is_stopped() {
                    if let crate::vpu::Step::Stopped = c1.step(&mut self.machine) {
                        core1_end =
                            Some(RunEnd::Core1Halted(c1.stopped.clone().expect("stop reason")));
                    }
                }
            }

            let fresh = self.machine.take_console_output();
            let had_output = !fresh.is_empty();
            console.extend_from_slice(&fresh);

            match step {
                crate::vpu::Step::Stopped => {
                    let stop = self.cpu.stopped.clone().expect("stopped without reason");
                    break RunEnd::Halted(stop);
                }
                crate::vpu::Step::Ran => {}
            }
            // Core 1 halting does not stop core 0 — record it and carry on.

            if limits.idle_spin_limit > 0 {
                if let Some(&cf) = self.cpu.cf_trace.last() {
                    let progressing = self.machine.ram_writes != writes_at_cf;
                    if cf == last_cf && !had_output && !progressing {
                        cf_repeat += 1;
                        if cf_repeat >= 4000 {
                            break RunEnd::IdleSpin(cf.0);
                        }
                    } else {
                        cf_repeat = 0;
                        last_cf = cf;
                        writes_at_cf = self.machine.ram_writes;
                    }
                }

                w_lo = w_lo.min(pc_before);
                w_hi = w_hi.max(pc_before);
                w_output |= had_output;
                w_steps += 1;
                if w_steps >= win {
                    let stalled = self.machine.ram_writes == writes_at_window;
                    if !w_output && stalled && w_hi.wrapping_sub(w_lo) <= 4096 {
                        break RunEnd::IdleSpin(w_lo);
                    }
                    w_lo = u32::MAX;
                    w_hi = 0;
                    w_steps = 0;
                    w_output = false;
                    writes_at_window = self.machine.ram_writes;
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
            unimpl,
            regs: std::array::from_fn(|i| self.cpu.regs.get(i)),
            core1_pc: self.cpu1.as_ref().map(|c| c.pc()),
            core1_retired: self.cpu1.as_ref().map(|c| c.retired),
            core1_end,
        }
    }
}
