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
/// `v16ld HX(0++,0)*,(r2+=r5) REP4` from `start4.elf` at `0x0eca59b0` — an
/// ordinary load but for the `*`, whose meaning nothing here establishes.
#[test]
fn vector_op_that_needs_the_register_file_faults() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    load_code(&mut m, CODE, &[0xF80A, 0x8038, 0x0380, 0xFD40, 0x0008, NOP]);

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

/// A displacement on the address is executed now, as the byte offset it is.
#[test]
fn vector_load_with_an_offset_reads_that_many_bytes_further() {
    // `v16ld HX(3,32),(r1+32)`, out of `start4.elf` at `0x0ec02822` — it was
    // `(r0+32)` there, and the base register is the only change.
    let bytes = [0x08, 0xf8, 0xf8, 0xa0, 0xa0, 0x03, 0xc0, 0xf3, 0x04, 0x00];
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    for (i, b) in bytes.iter().enumerate() {
        m.store8(CODE + i as u32, *b).unwrap();
    }
    m.store16(CODE + 10, NOP).unwrap();
    for i in 0..128u32 {
        m.store8(0x4000 + i, (i + 1) as u8).unwrap();
    }
    v.regs.set(1, 0x4000);
    v.regs.pc = CODE;

    assert_eq!(v.step(&mut m), Step::Ran, "stopped: {:?}", v.stopped);
    // 16-bit elements from byte 32 onwards, into the second band of row 3.
    assert_eq!(v.vrf.read(3, 16, 2), 0x2221);
    assert_eq!(v.vrf.read(3, 17, 2), 0x2423);
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
            step: true,
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
    let w = v.d.window().expect("a window");
    assert!(w.vertical && w.y == 0 && w.e0 == 32 && w.elem_bytes == 1);
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

/// A load's address carries a displacement, and it counts in plain bytes —
/// `v16ld HX(3,32),(r1+32)` over a page of ascending bytes reads the halfword
/// at byte 32 for every element width (measured, Pi 4B d03115).
#[test]
fn an_address_displacement_is_a_byte_offset() {
    // `v16ld HX(3,32),(r0+32)`
    let v = vector(&[0x08, 0xf8, 0xf8, 0xa0, 0xa0, 0x03, 0xc0, 0xf3, 0x00, 0x00]);
    assert_eq!((v.d.y, v.d.x), (3, 32));
    let addr = v.addr.expect("an address");
    assert_eq!((addr.base, addr.offset, addr.incr), (0, 32, None));
    match v.executable() {
        rpi_virt_fw::vpu::insn::VecExec::Mem { offset: 32, .. } => {}
        other => panic!("expected a transfer with a byte displacement: {other:?}"),
    }
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

/// A vertical transfer moves a *column* of the file, and the file is not laid
/// out the way it reads: element `e` of a register `w` bytes wide sits at byte
/// `(e & 15) * 4 + (e >> 4) * w` of its row. `VX(32,46)` is 16-bit element 30
/// — lane 14, second half — so sixteen consecutive halfwords in memory land at
/// bytes 58..59 of rows 32..47. Measured with exactly these bytes on a
/// Raspberry Pi 4B d03115.
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
        let got = vrf.byte(row, 58) as u32 | (vrf.byte(row, 59) as u32) << 8;
        assert_eq!(got, want, "row {row}");
    }
    assert_eq!(vrf.byte(32, 56), 0, "the rest of the lane is untouched");
    assert_eq!(vrf.byte(48, 58), 0, "sixteen rows, not seventeen");
}

/// The operation's width and the register's are separate, and the unit converts
/// between them: a `v8ld` into a 32-bit slot zero-extends, a `v32st` out of an
/// 8-bit slot writes each byte as a word. Both measured on a Pi 4B d03115.
#[test]
fn the_operation_width_converts_to_the_register_width() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    // `v8ld HY(0,0),(r1)` then `v32st HY(0,0),(r0)`: sixteen bytes in,
    // sixteen words out.
    let code: [u16; 6] = [0xF000, 0xC038, 0x0381, 0xF090, 0xE030, 0x0380];
    load_code(&mut m, CODE, &concat(&[&code, &[NOP]]));
    for i in 0..16u32 {
        m.store8(0x4000 + i, (0x40 + i) as u8).unwrap();
    }
    v.regs.set(1, 0x4000);
    v.regs.set(0, 0x5000);
    step(&mut v, &mut m);
    step(&mut v, &mut m);
    for i in 0..16u32 {
        assert_eq!(m.load32(0x5000 + i * 4).unwrap(), 0x40 + i, "word {i}");
    }
}

/// The whole of the addressing, against the hardware it was measured on.
///
/// These six instructions ran on a Raspberry Pi 4B d03115 through the
/// firmware's `EXECUTE_CODE` mailbox tag, over a page whose byte `n` holds
/// `n + 1`, and the register file was read back out with
/// `v32st HY(0++,0),(r0+=r3) REP64`. The rows below are what the silicon left
/// behind; this model has to leave the same.
///
/// Between them they pin every part of the addressing: the `+rN` addend
/// (`+r4` with `r4 = 3` starts the register at element 3 and wraps it into the
/// second band), the byte displacement on the address (`(r1+7)`), all three
/// element widths against all three operation widths, and `++` on a vertical
/// slot, which walks the elements rather than the rows.
#[test]
fn the_measured_register_file_layout() {
    const ROWS: &[(usize, &str)] = &[
    (0, "000e0000000f0000001000000100000002000000030000000400000005000000060000000700000008000000090000000a0000000b0000000c0000000d000000"),
    (1, "080900000a0b00000c0d00000e0f000010010000121300001415000016170000181900001a1b00001c1d00001e1f000020110000222300002425000026270000"),
    (2, "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40"),
    (3, "0100000002000000030000000400000005000000060000000700000008000000090000000a0000000b0000000c0000000d0000000e0000000f00000010000000"),
    (4, "0100000005000000090000000d0000001100000015000000190000001d0000002100000025000000290000002d0000003100000035000000390000003d000000"),
    (16, "01000000110000002100000031000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"),
    (17, "02000000120000002200000032000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"),
    (18, "03000000130000002300000033000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000")
    ];
    // `v8ld H(0,0)+r4,(r1)`, `v16ld HX(1,0),(r1+7)`, `v32ld HY(2,0),(r1)`,
    // `v8ld HY(3,0),(r1)`, `v32ld H(4,0),(r1)`,
    // `v8ld V(16,0++),(r1+=r2) REP4`.
    const CODE_BYTES: &[u8] = &[
        0x04, 0xf0, 0x38, 0x00, 0x81, 0x0b, //
        0x08, 0xf8, 0x78, 0x80, 0x87, 0x03, 0xc0, 0xf3, 0x04, 0x00, //
        0x10, 0xf0, 0xb8, 0xc0, 0x81, 0x03, //
        0x00, 0xf0, 0xf8, 0xc0, 0x81, 0x03, //
        0x10, 0xf0, 0x38, 0x01, 0x81, 0x03, //
        0x02, 0xf8, 0x38, 0x14, 0x80, 0x03, 0x80, 0xf8, 0x04, 0x00,
    ];

    let mut m = machine();
    let mut v = Vpu::new(CODE);
    for (i, b) in CODE_BYTES.iter().enumerate() {
        m.store8(CODE + i as u32, *b).unwrap();
    }
    for i in 0..256u32 {
        m.store8(0x4000 + i, (i + 1) as u8).unwrap();
    }
    v.regs.set(1, 0x4000);
    v.regs.set(2, 16);
    v.regs.set(4, 3);
    v.regs.pc = CODE;
    for _ in 0..6 {
        step(&mut v, &mut m);
    }

    for (row, want) in ROWS {
        let got: String = (0..64)
            .map(|c| format!("{:02x}", v.vrf.byte(*row, c)))
            .collect();
        assert_eq!(&got, want, "row {row}");
    }
}

/// The vector ALU, against the hardware it was measured on.
///
/// Each row ran on a Raspberry Pi 4B d03115 over two vectors of edge cases —
/// `examples-on-real-hardware/vpu-probe/probes/alu.s` — with A in `HX(60,0)`,
/// B in `HX(61,0)` and the result read back out of the register file. The
/// model has to produce the same sixteen elements.
#[test]
fn the_measured_alu_semantics() {
    const A: [u32; 16] = [
        0x7fff, 0x8000, 0xffff, 0x0001, 0x4000, 0xc000, 0x0010, 0x8001, 0x0003, 0xfffe, 0x00ff,
        0xff00, 0x1234, 0xedcc, 0x0000, 0x7f00,
    ];
    const B: [u32; 16] = [
        0x0001, 0x0001, 0x0002, 0xffff, 0x4000, 0x4000, 0x0004, 0x7fff, 0x0002, 0x0003, 0x0100,
        0x0080, 0x0001, 0x0002, 0x0001, 0x0100,
    ];
    #[allow(clippy::type_complexity)]
    const CASES: &[(&str, usize, &[u8], &[u32])] = &[
        (
            "v16add",
            0,
            &[0x00, 0xf5, 0x23, 0x80, 0x3d, 0xc2],
            &[
                0x8000, 0x8001, 0x0001, 0x0000, 0x8000, 0x0000, 0x0014, 0x0000, 0x0005, 0x0001,
                0x01ff, 0xff80, 0x1235, 0xedce, 0x0001, 0x8000,
            ],
        ),
        (
            "v16adds",
            1,
            &[0x08, 0xf5, 0x63, 0x80, 0x3d, 0xc2],
            &[
                0x7fff, 0x8001, 0x0001, 0x0000, 0x7fff, 0x0000, 0x0014, 0x0000, 0x0005, 0x0001,
                0x01ff, 0xff80, 0x1235, 0xedce, 0x0001, 0x7fff,
            ],
        ),
        (
            "v16sub",
            4,
            &[0x20, 0xf5, 0x23, 0x81, 0x3d, 0xc2],
            &[
                0x7ffe, 0x7fff, 0xfffd, 0x0002, 0x0000, 0x8000, 0x000c, 0x0002, 0x0001, 0xfffb,
                0xffff, 0xfe80, 0x1233, 0xedca, 0xffff, 0x7e00,
            ],
        ),
        (
            "v16subs",
            5,
            &[0x28, 0xf5, 0x63, 0x81, 0x3d, 0xc2],
            &[
                0x7ffe, 0x8000, 0xfffd, 0x0002, 0x0000, 0x8000, 0x000c, 0x8000, 0x0001, 0xfffb,
                0xffff, 0xfe80, 0x1233, 0xedca, 0xffff, 0x7e00,
            ],
        ),
        (
            "v16rsub",
            6,
            &[0x40, 0xf5, 0xa3, 0x81, 0x3d, 0xc2],
            &[
                0x8002, 0x8001, 0x0003, 0xfffe, 0x0000, 0x8000, 0xfff4, 0xfffe, 0xffff, 0x0005,
                0x0001, 0x0180, 0xedcd, 0x1236, 0x0001, 0x8200,
            ],
        ),
        (
            "v16rsubs",
            7,
            &[0x48, 0xf5, 0xe3, 0x81, 0x3d, 0xc2],
            &[
                0x8002, 0x7fff, 0x0003, 0xfffe, 0x0000, 0x7fff, 0xfff4, 0x7fff, 0xffff, 0x0005,
                0x0001, 0x0180, 0xedcd, 0x1236, 0x0001, 0x8200,
            ],
        ),
        (
            "v16min",
            8,
            &[0xc0, 0xf4, 0x23, 0x82, 0x3d, 0xc2],
            &[
                0x0001, 0x8000, 0xffff, 0xffff, 0x4000, 0xc000, 0x0004, 0x8001, 0x0002, 0xfffe,
                0x00ff, 0xff00, 0x0001, 0xedcc, 0x0000, 0x0100,
            ],
        ),
        (
            "v16max",
            9,
            &[0xc8, 0xf4, 0x63, 0x82, 0x3d, 0xc2],
            &[
                0x7fff, 0x0001, 0x0002, 0x0001, 0x4000, 0x4000, 0x0010, 0x7fff, 0x0003, 0x0003,
                0x0100, 0x0080, 0x1234, 0x0002, 0x0001, 0x7f00,
            ],
        ),
        (
            "v16asr",
            10,
            &[0x58, 0xf4, 0xa3, 0x82, 0x3d, 0xc2],
            &[
                0x3fff, 0xc000, 0xffff, 0x0000, 0x4000, 0xc000, 0x0001, 0xffff, 0x0000, 0xffff,
                0x00ff, 0xff00, 0x091a, 0xfb73, 0x0000, 0x7f00,
            ],
        ),
        (
            "v16lsr",
            11,
            &[0x50, 0xf4, 0xe3, 0x82, 0x3d, 0xc2],
            &[
                0x3fff, 0x4000, 0x3fff, 0x0000, 0x4000, 0xc000, 0x0001, 0x0001, 0x0000, 0x1fff,
                0x00ff, 0xff00, 0x091a, 0x3b73, 0x0000, 0x7f00,
            ],
        ),
        (
            "v16shl",
            12,
            &[0x40, 0xf4, 0x23, 0x83, 0x3d, 0xc2],
            &[
                0xfffe, 0x0000, 0xfffc, 0x8000, 0x4000, 0xc000, 0x0100, 0x8000, 0x000c, 0xfff0,
                0x00ff, 0xff00, 0x2468, 0xb730, 0x0000, 0x7f00,
            ],
        ),
        (
            "v16shls",
            13,
            &[0x48, 0xf4, 0x63, 0x83, 0x3d, 0xc2],
            &[
                0x7fff, 0x8000, 0xfffc, 0x7fff, 0x4000, 0xc000, 0x0100, 0x8000, 0x000c, 0xfff0,
                0x00ff, 0xff00, 0x2468, 0xb730, 0x0000, 0x7f00,
            ],
        ),
        (
            "v16dist",
            14,
            &[0xd0, 0xf4, 0xa3, 0x83, 0x3d, 0xc2],
            &[
                0x7ffe, 0x8001, 0x0003, 0x0002, 0x0000, 0x8000, 0x000c, 0xfffe, 0x0001, 0x0005,
                0x0001, 0x0180, 0x1233, 0x1236, 0x0001, 0x7e00,
            ],
        ),
        (
            "v16dists",
            15,
            &[0xd8, 0xf4, 0xe3, 0x83, 0x3d, 0xc2],
            &[
                0x7ffe, 0x7fff, 0x0003, 0x0002, 0x0000, 0x7fff, 0x000c, 0x7fff, 0x0001, 0x0005,
                0x0001, 0x0180, 0x1233, 0x1236, 0x0001, 0x7e00,
            ],
        ),
        (
            "v16clip",
            16,
            &[0xe0, 0xf4, 0x23, 0x84, 0x3d, 0xc2],
            &[
                0x0001, 0x0000, 0x0000, 0x0000, 0x4000, 0x0000, 0x0004, 0x0000, 0x0002, 0x0000,
                0x00ff, 0x0000, 0x0001, 0x0000, 0x0000, 0x0100,
            ],
        ),
        (
            "v16sign",
            18,
            &[0xe8, 0xf4, 0xa3, 0x84, 0x3d, 0xc2],
            &[
                0x0002, 0x0000, 0x0001, 0x0000, 0x4001, 0x3fff, 0x0005, 0x7ffe, 0x0003, 0x0002,
                0x0101, 0x007f, 0x0002, 0x0001, 0x0001, 0x0101,
            ],
        ),
        (
            "v16count",
            19,
            &[0xa0, 0xf4, 0xe3, 0x84, 0x3d, 0xc2],
            &[
                0x0010, 0x0002, 0x0011, 0x0011, 0x0002, 0x0003, 0x0002, 0x0011, 0x0003, 0x0011,
                0x0009, 0x0009, 0x0006, 0x000b, 0x0001, 0x0008,
            ],
        ),
        (
            "v16bitrev",
            20,
            &[0x30, 0xf4, 0x23, 0x85, 0x3d, 0xc2],
            &[
                0x0001, 0x0000, 0x0003, 0x4000, 0x0002, 0x0003, 0x0000, 0x4000, 0x0003, 0x0003,
                0xff00, 0x00ff, 0x0000, 0x0000, 0x0000, 0x00fe,
            ],
        ),
    ];

    for (name, row, bytes, want) in CASES {
        let mut m = machine();
        let mut v = Vpu::new(CODE);
        for (i, b) in bytes.iter().enumerate() {
            m.store8(CODE + i as u32, *b).unwrap();
        }
        m.store16(CODE + bytes.len() as u32, NOP).unwrap();
        for e in 0..16u32 {
            v.vrf.write(60, e, 2, A[e as usize]);
            v.vrf.write(61, e, 2, B[e as usize]);
        }
        v.regs.pc = CODE;
        assert_eq!(v.step(&mut m), Step::Ran, "{name}: {:?}", v.stopped);
        let got: Vec<u32> = (0..16).map(|e| v.vrf.read(*row as u8, e, 2)).collect();
        assert_eq!(&got[..], *want, "{name}");
    }
}

/// The rest of the measured ALU ops, from the same apparatus over a plainer
/// pair of vectors: the bitwise ops, the rotate, `msb`, and the four shuffles,
/// which read elements other than their own lane's.
#[test]
fn the_measured_alu_shuffles_and_bitwise_ops() {
    const A: [u32; 16] = [
        0x0201, 0x0403, 0x0605, 0x0807, 0x0a09, 0x0c0b, 0x0e0d, 0x100f, 0x1211, 0x1413, 0x1615,
        0x1817, 0x1a19, 0x1c1b, 0x1e1d, 0x201f,
    ];
    const B: [u32; 16] = [
        0x4241, 0x4443, 0x4645, 0x4847, 0x4a49, 0x4c4b, 0x4e4d, 0x504f, 0x5251, 0x5453, 0x5655,
        0x5857, 0x5a59, 0x5c5b, 0x5e5d, 0x605f,
    ];
    #[allow(clippy::type_complexity)]
    const CASES: &[(&str, usize, &[u8], &[u32])] = &[
        (
            "v16and",
            10,
            &[0x80, 0xf4, 0xa3, 0x82, 0x3d, 0xc2],
            &[
                0x0201, 0x0403, 0x0605, 0x0807, 0x0a09, 0x0c0b, 0x0e0d, 0x100f, 0x1211, 0x1413,
                0x1615, 0x1817, 0x1a19, 0x1c1b, 0x1e1d, 0x201f,
            ],
        ),
        (
            "v16or",
            11,
            &[0x88, 0xf4, 0xe3, 0x82, 0x3d, 0xc2],
            &[
                0x4241, 0x4443, 0x4645, 0x4847, 0x4a49, 0x4c4b, 0x4e4d, 0x504f, 0x5251, 0x5453,
                0x5655, 0x5857, 0x5a59, 0x5c5b, 0x5e5d, 0x605f,
            ],
        ),
        (
            "v16eor",
            12,
            &[0x90, 0xf4, 0x23, 0x83, 0x3d, 0xc2],
            &[
                0x4040, 0x4040, 0x4040, 0x4040, 0x4040, 0x4040, 0x4040, 0x4040, 0x4040, 0x4040,
                0x4040, 0x4040, 0x4040, 0x4040, 0x4040, 0x4040,
            ],
        ),
        (
            "v16bic",
            13,
            &[0x98, 0xf4, 0x63, 0x83, 0x3d, 0xc2],
            &[
                0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
                0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            ],
        ),
        (
            "v16ror",
            20,
            &[0x38, 0xf4, 0x23, 0x85, 0x3d, 0xc2],
            &[
                0x8100, 0x6080, 0x2830, 0x0e10, 0x0485, 0x8161, 0x7068, 0x201e, 0x8908, 0x6282,
                0xa8b0, 0x2e30, 0x0c8d, 0x8363, 0xf0e8, 0x403e,
            ],
        ),
        (
            "v16msb",
            22,
            &[0xa8, 0xf4, 0xa3, 0x85, 0x3d, 0xc2],
            &[
                0x000e, 0x000e, 0x000e, 0x000e, 0x000e, 0x000e, 0x000e, 0x000e, 0x000e, 0x000e,
                0x000e, 0x000e, 0x000e, 0x000e, 0x000e, 0x000e,
            ],
        ),
        (
            "v16even",
            24,
            &[0x10, 0xf4, 0x23, 0x86, 0x3d, 0xc2],
            &[
                0x0201, 0x0605, 0x0a09, 0x0e0d, 0x1211, 0x1615, 0x1a19, 0x1e1d, 0x4241, 0x4645,
                0x4a49, 0x4e4d, 0x5251, 0x5655, 0x5a59, 0x5e5d,
            ],
        ),
        (
            "v16odd",
            25,
            &[0x18, 0xf4, 0x63, 0x86, 0x3d, 0xc2],
            &[
                0x0403, 0x0807, 0x0c0b, 0x100f, 0x1413, 0x1817, 0x1c1b, 0x201f, 0x4443, 0x4847,
                0x4c4b, 0x504f, 0x5453, 0x5857, 0x5c5b, 0x605f,
            ],
        ),
        (
            "v16interl",
            26,
            &[0x20, 0xf4, 0xa3, 0x86, 0x3d, 0xc2],
            &[
                0x0201, 0x4241, 0x0403, 0x4443, 0x0605, 0x4645, 0x0807, 0x4847, 0x0a09, 0x4a49,
                0x0c0b, 0x4c4b, 0x0e0d, 0x4e4d, 0x100f, 0x504f,
            ],
        ),
        (
            "v16interh",
            27,
            &[0x28, 0xf4, 0xe3, 0x86, 0x3d, 0xc2],
            &[
                0x1211, 0x5251, 0x1413, 0x5453, 0x1615, 0x5655, 0x1817, 0x5857, 0x1a19, 0x5a59,
                0x1c1b, 0x5c5b, 0x1e1d, 0x5e5d, 0x201f, 0x605f,
            ],
        ),
    ];

    for (name, row, bytes, want) in CASES {
        let mut m = machine();
        let mut v = Vpu::new(CODE);
        for (i, b) in bytes.iter().enumerate() {
            m.store8(CODE + i as u32, *b).unwrap();
        }
        m.store16(CODE + bytes.len() as u32, NOP).unwrap();
        for e in 0..16u32 {
            v.vrf.write(60, e, 2, A[e as usize]);
            v.vrf.write(61, e, 2, B[e as usize]);
        }
        v.regs.pc = CODE;
        assert_eq!(v.step(&mut m), Step::Ran, "{name}: {:?}", v.stopped);
        let got: Vec<u32> = (0..16).map(|e| v.vrf.read(*row as u8, e, 2)).collect();
        assert_eq!(&got[..], *want, "{name}");
    }
}

/// The multiplies, against the same board.
///
/// `mull` keeps the low half of the product, `mulm` shifts it right by eight,
/// `mulhd` keeps the high half and `mulhn` rounds while doing so — each with
/// its operands read signed or unsigned as the suffix says. Measured over the
/// edge-case vectors in `probes/alu2-vectors.hex`.
#[test]
fn the_measured_multiplies() {
    const A: [u32; 16] = [
        0x7fff, 0x8000, 0xffff, 0x0001, 0x4000, 0xc000, 0x0010, 0x8001, 0x0003, 0xfffe, 0x00ff,
        0xff00, 0x1234, 0xedcc, 0x0000, 0x7f00,
    ];
    const B: [u32; 16] = [
        0x0001, 0x0001, 0x0002, 0xffff, 0x4000, 0x4000, 0x0004, 0x7fff, 0x0002, 0x0003, 0x0100,
        0x0080, 0x0001, 0x0002, 0x0001, 0x0100,
    ];
    #[allow(clippy::type_complexity)]
    const CASES: &[(&str, usize, &[u8], &[u32])] = &[
        (
            "vmull.ss",
            0,
            &[0x80, 0xf5, 0x23, 0x80, 0x3d, 0xc2],
            &[
                0x7fff, 0x8000, 0xfffe, 0xffff, 0x0000, 0x0000, 0x0040, 0xffff, 0x0006, 0xfffa,
                0xff00, 0x8000, 0x1234, 0xdb98, 0x0000, 0x0000,
            ],
        ),
        (
            "vmulls.ss",
            1,
            &[0x88, 0xf5, 0x63, 0x80, 0x3d, 0xc2],
            &[
                0x7fff, 0x8000, 0xfffe, 0xffff, 0x7fff, 0x8000, 0x0040, 0x8000, 0x0006, 0xfffa,
                0x7fff, 0x8000, 0x1234, 0xdb98, 0x0000, 0x7fff,
            ],
        ),
        (
            "vmulm.ss",
            2,
            &[0x90, 0xf5, 0xa3, 0x80, 0x3d, 0xc2],
            &[
                0x007f, 0xff80, 0xffff, 0xffff, 0x0000, 0x0000, 0x0000, 0x00ff, 0x0000, 0xffff,
                0x00ff, 0xff80, 0x0012, 0xffdb, 0x0000, 0x7f00,
            ],
        ),
        (
            "vmulms.ss",
            3,
            &[0x98, 0xf5, 0xe3, 0x80, 0x3d, 0xc2],
            &[
                0x007f, 0xff80, 0xffff, 0xffff, 0x7fff, 0x8000, 0x0000, 0x8000, 0x0000, 0xffff,
                0x00ff, 0xff80, 0x0012, 0xffdb, 0x0000, 0x7f00,
            ],
        ),
        (
            "vmulhd.ss",
            4,
            &[0xa0, 0xf5, 0x23, 0x81, 0x3d, 0xc2],
            &[
                0x0000, 0xffff, 0xffff, 0xffff, 0x1000, 0xf000, 0x0000, 0xc000, 0x0000, 0xffff,
                0x0000, 0xffff, 0x0000, 0xffff, 0x0000, 0x007f,
            ],
        ),
        (
            "vmulhd.su",
            5,
            &[0xa8, 0xf5, 0x63, 0x81, 0x3d, 0xc2],
            &[
                0x0000, 0xffff, 0xffff, 0x0000, 0x1000, 0xf000, 0x0000, 0xc000, 0x0000, 0xffff,
                0x0000, 0xffff, 0x0000, 0xffff, 0x0000, 0x007f,
            ],
        ),
        (
            "vmulhd.us",
            6,
            &[0xb0, 0xf5, 0xa3, 0x81, 0x3d, 0xc2],
            &[
                0x0000, 0x0000, 0x0001, 0xffff, 0x1000, 0x3000, 0x0000, 0x3fff, 0x0000, 0x0002,
                0x0000, 0x007f, 0x0000, 0x0001, 0x0000, 0x007f,
            ],
        ),
        (
            "vmulhd.uu",
            7,
            &[0xb8, 0xf5, 0xe3, 0x81, 0x3d, 0xc2],
            &[
                0x0000, 0x0000, 0x0001, 0x0000, 0x1000, 0x3000, 0x0000, 0x3fff, 0x0000, 0x0002,
                0x0000, 0x007f, 0x0000, 0x0001, 0x0000, 0x007f,
            ],
        ),
        (
            "vmulhn.ss",
            8,
            &[0xc0, 0xf5, 0x23, 0x82, 0x3d, 0xc2],
            &[
                0x0000, 0x0000, 0x0000, 0x0000, 0x1000, 0xf000, 0x0000, 0xc001, 0x0000, 0x0000,
                0x0001, 0x0000, 0x0000, 0x0000, 0x0000, 0x007f,
            ],
        ),
        (
            "vmulhn.su",
            9,
            &[0xc8, 0xf5, 0x63, 0x82, 0x3d, 0xc2],
            &[
                0x0000, 0x0000, 0x0000, 0x0001, 0x1000, 0xf000, 0x0000, 0xc001, 0x0000, 0x0000,
                0x0001, 0x0000, 0x0000, 0x0000, 0x0000, 0x007f,
            ],
        ),
        (
            "vmulhn.us",
            10,
            &[0xd0, 0xf5, 0xa3, 0x82, 0x3d, 0xc2],
            &[
                0x0000, 0x0001, 0x0002, 0x0000, 0x1000, 0x3000, 0x0000, 0x4000, 0x0000, 0x0003,
                0x0001, 0x0080, 0x0000, 0x0002, 0x0000, 0x007f,
            ],
        ),
        (
            "vmulhn.uu",
            11,
            &[0xd8, 0xf5, 0xe3, 0x82, 0x3d, 0xc2],
            &[
                0x0000, 0x0001, 0x0002, 0x0001, 0x1000, 0x3000, 0x0000, 0x4000, 0x0000, 0x0003,
                0x0001, 0x0080, 0x0000, 0x0002, 0x0000, 0x007f,
            ],
        ),
    ];

    for (name, row, bytes, want) in CASES {
        let mut m = machine();
        let mut v = Vpu::new(CODE);
        for (i, b) in bytes.iter().enumerate() {
            m.store8(CODE + i as u32, *b).unwrap();
        }
        m.store16(CODE + bytes.len() as u32, NOP).unwrap();
        for e in 0..16u32 {
            v.vrf.write(60, e, 2, A[e as usize]);
            v.vrf.write(61, e, 2, B[e as usize]);
        }
        v.regs.pc = CODE;
        assert_eq!(v.step(&mut m), Step::Ran, "{name}: {:?}", v.stopped);
        let got: Vec<u32> = (0..16).map(|e| v.vrf.read(*row as u8, e, 2)).collect();
        assert_eq!(&got[..], *want, "{name}");
    }
}

/// A whole probe program, run on the board and replayed here.
///
/// `examples-on-real-hardware/vpu-probe/probes/accmix.s` loads two vectors,
/// runs three multiplies, accumulates a sum three times, takes it back off
/// again, does the multiply-accumulate the codec code is built out of, and
/// dumps the register file with `v32st HY(0++,0),(r0+=r3) REP64`. The rows
/// below are the ones a Raspberry Pi 4B d03115 wrote; the model runs the same
/// bytes over the same memory and has to write them too.
#[test]
fn a_whole_probe_program_replays() {
    const CODE_BYTES: &[u8] = &[
        0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
        0x04, 0xc0, 0xfb, 0x00, 0x00, 0x08, 0xf0, 0x38, 0x8f, 0x84, 0x03, 0x08, 0xf8, 0x78, 0x8f,
        0xa0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x90, 0xf5, 0x23, 0x80, 0x3d, 0xc2, 0xc0, 0xf5, 0x63,
        0x80, 0x3d, 0xc2, 0x80, 0xf5, 0xa3, 0x80, 0x3d, 0xc2, 0x00, 0xfd, 0xe3, 0x80, 0x3d, 0xc2,
        0xc0, 0xf3, 0xbc, 0x09, 0x00, 0xfd, 0x23, 0x81, 0x3d, 0xc2, 0xc0, 0xf3, 0xbc, 0x08, 0x00,
        0xfd, 0x63, 0x81, 0x3d, 0xc2, 0xc0, 0xf3, 0xbc, 0x08, 0x00, 0xfd, 0xa3, 0x81, 0x3d, 0xc2,
        0xc0, 0xf3, 0xbc, 0x0b, 0x00, 0xfd, 0xe3, 0x81, 0x3d, 0xc2, 0xc0, 0xf3, 0xfc, 0x0a, 0x90,
        0xfd, 0x23, 0xe0, 0x3d, 0xc2, 0xc0, 0xf3, 0xbc, 0x0b, 0x90, 0xfd, 0x23, 0x82, 0x3d, 0xc2,
        0xc0, 0xf3, 0xbc, 0x0a, 0x02, 0xfc, 0x78, 0x82, 0x15, 0x04, 0xc0, 0xfb, 0x00, 0x00, 0x90,
        0xf4, 0x63, 0x83, 0x3d, 0xc2, 0x96, 0xf8, 0x30, 0xe0, 0x80, 0x03, 0xe0, 0x33, 0x00, 0x00,
        0x5a, 0x00,
    ];
    const VECTORS: &[u8] = &[
        0xff, 0x7f, 0x00, 0x80, 0xff, 0xff, 0x01, 0x00, 0x00, 0x40, 0x00, 0xc0, 0x10, 0x00, 0x01,
        0x80, 0x03, 0x00, 0xfe, 0xff, 0xff, 0x00, 0x00, 0xff, 0x34, 0x12, 0xcc, 0xed, 0x00, 0x00,
        0x00, 0x7f, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0xff, 0xff, 0x00, 0x40, 0x00, 0x40, 0x04,
        0x00, 0xff, 0x7f, 0x02, 0x00, 0x03, 0x00, 0x00, 0x01, 0x80, 0x00, 0x01, 0x00, 0x02, 0x00,
        0x01, 0x00, 0x00, 0x01,
    ];
    const ROWS: &[(usize, &str)] = &[
    (0, "7f00000080ff0000ffff0000ffff0000000000000000000000000000ff00000000000000ffff0000ff00000080ff000012000000dbff000000000000007f0000"),
    (1, "000000000000000000000000000000000010000000f000000000000001c00000000000000000000001000000000000000000000000000000000000007f000000"),
    (2, "ff7f000000800000feff0000ffff0000000000000000000040000000ffff000006000000faff000000ff0000008000003412000098db00000000000000000000"),
    (3, "00800000018000000100000000000000008000000000000014000000000000000500000001000000ff01000080ff000035120000ceed00000100000000800000"),
    (4, "00000000020000000200000000000000000000000000000028000000000000000a00000002000000fe03000000ff00006a2400009cdb00000200000000000000"),
    (5, "0080000003800000030000000000000000800000000000003c000000000000000f00000003000000fd05000080fe00009f3600006ac900000300000000800000"),
    (6, "00800000018000000100000000000000008000000000000014000000000000000500000001000000ff01000080ff000035120000ceed00000100000000800000"),
    (7, "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"),
    (8, "fe00000000ff0000feff0000feff0000000000000000000000000000fe01000000000000feff0000fe01000000ff000024000000b6ff00000000000000fe0000"),
    (9, "15000000150000001500000015000000150000001500000015000000150000001500000015000000150000001500000015000000150000001500000015000000"),
    (10, "15000000150000001500000015000000150000001500000015000000150000001500000015000000150000001500000015000000150000001500000015000000"),
    (11, "15000000150000001500000015000000150000001500000015000000150000001500000015000000150000001500000015000000150000001500000015000000"),
    (12, "15000000150000001500000015000000150000001500000015000000150000001500000015000000150000001500000015000000150000001500000015000000"),
    (13, "fe7f000001800000fdff0000feff0000000000000080000014000000feff000001000000fdff0000ff01000080ff000035120000ceed000001000000007e0000"),
    (60, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
    (61, "010000000100000002000000ffff0000004000000040000004000000ff7f00000200000003000000000100008000000001000000020000000100000000010000")
    ];

    let mut m = machine();
    let mut v = Vpu::new(CODE);
    for (i, b) in CODE_BYTES.iter().enumerate() {
        m.store8(CODE + i as u32, *b).unwrap();
    }
    // The marker page the harness fills, and the test vectors a page later.
    for i in 0..4096u32 {
        m.store8(0x4000 + i, (i + 1) as u8).unwrap();
    }
    for i in 0..4096u32 {
        m.store8(0x5000 + i, 0).unwrap();
    }
    for (i, b) in VECTORS.iter().enumerate() {
        m.store8(0x5000 + i as u32, *b).unwrap();
    }
    v.regs.set(0, 0x8000); // where the dump lands
    v.regs.set(1, 0x4000);
    v.regs.pc = CODE;
    for _ in 0..24 {
        if v.regs.pc == CODE + CODE_BYTES.len() as u32 - 2 {
            break; // the trailing `rts`
        }
        assert_eq!(
            v.step(&mut m),
            Step::Ran,
            "at {:#x}: {:?}",
            v.regs.pc,
            v.stopped
        );
    }

    for (row, want) in ROWS {
        let got: String = (0..64)
            .map(|c| format!("{:02x}", m.load8(0x8000 + (*row as u32) * 64 + c).unwrap()))
            .collect();
        assert_eq!(&got, want, "row {row}");
    }
}
