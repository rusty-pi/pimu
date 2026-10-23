//! Instructions decoded once and executed from the decoded form.
//!
//! Decoding is a pure function of the instruction word, so the cache is keyed
//! by the word alone: a slot is good for as long as it holds the word just
//! fetched. Nothing has to be invalidated — code rewritten in place, DMA into
//! a code page and `TLBI` all leave a slot that either still matches the word
//! the fetch returns or does not.
//!
//! The forms a boot executes most get a handler of their own, which does what
//! [`exec::execute`] would without walking the encoding tables again. Every
//! other word, and every reserved encoding of a handled form, is
//! [`Kind::Execute`]: the word goes to [`exec::execute`] as it is.

use super::cpu::{Cpu, Memory};
use super::exec::{
    self, add_with_carry, bit, decode_bit_masks, extend_reg, field, logic_flags, mask, ror, sext,
    shift_reg, single_access, Access, Exec,
};
use super::mmu::{load_le, store_le};
use std::cell::Cell;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub(super) enum Kind {
    // The last of a straight run (`Op::ends_run`), then the conditional
    // branches it runs through when they are not taken (`Op::cond_branch`).
    #[default]
    Execute,
    BranchReg,
    B,
    Bl,
    BCond,
    Cbz,
    Tbz,
    // Loads and stores (`Op::touches_memory`).
    LdrX,
    LdrW,
    LdrH,
    LdrB,
    StrX,
    StrW,
    StrH,
    StrB,
    Barrier,
    LdStReg,
    LdStPair,
    LoadUimm,
    StoreUimm,
    LdStRegOff,
    LdStImm,
    /// `LDP`/`STP` (and the non-temporal forms) of S, D or Q registers.
    PairV,
    Pair,
    // The PC, or an exception at it.
    DpImm,
    DpReg,
    Adr,
    Adrp,
    /// SIMD and floating point data processing: the vector and FP registers,
    /// `FPSR` and `NZCV` only, through `exec::execute`. A run goes on
    /// through it, where `Execute` would end there.
    SimdDp,
    /// `MRS` of a register the core holds itself (`sysreg::core_only`).
    Mrs,
    // Registers and flags only (`Op::pure`).
    Nop,
    CmpImm,
    CmpImmW,
    CmpReg,
    CmpRegW,
    AddImmR,
    SubImmR,
    MovReg,
    AddImm,
    SubImm,
    AddsImm,
    SubsImm,
    MovConst,
    MovK,
    LogicImm,
    LogicReg,
    AddSubReg,
    Bitfield,
    Ccmp,
    Csel,
    Madd,
}

/// One decoded instruction. What `a`, `b`, `c` and `imm` hold depends on
/// [`Kind`]; [`decode`] and [`Cpu::run_op`] are the two sides of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Op {
    pub(super) insn: u32,
    pub(super) kind: Kind,
    pub(super) a: u8,
    pub(super) b: u8,
    pub(super) c: u8,
    pub(super) imm: u64,
    pub(super) imm2: u64,
}

impl Op {
    pub fn insn(&self) -> u32 {
        self.insn
    }

    /// Whether the instruction loads or stores; the other forms a straight
    /// run holds before its last touch nothing but registers and flags.
    #[inline(always)]
    pub fn touches_memory(&self) -> bool {
        (Kind::LdrX as u8..=Kind::Pair as u8).contains(&(self.kind as u8))
    }

    /// Whether the instruction always moves the PC anywhere but on by 4, or
    /// does anything else [`exec::execute`] does: the last of a straight run.
    #[inline(always)]
    pub fn ends_run(&self) -> bool {
        self.kind as u8 <= Kind::Bl as u8
    }

    /// Whether the instruction is a conditional branch: a straight run goes
    /// on through it when it is not taken.
    #[inline(always)]
    pub fn cond_branch(&self) -> bool {
        (Kind::BCond as u8..=Kind::Tbz as u8).contains(&(self.kind as u8))
    }

    /// Whether the instruction is a direct or conditional branch: it ends
    /// its run, reads registers and flags only and cannot raise an
    /// exception.
    #[inline(always)]
    pub fn plain_branch(&self) -> bool {
        (Kind::B as u8..=Kind::Tbz as u8).contains(&(self.kind as u8))
    }

    /// Whether the instruction is a `BR`, `BLR` or `RET`, which a run may go
    /// on from through [`RunBus::indirect`].
    #[inline(always)]
    pub fn indirect_branch(&self) -> bool {
        self.kind == Kind::BranchReg && self.insn & 0xFF9F_FC1F == 0xD61F_0000
    }

    /// Whether the instruction reads and writes nothing but general
    /// registers and flags, and cannot raise an exception: it needs no PC.
    #[inline(always)]
    pub fn pure(&self) -> bool {
        self.kind as u8 >= Kind::Nop as u8
    }
}

const SLOTS_LOG2: u32 = 16;

/// The decoded forms of recently executed instruction words.
pub struct Decoded {
    slots: Box<[Op; 1 << SLOTS_LOG2]>,
}

impl Default for Decoded {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoded {
    pub fn new() -> Decoded {
        let slots = vec![Op::default(); 1 << SLOTS_LOG2].into_boxed_slice();
        Decoded {
            slots: slots.try_into().unwrap_or_else(|_| unreachable!()),
        }
    }

    /// `insn`, decoded.
    #[inline(always)]
    pub fn get(&mut self, insn: u32) -> Op {
        *self.slot(insn)
    }

    /// [`Self::get`], in place.
    #[inline(always)]
    pub fn slot(&mut self, insn: u32) -> &Op {
        let i = (insn.wrapping_mul(0x9E37_79B1) >> (32 - SLOTS_LOG2)) as usize;
        let slot = &mut self.slots[i];
        if slot.insn != insn {
            *slot = decode(insn);
        }
        slot
    }
}

fn op(insn: u32, kind: Kind, a: u32, b: u32, c: u32, imm: u64) -> Op {
    Op {
        insn,
        kind,
        a: a as u8,
        b: b as u8,
        c: c as u8,
        imm,
        imm2: 0,
    }
}

fn op2(insn: u32, kind: Kind, a: u32, b: u32, c: u32, imm: u64, imm2: u64) -> Op {
    Op {
        imm2,
        ..op(insn, kind, a, b, c, imm)
    }
}

/// [`Access`] as two bits, and back.
fn access_bits(a: Access) -> u32 {
    match a {
        Access::Store => 0,
        Access::Load => 1,
        Access::LoadSigned64 => 2,
        Access::LoadSigned32 => 3,
    }
}

#[inline(always)]
fn access_of(bits: u32) -> Access {
    match bits & 3 {
        0 => Access::Store,
        1 => Access::Load,
        2 => Access::LoadSigned64,
        _ => Access::LoadSigned32,
    }
}

#[inline(never)]
pub(super) fn decode(insn: u32) -> Op {
    let generic = |kind| op(insn, kind, 0, 0, 0, 0);
    match field(insn, 25, 4) {
        0b1000 | 0b1001 => dp_imm(insn).unwrap_or_else(|| generic(Kind::DpImm)),
        0b1010 | 0b1011 => branch(insn).unwrap_or_else(|| generic(Kind::Execute)),
        0b0100 | 0b0110 | 0b1100 | 0b1110 => ldst(insn).unwrap_or_else(|| generic(Kind::Execute)),
        0b0101 | 0b1101 => dp_reg(insn).unwrap_or_else(|| generic(Kind::DpReg)),
        0b0111 | 0b1111 => generic(Kind::SimdDp),
        _ => generic(Kind::Execute),
    }
}

fn dp_imm(insn: u32) -> Option<Op> {
    let sf = bit(insn, 31);
    let rd = field(insn, 0, 5);
    let rn = field(insn, 5, 5);
    match field(insn, 23, 3) {
        0b000 | 0b001 => {
            let imm = sext(((field(insn, 5, 19) << 2) | field(insn, 29, 2)) as u64, 21);
            Some(if sf {
                op(insn, Kind::Adrp, rd, 0, 0, imm << 12)
            } else {
                op(insn, Kind::Adr, rd, 0, 0, imm)
            })
        }
        0b010 => {
            let mut imm = field(insn, 10, 12) as u64;
            if bit(insn, 22) {
                imm <<= 12;
            }
            let kind = match (bit(insn, 30), bit(insn, 29)) {
                (false, false) if rd != 31 && rn != 31 => Kind::AddImmR,
                (false, false) => Kind::AddImm,
                (true, false) if rd != 31 && rn != 31 => Kind::SubImmR,
                (true, false) => Kind::SubImm,
                (false, true) => Kind::AddsImm,
                (true, true) if rd == 31 && rn != 31 && sf => Kind::CmpImm,
                (true, true) if rd == 31 && rn != 31 => Kind::CmpImmW,
                (true, true) => Kind::SubsImm,
            };
            Some(op(insn, kind, rd, rn, sf as u32, imm))
        }
        0b100 => {
            let n = field(insn, 22, 1);
            if !sf && n != 0 {
                return None;
            }
            let datasize = if sf { 64 } else { 32 };
            let (imm, _) =
                decode_bit_masks(n, field(insn, 10, 6), field(insn, 16, 6), true, datasize)?;
            let opc = field(insn, 29, 2);
            Some(op(
                insn,
                Kind::LogicImm,
                rd,
                rn,
                opc | (sf as u32) << 2,
                imm,
            ))
        }
        0b101 => {
            let opc = field(insn, 29, 2);
            let hw = field(insn, 21, 2);
            if opc == 1 || (!sf && hw >= 2) {
                return None;
            }
            let pos = hw * 16;
            let imm = (field(insn, 5, 16) as u64) << pos;
            Some(match opc {
                0 => op(insn, Kind::MovConst, rd, 0, 0, mask(!imm, sf)),
                2 => op(insn, Kind::MovConst, rd, 0, 0, imm),
                _ => op(insn, Kind::MovK, rd, pos, sf as u32, imm),
            })
        }
        0b110 => {
            let opc = field(insn, 29, 2);
            let n = field(insn, 22, 1);
            let immr = field(insn, 16, 6);
            let imms = field(insn, 10, 6);
            if opc == 3 || n != sf as u32 || (!sf && (immr | imms) & 0x20 != 0) {
                return None;
            }
            let datasize = if sf { 64 } else { 32 };
            let (wmask, tmask) = decode_bit_masks(n, imms, immr, false, datasize)?;
            Some(op2(insn, Kind::Bitfield, rd, rn, immr, wmask, tmask))
        }
        _ => None,
    }
}

fn branch(insn: u32) -> Option<Op> {
    if insn & 0x7C00_0000 == 0x1400_0000 {
        let off = sext(field(insn, 0, 26) as u64, 26) << 2;
        let kind = if bit(insn, 31) { Kind::Bl } else { Kind::B };
        Some(op(insn, kind, 0, 0, 0, off))
    } else if insn & 0x7E00_0000 == 0x3400_0000 {
        let off = sext(field(insn, 5, 19) as u64, 19) << 2;
        let flags = bit(insn, 31) as u32 | (bit(insn, 24) as u32) << 1;
        Some(op(insn, Kind::Cbz, field(insn, 0, 5), 0, flags, off))
    } else if insn & 0x7E00_0000 == 0x3600_0000 {
        let b = (field(insn, 31, 1) << 5) | field(insn, 19, 5);
        let off = sext(field(insn, 5, 14) as u64, 14) << 2;
        Some(op(
            insn,
            Kind::Tbz,
            field(insn, 0, 5),
            b,
            bit(insn, 24) as u32,
            off,
        ))
    } else if insn & 0xFF00_0010 == 0x5400_0000 {
        let off = sext(field(insn, 5, 19) as u64, 19) << 2;
        Some(op(insn, Kind::BCond, field(insn, 0, 4), 0, 0, off))
    } else if insn & 0xFFFF_F01F == 0xD503_201F && !(2..=5).contains(&field(insn, 5, 7)) {
        // NOP, YIELD and the hints the A72 treats as them.
        Some(op(insn, Kind::Nop, 0, 0, 0, 0))
    } else if insn & 0xFFFF_F0FF == 0xD503_30DF {
        // ISB: nothing in the model is out of date for it to resynchronise.
        Some(op(insn, Kind::Nop, 0, 0, 0, 0))
    } else if insn & 0xFFFF_F0DF == 0xD503_309F {
        // DSB, DMB.
        Some(op(insn, Kind::Barrier, 0, 0, 0, 0))
    } else if insn & 0xFFF0_0000 == 0xD530_0000 && super::sysreg::core_only(field(insn, 5, 16)) {
        Some(op(
            insn,
            Kind::Mrs,
            field(insn, 0, 5),
            0,
            0,
            field(insn, 5, 16) as u64,
        ))
    } else if insn & 0xFE00_0000 == 0xD600_0000 {
        Some(op(insn, Kind::BranchReg, 0, 0, 0, 0))
    } else {
        None
    }
}

fn ldst(insn: u32) -> Option<Op> {
    if insn & 0x3F00_0000 == 0x0800_0000 || insn & 0x3B00_0000 == 0x1800_0000 {
        None
    } else if insn & 0x3A00_0000 == 0x2800_0000 {
        Some(pair(insn).unwrap_or_else(|| op(insn, Kind::LdStPair, 0, 0, 0, 0)))
    } else if insn & 0x3B00_0000 == 0x3800_0000 {
        Some(ld_st_reg(insn).unwrap_or_else(|| op(insn, Kind::LdStReg, 0, 0, 0, 0)))
    } else if insn & 0x3B00_0000 == 0x3900_0000 {
        if bit(insn, 26) {
            return None;
        }
        let Some((scale, access)) =
            single_access(field(insn, 30, 2), false, field(insn, 22, 2)).ok()?
        else {
            // PRFM: the model has no caches to fill.
            return Some(op(insn, Kind::Nop, 0, 0, 0, 0));
        };
        let rt = field(insn, 0, 5);
        let rn = field(insn, 5, 5);
        let imm = (field(insn, 10, 12) as u64) << scale;
        let sized = |kinds: [Kind; 4], k| if rn == 31 { k } else { kinds[scale as usize] };
        Some(match access {
            Access::Store => op(
                insn,
                sized(
                    [Kind::StrB, Kind::StrH, Kind::StrW, Kind::StrX],
                    Kind::StoreUimm,
                ),
                rt,
                rn,
                scale,
                imm,
            ),
            Access::Load => op(
                insn,
                sized(
                    [Kind::LdrB, Kind::LdrH, Kind::LdrW, Kind::LdrX],
                    Kind::LoadUimm,
                ),
                rt,
                rn,
                scale,
                imm,
            ),
            Access::LoadSigned64 => op(insn, Kind::LoadUimm, rt, rn, scale | 4, imm),
            Access::LoadSigned32 => op(insn, Kind::LoadUimm, rt, rn, scale | 8, imm),
        })
    } else {
        None
    }
}

fn pair_v(insn: u32) -> Option<Op> {
    let opc = field(insn, 30, 2);
    if opc == 3 {
        return None;
    }
    let scale = 2 + opc;
    let mode = field(insn, 23, 2);
    let store = !bit(insn, 22);
    let off = sext(field(insn, 15, 7) as u64, 7) << scale;
    let flags = scale | (store as u32) << 3 | mode << 4;
    Some(op2(
        insn,
        Kind::PairV,
        field(insn, 0, 5),
        field(insn, 5, 5),
        field(insn, 10, 5),
        off,
        flags as u64,
    ))
}

fn pair(insn: u32) -> Option<Op> {
    if bit(insn, 26) {
        return pair_v(insn);
    }
    let opc = field(insn, 30, 2);
    let mode = field(insn, 23, 2);
    let load = bit(insn, 22);
    let (scale, access) = match opc {
        0 => (2, Access::Load),
        1 if load && mode != 0 => (2, Access::LoadSigned64),
        2 => (3, Access::Load),
        _ => return None,
    };
    let access = if load { access } else { Access::Store };
    let off = sext(field(insn, 15, 7) as u64, 7) << scale;
    let flags = scale | access_bits(access) << 2 | mode << 4;
    Some(op2(
        insn,
        Kind::Pair,
        field(insn, 0, 5),
        field(insn, 5, 5),
        field(insn, 10, 5),
        off,
        flags as u64,
    ))
}

fn ld_st_reg(insn: u32) -> Option<Op> {
    if bit(insn, 26) {
        return None;
    }
    let (rt, rn) = (field(insn, 0, 5), field(insn, 5, 5));
    let mode = field(insn, 10, 2);
    let (scale, access) = single_access(field(insn, 30, 2), false, field(insn, 22, 2)).ok()??;
    let access = access_bits(access);
    if bit(insn, 21) {
        let option = field(insn, 13, 3);
        if mode != 2 || option & 2 == 0 {
            return None;
        }
        let shift = if bit(insn, 12) { scale } else { 0 };
        let flags = scale | access << 2 | option << 4 | shift << 7;
        return Some(op(
            insn,
            Kind::LdStRegOff,
            rt,
            rn,
            field(insn, 16, 5),
            flags as u64,
        ));
    }
    if mode == 2 {
        return None;
    }
    let imm = sext(field(insn, 12, 9) as u64, 9);
    let flags = scale | access << 2 | mode << 4;
    Some(op2(insn, Kind::LdStImm, rt, rn, 0, imm, flags as u64))
}

fn dp_reg(insn: u32) -> Option<Op> {
    let sf = bit(insn, 31);
    let rd = field(insn, 0, 5);
    let rn = field(insn, 5, 5);
    let rm = field(insn, 16, 5);
    let imm6 = field(insn, 10, 6);
    let shift = field(insn, 22, 2);
    if insn & 0x1F00_0000 == 0x0A00_0000 {
        if !sf && imm6 >= 32 {
            return None;
        }
        let flags = shift | imm6 << 2 | (bit(insn, 21) as u32) << 8;
        let flags = flags | field(insn, 29, 2) << 9 | (sf as u32) << 11;
        // ORR from XZR, unshifted: MOV (register).
        if field(insn, 29, 2) == 1 && rn == 31 && imm6 == 0 && !bit(insn, 21) && rd != 31 {
            return Some(op(insn, Kind::MovReg, rd, rm, sf as u32, 0));
        }
        Some(op(insn, Kind::LogicReg, rd, rn, rm, flags as u64))
    } else if insn & 0x1F20_0000 == 0x0B00_0000 {
        if shift == 3 || (!sf && imm6 >= 32) {
            return None;
        }
        let flags = shift | imm6 << 2 | (bit(insn, 30) as u32) << 8;
        let flags = flags | (bit(insn, 29) as u32) << 9 | (sf as u32) << 11;
        // SUBS to XZR, unshifted: CMP (register).
        if bit(insn, 30) && bit(insn, 29) && rd == 31 && imm6 == 0 {
            let kind = if sf { Kind::CmpReg } else { Kind::CmpRegW };
            return Some(op(insn, kind, rn, rm, sf as u32, 0));
        }
        Some(op(insn, Kind::AddSubReg, rd, rn, rm, flags as u64))
    } else if insn & 0x1FE0_0000 == 0x1A40_0000 {
        if !bit(insn, 29) || bit(insn, 10) || bit(insn, 4) {
            return None;
        }
        let flags = field(insn, 12, 4)
            | (sf as u32) << 4
            | (bit(insn, 30) as u32) << 5
            | (bit(insn, 11) as u32) << 6;
        Some(op(
            insn,
            Kind::Ccmp,
            rn,
            rm,
            flags,
            u64::from(field(insn, 0, 4) << 28),
        ))
    } else if insn & 0x1FE0_0000 == 0x1A80_0000 {
        let op2 = field(insn, 10, 2);
        if bit(insn, 29) || op2 > 1 {
            return None;
        }
        let flags = field(insn, 12, 4) | (bit(insn, 30) as u32) << 4 | op2 << 5 | (sf as u32) << 6;
        Some(op(insn, Kind::Csel, rd, rn, rm, flags as u64))
    } else if insn & 0x1F00_0000 == 0x1B00_0000 {
        if field(insn, 29, 2) != 0 || field(insn, 21, 3) != 0 {
            return None;
        }
        let flags = field(insn, 10, 5) | (bit(insn, 15) as u32) << 5 | (sf as u32) << 6;
        Some(op(insn, Kind::Madd, rd, rn, rm, flags as u64))
    } else {
        None
    }
}

impl Cpu {
    /// [`exec::transfer`] for a general register, with the access itself out
    /// of line: inlined into every handler that loads or stores, it would
    /// crowd the registers of the loop the handlers are inlined into.
    #[inline(always)]
    fn gpr<M: Memory + ?Sized>(
        &mut self,
        mem: &mut M,
        scale: u32,
        access: Access,
        rt: u32,
        addr: u64,
    ) -> Result<Option<(u64, bool)>, exec::Stop> {
        let bytes = 1u32 << scale;
        if let Access::Store = access {
            let v = self.xr(rt, true);
            if !self.host_store(mem, addr, bytes, v) && !self.store_out(mem, addr, bytes, v) {
                return Err(self.take_fault());
            }
            return Ok(None);
        }
        let v = match self.host_load(mem, addr, bytes) {
            Some(v) => v,
            None => match self.load_out(mem, addr, bytes) {
                Some(v) => v,
                None => return Err(self.take_fault()),
            },
        };
        Ok(Some(match access {
            Access::LoadSigned64 => (sext(v, 8 * bytes), true),
            Access::LoadSigned32 => (sext(v, 8 * bytes), false),
            _ => (v, true),
        }))
    }

    #[inline(never)]
    fn load_out<M: Memory + ?Sized>(&mut self, mem: &mut M, va: u64, size: u32) -> Option<u64> {
        match self.read(mem, va, size) {
            Ok(v) => Some(v),
            Err(s) => {
                self.fault = Some(s);
                None
            }
        }
    }

    #[inline(never)]
    fn store_out<M: Memory + ?Sized>(&mut self, mem: &mut M, va: u64, size: u32, v: u64) -> bool {
        match self.write(mem, va, size, v) {
            Ok(()) => true,
            Err(s) => {
                self.fault = Some(s);
                false
            }
        }
    }

    #[cold]
    fn take_fault(&mut self) -> exec::Stop {
        self.fault.take().unwrap_or(exec::UNDEF)
    }

    /// [`exec::execute`] for a decoded instruction.
    #[inline(always)]
    pub(super) fn run_op<M: Memory + ?Sized>(&mut self, o: Op, mem: &mut M) -> Exec {
        let pc = self.pc;
        self.next_pc = o.handler::<M>()(self, &o, mem, pc);
        match self.fault.take() {
            Some(s) => Err(s),
            None => Ok(()),
        }
    }

    /// After a handler that stopped: what [`Self::step_decoded`] makes of
    /// it, with an exception taken. `pc` is the instruction's, `next` what
    /// the handler returned.
    #[cold]
    #[inline(never)]
    pub fn stopped_at(&mut self, pc: u64, next: u64, op: &Op) -> super::Step {
        let stop = self.fault.take().unwrap_or(exec::UNDEF);
        self.pc = pc;
        self.next_pc = next;
        match self.retire(Err(stop), op.insn) {
            super::Step::Exception(e) => {
                self.take_sync(e);
                super::Step::Took(e)
            }
            s => s,
        }
    }
}

/// What runs a decoded instruction: given the PC it is at, it returns the
/// next. One that stops leaves why in [`Cpu::fault`] and returns the PC a
/// `wfi` or `wfe` would retire to.
pub type Handler<M> = fn(&mut Cpu, &Op, &mut M, u64) -> u64;

/// A bus for the handlers of [`Op::pure`] instructions, which touch none.
pub struct NoMem;

impl Memory for NoMem {
    fn read(&mut self, addr: u64, _: u32) -> Result<u64, super::cpu::Abort> {
        Err(super::cpu::Abort { addr, write: false })
    }
    fn write(&mut self, addr: u64, _: u32, _: u64) -> Result<(), super::cpu::Abort> {
        Err(super::cpu::Abort { addr, write: true })
    }
}

impl Op {
    /// The handler for this instruction on bus `M`.
    #[inline(always)]
    pub fn handler<M: Memory + ?Sized>(&self) -> Handler<M> {
        handler_of(self.kind)
    }

    /// What runs this instruction inside a straight run on bus `M`.
    fn go<M: RunBus>(&self) -> Go<M> {
        macro_rules! go {
            ($($k:ident),*) => {
                match self.kind {
                    $(Kind::$k => go_kind::<M, { Kind::$k as u8 }>,)*
                }
            };
        }
        go!(
            Execute, BranchReg, B, Bl, BCond, Cbz, Tbz, LdrX, LdrW, LdrH, LdrB, StrX, StrW, StrH,
            StrB, Barrier, LdStReg, LdStPair, LoadUimm, StoreUimm, LdStRegOff, LdStImm, PairV,
            Pair, DpImm, DpReg, Adr, Adrp, SimdDp, Mrs, Nop, CmpImm, CmpImmW, CmpReg, CmpRegW,
            AddImmR, SubImmR, MovReg, AddImm, SubImm, AddsImm, SubsImm, MovConst, MovK, LogicImm,
            LogicReg, AddSubReg, Bitfield, Ccmp, Csel, Madd
        )
    }
}

/// The handler for instructions of `kind` on bus `M`.
#[inline(always)]
pub(super) fn handler_of<M: Memory + ?Sized>(kind: Kind) -> Handler<M> {
    match kind {
        Kind::Execute => h_execute::<M>,
        Kind::BranchReg => h_branch_reg::<M>,
        Kind::B => h_b::<M>,
        Kind::Bl => h_bl::<M>,
        Kind::BCond => h_bcond::<M>,
        Kind::Cbz => h_cbz::<M>,
        Kind::Tbz => h_tbz::<M>,
        Kind::LdrX => h_ldr::<M, 8>,
        Kind::LdrW => h_ldr::<M, 4>,
        Kind::LdrH => h_ldr::<M, 2>,
        Kind::LdrB => h_ldr::<M, 1>,
        Kind::StrX => h_str::<M, 8>,
        Kind::StrW => h_str::<M, 4>,
        Kind::StrH => h_str::<M, 2>,
        Kind::StrB => h_str::<M, 1>,
        Kind::Barrier => h_barrier::<M>,
        Kind::Nop => h_nop::<M>,
        Kind::CmpImm => h_cmp_imm::<M, true>,
        Kind::CmpImmW => h_cmp_imm::<M, false>,
        Kind::CmpReg => h_cmp_reg::<M, true>,
        Kind::CmpRegW => h_cmp_reg::<M, false>,
        Kind::AddImmR => h_add_imm_r::<M>,
        Kind::SubImmR => h_sub_imm_r::<M>,
        Kind::MovReg => h_mov_reg::<M>,
        Kind::LdStReg => h_ld_st_reg::<M>,
        Kind::LdStPair => h_ld_st_pair::<M>,
        Kind::LoadUimm => h_load_uimm::<M>,
        Kind::StoreUimm => h_store_uimm::<M>,
        Kind::LdStRegOff => h_ld_st_reg_off::<M>,
        Kind::LdStImm => h_ld_st_imm::<M>,
        Kind::PairV => h_pair_v::<M>,
        Kind::Pair => h_pair::<M>,
        Kind::DpImm => h_dp_imm::<M>,
        Kind::DpReg => h_dp_reg::<M>,
        Kind::Adr => h_adr::<M>,
        Kind::Adrp => h_adrp::<M>,
        Kind::SimdDp => h_simd::<M>,
        Kind::Mrs => h_mrs::<M>,
        Kind::AddImm => h_add_imm::<M>,
        Kind::SubImm => h_sub_imm::<M>,
        Kind::AddsImm => h_adds_imm::<M>,
        Kind::SubsImm => h_subs_imm::<M>,
        Kind::MovConst => h_mov_const::<M>,
        Kind::MovK => h_movk::<M>,
        Kind::LogicImm => h_logic_imm::<M>,
        Kind::LogicReg => h_logic_reg::<M>,
        Kind::AddSubReg => h_add_sub_reg::<M>,
        Kind::Bitfield => h_bitfield::<M>,
        Kind::Ccmp => h_ccmp::<M>,
        Kind::Csel => h_csel::<M>,
        Kind::Madd => h_madd::<M>,
    }
}

/// An instruction of a straight run, with what runs it there: its handler,
/// and then the next instruction's, for as long as each leaves nothing for
/// the run loop to look at. A branch out of the run into its own page can be
/// linked to the run at its target, and goes on into that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOp {
    pub op: Op,
    go: usize,
    link: Cell<usize>,
    link_gen: Cell<u64>,
    /// The run from here compiled (`src/aarch64/jit.rs`), and how often it
    /// was entered before it was.
    #[cfg(pimu_compiled)]
    pub(super) jit: Cell<usize>,
    #[cfg(pimu_compiled)]
    pub(super) hits: Cell<u32>,
}

/// What a straight run needs of the bus it runs on.
pub trait RunBus: Memory {
    /// Whether [`Self::indirect`] may answer, and its run loop take a
    /// register branch that ran as the end of the run's chain.
    const INDIRECT: bool = false;
    /// The load or store at hand happens at cycle `c`.
    fn at(&mut self, c: u64);
    /// Whether the loads and stores so far left nothing for the run loop to
    /// look at.
    fn plain(&self) -> bool;
    /// The run stopped at `op`, which would go on to `next`: after running,
    /// or before when it is the last of its run, which the run loop takes
    /// itself.
    fn stop(&mut self, op: &RunOp, next: u64, ran: bool);
    /// Whether a branch `c` cycles in may go on into the run it is linked
    /// to, which was decoded at its page's generation `gen`.
    fn chain(&self, c: u64, gen: u64) -> bool;
    /// After a load or store a compiled run made: whether the run may go on
    /// past it, as `Straight::run` would, forgetting the store if so. `gen`
    /// is the run's page's write generation when it began, `genp` where that
    /// page's generation is, and `marked` that another core holds an
    /// exclusive mark.
    #[cfg(pimu_compiled)]
    fn settle(&mut self, gen: u64, genp: *const u64, marked: bool) -> bool;
    /// Whether a branch into a run with code of its own ([`RunOp::jit`])
    /// goes there, through [`Self::compiled`].
    #[cfg(pimu_compiled)]
    const COMPILED: bool = false;
    /// Run `run`'s compiled code at `pc`, `c` cycles in: what [`RunOp::go`]
    /// would return.
    /// The table index of the compiled run at `target`, `c` cycles in, for
    /// a compiled run that branched there to go on into, with `ctx` made
    /// that run's; 0 to stop instead.
    #[cfg(pimu_compiled)]
    fn chain_compiled(
        &mut self,
        _cpu: &Cpu,
        _ctx: &mut super::compiled::Ctx,
        _target: u64,
        _c: u64,
    ) -> usize {
        0
    }
    #[cfg(pimu_compiled)]
    fn compiled(&mut self, cpu: &mut Cpu, run: &RunOp, pc: u64, c: u64) -> (u64, u64)
    where
        Self: Sized,
    {
        run.go(cpu, self, pc, c)
    }
    /// The run at `next`, where a `BR`, `BLR` or `RET` `c` cycles in went,
    /// if the bus knows it and lets the branch go on into it.
    fn indirect(&mut self, _cpu: &Cpu, _next: u64, _c: u64) -> Option<*const RunOp> {
        None
    }
}

type Go<M> = fn(&mut Cpu, &RunOp, &mut M, u64, u64) -> (u64, u64);

/// Go on into `run`'s instruction at `pc`, `c` cycles in: as a tail call
/// where the compiler is told to make one (`pimu_tail`), since a wasm engine
/// turns a plain call there into a frame per instruction of the chain.
#[cfg(pimu_tail)]
macro_rules! go_on {
    ($run:expr, $cpu:expr, $bus:expr, $pc:expr, $c:expr) => {{
        let run: &RunOp = $run;
        // A `Go` of the bus's, which `RunOp::new` made.
        let go: Go<_> = unsafe { std::mem::transmute(run.go) };
        become go($cpu, run, $bus, $pc, $c)
    }};
}

/// [`go_on!`] into the first instruction of `run`, or into the run's
/// compiled code where the bus has some for it ([`RunBus::compiled`]).
macro_rules! enter {
    ($run:expr, $cpu:expr, $bus:expr, $pc:expr, $c:expr) => {{
        let run: &RunOp = $run;
        #[cfg(pimu_compiled)]
        if M::COMPILED && run.jit.get() != 0 {
            return $bus.compiled($cpu, run, $pc, $c);
        }
        go_on!(run, $cpu, $bus, $pc, $c);
    }};
}

#[cfg(not(pimu_tail))]
macro_rules! go_on {
    ($run:expr, $cpu:expr, $bus:expr, $pc:expr, $c:expr) => {
        return $run.go($cpu, $bus, $pc, $c)
    };
}

impl RunOp {
    /// `op` in a run on bus `M`; `last` when it is the run's last.
    pub fn new<M: RunBus>(op: Op, last: bool) -> RunOp {
        let go: Go<M> = if last {
            op.go_last::<M>()
        } else {
            op.go::<M>()
        };
        RunOp {
            op,
            go: go as usize,
            link: Cell::new(0),
            link_gen: Cell::new(0),
            #[cfg(pimu_compiled)]
            jit: Cell::new(0),
            #[cfg(pimu_compiled)]
            hits: Cell::new(0),
        }
    }

    /// Run this instruction at `pc`, `c` cycles in, and those after it while
    /// they leave nothing to look at: the PC and the cycle of the one that
    /// stopped, which it told `bus`. `M` is the bus [`Self::new`] was given,
    /// but for its lifetimes.
    #[inline(always)]
    pub fn go<M: RunBus>(&self, cpu: &mut Cpu, bus: &mut M, pc: u64, c: u64) -> (u64, u64) {
        // A `Go<M>`, which `new` made.
        let go: Go<M> = unsafe { std::mem::transmute(self.go) };
        go(cpu, self, bus, pc, c)
    }

    /// How often the run from here was entered before it was compiled.
    #[cfg(pimu_compiled)]
    pub fn hits(&self) -> u32 {
        self.hits.get()
    }

    /// The table index of the code compiled for the run from here, 0 for
    /// none.
    #[cfg(pimu_compiled)]
    pub fn compiled_code(&self) -> usize {
        self.jit.get()
    }

    /// Link this branch to `run`, the run at its target, decoded at its
    /// page's generation `gen`. `run` has to stay put as long as this does.
    pub fn link(&self, run: &RunOp, gen: u64) {
        self.link.set(run as *const RunOp as usize);
        self.link_gen.set(gen);
    }

    /// The first instruction of the run this branch was linked to, while its
    /// page is at generation `gen`.
    #[inline(always)]
    pub fn linked(&self, gen: u64) -> Option<*const RunOp> {
        let link = self.link.get();
        (link != 0 && self.link_gen.get() == gen).then_some(link as *const RunOp)
    }
}

impl Op {
    /// What runs this instruction as the last of its run on bus `M`: a
    /// branch goes on into a linked run, anything else is left to the run
    /// loop.
    fn go_last<M: RunBus>(&self) -> Go<M> {
        match self.kind {
            Kind::B => go_branch::<M, { Kind::B as u8 }>,
            Kind::Bl => go_branch::<M, { Kind::Bl as u8 }>,
            Kind::BCond => go_branch::<M, { Kind::BCond as u8 }>,
            Kind::Cbz => go_branch::<M, { Kind::Cbz as u8 }>,
            Kind::Tbz => go_branch::<M, { Kind::Tbz as u8 }>,
            Kind::BranchReg if M::INDIRECT && self.indirect_branch() => go_indirect::<M>,
            _ => go_stop::<M>,
        }
    }
}

fn go_kind<M: RunBus, const K: u8>(
    cpu: &mut Cpu,
    o: &RunOp,
    bus: &mut M,
    pc: u64,
    c: u64,
) -> (u64, u64) {
    // `K` is one of `Kind`'s, from `Op::go`.
    let kind = unsafe { std::mem::transmute::<u8, Kind>(K) };
    let mem = (Kind::LdrX as u8..=Kind::Pair as u8).contains(&K);
    if mem {
        bus.at(c);
    }
    let next = handler_of::<M>(kind)(cpu, &o.op, bus, pc);
    let on = if K >= Kind::Nop as u8 {
        true
    } else if (Kind::BCond as u8..=Kind::Tbz as u8).contains(&K) {
        if next != pc.wrapping_add(4) {
            if let Some(run) = linked(o, bus, c) {
                enter!(run, cpu, bus, next, c + 1);
            }
            bus.stop(o, next, true);
            return (pc, c);
        }
        true
    } else if K <= Kind::Bl as u8 {
        false
    } else if mem {
        !cpu.faulted() && bus.plain()
    } else {
        !cpu.faulted()
    };
    if on {
        // Not the last of its run, which `go_last` runs.
        let n = unsafe { &*(o as *const RunOp).add(1) };
        go_on!(n, cpu, bus, next, c + 1);
    }
    bus.stop(o, next, true);
    (pc, c)
}

fn go_branch<M: RunBus, const K: u8>(
    cpu: &mut Cpu,
    o: &RunOp,
    bus: &mut M,
    pc: u64,
    c: u64,
) -> (u64, u64) {
    // `K` is one of `Kind`'s, from `Op::go_last`.
    let kind = unsafe { std::mem::transmute::<u8, Kind>(K) };
    let next = handler_of::<M>(kind)(cpu, &o.op, bus, pc);
    if next == pc.wrapping_add(4) {
        bus.stop(o, next, true);
        return (pc, c);
    }
    if let Some(run) = linked(o, bus, c) {
        enter!(run, cpu, bus, next, c + 1);
    }
    bus.stop(o, next, true);
    (pc, c)
}

/// The run the branch `o`, taken `c` cycles in, is linked to, where `bus`
/// lets it go on into it.
#[inline(always)]
fn linked<'a, M: RunBus>(o: &RunOp, bus: &M, c: u64) -> Option<&'a RunOp> {
    let link = o.link.get();
    // A run on this page from its current generation, which stays put.
    (link != 0 && bus.chain(c, o.link_gen.get())).then(|| unsafe { &*(link as *const RunOp) })
}

/// `BR`, `BLR` or `RET`: on into the run at its target where `bus` knows it.
fn go_indirect<M: RunBus>(cpu: &mut Cpu, o: &RunOp, bus: &mut M, pc: u64, c: u64) -> (u64, u64) {
    let next = h_branch_reg::<M>(cpu, &o.op, bus, pc);
    if !cpu.faulted() {
        if let Some(run) = bus.indirect(cpu, next, c) {
            // A run of the bus's, which stays put while the bus vouches for it.
            let run = unsafe { &*run };
            enter!(run, cpu, bus, next, c + 1);
        }
    }
    bus.stop(o, next, true);
    (pc, c)
}

fn go_stop<M: RunBus>(_: &mut Cpu, o: &RunOp, bus: &mut M, pc: u64, c: u64) -> (u64, u64) {
    bus.stop(o, pc, false);
    (pc, c)
}

/// The next PC after `exec`'s `r`, which ran with `next_pc` at PC + 4.
#[inline(always)]
fn settled(cpu: &mut Cpu, r: Exec) -> u64 {
    if let Err(s) = r {
        cpu.fault = Some(s);
    }
    cpu.next_pc
}

/// A load or store that faulted.
#[cold]
fn faulted(cpu: &mut Cpu, s: exec::Stop, pc: u64) -> u64 {
    cpu.fault = Some(s);
    pc.wrapping_add(4)
}

fn h_execute<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    cpu.pc = pc;
    cpu.next_pc = pc.wrapping_add(4);
    let r = exec::execute(cpu, o.insn, mem);
    settled(cpu, r)
}

fn h_simd<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    cpu.pc = pc;
    cpu.next_pc = pc.wrapping_add(4);
    let r = super::simd::execute(cpu, o.insn);
    settled(cpu, r)
}

fn h_mrs<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    match super::sysreg::read(cpu, o.imm as u32, mem) {
        Ok(v) => {
            cpu.set_xr(o.a as u32, true, v);
            pc.wrapping_add(4)
        }
        Err(s) => faulted(cpu, s, pc),
    }
}

fn h_branch_reg<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    cpu.pc = pc;
    cpu.next_pc = pc.wrapping_add(4);
    let r = exec::branch_reg(cpu, o.insn);
    settled(cpu, r)
}

fn h_barrier<M: Memory + ?Sized>(_: &mut Cpu, _: &Op, mem: &mut M, pc: u64) -> u64 {
    mem.barrier();
    pc.wrapping_add(4)
}

fn h_nop<M: Memory + ?Sized>(_: &mut Cpu, _: &Op, _: &mut M, pc: u64) -> u64 {
    pc.wrapping_add(4)
}

fn h_dp_imm<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    match exec::dp_imm(cpu, o.insn) {
        Ok(()) => pc.wrapping_add(4),
        Err(s) => faulted(cpu, s, pc),
    }
}

fn h_dp_reg<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    match exec::dp_reg(cpu, o.insn) {
        Ok(()) => pc.wrapping_add(4),
        Err(s) => faulted(cpu, s, pc),
    }
}

fn h_ld_st_reg<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    match exec::ld_st_reg(cpu, o.insn, mem) {
        Ok(()) => pc.wrapping_add(4),
        Err(s) => faulted(cpu, s, pc),
    }
}

fn h_ld_st_pair<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    match exec::ld_st_pair(cpu, o.insn, mem) {
        Ok(()) => pc.wrapping_add(4),
        Err(s) => faulted(cpu, s, pc),
    }
}

fn h_adr<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    cpu.set_xr(o.a as u32, true, pc.wrapping_add(o.imm));
    pc.wrapping_add(4)
}

fn h_adrp<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    cpu.set_xr(o.a as u32, true, (pc & !0xFFF).wrapping_add(o.imm));
    pc.wrapping_add(4)
}

fn h_b<M: Memory + ?Sized>(_: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    pc.wrapping_add(o.imm)
}

fn h_bl<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    cpu.x[30] = pc.wrapping_add(4);
    pc.wrapping_add(o.imm)
}

/// `pc` plus the offset when `taken`, else the next instruction.
#[inline(always)]
fn branch_if(taken: bool, o: &Op, pc: u64) -> u64 {
    pc.wrapping_add(if taken { o.imm } else { 4 })
}

fn h_bcond<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    branch_if(cpu.cond_holds(o.a as u32), o, pc)
}

fn h_cbz<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let zero = cpu.xr(o.a as u32, o.c & 1 != 0) == 0;
    branch_if(zero != (o.c & 2 != 0), o, pc)
}

fn h_tbz<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let set = (cpu.xr(o.a as u32, true) >> o.b) & 1 != 0;
    branch_if(set == (o.c != 0), o, pc)
}

/// `LDR` of `N` bytes, unsigned offset from a general register: straight
/// from the host page when the data TLB has it, else [`h_load_uimm`].
fn h_ldr<M: Memory + ?Sized, const N: u32>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    let addr = cpu.reg(o.b as u32).wrapping_add(o.imm);
    match cpu.host_load(mem, addr, N) {
        Some(v) => {
            cpu.set_xr(o.a as u32, true, v);
            pc.wrapping_add(4)
        }
        None => h_load_uimm(cpu, o, mem, pc),
    }
}

/// [`h_ldr`]'s store.
fn h_str<M: Memory + ?Sized, const N: u32>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    let addr = cpu.reg(o.b as u32).wrapping_add(o.imm);
    if cpu.host_store(mem, addr, N, cpu.xr(o.a as u32, true)) {
        return pc.wrapping_add(4);
    }
    h_store_uimm(cpu, o, mem, pc)
}

/// [`Cpu::gpr`] when the data TLB has the page: `None` when the access needs
/// the full path.
#[inline(always)]
fn fast_gpr<M: Memory + ?Sized>(
    cpu: &mut Cpu,
    mem: &mut M,
    scale: u32,
    access: Access,
    rt: u32,
    addr: u64,
) -> Option<Option<(u64, bool)>> {
    let bytes = 1u32 << scale;
    match access {
        Access::Store => cpu
            .host_store(mem, addr, bytes, cpu.xr(rt, true))
            .then_some(None),
        Access::Load => cpu.host_load(mem, addr, bytes).map(|v| Some((v, true))),
        Access::LoadSigned64 => cpu
            .host_load(mem, addr, bytes)
            .map(|v| Some((sext(v, 8 * bytes), true))),
        Access::LoadSigned32 => cpu
            .host_load(mem, addr, bytes)
            .map(|v| Some((sext(v, 8 * bytes), false))),
    }
}

fn h_load_uimm<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    let (a, b, c) = (o.a as u32, o.b as u32, o.c as u32);
    let addr = cpu.xsp(b).wrapping_add(o.imm);
    let access = match c >> 2 {
        0 => Access::Load,
        1 => Access::LoadSigned64,
        _ => Access::LoadSigned32,
    };
    match cpu.gpr(mem, c & 3, access, a, addr) {
        Ok(Some((v, sf))) => cpu.set_xr(a, sf, v),
        Ok(None) => {}
        Err(s) => return faulted(cpu, s, pc),
    }
    pc.wrapping_add(4)
}

fn h_store_uimm<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    let addr = cpu.xsp(o.b as u32).wrapping_add(o.imm);
    if let Err(s) = cpu.gpr(mem, o.c as u32, Access::Store, o.a as u32, addr) {
        return faulted(cpu, s, pc);
    }
    pc.wrapping_add(4)
}

fn h_ld_st_reg_off<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    let (a, b, c) = (o.a as u32, o.b as u32, o.c as u32);
    let f = o.imm as u32;
    let off = extend_reg(cpu.xr(c, true), (f >> 4) & 7, f >> 7, true);
    let addr = cpu.xsp(b).wrapping_add(off);
    match fast_gpr(cpu, mem, f & 3, access_of(f >> 2), a, addr) {
        Some(Some((v, sf))) => cpu.set_xr(a, sf, v),
        Some(None) => {}
        None => return h_ld_st_reg_off_slow(cpu, o, mem, pc),
    }
    pc.wrapping_add(4)
}

#[inline(never)]
fn h_ld_st_reg_off_slow<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    let (a, b, c) = (o.a as u32, o.b as u32, o.c as u32);
    let f = o.imm as u32;
    let off = extend_reg(cpu.xr(c, true), (f >> 4) & 7, f >> 7, true);
    let addr = cpu.xsp(b).wrapping_add(off);
    match cpu.gpr(mem, f & 3, access_of(f >> 2), a, addr) {
        Ok(Some((v, sf))) => cpu.set_xr(a, sf, v),
        Ok(None) => {}
        Err(s) => return faulted(cpu, s, pc),
    }
    pc.wrapping_add(4)
}

/// The address and any write-back of an index mode: 0 offset, 1
/// post-index, 3 pre-index (and 2, a pair's non-temporal offset).
#[inline(always)]
fn indexed(base: u64, imm: u64, mode: u32) -> (u64, Option<u64>) {
    match mode {
        0 | 2 => (base.wrapping_add(imm), None),
        1 => (base, Some(base.wrapping_add(imm))),
        _ => {
            let a = base.wrapping_add(imm);
            (a, Some(a))
        }
    }
}

fn h_ld_st_imm<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    let (a, b) = (o.a as u32, o.b as u32);
    let f = o.imm2 as u32;
    let (addr, wb) = indexed(cpu.xsp(b), o.imm, (f >> 4) & 3);
    let Some(loaded) = fast_gpr(cpu, mem, f & 3, access_of(f >> 2), a, addr) else {
        return h_ld_st_imm_slow(cpu, o, mem, pc);
    };
    if let Some(wb) = wb {
        cpu.set_xsp(b, true, wb);
    }
    if let Some((v, sf)) = loaded {
        cpu.set_xr(a, sf, v);
    }
    pc.wrapping_add(4)
}

#[inline(never)]
fn h_ld_st_imm_slow<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    let (a, b) = (o.a as u32, o.b as u32);
    let f = o.imm2 as u32;
    let (addr, wb) = indexed(cpu.xsp(b), o.imm, (f >> 4) & 3);
    let loaded = match cpu.gpr(mem, f & 3, access_of(f >> 2), a, addr) {
        Ok(l) => l,
        Err(s) => return faulted(cpu, s, pc),
    };
    if let Some(wb) = wb {
        cpu.set_xsp(b, true, wb);
    }
    if let Some((v, sf)) = loaded {
        cpu.set_xr(a, sf, v);
    }
    pc.wrapping_add(4)
}

fn h_pair<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    let (a, b, c) = (o.a as u32, o.b as u32, o.c as u32);
    let f = o.imm2 as u32;
    let (scale, access) = (f & 3, access_of(f >> 2));
    let bytes = 1u32 << scale;
    let (addr, wb) = indexed(cpu.xsp(b), o.imm, (f >> 4) & 3);
    let store = matches!(access, Access::Store);
    let (va, vc) = (cpu.xr(a, true), cpu.xr(c, true));
    // Both halves at once, straight through the data TLB, or neither.
    let Some(p) = cpu.host_span(mem, addr, 2 * bytes, store) else {
        return h_pair_slow(cpu, o, mem, pc);
    };
    let q = p.wrapping_add(bytes as usize);
    if store {
        // Inside the page, which `host_span` vouched for.
        unsafe {
            store_le(p, bytes, va);
            store_le(q, bytes, vc);
        }
        if let Some(wb) = wb {
            cpu.set_xsp(b, true, wb);
        }
        return pc.wrapping_add(4);
    }
    let (first, second) = unsafe { (load_le(p, bytes), load_le(q, bytes)) };
    let (first, second, sf) = match access {
        Access::LoadSigned64 => (sext(first, 8 * bytes), sext(second, 8 * bytes), true),
        _ => (first, second, true),
    };
    if let Some(wb) = wb {
        cpu.set_xsp(b, true, wb);
    }
    cpu.set_xr(a, sf, first);
    cpu.set_xr(c, sf, second);
    pc.wrapping_add(4)
}

/// `LDP`/`STP` of SIMD and FP registers: both halves straight through the
/// data TLB, or the whole instruction through `exec`.
fn h_pair_v<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    let (a, b, c) = (o.a as usize, o.b as u32, o.c as usize);
    let f = o.imm2 as u32;
    let bytes = 1usize << (f & 7);
    let store = f & 8 != 0;
    let (addr, wb) = indexed(cpu.xsp(b), o.imm, (f >> 4) & 3);
    let Some(p) = cpu.host_span(mem, addr, 2 * bytes as u32, store) else {
        return h_execute(cpu, o, mem, pc);
    };
    // Inside the page, which `host_span` vouched for; the vector registers
    // are little-endian as the guest's memory is.
    unsafe {
        if store {
            std::ptr::copy_nonoverlapping(cpu.v[a].to_le_bytes().as_ptr(), p, bytes);
            std::ptr::copy_nonoverlapping(cpu.v[c].to_le_bytes().as_ptr(), p.add(bytes), bytes);
        } else {
            let (mut x, mut y) = ([0u8; 16], [0u8; 16]);
            std::ptr::copy_nonoverlapping(p, x.as_mut_ptr(), bytes);
            std::ptr::copy_nonoverlapping(p.add(bytes), y.as_mut_ptr(), bytes);
            cpu.v[a] = u128::from_le_bytes(x);
            cpu.v[c] = u128::from_le_bytes(y);
        }
    }
    if let Some(wb) = wb {
        cpu.set_xsp(b, true, wb);
    }
    pc.wrapping_add(4)
}

#[inline(never)]
fn h_pair_slow<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, mem: &mut M, pc: u64) -> u64 {
    let (a, b, c) = (o.a as u32, o.b as u32, o.c as u32);
    let f = o.imm2 as u32;
    let (scale, access) = (f & 3, access_of(f >> 2));
    let (addr, wb) = indexed(cpu.xsp(b), o.imm, (f >> 4) & 3);
    let first = match cpu.gpr(mem, scale, access, a, addr) {
        Ok(l) => l,
        Err(s) => return faulted(cpu, s, pc),
    };
    let second = match cpu.gpr(mem, scale, access, c, addr.wrapping_add(1 << scale)) {
        Ok(l) => l,
        Err(s) => return faulted(cpu, s, pc),
    };
    if let Some(wb) = wb {
        cpu.set_xsp(b, true, wb);
    }
    if let Some((v, sf)) = first {
        cpu.set_xr(a, sf, v);
    }
    if let Some((v, sf)) = second {
        cpu.set_xr(c, sf, v);
    }
    pc.wrapping_add(4)
}

/// `CMP Xn|Wn, #imm`: the flags of the subtraction, register `n` not SP.
fn h_cmp_imm<M: Memory + ?Sized, const SF: bool>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    cpu.nzcv = add_with_carry(mask(cpu.reg(o.b as u32), SF), !o.imm, true, SF).1;
    pc.wrapping_add(4)
}

/// `CMP Xn|Wn, Xm|Wm` without a shift; `n` and `m` may be XZR.
fn h_cmp_reg<M: Memory + ?Sized, const SF: bool>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let (x, y) = (cpu.xr(o.a as u32, SF), cpu.xr(o.b as u32, SF));
    cpu.nzcv = add_with_carry(x, mask(!y, SF), true, SF).1;
    pc.wrapping_add(4)
}

/// `ADD` (immediate), neither register SP.
fn h_add_imm_r<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let r = mask(cpu.reg(o.b as u32).wrapping_add(o.imm), o.c != 0);
    cpu.set_reg(o.a as u32, r);
    pc.wrapping_add(4)
}

/// `SUB` (immediate), neither register SP.
fn h_sub_imm_r<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let r = mask(cpu.reg(o.b as u32).wrapping_sub(o.imm), o.c != 0);
    cpu.set_reg(o.a as u32, r);
    pc.wrapping_add(4)
}

/// `MOV Xd|Wd, Xm|Wm`, `d` not XZR.
fn h_mov_reg<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let v = cpu.xr(o.b as u32, o.c != 0);
    cpu.set_reg(o.a as u32, v);
    pc.wrapping_add(4)
}

fn h_add_imm<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let sf = o.c != 0;
    let r = mask(cpu.xsp(o.b as u32), sf).wrapping_add(o.imm);
    cpu.set_xsp(o.a as u32, sf, r);
    pc.wrapping_add(4)
}

fn h_sub_imm<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let sf = o.c != 0;
    let r = mask(cpu.xsp(o.b as u32), sf).wrapping_sub(o.imm);
    cpu.set_xsp(o.a as u32, sf, r);
    pc.wrapping_add(4)
}

/// `ADDS`/`SUBS` (immediate), `CMP`/`CMN`.
#[inline(always)]
fn adds_imm(cpu: &mut Cpu, o: &Op, sub: bool) {
    let sf = o.c != 0;
    let x = mask(cpu.xsp(o.b as u32), sf);
    let y = if sub { !o.imm } else { o.imm };
    let (r, nzcv) = add_with_carry(x, y, sub, sf);
    cpu.nzcv = nzcv;
    cpu.set_xr(o.a as u32, sf, r);
}

fn h_adds_imm<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    adds_imm(cpu, o, false);
    pc.wrapping_add(4)
}

fn h_subs_imm<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    adds_imm(cpu, o, true);
    pc.wrapping_add(4)
}

fn h_mov_const<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    cpu.set_xr(o.a as u32, true, o.imm);
    pc.wrapping_add(4)
}

fn h_movk<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let (a, sf) = (o.a as u32, o.c != 0);
    let r = (cpu.xr(a, sf) & !(0xFFFFu64 << o.b)) | o.imm;
    cpu.set_xr(a, sf, r);
    pc.wrapping_add(4)
}

fn h_logic_imm<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let (a, c) = (o.a as u32, o.c as u32);
    let sf = c & 4 != 0;
    let x = cpu.xr(o.b as u32, sf);
    let r = match c & 3 {
        0 | 3 => x & o.imm,
        1 => x | o.imm,
        _ => x ^ o.imm,
    };
    if c & 3 == 3 {
        cpu.nzcv = logic_flags(r, sf);
        cpu.set_xr(a, sf, r);
    } else {
        cpu.set_xsp(a, sf, r);
    }
    pc.wrapping_add(4)
}

fn h_logic_reg<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let f = o.imm as u32;
    let sf = f & (1 << 11) != 0;
    let mut y = shift_reg(cpu.xr(o.c as u32, sf), f & 3, (f >> 2) & 0x3F, sf);
    if f & (1 << 8) != 0 {
        y = mask(!y, sf);
    }
    let x = cpu.xr(o.b as u32, sf);
    let opc = (f >> 9) & 3;
    let r = match opc {
        0 | 3 => x & y,
        1 => x | y,
        _ => x ^ y,
    };
    if opc == 3 {
        cpu.nzcv = logic_flags(r, sf);
    }
    cpu.set_xr(o.a as u32, sf, r);
    pc.wrapping_add(4)
}

fn h_add_sub_reg<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let f = o.imm as u32;
    let sf = f & (1 << 11) != 0;
    let sub = f & (1 << 8) != 0;
    let y = shift_reg(cpu.xr(o.c as u32, sf), f & 3, (f >> 2) & 0x3F, sf);
    let y = if sub { mask(!y, sf) } else { y };
    let (r, nzcv) = add_with_carry(cpu.xr(o.b as u32, sf), y, sub, sf);
    if f & (1 << 9) != 0 {
        cpu.nzcv = nzcv;
    }
    cpu.set_xr(o.a as u32, sf, r);
    pc.wrapping_add(4)
}

fn h_bitfield<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let insn = o.insn;
    let sf = bit(insn, 31);
    let (wmask, tmask) = (o.imm, o.imm2);
    let src = cpu.xr(o.b as u32, sf);
    let bot = ror(src, o.c as u32, sf) & wmask;
    let r = match field(insn, 29, 2) {
        0 => {
            let top = if (src >> field(insn, 10, 6)) & 1 != 0 {
                !0
            } else {
                0
            };
            (top & !tmask) | (bot & tmask)
        }
        1 => {
            let dst = cpu.xr(o.a as u32, sf);
            let bot = (dst & !wmask) | bot;
            (dst & !tmask) | (bot & tmask)
        }
        _ => bot & tmask,
    };
    cpu.set_xr(o.a as u32, sf, r);
    pc.wrapping_add(4)
}

fn h_ccmp<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let f = o.c as u32;
    if cpu.cond_holds(f & 0xF) {
        let sf = f & (1 << 4) != 0;
        let y = if f & (1 << 6) != 0 {
            u64::from(o.b)
        } else {
            cpu.xr(o.b as u32, sf)
        };
        let sub = f & (1 << 5) != 0;
        let y = if sub { mask(!y, sf) } else { y };
        cpu.nzcv = add_with_carry(cpu.xr(o.a as u32, sf), y, sub, sf).1;
    } else {
        cpu.nzcv = o.imm as u32;
    }
    pc.wrapping_add(4)
}

fn h_csel<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let f = o.imm as u32;
    let sf = f & (1 << 6) != 0;
    let r = if cpu.cond_holds(f & 0xF) {
        cpu.xr(o.b as u32, sf)
    } else {
        let y = cpu.xr(o.c as u32, sf);
        match (f >> 4) & 3 {
            0 => y,
            2 => y.wrapping_add(1),
            1 => !y,
            _ => y.wrapping_neg(),
        }
    };
    cpu.set_xr(o.a as u32, sf, r);
    pc.wrapping_add(4)
}

fn h_madd<M: Memory + ?Sized>(cpu: &mut Cpu, o: &Op, _: &mut M, pc: u64) -> u64 {
    let f = o.imm as u32;
    let sf = f & (1 << 6) != 0;
    let p = cpu.xr(o.b as u32, sf).wrapping_mul(cpu.xr(o.c as u32, sf));
    let acc = cpu.xr(f & 0x1F, sf);
    let r = if f & (1 << 5) != 0 {
        acc.wrapping_sub(p)
    } else {
        acc.wrapping_add(p)
    };
    cpu.set_xr(o.a as u32, sf, r);
    pc.wrapping_add(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aarch64::cpu::Abort;

    /// RAM that records every write, so two runs can be compared.
    #[derive(Clone, PartialEq, Debug)]
    struct Mem {
        bytes: Vec<u8>,
        writes: Vec<(u64, u32, u64)>,
    }

    impl Memory for Mem {
        fn read(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
            let a = addr as usize;
            let s = self
                .bytes
                .get(a..a.saturating_add(size as usize))
                .ok_or(Abort { addr, write: false })?;
            let mut b = [0u8; 8];
            b[..s.len()].copy_from_slice(s);
            Ok(u64::from_le_bytes(b))
        }
        fn write(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
            let a = addr as usize;
            let d = self
                .bytes
                .get_mut(a..a.saturating_add(size as usize))
                .ok_or(Abort { addr, write: true })?;
            d.copy_from_slice(&value.to_le_bytes()[..size as usize]);
            self.writes.push((addr, size, value));
            Ok(())
        }
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn state(rng: &mut Rng) -> Cpu {
        let mut c = Cpu::new();
        c.el = (rng.next() % 3) as u32;
        c.spsel = rng.next() & 1 != 0;
        for r in c.x.iter_mut() {
            *r = match rng.next() % 4 {
                0 => rng.next() & 0xFFF8,
                1 => rng.next() % 64,
                2 => rng.next(),
                _ => 0x8000 + (rng.next() & 0x7FF8),
            };
        }
        for s in c.sp_el.iter_mut() {
            *s = 0x8000 + (rng.next() & 0x7FF0);
        }
        c.nzcv = (rng.next() as u32) & 0xF000_0000;
        c.pc = 0x4000 + (rng.next() & 0xFFC);
        c
    }

    /// Shapes of the handled forms, so random words land in them often.
    const SHAPES: &[(u32, u32)] = &[
        (0x1F00_0000, 0x1000_0000),
        (0x1F80_0000, 0x1100_0000),
        (0x1F80_0000, 0x1200_0000),
        (0x1F80_0000, 0x1280_0000),
        (0x1F80_0000, 0x1300_0000),
        (0x1F00_0000, 0x0A00_0000),
        (0x1F20_0000, 0x0B00_0000),
        (0x7C00_0000, 0x1400_0000),
        (0x7E00_0000, 0x3400_0000),
        (0x7E00_0000, 0x3600_0000),
        (0xFF00_0010, 0x5400_0000),
        (0xFE00_0000, 0xD600_0000),
        (0x3B00_0000, 0x3900_0000),
        (0x3B00_0000, 0x3800_0000),
        (0x3A00_0000, 0x2800_0000),
        (0x1FE0_0000, 0x1A40_0000),
        (0x1FE0_0000, 0x1A80_0000),
        (0x1F00_0000, 0x1B00_0000),
        (0xFFFF_F01F, 0xD503_201F),
        (0xFFFF_F01F, 0xD503_301F),
        (0x3B20_0C00, 0x3820_0800),
        (0x0000_0000, 0x0000_0000),
    ];

    #[test]
    fn decoded_forms_do_what_execute_does() {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let mem = Mem {
            bytes: (0..0x10000u32).map(|i| (i * 7 + 3) as u8).collect(),
            writes: Vec::new(),
        };
        let mut handled = 0;
        for _ in 0..400_000 {
            let (m, v) = SHAPES[(rng.next() % SHAPES.len() as u64) as usize];
            let insn = (rng.next() as u32 & !m) | v;
            let o = decode(insn);
            handled += usize::from(o.kind != Kind::Execute);
            let mut c1 = state(&mut rng);
            let mut c2 = c1.clone();
            let (mut m1, mut m2) = (mem.clone(), mem.clone());
            c1.next_pc = c1.pc.wrapping_add(4);
            c2.next_pc = c2.pc.wrapping_add(4);
            let r1 = exec::execute(&mut c1, insn, &mut m1);
            let r2 = c2.run_op(o, &mut m2);
            let what = |r: &Exec| match r {
                Ok(()) => "ok".to_string(),
                Err(exec::Stop::Exception(e)) => format!("{e:?}"),
                Err(exec::Stop::Wfi) => "wfi".into(),
                Err(exec::Stop::Wfe) => "wfe".into(),
                Err(exec::Stop::Unimplemented) => "unimplemented".into(),
            };
            assert_eq!(what(&r1), what(&r2), "{insn:#010x} {o:?}");
            assert_eq!(c1.x, c2.x, "{insn:#010x} {o:?}");
            assert_eq!(c1.sp_el, c2.sp_el, "{insn:#010x} {o:?}");
            assert_eq!(c1.nzcv, c2.nzcv, "{insn:#010x} {o:?}");
            assert_eq!(c1.next_pc, c2.next_pc, "{insn:#010x} {o:?}");
            assert_eq!(c1.v, c2.v, "{insn:#010x} {o:?}");
            assert_eq!(m1.writes, m2.writes, "{insn:#010x} {o:?}");
        }
        assert!(
            handled > 200_000,
            "only {handled} words decoded to a handler"
        );
    }

    #[test]
    fn register_only_forms_run_without_the_pc() {
        // add x2, x2, #1; cmp x20, #1; mov x0, x1; cmp x1, x2; nop; isb;
        // lsl x6, x5, #2; csel x7, x5, x6, eq
        for w in [
            0x9100_0442,
            0xF100_069F,
            0xAA01_03E0,
            0xEB02_003F,
            0xD503_201F,
            0xD503_3FDF,
            0xD37E_F4A6,
            0x9A86_00A7,
        ] {
            let o = decode(w);
            assert!(
                o.pure() && !o.touches_memory() && !o.ends_run(),
                "{w:#010x} {o:?}"
            );
        }
    }

    #[test]
    fn a_slot_follows_the_word_it_holds() {
        let mut d = Decoded::new();
        let add = 0x9100_0442; // add x2, x2, #1
        assert_eq!(d.get(add).kind, Kind::AddImmR);
        let colliding = (1u32..)
            .map(|k| add ^ k)
            .find(|&w| {
                let slot = |w: u32| w.wrapping_mul(0x9E37_79B1) >> (32 - SLOTS_LOG2);
                slot(w) == slot(add)
            })
            .unwrap();
        assert_eq!(d.get(colliding), decode(colliding));
        assert_eq!(d.get(add).kind, Kind::AddImmR);
    }
}
