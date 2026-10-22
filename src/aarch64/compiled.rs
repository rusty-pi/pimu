//! What compiled runs share, whichever backend compiled them: the context
//! they run in and the helpers they call back into (`src/aarch64/jit.rs` on
//! Cranelift, `src/aarch64/wasmjit.rs` in the browser).

use super::cpu::Cpu;
use super::predecode::{handler_of, Kind, RunBus, RunOp};

/// What a compiled run needs besides the core and the bus.
#[repr(C)]
pub struct Ctx {
    /// The write generation of the run's page when it was entered.
    pub gen: u64,
    /// Another core holds an exclusive mark, which a store may clear.
    pub marked: u64,
    /// The run's page's write generation: a store into that page stops the
    /// run.
    pub genp: *const u64,
    /// The cycle a branch may go on into the next translation before; 0 for
    /// none.
    pub chain_until: u64,
}

pub(super) type Step =
    unsafe extern "C" fn(*mut Cpu, *mut u8, *const Ctx, *const RunOp, u64, u64, *mut u64) -> u32;

/// [`step`] for instructions of `kind`.
pub(super) fn step_of<M: RunBus>(kind: Kind) -> Step {
    macro_rules! steps {
        ($($k:ident),*) => {
            match kind {
                $(Kind::$k => {
                    let f: unsafe extern "C" fn(*mut Cpu, *mut M, *const Ctx, *const RunOp, u64, u64, *mut u64) -> u32 =
                        step::<M, { Kind::$k as u8 }>;
                    // The bus pointer is an `M`'s either way.
                    unsafe {
                        std::mem::transmute::<
                            unsafe extern "C" fn(*mut Cpu, *mut M, *const Ctx, *const RunOp, u64, u64, *mut u64) -> u32,
                            Step,
                        >(f)
                    }
                })*
            }
        };
    }
    steps!(
        Execute, BranchReg, B, Bl, BCond, Cbz, Tbz, LdrX, LdrW, LdrH, LdrB, StrX, StrW, StrH, StrB,
        Barrier, LdStReg, LdStPair, LoadUimm, StoreUimm, LdStRegOff, LdStImm, PairV, Pair, DpImm,
        DpReg, Adr, Adrp, SimdDp, Mrs, Nop, CmpImm, CmpImmW, CmpReg, CmpRegW, AddImmR, SubImmR,
        MovReg, AddImm, SubImm, AddsImm, SubsImm, MovConst, MovK, LogicImm, LogicReg, AddSubReg,
        Bitfield, Ccmp, Csel, Madd
    )
}

/// One instruction of kind `K` the compiled code hands back: what `go_kind`
/// does with it, without going on to the next. 0 to go on, else the run
/// stopped and `out` has where.
unsafe extern "C" fn step<M: RunBus, const K: u8>(
    cpu: *mut Cpu,
    bus: *mut M,
    ctx: *const Ctx,
    o: *const RunOp,
    pc: u64,
    c: u64,
    out: *mut u64,
) -> u32 {
    let (cpu, bus, ctx, o) = (&mut *cpu, &mut *bus, &*ctx, &*o);
    let op = &o.op;
    // `K` is one of `Kind`'s, from `step_of`.
    let kind = std::mem::transmute::<u8, Kind>(K);
    let mem = (Kind::LdrX as u8..=Kind::Pair as u8).contains(&K);
    if mem {
        bus.at(c);
    }
    let next = handler_of::<M>(kind)(cpu, op, bus, pc);
    if !cpu.faulted() && (!mem || bus.settle(ctx.gen, ctx.genp, ctx.marked != 0)) {
        return 0;
    }
    bus.stop(o, next, true);
    *out = pc;
    *out.add(1) = c;
    1
}

/// The branch `o` at `pc` taken to `next`: back to `Straight::run`, which
/// goes on into the run there, compiled too if it is.
pub(super) unsafe extern "C" fn take<M: RunBus>(
    _: *mut Cpu,
    bus: *mut M,
    o: *const RunOp,
    pc: u64,
    next: u64,
    c: u64,
    out: *mut u64,
) {
    (*bus).stop(&*o, next, true);
    *out = pc;
    *out.add(1) = c;
}

/// The last instruction of the run, as the interpreter runs it.
pub(super) unsafe extern "C" fn last<M: RunBus>(
    cpu: *mut Cpu,
    bus: *mut M,
    o: *const RunOp,
    pc: u64,
    c: u64,
    out: *mut u64,
) {
    let (pc, c) = (*o).go(&mut *cpu, &mut *bus, pc, c);
    *out = pc;
    *out.add(1) = c;
}
