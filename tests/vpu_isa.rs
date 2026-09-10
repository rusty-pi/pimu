//! Instruction-level regression tests for the VideoCore IV core.
//!
//! Every test here pins down a decode or execute detail that has already been
//! wrong once and cost a debugging session. They run a handful of instructions
//! against a bare [`Machine`] — no firmware blob, no boot, microseconds each.

use rpi_virt_fw::bus::Bus;
use rpi_virt_fw::vpu::{Step, Vpu};
use rpi_virt_fw::Machine;

const CODE: u32 = 0x0000_1000;
const STACK_TOP: u32 = 0x0000_8000;

/// 16-bit `ldm`/`stm` encoding: `0000 001L Sbb nnnnn`, where `L` = include
/// `lr`/`pc`, `S` = store, `bb` = register bank (r0 / r6 / r16 / r24) and
/// `nnnnn` = count - 1. See `decode.rs`.
const fn ldm_stm(store: bool, with_ret: bool, bank: u16, count: u16) -> u16 {
    0x0200 | ((with_ret as u16) << 8) | ((store as u16) << 7) | (bank << 5) | (count - 1)
}

const PUSH_R0_R5_LR: u16 = ldm_stm(true, true, 0, 6);
const PUSH_R6_R15: u16 = ldm_stm(true, false, 1, 10);
const PUSH_R16_R23: u16 = ldm_stm(true, false, 2, 8);
const POP_R0_R15: u16 = ldm_stm(false, false, 0, 16);
const POP_R16_R23: u16 = ldm_stm(false, false, 2, 8);
const POP_R24_R26: u16 = ldm_stm(false, false, 3, 3);

/// `switch rd` — halfword table form (`0000 0000 101d dddd`).
const fn switch_half(rd: u16) -> u16 {
    0x00A0 | rd
}

const RTI: u16 = 0x000A;
const NOP: u16 = 0x0001;

fn machine() -> Machine {
    Machine::new(128 * 1024)
}

fn load_code(m: &mut Machine, at: u32, code: &[u16]) {
    for (i, hw) in code.iter().enumerate() {
        m.store16(at + 2 * i as u32, *hw).expect("write code");
    }
}

fn step(v: &mut Vpu, m: &mut Machine) {
    assert_eq!(v.step(m), Step::Ran, "stopped: {:?}", v.stopped);
}

/// `stm` writes the register list with the **highest-numbered register at the
/// lowest address**. Getting this backwards was commit `8d7c27a`: it rotated
/// the register file on every preemptive context switch, because ThreadX builds
/// its interrupt frame from several separate pushes and tears it down with one
/// wide pop.
#[test]
fn push_stores_the_highest_register_at_the_lowest_address() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &[PUSH_R0_R5_LR]);

    for r in 0..=5 {
        v.regs.set(r, 0xA000_0000 + r as u32);
    }
    v.regs.set(26, 0xDEAD_BEEF); // lr
    v.regs.set(25, STACK_TOP); // sp

    step(&mut v, &mut m);

    let sp = v.regs.get(25);
    assert_eq!(sp, STACK_TOP - 28, "sp must drop by 6 registers plus lr");
    for w in 0..6u32 {
        let reg = 5 - w;
        assert_eq!(
            m.load32(sp + 4 * w).unwrap(),
            0xA000_0000 + reg,
            "word {w} of the frame must hold r{reg}"
        );
    }
    assert_eq!(
        m.load32(sp + 24).unwrap(),
        0xDEAD_BEEF,
        "lr occupies the top word of the frame"
    );
}

/// The real reason the ordering matters: ThreadX's ISR stub pushes
/// `{r0-r5, lr}`, `_tx_thread_context_save` then pushes `{r6-r15}` and
/// `{r16-r23}`, and `_tx_thread_schedule` unwinds the lot with
/// `pop {r16-r23}; pop {r0-r15}; ld r26,(sp)++`. That composes only if each
/// block runs downwards in register number, so the three frames abut exactly.
#[test]
fn threadx_interrupt_frame_round_trips() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(
        &mut m,
        CODE,
        &[
            PUSH_R0_R5_LR,
            PUSH_R6_R15,
            PUSH_R16_R23,
            POP_R16_R23,
            POP_R0_R15,
        ],
    );

    let expect = |r: usize| 0xC0DE_0000 + r as u32;
    for r in 0..=23 {
        v.regs.set(r, expect(r));
    }
    v.regs.set(26, 0xDEAD_BEEF);
    v.regs.set(25, STACK_TOP);

    for _ in 0..3 {
        step(&mut v, &mut m);
    }
    assert_eq!(
        v.regs.get(25),
        STACK_TOP - 4 * (6 + 1 + 10 + 8),
        "the three pushes must produce one contiguous 25-word frame"
    );

    // Whatever ran in between (the tick handler) leaves the register file in a
    // completely different state.
    for r in 0..=23 {
        v.regs.set(r, 0x5555_5555);
    }

    step(&mut v, &mut m);
    step(&mut v, &mut m);

    for r in 0..=23 {
        assert_eq!(v.regs.get(r), expect(r), "r{r} did not survive the frame");
    }
    assert_eq!(
        m.load32(v.regs.get(25)).unwrap(),
        0xDEAD_BEEF,
        "sp must now point at the saved lr, for the trailing `ld r26,(sp)++`"
    );
}

/// A `ldm` whose list covers r25 loads a new `sp` off the stack; that value has
/// to win over the pop's own auto-increment, or every context restore lands on
/// the outgoing thread's stack.
#[test]
fn popping_sp_beats_the_auto_increment() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &[POP_R24_R26]);

    v.regs.set(25, 0x2000);
    m.store32(0x2000, 0x1111_1111).unwrap(); // r26 (highest, lowest address)
    m.store32(0x2004, 0x0000_4000).unwrap(); // r25 — the new sp
    m.store32(0x2008, 0x3333_3333).unwrap(); // r24

    step(&mut v, &mut m);

    assert_eq!(v.regs.get(26), 0x1111_1111);
    assert_eq!(v.regs.get(24), 0x3333_3333);
    assert_eq!(
        v.regs.get(25),
        0x0000_4000,
        "the loaded sp must win over sp + 12"
    );
}

/// `switch` jump-table entries are **signed** halfword displacements from the
/// end of the instruction. Handlers defined before the `switch` — and the
/// default case, which is a short hop backwards — are only reachable if the
/// sign is honoured.
#[test]
fn switch_table_entries_are_signed() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    let table = CODE + 2;
    load_code(&mut m, CODE, &[switch_half(0)]);
    m.store16(table, 6i16 as u16).unwrap(); // entry 0: forwards
    m.store16(table + 2, (-8i16) as u16).unwrap(); // entry 1: backwards

    v.regs.set(0, 0);
    step(&mut v, &mut m);
    assert_eq!(v.regs.pc, table + 12, "positive entry: base + 6 halfwords");

    v.regs.pc = CODE;
    v.regs.set(0, 1);
    step(&mut v, &mut m);
    assert_eq!(
        v.regs.pc,
        table.wrapping_sub(16),
        "negative entry must go backwards, not 32 KiB forwards"
    );
}

/// Set up a vector table at `vbase` whose slot `slot` points at a handler.
fn arm_vector(m: &mut Machine, v: &mut Vpu, vbase: u32, slot: u32, handler: u32) {
    m.store32(vbase + 4 * slot, handler).unwrap();
    load_code(m, handler, &[NOP, RTI]);
    v.exc_vbase = vbase;
    v.regs.set(25, STACK_TOP);
}

/// Interrupt delivery gates on the SR interrupt-enable bit (`r30` bit 30) and
/// nothing else. Commit `2bdbcbf`: gating on an exception-depth counter wedged
/// every tick after the first, because ThreadX's `_tx_thread_schedule` enters
/// its idle loop from inside the tick ISR and never returns from it.
#[test]
fn interrupts_gate_on_the_enable_bit_not_on_nesting() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    v.irq_model = true;
    arm_vector(&mut m, &mut v, 0x2000, 1, 0x3000);

    // Interrupts off: nothing is delivered.
    v.regs.set(30, 0);
    v.regs.pc = CODE;
    v.vector_irq(&mut m, 1);
    assert_eq!(v.regs.pc, CODE, "delivered with interrupts disabled");

    // Interrupts on, and already several exceptions deep — still delivered.
    v.regs.set(30, 1 << 30);
    v.in_exception = 7;
    v.vector_irq(&mut m, 1);
    assert_eq!(
        v.regs.pc, 0x3000,
        "nesting depth must not suppress a tick with interrupts enabled"
    );
}

/// The `sleep` wake is the one path that ignores the enable bit: ThreadX's idle
/// loop parks as `sleep; di; b` and nothing in it ever runs `ei`, so the wake
/// itself has to service the pending tick.
#[test]
fn the_sleep_wake_vectors_with_interrupts_disabled() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    v.irq_model = true;
    arm_vector(&mut m, &mut v, 0x2000, 1, 0x3000);

    v.regs.set(30, 0);
    v.regs.pc = CODE;
    v.vector_irq_forced(&mut m, 1);
    assert_eq!(v.regs.pc, 0x3000);
}

/// The exception frame a vectored interrupt leaves behind must be exactly what
/// `rti` expects: saved SR then resume address, `sp` down by 8. If the two ever
/// disagree the firmware returns into garbage.
#[test]
fn vectored_interrupt_frame_unwinds_through_rti() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    v.irq_model = true;
    arm_vector(&mut m, &mut v, 0x2000, 1, 0x3000);
    load_code(&mut m, CODE, &[NOP]);

    v.regs.set(30, 1 << 30);
    v.regs.pc = CODE;
    v.vector_irq(&mut m, 1);

    let sp = v.regs.get(25);
    assert_eq!(sp, STACK_TOP - 8, "the frame is two words");
    assert_eq!(
        m.load32(sp + 4).unwrap(),
        CODE,
        "the interrupted pc is the resume address"
    );
    assert_ne!(
        m.load32(sp).unwrap() & (1 << 30),
        0,
        "the saved SR must carry the interrupt-enable bit forward"
    );

    // The handler is `nop; rti`.
    v.regs.set(30, 0); // the handler ran `di`, as ThreadX's does
    step(&mut v, &mut m);
    step(&mut v, &mut m);

    assert_eq!(v.regs.pc, CODE, "rti must resume the interrupted pc");
    assert_eq!(v.regs.get(25), STACK_TOP, "rti must pop the frame");
    assert!(
        v.irq_enabled(),
        "rti must restore the interrupted context's enable bit"
    );
}
