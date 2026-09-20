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
/// ordinary load but for the `*`, which changes nothing the register file or
/// memory can see.
///
/// Measured with `probes/star.s` on a Raspberry Pi 4B d03115: a `*` on the
/// destination, on a source, on a `+rN` slot, under `REP`, on a load and on a
/// store all leave exactly what the same instruction without it leaves.
#[test]
fn a_star_on_a_slot_changes_nothing() {
    // `v16ld HX(0++,0),(r2+=r5) REP4` and the same with a `*`, as
    // `binutils-vc4` assembles them: one bit apart.
    let plain = [0xF80A, 0x8038, 0x0380, 0xF940, 0x0008, NOP];
    let starred = [0xF80A, 0x8038, 0x0380, 0xFD40, 0x0008, NOP];

    let mut rows = Vec::new();
    for code in [plain, starred] {
        let mut m = machine();
        let mut v = Vpu::new(CODE);
        load_code(&mut m, CODE, &code);
        fill(&mut m, 0x4000, 256);
        v.regs.set(2, 0x4000);
        v.regs.set(5, 64);
        step(&mut v, &mut m);
        rows.push(
            (0..4)
                .flat_map(|r| (0..16).map(move |e| (r, e)))
                .map(|(r, e)| v.vrf.read(r, e, 2))
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(rows[0], rows[1], "the `*` moved different bytes");
    assert!(rows[0].iter().any(|&w| w != 0), "nothing was transferred");
}

/// The "dash" test must not be a loose field check: a near neighbour that names
/// a real vector register where this model expects a bare dash has to fault too.
/// A load's A slot is ignored on hardware, whatever it names.
///
/// Measured with `probes/ldodd.s` on a Raspberry Pi 4B d03115:
/// `v16ld HX(1,0),-+r5,(r4)` and `v16ld HX(3,0),HX(20,0),(r4)` both move
/// exactly what the plain `v16ld HX(0,0),(r4)` moves. A **store** is another
/// matter: the same addend on one wrote nothing where the plain store wrote,
/// so that form still faults.
#[test]
fn a_loads_a_slot_is_ignored() {
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
    fill(&mut m, 0, 16);

    assert_eq!(v.step(&mut m), Step::Ran, "{:?}", v.stopped);
    for e in 0..16u32 {
        assert_eq!(v.vrf.read(0, e, 1), 0, "a dash destination writes nothing");
    }
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
fn vector_store_under_ifn_moves_the_lanes_the_flags_call_negative() {
    let mut m = machine();
    let mut v = Vpu::new(CODE);
    let mut st = V32ST_R3_IFZ;
    st[4] = (st[4] & !0xE000) | 0x8000; // predicate 4, `IFN`
    load_code(&mut m, CODE, &concat(&[&st, &[NOP]]));
    v.regs.set(3, 0x5000);
    v.vrf.lane_n = 0b0000_0000_0000_0101; // lanes 0 and 2
    for e in 0..16u32 {
        v.vrf.write(0, e, 4, 0x1000 + e);
    }

    step(&mut v, &mut m);

    for lane in 0..16u32 {
        let want = if v.vrf.lane_n & (1 << lane) != 0 {
            0x1000 + lane
        } else {
            0
        };
        let got = (0..4).fold(0u32, |acc, i| {
            acc | (m.load8(0x5000 + lane * 4 + i).unwrap() as u32) << (8 * i)
        });
        assert_eq!(got, want, "lane {lane}");
    }
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

/// What the unit does when the operation is wider than the registers it
/// touches — the commonest shape in `start4.elf`, and the one that used to
/// fault.
///
/// A source is read at its register's own width and widened into the
/// operation's: a byte unsigned, a halfword signed. The result comes back the
/// other way, truncated into the destination's element — except for the
/// saturating ops, which clamp to what that element can hold: `0..=0xff` for a
/// byte register, signed for a wider one. A shift, rotate or reversal counts in
/// the operation's width, so `v32` takes five bits of B where `v16` takes four.
///
/// Measured on a Raspberry Pi 4B d03115 with `probes/wmix.s`, `wmix2.s` and
/// `wmix3.s`; the registers are the ones those probes load.
#[test]
fn the_measured_width_conversions() {
    // Row 60 and 61 hold `A` and `B` as byte registers — two bands of sixteen
    // elements each — and rows 62 and 63 the same pairs of bytes read as
    // halfwords.
    const A_LO: [u32; 16] = [
        0x80, 0xff, 0x01, 0x7f, 0x10, 0x00, 0xab, 0x34, 0x55, 0xf0, 0x0f, 0xc3, 0x02, 0x7e, 0x81,
        0xfe,
    ];
    const A_HI: [u32; 16] = [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x01,
        0x02,
    ];
    const B_LO: [u32; 16] = [
        0x80, 0x01, 0xff, 0x01, 0x20, 0x05, 0x55, 0x08, 0x03, 0x10, 0xf0, 0x3c, 0x04, 0x02, 0x7f,
        0x01,
    ];
    const B_HI: [u32; 16] = [
        0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x01, 0x02, 0x02, 0x02, 0x02, 0x03, 0x03, 0x03,
        0x03,
    ];
    // The 16-bit rows of the second and third runs, whose vectors are the
    // edge cases of `probes/wmix2-vectors.hex`.
    const A16: [u32; 16] = [
        0x7fff, 0x8000, 0xffff, 0x0001, 0x4000, 0xc000, 0x0010, 0x8001, 0x0003, 0xfffe, 0x00ff,
        0xff00, 0x1234, 0xedcc, 0x0000, 0x7f00,
    ];
    const B16: [u32; 16] = [
        0x0001, 0x0001, 0x0002, 0xffff, 0x4000, 0x4000, 0x0004, 0x7fff, 0x0002, 0x0003, 0x0100,
        0x0080, 0x0001, 0x0002, 0x0001, 0x0100,
    ];
    // Name, destination row, destination element width, the bytes as they
    // assemble, and what the board left in that row.
    #[allow(clippy::type_complexity)]
    const CASES: &[(&str, usize, u32, &[u8], &[u32])] = &[
        (
            "v16mov H(0,0),H(60,0)",
            0,
            1,
            &[0x00, 0xf4, 0x38, 0x00, 0x3c, 0x00],
            &[
                0x80, 0xff, 0x01, 0x7f, 0x10, 0x00, 0xab, 0x34, 0x55, 0xf0, 0x0f, 0xc3, 0x02, 0x7e,
                0x81, 0xfe,
            ],
        ),
        (
            "v16add H(1,0),H(60,0),H(61,0)",
            1,
            1,
            &[0x00, 0xf5, 0x43, 0x00, 0x3d, 0xc0],
            &[
                0x00, 0x00, 0x00, 0x80, 0x30, 0x05, 0x00, 0x3c, 0x58, 0x00, 0xff, 0xff, 0x06, 0x80,
                0x00, 0xff,
            ],
        ),
        (
            "v16adds H(2,0),H(60,0),H(61,0)",
            2,
            1,
            &[0x08, 0xf5, 0x83, 0x00, 0x3d, 0xc0],
            &[
                0xff, 0xff, 0xff, 0x80, 0x30, 0x05, 0xff, 0x3c, 0x58, 0xff, 0xff, 0xff, 0x06, 0x80,
                0xff, 0xff,
            ],
        ),
        (
            "v16sub H(3,0),H(60,0),H(61,0)",
            3,
            1,
            &[0x20, 0xf5, 0xc3, 0x00, 0x3d, 0xc0],
            &[
                0x00, 0xfe, 0x02, 0x7e, 0xf0, 0xfb, 0x56, 0x2c, 0x52, 0xe0, 0x1f, 0x87, 0xfe, 0x7c,
                0x02, 0xfd,
            ],
        ),
        (
            "v16subs H(4,0),H(60,0),H(61,0)",
            4,
            1,
            &[0x28, 0xf5, 0x03, 0x01, 0x3d, 0xc0],
            &[
                0x00, 0xfe, 0x00, 0x7e, 0x00, 0x00, 0x56, 0x2c, 0x52, 0xe0, 0x00, 0x87, 0x00, 0x7c,
                0x02, 0xfd,
            ],
        ),
        (
            "v16shl H(5,0),H(60,0),H(61,0)",
            5,
            1,
            &[0x40, 0xf4, 0x43, 0x01, 0x3d, 0xc0],
            &[
                0x80, 0xfe, 0x00, 0xfe, 0x10, 0x00, 0x60, 0x00, 0xa8, 0xf0, 0x0f, 0x00, 0x20, 0xf8,
                0x00, 0xfc,
            ],
        ),
        (
            "v16lsr H(6,0),H(60,0),H(61,0)",
            6,
            1,
            &[0x50, 0xf4, 0x83, 0x01, 0x3d, 0xc0],
            &[
                0x80, 0x7f, 0x00, 0x3f, 0x10, 0x00, 0x05, 0x00, 0x0a, 0xf0, 0x0f, 0x00, 0x00, 0x1f,
                0x00, 0x7f,
            ],
        ),
        (
            "v16asr H(7,0),H(60,0),H(61,0)",
            7,
            1,
            &[0x58, 0xf4, 0xc3, 0x01, 0x3d, 0xc0],
            &[
                0x80, 0x7f, 0x00, 0x3f, 0x10, 0x00, 0x05, 0x00, 0x0a, 0xf0, 0x0f, 0x00, 0x00, 0x1f,
                0x00, 0x7f,
            ],
        ),
        (
            "v16and H(8,0),H(60,0),H(61,0)",
            8,
            1,
            &[0x80, 0xf4, 0x03, 0x02, 0x3d, 0xc0],
            &[
                0x80, 0x01, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x10, 0x00, 0x00, 0x00, 0x02,
                0x01, 0x00,
            ],
        ),
        (
            "v16min H(9,0),H(60,0),H(61,0)",
            9,
            1,
            &[0xc0, 0xf4, 0x43, 0x02, 0x3d, 0xc0],
            &[
                0x80, 0x01, 0x01, 0x01, 0x10, 0x00, 0x55, 0x08, 0x03, 0x10, 0x0f, 0x3c, 0x02, 0x02,
                0x7f, 0x01,
            ],
        ),
        (
            "v16dist H(10,0),H(60,0),H(61,0)",
            10,
            1,
            &[0xd0, 0xf4, 0x83, 0x02, 0x3d, 0xc0],
            &[
                0x00, 0xfe, 0xfe, 0x7e, 0x10, 0x05, 0x56, 0x2c, 0x52, 0xe0, 0xe1, 0x87, 0x02, 0x7c,
                0x02, 0xfd,
            ],
        ),
        (
            "v16even H(11,0),H(60,0),H(61,0)",
            11,
            1,
            &[0x10, 0xf4, 0xc3, 0x02, 0x3d, 0xc0],
            &[
                0x80, 0x01, 0x10, 0xab, 0x55, 0x0f, 0x02, 0x81, 0x80, 0xff, 0x20, 0x55, 0x03, 0xf0,
                0x04, 0x7f,
            ],
        ),
        (
            "v16count H(12,0),H(60,0),H(61,0)",
            12,
            1,
            &[0xa0, 0xf4, 0x03, 0x03, 0x3d, 0xc0],
            &[
                0x02, 0x09, 0x09, 0x08, 0x02, 0x02, 0x09, 0x04, 0x06, 0x05, 0x08, 0x08, 0x02, 0x07,
                0x09, 0x08,
            ],
        ),
        (
            "v16max H(13,0),H(60,0),H(61,0)",
            13,
            1,
            &[0xc8, 0xf4, 0x43, 0x03, 0x3d, 0xc0],
            &[
                0x80, 0xff, 0xff, 0x7f, 0x20, 0x05, 0xab, 0x34, 0x55, 0xf0, 0xf0, 0xc3, 0x04, 0x7e,
                0x81, 0xfe,
            ],
        ),
        (
            "v16add HX(14,0),H(60,0),H(61,0)",
            14,
            2,
            &[0x00, 0xf5, 0x83, 0x83, 0x3d, 0xc0],
            &[
                0x0100, 0x0100, 0x0100, 0x0080, 0x0030, 0x0005, 0x0100, 0x003c, 0x0058, 0x0100,
                0x00ff, 0x00ff, 0x0006, 0x0080, 0x0100, 0x00ff,
            ],
        ),
        (
            "v16adds HX(15,0),H(60,0),H(61,0)",
            15,
            2,
            &[0x08, 0xf5, 0xc3, 0x83, 0x3d, 0xc0],
            &[
                0x0100, 0x0100, 0x0100, 0x0080, 0x0030, 0x0005, 0x0100, 0x003c, 0x0058, 0x0100,
                0x00ff, 0x00ff, 0x0006, 0x0080, 0x0100, 0x00ff,
            ],
        ),
        (
            "v16mov HX(16,0),H(60,0)",
            16,
            2,
            &[0x00, 0xf4, 0x38, 0x84, 0x3c, 0x00],
            &[
                0x0080, 0x00ff, 0x0001, 0x007f, 0x0010, 0x0000, 0x00ab, 0x0034, 0x0055, 0x00f0,
                0x000f, 0x00c3, 0x0002, 0x007e, 0x0081, 0x00fe,
            ],
        ),
        (
            "v32add H(17,0),H(60,0),H(61,0)",
            17,
            1,
            &[0x00, 0xf7, 0x43, 0x04, 0x3d, 0xc0],
            &[
                0x00, 0x00, 0x00, 0x80, 0x30, 0x05, 0x00, 0x3c, 0x58, 0x00, 0xff, 0xff, 0x06, 0x80,
                0x00, 0xff,
            ],
        ),
        (
            "v32adds H(18,0),H(60,0),H(61,0)",
            18,
            1,
            &[0x08, 0xf7, 0x83, 0x04, 0x3d, 0xc0],
            &[
                0xff, 0xff, 0xff, 0x80, 0x30, 0x05, 0xff, 0x3c, 0x58, 0xff, 0xff, 0xff, 0x06, 0x80,
                0xff, 0xff,
            ],
        ),
        (
            "v32mov H(19,0),H(60,0)",
            19,
            1,
            &[0x00, 0xf6, 0xf8, 0x04, 0x3c, 0x00],
            &[
                0x80, 0xff, 0x01, 0x7f, 0x10, 0x00, 0xab, 0x34, 0x55, 0xf0, 0x0f, 0xc3, 0x02, 0x7e,
                0x81, 0xfe,
            ],
        ),
        (
            "v32mov HY(0,0),H(60,0)",
            0,
            4,
            &[0x00, 0xf6, 0x38, 0xc0, 0x3c, 0x00],
            &[
                0x00000080, 0x000000ff, 0x00000001, 0x0000007f, 0x00000010, 0x00000000, 0x000000ab,
                0x00000034, 0x00000055, 0x000000f0, 0x0000000f, 0x000000c3, 0x00000002, 0x0000007e,
                0x00000081, 0x000000fe,
            ],
        ),
        (
            "v32add HY(1,0),H(60,0),H(61,0)",
            1,
            4,
            &[0x00, 0xf7, 0x43, 0xc0, 0x3d, 0xc0],
            &[
                0x00000100, 0x00000100, 0x00000100, 0x00000080, 0x00000030, 0x00000005, 0x00000100,
                0x0000003c, 0x00000058, 0x00000100, 0x000000ff, 0x000000ff, 0x00000006, 0x00000080,
                0x00000100, 0x000000ff,
            ],
        ),
        (
            "v32mov HY(2,0),HX(62,0)",
            2,
            4,
            &[0x00, 0xf6, 0xb8, 0xc0, 0x3e, 0x02],
            &[
                0x00007fff, 0xffff8000, 0xffffffff, 0x00000001, 0x00004000, 0xffffc000, 0x00000010,
                0xffff8001, 0x00000003, 0xfffffffe, 0x000000ff, 0xffffff00, 0x00001234, 0xffffedcc,
                0x00000000, 0x00007f00,
            ],
        ),
        (
            "v32sub HY(3,0),HX(62,0),HX(63,0)",
            3,
            4,
            &[0x20, 0xf7, 0xe3, 0xc0, 0x3f, 0xe2],
            &[
                0x00007ffe, 0xffff7fff, 0xfffffffd, 0x00000002, 0x00000000, 0xffff8000, 0x0000000c,
                0xffff0002, 0x00000001, 0xfffffffb, 0xffffffff, 0xfffffe80, 0x00001233, 0xffffedca,
                0xffffffff, 0x00007e00,
            ],
        ),
        (
            "v16mov HX(4,0),H(60,0)",
            4,
            2,
            &[0x00, 0xf4, 0x38, 0x81, 0x3c, 0x00],
            &[
                0x0080, 0x00ff, 0x0001, 0x007f, 0x0010, 0x0000, 0x00ab, 0x0034, 0x0055, 0x00f0,
                0x000f, 0x00c3, 0x0002, 0x007e, 0x0081, 0x00fe,
            ],
        ),
        (
            "v32adds HX(5,0),HX(62,0),HX(63,0)",
            5,
            2,
            &[0x08, 0xf7, 0x63, 0x81, 0x3f, 0xe2],
            &[
                0x7fff, 0x8001, 0x0001, 0x0000, 0x7fff, 0x0000, 0x0014, 0x0000, 0x0005, 0x0001,
                0x01ff, 0xff80, 0x1235, 0xedce, 0x0001, 0x7fff,
            ],
        ),
        (
            "v32subs HX(6,0),HX(62,0),HX(63,0)",
            6,
            2,
            &[0x28, 0xf7, 0xa3, 0x81, 0x3f, 0xe2],
            &[
                0x7ffe, 0x8000, 0xfffd, 0x0002, 0x0000, 0x8000, 0x000c, 0x8000, 0x0001, 0xfffb,
                0xffff, 0xfe80, 0x1233, 0xedca, 0xffff, 0x7e00,
            ],
        ),
        (
            "v32adds H(7,0),H(60,0),H(61,0)",
            7,
            1,
            &[0x08, 0xf7, 0xc3, 0x01, 0x3d, 0xc0],
            &[
                0xff, 0xff, 0xff, 0x80, 0x30, 0x05, 0xff, 0x3c, 0x58, 0xff, 0xff, 0xff, 0x06, 0x80,
                0xff, 0xff,
            ],
        ),
        (
            "v32subs H(8,0),H(60,0),H(61,0)",
            8,
            1,
            &[0x28, 0xf7, 0x03, 0x02, 0x3d, 0xc0],
            &[
                0x00, 0xfe, 0x00, 0x7e, 0x00, 0x00, 0x56, 0x2c, 0x52, 0xe0, 0x00, 0x87, 0x00, 0x7c,
                0x02, 0xfd,
            ],
        ),
        (
            "v32adds HY(9,0),HX(62,0),HX(63,0)",
            9,
            4,
            &[0x08, 0xf7, 0x63, 0xc2, 0x3f, 0xe2],
            &[
                0x00008000, 0xffff8001, 0x00000001, 0x00000000, 0x00008000, 0x00000000, 0x00000014,
                0x00000000, 0x00000005, 0x00000001, 0x000001ff, 0xffffff80, 0x00001235, 0xffffedce,
                0x00000001, 0x00008000,
            ],
        ),
        (
            "v32sub HX(10,0),HX(62,0),HX(63,0)",
            10,
            2,
            &[0x20, 0xf7, 0xa3, 0x82, 0x3f, 0xe2],
            &[
                0x7ffe, 0x7fff, 0xfffd, 0x0002, 0x0000, 0x8000, 0x000c, 0x0002, 0x0001, 0xfffb,
                0xffff, 0xfe80, 0x1233, 0xedca, 0xffff, 0x7e00,
            ],
        ),
        (
            "v16adds HX(11,0),HX(62,0),HX(63,0)",
            11,
            2,
            &[0x08, 0xf5, 0xe3, 0x82, 0x3f, 0xe2],
            &[
                0x7fff, 0x8001, 0x0001, 0x0000, 0x7fff, 0x0000, 0x0014, 0x0000, 0x0005, 0x0001,
                0x01ff, 0xff80, 0x1235, 0xedce, 0x0001, 0x7fff,
            ],
        ),
        (
            "v16mov H(12,0),HX(62,0)",
            12,
            1,
            &[0x00, 0xf4, 0x38, 0x03, 0x3e, 0x02],
            &[
                0xff, 0x00, 0xff, 0x01, 0x00, 0x00, 0x10, 0x01, 0x03, 0xfe, 0xff, 0x00, 0x34, 0xcc,
                0x00, 0x00,
            ],
        ),
        (
            "v32mov H(13,0),HX(62,0)",
            13,
            1,
            &[0x00, 0xf6, 0x78, 0x03, 0x3e, 0x02],
            &[
                0xff, 0x00, 0xff, 0x01, 0x00, 0x00, 0x10, 0x01, 0x03, 0xfe, 0xff, 0x00, 0x34, 0xcc,
                0x00, 0x00,
            ],
        ),
        (
            "v32mov HX(14,0),H(60,0)",
            14,
            2,
            &[0x00, 0xf6, 0xb8, 0x83, 0x3c, 0x00],
            &[
                0x0080, 0x00ff, 0x0001, 0x007f, 0x0010, 0x0000, 0x00ab, 0x0034, 0x0055, 0x00f0,
                0x000f, 0x00c3, 0x0002, 0x007e, 0x0081, 0x00fe,
            ],
        ),
        (
            "v16dist HX(15,0),H(60,0),H(61,0)",
            15,
            2,
            &[0xd0, 0xf4, 0xc3, 0x83, 0x3d, 0xc0],
            &[
                0x0000, 0x00fe, 0x00fe, 0x007e, 0x0010, 0x0005, 0x0056, 0x002c, 0x0052, 0x00e0,
                0x00e1, 0x0087, 0x0002, 0x007c, 0x0002, 0x00fd,
            ],
        ),
        (
            "v16sub HX(16,0),H(60,0),H(61,0)",
            16,
            2,
            &[0x20, 0xf5, 0x03, 0x84, 0x3d, 0xc0],
            &[
                0x0000, 0x00fe, 0xff02, 0x007e, 0xfff0, 0xfffb, 0x0056, 0x002c, 0x0052, 0x00e0,
                0xff1f, 0x0087, 0xfffe, 0x007c, 0x0002, 0x00fd,
            ],
        ),
        (
            "v16subs HX(17,0),H(60,0),H(61,0)",
            17,
            2,
            &[0x28, 0xf5, 0x43, 0x84, 0x3d, 0xc0],
            &[
                0x0000, 0x00fe, 0xff02, 0x007e, 0xfff0, 0xfffb, 0x0056, 0x002c, 0x0052, 0x00e0,
                0xff1f, 0x0087, 0xfffe, 0x007c, 0x0002, 0x00fd,
            ],
        ),
        (
            "v16asr HX(18,0),HX(62,0),HX(63,0)",
            18,
            2,
            &[0x58, 0xf4, 0xa3, 0x84, 0x3f, 0xe2],
            &[
                0x3fff, 0xc000, 0xffff, 0x0000, 0x4000, 0xc000, 0x0001, 0xffff, 0x0000, 0xffff,
                0x00ff, 0xff00, 0x091a, 0xfb73, 0x0000, 0x7f00,
            ],
        ),
        (
            "v32asr HX(19,0),HX(62,0),HX(63,0)",
            19,
            2,
            &[0x58, 0xf6, 0xe3, 0x84, 0x3f, 0xe2],
            &[
                0x3fff, 0xc000, 0xffff, 0x0000, 0x4000, 0xc000, 0x0001, 0xffff, 0x0000, 0xffff,
                0x00ff, 0xff00, 0x091a, 0xfb73, 0x0000, 0x7f00,
            ],
        ),
        (
            "v32lsr HY(20,0),HX(62,0),HX(63,0)",
            20,
            4,
            &[0x50, 0xf6, 0x23, 0xc5, 0x3f, 0xe2],
            &[
                0x00003fff, 0x7fffc000, 0x3fffffff, 0x00000000, 0x00004000, 0xffffc000, 0x00000001,
                0x00000001, 0x00000000, 0x1fffffff, 0x000000ff, 0xffffff00, 0x0000091a, 0x3ffffb73,
                0x00000000, 0x00007f00,
            ],
        ),
        (
            "v32min HY(21,0),HX(62,0),HX(63,0)",
            21,
            4,
            &[0xc0, 0xf6, 0x63, 0xc5, 0x3f, 0xe2],
            &[
                0x00000001, 0xffff8000, 0xffffffff, 0xffffffff, 0x00004000, 0xffffc000, 0x00000004,
                0xffff8001, 0x00000002, 0xfffffffe, 0x000000ff, 0xffffff00, 0x00000001, 0xffffedcc,
                0x00000000, 0x00000100,
            ],
        ),
        (
            "v16count H(1,0),H(60,0),H(61,0)",
            1,
            1,
            &[0xa0, 0xf4, 0x43, 0x00, 0x3d, 0xc0],
            &[
                0x02, 0x09, 0x09, 0x08, 0x02, 0x02, 0x09, 0x04, 0x06, 0x05, 0x08, 0x08, 0x02, 0x07,
                0x09, 0x08,
            ],
        ),
        (
            "v16bitrev H(2,0),H(60,0),H(61,0)",
            2,
            1,
            &[0x30, 0xf4, 0x83, 0x00, 0x3d, 0xc0],
            &[
                0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x1a, 0x2c, 0x05, 0x00, 0x00, 0x30, 0x04, 0x01,
                0x80, 0x00,
            ],
        ),
        (
            "v32bitrev H(3,0),H(60,0),H(61,0)",
            3,
            1,
            &[0x30, 0xf6, 0xc3, 0x00, 0x3d, 0xc0],
            &[
                0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x2c, 0x05, 0x00, 0x00, 0x00, 0x04, 0x01,
                0x00, 0x00,
            ],
        ),
        (
            "v16ror H(4,0),H(60,0),H(61,0)",
            4,
            1,
            &[0x38, 0xf4, 0x03, 0x01, 0x3d, 0xc0],
            &[
                0x80, 0x7f, 0x02, 0x3f, 0x10, 0x00, 0x05, 0x00, 0x0a, 0xf0, 0x0f, 0x30, 0x00, 0x1f,
                0x02, 0x7f,
            ],
        ),
        (
            "v16msb H(5,0),H(60,0),H(61,0)",
            5,
            1,
            &[0xa8, 0xf4, 0x43, 0x01, 0x3d, 0xc0],
            &[
                0x07, 0x07, 0x07, 0x06, 0x05, 0x02, 0x07, 0x05, 0x06, 0x07, 0x07, 0x07, 0x02, 0x06,
                0x07, 0x07,
            ],
        ),
        (
            "v16clip H(6,0),H(60,0),H(61,0)",
            6,
            1,
            &[0xe0, 0xf4, 0x83, 0x01, 0x3d, 0xc0],
            &[
                0x80, 0x01, 0x01, 0x01, 0x10, 0x00, 0x55, 0x08, 0x03, 0x10, 0x0f, 0x3c, 0x02, 0x02,
                0x7f, 0x01,
            ],
        ),
        (
            "v16sign H(8,0),H(60,0),H(61,0)",
            8,
            1,
            &[0xe8, 0xf4, 0x03, 0x02, 0x3d, 0xc0],
            &[
                0x81, 0x02, 0x00, 0x02, 0x21, 0x05, 0x56, 0x09, 0x04, 0x11, 0xf1, 0x3d, 0x05, 0x03,
                0x80, 0x02,
            ],
        ),
        (
            "v16interl H(10,0),H(60,0),H(61,0)",
            10,
            1,
            &[0x20, 0xf4, 0x83, 0x02, 0x3d, 0xc0],
            &[
                0x80, 0x80, 0xff, 0x01, 0x01, 0xff, 0x7f, 0x01, 0x10, 0x20, 0x00, 0x05, 0xab, 0x55,
                0x34, 0x08,
            ],
        ),
        (
            "v16interh H(11,0),H(60,0),H(61,0)",
            11,
            1,
            &[0x28, 0xf4, 0xc3, 0x02, 0x3d, 0xc0],
            &[
                0x55, 0x03, 0xf0, 0x10, 0x0f, 0xf0, 0xc3, 0x3c, 0x02, 0x04, 0x7e, 0x02, 0x81, 0x7f,
                0xfe, 0x01,
            ],
        ),
        (
            "v16odd H(12,0),H(60,0),H(61,0)",
            12,
            1,
            &[0x18, 0xf4, 0x03, 0x03, 0x3d, 0xc0],
            &[
                0xff, 0x7f, 0x00, 0x34, 0xf0, 0xc3, 0x7e, 0xfe, 0x01, 0x01, 0x05, 0x08, 0x10, 0x3c,
                0x02, 0x01,
            ],
        ),
        (
            "v16shls H(14,0),H(60,0),H(61,0)",
            14,
            1,
            &[0x48, 0xf4, 0x83, 0x03, 0x3d, 0xc0],
            &[
                0x80, 0xff, 0xff, 0xfe, 0x10, 0x00, 0xff, 0xff, 0xff, 0xf0, 0x0f, 0xff, 0x20, 0xff,
                0xff, 0xff,
            ],
        ),
        (
            "v16dists H(15,0),H(60,0),H(61,0)",
            15,
            1,
            &[0xd8, 0xf4, 0xc3, 0x03, 0x3d, 0xc0],
            &[
                0x00, 0xfe, 0xfe, 0x7e, 0x10, 0x05, 0x56, 0x2c, 0x52, 0xe0, 0xe1, 0x87, 0x02, 0x7c,
                0x02, 0xfd,
            ],
        ),
        (
            "v32even H(17,0),H(60,0),H(61,0)",
            17,
            1,
            &[0x10, 0xf6, 0x43, 0x04, 0x3d, 0xc0],
            &[
                0x80, 0x01, 0x10, 0xab, 0x55, 0x0f, 0x02, 0x81, 0x80, 0xff, 0x20, 0x55, 0x03, 0xf0,
                0x04, 0x7f,
            ],
        ),
        (
            "v32bitrev HX(18,0),HX(62,0),HX(63,0)",
            18,
            2,
            &[0x30, 0xf6, 0xa3, 0x84, 0x3f, 0xe2],
            &[
                0x0001, 0x0000, 0x0003, 0x0000, 0x0000, 0xffff, 0x0000, 0xffff, 0x0003, 0x0003,
                0x0000, 0xffff, 0x0000, 0x0000, 0x0000, 0x0000,
            ],
        ),
        (
            "v16bitrev HX(19,0),H(60,0),H(61,0)",
            19,
            2,
            &[0x30, 0xf4, 0xc3, 0x84, 0x3d, 0xc0],
            &[
                0x0100, 0x0001, 0x4000, 0x0001, 0x0800, 0x0000, 0x001a, 0x002c, 0x0005, 0x0f00,
                0xf000, 0x0c30, 0x0004, 0x0001, 0x4080, 0x0000,
            ],
        ),
        (
            "v16msb HX(20,0),H(60,0),H(61,0)",
            20,
            2,
            &[0xa8, 0xf4, 0x03, 0x85, 0x3d, 0xc0],
            &[
                0x0007, 0x0007, 0x0007, 0x0006, 0x0005, 0x0002, 0x0007, 0x0005, 0x0006, 0x0007,
                0x0007, 0x0007, 0x0002, 0x0006, 0x0007, 0x0007,
            ],
        ),
        (
            "v16count HX(21,0),H(60,0),H(61,0)",
            21,
            2,
            &[0xa0, 0xf4, 0x43, 0x85, 0x3d, 0xc0],
            &[
                0x0002, 0x0009, 0x0009, 0x0008, 0x0002, 0x0002, 0x0009, 0x0004, 0x0006, 0x0005,
                0x0008, 0x0008, 0x0002, 0x0007, 0x0009, 0x0008,
            ],
        ),
        (
            "v32msb HY(22,0),HX(62,0),HX(63,0)",
            22,
            4,
            &[0xa8, 0xf6, 0xa3, 0xc5, 0x3f, 0xe2],
            &[
                0x0000000e, 0x0000001f, 0x0000001f, 0x0000001f, 0x0000000e, 0x0000001f, 0x00000004,
                0x0000001f, 0x00000001, 0x0000001f, 0x00000008, 0x0000001f, 0x0000000c, 0x0000001f,
                0x00000000, 0x0000000e,
            ],
        ),
        (
            "v32clip HY(23,0),HX(62,0),HX(63,0)",
            23,
            4,
            &[0xe0, 0xf6, 0xe3, 0xc5, 0x3f, 0xe2],
            &[
                0x00000001, 0x00000000, 0x00000000, 0x00000000, 0x00004000, 0x00000000, 0x00000004,
                0x00000000, 0x00000002, 0x00000000, 0x000000ff, 0x00000000, 0x00000001, 0x00000000,
                0x00000000, 0x00000100,
            ],
        ),
    ];

    for (name, row, w, bytes, want) in CASES {
        let mut m = machine();
        let mut v = Vpu::new(CODE);
        for (i, b) in bytes.iter().enumerate() {
            m.store8(CODE + i as u32, *b).unwrap();
        }
        m.store16(CODE + bytes.len() as u32, NOP).unwrap();
        for e in 0..16usize {
            v.vrf.write(60, e as u32, 1, A_LO[e]);
            v.vrf.write(60, e as u32 + 16, 1, A_HI[e]);
            v.vrf.write(61, e as u32, 1, B_LO[e]);
            v.vrf.write(61, e as u32 + 16, 1, B_HI[e]);
            v.vrf.write(62, e as u32, 2, A16[e]);
            v.vrf.write(63, e as u32, 2, B16[e]);
        }
        v.regs.pc = CODE;
        assert_eq!(v.step(&mut m), Step::Ran, "{name}: {:?}", v.stopped);
        let got: Vec<u32> = (0..16).map(|e| v.vrf.read(*row as u8, e, *w)).collect();
        assert_eq!(&got[..], *want, "{name}");
    }
}

/// The accumulator when the operation is wider than the registers it reads.
///
/// `examples-on-real-hardware/vpu-probe/probes/wacc.s` accumulates over byte
/// registers at 16 bits and over halfword registers at 32, clearing, writing
/// back and subtracting along the way, then dumps the file. Nothing about the
/// accumulator changes when the registers are narrower: it takes the result at
/// the operation's width, and the destination takes the accumulator, which is
/// what the `UACC` and `SACC` mnemonics ask for — both set `WBA` as well as
/// `ENA`. Rows from a Raspberry Pi 4B d03115.
#[test]
fn the_accumulator_across_a_width_change() {
    const CODE_BYTES: &[u8] = &[
        0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
        0x04, 0xc0, 0xfb, 0x00, 0x00, 0x00, 0xf0, 0x38, 0x0f, 0x84, 0x03, 0x00, 0xf8, 0x38, 0x2f,
        0x90, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0xf8, 0x78, 0x0f, 0xa0, 0x03, 0xc0, 0xf3, 0x10,
        0x00, 0x00, 0xf8, 0x78, 0x2f, 0xb0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x08, 0xf8, 0xb8, 0x8f,
        0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x08, 0xf8, 0xf8, 0x8f, 0xe0, 0x03, 0xc0, 0xf3, 0x10,
        0x00, 0x00, 0xfd, 0x03, 0xe0, 0x3d, 0xc0, 0xc0, 0xf3, 0xbc, 0x09, 0x00, 0xfd, 0x03, 0x00,
        0x3d, 0xc0, 0xc0, 0xf3, 0xbc, 0x08, 0x00, 0xfd, 0x43, 0x80, 0x3d, 0xc0, 0xc0, 0xf3, 0xbc,
        0x08, 0x00, 0xfc, 0x38, 0xe0, 0x3c, 0x00, 0xc0, 0xf3, 0xbc, 0x0b, 0x20, 0xfd, 0x83, 0x80,
        0x3d, 0xc0, 0xc0, 0xf3, 0xbc, 0x0a, 0x00, 0xfd, 0xc3, 0x80, 0x3d, 0xc0, 0xc0, 0xf3, 0xbc,
        0x0a, 0x00, 0xff, 0x23, 0xe0, 0x3f, 0xe2, 0xc0, 0xf3, 0xbc, 0x0b, 0x00, 0xff, 0x63, 0xc1,
        0x3f, 0xe2, 0xc0, 0xf3, 0xbc, 0x0a, 0x00, 0xff, 0xa3, 0x81, 0x3f, 0xe2, 0xc0, 0xf3, 0xbc,
        0x0a, 0x96, 0xf8, 0x30, 0xe0, 0x80, 0x03, 0xe0, 0x33, 0x00, 0x00, 0x5a, 0x00,
    ];
    const VECTORS: &[u8] = &[
        0x80, 0xff, 0x01, 0x7f, 0x10, 0x00, 0xab, 0x34, 0x55, 0xf0, 0x0f, 0xc3, 0x02, 0x7e, 0x81,
        0xfe, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0x01, 0x02, 0x80, 0x01, 0xff, 0x01, 0x20, 0x05, 0x55, 0x08, 0x03, 0x10, 0xf0, 0x3c, 0x04,
        0x02, 0x7f, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x01, 0x02, 0x02, 0x02, 0x02,
        0x03, 0x03, 0x03, 0x03, 0xff, 0x7f, 0x00, 0x80, 0xff, 0xff, 0x01, 0x00, 0x00, 0x40, 0x00,
        0xc0, 0x10, 0x00, 0x01, 0x80, 0x03, 0x00, 0xfe, 0xff, 0xff, 0x00, 0x00, 0xff, 0x34, 0x12,
        0xcc, 0xed, 0x00, 0x00, 0x00, 0x7f, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0xff, 0xff, 0x00,
        0x40, 0x00, 0x40, 0x04, 0x00, 0xff, 0x7f, 0x02, 0x00, 0x03, 0x00, 0x00, 0x01, 0x80, 0x00,
        0x01, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00, 0x01,
    ];
    const ROWS: &[(usize, &str)] = &[
        (0, "00000000000000000000000000000000600000000a0000000000000078000000b000000000000000fe000000fe0000000c0000000000000000000000fe000000"),
        (1, "00030000000300000003000080010000900000000f00000000030000b40000000801000000030000fd020000fd020000120000008001000000030000fd020000"),
        (2, "80000000fd01000003ff0000fd00000000000000fbff00000101000060000000a7000000d00100002eff00004a01000000000000fa00000083000000fb010000"),
        (3, "80010000fd020000030000007d0100003000000000000000010200009c000000ff000000d00200002d00000049020000060000007a01000083010000fa020000"),
        (5, "000001000200ffff0200000000000000000001000000000028000000000000000a00000002000000fe03000000ffffff6a2400009cdbffff0200000000000100"),
        (6, "0080000003800000030000000000000000800000000000003c000000000000000f00000003000000fd05000080fe00009f3600006ac900000300000000800000"),
        (60, "80110000ff220000013300007f4400001055000000660000ab7700003488000055990000f0aa00000fbb0000c3cc000002dd00007eee000081010000fe020000"),
        (61, "8000000001000000ff00000001000000200100000501000055010000080100000302000010020000f00200003c02000004030000020300007f03000001030000"),
        (62, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
        (63, "010000000100000002000000ffff0000004000000040000004000000ff7f00000200000003000000000100008000000001000000020000000100000000010000"),
    ];

    let mut m = machine();
    let mut v = Vpu::new(CODE);
    for (i, b) in CODE_BYTES.iter().enumerate() {
        m.store8(CODE + i as u32, *b).unwrap();
    }
    for i in 0..4096u32 {
        m.store8(0x4000 + i, (i + 1) as u8).unwrap();
        m.store8(0x5000 + i, 0).unwrap();
    }
    for (i, b) in VECTORS.iter().enumerate() {
        m.store8(0x5000 + i as u32, *b).unwrap();
    }
    v.regs.set(0, 0x8000);
    v.regs.set(1, 0x4000);
    v.regs.pc = CODE;
    for _ in 0..32 {
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

/// The lane flags, the predicates that read them, and `vgetacc`.
///
/// Four whole probe programs — `probes/setf.s`, `setf3.s`, `setf4.s` and
/// `setf5.s` — run on a Raspberry Pi 4B d03115 and replayed here. Each sets
/// the flags with one operation and then marks, row by row, the lanes a given
/// predicate lets through, so the rows below are the flags themselves as the
/// board computed them.
#[test]
fn the_measured_lane_flags() {
    #[allow(clippy::type_complexity)]
    const PROGRAMS: &[(&str, &[u8], &[(usize, &str)])] = &[
        (
            "setf.s: the eight predicates over a subtraction, an addition, a move and a load",
            &[
            0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
            0x04, 0xc0, 0xfb, 0x00, 0x00, 0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00,
            0x08, 0xf8, 0xf8, 0x8f, 0xe0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x20, 0xfd, 0x23, 0xe0, 0x3f,
            0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x38, 0xc0, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x40,
            0x00, 0xfe, 0x78, 0xc0, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x60, 0x00, 0xfe, 0xb8, 0xc0, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0x80, 0x00, 0xfe, 0xf8, 0xc0, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0xa0,
            0x00, 0xfe, 0x38, 0xc1, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfe, 0x78, 0xc1, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xe0, 0x00, 0xfe, 0xb8, 0xc1, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x20,
            0x20, 0xf5, 0xe3, 0x81, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c,
            0x00, 0x00, 0xfe, 0x38, 0xc2, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x40, 0x00, 0xfe, 0x78, 0xc2,
            0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x80, 0x00, 0xfe, 0xb8, 0xc2, 0xff, 0x07, 0xc0, 0xf3, 0x3f,
            0xc0, 0x00, 0xf5, 0xe3, 0x82, 0x3f, 0xe2, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x0a, 0xc0, 0xf3,
            0x3c, 0x00, 0x00, 0xfe, 0x38, 0xc3, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x40, 0x00, 0xfe, 0x78,
            0xc3, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x80, 0x00, 0xfe, 0xb8, 0xc3, 0xff, 0x07, 0xc0, 0xf3,
            0x3f, 0xc0, 0x08, 0xf8, 0xf8, 0x83, 0xc0, 0x0b, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0xfe, 0x38,
            0xc4, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x40, 0x00, 0xfe, 0x78, 0xc4, 0xff, 0x07, 0xc0, 0xf3,
            0x3f, 0x80, 0x00, 0xfe, 0xb8, 0xc4, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x96, 0xf8, 0x30,
            0xe0, 0x80, 0x03, 0xe0, 0x33, 0x00, 0x00, 0x5a, 0x00,
            ],
            &[
                (0, "00000000000000000000000000000000ffffffff0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"),
                (1, "ffffffffffffffffffffffffffffffff00000000ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (2, "0000000000000000ffffffff0000000000000000ffffffff000000000000000000000000ffffffffffffffffffffffff00000000ffffffffffffffff00000000"),
                (3, "ffffffffffffffff00000000ffffffffffffffff00000000ffffffffffffffffffffffff000000000000000000000000ffffffff0000000000000000ffffffff"),
                (4, "000000000000000000000000ffffffff000000000000000000000000000000000000000000000000ffffffff000000000000000000000000ffffffff00000000"),
                (5, "ffffffffffffffffffffffff00000000ffffffffffffffffffffffffffffffffffffffffffffffff00000000ffffffffffffffffffffffff00000000ffffffff"),
                (7, "fe7f0000ff7f0000fdff00000200000000000000008000000c0000000200000001000000fbff0000ffff000080fe000033120000caed0000ffff0000007e0000"),
                (8, "000000000000000000000000ffffffff00000000ffffffff00000000ffffffff0000000000000000000000000000000000000000000000000000000000000000"),
                (9, "ffffffffffffffff0000000000000000ffffffff000000000000000000000000000000000000000000000000ffffffff00000000ffffffff00000000ffffffff"),
                (10, "0000000000000000ffffffffffffffff00000000ffffffff00000000ffffffff00000000ffffffff000000000000000000000000000000000000000000000000"),
                (11, "00800000018000000100000000000000008000000000000014000000000000000500000001000000ff01000080ff000035120000ceed00000100000000800000"),
                (12, "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffff00000000"),
                (13, "00000000ffffffffffffffff0000000000000000ffffffff00000000ffffffff00000000ffffffff00000000ffffffff00000000ffffffff0000000000000000"),
                (14, "0000000000000000ffffffffffffffff00000000ffffffff00000000ffffffff00000000ffffffff000000000000000000000000000000000000000000000000"),
                (15, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
                (16, "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffff00000000"),
                (17, "00000000ffffffffffffffff0000000000000000ffffffff00000000ffffffff00000000ffffffff00000000ffffffff00000000ffffffff0000000000000000"),
                (18, "0000000000000000ffffffffffffffff00000000ffffffff00000000ffffffff00000000ffffffff000000000000000000000000000000000000000000000000"),
                (62, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
                (63, "010000000100000002000000ffff0000004000000040000004000000ff7f00000200000003000000000100008000000001000000020000000100000000010000"),
            ],
        ),
        (
            "setf3.s: which ops touch the carry, over a preset of ones and a preset of zeros",
            &[
            0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
            0x04, 0xc0, 0xfb, 0x00, 0x00, 0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00,
            0x08, 0xf8, 0xf8, 0x8f, 0xe0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0xfe, 0xf8, 0xce, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0x00, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0x80, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x38, 0xc0, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0x88, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x78, 0xc0, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0x90, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0xb8, 0xc0, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0x98, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0xf8, 0xc0, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0xd0, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x38, 0xc1, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0xa0, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x78, 0xc1, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0xa8, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0xb8, 0xc1, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0x30, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0xf8, 0xc1, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0xe0, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x38, 0xc2, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0xe8, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x78, 0xc2, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0x10, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0xb8, 0xc2, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0x20, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0xf8, 0xc2, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x0a, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x38, 0xc3, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0x80, 0xfd, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x78, 0xc3, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x38, 0x8a, 0xc0, 0xf3, 0x3c, 0x00,
            0xc8, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0xb8, 0xc3, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x38, 0x8a, 0xc0, 0xf3, 0x3c, 0x00,
            0xc0, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0xf8, 0xc3, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x38, 0x8a, 0xc0, 0xf3, 0x3c, 0x00,
            0x50, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x38, 0xc4, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x38, 0x8a, 0xc0, 0xf3, 0x3c, 0x00,
            0x38, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x78, 0xc4, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x38, 0x8a, 0xc0, 0xf3, 0x3c, 0x00,
            0x20, 0xfd, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0xb8, 0xc4, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x38, 0x8a, 0xc0, 0xf3, 0x3c, 0x00,
            0x28, 0xfd, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0xf8, 0xc4, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x38, 0x8a, 0xc0, 0xf3, 0x3c, 0x00,
            0xd8, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x38, 0xc5, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x38, 0x8a, 0xc0, 0xf3, 0x3c, 0x00,
            0x48, 0xfc, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x78, 0xc5, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x38, 0x8a, 0xc0, 0xf3, 0x3c, 0x00,
            0x48, 0xfd, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0xb8, 0xc5, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x38, 0x8a, 0xc0, 0xf3, 0x3c, 0x00,
            0x88, 0xf8, 0x23, 0xe0, 0x80, 0xeb, 0xc0, 0xf3, 0x12, 0x00, 0x00, 0xfe, 0xf8, 0xc5, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0x40, 0x00, 0xfe, 0x38, 0xc6, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x80,
            0x00, 0xfe, 0x78, 0xc6, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x96, 0xf8, 0x30, 0xe0, 0x80,
            0x03, 0xe0, 0x33, 0x00, 0x00, 0x5a, 0x00,
            ],
            &[
                (0, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (1, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (2, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (3, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (4, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (5, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (6, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (7, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (8, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (9, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (10, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (11, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (12, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (13, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (14, "00000000ffffffffffffffff0000000000000000ffffffff00000000ffffffff00000000ffffffffffffffffffffffff00000000ffffffffffffffff00000000"),
                (15, "ffffffff0000000000000000ffffffff0000000000000000ffffffff00000000ffffffff000000000000000000000000ffffffff0000000000000000ffffffff"),
                (16, "ffffffff00000000ffffffff0000000000000000ffffffff0000000000000000ffffffffffffffff00000000ffffffff00000000000000000000000000000000"),
                (17, "ffffffff00000000ffffffff0000000000000000ffffffff0000000000000000ffffffffffffffff00000000ffffffff00000000000000000000000000000000"),
                (18, "000000000000000000000000ffffffff000000000000000000000000000000000000000000000000ffffffff000000000000000000000000ffffffff00000000"),
                (19, "00000000ffffffff0000000000000000000000000000000000000000ffffffff0000000000000000000000000000000000000000000000000000000000000000"),
                (20, "00000000ffffffff000000000000000000000000ffffffff00000000ffffffff0000000000000000000000000000000000000000000000000000000000000000"),
                (21, "ffffffffffffffff00000000ffffffff000000000000000000000000ffffffff0000000000000000000000000000000000000000000000000000000000000000"),
                (22, "00000000ffffffff000000000000000000000000ffffffff00000000ffffffff0000000000000000000000000000000000000000000000000000000000000000"),
                (23, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (59, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (62, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
                (63, "010000000100000002000000ffff0000004000000040000004000000ff7f00000200000003000000000100008000000001000000020000000100000000010000"),
            ],
        ),
        (
            "setf4.s: a load and a store with SETF, and the accumulator read back",
            &[
            0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
            0x04, 0xc0, 0xfb, 0x00, 0x00, 0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00,
            0x08, 0xf8, 0xf8, 0x8f, 0xe0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0xfe, 0xf8, 0xce, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0x00, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0x08, 0xf8, 0x38, 0x80, 0xc0, 0x0b, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0xfe, 0x78, 0xc0, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0x40, 0x00, 0xfe, 0xb8, 0xc0, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x80,
            0x00, 0xfe, 0xf8, 0xc0, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x3b,
            0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x88, 0xf8, 0x23, 0xe0, 0x80, 0xeb, 0xc0, 0xf3, 0x12, 0x00,
            0x00, 0xfe, 0x38, 0xc1, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x40, 0x00, 0xfe, 0x78, 0xc1, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0x80, 0x00, 0xfe, 0xb8, 0xc1, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0xc0,
            0x00, 0xfd, 0x23, 0xe0, 0x38, 0x8a, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xf0, 0xf8, 0x01, 0xc4,
            0x03, 0x00, 0xfe, 0x38, 0xc2, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x40, 0x00, 0xfe, 0x78, 0xc2,
            0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x80, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0xbc,
            0x09, 0x00, 0xf3, 0xa3, 0x82, 0x38, 0xe2, 0x00, 0xf3, 0xe3, 0x82, 0x3f, 0xe2, 0x00, 0xfc,
            0x38, 0xe0, 0x3b, 0x02, 0xc0, 0xf3, 0xbc, 0x09, 0x00, 0xfc, 0x38, 0xe0, 0x3b, 0x02, 0xc0,
            0xf3, 0xbc, 0x08, 0x00, 0xf3, 0x23, 0x83, 0x38, 0xe2, 0x00, 0xf3, 0x63, 0x83, 0x38, 0x82,
            0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0xbc, 0x0b, 0x00, 0xf3, 0xa3, 0x83, 0x38,
            0xe2, 0x00, 0xf3, 0xe3, 0xc3, 0x38, 0xe2, 0x96, 0xf8, 0x30, 0xe0, 0x80, 0x03, 0xe0, 0x33,
            0x00, 0x00, 0x5a, 0x00,
            ],
            &[
                (0, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
                (2, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (3, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (5, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (6, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (7, "80000000ff000000010000007f0000001000000000000000ab0000003400000055000000f00000000f000000c3000000020000007e00000081000000fe000000"),
                (8, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (10, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
                (11, "ff3f000000400000ff3f0000000000000040000000c00000010000000000000000000000ff1f0000ff00000000ff00001a090000733b000000000000007f0000"),
                (12, "feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000"),
                (13, "feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000feff0000"),
                (14, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
                (15, "ff7f00000080ffffffffffff010000000040000000c0ffff100000000180ffff03000000feffffffff00000000ffffff34120000ccedffff00000000007f0000"),
                (59, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (62, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
                (63, "010000000100000002000000ffff0000004000000040000004000000ff7f00000200000003000000000100008000000001000000020000000100000000010000"),
            ],
        ),
        (
            "setf5.s: the 32-bit shifts' flags, and vgetacc's saturating forms",
            &[
            0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
            0x04, 0xc0, 0xfb, 0x00, 0x00, 0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00,
            0x08, 0xf8, 0xf8, 0x8f, 0xe0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0xfe, 0xf8, 0xce, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0x00, 0x00, 0xfd, 0x23, 0xe0, 0x38, 0x8a, 0xc0, 0xf3, 0x3c, 0x00,
            0x40, 0xfe, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00, 0x00, 0xfe, 0x38, 0xc0, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0x40, 0x00, 0xfe, 0x78, 0xc0, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x80,
            0x00, 0xfe, 0xb8, 0xc0, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0xc0, 0x00, 0xfd, 0x23, 0xe0, 0x38,
            0x8a, 0xc0, 0xf3, 0x3c, 0x00, 0x50, 0xfe, 0x23, 0xe0, 0x3f, 0xea, 0xc0, 0xf3, 0x3c, 0x00,
            0x00, 0xfe, 0xf8, 0xc0, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x40, 0x00, 0xfe, 0x38, 0xc1, 0xff,
            0x07, 0xc0, 0xf3, 0x3f, 0x80, 0x00, 0xfe, 0x78, 0xc1, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0xc0,
            0x40, 0xf6, 0xa3, 0xc1, 0x3f, 0xe2, 0x50, 0xf6, 0xe3, 0xc1, 0x3f, 0xe2, 0x00, 0xfc, 0x38,
            0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0xbc, 0x0b, 0x00, 0xf3, 0x23, 0x82, 0x38, 0xe2, 0x18, 0xf3,
            0x63, 0x82, 0x38, 0xe2, 0x08, 0xf3, 0xa3, 0x82, 0x38, 0xe2, 0x08, 0xf3, 0xe3, 0xc2, 0x38,
            0xe2, 0x00, 0xfc, 0x38, 0xe0, 0x3b, 0x02, 0xc0, 0xf3, 0xbc, 0x09, 0x00, 0xfc, 0x38, 0xe0,
            0x3b, 0x02, 0xc0, 0xf3, 0xbc, 0x08, 0x00, 0xfc, 0x38, 0xe0, 0x3b, 0x02, 0xc0, 0xf3, 0xbc,
            0x08, 0x00, 0xf3, 0x23, 0x83, 0x38, 0xe2, 0x18, 0xf3, 0x63, 0x83, 0x38, 0xe2, 0x08, 0xf3,
            0xa3, 0xc3, 0x38, 0xe2, 0x00, 0xf3, 0xe3, 0xc3, 0x38, 0xe2, 0x96, 0xf8, 0x30, 0xe0, 0x80,
            0x03, 0xe0, 0x33, 0x00, 0x00, 0x5a, 0x00,
            ],
            &[
                (0, "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ffffffff00000000"),
                (1, "00000000ffffffffffffffffffffffff00000000ffffffff00000000ffffffff00000000ffffffff00000000ffffffff00000000ffffffff0000000000000000"),
                (2, "00000000ffffffffffffffff000000000000000000000000000000000000000000000000ffffffffffffffff0000000000000000ffffffff0000000000000000"),
                (3, "000000000000000000000000ffffffff00000000000000000000000000000000ffffffff0000000000000000000000000000000000000000ffffffff00000000"),
                (4, "0000000000000000000000000000000000000000ffffffff0000000000000000000000000000000000000000ffffffff00000000000000000000000000000000"),
                (5, "ffffffff00000000ffffffff0000000000000000ffffffff00000000ffffffffffffffffffffffff00000000ffffffff00000000000000000000000000000000"),
                (6, "feff00000000fffffcffffff000000800040000000c0ffff00010000000000800c000000f0ffffffff00000000ffffff6824000030b7ffff00000000007f0000"),
                (7, "ff3f000000c0ff7fffffff3f000000000040000000c0ffff010000000100000000000000ffffff1fff00000000ffffff1a09000073fbff3f00000000007f0000"),
                (8, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
                (9, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
                (10, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
                (11, "ff7f00000080ffffffffffff010000000040000000c0ffff100000000180ffff03000000feffffffff00000000ffffff34120000ccedffff00000000007f0000"),
                (12, "fdff0000fdff0000fdff0000fdff0000fdff0000fdff0000fdff0000fdff0000fdff0000fdff0000fdff0000fdff0000fdff0000fdff0000fdff0000fdff0000"),
                (13, "ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000"),
                (14, "fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200"),
                (15, "fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200fdff0200"),
                (59, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (62, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
                (63, "010000000100000002000000ffff0000004000000040000004000000ff7f00000200000003000000000100008000000001000000020000000100000000010000"),
            ],
        ),
    ];
    const VECTORS: &[u8] = &[
        0x80, 0xff, 0x01, 0x7f, 0x10, 0x00, 0xab, 0x34, 0x55, 0xf0, 0x0f, 0xc3, 0x02, 0x7e, 0x81,
        0xfe, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0x01, 0x02, 0x80, 0x01, 0xff, 0x01, 0x20, 0x05, 0x55, 0x08, 0x03, 0x10, 0xf0, 0x3c, 0x04,
        0x02, 0x7f, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x01, 0x02, 0x02, 0x02, 0x02,
        0x03, 0x03, 0x03, 0x03, 0xff, 0x7f, 0x00, 0x80, 0xff, 0xff, 0x01, 0x00, 0x00, 0x40, 0x00,
        0xc0, 0x10, 0x00, 0x01, 0x80, 0x03, 0x00, 0xfe, 0xff, 0xff, 0x00, 0x00, 0xff, 0x34, 0x12,
        0xcc, 0xed, 0x00, 0x00, 0x00, 0x7f, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0xff, 0xff, 0x00,
        0x40, 0x00, 0x40, 0x04, 0x00, 0xff, 0x7f, 0x02, 0x00, 0x03, 0x00, 0x00, 0x01, 0x80, 0x00,
        0x01, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00, 0x01,
    ];

    for (name, code, rows) in PROGRAMS {
        let mut m = machine();
        let mut v = Vpu::new(CODE);
        for (i, b) in code.iter().enumerate() {
            m.store8(CODE + i as u32, *b).unwrap();
        }
        for i in 0..4096u32 {
            m.store8(0x4000 + i, (i + 1) as u8).unwrap();
            m.store8(0x5000 + i, 0).unwrap();
        }
        for (i, b) in VECTORS.iter().enumerate() {
            m.store8(0x5000 + i as u32, *b).unwrap();
        }
        v.regs.set(0, 0x8000);
        v.regs.set(1, 0x4000);
        v.regs.pc = CODE;
        for _ in 0..200 {
            if v.regs.pc == CODE + code.len() as u32 - 2 {
                break; // the trailing `rts`
            }
            assert_eq!(
                v.step(&mut m),
                Step::Ran,
                "{name} at {:#x}: {:?}",
                v.regs.pc,
                v.stopped
            );
        }
        for (row, want) in *rows {
            let got: String = (0..64)
                .map(|c| format!("{:02x}", m.load8(0x8000 + (*row as u32) * 64 + c).unwrap()))
                .collect();
            assert_eq!(&got, want, "{name}, row {row}");
        }
    }
}

/// A vector immediate is signed, in both encodings.
///
/// `v32mov HY(0,0),#0x20` leaves `0xffffffe0` in every lane on a Raspberry Pi
/// 4B d03115 — the 48-bit encoding's six-bit field is a signed one — and the
/// 80-bit form's sixteen-bit `#0xffff` leaves `0xffffffff`. Read as unsigned
/// they would be 32 and 65535. From `probes/imm.s`.
#[test]
fn a_vector_immediate_is_signed() {
    const A16: [u32; 16] = [
        0x7fff, 0x8000, 0xffff, 0x0001, 0x4000, 0xc000, 0x0010, 0x8001, 0x0003, 0xfffe, 0x00ff,
        0xff00, 0x1234, 0xedcc, 0x0000, 0x7f00,
    ];
    #[allow(clippy::type_complexity)]
    const CASES: &[(&str, usize, u32, &[u8], &[u32])] = &[
        (
            "v32mov HY(0,0),65535",
            0,
            4,
            &[0x00, 0xfe, 0x38, 0xc0, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x00],
            &[
                0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff,
                0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff,
                0xffffffff, 0xffffffff,
            ],
        ),
        (
            "v32mov HY(1,0),0x3f",
            1,
            4,
            &[0x00, 0xf6, 0x78, 0xc0, 0x3f, 0x04],
            &[
                0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff,
                0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff,
                0xffffffff, 0xffffffff,
            ],
        ),
        (
            "v32mov HY(2,0),0x20",
            2,
            4,
            &[0x00, 0xf6, 0xb8, 0xc0, 0x20, 0x04],
            &[
                0xffffffe0, 0xffffffe0, 0xffffffe0, 0xffffffe0, 0xffffffe0, 0xffffffe0, 0xffffffe0,
                0xffffffe0, 0xffffffe0, 0xffffffe0, 0xffffffe0, 0xffffffe0, 0xffffffe0, 0xffffffe0,
                0xffffffe0, 0xffffffe0,
            ],
        ),
        (
            "v16mov HX(3,0),65535",
            3,
            2,
            &[0x00, 0xfc, 0xf8, 0x80, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x00],
            &[
                0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
                0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
            ],
        ),
        (
            "v16mov HX(4,0),0x20",
            4,
            2,
            &[0x00, 0xf4, 0x38, 0x81, 0x20, 0x04],
            &[
                0xffe0, 0xffe0, 0xffe0, 0xffe0, 0xffe0, 0xffe0, 0xffe0, 0xffe0, 0xffe0, 0xffe0,
                0xffe0, 0xffe0, 0xffe0, 0xffe0, 0xffe0, 0xffe0,
            ],
        ),
        (
            "v32add HY(5,0),HX(62,0),0x20",
            5,
            4,
            &[0x00, 0xf7, 0x63, 0xc1, 0x20, 0xe4],
            &[
                0x00007fdf, 0xffff7fe0, 0xffffffdf, 0xffffffe1, 0x00003fe0, 0xffffbfe0, 0xfffffff0,
                0xffff7fe1, 0xffffffe3, 0xffffffde, 0x000000df, 0xfffffee0, 0x00001214, 0xffffedac,
                0xffffffe0, 0x00007ee0,
            ],
        ),
        (
            "v32add HY(6,0),HX(62,0),65535",
            6,
            4,
            &[0x00, 0xff, 0xa3, 0xc1, 0xff, 0xe7, 0xc0, 0xf3, 0x3f, 0x00],
            &[
                0x00007ffe, 0xffff7fff, 0xfffffffe, 0x00000000, 0x00003fff, 0xffffbfff, 0x0000000f,
                0xffff8000, 0x00000002, 0xfffffffd, 0x000000fe, 0xfffffeff, 0x00001233, 0xffffedcb,
                0xffffffff, 0x00007eff,
            ],
        ),
        (
            "v16add HX(7,0),HX(62,0),0x20",
            7,
            2,
            &[0x00, 0xf5, 0xe3, 0x81, 0x20, 0xe4],
            &[
                0x7fdf, 0x7fe0, 0xffdf, 0xffe1, 0x3fe0, 0xbfe0, 0xfff0, 0x7fe1, 0xffe3, 0xffde,
                0x00df, 0xfee0, 0x1214, 0xedac, 0xffe0, 0x7ee0,
            ],
        ),
    ];

    for (name, row, w, bytes, want) in CASES {
        let mut m = machine();
        let mut v = Vpu::new(CODE);
        for (i, b) in bytes.iter().enumerate() {
            m.store8(CODE + i as u32, *b).unwrap();
        }
        m.store16(CODE + bytes.len() as u32, NOP).unwrap();
        for e in 0..16u32 {
            v.vrf.write(62, e, 2, A16[e as usize]);
        }
        v.regs.pc = CODE;
        assert_eq!(v.step(&mut m), Step::Ran, "{name}: {:?}", v.stopped);
        let got: Vec<u32> = (0..16).map(|e| v.vrf.read(*row as u8, e, *w)).collect();
        assert_eq!(&got[..], *want, "{name}");
    }
}

/// The ALU sub-ops that were doubtful, as whole probe programs.
///
/// Each ran on a Raspberry Pi 4B d03115 and is replayed here against the same
/// memory. Between them they pin the carry-in forms (`addc`, `subc` and the
/// rest), the signed shifts, the sub-ops that write a lane of zeros at one
/// width and compute at the other, a dash A operand, a `*` slot, the
/// multiplies whose registers differ in width, and the scalar result unit.
#[test]
fn the_measured_alu_sub_ops() {
    #[allow(clippy::type_complexity)]
    const PROGRAMS: &[(&str, &[u8], &[u8], &[(usize, &str)])] = &[
        (
            "alu3.s: the carry-in forms, the sign shifts and the sub-ops that write zeros",
            &[
            0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
            0x04, 0xc0, 0xfb, 0x00, 0x00, 0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00,
            0x08, 0xf8, 0xf8, 0x8f, 0xe0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0xfd, 0x23, 0xe0, 0x3b,
            0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x60, 0xf4, 0x23, 0x80, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0,
            0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x68, 0xf4, 0x63, 0x80, 0x3f, 0xe2, 0x00, 0xfd, 0x23,
            0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x70, 0xf4, 0xa3, 0x80, 0x3f, 0xe2, 0x00, 0xfd,
            0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x78, 0xf4, 0xe3, 0x80, 0x3f, 0xe2, 0x00,
            0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0xb0, 0xf4, 0x23, 0x81, 0x3f, 0xe2,
            0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0xb8, 0xf4, 0x63, 0x81, 0x3f,
            0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0xf0, 0xf4, 0xa3, 0x81,
            0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0xf8, 0xf4, 0xe3,
            0x81, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x10, 0xf5,
            0x23, 0x82, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x18,
            0xf5, 0x63, 0x82, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00,
            0x30, 0xf5, 0xa3, 0x82, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c,
            0x00, 0x38, 0xf5, 0xe3, 0x82, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3,
            0x3c, 0x00, 0x50, 0xf5, 0x23, 0x83, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0,
            0xf3, 0x3c, 0x00, 0x58, 0xf5, 0x63, 0x83, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba,
            0xc0, 0xf3, 0x3c, 0x00, 0x60, 0xf5, 0xa3, 0x83, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3b,
            0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x68, 0xf5, 0xe3, 0x83, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0,
            0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x70, 0xf5, 0x23, 0x84, 0x3f, 0xe2, 0x00, 0xfd, 0x23,
            0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x78, 0xf5, 0x63, 0x84, 0x3f, 0xe2, 0x00, 0xfe,
            0xf8, 0xce, 0xff, 0x07, 0xc0, 0xf3, 0x3f, 0x00, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba, 0xc0,
            0xf3, 0x3c, 0x00, 0x10, 0xf5, 0xa3, 0x84, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3b, 0xba,
            0xc0, 0xf3, 0x3c, 0x00, 0x18, 0xf5, 0xe3, 0x84, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0, 0x3b,
            0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x30, 0xf5, 0x23, 0x85, 0x3f, 0xe2, 0x00, 0xfd, 0x23, 0xe0,
            0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x38, 0xf5, 0x63, 0x85, 0x3f, 0xe2, 0x00, 0xfd, 0x23,
            0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x50, 0xf5, 0xa3, 0x85, 0x3f, 0xe2, 0x00, 0xfd,
            0x23, 0xe0, 0x3b, 0xba, 0xc0, 0xf3, 0x3c, 0x00, 0x58, 0xf5, 0xe3, 0x85, 0x3f, 0xe2, 0x96,
            0xf8, 0x30, 0xe0, 0x80, 0x03, 0xe0, 0x33, 0x00, 0x00, 0x5a, 0x00,
            ],
            &[
            0x80, 0xff, 0x01, 0x7f, 0x10, 0x00, 0xab, 0x34, 0x55, 0xf0, 0x0f, 0xc3, 0x02, 0x7e, 0x81,
            0xfe, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
            0x01, 0x02, 0x80, 0x01, 0xff, 0x01, 0x20, 0x05, 0x55, 0x08, 0x03, 0x10, 0xf0, 0x3c, 0x04,
            0x02, 0x7f, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x01, 0x02, 0x02, 0x02, 0x02,
            0x03, 0x03, 0x03, 0x03, 0xff, 0x7f, 0x00, 0x80, 0xff, 0xff, 0x01, 0x00, 0x00, 0x40, 0x00,
            0xc0, 0x10, 0x00, 0x01, 0x80, 0x03, 0x00, 0xfe, 0xff, 0xff, 0x00, 0x00, 0xff, 0x34, 0x12,
            0xcc, 0xed, 0x00, 0x00, 0x00, 0x7f, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0xff, 0xff, 0x00,
            0x40, 0x00, 0x40, 0x04, 0x00, 0xff, 0x7f, 0x02, 0x00, 0x03, 0x00, 0x00, 0x01, 0x80, 0x00,
            0x01, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00, 0x01,
            ],
            &[
                (0, "feff000000000000fcff000000000000000000000000000000010000000000000c000000f0ff000000000000000000006824000030b700000000000000000000"),
                (2, "feff000000000000fcff000000000000000000000000000000010000000000000c000000f0ff000000000000000000006824000030b700000000000000000000"),
                (3, "ff7f000000800000fcff000000000000ff7f00000080000000010000008000000c000000f0ff0000ff7f0000008000006824000030b7000000000000ff7f0000"),
                (7, "01000000010000000000000001000000010000000100000001000000010000000100000000000000000000000100000001000000010000000000000001000000"),
                (8, "00800000018000000100000000000000008000000000000014000000000000000500000001000000ff01000080ff000035120000ceed00000100000000800000"),
                (9, "ff7f0000018000000100000000000000ff7f00000000000014000000000000000500000001000000ff01000080ff000035120000ceed000001000000ff7f0000"),
                (10, "fe7f0000ff7f0000fdff00000200000000000000008000000c0000000200000001000000fbff0000ffff000080fe000033120000caed0000ffff0000007e0000"),
                (11, "fe7f000000800000fdff00000200000000000000008000000c0000000080000001000000fbff0000ffff000080fe000033120000caed0000ffff0000007e0000"),
                (12, "028000000180000003000000feff00000000000000800000f4ff0000feff0000ffff0000050000000100000080010000cded0000361200000100000000820000"),
                (13, "02800000ff7f000003000000feff000000000000ff7f0000f4ff0000ff7f0000ffff0000050000000100000080010000cded0000361200000100000000820000"),
                (18, "018000000280000002000000010000000180000001000000150000000100000006000000020000000002000081ff000036120000cfed00000200000001800000"),
                (19, "ff7f0000028000000200000001000000ff7f000001000000150000000100000006000000020000000002000081ff000036120000cfed000002000000ff7f0000"),
                (20, "fd7f0000fe7f0000fcff000001000000ffff0000ff7f00000b0000000100000000000000faff0000feff00007ffe000032120000c9ed0000feff0000ff7d0000"),
                (21, "fd7f000000800000fcff000001000000ffff0000008000000b0000000080000000000000faff0000feff00007ffe000032120000c9ed0000feff0000ff7d0000"),
                (22, "018000000080000002000000fdff0000ffff0000ff7f0000f3ff0000fdff0000feff000004000000000000007f010000cced00003512000000000000ff810000"),
                (23, "01800000ff7f000002000000fdff0000ffff0000ff7f0000f3ff0000ff7f0000feff000004000000000000007f010000cced00003512000000000000ff810000"),
                (59, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (62, "ff7f000000800000ffff0000010000000040000000c00000100000000180000003000000feff0000ff00000000ff000034120000cced000000000000007f0000"),
                (63, "010000000100000002000000ffff0000004000000040000004000000ff7f00000200000003000000000100008000000001000000020000000100000000010000"),
            ],
        ),
        (
            "alu4.s: the sign shifts over negative counts, against a destination preset to ones",
            &[
            0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
            0x04, 0xc0, 0xfb, 0x00, 0x00, 0x05, 0xfe, 0x38, 0xc0, 0xff, 0x07, 0xc0, 0xfb, 0x3f, 0x00,
            0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x08, 0xf8, 0xf8, 0x8f, 0xe0,
            0x03, 0xc0, 0xf3, 0x10, 0x00, 0x60, 0xf4, 0x23, 0x80, 0x3f, 0xe2, 0x70, 0xf4, 0x63, 0x80,
            0x3f, 0xe2, 0x78, 0xf4, 0xa3, 0x80, 0x3f, 0xe2, 0x68, 0xf4, 0xe3, 0x80, 0x3f, 0xe2, 0xb0,
            0xf4, 0x23, 0x81, 0x3f, 0xe2, 0xb8, 0xf4, 0x63, 0x81, 0x3f, 0xe2, 0x60, 0xf5, 0xa3, 0x81,
            0x3f, 0xe2, 0x68, 0xf5, 0xe3, 0x81, 0x3f, 0xe2, 0x70, 0xf5, 0x23, 0x82, 0x3f, 0xe2, 0x78,
            0xf5, 0x63, 0x82, 0x3f, 0xe2, 0xf0, 0xf4, 0xa3, 0x82, 0x3f, 0xe2, 0xf0, 0xf6, 0xe3, 0xc2,
            0x3f, 0xe2, 0xe0, 0xf4, 0x23, 0x83, 0x3f, 0xe2, 0xf8, 0xf4, 0x63, 0x83, 0x3f, 0xe2, 0x40,
            0xf4, 0xa3, 0x83, 0x3f, 0xe2, 0x58, 0xf4, 0xe3, 0x83, 0x3f, 0xe2, 0x96, 0xf8, 0x30, 0xe0,
            0x80, 0x03, 0xe0, 0x33, 0x00, 0x00, 0x5a, 0x00,
            ],
            &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x80, 0x00, 0xf0, 0xf0, 0x0f, 0x01, 0x80, 0x00,
            0xc0, 0xff, 0x00, 0xff, 0x7f, 0x34, 0x12, 0xcc, 0xed, 0x01, 0x00, 0xfe, 0xff, 0x00, 0x40,
            0x10, 0x00, 0x00, 0x00, 0xf0, 0x00, 0xff, 0xff, 0xfe, 0xff, 0xfc, 0xff, 0xf1, 0xff, 0xf0,
            0xff, 0xe0, 0xff, 0x01, 0x00, 0x02, 0x00, 0xff, 0xff, 0xfd, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xfc, 0xff, 0xff, 0xff, 0xf8, 0xff,
            ],
            &[
                (0, "ff7fffff0020ffff000fffff0000ffff0000ffff0000fffffe01fffffcffffff1a09ffffb91dffff0000ffffff7fffff0020ffff0100ffff0000ffff0000ffff"),
                (1, "ffffffff00e0ffff00ffffff0000fffffffffffffffffffffe01fffffcffffff1a09ffffb9fdffff0000ffffffffffff0020ffff0100ffff0000ffff0000ffff"),
                (2, "ffffffff00e0ffff00ffffff0000fffffffffffffffffffffe01ffffff7fffff1a09ffffb9fdffff0000ffffffffffff0020ffff0100ffff0000ffff0000ffff"),
                (3, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (4, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (5, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (6, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (7, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (8, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (9, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (10, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (11, "010000000200000004000000f1ffffff10000000200000000100000002000000ffffffff03000000ffffffff01000000fffffffffcfffffffffffffff8ffffff"),
                (12, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0100ffff0200ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (13, "0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff"),
                (14, "0080ffff0000ffff0000ffffe01fffff0180ffff00c0fffffe01fffffcffffff0000ffff0080ffff0080ffff0000ffff0000ffff0000ffff0000ffff00f0ffff"),
                (15, "fffffffffefffffffffffffff807ffff0180ffff00c0ffff7f00ffffff1fffff0000ffffffffffff0000ffffffffffff0000ffff0000ffff0000ffff0000ffff"),
                (16, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (17, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (18, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (19, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (20, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (21, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (22, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (23, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (24, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (25, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (26, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (27, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (28, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (29, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (30, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (31, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (62, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
                (63, "ffff0000feff0000fcff0000f1ff0000f0ff0000e0ff00000100000002000000ffff0000fdff0000ffff0000ffff0000ffff0000fcff0000ffff0000f8ff0000"),
            ],
        ),
        (
            "alu5.s: every doubtful sub-op at both widths",
            &[
            0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
            0x04, 0xc0, 0xfb, 0x00, 0x00, 0x05, 0xfe, 0x38, 0xc0, 0xff, 0x07, 0xc0, 0xfb, 0x3f, 0x00,
            0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x08, 0xf8, 0xf8, 0x8f, 0xe0,
            0x03, 0xc0, 0xf3, 0x10, 0x00, 0x68, 0xf4, 0x23, 0x80, 0x3f, 0xe2, 0x68, 0xf6, 0x63, 0xc0,
            0x3f, 0xe2, 0xb0, 0xf4, 0xa3, 0x80, 0x3f, 0xe2, 0xb0, 0xf6, 0xe3, 0xc0, 0x3f, 0xe2, 0xb8,
            0xf4, 0x23, 0x81, 0x3f, 0xe2, 0xb8, 0xf6, 0x63, 0xc1, 0x3f, 0xe2, 0x60, 0xf5, 0xa3, 0x81,
            0x3f, 0xe2, 0x60, 0xf7, 0xe3, 0xc1, 0x3f, 0xe2, 0x68, 0xf5, 0x23, 0x82, 0x3f, 0xe2, 0x68,
            0xf7, 0x63, 0xc2, 0x3f, 0xe2, 0x70, 0xf5, 0xa3, 0x82, 0x3f, 0xe2, 0x70, 0xf7, 0xe3, 0xc2,
            0x3f, 0xe2, 0x78, 0xf5, 0x23, 0x83, 0x3f, 0xe2, 0x78, 0xf7, 0x63, 0xc3, 0x3f, 0xe2, 0xf0,
            0xf4, 0xa3, 0x83, 0x3f, 0xe2, 0xf0, 0xf6, 0xe3, 0xc3, 0x3f, 0xe2, 0xf8, 0xf4, 0x23, 0x84,
            0x3f, 0xe2, 0xf8, 0xf6, 0x63, 0xc4, 0x3f, 0xe2, 0xa0, 0xf4, 0xa3, 0x84, 0x3f, 0xe2, 0xa0,
            0xf6, 0xe3, 0xc4, 0x3f, 0xe2, 0x60, 0xf4, 0x23, 0x85, 0x3f, 0xe2, 0x60, 0xf6, 0x63, 0xc5,
            0x3f, 0xe2, 0xa8, 0xf4, 0xa3, 0x85, 0x3f, 0xe2, 0xa8, 0xf6, 0xe3, 0xc5, 0x3f, 0xe2, 0x30,
            0xf4, 0x23, 0x86, 0x3f, 0xe2, 0x30, 0xf6, 0x63, 0xc6, 0x3f, 0xe2, 0xd0, 0xf4, 0xa3, 0x86,
            0x3f, 0xe2, 0xd0, 0xf6, 0xe3, 0xc6, 0x3f, 0xe2, 0x96, 0xf8, 0x30, 0xe0, 0x80, 0x03, 0xe0,
            0x33, 0x00, 0x00, 0x5a, 0x00,
            ],
            &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x80, 0x00, 0xf0, 0xf0, 0x0f, 0x01, 0x80, 0x00,
            0xc0, 0xff, 0x00, 0xff, 0x7f, 0x34, 0x12, 0xcc, 0xed, 0x01, 0x00, 0xfe, 0xff, 0x00, 0x40,
            0x10, 0x00, 0x00, 0x00, 0xf0, 0x00, 0xff, 0xff, 0xfe, 0xff, 0xfc, 0xff, 0xf1, 0xff, 0xf0,
            0xff, 0xe0, 0xff, 0x01, 0x00, 0x02, 0x00, 0xff, 0xff, 0xfd, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xfc, 0xff, 0xff, 0xff, 0xf8, 0xff,
            ],
            &[
                (0, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (2, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (4, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (6, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (8, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (10, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (12, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (14, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (15, "010000000200000004000000f1ffffff10000000200000000100000002000000ffffffff03000000ffffffff01000000fffffffffcfffffffffffffff8ffffff"),
                (16, "0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff0100ffff"),
                (18, "2000ffff1000ffff1200ffff1500ffff0e00ffff0d00ffff0900ffff1000ffff1500ffff1900ffff1100ffff1f00ffff1100ffff0f00ffff1000ffff1100ffff"),
                (20, "ff7fffff0020ffff000fffff0000ffff0000ffff0000fffffe01fffffcffffff1a09ffffb91dffff0000ffffff7fffff0020ffff0100ffff0000ffff0000ffff"),
                (21, "ffffff7f00e0ff3f00ffff0f00000000ffff000000000000fe010000fcff01001a090000b9fdff1f00000000ffffff7f00200000010000000000000000000000"),
                (22, "0f00ffff0f00ffff0f00ffff0f00ffff0f00ffff0f00ffff0700ffff0e00ffff0f00ffff0f00ffff0f00ffff0f00ffff0f00ffff0f00ffff0f00ffff0f00ffff"),
                (23, "1f0000001f0000001f0000001f0000001f0000001f000000070000000e0000001f0000001f0000001f0000001f0000001f0000001f0000001f0000001f000000"),
                (24, "ff7fffff0000ffff0000ffff0000ffff0180ffff0300ffff0100ffff0300ffff2416ffff7606ffff0040ffffff3fffff0100ffff8000ffff0000ffff0f00ffff"),
                (25, "ffffff7fff7f0000ffff0000e01f000001800000ffff0300010000000300000000002416ffff760600000040ffffff3f00000100000080000000000000000f00"),
                (26, "0000fffffe7ffffffc0fffffff0fffffef7fffffe03ffffffe00fffffd7fffff3512ffff3112ffff0200ffff0100ffff0140ffff1400ffff0100fffff800ffff"),
                (27, "00000000fe7f0000fc0f0000ff0f0000ef7f0000e03f0000fe000000fd7f000035120000311200000200000001000000014000001400000001000000f8000000"),
                (28, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (29, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (30, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (31, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (62, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
                (63, "ffff0000feff0000fcff0000f1ff0000f0ff0000e0ff00000100000002000000ffff0000fdff0000ffff0000ffff0000ffff0000fcff0000ffff0000f8ff0000"),
            ],
        ),
        (
            "alu6.s: a dash A operand, a `*` slot, and multiplies whose registers differ in width",
            &[
            0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
            0x04, 0xc0, 0xfb, 0x00, 0x00, 0x05, 0xfe, 0x38, 0xc0, 0xff, 0x07, 0xc0, 0xfb, 0x3f, 0x00,
            0x00, 0xf8, 0xb8, 0x0e, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0xf8, 0x78, 0x0e, 0xe0,
            0x03, 0xc0, 0xf3, 0x10, 0x00, 0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00,
            0x08, 0xf8, 0xf8, 0x8f, 0xe0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0xf5, 0x38, 0x80, 0x3f,
            0x02, 0x20, 0xf5, 0x78, 0x80, 0x3f, 0x02, 0x80, 0xf4, 0xb8, 0x80, 0x3f, 0x02, 0xc0, 0xf4,
            0xf8, 0x80, 0x3f, 0x02, 0x00, 0xfd, 0x23, 0x81, 0x3f, 0xe2, 0xd0, 0xf3, 0x3c, 0x00, 0x00,
            0xfd, 0x63, 0x81, 0x3f, 0xe2, 0xc0, 0xf7, 0x3c, 0x00, 0x00, 0xfd, 0xa3, 0x81, 0x3f, 0xe2,
            0xc0, 0xf3, 0x3d, 0x00, 0x00, 0xfc, 0xf8, 0x81, 0x3e, 0x02, 0xc0, 0xf3, 0x3d, 0x00, 0x08,
            0xf8, 0x38, 0x82, 0xc0, 0x03, 0xc0, 0xf7, 0x10, 0x00, 0x08, 0xf8, 0x78, 0x82, 0xc0, 0x03,
            0xc0, 0xf3, 0x10, 0x00, 0x80, 0xf5, 0xa3, 0x82, 0x39, 0xe0, 0x80, 0xf5, 0xe3, 0x02, 0x3f,
            0xe2, 0x80, 0xf5, 0x03, 0x83, 0x3f, 0xa2, 0xa0, 0xf5, 0x63, 0x83, 0x39, 0xe0, 0x96, 0xf8,
            0x30, 0xe0, 0x80, 0x03, 0xe0, 0x33, 0x00, 0x00, 0x5a, 0x00,
            ],
            &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x80, 0x00, 0xf0, 0xf0, 0x0f, 0x01, 0x80, 0x00,
            0xc0, 0xff, 0x00, 0xff, 0x7f, 0x34, 0x12, 0xcc, 0xed, 0x01, 0x00, 0xfe, 0xff, 0x00, 0x40,
            0x10, 0x00, 0x00, 0x00, 0xf0, 0x00, 0xff, 0xff, 0xfe, 0xff, 0xfc, 0xff, 0xf1, 0xff, 0xf0,
            0xff, 0xe0, 0xff, 0x01, 0x00, 0x02, 0x00, 0xff, 0xff, 0xfd, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xfc, 0xff, 0xff, 0xff, 0xf8, 0xff,
            ],
            &[
                (0, "fffffffffefffffffcfffffff1fffffff0ffffffe0ffffff0100ffff0200fffffffffffffdfffffffffffffffffffffffffffffffcfffffffffffffff8ffffff"),
                (1, "0100ffff0200ffff0400ffff0f00ffff1000ffff2000fffffffffffffeffffff0100ffff0300ffff0100ffff0100ffff0100ffff0400ffff0100ffff0800ffff"),
                (2, "0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff0000ffff"),
                (3, "fffffffffefffffffcfffffff1fffffff0ffffffe0ffffff0000ffff0000fffffffffffffdfffffffffffffffffffffffffffffffcfffffffffffffff8ffffff"),
                (4, "fefffffffe7ffffffcefffffe10ffffff17fffffe0bfffff0001ffff0180ffff3312ffffc9edffff0000fffffdffffffff3fffff0c00ffffffffffffe800ffff"),
                (5, "fefffffffe7ffffffcefffffe10ffffff17fffffe0bfffff0001ffff0180ffff3312ffffc9edffff0000fffffdffffffff3fffff0c00ffffffffffffe800ffff"),
                (6, "fefffffffe7ffffffcefffffe10ffffff17fffffe0bfffff0001ffff0180ffff3312ffffc9edffff0000fffffdffffffff3fffff0c00ffffffffffffe800ffff"),
                (7, "ffffffff0080ffff00f0fffff00fffff0180ffff00c0ffffff00ffffff7fffff3412ffffccedffff0100fffffeffffff0040ffff1000ffff0000fffff000ffff"),
                (8, "ffffffff0080ffff00f0fffff00fffff0180ffff00c0ffffff00ffffff7fffff3412ffffccedffff0100fffffeffffff0040ffff1000ffff0000fffff000ffff"),
                (9, "ffffffff0080ffff00f0fffff00fffff0180ffff00c0ffffff00ffffff7fffff3412ffffccedffff0100fffffeffffff0040ffff1000ffff0000fffff000ffff"),
                (10, "01ffffff0080ffff0020ffff10e0fffffc00ffff0040ffff0ff0ffff017fffffc010ffff34deffffe000ffff02feffff0040ffff0000ffff0000ffff0000ffff"),
                (11, "01ffffff00ffffff00fffffff0fffffff0ffffff00fffffffffffffffeffffffccffffff9cffffffffffffff02ffffff00ffffffc0ffffff00ffffff80ffffff"),
                (12, "01ffffff02feffff0000ffff80f8ffff0000ffff00e2fffff000ffff1e00ffffffffffff80feffff0000ffff40ffffff01ffffff0000ffff01ffffff08fcffff"),
                (13, "ffffffff80fffffff0ffffff0f00ffff82ffffffc0ffffff0000ffff7f00ffff1100ffffedffffff0000ffffffffffff0000ffff0000ffff0000ffff0000ffff"),
                (14, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (15, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (16, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (17, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (18, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (19, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (20, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (21, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (22, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (23, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (24, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (25, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (26, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (27, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (28, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (29, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (30, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (31, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
                (57, "ff000000ff000000fe000000ff000000fc000000ff000000f1000000ff000000f0000000ff000000e0000000ff00000001000000000000000200000000000000"),
                (58, "ff000000ff000000000000008000000000000000f0000000f00000000f000000010000008000000000000000c0000000ff00000000000000ff0000007f000000"),
                (62, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
                (63, "ffff0000feff0000fcff0000f1ff0000f0ff0000e0ff00000100000002000000ffff0000fdff0000ffff0000ffff0000ffff0000fcff0000ffff0000f8ff0000"),
            ],
        ),
        (
            "star.s: a `*` on each slot in turn, under REP and with an addend",
            &[
            0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x25, 0x60, 0x06, 0xfe, 0x38,
            0xc0, 0x00, 0x04, 0xc0, 0xfb, 0x00, 0x00, 0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3,
            0x10, 0x00, 0x08, 0xf8, 0xf8, 0x8f, 0xe0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x02, 0xfd, 0x23,
            0x80, 0x3f, 0xe2, 0xc0, 0xfb, 0x3c, 0x00, 0x02, 0xfd, 0x23, 0x81, 0x3f, 0xe2, 0xc0, 0xff,
            0x3c, 0x00, 0x02, 0xfd, 0x23, 0x82, 0x3f, 0xe2, 0xd0, 0xfb, 0x3c, 0x00, 0x05, 0xf4, 0x38,
            0x83, 0x7e, 0x02, 0x00, 0xfc, 0x78, 0x83, 0x3e, 0x02, 0xc0, 0xf3, 0x15, 0x00, 0x09, 0xf8,
            0xb8, 0x83, 0xc0, 0x03, 0xc0, 0xfb, 0x10, 0x00, 0x09, 0xf8, 0x38, 0x84, 0xc0, 0x03, 0xc0,
            0xff, 0x10, 0x00, 0x88, 0xf8, 0x23, 0xe0, 0x80, 0xe3, 0xd0, 0xf3, 0x12, 0x00, 0x08, 0xf8,
            0xb8, 0x84, 0x80, 0x03, 0xc0, 0xf3, 0x12, 0x00, 0x96, 0xf8, 0x30, 0xe0, 0x80, 0x03, 0xe0,
            0x33, 0x00, 0x00, 0x5a, 0x00,
            ],
            &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x80, 0x00, 0xf0, 0xf0, 0x0f, 0x01, 0x80, 0x00,
            0xc0, 0xff, 0x00, 0xff, 0x7f, 0x34, 0x12, 0xcc, 0xed, 0x01, 0x00, 0xfe, 0xff, 0x00, 0x40,
            0x10, 0x00, 0x00, 0x00, 0xf0, 0x00, 0xff, 0xff, 0xfe, 0xff, 0xfc, 0xff, 0xf1, 0xff, 0xf0,
            0xff, 0xe0, 0xff, 0x01, 0x00, 0x02, 0x00, 0xff, 0xff, 0xfd, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xfc, 0xff, 0xff, 0xff, 0xf8, 0xff,
            ],
            &[
                (0, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (1, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (2, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (3, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (4, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (5, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (6, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (7, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (8, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (9, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (10, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (11, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (12, "00f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f00000000000000000000000"),
                (13, "00f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f00000000000000000000000"),
                (14, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
                (15, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
                (16, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
                (17, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
                (18, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
                (62, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
                (63, "ffff0000feff0000fcff0000f1ff0000f0ff0000e0ff00000100000002000000ffff0000fdff0000ffff0000ffff0000ffff0000fcff0000ffff0000f8ff0000"),
            ],
        ),
        (
            "sru.s: the scalar result unit's eight functions",
            &[
            0x03, 0xb0, 0x40, 0x00, 0x06, 0x40, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38,
            0xc0, 0x00, 0x04, 0xc0, 0xfb, 0x00, 0x00, 0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3,
            0x10, 0x00, 0x08, 0xf8, 0xf8, 0x8f, 0xe0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0x60, 0x00,
            0xfd, 0x23, 0xe0, 0x3f, 0xe2, 0xc0, 0xf3, 0x3c, 0x10, 0x00, 0xf6, 0x38, 0xc0, 0x80, 0x03,
            0x00, 0x60, 0x00, 0xfd, 0x23, 0xe0, 0x3f, 0xe2, 0xc0, 0xf3, 0x3c, 0x12, 0x00, 0xf6, 0x78,
            0xc0, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfd, 0x23, 0xe0, 0x3f, 0xe2, 0xc0, 0xf3, 0x3c, 0x14,
            0x00, 0xf6, 0xb8, 0xc0, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfd, 0x23, 0xe0, 0x3f, 0xe2, 0xc0,
            0xf3, 0x3c, 0x16, 0x00, 0xf6, 0xf8, 0xc0, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfd, 0x23, 0xe0,
            0x3f, 0xe2, 0xc0, 0xf3, 0x3c, 0x18, 0x00, 0xf6, 0x38, 0xc1, 0x80, 0x03, 0x00, 0x60, 0x00,
            0xfd, 0x23, 0xe0, 0x3f, 0xe2, 0xc0, 0xf3, 0x3c, 0x1a, 0x00, 0xf6, 0x78, 0xc1, 0x80, 0x03,
            0x00, 0x60, 0x00, 0xfd, 0x23, 0xe0, 0x3f, 0xe2, 0xc0, 0xf3, 0x3c, 0x1c, 0x00, 0xf6, 0xb8,
            0xc1, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfd, 0x23, 0xe0, 0x3f, 0xe2, 0xc0, 0xf3, 0x3c, 0x1e,
            0x00, 0xf6, 0xf8, 0xc1, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0,
            0xf3, 0x3c, 0x10, 0x00, 0xf6, 0x38, 0xc2, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfc, 0x38, 0xe0,
            0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x12, 0x00, 0xf6, 0x78, 0xc2, 0x80, 0x03, 0x00, 0x60, 0x00,
            0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x14, 0x00, 0xf6, 0xb8, 0xc2, 0x80, 0x03,
            0x00, 0x60, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x16, 0x00, 0xf6, 0xf8,
            0xc2, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x18,
            0x00, 0xf6, 0x38, 0xc3, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0,
            0xf3, 0x3c, 0x1a, 0x00, 0xf6, 0x78, 0xc3, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfc, 0x38, 0xe0,
            0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x1c, 0x00, 0xf6, 0xb8, 0xc3, 0x80, 0x03, 0x00, 0x60, 0x00,
            0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x1e, 0x00, 0xf6, 0xf8, 0xc3, 0x80, 0x03,
            0x00, 0x60, 0x00, 0xfe, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x10, 0x00, 0xf6, 0x38,
            0xc4, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfe, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x12,
            0x00, 0xf6, 0x78, 0xc4, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfe, 0x38, 0xe0, 0x3e, 0x02, 0xc0,
            0xf3, 0x3c, 0x16, 0x00, 0xf6, 0xb8, 0xc4, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfe, 0x38, 0xe0,
            0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x1a, 0x00, 0xf6, 0xf8, 0xc4, 0x80, 0x03, 0x00, 0x60, 0x00,
            0xfe, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x1e, 0x00, 0xf6, 0x38, 0xc5, 0x80, 0x03,
            0x00, 0x60, 0x00, 0xfd, 0xa3, 0x87, 0x3f, 0xe2, 0xc0, 0xf3, 0x3c, 0x10, 0x00, 0xf6, 0x78,
            0xc5, 0x80, 0x03, 0x60, 0x40, 0x96, 0xf8, 0x30, 0xe0, 0x80, 0x03, 0xe0, 0x33, 0x00, 0x00,
            0x5a, 0x00,
            ],
            &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x80, 0x00, 0xf0, 0xf0, 0x0f, 0x01, 0x80, 0x00,
            0xc0, 0xff, 0x00, 0xff, 0x7f, 0x34, 0x12, 0xcc, 0xed, 0x01, 0x00, 0xfe, 0xff, 0x00, 0x40,
            0x10, 0x00, 0x00, 0x00, 0xf0, 0x00, 0xff, 0xff, 0xfe, 0xff, 0xfc, 0xff, 0xf1, 0xff, 0xf0,
            0xff, 0xe0, 0xff, 0x01, 0x00, 0x02, 0x00, 0xff, 0xff, 0xfd, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xfc, 0xff, 0xff, 0xff, 0xf8, 0xff,
            ],
            &[
                (0, "96810700968107009681070096810700968107009681070096810700968107009681070096810700968107009681070096810700968107009681070096810700"),
                (1, "96810000968100009681000096810000968100009681000096810000968100009681000096810000968100009681000096810000968100009681000096810000"),
                (2, "fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000"),
                (3, "07000000070000000700000007000000070000000700000007000000070000000700000007000000070000000700000007000000070000000700000007000000"),
                (4, "fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000"),
                (5, "01000000010000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000"),
                (6, "fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000"),
                (7, "fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000fe7f0000"),
                (8, "ed810600ed810600ed810600ed810600ed810600ed810600ed810600ed810600ed810600ed810600ed810600ed810600ed810600ed810600ed810600ed810600"),
                (9, "ed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffff"),
                (10, "ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000"),
                (11, "01000000010000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000"),
                (12, "ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000"),
                (13, "07000000070000000700000007000000070000000700000007000000070000000700000007000000070000000700000007000000070000000700000007000000"),
                (14, "ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000"),
                (15, "ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000"),
                (16, "ed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffff"),
                (17, "ed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffffed81ffff"),
                (18, "01000000010000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000"),
                (19, "07000000070000000700000007000000070000000700000007000000070000000700000007000000070000000700000007000000070000000700000007000000"),
                (20, "ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000"),
                (21, "96810700968107009681070096810700968107009681070096810700968107009681070096810700968107009681070096810700968107009681070096810700"),
                (30, "feff0000fe7f0000fcef0000e10f0000f17f0000e0bf0000000100000180000033120000c9ed000000000000fdff0000ff3f00000c000000ffff0000e8000000"),
                (62, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
                (63, "ffff0000feff0000fcff0000f1ff0000f0ff0000e0ff00000100000002000000ffff0000fdff0000ffff0000ffff0000ffff0000fcff0000ffff0000f8ff0000"),
            ],
        ),
        (
            "sru2.s: the same over ascending lanes, and over a tie",
            &[
            0x03, 0xb0, 0x40, 0x00, 0x06, 0x40, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38,
            0xc0, 0x00, 0x04, 0xc0, 0xfb, 0x00, 0x00, 0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3,
            0x10, 0x00, 0x08, 0xf8, 0xf8, 0x8f, 0xe0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0x60, 0x00,
            0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x14, 0x00, 0xf6, 0x38, 0xc0, 0x80, 0x03,
            0x00, 0x60, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x18, 0x00, 0xf6, 0x78,
            0xc0, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x1c,
            0x00, 0xf6, 0xb8, 0xc0, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0,
            0xf3, 0x3c, 0x1e, 0x00, 0xf6, 0xf8, 0xc0, 0x80, 0x03, 0x00, 0x60, 0x00, 0xfc, 0x38, 0xe0,
            0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x16, 0x00, 0xf6, 0x38, 0xc1, 0x80, 0x03, 0x00, 0x60, 0x00,
            0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x1a, 0x00, 0xf6, 0x78, 0xc1, 0x80, 0x03,
            0x00, 0x60, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x3c, 0x10, 0x00, 0xf6, 0xb8,
            0xc1, 0x80, 0x03, 0x00, 0xf4, 0x78, 0x8f, 0x3f, 0x02, 0x00, 0x60, 0x00, 0xfc, 0x38, 0xe0,
            0x3d, 0x02, 0xc0, 0xf3, 0x3c, 0x16, 0x00, 0xf6, 0xf8, 0xc1, 0x80, 0x03, 0x00, 0x60, 0x00,
            0xfc, 0x38, 0xe0, 0x3d, 0x02, 0xc0, 0xf3, 0x3c, 0x1a, 0x00, 0xf6, 0x38, 0xc2, 0x80, 0x03,
            0x60, 0x40, 0x96, 0xf8, 0x30, 0xe0, 0x80, 0x03, 0xe0, 0x33, 0x00, 0x00, 0x5a, 0x00,
            ],
            &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04, 0x00, 0x05, 0x00, 0x06,
            0x00, 0x07, 0x00, 0x08, 0x00, 0x09, 0x00, 0x0a, 0x00, 0x0b, 0x00, 0x0c, 0x00, 0x0d, 0x00,
            0x0e, 0x00, 0x0f, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
            &[
                (0, "10000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000010000000"),
                (1, "10000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000010000000"),
                (2, "10000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000010000000"),
                (3, "10000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000010000000100000001000000010000000"),
                (5, "0f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f000000"),
                (6, "88000000880000008800000088000000880000008800000088000000880000008800000088000000880000008800000088000000880000008800000088000000"),
                (8, "0f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f0000000f000000"),
                (62, "0100000002000000030000000400000005000000060000000700000008000000090000000a0000000b0000000c0000000d0000000e0000000f00000010000000"),
            ],
        ),
    ];

    for (name, code, vectors, rows) in PROGRAMS {
        let mut m = machine();
        let mut v = Vpu::new(CODE);
        for (i, b) in code.iter().enumerate() {
            m.store8(CODE + i as u32, *b).unwrap();
        }
        for i in 0..4096u32 {
            m.store8(0x4000 + i, (i + 1) as u8).unwrap();
            m.store8(0x5000 + i, 0).unwrap();
        }
        for (i, b) in vectors.iter().enumerate() {
            m.store8(0x5000 + i as u32, *b).unwrap();
        }
        v.regs.set(0, 0x8000);
        v.regs.set(1, 0x4000);
        v.regs.pc = CODE;
        for _ in 0..400 {
            if v.regs.pc == CODE + code.len() as u32 - 2 {
                break; // the trailing `rts`
            }
            assert_eq!(
                v.step(&mut m),
                Step::Ran,
                "{name} at {:#x}: {:?}",
                v.regs.pc,
                v.stopped
            );
        }
        for (row, want) in *rows {
            let got: String = (0..64)
                .map(|c| format!("{:02x}", m.load8(0x8000 + (*row as u32) * 64 + c).unwrap()))
                .collect();
            assert_eq!(&got, want, "{name}, row {row}");
        }
    }
}

/// The `...H` accumulator forms, read back with `vgetacc`.
///
/// `probes/acch.s` on a Raspberry Pi 4B d03115. `UACCH` and `SACCH`
/// accumulate the result into the accumulator's **high** half — `v16mov -,A
/// CLRA UACCH` leaves `A << 16` in it — and their write-back reads it shifted
/// back down by sixteen, clamped into the destination's signed range.
#[test]
fn the_high_half_accumulator() {
    const CODE_BYTES: &[u8] = &[
        0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
        0x04, 0xc0, 0xfb, 0x00, 0x00, 0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00,
        0x08, 0xf8, 0xf8, 0x8f, 0xe0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0xfc, 0x38, 0xe0, 0x3e,
        0x02, 0xc0, 0xf3, 0xbc, 0x09, 0x00, 0xf3, 0x23, 0xc0, 0x38, 0x82, 0x00, 0xfc, 0x38, 0xe0,
        0x3f, 0x02, 0xc0, 0xf3, 0xbc, 0x0c, 0x00, 0xf3, 0x63, 0xc0, 0x38, 0x82, 0x00, 0xfc, 0xb8,
        0x80, 0x3f, 0x02, 0xc0, 0xf3, 0xbc, 0x0c, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3,
        0xbc, 0x0b, 0x00, 0xf3, 0xe3, 0xc0, 0x38, 0x82, 0x00, 0xfc, 0x38, 0xe0, 0x3f, 0x02, 0xc0,
        0xf3, 0xbc, 0x0e, 0x00, 0xf3, 0x23, 0xc1, 0x38, 0x82, 0x00, 0xfc, 0x78, 0x81, 0x3f, 0x02,
        0xc0, 0xf3, 0xbc, 0x0e, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0xbc, 0x0d, 0x00,
        0xf3, 0xa3, 0xc1, 0x38, 0x82, 0x00, 0xfc, 0xf8, 0x81, 0x3e, 0x02, 0xc0, 0xf3, 0xbc, 0x0d,
        0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0xbc, 0x0f, 0x00, 0xf3, 0x23, 0xc2, 0x38,
        0x82, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0xbc, 0x09, 0x00, 0xfc, 0x38, 0xe0,
        0x3f, 0x02, 0xc0, 0xf3, 0xbc, 0x08, 0x00, 0xf3, 0x63, 0xc2, 0x38, 0x82, 0x00, 0xfc, 0x38,
        0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x7c, 0x09, 0x00, 0xf3, 0xa3, 0xc2, 0x38, 0x82, 0x96, 0xf8,
        0x30, 0xe0, 0x80, 0x03, 0xe0, 0x33, 0x00, 0x00, 0x5a, 0x00,
    ];
    const VECTORS: &[u8] = &[
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x80, 0x00, 0xf0, 0xf0, 0x0f, 0x01, 0x80, 0x00,
        0xc0, 0xff, 0x00, 0xff, 0x7f, 0x34, 0x12, 0xcc, 0xed, 0x01, 0x00, 0xfe, 0xff, 0x00, 0x40,
        0x10, 0x00, 0x00, 0x00, 0xf0, 0x00, 0xff, 0xff, 0xfe, 0xff, 0xfc, 0xff, 0xf1, 0xff, 0xf0,
        0xff, 0xe0, 0xff, 0x01, 0x00, 0x02, 0x00, 0xff, 0xff, 0xfd, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xfc, 0xff, 0xff, 0xff, 0xf8, 0xff,
    ];
    const ROWS: &[(usize, &str)] = &[
        (0, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
        (1, "ffffffff0080feff00f0fcfff00ff1ff0180f0ff00c0e0ffff000100ff7f02003412ffffccedfdff0100fffffeffffff0040ffff1000fcff0000fffff000f8ff"),
        (2, "ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f00000200000004000000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000ff7f0000"),
        (3, "ffffffff0080ffff00f0fffff00f00000180ffff00c0ffffff000000ff7f000034120000ccedffff01000000feffffff004000001000000000000000f0000000"),
        (4, "fffffeff0080fdff00f0fbfff00ff1ff0180efff00c0dfffff000100ff7f02003412ffffccedfcff0100fffffefffeff0040ffff1000fcff0000fffff000f8ff"),
        (5, "fdff0000fbff0000f7ff0000e2ff0000dfff0000bfff00000200000004000000feff0000f9ff0000feff0000fdff0000feff0000f8ff0000feff0000f0ff0000"),
        (6, "0000ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f000"),
        (7, "ff7f0000ff7f0000ff7f0000f00f0000ff7f0000ff7f0000ff000000ff7f000034120000ff7f000001000000ff7f0000004000001000000000000000f0000000"),
        (8, "0000ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f000"),
        (9, "feff0100fe7f0100fcef0100e10f0100f17f0100e0bf0100000100000180000033120100c9ed010000000100fdff0100ff3f01000c000100ffff0000e8000100"),
        (62, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
        (63, "ffff0000feff0000fcff0000f1ff0000f0ff0000e0ff00000100000002000000ffff0000fdff0000ffff0000ffff0000ffff0000fcff0000ffff0000f8ff0000"),
    ];

    let mut m = machine();
    let mut v = Vpu::new(CODE);
    for (i, b) in CODE_BYTES.iter().enumerate() {
        m.store8(CODE + i as u32, *b).unwrap();
    }
    for i in 0..4096u32 {
        m.store8(0x4000 + i, (i + 1) as u8).unwrap();
        m.store8(0x5000 + i, 0).unwrap();
    }
    for (i, b) in VECTORS.iter().enumerate() {
        m.store8(0x5000 + i as u32, *b).unwrap();
    }
    v.regs.set(0, 0x8000);
    v.regs.set(1, 0x4000);
    v.regs.pc = CODE;
    for _ in 0..64 {
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

/// `SUB` is a difference read-out, not a subtracting accumulate.
///
/// `probes/usub.s` on a Raspberry Pi 4B d03115: after `CLRA UACC(A)`, a
/// `USUB(B)` leaves `A` still in the accumulator — read back with `vgetacc` —
/// and hands the destination `A - B`.
#[test]
fn the_sub_modifier_leaves_the_accumulator_alone() {
    const CODE_BYTES: &[u8] = &[
        0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
        0x04, 0xc0, 0xfb, 0x00, 0x00, 0x08, 0xf8, 0xb8, 0x8f, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00,
        0x08, 0xf8, 0xf8, 0x8f, 0xe0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x00, 0xfc, 0x38, 0xe0, 0x3e,
        0x02, 0xc0, 0xf3, 0xbc, 0x09, 0x00, 0xfc, 0x38, 0xe0, 0x3f, 0x02, 0xc0, 0xf3, 0x7c, 0x08,
        0x00, 0xf3, 0x23, 0xc0, 0x38, 0x82, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0x7c,
        0x09, 0x00, 0xf3, 0x63, 0xc0, 0x38, 0x82, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3,
        0xbc, 0x0b, 0x00, 0xfc, 0x38, 0xe0, 0x3f, 0x02, 0xc0, 0xf3, 0x7c, 0x0a, 0x00, 0xf3, 0xa3,
        0xc0, 0x38, 0x82, 0x00, 0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0xbc, 0x0b, 0x00, 0xfc,
        0x38, 0xe0, 0x3f, 0x02, 0xc0, 0xf3, 0x7c, 0x0e, 0x00, 0xf3, 0xe3, 0xc0, 0x38, 0x82, 0x00,
        0xfc, 0x38, 0xe0, 0x3e, 0x02, 0xc0, 0xf3, 0xbc, 0x09, 0x00, 0xfc, 0x38, 0xe0, 0x3f, 0x02,
        0xc0, 0xf3, 0x7c, 0x08, 0x00, 0xfc, 0x38, 0x81, 0x3f, 0x02, 0xc0, 0xf3, 0x7c, 0x08, 0x00,
        0xf3, 0x63, 0xc1, 0x38, 0x82, 0x96, 0xf8, 0x30, 0xe0, 0x80, 0x03, 0xe0, 0x33, 0x00, 0x00,
        0x5a, 0x00,
    ];
    const VECTORS: &[u8] = &[
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x80, 0x00, 0xf0, 0xf0, 0x0f, 0x01, 0x80, 0x00,
        0xc0, 0xff, 0x00, 0xff, 0x7f, 0x34, 0x12, 0xcc, 0xed, 0x01, 0x00, 0xfe, 0xff, 0x00, 0x40,
        0x10, 0x00, 0x00, 0x00, 0xf0, 0x00, 0xff, 0xff, 0xfe, 0xff, 0xfc, 0xff, 0xf1, 0xff, 0xf0,
        0xff, 0xe0, 0xff, 0x01, 0x00, 0x02, 0x00, 0xff, 0xff, 0xfd, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xfc, 0xff, 0xff, 0xff, 0xf8, 0xff,
    ];
    const ROWS: &[(usize, &str)] = &[
        (0, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
        (2, "ffffffff0080ffff00f0fffff00f00000180ffff00c0ffffff000000ff7f000034120000ccedffff01000000feffffff004000001000000000000000f0000000"),
        (3, "ffffffff0080ffff00f0fffff00f00000180ffff00c0ffffff000000ff7f000034120000ccedffff01000000feffffff004000001000000000000000f0000000"),
        (4, "000000000280000004f00000ff0f00001180000020c00000fe000000fd7f000035120000cfed000002000000ffff0000014000001400000001000000f8000000"),
        (5, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
        (62, "ffff00000080000000f00000f00f00000180000000c00000ff000000ff7f000034120000cced000001000000feff0000004000001000000000000000f0000000"),
        (63, "ffff0000feff0000fcff0000f1ff0000f0ff0000e0ff00000100000002000000ffff0000fdff0000ffff0000ffff0000ffff0000fcff0000ffff0000f8ff0000"),
    ];

    let mut m = machine();
    let mut v = Vpu::new(CODE);
    for (i, b) in CODE_BYTES.iter().enumerate() {
        m.store8(CODE + i as u32, *b).unwrap();
    }
    for i in 0..4096u32 {
        m.store8(0x4000 + i, (i + 1) as u8).unwrap();
        m.store8(0x5000 + i, 0).unwrap();
    }
    for (i, b) in VECTORS.iter().enumerate() {
        m.store8(0x5000 + i as u32, *b).unwrap();
    }
    v.regs.set(0, 0x8000);
    v.regs.set(1, 0x4000);
    v.regs.pc = CODE;
    for _ in 0..64 {
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

/// The gather and the scatter, indexed by the accumulator.
///
/// `probes/mem9.s` on a Raspberry Pi 4B d03115, over a page whose byte `n`
/// holds `n + 1`. `lookupml` reads element `acc & 0xffff` of the table at the
/// address and `lookupm` element `acc >> 16`, each element as wide as the
/// operation; `indexwritem[l]` writes one back at the same place. The rows
/// below include the scattered bytes read back in.
#[test]
fn the_measured_gather_and_scatter() {
    const CODE_BYTES: &[u8] = &[
        0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
        0x04, 0xc0, 0xfb, 0x00, 0x00, 0x08, 0xf8, 0x38, 0x85, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00,
        0x00, 0xf8, 0x78, 0x05, 0xa0, 0x03, 0xc0, 0xf3, 0x04, 0x00, 0x00, 0xfc, 0x38, 0xe0, 0x14,
        0x02, 0xc0, 0xf3, 0xbc, 0x09, 0x40, 0xf0, 0x38, 0x00, 0x81, 0x03, 0x48, 0xf0, 0x78, 0x80,
        0x81, 0x03, 0x50, 0xf0, 0xb8, 0xc0, 0x81, 0x03, 0x40, 0xf8, 0xf8, 0x00, 0x87, 0x03, 0xc0,
        0xf3, 0x04, 0x00, 0x00, 0xfc, 0x38, 0xe0, 0x14, 0x02, 0xc0, 0xf3, 0xbc, 0x0d, 0x20, 0xf0,
        0x38, 0x01, 0x81, 0x03, 0x28, 0xf0, 0x78, 0x81, 0x81, 0x03, 0x30, 0xf0, 0xb8, 0xc1, 0x81,
        0x03, 0x00, 0xfc, 0x38, 0xe0, 0x14, 0x02, 0xc0, 0xf3, 0xbc, 0x09, 0xc0, 0xf8, 0x01, 0xe0,
        0x80, 0x53, 0xc0, 0xf3, 0x12, 0x00, 0x00, 0xf8, 0xf8, 0x01, 0x80, 0x03, 0xc0, 0xf3, 0x12,
        0x00, 0xc8, 0xf8, 0x21, 0xe0, 0x80, 0x43, 0xc0, 0xf3, 0x50, 0x00, 0x08, 0xf8, 0x38, 0x82,
        0x80, 0x03, 0xc0, 0xf3, 0x50, 0x00, 0x00, 0xfc, 0x38, 0xe0, 0x14, 0x02, 0xc0, 0xf3, 0xbc,
        0x0d, 0xa0, 0xf8, 0x01, 0xe0, 0x80, 0x53, 0xc0, 0xf3, 0x52, 0x00, 0x00, 0xf8, 0x78, 0x02,
        0x80, 0x03, 0xc0, 0xf3, 0x52, 0x00, 0x96, 0xf8, 0x30, 0xe0, 0x80, 0x03, 0xe0, 0x33, 0x00,
        0x00, 0x5a, 0x00,
    ];
    const VECTORS: &[u8] = &[
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x08, 0x00, 0x0c, 0x00, 0x01, 0x00, 0x05,
        0x00, 0x09, 0x00, 0x0d, 0x00, 0x02, 0x00, 0x06, 0x00, 0x0a, 0x00, 0x0e, 0x00, 0x03, 0x00,
        0x07, 0x00, 0x0b, 0x00, 0x0f, 0x00, 0x0f, 0x00, 0x0e, 0x00, 0x0d, 0x00, 0x0c, 0x00, 0x0b,
        0x00, 0x0a, 0x00, 0x09, 0x00, 0x08, 0x00, 0x07, 0x00, 0x06, 0x00, 0x05, 0x00, 0x04, 0x00,
        0x03, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00, 0x00,
    ];
    const ROWS: &[(usize, &str)] = &[
        (0, "0100000005000000090000000d00000002000000060000000a0000000e00000003000000070000000b0000000f00000004000000080000000c00000010000000"),
        (1, "01020000090a000011120000191a0000030400000b0c0000131400001b1c0000050600000d0e0000151600001d1e0000070800000f100000171800001f200000"),
        (2, "0102030411121314212223243132333405060708151617182526272835363738090a0b0c191a1b1c292a2b2c393a3b3c0d0e0f101d1e1f202d2e2f303d3e3f40"),
        (3, "080000000c0000001000000014000000090000000d00000011000000150000000a0000000e00000012000000160000000b0000000f0000001300000017000000"),
        (4, "0100000005000000090000000d00000002000000060000000a0000000e00000003000000070000000b0000000f00000004000000080000000c00000010000000"),
        (5, "01020000090a000011120000191a0000030400000b0c0000131400001b1c0000050600000d0e0000151600001d1e0000070800000f100000171800001f200000"),
        (6, "0102030411121314212223243132333405060708151617182526272835363738090a0b0c191a1b1c292a2b2c393a3b3c0d0e0f101d1e1f202d2e2f303d3e3f40"),
        (7, "2100000025000000290000002d00000022000000260000002a0000002e00000023000000270000002b0000002f00000024000000280000002c00000030000000"),
        (8, "000000000100000002000000030000000400000005000000060000000700000008000000090000000a0000000b0000000c0000000d0000000e0000000f000000"),
        (9, "2100000025000000290000002d00000022000000260000002a0000002e00000023000000270000002b0000002f00000024000000280000002c00000030000000"),
        (20, "0000000004000000080000000c0000000100000005000000090000000d00000002000000060000000a0000000e00000003000000070000000b0000000f000000"),
        (21, "2100000022000000230000002400000025000000260000002700000028000000290000002a0000002b0000002c0000002d0000002e0000002f00000030000000"),
    ];

    let mut m = machine();
    let mut v = Vpu::new(CODE);
    for (i, b) in CODE_BYTES.iter().enumerate() {
        m.store8(CODE + i as u32, *b).unwrap();
    }
    for i in 0..4096u32 {
        m.store8(0x4000 + i, (i + 1) as u8).unwrap();
        m.store8(0x5000 + i, 0).unwrap();
    }
    for (i, b) in VECTORS.iter().enumerate() {
        m.store8(0x5000 + i as u32, *b).unwrap();
    }
    v.regs.set(0, 0x8000);
    v.regs.set(1, 0x4000);
    v.regs.pc = CODE;
    for _ in 0..64 {
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

/// `vmul32` — the family the `L` bit selects.
///
/// `probes/mul32.s` on a Raspberry Pi 4B d03115: it is a 16 x 16 into 32
/// multiply, taking the low halfword of each operand — signed or unsigned as
/// the suffix says — and keeping the whole product.
#[test]
fn the_measured_16_by_16_multiply() {
    const CODE_BYTES: &[u8] = &[
        0x03, 0xb0, 0x40, 0x00, 0x14, 0x40, 0x44, 0xb0, 0x00, 0x10, 0x06, 0xfe, 0x38, 0xc0, 0x00,
        0x04, 0xc0, 0xfb, 0x00, 0x00, 0x04, 0xfe, 0x38, 0xc0, 0xff, 0x07, 0xc0, 0xfb, 0x3f, 0x00,
        0x10, 0xf8, 0xb8, 0xcf, 0xc0, 0x03, 0xc0, 0xf3, 0x10, 0x00, 0x10, 0xf8, 0xf8, 0xcf, 0x80,
        0x03, 0xc0, 0xf3, 0x11, 0x00, 0xa0, 0xf7, 0x33, 0xc0, 0x3f, 0xe3, 0xa8, 0xf7, 0x73, 0xc0,
        0x3f, 0xe3, 0xb0, 0xf7, 0xb3, 0xc0, 0x3f, 0xe3, 0xb8, 0xf7, 0xf3, 0xc0, 0x3f, 0xe3, 0xb8,
        0xf7, 0x33, 0x81, 0x3f, 0xe3, 0xa0, 0xff, 0x73, 0xc1, 0x3f, 0xe3, 0xc0, 0xf3, 0xbc, 0x0b,
        0x00, 0xf3, 0xb3, 0xc1, 0x38, 0x83, 0x96, 0xf8, 0x30, 0xe0, 0x80, 0x03, 0xe0, 0x33, 0x00,
        0x00, 0x5a, 0x00,
    ];
    const VECTORS: &[u8] = &[
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff,
        0xff, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xff, 0xff, 0x00, 0x00, 0x78, 0x56,
        0x34, 0x12, 0xfe, 0xff, 0xff, 0xff, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00,
        0x01, 0x00, 0x00, 0x01, 0xef, 0xcd, 0xab, 0x05, 0x00, 0x00, 0x00, 0xff, 0x7f, 0x00, 0x00,
        0x01, 0x00, 0x00, 0x10, 0xef, 0xbe, 0xad, 0xde, 0x00, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00,
        0x00, 0x03, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x7f, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x01, 0x00, 0x10, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x07, 0x00, 0x00, 0x00, 0x04,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x30, 0x00, 0x00, 0x00,
        0x01, 0x00, 0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
    ];
    const ROWS: &[(usize, &str)] = &[
        (0, "00000000fefffffffdfffffffeffffff00000000ffffffff806705000200000015000000000000000000010002defffff0000000ff7f000003000000de7dffff"),
        (1, "00000000fefffffffdfffffffeff010000000000ffffffff806705000200feff15000000000000000000010002defffff0000000ff7f000003000000de7dffff"),
        (2, "00000000feff0100fdff0200feffffff00000000ffff0000806705000200ffff15000000000000000000010002de0100f0000000ff7f000003000000de7d0100"),
        (3, "00000000feff0100fdff0200feff010000000000ffff0000806705000200fdff15000000000000000000010002de0100f0000000ff7f000003000000de7d0100"),
        (4, "0000fffffefffffffdfffffffeffffff0000ffffffffffff8067ffff0200ffff1500ffff0000ffff0000ffff02defffff000ffffff7fffff0300ffffde7dffff"),
        (5, "00000000fefffffffdfffffffeffffff00000000ffffffff806705000200000015000000000000000000010002defffff0000000ff7f000003000000de7dffff"),
        (6, "00000000fefffffffdfffffffeffffff00000000ffffffff806705000200000015000000000000000000010002defffff0000000ff7f000003000000de7dffff"),
        (7, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        (8, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        (9, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        (10, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        (11, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        (12, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        (13, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        (14, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        (15, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        (62, "00000100ffffff7fffffffff0200000000000080ffff000078563412feffffff03000000000000400001000001efcdab05000000ff7f000001000010efbeadde"),
        (63, "000001000200000003000000ffffff7f020000000100010010000000ffffffff0700000004000000000100000200000030000000010001000300000002000000"),
    ];

    let mut m = machine();
    let mut v = Vpu::new(CODE);
    for (i, b) in CODE_BYTES.iter().enumerate() {
        m.store8(CODE + i as u32, *b).unwrap();
    }
    for i in 0..4096u32 {
        m.store8(0x4000 + i, (i + 1) as u8).unwrap();
        m.store8(0x5000 + i, 0).unwrap();
    }
    for (i, b) in VECTORS.iter().enumerate() {
        m.store8(0x5000 + i as u32, *b).unwrap();
    }
    v.regs.set(0, 0x8000);
    v.regs.set(1, 0x4000);
    v.regs.pc = CODE;
    for _ in 0..64 {
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
