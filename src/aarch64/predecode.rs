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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub(super) enum Kind {
    // The last of a straight run (`Op::ends_run`).
    #[default]
    Execute,
    BranchReg,
    B,
    Bl,
    BCond,
    Cbz,
    Tbz,
    // Loads and stores (`Op::touches_memory`).
    LdStReg,
    LdStPair,
    LoadUimm,
    StoreUimm,
    LdStRegOff,
    LdStImm,
    Pair,
    // The PC, or an exception at it.
    DpImm,
    DpReg,
    Adr,
    Adrp,
    // Registers and flags only (`Op::pure`).
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
    insn: u32,
    kind: Kind,
    a: u8,
    b: u8,
    c: u8,
    imm: u64,
    imm2: u64,
}

impl Op {
    pub fn insn(&self) -> u32 {
        self.insn
    }

    /// Whether the instruction loads or stores; the other forms a straight
    /// run holds before its last touch nothing but registers and flags.
    #[inline(always)]
    pub fn touches_memory(&self) -> bool {
        (Kind::LdStReg as u8..=Kind::Pair as u8).contains(&(self.kind as u8))
    }

    /// Whether the instruction can move the PC anywhere but on by 4, or do
    /// anything else [`exec::execute`] does: the last of a straight run.
    #[inline(always)]
    pub fn ends_run(&self) -> bool {
        self.kind as u8 <= Kind::Tbz as u8
    }

    /// Whether the instruction reads and writes nothing but general
    /// registers and flags, and cannot raise an exception: it needs no PC.
    #[inline(always)]
    pub fn pure(&self) -> bool {
        self.kind as u8 >= Kind::AddImm as u8
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
        let i = (insn.wrapping_mul(0x9E37_79B1) >> (32 - SLOTS_LOG2)) as usize;
        let slot = &mut self.slots[i];
        if slot.insn != insn {
            *slot = decode(insn);
        }
        *slot
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
                (false, false) => Kind::AddImm,
                (true, false) => Kind::SubImm,
                (false, true) => Kind::AddsImm,
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
        let (scale, access) =
            single_access(field(insn, 30, 2), false, field(insn, 22, 2)).ok()??;
        let rt = field(insn, 0, 5);
        let rn = field(insn, 5, 5);
        let imm = (field(insn, 10, 12) as u64) << scale;
        Some(match access {
            Access::Store => op(insn, Kind::StoreUimm, rt, rn, scale, imm),
            Access::Load => op(insn, Kind::LoadUimm, rt, rn, scale, imm),
            Access::LoadSigned64 => op(insn, Kind::LoadUimm, rt, rn, scale | 4, imm),
            Access::LoadSigned32 => op(insn, Kind::LoadUimm, rt, rn, scale | 8, imm),
        })
    } else {
        None
    }
}

fn pair(insn: u32) -> Option<Op> {
    if bit(insn, 26) {
        return None;
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
        Some(op(insn, Kind::LogicReg, rd, rn, rm, flags as u64))
    } else if insn & 0x1F20_0000 == 0x0B00_0000 {
        if shift == 3 || (!sf && imm6 >= 32) {
            return None;
        }
        let flags = shift | imm6 << 2 | (bit(insn, 30) as u32) << 8;
        let flags = flags | (bit(insn, 29) as u32) << 9 | (sf as u32) << 11;
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
            if !self.host_write(mem, addr, bytes, v) && !self.store_out(mem, addr, bytes, v) {
                return Err(self.take_fault());
            }
            return Ok(None);
        }
        let v = match self.host_read(mem, addr, bytes) {
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
        let (a, b, c) = (o.a as u32, o.b as u32, o.c as u32);
        if o.pure() {
            self.run_pure(o);
            return Ok(());
        }
        match o.kind {
            Kind::Execute => exec::execute(self, o.insn, mem),
            Kind::DpImm => exec::dp_imm(self, o.insn),
            Kind::DpReg => exec::dp_reg(self, o.insn),
            Kind::LdStReg => exec::ld_st_reg(self, o.insn, mem),
            Kind::LdStPair => exec::ld_st_pair(self, o.insn, mem),
            Kind::BranchReg => exec::branch_reg(self, o.insn),
            Kind::Adr => {
                let v = self.pc.wrapping_add(o.imm);
                self.set_xr(a, true, v);
                Ok(())
            }
            Kind::Adrp => {
                let v = (self.pc & !0xFFF).wrapping_add(o.imm);
                self.set_xr(a, true, v);
                Ok(())
            }
            Kind::B => {
                self.next_pc = self.pc.wrapping_add(o.imm);
                Ok(())
            }
            Kind::Bl => {
                self.x[30] = self.pc.wrapping_add(4);
                self.next_pc = self.pc.wrapping_add(o.imm);
                Ok(())
            }
            Kind::BCond => {
                if self.cond_holds(a) {
                    self.next_pc = self.pc.wrapping_add(o.imm);
                }
                Ok(())
            }
            Kind::Cbz => {
                let zero = self.xr(a, c & 1 != 0) == 0;
                if zero != (c & 2 != 0) {
                    self.next_pc = self.pc.wrapping_add(o.imm);
                }
                Ok(())
            }
            Kind::Tbz => {
                let set = (self.xr(a, true) >> b) & 1 != 0;
                if set == (c != 0) {
                    self.next_pc = self.pc.wrapping_add(o.imm);
                }
                Ok(())
            }
            Kind::LoadUimm => {
                let addr = self.xsp(b).wrapping_add(o.imm);
                let access = match c >> 2 {
                    0 => Access::Load,
                    1 => Access::LoadSigned64,
                    _ => Access::LoadSigned32,
                };
                if let Some((v, sf)) = self.gpr(mem, c & 3, access, a, addr)? {
                    self.set_xr(a, sf, v);
                }
                Ok(())
            }
            Kind::StoreUimm => {
                let addr = self.xsp(b).wrapping_add(o.imm);
                self.gpr(mem, c, Access::Store, a, addr)?;
                Ok(())
            }
            Kind::LdStRegOff => {
                let f = o.imm as u32;
                let scale = f & 3;
                let off = extend_reg(self.xr(c, true), (f >> 4) & 7, f >> 7, true);
                let addr = self.xsp(b).wrapping_add(off);
                let access = access_of(f >> 2);
                if let Some((v, sf)) = self.gpr(mem, scale, access, a, addr)? {
                    self.set_xr(a, sf, v);
                }
                Ok(())
            }
            Kind::LdStImm => {
                let f = o.imm2 as u32;
                let base = self.xsp(b);
                let (addr, wb) = match (f >> 4) & 3 {
                    0 => (base.wrapping_add(o.imm), None),
                    1 => (base, Some(base.wrapping_add(o.imm))),
                    _ => {
                        let a = base.wrapping_add(o.imm);
                        (a, Some(a))
                    }
                };
                let loaded = self.gpr(mem, f & 3, access_of(f >> 2), a, addr)?;
                if let Some(wb) = wb {
                    self.set_xsp(b, true, wb);
                }
                if let Some((v, sf)) = loaded {
                    self.set_xr(a, sf, v);
                }
                Ok(())
            }
            Kind::Pair => {
                let f = o.imm2 as u32;
                let scale = f & 3;
                let access = access_of(f >> 2);
                let base = self.xsp(b);
                let (addr, wb) = match (f >> 4) & 3 {
                    0 | 2 => (base.wrapping_add(o.imm), None),
                    1 => (base, Some(base.wrapping_add(o.imm))),
                    _ => {
                        let a = base.wrapping_add(o.imm);
                        (a, Some(a))
                    }
                };
                let first = self.gpr(mem, scale, access, a, addr)?;
                let next = addr.wrapping_add(1 << scale);
                let second = self.gpr(mem, scale, access, c, next)?;
                if let Some(wb) = wb {
                    self.set_xsp(b, true, wb);
                }
                if let Some((v, sf)) = first {
                    self.set_xr(a, sf, v);
                }
                if let Some((v, sf)) = second {
                    self.set_xr(c, sf, v);
                }
                Ok(())
            }
            _ => unreachable!(),
        }
    }

    /// [`Self::run_op`] for an [`Op::pure`] instruction.
    #[inline(always)]
    pub(super) fn run_pure(&mut self, o: Op) {
        let (a, b, c) = (o.a as u32, o.b as u32, o.c as u32);
        let _: Exec = match o.kind {
            Kind::AddImm => {
                let sf = c != 0;
                let r = mask(self.xsp(b), sf).wrapping_add(o.imm);
                self.set_xsp(a, sf, r);
                Ok(())
            }
            Kind::SubImm => {
                let sf = c != 0;
                let r = mask(self.xsp(b), sf).wrapping_sub(o.imm);
                self.set_xsp(a, sf, r);
                Ok(())
            }
            Kind::AddsImm | Kind::SubsImm => {
                let sf = c != 0;
                let x = mask(self.xsp(b), sf);
                let sub = o.kind == Kind::SubsImm;
                let y = if sub { !o.imm } else { o.imm };
                let (r, nzcv) = add_with_carry(x, y, sub, sf);
                self.nzcv = nzcv;
                self.set_xr(a, sf, r);
                Ok(())
            }
            Kind::MovConst => {
                self.set_xr(a, true, o.imm);
                Ok(())
            }
            Kind::MovK => {
                let sf = c != 0;
                let r = (self.xr(a, sf) & !(0xFFFFu64 << b)) | o.imm;
                self.set_xr(a, sf, r);
                Ok(())
            }
            Kind::LogicImm => {
                let sf = c & 4 != 0;
                let x = self.xr(b, sf);
                let r = match c & 3 {
                    0 | 3 => x & o.imm,
                    1 => x | o.imm,
                    _ => x ^ o.imm,
                };
                if c & 3 == 3 {
                    self.nzcv = logic_flags(r, sf);
                    self.set_xr(a, sf, r);
                } else {
                    self.set_xsp(a, sf, r);
                }
                Ok(())
            }
            Kind::LogicReg => {
                let f = o.imm as u32;
                let sf = f & (1 << 11) != 0;
                let mut y = shift_reg(self.xr(c, sf), f & 3, (f >> 2) & 0x3F, sf);
                if f & (1 << 8) != 0 {
                    y = mask(!y, sf);
                }
                let x = self.xr(b, sf);
                let opc = (f >> 9) & 3;
                let r = match opc {
                    0 | 3 => x & y,
                    1 => x | y,
                    _ => x ^ y,
                };
                if opc == 3 {
                    self.nzcv = logic_flags(r, sf);
                }
                self.set_xr(a, sf, r);
                Ok(())
            }
            Kind::AddSubReg => {
                let f = o.imm as u32;
                let sf = f & (1 << 11) != 0;
                let sub = f & (1 << 8) != 0;
                let y = shift_reg(self.xr(c, sf), f & 3, (f >> 2) & 0x3F, sf);
                let y = if sub { mask(!y, sf) } else { y };
                let (r, nzcv) = add_with_carry(self.xr(b, sf), y, sub, sf);
                if f & (1 << 9) != 0 {
                    self.nzcv = nzcv;
                }
                self.set_xr(a, sf, r);
                Ok(())
            }
            Kind::Bitfield => {
                let insn = o.insn;
                let sf = bit(insn, 31);
                let (wmask, tmask) = (o.imm, o.imm2);
                let src = self.xr(b, sf);
                let bot = ror(src, c, sf) & wmask;
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
                        let dst = self.xr(a, sf);
                        let bot = (dst & !wmask) | bot;
                        (dst & !tmask) | (bot & tmask)
                    }
                    _ => bot & tmask,
                };
                self.set_xr(a, sf, r);
                Ok(())
            }
            Kind::Ccmp => {
                let f = c;
                if self.cond_holds(f & 0xF) {
                    let sf = f & (1 << 4) != 0;
                    let y = if f & (1 << 6) != 0 {
                        u64::from(b)
                    } else {
                        self.xr(b, sf)
                    };
                    let sub = f & (1 << 5) != 0;
                    let y = if sub { mask(!y, sf) } else { y };
                    self.nzcv = add_with_carry(self.xr(a, sf), y, sub, sf).1;
                } else {
                    self.nzcv = o.imm as u32;
                }
                Ok(())
            }
            Kind::Csel => {
                let f = o.imm as u32;
                let sf = f & (1 << 6) != 0;
                let r = if self.cond_holds(f & 0xF) {
                    self.xr(b, sf)
                } else {
                    let y = self.xr(c, sf);
                    match (f >> 4) & 3 {
                        0 => y,
                        2 => y.wrapping_add(1),
                        1 => !y,
                        _ => y.wrapping_neg(),
                    }
                };
                self.set_xr(a, sf, r);
                Ok(())
            }
            Kind::Madd => {
                let f = o.imm as u32;
                let sf = f & (1 << 6) != 0;
                let p = self.xr(b, sf).wrapping_mul(self.xr(c, sf));
                let acc = self.xr(f & 0x1F, sf);
                let r = if f & (1 << 5) != 0 {
                    acc.wrapping_sub(p)
                } else {
                    acc.wrapping_add(p)
                };
                self.set_xr(a, sf, r);
                Ok(())
            }
            _ => unreachable!(),
        };
    }
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
                .get(a..a + size as usize)
                .ok_or(Abort { addr, write: false })?;
            let mut b = [0u8; 8];
            b[..s.len()].copy_from_slice(s);
            Ok(u64::from_le_bytes(b))
        }
        fn write(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
            let a = addr as usize;
            let d = self
                .bytes
                .get_mut(a..a + size as usize)
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
    fn a_slot_follows_the_word_it_holds() {
        let mut d = Decoded::new();
        let add = 0x9100_0442; // add x2, x2, #1
        assert_eq!(d.get(add).kind, Kind::AddImm);
        let colliding = (1u32..)
            .map(|k| add ^ k)
            .find(|&w| {
                let slot = |w: u32| w.wrapping_mul(0x9E37_79B1) >> (32 - SLOTS_LOG2);
                slot(w) == slot(add)
            })
            .unwrap();
        assert_eq!(d.get(colliding), decode(colliding));
        assert_eq!(d.get(add).kind, Kind::AddImm);
    }
}
