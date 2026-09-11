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

// --- vector unit -----------------------------------------------------------
//
// The vector register file is not modelled, so almost everything the vector
// unit can do must fault rather than be guessed at. Exactly two encodings are
// executed, both from `FUN_0edc9e20` in `start4.elf` — the routine that flushes
// the unit's outstanding reads before the VRF semaphore is released:
//
//     v8ld  -,(r0)          x4
//     ld    r0,(sp)
//     v16mov -,r0 SUMS r0
//     mov   r0,r0
//     rts
//
// Both readings were confirmed against `binutils-vc4` objdump.

/// `00 f0 38 e0 80 03` = `v8ld -,(r0)`: a 16-lane 8-bit load whose destination
/// descriptor is the "dash" slot, so nothing lands in a vector register. The
/// read still happens — it is the whole point of the instruction — but no
/// scalar register may change.
#[test]
fn vector_discarded_load_touches_no_register() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &[0xF000, 0xE038, 0x0380, NOP]);
    for i in 0..16u32 {
        m.store8(0x4000 + i, 0xA5).unwrap();
    }
    v.regs.set(0, 0x4000);
    let before: Vec<u32> = (0..32).map(|r| v.regs.get(r)).collect();

    step(&mut v, &mut m);

    assert_eq!(v.regs.pc, CODE + 6, "48-bit vector instruction");
    // `r` is a register *number*, not just an index — it is what the failure
    // message names, so enumerate() would make this worse.
    #[allow(clippy::needless_range_loop)]
    for r in 0..32 {
        assert_eq!(v.regs.get(r), before[r], "r{r} must be untouched");
    }
}

/// `00 fc 38 e0 80 03 c0 f3 00 12` = `v16mov -,r0 SUMS r0`: r0 is broadcast
/// across the 16 lanes at 16-bit width, the vector result is discarded, and the
/// scalar result unit writes the signed sum of the lanes back to r0.
#[test]
fn vector_sum_of_broadcast_writes_the_scalar() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &[0xFC00, 0xE038, 0x0380, 0xF3C0, 0x1200, NOP]);
    v.regs.set(0, 3);

    step(&mut v, &mut m);

    assert_eq!(v.regs.pc, CODE + 10, "80-bit vector instruction");
    assert_eq!(v.regs.get(0), 16 * 3, "sum of 16 lanes each holding r0");
}

/// The lanes are 16 bits wide and `SUMS` reads them as signed, so a value that
/// is negative in 16 bits sums negative — not as the 32-bit register would.
#[test]
fn vector_sum_of_broadcast_is_signed_at_the_lane_width() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &[0xFC00, 0xE038, 0x0380, 0xF3C0, 0x1200, NOP]);
    v.regs.set(0, 0x0001_FFFF); // -1 in a 16-bit lane, positive in 32

    step(&mut v, &mut m);

    assert_eq!(v.regs.get(0), (-16i32) as u32);
    assert!(v.regs.flags.n, "the SRU writeback updates the scalar flags");
}

/// Anything that would read or write a real vector register has to fault: the
/// model has no VRF, and quietly stepping over it corrupts the copy it was
/// making. `08 f0 b8 80 80 03` = `v16ld HX(2,0),(r0)` from the blit loop at
/// `FUN_0edc9bbc`.
#[test]
fn vector_op_that_needs_the_register_file_faults() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &[0xF008, 0x80B8, 0x0380, NOP]);

    assert_eq!(v.step(&mut m), Step::Stopped);
    assert!(
        matches!(
            v.stopped,
            Some(rpi_virt_fw::vpu::Stop::Fault(
                rpi_virt_fw::vpu::Fault::Unimplemented { .. }
            ))
        ),
        "stopped: {:?}",
        v.stopped
    );
}

/// The "dash" test must not be a loose field check: a near neighbour that names
/// a real vector register in the discarded slot has to fault too. Here the
/// destination descriptor is a genuine `H32` register rather than the dash.
#[test]
fn vector_near_miss_of_the_discarded_load_faults() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    // v8ld -,(r0) is 0xF000_E038_0380; clear the top bit of the destination
    // descriptor (bit 18 counted from the MSB) so the slot names H32(0,0).
    let raw: u64 = 0xF000_E038_0380 & !(1u64 << (47 - 16));
    load_code(
        &mut m,
        CODE,
        &[(raw >> 32) as u16, (raw >> 16) as u16, raw as u16, NOP],
    );

    assert_eq!(v.step(&mut m), Step::Stopped, "must not be executed");
}

/// An 80-bit vector word does not fit in 64 bits. It used to be truncated on
/// the way into the report, which made two different instructions look
/// identical; the decoder must keep all five parcels.
#[test]
fn vector80_keeps_its_top_parcel() {
    use rpi_virt_fw::vpu::decode::decode;
    use rpi_virt_fw::vpu::insn::Op;

    let bytes = [0x00, 0xFC, 0x38, 0xE0, 0x80, 0x03, 0xC0, 0xF3, 0x00, 0x12];
    let insn = decode(&bytes, CODE);
    assert_eq!(insn.len, 10);
    match insn.op {
        Op::Vector(v) => assert_eq!(v.raw, 0xFC00_E038_0380_F3C0_1200),
        other => panic!("expected a vector op, got {other:?}"),
    }
}
