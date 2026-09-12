//! Scalar floating-point data processing (ARM ARM C4.1.96, "Floating-point
//! data-processing" and the conversions to and from integers).
//!
//! Single and double precision; half precision exists here only as a
//! `FCVT` source or destination, the arithmetic forms being FEAT_FP16
//! (v8.2), which the A72 does not have.

use super::cpu::Cpu;
use super::exec::{bit, field, undef, Exec};
use super::fp::{Fmt, Fp, Rounding};

/// `ftype` 00 = single, 01 = double; 11 (half) only where the caller allows.
fn fmt_of(ftype: u32) -> Option<Fmt> {
    match ftype {
        0 => Some(Fmt::S),
        1 => Some(Fmt::D),
        _ => None,
    }
}

fn mask(fmt: Fmt) -> u64 {
    u64::MAX >> (64 - fmt.bits())
}

fn sign_bit(fmt: Fmt) -> u64 {
    1 << (fmt.bits() - 1)
}

/// `VFPExpandImm`.
pub(super) fn expand_imm(fmt: Fmt, imm8: u32) -> u64 {
    let e = fmt.e();
    let f = fmt.f();
    let sign = (imm8 >> 7) as u64 & 1;
    let b6 = (imm8 >> 6) as u64 & 1;
    let exp = ((b6 ^ 1) << (e - 1))
        | (if b6 != 0 { (1 << (e - 3)) - 1 } else { 0 } << 2)
        | ((imm8 >> 4) as u64 & 3);
    let frac = ((imm8 & 0xF) as u64) << (f - 4);
    (sign << (e + f)) | (exp << f) | frac
}

pub(super) fn execute(cpu: &mut Cpu, insn: u32) -> Exec {
    if bit(insn, 29) {
        return undef();
    }
    let ftype = field(insn, 22, 2);
    let rd = field(insn, 0, 5) as usize;
    let rn = field(insn, 5, 5) as usize;
    let rm = field(insn, 16, 5) as usize;
    if !bit(insn, 21) && !bit(insn, 24) {
        return fixed_conv(cpu, insn, ftype, rd, rn);
    }
    if !bit(insn, 24) && field(insn, 10, 6) == 0 {
        return int_conv(cpu, insn, ftype, rd, rn);
    }
    // Everything else has M (bit 31) = 0.
    if bit(insn, 31) {
        return undef();
    }
    if !bit(insn, 24) && field(insn, 10, 5) == 0b10000 && ftype == 3 {
        // FCVT from half is the only half-precision 1-source form.
        return one_source(cpu, insn, ftype, rd, rn);
    }
    let Some(fmt) = fmt_of(ftype) else {
        return undef();
    };
    let a = cpu.v[rn] as u64 & mask(fmt);
    let b = cpu.v[rm] as u64 & mask(fmt);
    let mut fp = Fp::new(cpu.fpcr);
    let result = if bit(insn, 24) {
        // FMADD / FMSUB / FNMADD / FNMSUB
        let c = cpu.v[field(insn, 10, 5) as usize] as u64 & mask(fmt);
        let (neg_c, neg_a) = (bit(insn, 21), bit(insn, 15) != bit(insn, 21));
        let c = if neg_c { c ^ sign_bit(fmt) } else { c };
        let a = if neg_a { a ^ sign_bit(fmt) } else { a };
        fp.mul_add(fmt, c, a, b)
    } else {
        match field(insn, 10, 2) {
            1 => {
                // FCCMP / FCCMPE
                cpu.nzcv = if cpu.cond_holds(field(insn, 12, 4)) {
                    fp.compare(fmt, a, b, bit(insn, 4))
                } else {
                    field(insn, 0, 4) << 28
                };
                cpu.fpsr |= fp.flags;
                return Ok(());
            }
            2 => match field(insn, 12, 4) {
                0 => fp.mul(fmt, a, b, false),
                1 => fp.div(fmt, a, b),
                2 => fp.add(fmt, a, b, false),
                3 => fp.add(fmt, a, b, true),
                4 => fp.max_min(fmt, a, b, true, false),
                5 => fp.max_min(fmt, a, b, false, false),
                6 => fp.max_min(fmt, a, b, true, true),
                7 => fp.max_min(fmt, a, b, false, true),
                8 => fp.mul(fmt, a, b, false) ^ sign_bit(fmt),
                _ => return undef(),
            },
            3 => {
                // FCSEL
                if cpu.cond_holds(field(insn, 12, 4)) {
                    a
                } else {
                    b
                }
            }
            _ => {
                let low = field(insn, 12, 4);
                if low & 1 != 0 {
                    // FMOV (immediate)
                    if field(insn, 5, 5) != 0 {
                        return undef();
                    }
                    expand_imm(fmt, field(insn, 13, 8))
                } else if low & 2 != 0 {
                    // FCMP / FCMPE
                    let opc2 = field(insn, 0, 5);
                    if field(insn, 14, 2) != 0 || opc2 & 7 != 0 {
                        return undef();
                    }
                    let b = if opc2 & 8 != 0 { 0 } else { b };
                    cpu.nzcv = fp.compare(fmt, a, b, opc2 & 16 != 0);
                    cpu.fpsr |= fp.flags;
                    return Ok(());
                } else if low & 4 != 0 {
                    return one_source(cpu, insn, ftype, rd, rn);
                } else {
                    return undef();
                }
            }
        }
    };
    cpu.fpsr |= fp.flags;
    cpu.v[rd] = result as u128;
    Ok(())
}

fn one_source(cpu: &mut Cpu, insn: u32, ftype: u32, rd: usize, rn: usize) -> Exec {
    let opcode = field(insn, 15, 6);
    let mut fp = Fp::new(cpu.fpcr);
    if (4..8).contains(&opcode) {
        // FCVT between precisions.
        let fmt = |t| match t {
            0 => Some(Fmt::S),
            1 => Some(Fmt::D),
            3 => Some(Fmt::H),
            _ => None,
        };
        let (Some(from), Some(to)) = (fmt(ftype), fmt(opcode & 3)) else {
            return undef();
        };
        if from == to {
            return undef();
        }
        let a = cpu.v[rn] as u64 & mask(from);
        let r = fp.convert(from, to, a, fp.rounding());
        cpu.fpsr |= fp.flags;
        cpu.v[rd] = r as u128;
        return Ok(());
    }
    let Some(fmt) = fmt_of(ftype) else {
        return undef();
    };
    let a = cpu.v[rn] as u64 & mask(fmt);
    let r = match opcode {
        0 => a,
        1 => a & !sign_bit(fmt),
        2 => a ^ sign_bit(fmt),
        3 => fp.sqrt(fmt, a),
        8..=12 | 14 | 15 => {
            let rounding = match opcode {
                8 => Rounding::TieEven,
                9 => Rounding::PosInf,
                10 => Rounding::NegInf,
                11 => Rounding::Zero,
                12 => Rounding::TieAway,
                _ => fp.rounding(),
            };
            fp.round_int(fmt, a, rounding, opcode == 14)
        }
        _ => return undef(),
    };
    cpu.fpsr |= fp.flags;
    cpu.v[rd] = r as u128;
    Ok(())
}

/// Conversions between floating-point and fixed-point.
fn fixed_conv(cpu: &mut Cpu, insn: u32, ftype: u32, rd: usize, rn: usize) -> Exec {
    let sf = bit(insn, 31);
    let scale = field(insn, 10, 6);
    let Some(fmt) = fmt_of(ftype) else {
        return undef();
    };
    if !sf && scale < 32 {
        return undef();
    }
    let fbits = 64 - scale;
    let bits = if sf { 64 } else { 32 };
    let mut fp = Fp::new(cpu.fpcr);
    match (field(insn, 19, 2), field(insn, 16, 3)) {
        (0, op @ (2 | 3)) => {
            let v = cpu.xr(rn as u32, sf);
            let r = fp.from_fixed(fmt, v, fbits, op == 3, bits);
            cpu.v[rd] = r as u128;
        }
        (3, op @ (0 | 1)) => {
            let a = cpu.v[rn] as u64 & mask(fmt);
            let r = fp.to_fixed(fmt, a, fbits, op == 1, bits, Rounding::Zero);
            cpu.set_xr(rd as u32, sf, r);
        }
        _ => return undef(),
    }
    cpu.fpsr |= fp.flags;
    Ok(())
}

/// Conversions between floating-point and integer, and `FMOV` to and from
/// general registers.
fn int_conv(cpu: &mut Cpu, insn: u32, ftype: u32, rd: usize, rn: usize) -> Exec {
    if bit(insn, 29) {
        return undef();
    }
    let sf = bit(insn, 31);
    let bits = if sf { 64 } else { 32 };
    let rmode = field(insn, 19, 2);
    let opcode = field(insn, 16, 3);
    let mut fp = Fp::new(cpu.fpcr);
    match (opcode, rmode) {
        (6, 0) | (7, 0) | (6, 1) | (7, 1) => {
            // FMOV: W<->S, X<->D, X<->V.D[1].
            let top = match (sf, ftype, rmode) {
                (false, 0, 0) | (true, 1, 0) => false,
                (true, 2, 1) => true,
                _ => return undef(),
            };
            if opcode == 6 {
                let v = if top {
                    (cpu.v[rn] >> 64) as u64
                } else {
                    cpu.v[rn] as u64
                };
                cpu.set_xr(rd as u32, sf, v);
            } else {
                let v = cpu.xr(rn as u32, sf);
                cpu.v[rd] = if top {
                    (cpu.v[rd] & u64::MAX as u128) | ((v as u128) << 64)
                } else {
                    v as u128
                };
            }
            return Ok(());
        }
        _ => {}
    }
    let Some(fmt) = fmt_of(ftype) else {
        return undef();
    };
    match (opcode, rmode) {
        (0 | 1, _) | (4 | 5, 0) => {
            let rounding = if opcode >= 4 {
                Rounding::TieAway
            } else {
                Rounding::from_rmode(rmode)
            };
            let a = cpu.v[rn] as u64 & mask(fmt);
            let r = fp.to_fixed(fmt, a, 0, opcode & 1 != 0, bits, rounding);
            cpu.set_xr(rd as u32, sf, r);
        }
        (2 | 3, 0) => {
            let v = cpu.xr(rn as u32, sf);
            cpu.v[rd] = fp.from_fixed(fmt, v, 0, opcode == 3, bits) as u128;
        }
        _ => return undef(),
    }
    cpu.fpsr |= fp.flags;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmov_immediates() {
        // fmov s0, #1.0 (imm8 0x70), #-2.0 (0x80), #0.125 (0x40)
        assert_eq!(expand_imm(Fmt::S, 0x70), 1f32.to_bits() as u64);
        assert_eq!(expand_imm(Fmt::S, 0x80), (-2f32).to_bits() as u64);
        assert_eq!(expand_imm(Fmt::D, 0x40), 0.125f64.to_bits());
    }
}
