//! Interrupts the firmware raises in software for VPU core 1. Core 0 sets the
//! source's bit in core 1's CoreCtl pending word; core 1 takes it once its own
//! bank enables the source and it can take one — interrupts on, or asleep in
//! `sleep`, which takes one even with them off. Until then it stays pending and
//! is never dropped. Each case runs both run loops, which must make one run.

use pimu::bus::Bus;
use pimu::emulator::{Emulator, RunEnd, RunLimits, RunReport};
use pimu::soc::bcm2711::{CORECTL_BASE, UART0_BASE};
use pimu::spec::corectl::{
    INSTANCE_STRIDE, IRQ_PENDING, IRQ_PENDING_BITS, IRQ_PRIO, IRQ_PRIO_STRIDE,
};
use pimu::vpu::UnimplPolicy;
use pimu::Machine;

const CODE: u32 = 0x1000;
const VBASE1: u32 = 0x2000;
const HANDLER: u32 = 0x3000;
const CORE1: u32 = 0x4000;
const SEEN: u32 = 0x6000;
const STACK1: u32 = 0x8000;
const SRC: u32 = 79;
/// Core 1's `IRQ_PENDING`: source and priority in both half-words, then 0 on the
/// read-to-clear second read. A Raspberry Pi 4B d03115, read from inside a
/// handler, answers `0x01470147` for source 71 at priority 1.
const SEEN_HALF: u32 = 0x100 | SRC;
const SEEN_OK: [u32; 2] = [SEEN_HALF | (SEEN_HALF << 16), 0];

const NE: u16 = 0x1;
const AL: u16 = 0xE;
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

    fn poke(&mut self, addr: u32, v: u32) {
        self.mov32(2, addr);
        self.mov32(3, v);
        self.op(st(3, 2));
    }

    /// Print `c`; r7 is clobbered.
    fn putc(&mut self, c: u8) {
        self.mov32(7, u32::from(c));
        self.op(st(7, 6));
    }

    fn wait(&mut self, n: u32) {
        self.mov32(10, n);
        self.op(mov5(1, 0));
        let top = self.pc();
        self.op(add5(1, 1));
        self.op(cmp(1, 10));
        self.b(NE, top);
    }

    fn spin(&mut self) {
        let here = self.pc();
        self.b(AL, here);
    }

    fn load(&self, m: &mut Machine) {
        for (i, h) in self.code.iter().enumerate() {
            m.store16(self.at + 2 * i as u32, *h).unwrap();
        }
    }
}

fn enable(a: &mut Asm) {
    let word = CORECTL_BASE + INSTANCE_STRIDE + IRQ_PRIO + ((SRC >> 3) & 3) * IRQ_PRIO_STRIDE;
    a.poke(word, 1 << ((SRC & 7) * 4));
}

fn raise(a: &mut Asm) {
    a.poke(
        CORECTL_BASE + INSTANCE_STRIDE + IRQ_PENDING_BITS,
        1 << (SRC - 64),
    );
}

/// Run both cores until the console shows `until`, fast or slow. Core 1's vector
/// for `SRC` reads `IRQ_PENDING` twice into `SEEN`, then prints `!`.
fn run(core0: &Asm, core1: &Asm, until: &str, fast: bool) -> (RunReport, [u32; 2]) {
    let mut m = Machine::new(1 << 20);
    core0.load(&mut m);
    core1.load(&mut m);
    let mut h = Asm::new(HANDLER);
    h.mov32(2, CORECTL_BASE + INSTANCE_STRIDE + IRQ_PENDING);
    h.mov32(4, SEEN);
    h.mov32(5, SEEN + 4);
    h.op(ld(3, 2));
    h.op(st(3, 4));
    h.op(ld(3, 2));
    h.op(st(3, 5));
    h.putc(b'!');
    h.op(RTI);
    h.load(&mut m);
    m.store32(VBASE1 + 4 * SRC, HANDLER).unwrap();
    m.store32(SEEN, 0xDEAD_BEEF).unwrap();
    m.store32(SEEN + 4, 0xDEAD_BEEF).unwrap();
    let mut emu = Emulator::new(m, CODE);
    emu.fast_loop = fast;
    emu.set_unimpl_policy(UnimplPolicy::ReconFault);
    emu.cpu.regs.set(6, UART0_BASE);
    emu.start_smp(CORE1);
    let c1 = emu.cpu1.as_mut().unwrap();
    c1.exc_vbase = VBASE1;
    c1.regs.set(25, STACK1);
    c1.regs.set(6, UART0_BASE);
    let report = emu.run(&RunLimits {
        max_steps: Some(50_000),
        until: Some(until.into()),
        ..RunLimits::default()
    });
    let seen = [SEEN, SEEN + 4].map(|a| emu.machine.load32(a).unwrap());
    (report, seen)
}

/// Run both ways, require the same run, and return what the handler read.
fn both(core0: &Asm, core1: &Asm, until: &str) -> (String, [u32; 2]) {
    let (fast, fast_seen) = run(core0, core1, until, true);
    let (slow, slow_seen) = run(core0, core1, until, false);
    let console = String::from_utf8_lossy(&fast.console).into_owned();
    assert_eq!(
        console,
        String::from_utf8_lossy(&slow.console),
        "console, fast vs slow"
    );
    assert_eq!(fast.retired, slow.retired, "core 0 retired, fast vs slow");
    assert_eq!(
        fast.core1_retired, slow.core1_retired,
        "core 1 retired, fast vs slow"
    );
    assert_eq!(fast_seen, slow_seen, "IRQ_PENDING, fast vs slow");
    assert_eq!(
        fast.end,
        RunEnd::Until,
        "never printed {until:?}: {console:?}"
    );
    (console, fast_seen)
}

/// Raised while core 1 has interrupts off: taken right after its `ei`.
#[test]
fn a_source_raised_with_interrupts_off_waits_for_ei() {
    let mut c0 = Asm::new(CODE);
    c0.putc(b'R');
    enable(&mut c0);
    raise(&mut c0);
    c0.spin();
    let mut c1 = Asm::new(CORE1);
    c1.op(DI);
    c1.putc(b'a');
    c1.wait(100);
    c1.putc(b'b');
    c1.op(EI);
    c1.spin();
    let (console, seen) = both(&c0, &c1, "!");
    assert_eq!(console, "Rab!");
    assert_eq!(seen, SEEN_OK);
}

/// Raised while core 1 sleeps with interrupts off: `sleep` takes it anyway.
#[test]
fn a_source_raised_during_sleep_wakes_core_1() {
    let mut c0 = Asm::new(CODE);
    c0.wait(50);
    c0.putc(b'R');
    enable(&mut c0);
    raise(&mut c0);
    c0.spin();
    let mut c1 = Asm::new(CORE1);
    c1.op(DI);
    c1.putc(b'a');
    c1.op(SLEEP);
    c1.putc(b'w');
    c1.spin();
    let (console, seen) = both(&c0, &c1, "w");
    assert_eq!(console, "aR!w");
    assert_eq!(seen, SEEN_OK);
}

/// Raised before core 1's bank enables the source: taken once it does.
#[test]
fn a_source_waits_for_its_enable_in_core_1s_bank() {
    let mut c0 = Asm::new(CODE);
    c0.putc(b'R');
    raise(&mut c0);
    c0.wait(50);
    c0.putc(b'E');
    enable(&mut c0);
    c0.spin();
    let mut c1 = Asm::new(CORE1);
    c1.spin();
    let (console, seen) = both(&c0, &c1, "!");
    assert_eq!(console, "RE!");
    assert_eq!(seen, SEEN_OK);
}
