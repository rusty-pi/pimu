//! Instruction decoder for the VideoCore IV scalar VPU.
//!
//! Bit patterns are transcribed from Herman Hermitage's `videocoreiv.arch` and
//! the `vciv.py` IDA processor module (both in `hermanhermitage/videocoreiv`),
//! cross-checked against a sweep of real `start4.elf`. Coverage is the scalar
//! integer ISA; the vector unit (0xF000+) is decoded to length only.

use super::insn::{AddrMode, AluOp, Base, FpOp, Insn, MemWidth, Op, RegOrImm, Writeback};
use super::length::{insn_class, insn_len_bytes, InsnClass};
use super::reg::Cond;

#[inline]
fn sext(value: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((value << shift) as i32) >> shift
}

#[inline]
fn parcel(bytes: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]])
}

/// Decode one instruction. `bytes` must hold at least `insn_len_bytes(parcel0)`
/// bytes; `pc` is the address of this instruction (needed for pc-relative forms).
pub fn decode(bytes: &[u8], pc: u32) -> Insn {
    let p0 = parcel(bytes, 0);
    let len = insn_len_bytes(p0);
    let class = insn_class(p0);

    if (bytes.len() as u8) < len {
        return Insn {
            op: Op::Unimpl {
                raw: p0 as u64,
                len,
                class,
            },
            len,
        };
    }

    let op = match class {
        InsnClass::Scalar16 => decode16(p0, pc),
        InsnClass::Scalar32 => decode32(p0, parcel(bytes, 1), pc),
        InsnClass::Scalar48 => decode48(bytes, pc),
        _ => {
            let mut raw = 0u64;
            for i in 0..(len as usize / 2) {
                raw = (raw << 16) | parcel(bytes, i) as u64;
            }
            Op::Unimpl { raw, len, class }
        }
    };

    Insn { op, len }
}

fn ldst(store: bool, w: MemWidth, rd: u8, addr: AddrMode, cond: Cond) -> Op {
    if store {
        Op::Store { w, rd, addr, cond }
    } else {
        Op::Load { w, rd, addr, cond }
    }
}

fn decode16(p0: u16, pc: u32) -> Op {
    let p = p0 as u32;
    let rd4 = (p & 0xF) as u8;
    let rd5 = (p & 0x1F) as u8;

    // Fixed system encodings 0x0000..=0x000A.
    match p0 {
        0x0000 => return Op::Bkpt,
        0x0001 => return Op::Nop,
        0x0002 => return Op::Sleep,
        0x0004 => return Op::SetIrqEnable(true), // ei
        0x0005 => return Op::SetIrqEnable(false), // di
        0x0003 | 0x0006..=0x0009 => return Op::Nop, // user / cbclr / cbadd{1,2,3}
        0x000A => return Op::Rti,
        _ => {}
    }

    // 0000 0000 001d dddd : swi rd
    if p & 0xFFE0 == 0x0020 {
        return Op::Swi {
            vector: 0x20 + (p & 0x1F),
        };
    }
    // 0000 0000 010d dddd : b rd     /     011d dddd : bl rd
    if p & 0xFFC0 == 0x0040 {
        return Op::BranchReg {
            link: p & 0x20 != 0,
            rd: rd5,
        };
    }
    // 0000 0000 100d dddd : switch.b rd   /   101d dddd : switch rd
    if p & 0xFFC0 == 0x0080 {
        return Op::Switch {
            rd: rd5,
            byte: p & 0x20 == 0,
        };
    }
    // 0000 0000 111d dddd : version rd
    if p & 0xFFE0 == 0x00E0 {
        return Op::Version { rd: rd5 };
    }
    // 0000 0001 11uu uuuu : swi #u
    if p & 0xFFC0 == 0x01C0 {
        return Op::Swi {
            vector: 0x20 + (p & 0x3F),
        };
    }
    // 0000 001X Ybb nnnnn : ldm/stm    (0x0200 ldm, 0x0280 stm, 0x0300 ldm+pc,
    //                                   0x0380 stm+lr; bb=bank, n=width-1)
    if (0x0200..0x0400).contains(&p) {
        // Per Hermitage's `videocoreiv.arch`:
        //   0000 001L Sbb nnnnn
        // L (bit 8) = include lr/pc; S (bit 7) = store(1)/load(0); bb = bank;
        // n = (register count - 1). The list is `r{bank*8} ..= r{(bank*8+n)&31}`
        // (bank 1 is special-cased to start at r6, not r8). `lr`/`pc`, when
        // present, occupies the top (highest-address) word of the frame.
        let with_ret = p & 0x0100 != 0;
        let is_store = p & 0x0080 != 0;
        let bank = (p >> 5) & 3;
        let first = [0u8, 6, 16, 24][bank as usize];
        let m = p & 0x1F;
        // Per the VC4 Programmers Manual: "If mmmmm is 31 and pc/lr are
        // stored/loaded, then no register but pc/lr is stored/loaded" — and the
        // same holds once the `rb..rm` range wraps past r31 (e.g.
        // `stm r24-r7, lr`). Those forms push/pop `lr`/`pc` alone.
        let ret_only = with_ret && (m == 31 || first as u32 + m >= 32);
        let count = if ret_only { 0 } else { (m as u8) + 1 };
        return if is_store {
            Op::PushMulti {
                first,
                count,
                include_lr: with_ret,
            }
        } else {
            Op::PopMulti {
                first,
                count,
                include_pc: with_ret,
            }
        };
    }
    // 0000 01Xu uuuu dddd : ld/st rd, (sp + u*4)
    if p & 0xFC00 == 0x0400 {
        let u = ((p >> 4) & 0x1F) as i32 * 4;
        return ldst(
            p & 0x200 != 0,
            MemWidth::Word,
            rd4,
            AddrMode::simple(Base::Sp, u),
            Cond::Al,
        );
    }
    // 0000 1sss ssss dddd : ld/st{w} rd, (rs)   (sub-op 0..7)
    if p & 0xF800 == 0x0800 {
        let sub = (p >> 8) & 7;
        let rs = ((p >> 4) & 0xF) as u8;
        let (store, w) = ldst_suffix(sub);
        return ldst(store, w, rd4, AddrMode::simple(Base::Reg(rs), 0), Cond::Al);
    }
    // 0001 0ooo oood dddd : lea rd, (sp + sext6(o)*4)
    if p & 0xF800 == 0x1000 {
        let o = sext((p >> 5) & 0x3F, 6) * 4;
        return Op::Lea {
            rd: rd5,
            addr: AddrMode::simple(Base::Sp, o),
        };
    }
    // 0001 1ccc cooo oooo : b<cond>  (pc + sext7(o)*2)
    if p & 0xF800 == 0x1800 {
        let cond = Cond::from_bits((p >> 7) & 0xF);
        let off = sext(p & 0x7F, 7) * 2;
        return Op::BranchImm {
            cond,
            link: false,
            target: pc.wrapping_add(off as u32),
        };
    }
    // 0010 uuuu ssss dddd : ld / 0011 .... : st  rd, (rs + u*4)
    if p & 0xE000 == 0x2000 {
        let u = ((p >> 8) & 0xF) as i32 * 4;
        let rs = ((p >> 4) & 0xF) as u8;
        return ldst(
            p & 0x1000 != 0,
            MemWidth::Word,
            rd4,
            AddrMode::simple(Base::Reg(rs), u),
            Cond::Al,
        );
    }
    // 010p pppp ssss dddd : rd = rd <p> rs
    if p & 0xE000 == 0x4000 {
        let op = AluOp::from_p((p >> 8) & 0x1F);
        let rs = ((p >> 4) & 0xF) as u8;
        return Op::Alu2 {
            op,
            rd: rd4,
            rs,
            set_flags: op.is_compare(),
        };
    }
    // 011q qqqu uuuu dddd : rd = rd <q> #u   (u is an unsigned 5-bit literal,
    // 0..31 — per Hermitage's `vciv.py`, which types it `o_imm`; the small
    // negative-constant forms use the 32-bit `add rd, #simm` encoding instead)
    if p & 0xE000 == 0x6000 {
        let op = AluOp::from_q((p >> 9) & 0xF);
        let imm = ((p >> 4) & 0x1F) as i32;
        return Op::AluImm {
            op,
            rd: rd4,
            imm,
            set_flags: op.is_compare(),
        };
    }

    Op::Unimpl {
        raw: p as u64,
        len: 2,
        class: InsnClass::Scalar16,
    }
}

/// Map a 3-bit load/store sub-op (`0000 1sss ...` and `1010 xxxs ss...`) to
/// `(is_store, width)`: 0 ld, 1 st, 2 ldh, 3 sth, 4 ldb, 5 stb, 6 lds, 7 sts.
fn ldst_suffix(sub: u32) -> (bool, MemWidth) {
    let store = sub & 1 != 0;
    let w = match sub >> 1 {
        0 => MemWidth::Word,
        1 => MemWidth::Half,
        2 => MemWidth::Byte,
        _ => MemWidth::SignedHalf,
    };
    (store, w)
}

fn decode32(p0: u16, p1: u16, pc: u32) -> Op {
    let hw0 = p0 as u32;
    let hw1 = p1 as u32;
    let w = (hw0 << 16) | hw1;

    // 1000 cccc <a4> <d4>  SS ...  : addcmpb<c>
    if hw0 & 0xF000 == 0x8000 {
        let cond = Cond::from_bits((hw0 >> 8) & 0xF);
        let f1 = (hw0 >> 4) & 0xF;
        let rd = (hw0 & 0xF) as u8;
        let sel = (hw1 >> 14) & 3;
        let (a, b, off) = match sel {
            0 => (
                RegOrImm::Reg(f1 as u8),
                RegOrImm::Reg(((hw1 >> 10) & 0xF) as u8),
                sext(hw1 & 0x3FF, 10),
            ),
            1 => (
                RegOrImm::Imm(sext(f1, 4)),
                RegOrImm::Reg(((hw1 >> 10) & 0xF) as u8),
                sext(hw1 & 0x3FF, 10),
            ),
            2 => (
                RegOrImm::Reg(f1 as u8),
                RegOrImm::Imm(((hw1 >> 8) & 0x3F) as i32),
                sext(hw1 & 0xFF, 8),
            ),
            _ => (
                RegOrImm::Imm(sext(f1, 4)),
                RegOrImm::Imm(((hw1 >> 8) & 0x3F) as i32),
                sext(hw1 & 0xFF, 8),
            ),
        };
        return Op::AddCmpB {
            cond,
            rd,
            a,
            b,
            target: pc.wrapping_add((off * 2) as u32),
        };
    }

    // 1001 cccc 0 <off23>  : b<cond>    /    1001 <hi4> 1 <off23> : bl (27-bit)
    if hw0 & 0xF000 == 0x9000 {
        return if w & 0x0080_0000 == 0 {
            let cond = Cond::from_bits((w >> 24) & 0xF);
            let target = pc.wrapping_add((sext(w & 0x007F_FFFF, 23) * 2) as u32);
            Op::BranchImm {
                cond,
                link: false,
                target,
            }
        } else {
            let o = (((w >> 24) & 0xF) << 23) | (w & 0x007F_FFFF);
            let target = pc.wrapping_add((sext(o, 27) * 2) as u32);
            Op::BranchImm {
                cond: Cond::Al,
                link: true,
                target,
            }
        };
    }

    // 1010 xxxx ... : the load/store block
    if hw0 & 0xF000 == 0xA000 {
        return decode_ldst32(hw0, hw1);
    }

    // 1011 00pp pppd dddd <i16>       : rd = rd <p> #sext16(i)   (0xB000..0xB3E0)
    if hw0 & 0xFC00 == 0xB000 {
        let op = AluOp::from_p((hw0 >> 5) & 0x1F);
        let rd = (hw0 & 0x1F) as u8;
        return Op::AluImm {
            op,
            rd,
            imm: sext(hw1, 16),
            set_flags: op.is_compare(),
        };
    }
    // 1011 01nn nnnd dddd <o16>  : lea rd, (rN + sext16(o))   (0xB400..0xB7FF)
    // 1011 1111 111d dddd <o16>  : lea rd, (pc + sext16(o))   (0xBFE0, N == 31)
    //   rN = hw0 bits 5..9;  N == 31 means PC.
    if hw0 & 0xFC00 == 0xB400 || hw0 & 0xFFE0 == 0xBFE0 {
        let n = (hw0 >> 5) & 0x1F;
        let base = if n == 31 {
            Base::Pc
        } else {
            Base::Reg(n as u8)
        };
        return Op::Lea {
            rd: (hw0 & 0x1F) as u8,
            addr: AddrMode::simple(base, sext(hw1, 16)),
        };
    }

    // 1100 0ppp pppd dddd | aaaaa 0 CCCC {0 bbbbb | 1 iiiiii} :
    //   rd = ra <p> b  if <cond>          (`0xC000..=0xC7FF` triadic ALU)
    // The 6-bit `p` selects the op (mov/add/.../mulhd/div/count/subscale);
    // w1 bit 6 picks reg vs. signed-6-bit-imm; w1 bits 7..10 are the condition.
    if hw0 & 0xF800 == 0xC000 {
        let idx6 = (hw0 >> 5) & 0x3F;
        let op = AluOp::from_c_triadic(idx6);
        let rd = (hw0 & 0x1F) as u8;
        let ra = ((hw1 >> 11) & 0x1F) as u8;
        let cond = Cond::from_bits((hw1 >> 7) & 0xF);
        let b = if hw1 & 0x40 != 0 {
            RegOrImm::Imm(sext(hw1 & 0x3F, 6))
        } else {
            RegOrImm::Reg((hw1 & 0x1F) as u8)
        };
        // `adds`/`subs`/`shls` (idx 0x28..0x2a) set flags; so do the compares.
        let set_flags = op.is_compare() || (0x28..=0x2a).contains(&idx6);
        return Op::Alu3 {
            op,
            cond,
            rd,
            ra,
            b,
            set_flags,
        };
    }

    // 1100 100f fffd dddd | aaaaa CCCC {0 bbbbb | 1 iiiiii} :
    //   scalar FP triadic  (`0xC800..=0xC9FF`) — f selects the `f` table op.
    // 1100 1010 0ttd dddd | ... : FP convert (`0xCA00..=0xCA7F`) —
    //   tt = 00 ftrunc, 01 floor, 10 flts, 11 fltu; operand is the shift.
    if hw0 & 0xFE00 == 0xC800 || hw0 & 0xFF80 == 0xCA00 {
        let rd = (hw0 & 0x1F) as u8;
        let ra = ((hw1 >> 11) & 0x1F) as u8;
        let cond = Cond::from_bits((hw1 >> 7) & 0xF);
        let b = if hw1 & 0x40 != 0 {
            RegOrImm::Imm(sext(hw1 & 0x3F, 6))
        } else {
            RegOrImm::Reg((hw1 & 0x1F) as u8)
        };
        let op = if hw0 & 0xFE00 == 0xC800 {
            FpOp::from_f_table((hw0 >> 5) & 0xF)
        } else {
            match (hw0 >> 5) & 3 {
                0 => FpOp::Ftrunc,
                1 => FpOp::FtruncFloor,
                2 => FpOp::Flts,
                _ => FpOp::Fltu,
            }
        };
        return Op::FpAlu3 {
            op,
            cond,
            rd,
            ra,
            b,
        };
    }

    // 1100 1100 000d dddd | ...000a aaaa : mov p<a>, r<d>   (write coproc)
    // 1100 1100 001d dddd | ...000a aaaa : mov r<d>, p<a>   (read coproc)
    if hw0 & 0xFFC0 == 0xCC00 {
        let d = (hw0 & 0x1F) as u8;
        let a = (hw1 & 0x1F) as u8;
        return if hw0 & 0x20 == 0 {
            Op::MovToCoproc { preg: a, rs: d }
        } else {
            Op::MovFromCoproc { rd: d, preg: a }
        };
    }

    Op::Unimpl {
        raw: w as u64,
        len: 4,
        class: InsnClass::Scalar32,
    }
}

fn decode_ldst32(hw0: u32, hw1: u32) -> Op {
    let rd = (hw0 & 0x1F) as u8;
    let store = hw0 & 0x20 != 0;
    let w = MemWidth::from_ww((hw0 >> 6) & 3);
    let hi = (hw0 >> 8) & 0xFF; // 0xA0..0xAB

    match hi {
        // 1010 0000 : ld/st{w}{C} rd, (ra + rb)
        0xA0 => {
            let ra = ((hw1 >> 11) & 0x1F) as u8;
            let cond = Cond::from_bits((hw1 >> 7) & 0xF);
            let rb = (hw1 & 0x1F) as u8;
            ldst(
                store,
                w,
                rd,
                AddrMode::simple(Base::RegReg(ra, rb), 0),
                cond,
            )
        }
        // 1010 001o ww{0/1}d dddd | sssss ooo oooo oooo :
        //   ld/st{w} rd, (rs + o)   — `o` is a signed 12-bit displacement, its
        //   top bit is `o` in `1010 001o` and the low 11 bits are in hw1.
        0xA2 | 0xA3 => {
            let rs = ((hw1 >> 11) & 0x1F) as u8;
            let off = sext((((hw0 >> 8) & 1) << 11) | (hw1 & 0x7FF), 12);
            ldst(store, w, rd, AddrMode::simple(Base::Reg(rs), off), Cond::Al)
        }
        // 1010 0100 : ld/st{w}{C} rd, (--rs)   /   1010 0101 : (rs++)
        0xA4 | 0xA5 => {
            let rs = ((hw1 >> 11) & 0x1F) as u8;
            let cond = Cond::from_bits((hw1 >> 7) & 0xF);
            let wb = if hi == 0xA4 {
                Writeback::PreDec
            } else {
                Writeback::PostInc
            };
            let addr = AddrMode {
                base: Base::Reg(rs),
                offset: 0,
                writeback: wb,
            };
            ldst(store, w, rd, addr, cond)
        }
        // 1010 10bb : ld/st{w} rd, (base + sext16(o))   base: r24 / sp / pc / r0
        0xA8..=0xAB => {
            let base = match hi & 3 {
                0 => Base::Gp,
                1 => Base::Sp,
                2 => Base::Pc,
                _ => Base::R0,
            };
            ldst(
                store,
                w,
                rd,
                AddrMode::simple(base, sext(hw1, 16)),
                Cond::Al,
            )
        }
        _ => Op::Unimpl {
            raw: ((hw0 << 16) | hw1) as u64,
            len: 4,
            class: InsnClass::Scalar32,
        },
    }
}

/// Combine the three parcels of a 48-bit instruction: opcode halfword is
/// `bytes[0..2]`, trailing 32-bit field is `LE(bytes[4..6]) << 16 | LE(bytes[2..4])`.
fn imm32_of_48(bytes: &[u8]) -> u32 {
    ((parcel(bytes, 2) as u32) << 16) | parcel(bytes, 1) as u32
}

fn decode48(bytes: &[u8], pc: u32) -> Op {
    let hw0 = parcel(bytes, 0) as u32;
    let imm = imm32_of_48(bytes);

    match hw0 {
        0xE000 => {
            return Op::BranchImm {
                cond: Cond::Al,
                link: false,
                target: imm,
            }
        }
        0xE100 => {
            return Op::BranchImm {
                cond: Cond::Al,
                link: false,
                target: pc.wrapping_add(imm),
            }
        }
        0xE200 => {
            return Op::BranchImm {
                cond: Cond::Al,
                link: true,
                target: imm,
            }
        }
        0xE300 => {
            return Op::BranchImm {
                cond: Cond::Al,
                link: true,
                target: pc.wrapping_add(imm),
            }
        }
        _ => {}
    }

    // 1110 0101 000d dddd <off32> : lea rd, (pc + off32)
    if hw0 & 0xFFE0 == 0xE500 {
        return Op::Lea {
            rd: (hw0 & 0x1F) as u8,
            addr: AddrMode::simple(Base::Pc, imm as i32),
        };
    }

    // 1110 011s ss.d dddd <rs:5 off:27> : ld/st{w} rd, (rs + off27)
    if hw0 & 0xFE00 == 0xE600 {
        let w = MemWidth::from_ww((hw0 >> 6) & 3);
        let store = hw0 & 0x20 != 0;
        let rd = (hw0 & 0x1F) as u8;
        let rs = ((imm >> 27) & 0x1F) as u8;
        let off = sext(imm & 0x07FF_FFFF, 27);
        let base = if hw0 & 0x0100 != 0 {
            Base::Pc
        } else {
            Base::Reg(rs)
        };
        return ldst(store, w, rd, AddrMode::simple(base, off), Cond::Al);
    }

    // 1110 10pp pppd dddd <imm32> : rd = rd <p> #imm32
    if hw0 & 0xFC00 == 0xE800 {
        let op = AluOp::from_p((hw0 >> 5) & 0x1F);
        return Op::AluImm {
            op,
            rd: (hw0 & 0x1F) as u8,
            imm: imm as i32,
            set_flags: op.is_compare(),
        };
    }
    // 1110 11ss sssd dddd <imm32> : add rd, rs, #imm32
    if hw0 & 0xFC00 == 0xEC00 {
        return Op::Alu3 {
            op: AluOp::Add,
            cond: Cond::Al,
            rd: (hw0 & 0x1F) as u8,
            ra: ((hw0 >> 5) & 0x1F) as u8,
            b: RegOrImm::Imm(imm as i32),
            set_flags: false,
        };
    }

    Op::Unimpl {
        raw: ((hw0 as u64) << 32) | imm as u64,
        len: 6,
        class: InsnClass::Scalar48,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(bytes: &[u8]) -> Op {
        decode(bytes, 0x8000_0000).op
    }

    fn dec32(w: u32, pc: u32) -> Op {
        let mut raw = [0u8; 4];
        raw[..2].copy_from_slice(&((w >> 16) as u16).to_le_bytes());
        raw[2..].copy_from_slice(&(w as u16).to_le_bytes());
        decode(&raw, pc).op
    }

    #[test]
    fn nop_and_friends() {
        assert_eq!(dec(&[0x01, 0x00]), Op::Nop);
        assert_eq!(dec(&[0x00, 0x00]), Op::Bkpt);
        assert_eq!(dec(&[0x0A, 0x00]), Op::Rti);
        assert_eq!(dec(&[0x04, 0x00]), Op::SetIrqEnable(true));
        assert_eq!(dec(&[0x05, 0x00]), Op::SetIrqEnable(false));
        assert_eq!(dec(&[0x03, 0x00]), Op::Nop);
        assert_eq!(dec(&[0x06, 0x00]), Op::Nop);
        assert_eq!(dec(&0x00E5u16.to_le_bytes()), Op::Version { rd: 5 });
    }

    #[test]
    fn mov_imm5_and_add_reg() {
        assert_eq!(
            dec(&0x6073u16.to_le_bytes()),
            Op::AluImm {
                op: AluOp::Mov,
                rd: 3,
                imm: 7,
                set_flags: false
            }
        );
        assert_eq!(
            dec(&0x4210u16.to_le_bytes()),
            Op::Alu2 {
                op: AluOp::Add,
                rd: 0,
                rs: 1,
                set_flags: false
            }
        );
    }

    #[test]
    fn ldst_indirect_widths() {
        // 0000 1sss ssss dddd, sub=5 (stb), rs=4, rd=5
        let bits = 0x0800u16 | (5 << 8) | (4 << 4) | 5;
        assert_eq!(
            dec(&bits.to_le_bytes()),
            Op::Store {
                w: MemWidth::Byte,
                rd: 5,
                addr: AddrMode::simple(Base::Reg(4), 0),
                cond: Cond::Al,
            }
        );
    }

    #[test]
    fn cond_branch_16_backwards() {
        let bits = 0x1800u16 | (1 << 7) | 0x7E; // ne, -2
        assert_eq!(
            decode(&bits.to_le_bytes(), 0x8000_0010).op,
            Op::BranchImm {
                cond: Cond::Ne,
                link: false,
                target: 0x8000_000C
            }
        );
    }

    #[test]
    fn branch_32_is_23bit() {
        // 1001 1110 0 <off23=0x543> -> pc + 0x543*2
        assert_eq!(
            dec32(0x9E00_0543, 0xCEC0_02F0),
            Op::BranchImm {
                cond: Cond::Al,
                link: false,
                target: 0xCEC0_0D76
            }
        );
    }

    #[test]
    fn predecrement_store_conditional() {
        // 1010 0100 ww1d dddd | sssss CCCC 0000000 ; ww=word, st, rd=26, rs=25, al
        let hw0 = 0xA400u32 | 0x20 | 26;
        let hw1 = (25u32 << 11) | (0xE << 7);
        assert_eq!(
            dec32((hw0 << 16) | hw1, 0),
            Op::Store {
                w: MemWidth::Word,
                rd: 26,
                addr: AddrMode {
                    base: Base::Reg(25),
                    offset: 0,
                    writeback: Writeback::PreDec
                },
                cond: Cond::Al,
            }
        );
    }

    #[test]
    fn j_absolute_48bit() {
        let bytes = [0x00, 0xE0, 0x34, 0x12, 0xC0, 0x0E];
        assert_eq!(
            decode(&bytes, 0xCEC0_0000).op,
            Op::BranchImm {
                cond: Cond::Al,
                link: false,
                target: 0x0EC0_1234
            }
        );
    }

    #[test]
    fn alu_imm32_48bit_peripheral_addr() {
        // 1110 1000 000d dddd <imm32 = 0x7E002030> ; mov r1, #0x7E002030
        let bytes = [0x01, 0xE8, 0x30, 0x20, 0x00, 0x7E];
        assert_eq!(
            decode(&bytes, 0).op,
            Op::AluImm {
                op: AluOp::Mov,
                rd: 1,
                imm: 0x7E00_2030u32 as i32,
                set_flags: false
            }
        );
    }

    #[test]
    fn unknown_48bit_is_sized() {
        let insn = decode(&[0x00, 0xE4, 0, 0, 0, 0], 0);
        assert_eq!(insn.len, 6);
        assert!(matches!(insn.op, Op::Unimpl { len: 6, .. }));
    }
}
