//! A64 decode and execute, integer and load/store (ARMv8.0-A, as on the
//! Cortex-A72, plus the CRC32 instructions the A72 has).
//!
//! Decoding follows the encoding tables in ARM ARM chapter C4 top-down: the
//! `op0` field picks the group, then each group matches its classes in table
//! order. Anything the tables leave unallocated, or that belongs to an
//! extension the A72 lacks (v8.1 atomics, pointer authentication, ...), is
//! UNDEFINED, which is what the real core and `qemu-aarch64 -cpu cortex-a72`
//! both do. `tests/a64_diff.rs` holds this file to that.

use super::cpu::{Cpu, Exception, Memory, NZCV_C, NZCV_N, NZCV_V, NZCV_Z};

/// Why an instruction did not simply retire.
pub(super) enum Stop {
    Wfi,
    Wfe,
    Exception(Exception),
    Unimplemented,
}

pub(super) type Exec = Result<(), Stop>;

pub(super) const UNDEF: Stop = Stop::Exception(Exception::Undefined);

#[inline]
pub(super) fn undef() -> Exec {
    Err(UNDEF)
}

#[inline]
pub(super) fn bit(insn: u32, n: u32) -> bool {
    (insn >> n) & 1 != 0
}

#[inline]
pub(super) fn field(insn: u32, lo: u32, len: u32) -> u32 {
    (insn >> lo) & ((1 << len) - 1)
}

/// Sign-extend the low `bits` of `v`.
#[inline]
fn sext(v: u64, bits: u32) -> u64 {
    let s = 64 - bits;
    (((v << s) as i64) >> s) as u64
}

#[inline]
fn ones(n: u32) -> u64 {
    if n >= 64 {
        !0
    } else {
        (1u64 << n) - 1
    }
}

#[inline]
fn datasize(sf: bool) -> u32 {
    if sf {
        64
    } else {
        32
    }
}

#[inline]
fn mask(v: u64, sf: bool) -> u64 {
    if sf {
        v
    } else {
        v as u32 as u64
    }
}

#[inline]
fn top_bit(v: u64, sf: bool) -> bool {
    (v >> (datasize(sf) - 1)) & 1 != 0
}

#[inline]
fn ror(v: u64, amount: u32, sf: bool) -> u64 {
    if sf {
        v.rotate_right(amount)
    } else {
        (v as u32).rotate_right(amount) as u64
    }
}

/// `AddWithCarry` (ARM ARM J1): the result and its `NZCV`.
pub(super) fn add_with_carry(x: u64, y: u64, carry: bool, sf: bool) -> (u64, u32) {
    let c = carry as u64;
    let (r, carry_out, overflow) = if sf {
        let (r1, c1) = x.overflowing_add(y);
        let (r, c2) = r1.overflowing_add(c);
        let v = ((x ^ r) & (y ^ r)) >> 63 != 0;
        (r, c1 | c2, v)
    } else {
        let (x, y) = (x as u32, y as u32);
        let (r1, c1) = x.overflowing_add(y);
        let (r, c2) = r1.overflowing_add(c as u32);
        let v = ((x ^ r) & (y ^ r)) >> 31 != 0;
        (r as u64, c1 | c2, v)
    };
    let mut nzcv = 0;
    if top_bit(r, sf) {
        nzcv |= NZCV_N;
    }
    if r == 0 {
        nzcv |= NZCV_Z;
    }
    if carry_out {
        nzcv |= NZCV_C;
    }
    if overflow {
        nzcv |= NZCV_V;
    }
    (r, nzcv)
}

/// `NZ` from a logical result, `C` and `V` cleared.
#[inline]
fn logic_flags(r: u64, sf: bool) -> u32 {
    let mut nzcv = 0;
    if top_bit(r, sf) {
        nzcv |= NZCV_N;
    }
    if r == 0 {
        nzcv |= NZCV_Z;
    }
    nzcv
}

/// `DecodeBitMasks` (ARM ARM J1): `(wmask, tmask)` for the logical-immediate
/// and bitfield encodings, or `None` for a reserved combination.
pub(super) fn decode_bit_masks(
    n: u32,
    imms: u32,
    immr: u32,
    immediate: bool,
    datasize: u32,
) -> Option<(u64, u64)> {
    let combined = (n << 6) | (!imms & 0x3F);
    if combined == 0 {
        return None;
    }
    let len = 31 - combined.leading_zeros();
    if len < 1 {
        return None;
    }
    let esize = 1u32 << len;
    if esize > datasize {
        return None;
    }
    let levels = esize - 1;
    if immediate && imms & levels == levels {
        return None;
    }
    let s = imms & levels;
    let r = immr & levels;
    let d = s.wrapping_sub(r) & levels;
    let emask = ones(esize);
    let welem = ones(s + 1);
    let telem = ones(d + 1);
    let wrot = if r == 0 {
        welem
    } else {
        ((welem >> r) | (welem << (esize - r))) & emask
    };
    let replicate = |e: u64| {
        let mut out = 0u64;
        let mut i = 0;
        while i < 64 {
            out |= e << i;
            i += esize;
        }
        out
    };
    let dmask = ones(datasize);
    Some((replicate(wrot) & dmask, replicate(telem) & dmask))
}

/// `ShiftReg`: LSL, LSR, ASR, ROR of a `datasize`-bit value.
#[inline]
fn shift_reg(v: u64, kind: u32, amount: u32, sf: bool) -> u64 {
    if amount == 0 {
        return v;
    }
    if sf {
        match kind {
            0 => v << amount,
            1 => v >> amount,
            2 => ((v as i64) >> amount) as u64,
            _ => v.rotate_right(amount),
        }
    } else {
        let w = v as u32;
        (match kind {
            0 => w << amount,
            1 => w >> amount,
            2 => ((w as i32) >> amount) as u32,
            _ => w.rotate_right(amount),
        }) as u64
    }
}

/// `ExtendReg`: the `UXTB`..`SXTX` extend, then a left shift.
#[inline]
fn extend_reg(v: u64, option: u32, shift: u32, sf: bool) -> u64 {
    let len = 8 << (option & 3);
    let e = if option & 4 != 0 {
        sext(v, len)
    } else {
        v & ones(len)
    };
    mask(e << shift, sf)
}

/// CRC-32 update, bit-reflected, no pre/post inversion (ARM ARM C6.2
/// `CRC32B` etc.): what `CRC32*` / `CRC32C*` compute over `bytes` of `val`.
fn crc32(mut acc: u32, val: u64, bytes: u32, castagnoli: bool) -> u32 {
    let poly = if castagnoli { 0x82F6_3B78 } else { 0xEDB8_8320 };
    for i in 0..bytes {
        acc ^= (val >> (8 * i)) as u8 as u32;
        for _ in 0..8 {
            acc = if acc & 1 != 0 {
                (acc >> 1) ^ poly
            } else {
                acc >> 1
            };
        }
    }
    acc
}

impl Cpu {
    /// Read general register `r` with 31 = XZR, truncated to the operand size.
    #[inline]
    pub(super) fn xr(&self, r: u32, sf: bool) -> u64 {
        if r == 31 {
            0
        } else {
            mask(self.x[r as usize], sf)
        }
    }

    /// Read general register `r` with 31 = SP.
    #[inline]
    pub(super) fn xsp(&self, r: u32) -> u64 {
        if r == 31 {
            self.sp()
        } else {
            self.x[r as usize]
        }
    }

    /// Write general register `r` with 31 = XZR; a 32-bit write zeroes the
    /// top half.
    #[inline]
    pub(super) fn set_xr(&mut self, r: u32, sf: bool, v: u64) {
        if r != 31 {
            self.x[r as usize] = mask(v, sf);
        }
    }

    /// Write general register `r` with 31 = SP.
    #[inline]
    pub(super) fn set_xsp(&mut self, r: u32, sf: bool, v: u64) {
        if r == 31 {
            self.set_sp(mask(v, sf));
        } else {
            self.x[r as usize] = mask(v, sf);
        }
    }

    /// `ConditionHolds`.
    #[inline]
    pub(super) fn cond_holds(&self, cond: u32) -> bool {
        let f = self.nzcv;
        let (n, z, c, v) = (
            f & NZCV_N != 0,
            f & NZCV_Z != 0,
            f & NZCV_C != 0,
            f & NZCV_V != 0,
        );
        let r = match cond >> 1 {
            0 => z,
            1 => c,
            2 => n,
            3 => v,
            4 => c && !z,
            5 => n == v,
            6 => n == v && !z,
            _ => true,
        };
        if cond & 1 != 0 && cond != 0xF {
            !r
        } else {
            r
        }
    }

    fn branch_to(&mut self, target: u64) {
        self.next_pc = target;
    }
}

/// Execute `insn`, which was fetched from `cpu.pc`.
pub(super) fn execute<M: Memory + ?Sized>(cpu: &mut Cpu, insn: u32, mem: &mut M) -> Exec {
    match field(insn, 25, 4) {
        0b1000 | 0b1001 => dp_imm(cpu, insn),
        0b1010 | 0b1011 => branch_sys(cpu, insn, mem),
        0b0100 | 0b0110 | 0b1100 | 0b1110 => ldst(cpu, insn, mem),
        0b0101 | 0b1101 => dp_reg(cpu, insn),
        0b0111 | 0b1111 => super::simd::execute(cpu, insn),
        _ => undef(),
    }
}

// ---------------------------------------------------------------------------
// Data processing, immediate (C4.1.86)

fn dp_imm(cpu: &mut Cpu, insn: u32) -> Exec {
    let sf = bit(insn, 31);
    let rd = field(insn, 0, 5);
    let rn = field(insn, 5, 5);
    match field(insn, 23, 3) {
        0b000 | 0b001 => {
            // ADR / ADRP
            let imm = sext(((field(insn, 5, 19) << 2) | field(insn, 29, 2)) as u64, 21);
            let v = if sf {
                (cpu.pc & !0xFFF).wrapping_add(imm << 12)
            } else {
                cpu.pc.wrapping_add(imm)
            };
            cpu.set_xr(rd, true, v);
        }
        0b010 => {
            // ADD/ADDS/SUB/SUBS (immediate)
            let sub = bit(insn, 30);
            let s = bit(insn, 29);
            let mut imm = field(insn, 10, 12) as u64;
            if bit(insn, 22) {
                imm <<= 12;
            }
            let x = mask(cpu.xsp(rn), sf);
            let (y, c) = if sub { (!imm, true) } else { (imm, false) };
            let (r, nzcv) = add_with_carry(x, y, c, sf);
            if s {
                cpu.nzcv = nzcv;
                cpu.set_xr(rd, sf, r);
            } else {
                cpu.set_xsp(rd, sf, r);
            }
        }
        0b100 => {
            // AND/ORR/EOR/ANDS (immediate)
            let n = field(insn, 22, 1);
            if !sf && n != 0 {
                return undef();
            }
            let Some((imm, _)) = decode_bit_masks(
                n,
                field(insn, 10, 6),
                field(insn, 16, 6),
                true,
                datasize(sf),
            ) else {
                return undef();
            };
            let x = cpu.xr(rn, sf);
            let opc = field(insn, 29, 2);
            let r = match opc {
                0 | 3 => x & imm,
                1 => x | imm,
                _ => x ^ imm,
            };
            if opc == 3 {
                cpu.nzcv = logic_flags(r, sf);
                cpu.set_xr(rd, sf, r);
            } else {
                cpu.set_xsp(rd, sf, r);
            }
        }
        0b101 => {
            // MOVN/MOVZ/MOVK
            let opc = field(insn, 29, 2);
            let hw = field(insn, 21, 2);
            if opc == 1 || (!sf && hw >= 2) {
                return undef();
            }
            let pos = hw * 16;
            let imm = (field(insn, 5, 16) as u64) << pos;
            let r = match opc {
                0 => !imm,
                2 => imm,
                _ => (cpu.xr(rd, sf) & !(0xFFFFu64 << pos)) | imm,
            };
            cpu.set_xr(rd, sf, r);
        }
        0b110 => {
            // SBFM/BFM/UBFM
            let opc = field(insn, 29, 2);
            let n = field(insn, 22, 1);
            let immr = field(insn, 16, 6);
            let imms = field(insn, 10, 6);
            if opc == 3 || n != sf as u32 || (!sf && (immr | imms) & 0x20 != 0) {
                return undef();
            }
            let Some((wmask, tmask)) = decode_bit_masks(n, imms, immr, false, datasize(sf)) else {
                return undef();
            };
            let src = cpu.xr(rn, sf);
            let bot = ror(src, immr, sf) & wmask;
            let r = match opc {
                0 => {
                    let top = if (src >> imms) & 1 != 0 { !0 } else { 0 };
                    (top & !tmask) | (bot & tmask)
                }
                1 => {
                    let dst = cpu.xr(rd, sf);
                    let bot = (dst & !wmask) | bot;
                    (dst & !tmask) | (bot & tmask)
                }
                _ => bot & tmask,
            };
            cpu.set_xr(rd, sf, r);
        }
        0b111 => {
            // EXTR
            let rm = field(insn, 16, 5);
            let imms = field(insn, 10, 6);
            if field(insn, 29, 2) != 0
                || field(insn, 22, 1) != sf as u32
                || bit(insn, 21)
                || (!sf && imms >= 32)
            {
                return undef();
            }
            let hi = cpu.xr(rn, sf);
            let lo = cpu.xr(rm, sf);
            let r = if imms == 0 {
                lo
            } else {
                (lo >> imms) | (hi << (datasize(sf) - imms))
            };
            cpu.set_xr(rd, sf, r);
        }
        _ => return undef(),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Branches, exception generating and system instructions (C4.1.87)

fn branch_sys<M: Memory + ?Sized>(cpu: &mut Cpu, insn: u32, mem: &mut M) -> Exec {
    let pc = cpu.pc;
    if insn & 0x7C00_0000 == 0x1400_0000 {
        // B / BL
        let off = sext(field(insn, 0, 26) as u64, 26) << 2;
        if bit(insn, 31) {
            cpu.x[30] = pc.wrapping_add(4);
        }
        cpu.branch_to(pc.wrapping_add(off));
    } else if insn & 0x7E00_0000 == 0x3400_0000 {
        // CBZ / CBNZ
        let sf = bit(insn, 31);
        let zero = cpu.xr(field(insn, 0, 5), sf) == 0;
        if zero != bit(insn, 24) {
            let off = sext(field(insn, 5, 19) as u64, 19) << 2;
            cpu.branch_to(pc.wrapping_add(off));
        }
    } else if insn & 0x7E00_0000 == 0x3600_0000 {
        // TBZ / TBNZ
        let b = (field(insn, 31, 1) << 5) | field(insn, 19, 5);
        let set = (cpu.xr(field(insn, 0, 5), true) >> b) & 1 != 0;
        if set == bit(insn, 24) {
            let off = sext(field(insn, 5, 14) as u64, 14) << 2;
            cpu.branch_to(pc.wrapping_add(off));
        }
    } else if insn & 0xFF00_0010 == 0x5400_0000 {
        // B.cond
        if cpu.cond_holds(field(insn, 0, 4)) {
            let off = sext(field(insn, 5, 19) as u64, 19) << 2;
            cpu.branch_to(pc.wrapping_add(off));
        }
    } else if insn & 0xFF00_0000 == 0xD400_0000 {
        return exception_gen(cpu, insn);
    } else if insn & 0xFFC0_0000 == 0xD500_0000 {
        return system(cpu, insn, mem);
    } else if insn & 0xFE00_0000 == 0xD600_0000 {
        return branch_reg(cpu, insn);
    } else {
        return undef();
    }
    Ok(())
}

fn exception_gen(cpu: &mut Cpu, insn: u32) -> Exec {
    let imm = field(insn, 5, 16) as u16;
    if field(insn, 2, 3) != 0 {
        return undef();
    }
    let e = match (field(insn, 21, 3), field(insn, 0, 2)) {
        (0, 1) => Exception::Svc(imm),
        (0, 2) if cpu.el > 0 => Exception::Hvc(imm),
        (0, 3) if cpu.el > 0 => Exception::Smc(imm),
        (1, 0) => Exception::Brk(imm),
        // HLT with halting debug disabled, DCPSn outside debug state: both
        // UNDEFINED on a production part.
        _ => Exception::Undefined,
    };
    Err(Stop::Exception(e))
}

fn branch_reg(cpu: &mut Cpu, insn: u32) -> Exec {
    let opc = field(insn, 21, 4);
    let rn = field(insn, 5, 5);
    // op2 = 11111, op3 = 000000, op4 = 00000; anything else is pointer
    // authentication (v8.3).
    if field(insn, 16, 5) != 0x1F || field(insn, 10, 6) != 0 || field(insn, 0, 5) != 0 {
        return undef();
    }
    match opc {
        0 | 2 => {
            let t = cpu.xr(rn, true);
            cpu.branch_to(t);
        }
        1 => {
            let t = cpu.xr(rn, true);
            cpu.x[30] = cpu.pc.wrapping_add(4);
            cpu.branch_to(t);
        }
        4 if cpu.el > 0 && rn == 31 => {
            if !cpu.eret() {
                return Err(Stop::Unimplemented);
            }
        }
        _ => return undef(),
    }
    Ok(())
}

fn system<M: Memory + ?Sized>(cpu: &mut Cpu, insn: u32, mem: &mut M) -> Exec {
    let l = bit(insn, 21);
    let op0 = field(insn, 19, 2);
    let op1 = field(insn, 16, 3);
    let crn = field(insn, 12, 4);
    let crm = field(insn, 8, 4);
    let op2 = field(insn, 5, 3);
    let rt = field(insn, 0, 5);
    match (l, op0) {
        (false, 0) => match crn {
            // Hints. Unallocated hints execute as NOP.
            2 if rt == 31 => match (crm << 3) | op2 {
                // WFE: a pending event is consumed instead of waiting.
                2 if cpu.event => {
                    cpu.event = false;
                    Ok(())
                }
                2 => Err(Stop::Wfe),
                3 => Err(Stop::Wfi),
                // SEV: every core's Event Register, this one's included.
                4 => {
                    cpu.event = true;
                    cpu.sev = true;
                    Ok(())
                }
                // SEVL: this core's only.
                5 => {
                    cpu.event = true;
                    Ok(())
                }
                _ => Ok(()),
            },
            // Barriers.
            3 if rt == 31 && op1 == 3 => match op2 {
                2 => {
                    cpu.exclusive = None;
                    Ok(())
                }
                4..=6 => Ok(()),
                _ => undef(),
            },
            // MSR (immediate).
            4 if rt == 31 => msr_imm(cpu, op1, op2, crm),
            _ => undef(),
        },
        (_, 1) => sys(cpu, l, op1, crn, crm, op2, rt, mem),
        (_, _) if op0 >= 2 => {
            let key = super::sysreg::key(op0, op1, crn, crm, op2);
            if l {
                let v = super::sysreg::read(cpu, key, mem)?;
                cpu.set_xr(rt, true, v);
                Ok(())
            } else {
                let v = cpu.xr(rt, true);
                super::sysreg::write(cpu, key, v, mem)
            }
        }
        _ => undef(),
    }
}

fn msr_imm(cpu: &mut Cpu, op1: u32, op2: u32, crm: u32) -> Exec {
    match (op1, op2) {
        (0, 5) if cpu.el > 0 => cpu.spsel = crm & 1 != 0,
        (3, 6) if cpu.el > 0 => cpu.daif |= crm << 6,
        (3, 7) if cpu.el > 0 => cpu.daif &= !(crm << 6),
        _ => return undef(),
    }
    Ok(())
}

/// `SYS`/`SYSL`: cache, TLB and address-translation maintenance.
#[allow(clippy::too_many_arguments)]
fn sys<M: Memory + ?Sized>(
    cpu: &mut Cpu,
    l: bool,
    op1: u32,
    crn: u32,
    crm: u32,
    op2: u32,
    rt: u32,
    mem: &mut M,
) -> Exec {
    if l {
        return undef();
    }
    match (op1, crn, crm, op2) {
        // DC ZVA: DCZID_EL0.BS = 4, so a 64-byte block.
        (3, 7, 4, 1) => {
            let base = cpu.xr(rt, true) & !63;
            for i in 0..8 {
                cpu.write(mem, base + i * 8, 8, 0)?;
            }
            Ok(())
        }
        // DC CVAC/CVAU/CIVAC, IC IVAU: no caches modelled. The EL0 trap
        // controls (SCTLR_EL1.UCI) are not modelled.
        (3, 7, 10 | 11 | 14, 1) | (3, 7, 5, 1) => Ok(()),
        _ if cpu.el == 0 => undef(),
        // The rest of the cache maintenance (DC IVAC/ISW/CSW/CISW, IC IALLU/
        // IALLUIS): no caches.
        (0, 7, 6 | 10 | 14, 1 | 2) | (0, 7, 5 | 1, 0) => Ok(()),
        // TLB maintenance: every TLBI drops the whole TLB, which is always
        // allowed.
        (0 | 4 | 6, 8, _, _) if op1 / 2 <= cpu.el => {
            cpu.tlb.flush();
            cpu.tlb.broadcast = true;
            Ok(())
        }
        // AT S1E1R/W, S1E0R/W; S1E2R/W and S12E1*/S12E0* from EL2; S1E3R/W.
        (0, 7, 8, 0..=3) | (4, 7, 8, 0 | 1 | 4..=7) | (6, 7, 8, 0 | 1) if op1 / 2 <= cpu.el => {
            let regime = match (op1, op2) {
                (4, 0 | 1) => 2,
                (6, _) => 3,
                _ => 1,
            };
            let user = regime == 1 && op2 & 2 != 0;
            let va = cpu.xr(rt, true);
            super::mmu::at(cpu, mem, regime, user, op2 & 1 != 0, va);
            Ok(())
        }
        _ => Err(Stop::Unimplemented),
    }
}

// ---------------------------------------------------------------------------
// Loads and stores (C4.1.88)

fn ldst<M: Memory + ?Sized>(cpu: &mut Cpu, insn: u32, mem: &mut M) -> Exec {
    if insn & 0x3F00_0000 == 0x0800_0000 {
        ld_st_exclusive(cpu, insn, mem)
    } else if insn & 0x3B00_0000 == 0x1800_0000 {
        ld_literal(cpu, insn, mem)
    } else if insn & 0x3A00_0000 == 0x2800_0000 {
        ld_st_pair(cpu, insn, mem)
    } else if insn & 0x3B00_0000 == 0x3800_0000 {
        ld_st_reg(cpu, insn, mem)
    } else if insn & 0x3B00_0000 == 0x3900_0000 {
        ld_st_reg_uimm(cpu, insn, mem)
    } else if insn & 0xBE00_0000 == 0x0C00_0000 {
        super::simd::ldst_structures(cpu, insn, mem)
    } else {
        undef()
    }
}

fn read128<M: Memory + ?Sized>(cpu: &mut Cpu, mem: &mut M, addr: u64) -> Result<u128, Stop> {
    let lo = cpu.read(mem, addr, 8)?;
    let hi = cpu.read(mem, addr.wrapping_add(8), 8)?;
    Ok(((hi as u128) << 64) | lo as u128)
}

fn write128<M: Memory + ?Sized>(cpu: &mut Cpu, mem: &mut M, addr: u64, v: u128) -> Exec {
    cpu.write(mem, addr, 8, v as u64)?;
    cpu.write(mem, addr.wrapping_add(8), 8, (v >> 64) as u64)?;
    Ok(())
}

/// One access of `1 << scale` bytes between memory and `rt`. `kind` is the
/// non-SIMD `opc` (store, load, load signed to 64, load signed to 32); for
/// a SIMD&FP register it is store/load only.
#[derive(Clone, Copy)]
enum Access {
    Store,
    Load,
    LoadSigned64,
    LoadSigned32,
}

/// Load or store one register. Returns the loaded value for loads to a
/// general register so the caller can write it after any base write-back.
fn transfer<M: Memory + ?Sized>(
    cpu: &mut Cpu,
    mem: &mut M,
    simd: bool,
    scale: u32,
    access: Access,
    rt: u32,
    addr: u64,
) -> Result<Option<(u64, bool)>, Stop> {
    let bytes = 1u32 << scale;
    if simd {
        match access {
            Access::Store => {
                let v = cpu.v[rt as usize];
                if scale == 4 {
                    write128(cpu, mem, addr, v)?;
                } else {
                    cpu.write(mem, addr, bytes, v as u64)?;
                }
            }
            _ => {
                let v = if scale == 4 {
                    read128(cpu, mem, addr)?
                } else {
                    cpu.read(mem, addr, bytes)? as u128
                };
                cpu.v[rt as usize] = v;
            }
        }
        return Ok(None);
    }
    Ok(match access {
        Access::Store => {
            cpu.write(mem, addr, bytes, cpu.xr(rt, true))?;
            None
        }
        Access::Load => Some((cpu.read(mem, addr, bytes)?, true)),
        Access::LoadSigned64 => Some((sext(cpu.read(mem, addr, bytes)?, 8 * bytes), true)),
        Access::LoadSigned32 => Some((sext(cpu.read(mem, addr, bytes)?, 8 * bytes), false)),
    })
}

/// Decode `size`, `V`, `opc` of the single-register forms into the access
/// scale and kind. `Ok(None)` is a prefetch.
fn single_access(size: u32, simd: bool, opc: u32) -> Result<Option<(u32, Access)>, Stop> {
    if simd {
        let scale = if opc & 2 != 0 {
            if size != 0 {
                return Err(UNDEF);
            }
            4
        } else {
            size
        };
        let access = if opc & 1 != 0 {
            Access::Load
        } else {
            Access::Store
        };
        return Ok(Some((scale, access)));
    }
    let access = match (size, opc) {
        (_, 0) => Access::Store,
        (_, 1) => Access::Load,
        (3, 2) => return Ok(None),
        (3, 3) | (2, 3) => return Err(UNDEF),
        (_, 2) => Access::LoadSigned64,
        (_, _) => Access::LoadSigned32,
    };
    Ok(Some((size, access)))
}

fn ld_literal<M: Memory + ?Sized>(cpu: &mut Cpu, insn: u32, mem: &mut M) -> Exec {
    let opc = field(insn, 30, 2);
    let simd = bit(insn, 26);
    let rt = field(insn, 0, 5);
    let addr = cpu
        .pc
        .wrapping_add(sext(field(insn, 5, 19) as u64, 19) << 2);
    let (scale, access) = match (simd, opc) {
        (false, 0) => (2, Access::Load),
        (false, 1) => (3, Access::Load),
        (false, 2) => (2, Access::LoadSigned64),
        (false, _) => return Ok(()), // PRFM (literal)
        (true, 3) => return undef(),
        (true, o) => (2 + o, Access::Load),
    };
    if let Some((v, sf)) = transfer(cpu, mem, simd, scale, access, rt, addr)? {
        cpu.set_xr(rt, sf, v);
    }
    Ok(())
}

fn ld_st_reg_uimm<M: Memory + ?Sized>(cpu: &mut Cpu, insn: u32, mem: &mut M) -> Exec {
    let size = field(insn, 30, 2);
    let simd = bit(insn, 26);
    let opc = field(insn, 22, 2);
    let rt = field(insn, 0, 5);
    let rn = field(insn, 5, 5);
    let Some((scale, access)) = single_access(size, simd, opc)? else {
        return Ok(()); // PRFM
    };
    let addr = cpu
        .xsp(rn)
        .wrapping_add((field(insn, 10, 12) as u64) << scale);
    if let Some((v, sf)) = transfer(cpu, mem, simd, scale, access, rt, addr)? {
        cpu.set_xr(rt, sf, v);
    }
    Ok(())
}

/// The `bit 21` / `bits 11:10` sub-table: unscaled, post-indexed,
/// unprivileged, pre-indexed and register-offset forms.
fn ld_st_reg<M: Memory + ?Sized>(cpu: &mut Cpu, insn: u32, mem: &mut M) -> Exec {
    let size = field(insn, 30, 2);
    let simd = bit(insn, 26);
    let opc = field(insn, 22, 2);
    let rt = field(insn, 0, 5);
    let rn = field(insn, 5, 5);
    let mode = field(insn, 10, 2);
    let base = cpu.xsp(rn);

    if bit(insn, 21) {
        // Register offset; mode 00 is the v8.1 atomics, x1 is PAC loads.
        if mode != 2 {
            return undef();
        }
        let option = field(insn, 13, 3);
        if option & 2 == 0 {
            return undef();
        }
        let Some((scale, access)) = single_access(size, simd, opc)? else {
            return Ok(());
        };
        let shift = if bit(insn, 12) { scale } else { 0 };
        let off = extend_reg(cpu.xr(field(insn, 16, 5), true), option, shift, true);
        let addr = base.wrapping_add(off);
        if let Some((v, sf)) = transfer(cpu, mem, simd, scale, access, rt, addr)? {
            cpu.set_xr(rt, sf, v);
        }
        return Ok(());
    }

    let imm = sext(field(insn, 12, 9) as u64, 9);
    let Some((scale, access)) = single_access(size, simd, opc)? else {
        // PRFUM is the unscaled form only; prefetch has no index forms.
        return if mode == 0 { Ok(()) } else { undef() };
    };
    let (addr, wb) = match mode {
        0 => (base.wrapping_add(imm), None),
        1 => (base, Some(base.wrapping_add(imm))),
        2 => {
            // LDTR/STTR: EL0 permissions from EL1 (see mmu.rs); no SIMD
            // form exists.
            if simd {
                return undef();
            }
            (base.wrapping_add(imm), None)
        }
        _ => {
            let a = base.wrapping_add(imm);
            (a, Some(a))
        }
    };
    cpu.unprivileged = mode == 2;
    let loaded = transfer(cpu, mem, simd, scale, access, rt, addr);
    cpu.unprivileged = false;
    let loaded = loaded?;
    if let Some(wb) = wb {
        cpu.set_xsp(rn, true, wb);
    }
    if let Some((v, sf)) = loaded {
        cpu.set_xr(rt, sf, v);
    }
    Ok(())
}

fn ld_st_pair<M: Memory + ?Sized>(cpu: &mut Cpu, insn: u32, mem: &mut M) -> Exec {
    let opc = field(insn, 30, 2);
    let simd = bit(insn, 26);
    let mode = field(insn, 23, 2);
    let load = bit(insn, 22);
    let rt = field(insn, 0, 5);
    let rt2 = field(insn, 10, 5);
    let rn = field(insn, 5, 5);
    let (scale, access) = match (simd, opc) {
        (false, 0) => (2, Access::Load),
        // LDPSW; the store slot is STGP (v8.5).
        (false, 1) if load && mode != 0 => (2, Access::LoadSigned64),
        (false, 2) => (3, Access::Load),
        (true, 0..=2) => (2 + opc, Access::Load),
        _ => return undef(),
    };
    let access = if load { access } else { Access::Store };
    let off = sext(field(insn, 15, 7) as u64, 7) << scale;
    let base = cpu.xsp(rn);
    let (addr, wb) = match mode {
        0 | 2 => (base.wrapping_add(off), None),
        1 => (base, Some(base.wrapping_add(off))),
        _ => {
            let a = base.wrapping_add(off);
            (a, Some(a))
        }
    };
    let step = 1u64 << scale;
    let first = transfer(cpu, mem, simd, scale, access, rt, addr)?;
    let second = transfer(cpu, mem, simd, scale, access, rt2, addr.wrapping_add(step))?;
    if let Some(wb) = wb {
        cpu.set_xsp(rn, true, wb);
    }
    if let Some((v, sf)) = first {
        cpu.set_xr(rt, sf, v);
    }
    if let Some((v, sf)) = second {
        cpu.set_xr(rt2, sf, v);
    }
    Ok(())
}

fn ld_st_exclusive<M: Memory + ?Sized>(cpu: &mut Cpu, insn: u32, mem: &mut M) -> Exec {
    let size = field(insn, 30, 2);
    let o2 = bit(insn, 23);
    let load = bit(insn, 22);
    let o1 = bit(insn, 21);
    let o0 = bit(insn, 15);
    let rs = field(insn, 16, 5);
    let rt2 = field(insn, 10, 5);
    let rn = field(insn, 5, 5);
    let rt = field(insn, 0, 5);
    let addr = cpu.xsp(rn);
    let bytes = 1u32 << size;

    // o2 = 1 is load-acquire / store-release (o0 = 0 there is LDLAR/STLLR,
    // v8.1); o1 = 1 with o2 = 0 is the pair forms for 32/64-bit, CASP (v8.1)
    // for the smaller sizes.
    let pair = o1;
    if o2 {
        if o1 || !o0 {
            return undef();
        }
    } else if pair && size < 2 {
        return undef();
    }
    let total = if pair { 2 * bytes } else { bytes };
    // Exclusives and acquire/release demand natural alignment of the whole
    // access regardless of SCTLR.A.
    if addr & (total as u64 - 1) != 0 {
        return Err(Stop::Exception(Exception::DataAbort {
            addr,
            write: !load,
            fsc: super::mmu::FSC_ALIGNMENT,
        }));
    }

    if o2 {
        if load {
            let v = cpu.read(mem, addr, bytes)?;
            cpu.set_xr(rt, true, v);
        } else {
            cpu.write(mem, addr, bytes, cpu.xr(rt, true))?;
        }
        return Ok(());
    }

    if load {
        let v1 = cpu.read(mem, addr, bytes)?;
        let v2 = if pair {
            Some(cpu.read(mem, addr.wrapping_add(bytes as u64), bytes)?)
        } else {
            None
        };
        cpu.exclusive = Some((addr, cpu.last_pa));
        cpu.set_xr(rt, true, v1);
        if let Some(v2) = v2 {
            cpu.set_xr(rt2, true, v2);
        }
    } else {
        let ok = cpu.exclusive.map(|(va, _)| va) == Some(addr);
        if ok {
            cpu.write(mem, addr, bytes, cpu.xr(rt, true))?;
            if pair {
                cpu.write(
                    mem,
                    addr.wrapping_add(bytes as u64),
                    bytes,
                    cpu.xr(rt2, true),
                )?;
            }
        }
        cpu.exclusive = None;
        cpu.set_xr(rs, false, !ok as u64);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Data processing, register (C4.1.89)

fn dp_reg(cpu: &mut Cpu, insn: u32) -> Exec {
    let sf = bit(insn, 31);
    let rd = field(insn, 0, 5);
    let rn = field(insn, 5, 5);
    let rm = field(insn, 16, 5);

    if insn & 0x1F00_0000 == 0x0A00_0000 {
        // Logical (shifted register)
        let imm6 = field(insn, 10, 6);
        if !sf && imm6 >= 32 {
            return undef();
        }
        let mut y = shift_reg(cpu.xr(rm, sf), field(insn, 22, 2), imm6, sf);
        if bit(insn, 21) {
            y = mask(!y, sf);
        }
        let x = cpu.xr(rn, sf);
        let opc = field(insn, 29, 2);
        let r = match opc {
            0 | 3 => x & y,
            1 => x | y,
            _ => x ^ y,
        };
        if opc == 3 {
            cpu.nzcv = logic_flags(r, sf);
        }
        cpu.set_xr(rd, sf, r);
    } else if insn & 0x1F20_0000 == 0x0B00_0000 {
        // Add/subtract (shifted register)
        let kind = field(insn, 22, 2);
        let imm6 = field(insn, 10, 6);
        if kind == 3 || (!sf && imm6 >= 32) {
            return undef();
        }
        let y = shift_reg(cpu.xr(rm, sf), kind, imm6, sf);
        addsub(cpu, insn, sf, cpu.xr(rn, sf), y, false);
    } else if insn & 0x1F20_0000 == 0x0B20_0000 {
        // Add/subtract (extended register)
        let imm3 = field(insn, 10, 3);
        if field(insn, 22, 2) != 0 || imm3 > 4 {
            return undef();
        }
        let y = extend_reg(cpu.xr(rm, true), field(insn, 13, 3), imm3, sf);
        addsub(cpu, insn, sf, mask(cpu.xsp(rn), sf), y, true);
    } else if insn & 0x1FE0_0000 == 0x1A00_0000 {
        // ADC/ADCS/SBC/SBCS; the other opcode2 values are v8.4 flag ops.
        if field(insn, 10, 6) != 0 {
            return undef();
        }
        let y = cpu.xr(rm, sf);
        let y = if bit(insn, 30) { mask(!y, sf) } else { y };
        let (r, nzcv) = add_with_carry(cpu.xr(rn, sf), y, cpu.nzcv & NZCV_C != 0, sf);
        if bit(insn, 29) {
            cpu.nzcv = nzcv;
        }
        cpu.set_xr(rd, sf, r);
    } else if insn & 0x1FE0_0000 == 0x1A40_0000 {
        // CCMN/CCMP (register or immediate)
        if !bit(insn, 29) || bit(insn, 10) || bit(insn, 4) {
            return undef();
        }
        if cpu.cond_holds(field(insn, 12, 4)) {
            let y = if bit(insn, 11) {
                rm as u64
            } else {
                cpu.xr(rm, sf)
            };
            let sub = bit(insn, 30);
            let y = if sub { mask(!y, sf) } else { y };
            cpu.nzcv = add_with_carry(cpu.xr(rn, sf), y, sub, sf).1;
        } else {
            cpu.nzcv = field(insn, 0, 4) << 28;
        }
    } else if insn & 0x1FE0_0000 == 0x1A80_0000 {
        // CSEL/CSINC/CSINV/CSNEG
        let op2 = field(insn, 10, 2);
        if bit(insn, 29) || op2 > 1 {
            return undef();
        }
        let r = if cpu.cond_holds(field(insn, 12, 4)) {
            cpu.xr(rn, sf)
        } else {
            let y = cpu.xr(rm, sf);
            match (bit(insn, 30), op2) {
                (false, 0) => y,
                (false, _) => y.wrapping_add(1),
                (true, 0) => !y,
                (true, _) => y.wrapping_neg(),
            }
        };
        cpu.set_xr(rd, sf, r);
    } else if insn & 0x5FE0_0000 == 0x1AC0_0000 {
        return dp_2src(cpu, insn, sf, rd, rn, rm);
    } else if insn & 0x5FE0_0000 == 0x5AC0_0000 {
        return dp_1src(cpu, insn, sf, rd, rn);
    } else if insn & 0x1F00_0000 == 0x1B00_0000 {
        return dp_3src(cpu, insn, sf, rd, rn, rm);
    } else {
        return undef();
    }
    Ok(())
}

/// The add/subtract tail shared by the shifted and extended forms. `sp`:
/// register 31 is SP for `Rd` when flags are not set (extended form only).
fn addsub(cpu: &mut Cpu, insn: u32, sf: bool, x: u64, y: u64, sp: bool) {
    let sub = bit(insn, 30);
    let set_flags = bit(insn, 29);
    let rd = field(insn, 0, 5);
    let y = if sub { mask(!y, sf) } else { y };
    let (r, nzcv) = add_with_carry(x, y, sub, sf);
    if set_flags {
        cpu.nzcv = nzcv;
        cpu.set_xr(rd, sf, r);
    } else if sp {
        cpu.set_xsp(rd, sf, r);
    } else {
        cpu.set_xr(rd, sf, r);
    }
}

fn dp_2src(cpu: &mut Cpu, insn: u32, sf: bool, rd: u32, rn: u32, rm: u32) -> Exec {
    if bit(insn, 29) {
        return undef();
    }
    let x = cpu.xr(rn, sf);
    let y = cpu.xr(rm, sf);
    let opcode = field(insn, 10, 6);
    let r = match opcode {
        2 => x.checked_div(y).unwrap_or(0),
        3 => {
            if y == 0 {
                0
            } else if sf {
                (x as i64).wrapping_div(y as i64) as u64
            } else {
                (x as i32).wrapping_div(y as i32) as u32 as u64
            }
        }
        8..=11 => shift_reg(x, opcode - 8, (y % datasize(sf) as u64) as u32, sf),
        0x10..=0x17 => {
            let sz = opcode & 3;
            if (sz == 3) != sf {
                return undef();
            }
            crc32(x as u32, cpu.xr(rm, true), 1 << sz, opcode & 4 != 0) as u64
        }
        _ => return undef(),
    };
    cpu.set_xr(rd, sf, r);
    Ok(())
}

fn dp_1src(cpu: &mut Cpu, insn: u32, sf: bool, rd: u32, rn: u32) -> Exec {
    if bit(insn, 29) || field(insn, 16, 5) != 0 {
        return undef();
    }
    let x = cpu.xr(rn, sf);
    let r = match (field(insn, 10, 6), sf) {
        (0, true) => x.reverse_bits(),
        (0, false) => (x as u32).reverse_bits() as u64,
        (1, true) => ((x & 0x00FF_00FF_00FF_00FF) << 8) | ((x >> 8) & 0x00FF_00FF_00FF_00FF),
        (1, false) => {
            let w = x as u32;
            (((w & 0x00FF_00FF) << 8) | ((w >> 8) & 0x00FF_00FF)) as u64
        }
        (2, false) => (x as u32).swap_bytes() as u64,
        (2, true) => {
            let lo = (x as u32).swap_bytes() as u64;
            let hi = ((x >> 32) as u32).swap_bytes() as u64;
            (hi << 32) | lo
        }
        (3, true) => x.swap_bytes(),
        (4, true) => x.leading_zeros() as u64,
        (4, false) => (x as u32).leading_zeros() as u64,
        (5, true) => ((x ^ (x >> 1)) & (u64::MAX >> 1)).leading_zeros() as u64 - 1,
        (5, false) => {
            let w = x as u32;
            (((w ^ (w >> 1)) & (u32::MAX >> 1)).leading_zeros() - 1) as u64
        }
        _ => return undef(),
    };
    cpu.set_xr(rd, sf, r);
    Ok(())
}

fn dp_3src(cpu: &mut Cpu, insn: u32, sf: bool, rd: u32, rn: u32, rm: u32) -> Exec {
    let op31 = field(insn, 21, 3);
    let o0 = bit(insn, 15);
    let ra = field(insn, 10, 5);
    if field(insn, 29, 2) != 0 || (!sf && op31 != 0) {
        return undef();
    }
    let n = cpu.xr(rn, sf);
    let m = cpu.xr(rm, sf);
    let a = cpu.xr(ra, sf);
    let r = match (op31, o0) {
        (0, false) => a.wrapping_add(n.wrapping_mul(m)),
        (0, true) => a.wrapping_sub(n.wrapping_mul(m)),
        (1 | 5, _) => {
            let p = if op31 == 1 {
                (n as i32 as i64).wrapping_mul(m as i32 as i64) as u64
            } else {
                (n as u32 as u64).wrapping_mul(m as u32 as u64)
            };
            if o0 {
                a.wrapping_sub(p)
            } else {
                a.wrapping_add(p)
            }
        }
        // SMULH/UMULH: Ra is (11111). Anything else is CONSTRAINED
        // UNPREDICTABLE; QEMU makes it UNDEFINED and so do we.
        (2 | 6, _) if o0 || ra != 31 => return undef(),
        (2, _) => ((n as i64 as i128 * m as i64 as i128) >> 64) as u64,
        (6, _) => ((n as u128 * m as u128) >> 64) as u64,
        _ => return undef(),
    };
    cpu.set_xr(rd, sf, r);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_masks_match_known_immediates() {
        // and x0, x0, #0xff  => N=1 immr=0 imms=7
        assert_eq!(decode_bit_masks(1, 7, 0, true, 64).unwrap().0, 0xFF);
        // orr w0, wzr, #0xaaaaaaaa => N=0 immr=1 imms=0b111100
        assert_eq!(
            decode_bit_masks(0, 0x3C, 1, true, 32).unwrap().0,
            0xAAAA_AAAA
        );
        // All-ones is reserved for the immediate form.
        assert_eq!(decode_bit_masks(1, 0x3F, 0, true, 64), None);
    }

    #[test]
    fn crc32_matches_the_standard_check_values() {
        // "123456789": CRC-32 = 0xCBF43926, CRC-32C = 0xE3069283, each with
        // the usual ~ before and after that the instruction leaves out.
        let data = b"123456789";
        let run = |c| {
            let mut acc = !0u32;
            for &b in data {
                acc = crc32(acc, b as u64, 1, c);
            }
            !acc
        };
        assert_eq!(run(false), 0xCBF4_3926);
        assert_eq!(run(true), 0xE306_9283);
    }
}
