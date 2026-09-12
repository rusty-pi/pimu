//! Advanced SIMD: the vector and scalar forms, and the structure loads and
//! stores (ARM ARM C4.1.95 and C4.1.88).
//!
//! ARMv8.0 as on the A72, without the Cryptographic Extension: BCM2711 does
//! not implement it (`/proc/cpuinfo` on a Pi 4: `fp asimd evtstrm crc32
//! cpuid`), so AES, SHA and 64-bit PMULL are UNDEFINED here even though
//! QEMU's `cortex-a72` model executes them.
//!
//! The decode table mirrors QEMU's `data_proc_simd` table: order matters,
//! because several classes are subsets of others (modified immediate is
//! shift-by-immediate with `immh == 0`).

use super::cpu::{Cpu, Memory};
use super::exec::{bit, field, undef, Exec};
use super::fp::{self, CmpOp, Fmt, Fp, Rounding};
use super::fpinsn;

/// `FPSR.QC`, the cumulative saturation flag.
const QC: u32 = 1 << 27;
const LO64: u128 = u64::MAX as u128;

#[inline]
fn ones(n: u32) -> u64 {
    if n >= 64 {
        !0
    } else {
        (1 << n) - 1
    }
}

#[inline]
fn elem(v: u128, i: u32, esize: u32) -> u64 {
    (v >> (i * esize)) as u64 & ones(esize)
}

#[inline]
fn set_elem(v: &mut u128, i: u32, esize: u32, x: u64) {
    let m = (ones(esize) as u128) << (i * esize);
    *v = (*v & !m) | (((x & ones(esize)) as u128) << (i * esize));
}

#[inline]
fn sx(x: u64, esize: u32) -> i64 {
    ((x << (64 - esize)) as i64) >> (64 - esize)
}

/// `Int(x, unsigned)`.
#[inline]
fn int(x: u64, esize: u32, unsigned: bool) -> i128 {
    if unsigned {
        (x & ones(esize)) as i128
    } else {
        sx(x, esize) as i128
    }
}

/// `SatQ`: clamp to `esize` bits, and whether that changed the value.
fn sat(v: i128, esize: u32, unsigned: bool) -> (u64, bool) {
    let (lo, hi) = if unsigned {
        (0, (1i128 << esize) - 1)
    } else {
        (-(1i128 << (esize - 1)), (1i128 << (esize - 1)) - 1)
    };
    let c = v.clamp(lo, hi);
    (c as u64 & ones(esize), c != v)
}

fn replicate(x: u64, esize: u32) -> u128 {
    let mut r = 0u128;
    for i in 0..128 / esize {
        set_elem(&mut r, i, esize, x);
    }
    r
}

fn datasize(q: bool) -> u32 {
    if q {
        128
    } else {
        64
    }
}

fn fmt_sz(sz: bool) -> Fmt {
    if sz {
        Fmt::D
    } else {
        Fmt::S
    }
}

/// Write a vector result; a 64-bit one clears the top half.
fn write(cpu: &mut Cpu, d: u32, v: u128, q: bool) {
    cpu.v[d as usize] = if q { v } else { v & LO64 };
}

/// `Vpart[d, part] = v`: the bottom half clears the top, the top half keeps
/// the bottom.
fn write_part(cpu: &mut Cpu, d: u32, v: u64, part: bool) {
    let d = d as usize;
    cpu.v[d] = if part {
        (cpu.v[d] & LO64) | ((v as u128) << 64)
    } else {
        v as u128
    };
}

fn regs(insn: u32) -> (u32, u32, u32) {
    (field(insn, 0, 5), field(insn, 5, 5), field(insn, 16, 5))
}

fn flag_qc(cpu: &mut Cpu, qc: bool) {
    if qc {
        cpu.fpsr |= QC;
    }
}

/// Carry-less multiply of the low `bits` of `x` and `y`.
fn pmul(x: u64, y: u64, bits: u32) -> u128 {
    let mut r = 0u128;
    for i in 0..bits {
        if (y >> i) & 1 != 0 {
            r ^= ((x & ones(bits)) as u128) << i;
        }
    }
    r
}

/// `SQDMULH` / `SQRDMULH` of two signed elements.
fn doubling_mul_high(x: i128, y: i128, esize: u32, round: bool) -> (u64, bool) {
    let rc = if round { 1i128 << (esize - 1) } else { 0 };
    sat((2 * x * y + rc) >> esize, esize, false)
}

/// `SQDMULL` then optionally accumulate (`SQDMLAL` +1 / `SQDMLSL` -1),
/// saturating twice like the pseudocode.
fn doubling_mul_long(x: i128, y: i128, acc: u64, wide: u32, accumulate: i32) -> (u64, bool) {
    let (p, s1) = sat(2 * x * y, wide, false);
    if accumulate == 0 {
        return (p, s1);
    }
    let (a, p) = (int(acc, wide, false), int(p, wide, false));
    let (v, s2) = sat(if accumulate > 0 { a + p } else { a - p }, wide, false);
    (v, s1 | s2)
}

/// `SSHL`, `SRSHL`, `SQSHL`, `SQRSHL` and the unsigned forms: shift by the
/// signed low byte of `y`.
fn shift_reg(
    x: u64,
    y: u64,
    esize: u32,
    unsigned: bool,
    round: bool,
    saturate: bool,
) -> (u64, bool) {
    let shift = y as u8 as i8 as i32;
    let v = int(x, esize, unsigned);
    let res: i128 = if shift >= 0 {
        let s = shift as u32;
        if v == 0 {
            0
        } else if !saturate {
            if s >= esize {
                0
            } else {
                v << s
            }
        } else if s < 64 {
            v << s
        } else if v > 0 {
            i128::MAX
        } else {
            i128::MIN
        }
    } else {
        let s = (-shift) as u32;
        if round {
            if s > 65 {
                0
            } else {
                (v + (1i128 << (s - 1))) >> s
            }
        } else {
            v >> s.min(127)
        }
    };
    if saturate {
        sat(res, esize, unsigned)
    } else {
        (res as u64 & ones(esize), false)
    }
}

pub(super) fn execute(cpu: &mut Cpu, insn: u32) -> Exec {
    if bit(insn, 28) && !bit(insn, 30) {
        return fpinsn::execute(cpu, insn);
    }
    type Handler = fn(&mut Cpu, u32) -> Exec;
    const TABLE: &[(u32, u32, Handler)] = &[
        (0x0e20_0400, 0x9f20_0400, three_same),
        (0x0e20_0000, 0x9f20_0c00, three_diff),
        (0x0e20_0800, 0x9f3e_0c00, two_misc),
        (0x0e30_0800, 0x9f3e_0c00, across),
        (0x0e00_0400, 0x9fe0_8400, copy),
        (0x0f00_0000, 0x9f00_0400, indexed),
        (0x0f00_0400, 0x9ff8_0400, mod_imm),
        (0x0f00_0400, 0x9f80_0400, shift_imm),
        (0x0e00_0000, 0xbf20_8c00, table),
        (0x0e00_0800, 0xbf20_8c00, permute),
        (0x2e00_0000, 0xbf20_8400, ext),
        (0x5e20_0400, 0xdf20_0400, three_same),
        (0x5e20_0000, 0xdf20_0c00, three_diff),
        (0x5e20_0800, 0xdf3e_0c00, two_misc),
        (0x5e30_0800, 0xdf3e_0c00, scalar_pairwise),
        (0x5e00_0400, 0xdfe0_8400, scalar_copy),
        (0x5f00_0000, 0xdf00_0400, indexed),
        (0x5f00_0400, 0xdf80_0400, shift_imm),
    ];
    for &(value, mask, handler) in TABLE {
        if insn & mask == value {
            return handler(cpu, insn);
        }
    }
    undef()
}

// --- Three registers, same type ---------------------------------------------

fn three_same(cpu: &mut Cpu, insn: u32) -> Exec {
    let scalar = bit(insn, 28);
    let q = bit(insn, 30);
    let u = bit(insn, 29);
    let size = field(insn, 22, 2);
    let opcode = field(insn, 11, 5);
    let (rd, rn, rm) = regs(insn);
    if opcode >= 0x18 {
        return three_same_fp(cpu, insn, scalar);
    }
    let (a, b, d) = (cpu.v[rn as usize], cpu.v[rm as usize], cpu.v[rd as usize]);
    if opcode == 3 {
        if scalar {
            return undef();
        }
        let r = match (u, size) {
            (false, 0) => a & b,
            (false, 1) => a & !b,
            (false, 2) => a | b,
            (false, _) => a | !b,
            (true, 0) => a ^ b,
            (true, 1) => b ^ ((b ^ a) & d),
            (true, 2) => d ^ ((d ^ a) & b),
            (true, _) => d ^ ((d ^ a) & !b),
        };
        write(cpu, rd, r, q);
        return Ok(());
    }
    let ok = if scalar {
        match opcode {
            1 | 5 | 9 | 11 => true,
            6 | 7 | 8 | 10 | 0x10 | 0x11 => size == 3,
            0x16 => size == 1 || size == 2,
            _ => false,
        }
    } else {
        let allow64 = matches!(opcode, 1 | 5..=11 | 0x10 | 0x11 | 0x17);
        let valid = match opcode {
            0x13 => !u || size == 0,
            0x16 => size == 1 || size == 2,
            0x17 => !u,
            _ => true,
        };
        valid && (size != 3 || (allow64 && q))
    };
    if !ok {
        return undef();
    }
    let esize = 8 << size;
    let elements = if scalar { 1 } else { datasize(q) / esize };
    let mut qc = false;
    let mut r = 0u128;
    let all = |c: bool| if c { !0 } else { 0 };
    for e in 0..elements {
        let (x, y) = if matches!(opcode, 0x14 | 0x15 | 0x17) {
            // Pairwise over b:a.
            let pick = |i: u32| {
                if i < elements {
                    elem(a, i, esize)
                } else {
                    elem(b, i - elements, esize)
                }
            };
            (pick(2 * e), pick(2 * e + 1))
        } else {
            (elem(a, e, esize), elem(b, e, esize))
        };
        let acc = elem(d, e, esize);
        let (xi, yi) = (int(x, esize, u), int(y, esize, u));
        let mut s = false;
        let v = match opcode {
            0 => ((xi + yi) >> 1) as u64,
            1 => {
                let (v, t) = sat(xi + yi, esize, u);
                s = t;
                v
            }
            2 => ((xi + yi + 1) >> 1) as u64,
            4 => ((xi - yi) >> 1) as u64,
            5 => {
                let (v, t) = sat(xi - yi, esize, u);
                s = t;
                v
            }
            6 => all(xi > yi),
            7 => all(xi >= yi),
            8..=11 => {
                let (v, t) = shift_reg(x, y, esize, u, opcode & 2 != 0, opcode & 1 != 0);
                s = t;
                v
            }
            12 | 0x14 => xi.max(yi) as u64,
            13 | 0x15 => xi.min(yi) as u64,
            14 => (xi - yi).unsigned_abs() as u64,
            15 => acc.wrapping_add((xi - yi).unsigned_abs() as u64),
            0x10 if u => x.wrapping_sub(y),
            0x10 | 0x17 => x.wrapping_add(y),
            0x11 => all(if u { x == y } else { x & y != 0 }),
            0x12 if u => acc.wrapping_sub(x.wrapping_mul(y)),
            0x12 => acc.wrapping_add(x.wrapping_mul(y)),
            0x13 if u => pmul(x, y, 8) as u64,
            0x13 => x.wrapping_mul(y),
            0x16 => {
                let (v, t) =
                    doubling_mul_high(sx(x, esize) as i128, sx(y, esize) as i128, esize, u);
                s = t;
                v
            }
            _ => unreachable!(),
        };
        qc |= s;
        set_elem(&mut r, e, esize, v);
    }
    flag_qc(cpu, qc);
    write(cpu, rd, r, q || scalar);
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FOp {
    MaxNm,
    MinNm,
    Fmla,
    Fmls,
    Add,
    Sub,
    Mulx,
    Mul,
    Cmeq,
    Cmge,
    Cmgt,
    Acge,
    Acgt,
    Max,
    Min,
    Recps,
    Rsqrts,
    Div,
    Abd,
    MaxNmP,
    MinNmP,
    AddP,
    MaxP,
    MinP,
}

fn three_same_fp(cpu: &mut Cpu, insn: u32, scalar: bool) -> Exec {
    use FOp::*;
    let q = bit(insn, 30);
    let u = bit(insn, 29);
    let sz = bit(insn, 22);
    let (rd, rn, rm) = regs(insn);
    if !scalar && sz && !q {
        return undef();
    }
    let op = match (u, bit(insn, 23), field(insn, 11, 5)) {
        (false, false, 0x18) => MaxNm,
        (false, true, 0x18) => MinNm,
        (false, false, 0x19) => Fmla,
        (false, true, 0x19) => Fmls,
        (false, false, 0x1A) => Add,
        (false, true, 0x1A) => Sub,
        (false, false, 0x1B) => Mulx,
        (false, false, 0x1C) => Cmeq,
        (false, false, 0x1E) => Max,
        (false, true, 0x1E) => Min,
        (false, false, 0x1F) => Recps,
        (false, true, 0x1F) => Rsqrts,
        (true, false, 0x18) => MaxNmP,
        (true, true, 0x18) => MinNmP,
        (true, false, 0x1A) => AddP,
        (true, true, 0x1A) => Abd,
        (true, false, 0x1B) => Mul,
        (true, false, 0x1C) => Cmge,
        (true, true, 0x1C) => Cmgt,
        (true, false, 0x1D) => Acge,
        (true, true, 0x1D) => Acgt,
        (true, false, 0x1E) => MaxP,
        (true, true, 0x1E) => MinP,
        (true, false, 0x1F) => Div,
        _ => return undef(),
    };
    if scalar
        && !matches!(
            op,
            Mulx | Cmeq | Cmge | Cmgt | Acge | Acgt | Recps | Rsqrts | Abd
        )
    {
        return undef();
    }
    let fmt = fmt_sz(sz);
    let esize = fmt.bits();
    let sign = 1u64 << (esize - 1);
    let elements = if scalar { 1 } else { datasize(q) / esize };
    let (a, b, d) = (cpu.v[rn as usize], cpu.v[rm as usize], cpu.v[rd as usize]);
    let pairwise = matches!(op, MaxNmP | MinNmP | AddP | MaxP | MinP);
    let mut fp = Fp::new(cpu.fpcr);
    let mut r = 0u128;
    let all = |c: bool| if c { ones(esize) } else { 0 };
    for e in 0..elements {
        let (x, y) = if pairwise {
            let pick = |i: u32| {
                if i < elements {
                    elem(a, i, esize)
                } else {
                    elem(b, i - elements, esize)
                }
            };
            (pick(2 * e), pick(2 * e + 1))
        } else {
            (elem(a, e, esize), elem(b, e, esize))
        };
        let acc = elem(d, e, esize);
        let v = match op {
            MaxNm | MaxNmP => fp.max_min(fmt, x, y, true, true),
            MinNm | MinNmP => fp.max_min(fmt, x, y, false, true),
            Max | MaxP => fp.max_min(fmt, x, y, true, false),
            Min | MinP => fp.max_min(fmt, x, y, false, false),
            Fmla => fp.mul_add(fmt, acc, x, y),
            Fmls => fp.mul_add(fmt, acc, x ^ sign, y),
            Add | AddP => fp.add(fmt, x, y, false),
            Sub => fp.add(fmt, x, y, true),
            Abd => fp.add(fmt, x, y, true) & !sign,
            Mulx => fp.mul(fmt, x, y, true),
            Mul => fp.mul(fmt, x, y, false),
            Cmeq => all(fp.compare_op(fmt, x, y, CmpOp::Eq)),
            Cmge => all(fp.compare_op(fmt, x, y, CmpOp::Ge)),
            Cmgt => all(fp.compare_op(fmt, x, y, CmpOp::Gt)),
            Acge => all(fp.compare_op(fmt, x & !sign, y & !sign, CmpOp::Ge)),
            Acgt => all(fp.compare_op(fmt, x & !sign, y & !sign, CmpOp::Gt)),
            Recps => fp.step_fused(fmt, x, y, false),
            Rsqrts => fp.step_fused(fmt, x, y, true),
            Div => fp.div(fmt, x, y),
        };
        set_elem(&mut r, e, esize, v);
    }
    cpu.fpsr |= fp.flags;
    write(cpu, rd, r, q || scalar);
    Ok(())
}

// --- Three registers, different types ----------------------------------------

fn three_diff(cpu: &mut Cpu, insn: u32) -> Exec {
    let scalar = bit(insn, 28);
    let part = bit(insn, 30) && !scalar;
    let u = bit(insn, 29);
    let size = field(insn, 22, 2);
    let opcode = field(insn, 12, 4);
    let (rd, rn, rm) = regs(insn);
    let ok = size != 3
        && if scalar {
            !u && matches!(opcode, 9 | 11 | 13) && (size == 1 || size == 2)
        } else {
            match opcode {
                9 | 11 | 13 => !u && (size == 1 || size == 2),
                14 => !u && size == 0,
                15 => false,
                _ => true,
            }
        };
    if !ok {
        return undef();
    }
    let esize = 8 << size;
    let wide = 2 * esize;
    let elements = if scalar { 1 } else { 64 / esize };
    let base = if part { elements } else { 0 };
    let (a, b, d) = (cpu.v[rn as usize], cpu.v[rm as usize], cpu.v[rd as usize]);
    let mut r = 0u128;
    if opcode == 4 || opcode == 6 {
        // ADDHN / RADDHN / SUBHN / RSUBHN
        for e in 0..elements {
            let (x, y) = (elem(a, e, wide) as u128, elem(b, e, wide) as u128);
            let rc = if u { 1u128 << (esize - 1) } else { 0 };
            let s = if opcode == 4 {
                x.wrapping_add(y)
            } else {
                x.wrapping_sub(y)
            }
            .wrapping_add(rc)
                & ones(wide) as u128;
            set_elem(&mut r, e, esize, (s >> esize) as u64);
        }
        write_part(cpu, rd, r as u64, part);
        return Ok(());
    }
    let mut qc = false;
    for e in 0..elements {
        let xn = if opcode == 1 || opcode == 3 {
            int(elem(a, e, wide), wide, u)
        } else {
            int(elem(a, base + e, esize), esize, u)
        };
        let ym = elem(b, base + e, esize);
        let yi = int(ym, esize, u);
        let acc = elem(d, e, wide);
        let v = match opcode {
            0 | 1 => (xn + yi) as u64,
            2 | 3 => (xn - yi) as u64,
            5 => acc.wrapping_add((xn - yi).unsigned_abs() as u64),
            7 => (xn - yi).unsigned_abs() as u64,
            8 => acc.wrapping_add((xn * yi) as u64),
            10 => acc.wrapping_sub((xn * yi) as u64),
            12 => (xn * yi) as u64,
            9 | 11 | 13 => {
                let accumulate = match opcode {
                    9 => 1,
                    11 => -1,
                    _ => 0,
                };
                let (v, s) = doubling_mul_long(xn, yi, acc, wide, accumulate);
                qc |= s;
                v
            }
            14 => pmul(elem(a, base + e, esize), ym, 8) as u64,
            _ => unreachable!(),
        };
        set_elem(&mut r, e, wide, v);
    }
    flag_qc(cpu, qc);
    cpu.v[rd as usize] = r;
    Ok(())
}

// --- Two-register miscellaneous ------------------------------------------------

fn clz(x: u64, esize: u32) -> u64 {
    let x = x & ones(esize);
    if x == 0 {
        esize as u64
    } else {
        (x.leading_zeros() - (64 - esize)) as u64
    }
}

fn two_misc(cpu: &mut Cpu, insn: u32) -> Exec {
    let scalar = bit(insn, 28);
    let q = bit(insn, 30);
    let u = bit(insn, 29);
    let size = field(insn, 22, 2);
    let opcode = field(insn, 12, 5);
    if (0x0C..=0x0F).contains(&opcode) || opcode >= 0x16 {
        return two_misc_fp(cpu, insn, scalar);
    }
    let (rd, rn, _) = regs(insn);
    let (a, d) = (cpu.v[rn as usize], cpu.v[rd as usize]);
    let esize = 8 << size;
    let elements = if scalar { 1 } else { datasize(q) / esize };
    let mut qc = false;
    let mut r = 0u128;
    match opcode {
        0 | 1 => {
            let container = match (u, opcode) {
                (false, 0) => 64,
                (true, 0) => 32,
                (false, _) => 16,
                _ => return undef(),
            };
            if scalar || esize >= container {
                return undef();
            }
            let per = container / esize;
            for e in 0..elements {
                let (c, i) = (e / per, e % per);
                set_elem(&mut r, c * per + (per - 1 - i), esize, elem(a, e, esize));
            }
        }
        2 | 6 => {
            if scalar || size == 3 {
                return undef();
            }
            let wide = 2 * esize;
            for e in 0..datasize(q) / wide {
                let s =
                    int(elem(a, 2 * e, esize), esize, u) + int(elem(a, 2 * e + 1, esize), esize, u);
                let acc = if opcode == 6 { elem(d, e, wide) } else { 0 };
                set_elem(&mut r, e, wide, acc.wrapping_add(s as u64));
            }
        }
        3 | 7 => {
            if !scalar && size == 3 && !q {
                return undef();
            }
            for e in 0..elements {
                let x = elem(a, e, esize);
                let v = if opcode == 3 {
                    // SUQADD: unsigned Vn into signed Vd; USQADD the reverse.
                    int(x, esize, !u) + int(elem(d, e, esize), esize, u)
                } else if u {
                    -int(x, esize, false)
                } else {
                    int(x, esize, false).abs()
                };
                let (v, s) = sat(v, esize, opcode == 3 && u);
                qc |= s;
                set_elem(&mut r, e, esize, v);
            }
        }
        4 => {
            if scalar || size == 3 {
                return undef();
            }
            for e in 0..elements {
                let x = elem(a, e, esize);
                let v = if u {
                    clz(x, esize)
                } else {
                    clz((x ^ (x >> 1)) & ones(esize - 1), esize - 1)
                };
                set_elem(&mut r, e, esize, v);
            }
        }
        5 => {
            if scalar {
                return undef();
            }
            r = match (u, size) {
                (false, 0) => {
                    let mut r = 0u128;
                    for e in 0..16 {
                        set_elem(&mut r, e, 8, elem(a, e, 8).count_ones() as u64);
                    }
                    r
                }
                (true, 0) => !a,
                (true, 1) => {
                    let mut r = 0u128;
                    for e in 0..16 {
                        set_elem(&mut r, e, 8, (elem(a, e, 8) as u8).reverse_bits() as u64);
                    }
                    r
                }
                _ => return undef(),
            };
        }
        8..=11 => {
            if (scalar && size != 3) || (!scalar && size == 3 && !q) || (opcode == 10 && u) {
                return undef();
            }
            for e in 0..elements {
                let x = int(elem(a, e, esize), esize, false);
                let all = |c: bool| if c { !0 } else { 0 };
                let v = match (opcode, u) {
                    (8, false) => all(x > 0),
                    (8, true) => all(x >= 0),
                    (9, false) => all(x == 0),
                    (9, true) => all(x <= 0),
                    (10, _) => all(x < 0),
                    (_, false) => x.unsigned_abs() as u64,
                    (_, true) => (-x) as u64,
                };
                set_elem(&mut r, e, esize, v);
            }
        }
        0x12 | 0x14 => {
            if size == 3 || (scalar && opcode == 0x12 && !u) {
                return undef();
            }
            let wide = 2 * esize;
            let n = if scalar { 1 } else { 64 / esize };
            for e in 0..n {
                let x = elem(a, e, wide);
                let v = match (opcode, u) {
                    (0x12, false) => x,
                    (0x12, true) => {
                        let (v, s) = sat(int(x, wide, false), esize, true);
                        qc |= s;
                        v
                    }
                    (_, u) => {
                        let (v, s) = sat(int(x, wide, u), esize, u);
                        qc |= s;
                        v
                    }
                };
                set_elem(&mut r, e, esize, v);
            }
            flag_qc(cpu, qc);
            write_part(cpu, rd, r as u64, q && !scalar);
            return Ok(());
        }
        0x13 => {
            if !u || scalar || size == 3 {
                return undef();
            }
            let n = 64 / esize;
            let base = if q { n } else { 0 };
            for e in 0..n {
                set_elem(&mut r, e, 2 * esize, elem(a, base + e, esize) << esize);
            }
            cpu.v[rd as usize] = r;
            return Ok(());
        }
        _ => return undef(),
    }
    flag_qc(cpu, qc);
    write(cpu, rd, r, q || scalar);
    Ok(())
}

fn two_misc_fp(cpu: &mut Cpu, insn: u32, scalar: bool) -> Exec {
    let q = bit(insn, 30);
    let u = bit(insn, 29);
    let a_bit = bit(insn, 23);
    let sz = bit(insn, 22);
    let opcode = field(insn, 12, 5);
    let (rd, rn, _) = regs(insn);
    let src = cpu.v[rn as usize];
    let mut fp = Fp::new(cpu.fpcr);

    // Narrowing and lengthening conversions use Q as the half selector.
    if opcode == 0x16 || opcode == 0x17 {
        if a_bit {
            return undef();
        }
        let (from, to) = match (opcode, sz) {
            (0x16, false) => (Fmt::S, Fmt::H),
            (0x16, true) => (Fmt::D, Fmt::S),
            (_, false) => (Fmt::H, Fmt::S),
            (_, true) => (Fmt::S, Fmt::D),
        };
        let rounding = if opcode == 0x16 && u {
            // FCVTXN: double to single, round to odd.
            if !sz {
                return undef();
            }
            Rounding::Odd
        } else {
            if u || scalar {
                return undef();
            }
            fp.rounding()
        };
        let (fb, tb) = (from.bits(), to.bits());
        let mut r = 0u128;
        if opcode == 0x16 {
            let n = if scalar { 1 } else { 64 / tb };
            for e in 0..n {
                set_elem(
                    &mut r,
                    e,
                    tb,
                    fp.convert(from, to, elem(src, e, fb), rounding),
                );
            }
            cpu.fpsr |= fp.flags;
            write_part(cpu, rd, r as u64, q && !scalar);
        } else {
            let n = 64 / fb;
            let base = if q { n } else { 0 };
            for e in 0..n {
                set_elem(
                    &mut r,
                    e,
                    tb,
                    fp.convert(from, to, elem(src, base + e, fb), rounding),
                );
            }
            cpu.fpsr |= fp.flags;
            cpu.v[rd as usize] = r;
        }
        return Ok(());
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum M {
        Cmp(CmpOp, bool),
        Abs,
        Neg,
        Rint(Option<Rounding>, bool),
        ToInt(Rounding, bool),
        FromInt(bool),
        Urecpe,
        Ursqrte,
        Recpe,
        Rsqrte,
        Recpx,
        Sqrt,
    }
    use Rounding::*;
    // (u, a, opcode); the Cmp bool swaps operands (compare zero against x).
    let op = match (u, a_bit, opcode) {
        (false, true, 0x0C) => M::Cmp(CmpOp::Gt, false),
        (true, true, 0x0C) => M::Cmp(CmpOp::Ge, false),
        (false, true, 0x0D) => M::Cmp(CmpOp::Eq, false),
        (true, true, 0x0D) => M::Cmp(CmpOp::Ge, true),
        (false, true, 0x0E) => M::Cmp(CmpOp::Gt, true),
        (false, true, 0x0F) => M::Abs,
        (true, true, 0x0F) => M::Neg,
        (false, false, 0x18) => M::Rint(Some(TieEven), false),
        (false, true, 0x18) => M::Rint(Some(PosInf), false),
        (true, false, 0x18) => M::Rint(Some(TieAway), false),
        (false, false, 0x19) => M::Rint(Some(NegInf), false),
        (false, true, 0x19) => M::Rint(Some(Zero), false),
        (true, false, 0x19) => M::Rint(None, true),
        (true, true, 0x19) => M::Rint(None, false),
        (u, false, 0x1A) => M::ToInt(TieEven, u),
        (u, true, 0x1A) => M::ToInt(PosInf, u),
        (u, false, 0x1B) => M::ToInt(NegInf, u),
        (u, true, 0x1B) => M::ToInt(Zero, u),
        (u, false, 0x1C) => M::ToInt(TieAway, u),
        (false, true, 0x1C) => M::Urecpe,
        (true, true, 0x1C) => M::Ursqrte,
        (u, false, 0x1D) => M::FromInt(u),
        (false, true, 0x1D) => M::Recpe,
        (true, true, 0x1D) => M::Rsqrte,
        (false, true, 0x1F) => M::Recpx,
        (true, true, 0x1F) => M::Sqrt,
        _ => return undef(),
    };
    let vector_only = matches!(
        op,
        M::Abs | M::Neg | M::Rint(..) | M::Urecpe | M::Ursqrte | M::Sqrt
    );
    if (scalar && vector_only) || (!scalar && op == M::Recpx) {
        return undef();
    }
    if !scalar && sz && !q {
        return undef();
    }
    if matches!(op, M::Urecpe | M::Ursqrte) && sz {
        return undef();
    }
    let fmt = fmt_sz(sz);
    let esize = fmt.bits();
    let sign = 1u64 << (esize - 1);
    let elements = if scalar { 1 } else { datasize(q) / esize };
    let mut r = 0u128;
    for e in 0..elements {
        let x = elem(src, e, esize);
        let v = match op {
            M::Cmp(c, swap) => {
                let res = if swap {
                    fp.compare_op(fmt, 0, x, c)
                } else {
                    fp.compare_op(fmt, x, 0, c)
                };
                if res {
                    ones(esize)
                } else {
                    0
                }
            }
            M::Abs => x & !sign,
            M::Neg => x ^ sign,
            M::Rint(rounding, exact) => {
                let rounding = rounding.unwrap_or(fp.rounding());
                fp.round_int(fmt, x, rounding, exact)
            }
            M::ToInt(rounding, unsigned) => fp.to_fixed(fmt, x, 0, unsigned, esize, rounding),
            M::FromInt(unsigned) => fp.from_fixed(fmt, x, 0, unsigned, esize),
            M::Urecpe => {
                if x >> 31 == 0 {
                    ones(32)
                } else {
                    ((fp::recip_estimate_int((x >> 23) as u32 & 0x1FF) & 0x1FF) as u64) << 23
                }
            }
            M::Ursqrte => {
                if x >> 30 == 0 {
                    ones(32)
                } else {
                    ((fp::rsqrt_estimate_int((x >> 23) as u32 & 0x1FF) & 0x1FF) as u64) << 23
                }
            }
            M::Recpe => fp.recip_estimate(fmt, x),
            M::Rsqrte => fp.rsqrt_estimate(fmt, x),
            M::Recpx => fp.recpx(fmt, x),
            M::Sqrt => fp.sqrt(fmt, x),
        };
        set_elem(&mut r, e, esize, v);
    }
    cpu.fpsr |= fp.flags;
    write(cpu, rd, r, q || scalar);
    Ok(())
}

// --- Across lanes, copy, pairwise --------------------------------------------

fn across(cpu: &mut Cpu, insn: u32) -> Exec {
    let q = bit(insn, 30);
    let u = bit(insn, 29);
    let size = field(insn, 22, 2);
    let opcode = field(insn, 12, 5);
    let (rd, rn, _) = regs(insn);
    let a = cpu.v[rn as usize];
    if opcode == 0x0C || opcode == 0x0F {
        // FMAXNMV / FMINNMV / FMAXV / FMINV, single precision, four lanes.
        if !u || size & 1 != 0 || !q {
            return undef();
        }
        let (max, num) = (size & 2 == 0, opcode == 0x0C);
        let mut fp = Fp::new(cpu.fpcr);
        let e = |i| elem(a, i, 32);
        let lo = fp.max_min(Fmt::S, e(0), e(1), max, num);
        let hi = fp.max_min(Fmt::S, e(2), e(3), max, num);
        let r = fp.max_min(Fmt::S, lo, hi, max, num);
        cpu.fpsr |= fp.flags;
        cpu.v[rd as usize] = r as u128;
        return Ok(());
    }
    if size == 3 || (size == 2 && !q) {
        return undef();
    }
    let esize = 8 << size;
    let elements = datasize(q) / esize;
    let vals = (0..elements).map(|i| int(elem(a, i, esize), esize, u));
    let (v, bits) = match (opcode, u) {
        (0x03, _) => (vals.sum::<i128>() as u64, 2 * esize),
        (0x0A, _) => (vals.max().unwrap() as u64, esize),
        (0x1A, _) => (vals.min().unwrap() as u64, esize),
        (0x1B, false) => (vals.sum::<i128>() as u64, esize),
        _ => return undef(),
    };
    cpu.v[rd as usize] = (v & ones(bits)) as u128;
    Ok(())
}

/// `imm5` to (element size in bits, index), or `None` if reserved.
fn imm5_elem(imm5: u32) -> Option<(u32, u32)> {
    let size = imm5.trailing_zeros();
    (size <= 3).then(|| (8 << size, imm5 >> (size + 1)))
}

fn copy(cpu: &mut Cpu, insn: u32) -> Exec {
    let q = bit(insn, 30);
    let op = bit(insn, 29);
    let imm4 = field(insn, 11, 4);
    let (rd, rn, _) = regs(insn);
    let Some((esize, index)) = imm5_elem(field(insn, 16, 5)) else {
        return undef();
    };
    if op {
        // INS (element)
        if !q {
            return undef();
        }
        let v = elem(
            cpu.v[rn as usize],
            imm4 >> (esize.trailing_zeros() - 3),
            esize,
        );
        set_elem(&mut cpu.v[rd as usize], index, esize, v);
        return Ok(());
    }
    match imm4 {
        0 | 1 => {
            if esize == 64 && !q {
                return undef();
            }
            let x = if imm4 == 0 {
                elem(cpu.v[rn as usize], index, esize)
            } else {
                cpu.xr(rn, true)
            };
            write(cpu, rd, replicate(x, esize), q);
        }
        3 => {
            if !q {
                return undef();
            }
            let x = cpu.xr(rn, true);
            set_elem(&mut cpu.v[rd as usize], index, esize, x);
        }
        5 => {
            // SMOV
            if esize == 64 || (!q && esize == 32) {
                return undef();
            }
            let x = sx(elem(cpu.v[rn as usize], index, esize), esize) as u64;
            cpu.set_xr(rd, q, x);
        }
        7 => {
            // UMOV
            if (q && esize != 64) || (!q && esize == 64) {
                return undef();
            }
            let x = elem(cpu.v[rn as usize], index, esize);
            cpu.set_xr(rd, q, x);
        }
        _ => return undef(),
    }
    Ok(())
}

fn scalar_copy(cpu: &mut Cpu, insn: u32) -> Exec {
    let (rd, rn, _) = regs(insn);
    if bit(insn, 29) || field(insn, 11, 4) != 0 {
        return undef();
    }
    let Some((esize, index)) = imm5_elem(field(insn, 16, 5)) else {
        return undef();
    };
    cpu.v[rd as usize] = elem(cpu.v[rn as usize], index, esize) as u128;
    Ok(())
}

fn scalar_pairwise(cpu: &mut Cpu, insn: u32) -> Exec {
    let u = bit(insn, 29);
    let size = field(insn, 22, 2);
    let opcode = field(insn, 12, 5);
    let (rd, rn, _) = regs(insn);
    let a = cpu.v[rn as usize];
    if !u {
        if opcode != 0x1B || size != 3 {
            return undef();
        }
        cpu.v[rd as usize] = elem(a, 0, 64).wrapping_add(elem(a, 1, 64)) as u128;
        return Ok(());
    }
    let fmt = fmt_sz(size & 1 != 0);
    let esize = fmt.bits();
    let (x, y) = (elem(a, 0, esize), elem(a, 1, esize));
    let mut fp = Fp::new(cpu.fpcr);
    let v = match (opcode, size & 2 != 0) {
        (0x0C, max_is_min) => fp.max_min(fmt, x, y, !max_is_min, true),
        (0x0D, false) => fp.add(fmt, x, y, false),
        (0x0F, max_is_min) => fp.max_min(fmt, x, y, !max_is_min, false),
        _ => return undef(),
    };
    cpu.fpsr |= fp.flags;
    cpu.v[rd as usize] = v as u128;
    Ok(())
}

// --- By element, immediates, shifts -----------------------------------------

fn indexed(cpu: &mut Cpu, insn: u32) -> Exec {
    let scalar = bit(insn, 28);
    let q = bit(insn, 30);
    let u = bit(insn, 29);
    let size = field(insn, 22, 2);
    let (l, m, h) = (field(insn, 21, 1), field(insn, 20, 1), field(insn, 11, 1));
    let opcode = field(insn, 12, 4);
    let rm4 = field(insn, 16, 4);
    let (rd, rn, _) = regs(insn);
    let (a, d) = (cpu.v[rn as usize], cpu.v[rd as usize]);

    if matches!(opcode, 1 | 5 | 9) {
        if u && opcode != 9 {
            return undef();
        }
        let (fmt, index) = match size {
            2 => (Fmt::S, (h << 1) | l),
            3 => {
                if l == 1 || (!q && !scalar) {
                    return undef();
                }
                (Fmt::D, h)
            }
            _ => return undef(),
        };
        let esize = fmt.bits();
        let sign = 1u64 << (esize - 1);
        let y = elem(cpu.v[((m << 4) | rm4) as usize], index, esize);
        let elements = if scalar { 1 } else { datasize(q) / esize };
        let mut fp = Fp::new(cpu.fpcr);
        let mut r = 0u128;
        for e in 0..elements {
            let (x, acc) = (elem(a, e, esize), elem(d, e, esize));
            let v = match (u, opcode) {
                (false, 1) => fp.mul_add(fmt, acc, x, y),
                (false, 5) => fp.mul_add(fmt, acc, x ^ sign, y),
                (mulx, _) => fp.mul(fmt, x, y, mulx),
            };
            set_elem(&mut r, e, esize, v);
        }
        cpu.fpsr |= fp.flags;
        write(cpu, rd, r, q || scalar);
        return Ok(());
    }

    let (index, rm) = match size {
        1 => ((h << 2) | (l << 1) | m, rm4),
        2 => ((h << 1) | l, (m << 4) | rm4),
        _ => return undef(),
    };
    let valid = if scalar {
        !u && matches!(opcode, 3 | 7 | 11 | 12 | 13)
    } else {
        matches!(
            (u, opcode),
            (false, 2 | 3 | 6 | 7 | 8 | 10 | 11 | 12 | 13) | (true, 0 | 2 | 4 | 6 | 10)
        )
    };
    if !valid {
        return undef();
    }
    let esize = 8 << size;
    let y = elem(cpu.v[rm as usize], index, esize);
    let yi = int(y, esize, u);
    let mut qc = false;
    let mut r = 0u128;
    if matches!(opcode, 2 | 3 | 6 | 7 | 10 | 11) {
        let wide = 2 * esize;
        let n = if scalar { 1 } else { 64 / esize };
        let base = if q && !scalar { n } else { 0 };
        for e in 0..n {
            let xi = int(elem(a, base + e, esize), esize, u);
            let acc = elem(d, e, wide);
            let v = match opcode {
                2 => acc.wrapping_add((xi * yi) as u64),
                6 => acc.wrapping_sub((xi * yi) as u64),
                10 => (xi * yi) as u64,
                _ => {
                    let accumulate = match opcode {
                        3 => 1,
                        7 => -1,
                        _ => 0,
                    };
                    let (v, s) = doubling_mul_long(xi, yi, acc, wide, accumulate);
                    qc |= s;
                    v
                }
            };
            set_elem(&mut r, e, wide, v);
        }
        flag_qc(cpu, qc);
        cpu.v[rd as usize] = r;
        return Ok(());
    }
    let elements = if scalar { 1 } else { datasize(q) / esize };
    for e in 0..elements {
        let (x, acc) = (elem(a, e, esize), elem(d, e, esize));
        let v = match (u, opcode) {
            (true, 0) => acc.wrapping_add(x.wrapping_mul(y)),
            (true, 4) => acc.wrapping_sub(x.wrapping_mul(y)),
            (false, 8) => x.wrapping_mul(y),
            (_, op) => {
                let (v, s) =
                    doubling_mul_high(sx(x, esize) as i128, sx(y, esize) as i128, esize, op == 13);
                qc |= s;
                v
            }
        };
        set_elem(&mut r, e, esize, v);
    }
    flag_qc(cpu, qc);
    write(cpu, rd, r, q || scalar);
    Ok(())
}

/// `AdvSIMDExpandImm`.
fn expand_imm(op: bool, cmode: u32, imm8: u32) -> u64 {
    let i = imm8 as u64;
    let rep = |v: u64, esize: u32| replicate(v, esize) as u64;
    match cmode >> 1 {
        0..=3 => rep(i << (8 * (cmode >> 1)), 32),
        4 | 5 => rep(i << (8 * ((cmode >> 1) & 1)), 16),
        6 => {
            if cmode & 1 == 0 {
                rep((i << 8) | 0xFF, 32)
            } else {
                rep((i << 16) | 0xFFFF, 32)
            }
        }
        _ => match (cmode & 1, op) {
            (0, false) => rep(i, 8),
            (0, true) => (0..8)
                .filter(|b| (i >> b) & 1 != 0)
                .fold(0, |acc, b| acc | (0xFF << (8 * b))),
            (_, false) => rep(fpinsn::expand_imm(Fmt::S, imm8), 32),
            (_, true) => fpinsn::expand_imm(Fmt::D, imm8),
        },
    }
}

fn mod_imm(cpu: &mut Cpu, insn: u32) -> Exec {
    let q = bit(insn, 30);
    let op = bit(insn, 29);
    let cmode = field(insn, 12, 4);
    let rd = field(insn, 0, 5);
    if bit(insn, 11) || (cmode == 15 && op && !q) {
        return undef();
    }
    let imm8 = (field(insn, 16, 3) << 5) | field(insn, 5, 5);
    let imm = expand_imm(op, cmode, imm8) as u128;
    let imm = (imm << 64) | imm;
    let d = cpu.v[rd as usize];
    let orr_bic = cmode & 1 == 1 && cmode < 12;
    let r = match (orr_bic, op) {
        (true, false) => d | imm,
        (true, true) => d & !imm,
        (false, true) if cmode < 14 => !imm,
        _ => imm,
    };
    write(cpu, rd, r, q);
    Ok(())
}

fn shift_imm(cpu: &mut Cpu, insn: u32) -> Exec {
    let scalar = bit(insn, 28);
    let q = bit(insn, 30);
    let u = bit(insn, 29);
    let immh = field(insn, 19, 4);
    let immhb = field(insn, 16, 7);
    let opcode = field(insn, 11, 5);
    let (rd, rn, _) = regs(insn);
    if immh == 0 {
        return undef();
    }
    let esize = 8u32 << (31 - immh.leading_zeros());
    let narrow = (16..=19).contains(&opcode);
    let long = opcode == 20;
    let valid = match opcode {
        0 | 2 | 4 | 6 | 10 | 14 | 18 | 19 | 28 | 31 => true,
        8 | 12 => u,
        16 | 17 => !scalar || u,
        20 => !scalar,
        _ => false,
    };
    if !valid
        || ((narrow || long) && esize == 64)
        || (scalar && matches!(opcode, 0 | 2 | 4 | 6 | 8 | 10) && esize != 64)
        || (!scalar && !narrow && !long && esize == 64 && !q)
    {
        return undef();
    }
    let (a, d) = (cpu.v[rn as usize], cpu.v[rd as usize]);
    let right = 2 * esize - immhb;
    let left = immhb.wrapping_sub(esize);
    let mut qc = false;
    let mut r = 0u128;

    if opcode == 28 || opcode == 31 {
        // SCVTF / UCVTF / FCVTZS / FCVTZU (fixed-point)
        if esize < 32 {
            return undef();
        }
        let fmt = if esize == 64 { Fmt::D } else { Fmt::S };
        let elements = if scalar { 1 } else { datasize(q) / esize };
        let mut fp = Fp::new(cpu.fpcr);
        for e in 0..elements {
            let x = elem(a, e, esize);
            let v = if opcode == 28 {
                fp.from_fixed(fmt, x, right, u, esize)
            } else {
                fp.to_fixed(fmt, x, right, u, esize, Rounding::Zero)
            };
            set_elem(&mut r, e, esize, v);
        }
        cpu.fpsr |= fp.flags;
        write(cpu, rd, r, q || scalar);
        return Ok(());
    }

    if narrow {
        let wide = 2 * esize;
        let n = if scalar { 1 } else { 64 / esize };
        let round = opcode & 1 != 0;
        let (src_unsigned, dst_unsigned, saturate) = match (opcode, u) {
            (16 | 17, false) => (true, true, false),
            (16 | 17, true) => (false, true, true),
            (_, false) => (false, false, true),
            (_, true) => (true, true, true),
        };
        for e in 0..n {
            let xi = int(elem(a, e, wide), wide, src_unsigned);
            let rc = if round { 1i128 << (right - 1) } else { 0 };
            let v = (xi + rc) >> right;
            let out = if saturate {
                let (o, s) = sat(v, esize, dst_unsigned);
                qc |= s;
                o
            } else {
                v as u64
            };
            set_elem(&mut r, e, esize, out);
        }
        flag_qc(cpu, qc);
        write_part(cpu, rd, r as u64, q && !scalar);
        return Ok(());
    }

    if long {
        let n = 64 / esize;
        let base = if q { n } else { 0 };
        for e in 0..n {
            let v = int(elem(a, base + e, esize), esize, u) << left;
            set_elem(&mut r, e, 2 * esize, v as u64);
        }
        cpu.v[rd as usize] = r;
        return Ok(());
    }

    let elements = if scalar { 1 } else { datasize(q) / esize };
    for e in 0..elements {
        let (x, acc) = (elem(a, e, esize), elem(d, e, esize));
        let v = match opcode {
            0 | 2 | 4 | 6 => {
                let rc = if opcode & 4 != 0 {
                    1i128 << (right - 1)
                } else {
                    0
                };
                let s = ((int(x, esize, u) + rc) >> right) as u64;
                if opcode & 2 != 0 {
                    acc.wrapping_add(s)
                } else {
                    s
                }
            }
            8 => {
                let mask = ones(esize).checked_shr(right).unwrap_or(0);
                (acc & !mask) | (x.checked_shr(right).unwrap_or(0) & mask)
            }
            10 if u => {
                let mask = ones(esize) << left;
                (acc & !mask) | ((x << left) & mask)
            }
            10 => x << left,
            _ => {
                // SQSHLU (12) and SQSHL / UQSHL (14).
                let signed_src = opcode == 12 || !u;
                let (v, s) = sat(int(x, esize, !signed_src) << left, esize, opcode == 12 || u);
                qc |= s;
                v
            }
        };
        set_elem(&mut r, e, esize, v);
    }
    flag_qc(cpu, qc);
    write(cpu, rd, r, q || scalar);
    Ok(())
}

// --- Permutes -------------------------------------------------------------------

fn table(cpu: &mut Cpu, insn: u32) -> Exec {
    if field(insn, 22, 2) != 0 {
        return undef();
    }
    let q = bit(insn, 30);
    let (rd, rn, rm) = regs(insn);
    let regs = field(insn, 13, 2) + 1;
    let tbx = bit(insn, 12);
    let (idx, d) = (cpu.v[rm as usize], cpu.v[rd as usize]);
    let mut r = 0u128;
    for i in 0..datasize(q) / 8 {
        let ix = elem(idx, i, 8) as u32;
        let v = if ix < 16 * regs {
            elem(cpu.v[((rn + ix / 16) % 32) as usize], ix % 16, 8)
        } else if tbx {
            elem(d, i, 8)
        } else {
            0
        };
        set_elem(&mut r, i, 8, v);
    }
    write(cpu, rd, r, q);
    Ok(())
}

fn permute(cpu: &mut Cpu, insn: u32) -> Exec {
    let q = bit(insn, 30);
    let size = field(insn, 22, 2);
    let opcode = field(insn, 12, 3);
    let (rd, rn, rm) = regs(insn);
    if opcode & 3 == 0 || (size == 3 && !q) {
        return undef();
    }
    let esize = 8 << size;
    let elements = datasize(q) / esize;
    let pairs = elements / 2;
    let part = opcode >> 2;
    let (a, b) = (cpu.v[rn as usize], cpu.v[rm as usize]);
    let mut r = 0u128;
    match opcode & 3 {
        1 => {
            for e in 0..elements {
                let i = 2 * e + part;
                let v = if i < elements {
                    elem(a, i, esize)
                } else {
                    elem(b, i - elements, esize)
                };
                set_elem(&mut r, e, esize, v);
            }
        }
        2 => {
            for p in 0..pairs {
                set_elem(&mut r, 2 * p, esize, elem(a, 2 * p + part, esize));
                set_elem(&mut r, 2 * p + 1, esize, elem(b, 2 * p + part, esize));
            }
        }
        _ => {
            let base = part * pairs;
            for p in 0..pairs {
                set_elem(&mut r, 2 * p, esize, elem(a, base + p, esize));
                set_elem(&mut r, 2 * p + 1, esize, elem(b, base + p, esize));
            }
        }
    }
    write(cpu, rd, r, q);
    Ok(())
}

fn ext(cpu: &mut Cpu, insn: u32) -> Exec {
    let q = bit(insn, 30);
    let imm4 = field(insn, 11, 4);
    let (rd, rn, rm) = regs(insn);
    if field(insn, 22, 2) != 0 || (!q && imm4 >= 8) {
        return undef();
    }
    let bytes = datasize(q) / 8;
    let (a, b) = (cpu.v[rn as usize], cpu.v[rm as usize]);
    let mut r = 0u128;
    for i in 0..bytes {
        let j = i + imm4;
        let v = if j < bytes {
            elem(a, j, 8)
        } else {
            elem(b, j - bytes, 8)
        };
        set_elem(&mut r, i, 8, v);
    }
    write(cpu, rd, r, q);
    Ok(())
}

// --- Structure loads and stores ------------------------------------------------

/// `LD1`-`LD4` / `ST1`-`ST4` (multiple and single structures), `LD1R`-`LD4R`.
pub(super) fn ldst_structures(cpu: &mut Cpu, insn: u32, mem: &mut dyn Memory) -> Exec {
    let q = bit(insn, 30);
    let single = bit(insn, 24);
    let post = bit(insn, 23);
    let load = bit(insn, 22);
    let (rt, rn, rm) = regs(insn);
    let size = field(insn, 10, 2);
    if !post && field(insn, 16, 5) != 0 || (!single && bit(insn, 21)) {
        return undef();
    }
    let base = cpu.xsp(rn);
    let mut vals = [0u128; 4];
    for (k, v) in vals.iter_mut().enumerate() {
        *v = cpu.v[(rt as usize + k) % 32];
    }
    let mut offs = 0u64;
    let (nregs, full) = if !single {
        let (rpt, selem) = match field(insn, 12, 4) {
            0 => (1, 4),
            2 => (4, 1),
            4 => (1, 3),
            6 => (3, 1),
            7 => (1, 1),
            8 => (1, 2),
            10 => (2, 1),
            _ => return undef(),
        };
        if size == 3 && !q && selem != 1 {
            return undef();
        }
        let esize = 8 << size;
        let ebytes = esize / 8;
        for r in 0..rpt {
            for e in 0..datasize(q) / esize {
                for s in 0..selem {
                    let k = (r + s) as usize;
                    let addr = base.wrapping_add(offs);
                    if load {
                        let v = cpu.read(mem, addr, ebytes)?;
                        set_elem(&mut vals[k], e, esize, v);
                    } else {
                        cpu.write(mem, addr, ebytes, elem(vals[k], e, esize))?;
                    }
                    offs += ebytes as u64;
                }
            }
        }
        (rpt * selem, q)
    } else {
        let opc3 = field(insn, 13, 3);
        let s_bit = field(insn, 12, 1);
        let mut scale = opc3 >> 1;
        let selem = (((opc3 & 1) << 1) | field(insn, 21, 1)) + 1;
        let qb = q as u32;
        let mut index = 0;
        let mut replicate_ = false;
        match scale {
            3 => {
                if !load || s_bit == 1 {
                    return undef();
                }
                scale = size;
                replicate_ = true;
            }
            0 => index = (qb << 3) | (s_bit << 2) | size,
            1 => {
                if size & 1 != 0 {
                    return undef();
                }
                index = (qb << 2) | (s_bit << 1) | (size >> 1);
            }
            _ => {
                if size & 2 != 0 {
                    return undef();
                }
                if size & 1 == 0 {
                    index = (qb << 1) | s_bit;
                } else {
                    if s_bit == 1 {
                        return undef();
                    }
                    index = qb;
                    scale = 3;
                }
            }
        }
        let esize = 8 << scale;
        let ebytes = esize / 8;
        for v in vals.iter_mut().take(selem as usize) {
            let addr = base.wrapping_add(offs);
            if replicate_ {
                *v = replicate(cpu.read(mem, addr, ebytes)?, esize);
            } else if load {
                set_elem(v, index, esize, cpu.read(mem, addr, ebytes)?);
            } else {
                cpu.write(mem, addr, ebytes, elem(*v, index, esize))?;
            }
            offs += ebytes as u64;
        }
        (selem, !replicate_ || q)
    };
    if load {
        for (k, v) in vals.iter().enumerate().take(nregs as usize) {
            cpu.v[(rt as usize + k) % 32] = if full { *v } else { v & LO64 };
        }
    }
    if post {
        let off = if rm == 31 { offs } else { cpu.xr(rm, true) };
        cpu.set_xsp(rn, true, base.wrapping_add(off));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::cpu::{Abort, Exception, Memory, Step};
    use super::*;

    struct NoMem;
    impl Memory for NoMem {
        fn read(&mut self, addr: u64, _: u32) -> Result<u64, Abort> {
            Err(Abort { addr, write: false })
        }
        fn write(&mut self, addr: u64, _: u32, _: u64) -> Result<(), Abort> {
            Err(Abort { addr, write: true })
        }
    }

    fn run(insn: u32) -> Step {
        struct One(u32);
        impl Memory for One {
            fn read(&mut self, _: u64, _: u32) -> Result<u64, Abort> {
                Ok(self.0 as u64)
            }
            fn write(&mut self, addr: u64, _: u32, _: u64) -> Result<(), Abort> {
                NoMem.write(addr, 0, 0)
            }
        }
        Cpu::new_el0().step(&mut One(insn))
    }

    /// The Pi's A72 has no Cryptographic Extension, unlike QEMU's model, so
    /// the differential test cannot check these.
    #[test]
    fn crypto_is_undefined() {
        for insn in [
            0x4e28_4820, // aese v0.16b, v1.16b
            0x4e28_6820, // aesmc v0.16b, v1.16b
            0x5e02_0020, // sha1c q0, s1, v2.4s
            0x5e28_0820, // sha1h s0, s1
            0x0ee2_e020, // pmull v0.1q, v1.1d, v2.1d
        ] {
            assert_eq!(
                run(insn),
                Step::Exception(Exception::Undefined),
                "{insn:#x}"
            );
        }
        // pmull v0.8h, v1.8b, v2.8b is base AdvSIMD.
        assert_eq!(run(0x0e22_e020), Step::Retired);
    }
}
