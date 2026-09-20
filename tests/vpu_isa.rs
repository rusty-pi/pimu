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

/// The decode cache (#43) serves an instruction again only while nothing has
/// been written into its page. Here the firmware-style case: code patched in
/// place after it already ran once.
#[test]
fn decode_cache_redecodes_after_a_store_into_the_page() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &[NOP]);
    v.regs.set(25, STACK_TOP);

    step(&mut v, &mut m);
    v.regs.pc = CODE;
    step(&mut v, &mut m);
    assert_eq!(v.icache.hits, 1, "the second run of the same nop is a hit");
    assert_eq!(v.regs.get(25), STACK_TOP);

    // A store to another page leaves the entry alone.
    m.store32(CODE + 0x1000, 0xFFFF_FFFF).unwrap();
    v.regs.pc = CODE;
    step(&mut v, &mut m);
    assert_eq!(v.icache.hits, 2);

    // Overwrite the nop with a push: the cached nop must not run again.
    load_code(&mut m, CODE, &[PUSH_R0_R5_LR]);
    v.regs.pc = CODE;
    step(&mut v, &mut m);
    assert_eq!(
        v.regs.get(25),
        STACK_TOP - 28,
        "the push ran, not the stale nop"
    );
    assert_eq!(v.icache.stale, 1);
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
// Only the forms `VecInsn::executable` matches exactly are carried out; the
// rest of the vector unit must fault rather than be guessed at. Two of them come
// from `FUN_0edc9e20` in `start4.elf` — the routine that flushes the unit's
// outstanding reads before the VRF semaphore is released:
//
//     v8ld  -,(r0)          x4
//     ld    r0,(sp)
//     v16mov -,r0 SUMS r0
//     mov   r0,r0
//     rts
//
// and the rest are what VC4 libc's `memcpy`/`memmove`/`memset` are built out
// of: VRF<->memory transfers with `++`/`REP`, a scalar broadcast, and
// `bitplanes` + per-lane predication for the ragged head and tail.
//
// All readings were confirmed against `binutils-vc4` objdump.

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

/// Anything outside the implemented forms has to fault: quietly stepping over
/// a vector instruction corrupts whatever it was moving. This is
/// `v8ld V(0,32++),(r3+=r5) REP4` from `start4.elf` — a vertical window, which
/// this model does handle, with a `++` that steps its *column* by an amount
/// nothing here pins down.
#[test]
fn vector_op_that_needs_the_register_file_faults() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &[0xF802, 0x5038, 0x0380, 0xF940, 0x000C, NOP]);

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
/// a real vector register where this model expects a bare dash has to fault too.
/// A load's A slot is ignored on hardware and is a dash in every load start4
/// issues; one that names a register is an encoding whose meaning is not
/// established here.
#[test]
fn vector_near_miss_of_the_discarded_load_faults() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    // v8ld -,(r0) is 0xF000_E038_0380; clear the top bit of the A-slot
    // descriptor (bit 26 counted from the MSB) so the slot names H(0,48).
    let raw: u64 = 0xF000_E038_0380 & !(1u64 << (47 - 26));
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

// --- the memcpy/memmove/memset vector loops ---------------------------------
//
// Instruction words as they sit in `start4.elf`, named by the address they are
// at. Each is a 16-bit-parcel array in memory order, which is what `load_code`
// takes; `disasm --vaddr <addr>` prints the same bytes.

/// `0x3EDA29AE`: `v8ld H(0,0),(r1)` — 16 bytes into the first VRF row.
const V8LD_R1: [u16; 3] = [0xF000, 0x0038, 0x0381];
/// `0x3EDA2A42`: `v8st H(0,0),(r0)` — the same row back out to `(r0)`.
const V8ST_R0: [u16; 3] = [0xF080, 0xE000, 0x0380];
/// `0x3EDA28F2`: `v32ld HY(0,0)++,(r1+=r4) REP r0` — `r0` rows of 16 32-bit
/// lanes, stepping the address by `r4` and the register down a row each time.
const V32LD_REP: [u16; 5] = [0xF817, 0xC038, 0x0380, 0xF900, 0x0004];
/// `0x3EDA2904`: `v32st HY(0,0)++,(r3+=r4) REP r0`, the matching store.
const V32ST_REP: [u16; 5] = [0xF897, 0xE030, 0x0380, 0x43E0, 0x000C];
/// `0x3EDA292C`: `v16bitplanes -,r0 SETF` — one flag per lane, from the bits
/// of `r0`.
const V16BITPLANES_R0: [u16; 3] = [0xF408, 0xE038, 0x03C0];
/// `0x3EDA2932` / `0x3EDA293E`: the predicated tail pair, executing the lanes
/// whose `bitplanes` bit was 0.
const V32LD_R1_IFZ: [u16; 5] = [0xF810, 0xC038, 0x0380, 0xF3C0, 0x4004];
const V32ST_R3_IFZ: [u16; 5] = [0xF890, 0xE030, 0x0380, 0xF3C0, 0x400C];
/// `0x3EDA2B44`: `v32mov HY(0,0),r1` — broadcast `r1` over the 16 lanes.
const V32MOV_R1: [u16; 3] = [0xF600, 0xC038, 0x0381];
/// `0x3EDA2B66`: `v32st HY(0,0),(r1)` under the *other* predicate — the lanes
/// whose bit was 1. This is how `memset` fills a partial first block.
const V32ST_R1_IFNZ: [u16; 5] = [0xF890, 0xE030, 0x0380, 0xF3C0, 0x6004];

fn concat(parts: &[&[u16]]) -> Vec<u16> {
    parts.iter().flat_map(|p| p.iter().copied()).collect()
}

/// Fill `len` bytes at `at` with a recognisable pattern.
fn fill(m: &mut Machine, at: u32, len: u32) {
    for i in 0..len {
        m.store8(at + i, (0x40 + i) as u8).unwrap();
    }
}

/// The simplest transfer: a VRF row is loaded from one address and stored to
/// another, 16 bytes at a time. `memmove`'s backward loop is exactly this pair.
#[test]
fn vector_load_store_moves_one_register_through_the_vrf() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &concat(&[&V8LD_R1, &V8ST_R0, &[NOP]]));
    fill(&mut m, 0x4000, 16);
    v.regs.set(1, 0x4000);
    v.regs.set(0, 0x5000);

    step(&mut v, &mut m);
    step(&mut v, &mut m);

    assert_eq!(v.regs.pc, CODE + 12);
    for i in 0..16u32 {
        assert_eq!(m.load8(0x5000 + i).unwrap(), (0x40 + i) as u8, "byte {i}");
    }
    assert_eq!(m.load8(0x5010).unwrap(), 0, "16 bytes, not 17");
    assert_eq!(
        (v.regs.get(0), v.regs.get(1)),
        (0x5000, 0x4000),
        "the base registers are not written back"
    );
}

/// `REP r0` with `++`: `r0` repetitions, each moving one 64-byte row and
/// stepping the address by `r4`. `memcpy`'s bulk loop reads the count out of
/// `r0` and then advances the pointers by `r0 * 64` itself, so the instruction
/// must transfer exactly that much and leave the registers alone.
#[test]
fn vector_rep_transfers_r0_rows() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &concat(&[&V32LD_REP, &V32ST_REP, &[NOP]]));
    fill(&mut m, 0x4000, 192);
    v.regs.set(0, 3); // rows
    v.regs.set(4, 64); // bytes per row
    v.regs.set(1, 0x4000); // source
    v.regs.set(3, 0x5000); // destination

    step(&mut v, &mut m);
    step(&mut v, &mut m);

    for i in 0..192u32 {
        assert_eq!(m.load8(0x5000 + i).unwrap(), (0x40 + i) as u8, "byte {i}");
    }
    assert_eq!(m.load8(0x50C0).unwrap(), 0, "192 bytes, not 193");
    assert_eq!(v.regs.get(1), 0x4000, "no writeback of the source pointer");
    assert_eq!(v.regs.get(3), 0x5000, "no writeback of the destination");
}

/// Without `++` the same register is reused, so every repetition overwrites the
/// row and only the last one survives — which is why the copy loops need it.
#[test]
fn vector_rep_without_the_row_step_reuses_one_register() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    // Clear the D-slot modifier (bits 52..53 of the 80-bit word) on the load.
    let mut ld = V32LD_REP;
    ld[3] &= !0x0800;
    load_code(&mut m, CODE, &concat(&[&ld, &V32ST_REP, &[NOP]]));
    fill(&mut m, 0x4000, 192);
    v.regs.set(0, 3);
    v.regs.set(4, 64);
    v.regs.set(1, 0x4000);
    v.regs.set(3, 0x5000);

    step(&mut v, &mut m);
    step(&mut v, &mut m);

    // Row 0 of the VRF ends up holding the *third* source row, and the store
    // (which does step) writes it out first.
    assert_eq!(m.load8(0x5000).unwrap(), 0x40 + 128);
}

/// `bitplanes` + predicate 2 is `memcpy`'s tail: `r0 = ~0 << n` leaves the low
/// `n` bits clear, and the pair transfers exactly those `n` lanes. One byte past
/// them must be untouched — getting the polarity backwards would copy the wrong
/// end of the register and silently corrupt the destination.
#[test]
fn vector_predicate_ifz_transfers_the_lanes_whose_bit_is_clear() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(
        &mut m,
        CODE,
        &concat(&[&V16BITPLANES_R0, &V32LD_R1_IFZ, &V32ST_R3_IFZ, &[NOP]]),
    );
    fill(&mut m, 0x4000, 64);
    v.regs.set(0, !0u32 << 3); // three words
    v.regs.set(1, 0x4000);
    v.regs.set(3, 0x5000);

    for _ in 0..3 {
        step(&mut v, &mut m);
    }

    for i in 0..12u32 {
        assert_eq!(m.load8(0x5000 + i).unwrap(), (0x40 + i) as u8, "byte {i}");
    }
    for i in 12..64u32 {
        assert_eq!(
            m.load8(0x5000 + i).unwrap(),
            0,
            "lane {} must be masked off",
            i / 4
        );
    }
}

/// The other polarity, from `memset`: a band of *set* bits selects the lanes,
/// under predicate 3. Both polarities are in the firmware, so one implementation
/// cannot satisfy both by accident.
#[test]
fn vector_predicate_ifnz_transfers_the_lanes_whose_bit_is_set() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(
        &mut m,
        CODE,
        &concat(&[&V32MOV_R1, &V16BITPLANES_R0, &V32ST_R1_IFNZ, &[NOP]]),
    );
    v.regs.set(1, 0x5000);
    v.regs.set(0, 0b0000_0000_0011_1100); // lanes 2..5

    step(&mut v, &mut m); // v32mov HY(0,0),r1 — broadcast 0x5000
    step(&mut v, &mut m);
    step(&mut v, &mut m);

    for lane in 0..16u32 {
        let want = if (2..6).contains(&lane) { 0x5000 } else { 0 };
        assert_eq!(m.load32(0x5000 + 4 * lane).unwrap(), want, "lane {lane}");
    }
}

/// The broadcast itself: every lane of the destination row holds the scalar.
#[test]
fn vector_broadcast_fills_every_lane() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &concat(&[&V32MOV_R1, &V32ST_REP, &[NOP]]));
    v.regs.set(1, 0xA5A5_1234);
    v.regs.set(0, 1); // one repetition
    v.regs.set(4, 64);
    v.regs.set(3, 0x5000);

    step(&mut v, &mut m);
    v.regs.set(1, 0); // the store reads r3, not r1
    step(&mut v, &mut m);

    for lane in 0..16u32 {
        assert_eq!(
            m.load32(0x5000 + 4 * lane).unwrap(),
            0xA5A5_1234,
            "lane {lane}"
        );
    }
}

/// The forms are matched as whole words, so a near miss must still fault rather
/// than be executed with a field this model does not interpret. Here the 80-bit
/// load carries a non-zero address offset — a field whose placement comes from
/// Hermitage's notes and which no executed instruction exercises.
#[test]
fn vector_load_with_an_unmodelled_offset_faults() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    let mut ld = V32LD_R1_IFZ;
    ld[2] |= 0x0010; // bit 47 of the word: the low bit of the offset field
    load_code(&mut m, CODE, &concat(&[&ld, &[NOP]]));
    v.regs.set(1, 0x4000);

    assert_eq!(v.step(&mut m), Step::Stopped, "must not be executed");
}

/// Likewise an unknown lane predicate: predicates 2 and 3 are pinned by the
/// firmware, the other five are not.
#[test]
fn vector_store_with_an_unknown_predicate_faults() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    let mut st = V32ST_R3_IFZ;
    st[4] = (st[4] & !0xE000) | 0x8000; // predicate 4
    load_code(&mut m, CODE, &concat(&[&st, &[NOP]]));
    v.regs.set(3, 0x5000);

    assert_eq!(v.step(&mut m), Step::Stopped, "must not be executed");
}

/// `v32mov HY(0,0)++,#0 REP8` — the vector memory-clear at `0x60000446` in the
/// BCM2711 boot ROM. This 80-bit `REP` broadcast used to fall through to
/// `VecExec::NeedsVrf` and fault; the model now clears `reps` consecutive VRF
/// rows so `--boot-rom` can execute the maskROM's RAM zeroing.
#[test]
fn boot_rom_vector_memclear_is_an_executable_rep_broadcast() {
    use rpi_virt_fw::vpu::decode::decode;
    use rpi_virt_fw::vpu::insn::{Op, RegOrImm, VecExec, VecRep};

    let bytes = [0x03, 0xfe, 0x38, 0xc0, 0x00, 0x04, 0xc0, 0xfb, 0x00, 0x00];
    let insn = decode(&bytes, 0x6000_0446);
    assert_eq!(insn.len, 10, "80-bit vector instruction");
    let Op::Vector(v) = insn.op else {
        panic!("expected a vector op");
    };
    assert!(!v.mem && v.subop == 0 && v.lane_bits == 32, "v32mov");
    match v.executable() {
        VecExec::Broadcast {
            src: RegOrImm::Imm(0),
            reps: VecRep::Fixed(8),
            step_row: true,
            ..
        } => {}
        _ => panic!("expected an 8-row zero broadcast"),
    }

    // And it executes rather than faulting.
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &[]);
    for (i, b) in bytes.iter().enumerate() {
        m.store8(CODE + i as u32, *b).unwrap();
    }
    v.regs.pc = CODE;
    assert_eq!(v.step(&mut m), Step::Ran, "stopped: {:?}", v.stopped);
    assert_eq!(v.regs.pc, CODE + 10, "advanced past the 80-bit insn");
}

// ---------------------------------------------------------------------------
// Vector operand slots.
//
// Each fixture is an instruction lifted out of `start4.elf`, with
// `binutils-vc4` objdump's spelling of those exact bytes in the comment. The
// whole of that `.text` was compared against this decoder instruction by
// instruction: 15054 of the 15180 vector words agree, and the 126 that do not
// are ones objdump renders as a raw `vec48`/`vec80` because its own tables have
// no form for them.

fn vector(bytes: &[u8]) -> rpi_virt_fw::vpu::insn::VecInsn {
    use rpi_virt_fw::vpu::decode::decode;
    use rpi_virt_fw::vpu::insn::Op;
    let Op::Vector(v) = decode(bytes, 0).op else {
        panic!("not a vector instruction");
    };
    *v
}

/// The type nibble is not an element width: it says how coarsely the slot can
/// spell its column. `H` steps in 16 bytes, `HX` in 32, `HY` can only say 0 —
/// and the lanes are as wide as the *operation*, here 8 bits.
#[test]
fn a_horizontal_slot_takes_its_column_from_the_type_nibble() {
    // `v8ld H(0++,32),(r2+=r3) REP16`
    let v = vector(&[0x04, 0xf8, 0x38, 0x40, 0x80, 0x03, 0xc0, 0xf8, 0x08, 0x00]);
    assert_eq!((v.lane_bits, v.subop), (8, 0), "v8ld");
    assert!(!v.d.is_vertical() && v.d.ty >> 1 <= 3, "the H family");
    assert_eq!((v.d.y, v.d.x), (0, 32), "row 0, column 32");
    assert!(v.d.inc, "++");
    assert!(!v.d.star && v.d.addend == 15, "no `*`, no `+rN`");
    let addr = v.addr.expect("a dash B slot spells the address");
    assert_eq!((addr.base, addr.incr, addr.offset), (2, Some(3), 0));
    assert_eq!(v.rep, 4, "REP16");
}

/// A vertical slot is a *column*: its `y` names the 16-aligned band of rows it
/// covers and the low nibble of the coordinate belongs to `x` instead. Reading
/// it like a horizontal slot prints rows that cannot exist (`binutils-vc4`'s
/// V-direction fix, which `vc4.slaspec` agrees with).
#[test]
fn a_vertical_slot_splits_its_coordinate() {
    // `v8ld V(0,32++),(r3+=r5) REP4`
    let v = vector(&[0x02, 0xf8, 0x38, 0x50, 0x80, 0x03, 0x40, 0xf9, 0x0c, 0x00]);
    assert!(v.d.is_vertical(), "V");
    assert_eq!((v.d.y, v.d.x), (0, 32), "band 0, column 32");
    assert!(v.d.inc, "++ steps the column, not the row");
    let w = v.d.window(v.lane_bits).expect("a window");
    assert!(w.vertical && w.y == 0 && w.x == 32);
    // The column step `++` asks for is pinned down by nothing, so this one
    // still faults: the window is modelled, the stepping is not.
    assert_eq!(v.executable(), rpi_virt_fw::vpu::insn::VecExec::NeedsVrf);
}

/// A 48-bit instruction has one addend register for all three slots — the
/// opcode's own `r0..r7` field — and one presence bit per slot.
#[test]
fn the_48_bit_addend_is_one_register_for_every_slot() {
    // `v16or H(14,0),H(49,0)+r3,H(14,0)`
    let v = vector(&[0x8b, 0xf4, 0x87, 0x03, 0x0e, 0x10]);
    assert_eq!((v.a.y, v.a.addend), (49, 3), "+r3 on A");
    assert_eq!(v.d.addend, 15, "none on D");
    assert_eq!((v.d.y, v.d.x), (14, 0));
}

/// A dash in the B slot names a scalar register: the 48-bit form spells it in
/// the coordinate field, and the bit that would be a B addend is `SETF`.
#[test]
fn a_48_bit_dash_b_names_a_scalar_register() {
    // `v16bitplanes -,r3 SETF`
    let v = vector(&[0x08, 0xf4, 0x38, 0xe0, 0xc3, 0x03]);
    let rpi_virt_fw::vpu::insn::VecOperandB::Slot(b) = v.b else {
        panic!("a slot, not an immediate");
    };
    assert!(b.is_dash());
    assert_eq!(b.scalar, 3, "r3");
    assert!(v.setf);
    assert_eq!(
        v.executable(),
        rpi_virt_fw::vpu::insn::VecExec::Bitplanes { src: 3 }
    );
}

/// The 80-bit form spells that register in the addend nibble instead, beside a
/// signed 9-bit displacement.
#[test]
fn an_80_bit_dash_b_carries_a_signed_displacement() {
    // `v8mem29 -,H(63,3)+r0,r3-219 REP2 SETF IFNC max2 r0`
    let v = vector(&[0xa1, 0xfb, 0xc3, 0xe8, 0xa5, 0xfb, 0x03, 0x3c, 0x0e, 0xf4]);
    let rpi_virt_fw::vpu::insn::VecOperandB::Slot(b) = v.b else {
        panic!("a slot, not an immediate");
    };
    assert!(b.is_dash());
    assert_eq!((b.scalar, b.disp), (3, -219));
    assert!(v.addr.is_none(), "only `vld`/`vst` read the wide address");
}

/// A load's address carries a displacement, and this model does not execute
/// one: nothing says whether it counts bytes or elements.
#[test]
fn an_address_displacement_decodes_but_does_not_execute() {
    // `v16ld HX(3,32),(r0+32)`
    let v = vector(&[0x08, 0xf8, 0xf8, 0xa0, 0xa0, 0x03, 0xc0, 0xf3, 0x00, 0x00]);
    assert_eq!((v.d.y, v.d.x), (3, 32));
    let addr = v.addr.expect("an address");
    assert_eq!((addr.base, addr.offset, addr.incr), (0, 32, None));
    assert_eq!(v.executable(), rpi_virt_fw::vpu::insn::VecExec::NeedsVrf);
}

/// In the memory class the same bit is a B addend, not `SETF` — the B slot here
/// is a vector register with a coordinate to step.
#[test]
fn a_memory_class_b_register_takes_the_addend_not_setf() {
    // `v16mem27 V(32,16),V(48,15),V(16,9)+r2`
    let v = vector(&[0x6a, 0xf3, 0x03, 0x38, 0x59, 0xf0]);
    let rpi_virt_fw::vpu::insn::VecOperandB::Slot(b) = v.b else {
        panic!("a slot, not an immediate");
    };
    assert_eq!((b.y, b.x, b.addend), (16, 9, 2), "V(16,9)+r2");
    assert!(!v.setf, "the bit is the addend here");
    assert!(v.addr.is_none(), "a register B is not an address");
}

/// A vertical transfer moves a *column* of the file. Sixteen 16-bit lanes,
/// one per row from the band the slot names, at one byte column — so sixteen
/// consecutive halfwords in memory land sixteen rows apart in the file.
#[test]
fn a_vertical_load_fills_a_column_of_the_file() {
    // `v16ld VX(32,46),(r0)` out of `start4.elf` at `0x0ec8bb66`.
    let bytes = [0x08, 0xf0, 0xb8, 0xbb, 0x80, 0x03];
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    for (i, b) in bytes.iter().enumerate() {
        m.store8(CODE + i as u32, *b).unwrap();
    }
    m.store16(CODE + 6, NOP).unwrap();
    for lane in 0..16u32 {
        m.store16(0x4000 + lane * 2, (0x1000 + lane) as u16)
            .unwrap();
    }
    v.regs.set(0, 0x4000);
    v.regs.pc = CODE;
    assert_eq!(v.step(&mut m), Step::Ran, "stopped: {:?}", v.stopped);

    let vrf = &v.vrf;
    for lane in 0..16u32 {
        let row = 32 + lane as usize;
        let want = 0x1000 + lane;
        let got = vrf.byte(row, 46) as u32 | (vrf.byte(row, 47) as u32) << 8;
        assert_eq!(got, want, "row {row}");
    }
    // Nothing landed beside it: the lanes went down, not across.
    assert_eq!(vrf.byte(32, 48), 0, "the next column is untouched");
}
