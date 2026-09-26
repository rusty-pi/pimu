//! The run loop's fast path (`Emulator::fast_steps`) held to the slow one: each
//! test runs one payload both ways and requires the same instructions retired,
//! registers, console bytes and modelled time. The payloads cross every edge the
//! fast path hands back at — timer interrupt, `ei`/`di`, `sleep`, counter reads,
//! console writes from both cores. The busy-wait fast-forward is held to the same
//! rule and to what it is for: a long `udelay` is jumped, a train of short ones
//! or a bare timestamp loop is not.

use std::time::Duration;

use pimu::bus::Bus;
use pimu::emulator::{Emulator, RunEnd, RunLimits, RunReport};
use pimu::soc::bcm2711::{CORECTL_BASE, SYSTIMER_BASE, UART0_BASE};
use pimu::vpu::UnimplPolicy;
use pimu::Machine;

const CODE: u32 = 0x1000;
const VBASE: u32 = 0x2000;
const HANDLER: u32 = 0x3000;
const CORE1: u32 = 0x4000;
const STACK_TOP: u32 = 0x8000;
/// The system-timer compare 0 interrupt: source 64, its own vector slot.
const TICK_SLOT: u32 = 64;

const NE: u16 = 0x1;
const AL: u16 = 0xE;
const NOP: u16 = 0x0001;
const SLEEP: u16 = 0x0002;
const EI: u16 = 0x0004;
const DI: u16 = 0x0005;
const RTI: u16 = 0x000A;

const fn ld(rd: u16, rs: u16) -> u16 {
    0x0800 | (rs << 4) | rd
}
const fn st(rd: u16, rs: u16) -> u16 {
    0x0900 | (rs << 4) | rd
}
const fn mov5(rd: u16, u: u16) -> u16 {
    0x6000 | (u << 4) | rd
}
const fn add5(rd: u16, u: u16) -> u16 {
    0x6200 | (u << 4) | rd
}
const fn cmp(rd: u16, rs: u16) -> u16 {
    0x4A00 | (rs << 4) | rd
}
const fn add(rd: u16, rs: u16) -> u16 {
    0x4200 | (rs << 4) | rd
}
const fn sub(rd: u16, rs: u16) -> u16 {
    0x4600 | (rs << 4) | rd
}

struct Asm {
    at: u32,
    code: Vec<u16>,
}

impl Asm {
    fn new(at: u32) -> Asm {
        Asm {
            at,
            code: Vec::new(),
        }
    }

    fn pc(&self) -> u32 {
        self.at + 2 * self.code.len() as u32
    }

    fn op(&mut self, h: u16) {
        self.code.push(h);
    }

    fn mov32(&mut self, rd: u16, v: u32) {
        self.code.extend([0xE800 | rd, v as u16, (v >> 16) as u16]);
    }

    fn b(&mut self, cond: u16, to: u32) {
        let halfwords = (to.wrapping_sub(self.pc()) as i32) / 2;
        assert!((-64..64).contains(&halfwords), "branch out of range");
        self.op(0x1800 | (cond << 7) | (halfwords as u16 & 0x7F));
    }

    fn load(&self, m: &mut Machine) {
        for (i, h) in self.code.iter().enumerate() {
            m.store16(self.at + 2 * i as u32, *h).unwrap();
        }
    }
}

/// Core 0: 40 outer turns of a 200-step loop, each printing `.`, toggling
/// interrupts, reading the counter and sleeping. Returns the done pc.
fn core0(m: &mut Machine) -> u32 {
    let mut a = Asm::new(CODE);
    a.mov32(4, SYSTIMER_BASE + 0x04); // CLO
    a.mov32(5, SYSTIMER_BASE + 0x0C); // C0
    a.mov32(6, UART0_BASE); // DR
    a.mov32(7, u32::from(b'.'));
    a.mov32(8, SYSTIMER_BASE); // CS
    a.op(mov5(9, 1));
    a.mov32(10, 200);
    a.mov32(11, 40);
    a.op(ld(3, 4));
    a.op(add5(3, 30));
    a.op(st(3, 5));
    a.op(EI);
    let outer = a.pc();
    a.op(mov5(1, 0));
    let inner = a.pc();
    a.op(add5(1, 1));
    a.op(cmp(1, 10));
    a.b(NE, inner);
    a.op(st(7, 6));
    a.op(DI);
    a.op(add5(0, 1));
    a.op(EI);
    a.op(ld(3, 4));
    a.op(SLEEP);
    a.op(cmp(0, 11));
    a.b(NE, outer);
    let done = a.pc();
    a.op(NOP);
    a.load(m);
    done
}

fn handler(m: &mut Machine) {
    let mut h = Asm::new(HANDLER);
    h.op(st(9, 8));
    h.op(ld(12, 4));
    h.op(add5(12, 20));
    h.op(st(12, 5));
    h.mov32(13, u32::from(b'!'));
    h.op(st(13, 6));
    h.op(RTI);
    h.load(m);
    m.store32(VBASE + 4 * TICK_SLOT, HANDLER).unwrap();
    // `enable_irq_source(64, 1)`: the controller takes no source whose field
    // is 0.
    m.store32(CORECTL_BASE + 0x10, 1).unwrap();
}

/// Core 1 prints `#` every 333 steps, for ever.
fn core1(m: &mut Machine) {
    let mut c = Asm::new(CORE1);
    c.op(mov5(1, 0));
    let top = c.pc();
    c.op(add5(1, 1));
    c.op(cmp(1, 10));
    c.b(NE, top);
    c.op(st(7, 6));
    c.op(mov5(1, 0));
    c.b(AL, top);
    c.load(m);
}

fn emulator(fast: bool) -> (Emulator, u32) {
    let mut m = Machine::new(1 << 20);
    let done = core0(&mut m);
    handler(&mut m);
    core1(&mut m);
    let mut emu = Emulator::new(m, CODE);
    emu.fast_loop = fast;
    emu.set_unimpl_policy(UnimplPolicy::ReconFault);
    emu.cpu.exc_vbase = VBASE;
    emu.cpu.regs.set(25, STACK_TOP);
    emu.start_smp(CORE1);
    let c1 = emu.cpu1.as_mut().unwrap();
    c1.regs.set(6, UART0_BASE);
    c1.regs.set(7, u32::from(b'#'));
    c1.regs.set(10, 333);
    (emu, done)
}

fn limits(done: u32, max_steps: u64) -> RunLimits {
    RunLimits {
        max_steps: Some(max_steps),
        max_wall: Some(Duration::from_secs(60)),
        stop_pc: Some(done),
        idle_spin_limit: 2_000,
        // Armed, so its budget is in play, but 20M silent instructions away.
        silent_us: 1,
        until: None,
        speed: None,
    }
}

/// Run the payload both ways; return both reports and emulators.
fn both(max_steps: u64) -> [(RunReport, Emulator); 2] {
    [true, false].map(|fast| {
        let (mut emu, done) = emulator(fast);
        let report = emu.run(&limits(done, max_steps));
        (report, emu)
    })
}

fn assert_same(runs: &[(RunReport, Emulator); 2], what: &str) {
    let [(a, ea), (b, eb)] = runs;
    assert_eq!(a.end, b.end, "{what}: end");
    assert_eq!(a.retired, b.retired, "{what}: core 0 retired");
    assert_eq!(a.cycles, b.cycles, "{what}: cycles");
    assert_eq!(a.pc, b.pc, "{what}: pc");
    assert_eq!(a.regs, b.regs, "{what}: registers");
    assert_eq!(a.skipped, b.skipped, "{what}: skipped");
    assert_eq!(a.core1_pc, b.core1_pc, "{what}: core 1 pc");
    assert_eq!(a.core1_retired, b.core1_retired, "{what}: core 1 retired");
    assert_eq!(
        String::from_utf8_lossy(&a.console),
        String::from_utf8_lossy(&b.console),
        "{what}: console"
    );
    let (ma, mb) = (&ea.machine, &eb.machine);
    assert_eq!(ma.systimer.now_us(), mb.systimer.now_us(), "{what}: time");
    assert_eq!(
        ma.systimer.clo_reads, mb.systimer.clo_reads,
        "{what}: CLO reads"
    );
    assert_eq!(ma.ram_reads, mb.ram_reads, "{what}: RAM reads");
    assert_eq!(ma.ram_writes, mb.ram_writes, "{what}: stores");
    assert_eq!(ma.mmio_writes, mb.mmio_writes, "{what}: MMIO writes");
}

#[test]
fn fast_and_slow_steps_make_the_same_run() {
    let runs = both(10_000_000);
    assert_same(&runs, "to the end");
    let (report, emu) = &runs[0];
    assert!(
        matches!(report.end, RunEnd::StopPc(_)),
        "the payload runs to its end: {:?}",
        report.end
    );
    let console = String::from_utf8_lossy(&report.console);
    for c in ['.', '!', '#'] {
        assert!(console.contains(c), "no {c:?} in {console:?}");
    }
    assert_eq!(console.matches('.').count(), 40);
    assert!(
        emu.fast_stepped > report.retired / 2,
        "only {} of {} steps were fast",
        emu.fast_stepped,
        report.retired
    );
    assert_eq!(runs[1].1.fast_stepped, 0);
}

/// The step limit is counted, not flagged: both paths stop on the same step.
#[test]
fn the_step_limit_lands_on_the_same_step() {
    let (full, _) = &both(10_000_000)[1];
    let total = full.retired + full.core1_retired.unwrap_or(0);
    for max in [
        1,
        2,
        3,
        57,
        1_000,
        4_321,
        total / 3,
        total / 2 + 1,
        total - 1,
    ] {
        let runs = both(max);
        assert_same(&runs, &format!("max_steps {max}"));
        assert_eq!(runs[0].0.end, RunEnd::StepLimit, "max_steps {max}");
    }
}

const CLO: u32 = SYSTIMER_BASE + 0x04;
const CYCLES_PER_US: u64 = 54;
const HI: u16 = 0x8;

/// A firmware `udelay(r1)` as the bootloaders spell it:
/// `r3 = CLO; do r2 = CLO - r3; while (r1 > r2)`, `r4` holding `CLO`.
fn udelay(a: &mut Asm) {
    a.op(ld(3, 4));
    let spin = a.pc();
    a.op(ld(2, 4));
    a.op(sub(2, 3));
    a.op(cmp(1, 2));
    a.b(HI, spin);
}

/// Load `code` at [`CODE`] and run it to `done` with the detectors on.
fn run_payload(code: impl Fn(&mut Machine) -> u32, fast: bool) -> (RunReport, Emulator) {
    let mut m = Machine::new(1 << 20);
    let done = code(&mut m);
    let mut emu = Emulator::new(m, CODE);
    emu.fast_loop = fast;
    emu.cpu.exc_vbase = VBASE;
    emu.cpu.regs.set(25, STACK_TOP);
    let report = emu.run(&RunLimits {
        max_steps: Some(20_000_000),
        max_wall: Some(Duration::from_secs(60)),
        stop_pc: Some(done),
        idle_spin_limit: 200_000,
        silent_us: 0,
        until: None,
        speed: None,
    });
    assert!(
        matches!(report.end, RunEnd::StopPc(_)),
        "the payload runs to its end: {:?}",
        report.end
    );
    (report, emu)
}

fn both_payload(code: impl Fn(&mut Machine) -> u32 + Copy, what: &str) -> (RunReport, Emulator) {
    let runs = [true, false].map(|fast| run_payload(code, fast));
    assert_same(&runs, what);
    let [fast, _] = runs;
    fast
}

/// A 20 ms `udelay`, with a 1 ms tick counting itself at `TICKS` when asked.
fn long_wait(m: &mut Machine, tick: bool) -> u32 {
    let mut a = Asm::new(CODE);
    a.mov32(4, CLO);
    a.mov32(1, 20_000);
    if tick {
        a.mov32(5, SYSTIMER_BASE + 0x0C); // C0
        a.mov32(8, SYSTIMER_BASE); // CS
        a.op(mov5(9, 1));
        a.mov32(11, TICKS);
        a.mov32(10, 1_000);
        a.op(ld(3, 4));
        a.op(add(3, 10));
        a.op(st(3, 5));
        a.op(EI);
        let mut h = Asm::new(HANDLER);
        h.op(st(9, 8));
        h.op(ld(12, 4));
        h.op(add(12, 10));
        h.op(st(12, 5));
        h.op(ld(13, 11));
        h.op(add5(13, 1));
        h.op(st(13, 11));
        h.op(RTI);
        h.load(m);
        m.store32(VBASE + 4 * TICK_SLOT, HANDLER).unwrap();
        m.store32(CORECTL_BASE + 0x10, 1).unwrap();
    }
    udelay(&mut a);
    let done = a.pc();
    a.op(NOP);
    a.load(m);
    done
}
const TICKS: u32 = 0x6000;

#[test]
fn a_long_wait_is_jumped_by_about_its_length() {
    for tick in [false, true] {
        let (report, mut emu) = both_payload(|m| long_wait(m, tick), &format!("tick {tick}"));
        let now = emu.machine.systimer.now_us();
        // Each jump is as long as the wait so far, so it ends inside 2x.
        assert!(
            (20_000..41_000).contains(&now),
            "tick {tick}: a 20 ms wait took {now} us"
        );
        // Unjumped, the wait spins 20 ms worth of cycles.
        let spun = 20_000 * CYCLES_PER_US;
        assert!(
            report.retired < spun / 10,
            "tick {tick}: {} instructions for a wait of {spun}",
            report.retired
        );
        if tick {
            let ticks = emu.machine.load32(TICKS).unwrap();
            assert!(ticks > 0, "the tick never fired");
        }
    }
}

/// [`long_wait`] with real-time pacing set to `speed`.
fn paced(speed: Option<f64>) -> RunReport {
    let mut m = Machine::new(1 << 20);
    let done = long_wait(&mut m, false);
    let mut emu = Emulator::new(m, CODE);
    emu.cpu.exc_vbase = VBASE;
    emu.cpu.regs.set(25, STACK_TOP);
    let report = emu.run(&RunLimits {
        stop_pc: Some(done),
        speed,
        ..limits(done, 20_000_000)
    });
    assert!(
        matches!(report.end, RunEnd::StopPc(_)),
        "the payload runs to its end: {:?}",
        report.end
    );
    report
}

/// The 20 ms wait is jumped in a few thousand instructions, so the modelled clock
/// ends far ahead of the host's: paced, the run has to sleep that lead off, and what
/// the guest sees is the same either way.
#[test]
fn a_paced_run_sleeps_off_its_lead() {
    let free = paced(None);
    assert_eq!(free.slept, Duration::ZERO, "--speed max never sleeps");
    let paced = paced(Some(1.0));
    assert!(
        paced.slept >= Duration::from_millis(15),
        "a 20 ms lead slept off in {:?}",
        paced.slept
    );
    assert!(
        paced.wall >= paced.slept,
        "wall {:?} against {:?} asleep",
        paced.wall,
        paced.slept
    );
    assert_eq!(paced.retired, free.retired, "pacing changed the run");
    assert_eq!(paced.pc, free.pc);
}

/// `udelay(4)` 2000 times, a sampling loop's peripheral read after each if asked.
fn short_waits(m: &mut Machine, poll: bool) -> u32 {
    let mut a = Asm::new(CODE);
    a.mov32(4, CLO);
    a.mov32(5, UART0_BASE + 0x18); // FR
    a.op(mov5(1, 4));
    a.mov32(0, 2_000);
    a.op(mov5(8, 0));
    a.op(mov5(9, 1));
    let call = a.pc();
    udelay(&mut a);
    if poll {
        a.op(ld(6, 5));
    }
    a.op(sub(0, 9));
    a.op(cmp(0, 8));
    a.b(NE, call);
    let done = a.pc();
    a.op(NOP);
    a.load(m);
    done
}

/// 5000 turns reading the counter and a peripheral: a timestamp, not a wait.
fn timestamps(m: &mut Machine) -> u32 {
    let mut a = Asm::new(CODE);
    a.mov32(4, CLO);
    a.mov32(5, UART0_BASE + 0x18); // FR
    a.mov32(0, 5_000);
    a.op(mov5(8, 0));
    a.op(mov5(9, 1));
    let top = a.pc();
    a.op(ld(2, 4));
    a.op(ld(6, 5));
    a.op(sub(0, 9));
    a.op(cmp(0, 8));
    a.b(NE, top);
    let done = a.pc();
    a.op(NOP);
    a.load(m);
    done
}

type Payload = fn(&mut Machine) -> u32;

#[test]
fn short_waits_and_timestamps_are_not_jumped() {
    let runs: [(&str, Payload); 3] = [
        ("short waits", |m| short_waits(m, false)),
        ("short waits and polls", |m| short_waits(m, true)),
        ("timestamps", timestamps),
    ];
    for (what, code) in runs {
        let (report, emu) = both_payload(code, what);
        // Unjumped, the counter shows exactly the cycles that ran.
        assert_eq!(
            emu.machine.systimer.now_us(),
            report.cycles / CYCLES_PER_US,
            "{what}: the counter was jumped"
        );
    }
}
