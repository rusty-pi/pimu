//! Instruction decoder for the implemented VPU subset.
//!
//! Coverage is deliberately partial (milestone M1): enough 16- and 32-bit scalar
//! forms to run hand-written console payloads and simple control flow. Every
//! instruction is still *sized* correctly, so unknown ones become
//! [`Op::Unimpl`] with the right length and the PC keeps its footing.
//!
//! Bit patterns are transcribed from Herman Hermitage's `videocoreiv.arch`.

use super::insn::{AddrMode, AluOp, Base, Insn, MemWidth, Op, RegOrImm};
use super::length::{insn_class, insn_len_bytes, InsnClass};
use super::reg::{Cond, SP};

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

/// Combine the three parcels of a 48-bit instruction. Per `videocoreiv.arch`
/// (`set-byte-order 1 0 5 4 3 2`, "short0 short2 short1"): the opcode halfword is
/// `bytes[0..2]`, and the trailing 32-bit field is
/// `LE(bytes[4..6]) << 16 | LE(bytes[2..4])`.
fn imm32_of_48(bytes: &[u8]) -> u32 {
    ((parcel(bytes, 2) as u32) << 16) | parcel(bytes, 1) as u32
}

fn decode48(bytes: &[u8], pc: u32) -> Op {
    let hw0 = parcel(bytes, 0) as u32;
    let imm = imm32_of_48(bytes);

    // 1110 00xx 0000 0000 <abs32|off32> : j / b / jl / bl
    match hw0 {
        0xE000 => {
            return Op::BranchImm {
                cond: Cond::Al,
                link: false,
                target: imm,
            }; // j abs
        }
        0xE100 => {
            return Op::BranchImm {
                cond: Cond::Al,
                link: false,
                target: pc.wrapping_add(imm),
            };
        }
        0xE200 => {
            return Op::BranchImm {
                cond: Cond::Al,
                link: true,
                target: imm,
            }; // jl abs
        }
        0xE300 => {
            return Op::BranchImm {
                cond: Cond::Al,
                link: true,
                target: pc.wrapping_add(imm),
            };
        }
        _ => {}
    }

    // 1110 0101 000d dddd <off32> : add rd, pc, #off32
    if hw0 & 0xFFE0 == 0xE500 {
        return Op::AddRegPc {
            rd: (hw0 & 0x1F) as u8,
            imm: imm as i32,
        };
    }

    // 1110 011x wwXd dddd  <rs(5) off(27)> : ld/st (rs + off27) / (pc + off27)
    if hw0 & 0xFE00 == 0xE600 {
        let ww = MemWidth::from_ww((hw0 >> 6) & 3);
        let store = hw0 & 0x20 != 0;
        let rd = (hw0 & 0x1F) as u8;
        let rs = ((imm >> 27) & 0x1F) as u8;
        let off = sext(imm & 0x07FF_FFFF, 27);
        let pc_relative = hw0 & 0x0100 != 0; // 1110 0111 -> (pc + off)
        let addr = AddrMode {
            base: if pc_relative { Base::Pc } else { Base::Reg(rs) },
            offset: off,
        };
        return if store {
            Op::Store { w: ww, rd, addr }
        } else {
            Op::Load { w: ww, rd, addr }
        };
    }

    // 1110 10pp pppd dddd <imm32> : rd = rd <p> #imm32
    if hw0 & 0xFC00 == 0xE800 {
        let op = AluOp::from_p((hw0 >> 5) & 0x1F);
        let rd = (hw0 & 0x1F) as u8;
        return Op::AluImm {
            op,
            rd,
            imm: imm as i32,
            set_flags: op.is_compare(),
        };
    }
    // 1110 11ss sssd dddd <imm32> : add rd, rs, #imm32
    if hw0 & 0xFC00 == 0xEC00 {
        let rs = ((hw0 >> 5) & 0x1F) as u8;
        let rd = (hw0 & 0x1F) as u8;
        return Op::Alu3 {
            op: AluOp::Add,
            cond: Cond::Al,
            rd,
            ra: rs,
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

fn decode16(p0: u16, pc: u32) -> Op {
    let p = p0 as u32;

    // Fixed 16-bit encodings.
    match p0 {
        0x0000 => return Op::Bkpt,
        0x0001 => return Op::Nop,
        0x0002 => return Op::Sleep,
        0x000A => return Op::Rti,
        _ => {}
    }

    let rd4 = (p & 0xF) as u8;
    let rd5 = (p & 0x1F) as u8;

    // 0000 0000 001d dddd : swi rd  (vector 0x20 + d)
    if p & 0xFFE0 == 0x0020 {
        return Op::Swi {
            vector: 0x20 + (p & 0x1F),
        };
    }
    // 0000 0000 010d dddd : b rd
    if p & 0xFFE0 == 0x0040 {
        return Op::BranchReg {
            link: false,
            rd: rd5,
        };
    }
    // 0000 0000 011d dddd : bl rd
    if p & 0xFFE0 == 0x0060 {
        return Op::BranchReg {
            link: true,
            rd: rd5,
        };
    }
    // 0000 0001 11uu uuuu : swi #u
    if p & 0xFFC0 == 0x01C0 {
        return Op::Swi {
            vector: 0x20 + (p & 0x3F),
        };
    }
    // 0000 010u uuuu dddd : ld rd,(sp + u*4)
    if p & 0xFE00 == 0x0400 {
        let u = (p >> 4) & 0x1F;
        return Op::Load {
            w: MemWidth::Word,
            rd: rd4,
            addr: AddrMode {
                base: Base::Sp,
                offset: (u * 4) as i32,
            },
        };
    }
    // 0000 011u uuuu dddd : st rd,(sp + u*4)
    if p & 0xFE00 == 0x0600 {
        let u = (p >> 4) & 0x1F;
        return Op::Store {
            w: MemWidth::Word,
            rd: rd4,
            addr: AddrMode {
                base: Base::Sp,
                offset: (u * 4) as i32,
            },
        };
    }
    // 0000 1ww0 ssss dddd : ld{w} rd,(rs)   /   0000 1ww1 ... : st{w}
    if p & 0xF800 == 0x0800 {
        let w = MemWidth::from_ww((p >> 9) & 3);
        let rs = ((p >> 4) & 0xF) as u8;
        let addr = AddrMode {
            base: Base::Reg(rs),
            offset: 0,
        };
        return if p & 0x0100 == 0 {
            Op::Load { w, rd: rd4, addr }
        } else {
            Op::Store { w, rd: rd4, addr }
        };
    }
    // 0001 0ooo oood dddd : add rd,sp,#o*4   (d == 25 => add sp,#o*4)
    if p & 0xF800 == 0x1000 {
        let o = ((p >> 5) & 0x3F) as i32 * 4;
        return if rd5 as usize == SP {
            Op::AddSp { imm: o }
        } else {
            Op::AddRegSp { rd: rd5, imm: o }
        };
    }
    // 0001 1ccc cooo oooo : b<cond> pc-relative
    if p & 0xF800 == 0x1800 {
        let cond = Cond::from_bits((p >> 7) & 0xF);
        let off = sext(p & 0x7F, 7) * 2;
        return Op::BranchImm {
            cond,
            link: false,
            target: pc.wrapping_add(off as u32),
        };
    }
    // 0010 uuuu ssss dddd : ld rd,(rs + u*4)   /   0011 .... : st
    if p & 0xE000 == 0x2000 {
        let u = ((p >> 8) & 0xF) as i32 * 4;
        let rs = ((p >> 4) & 0xF) as u8;
        let addr = AddrMode {
            base: Base::Reg(rs),
            offset: u,
        };
        return if p & 0x1000 == 0 {
            Op::Load {
                w: MemWidth::Word,
                rd: rd4,
                addr,
            }
        } else {
            Op::Store {
                w: MemWidth::Word,
                rd: rd4,
                addr,
            }
        };
    }
    // 010p pppp ssss dddd : rd = rd op rs
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
    // 011q qqqu uuuu dddd : rd = rd op #sext5(u)
    if p & 0xE000 == 0x6000 {
        let op = AluOp::from_q((p >> 9) & 0xF);
        let imm = sext((p >> 4) & 0x1F, 5);
        return Op::AluImm {
            op,
            rd: rd4,
            imm,
            set_flags: op.is_compare(),
        };
    }
    // 0000 0000 111d dddd : version rd
    if p & 0xFFE0 == 0x00E0 {
        return Op::Version { rd: rd5 };
    }
    // 0000 001X Ybb nnnnn : ldm/stm  (X=+lr/pc, Y=store, bb=bank, n=count-1)
    if p & 0xFE00 == 0x0200 {
        let include_extra = p & 0x0100 != 0;
        let is_store = p & 0x0080 != 0;
        let bank = (p >> 5) & 3;
        // Bank 1 conventionally starts at r6 (the classic GP/first-saved reg),
        // not r8; the other banks are b*8. Verify against real traces.
        let first = [0u8, 6, 16, 24][bank as usize];
        let last = first.saturating_add((p & 0x1F) as u8).min(31);
        return if is_store {
            Op::PushMulti {
                first,
                last,
                include_lr: include_extra,
            }
        } else {
            Op::PopMulti {
                first,
                last,
                include_pc: include_extra,
            }
        };
    }

    Op::Unimpl {
        raw: p as u64,
        len: 2,
        class: InsnClass::Scalar16,
    }
}

fn decode32(p0: u16, p1: u16, pc: u32) -> Op {
    let w = ((p0 as u32) << 16) | p1 as u32;

    // 1000 cccc AAAA DDDD  SS ffffff oooooooo : addcmpb<c>
    //   SS=00: a=rA,  b=rS       , 10-bit offset
    //   SS=01: a=#i4, b=rS       , 10-bit offset
    //   SS=10: a=rA,  b=#u6      , 8-bit offset
    //   SS=11: a=#i4, b=#u6      , 8-bit offset
    if w & 0xF000_0000 == 0x8000_0000 {
        let cond = Cond::from_bits((w >> 24) & 0xF);
        let f1 = (w >> 20) & 0xF; // rA or #i4
        let rd = ((w >> 16) & 0xF) as u8;
        let sel = (w >> 14) & 3;
        let (a, b, off) = match sel {
            0 => (
                RegOrImm::Reg(f1 as u8),
                RegOrImm::Reg(((w >> 10) & 0xF) as u8),
                sext(w & 0x3FF, 10),
            ),
            1 => (
                RegOrImm::Imm(sext(f1, 4)),
                RegOrImm::Reg(((w >> 10) & 0xF) as u8),
                sext(w & 0x3FF, 10),
            ),
            2 => (
                RegOrImm::Reg(f1 as u8),
                RegOrImm::Imm(((w >> 8) & 0x3F) as i32),
                sext(w & 0xFF, 8),
            ),
            _ => (
                RegOrImm::Imm(sext(f1, 4)),
                RegOrImm::Imm(((w >> 8) & 0x3F) as i32),
                sext(w & 0xFF, 8),
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

    // 1001 cccc 0ooo... : b<cond>    /    1001 oooo 1ooo... : bl
    if w & 0xF000_0000 == 0x9000_0000 {
        return if w & 0x0080_0000 == 0 {
            let cond = Cond::from_bits((w >> 24) & 0xF);
            let target = pc.wrapping_add((sext(w & 0x07FF_FFFF, 27) * 2) as u32);
            Op::BranchImm {
                cond,
                link: false,
                target,
            }
        } else {
            let o = ((w >> 24) & 0xF) << 23 | (w & 0x007F_FFFF);
            let target = pc.wrapping_add((sext(o, 27) * 2) as u32);
            Op::BranchImm {
                cond: Cond::Al,
                link: true,
                target,
            }
        };
    }

    // 1011 1111 111d dddd oooo... : add rd, pc, #o
    if w & 0xFFE0_0000 == 0xBFE0_0000 {
        let rd = ((w >> 16) & 0x1F) as u8;
        return Op::AddRegPc {
            rd,
            imm: sext(w & 0xFFFF, 16),
        };
    }
    // 1011 00pp pppd dddd i16 : rd = rd op #sext16(i)
    if w & 0xFC00_0000 == 0xB000_0000 {
        let op = AluOp::from_p((w >> 21) & 0x1F);
        let rd = ((w >> 16) & 0x1F) as u8;
        return Op::AluImm {
            op,
            rd,
            imm: sext(w & 0xFFFF, 16),
            set_flags: op.is_compare(),
        };
    }
    // 1011 01ss sssd dddd i16 : add rd, rs, #sext16(i)
    if w & 0xFC00_0000 == 0xB400_0000 {
        let rs = ((w >> 21) & 0x1F) as u8;
        let rd = ((w >> 16) & 0x1F) as u8;
        return Op::Alu3 {
            op: AluOp::Add,
            cond: Cond::Al,
            rd,
            ra: rs,
            b: RegOrImm::Imm(sext(w & 0xFFFF, 16)),
            set_flags: false,
        };
    }

    // 1010 1000/1001/1010/1011 wwXd dddd o16 : ld/st with base + 16-bit offset
    if w & 0xFC00_0000 == 0xA800_0000 {
        let base = match (w >> 24) & 0x3 {
            0 => Base::Gp,
            1 => Base::Sp,
            2 => Base::Pc,
            _ => Base::R0,
        };
        let ww = MemWidth::from_ww((w >> 22) & 3);
        let rd = ((w >> 16) & 0x1F) as u8;
        let addr = AddrMode {
            base,
            offset: sext(w & 0xFFFF, 16),
        };
        return if w & 0x0020_0000 == 0 {
            Op::Load { w: ww, rd, addr }
        } else {
            Op::Store { w: ww, rd, addr }
        };
    }
    // 1010 001o wwXd dddd sssss o11 : ld/st rd,(rs + sext12(o))
    if w & 0xFE00_0000 == 0xA200_0000 {
        let ww = MemWidth::from_ww((w >> 22) & 3);
        let rd = ((w >> 16) & 0x1F) as u8;
        let rs = ((w >> 11) & 0x1F) as u8;
        let off = (((w >> 24) & 1) << 11) | (w & 0x7FF);
        let addr = AddrMode {
            base: Base::Reg(rs),
            offset: sext(off, 12),
        };
        return if w & 0x0020_0000 == 0 {
            Op::Load { w: ww, rd, addr }
        } else {
            Op::Store { w: ww, rd, addr }
        };
    }

    // 1100 00pp pppd dddd aaaaa CCCC {00bbbbb | 1iiiiii} : rd = ra op b [cond]
    if w & 0xFC00_0000 == 0xC000_0000 {
        let op = AluOp::from_p((w >> 21) & 0x1F);
        let rd = ((w >> 16) & 0x1F) as u8;
        let ra = ((w >> 11) & 0x1F) as u8;
        let cond = Cond::from_bits((w >> 7) & 0xF);
        let b = if w & 0x40 != 0 {
            RegOrImm::Imm(sext(w & 0x3F, 6))
        } else {
            RegOrImm::Reg((w & 0x1F) as u8)
        };
        return Op::Alu3 {
            op,
            cond,
            rd,
            ra,
            b,
            set_flags: op.is_compare(),
        };
    }

    let _ = p1;
    Op::Unimpl {
        raw: w as u64,
        len: 4,
        class: InsnClass::Scalar32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(bytes: &[u8]) -> Op {
        decode(bytes, 0x8000_0000).op
    }

    #[test]
    fn nop_and_friends() {
        assert_eq!(dec(&[0x01, 0x00]), Op::Nop);
        assert_eq!(dec(&[0x00, 0x00]), Op::Bkpt);
        assert_eq!(dec(&[0x02, 0x00]), Op::Sleep);
        assert_eq!(dec(&[0x0A, 0x00]), Op::Rti);
    }

    #[test]
    fn mov_imm5() {
        // 011q qqqu uuuu dddd, q=mov(0), u=7, d=3  => 0x6073
        assert_eq!(
            dec(&0x6073u16.to_le_bytes()),
            Op::AluImm {
                op: AluOp::Mov,
                rd: 3,
                imm: 7,
                set_flags: false
            }
        );
    }

    #[test]
    fn add_reg() {
        // 010p pppp ssss dddd, p=add(2), s=1, d=0 => 0b010_00010_0001_0000 = 0x4210
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
    fn cmp_sets_flags() {
        // p=cmp(10) => 0b010_01010_ssss_dddd
        let bits = 0x4000u16 | (10 << 8) | (2 << 4) | 3;
        assert_eq!(
            dec(&bits.to_le_bytes()),
            Op::Alu2 {
                op: AluOp::Cmp,
                rd: 3,
                rs: 2,
                set_flags: true
            }
        );
    }

    #[test]
    fn store_byte_indirect() {
        // 0000 1ww1 ssss dddd, ww=byte(2), s=4, d=5
        let bits = 0x0800u16 | (2 << 9) | 0x0100 | (4 << 4) | 5;
        assert_eq!(
            dec(&bits.to_le_bytes()),
            Op::Store {
                w: MemWidth::Byte,
                rd: 5,
                addr: AddrMode {
                    base: Base::Reg(4),
                    offset: 0
                },
            }
        );
    }

    #[test]
    fn cond_branch_16_backwards() {
        // 0001 1ccc cooo oooo, cond=ne(1), o=-2 (0x7E) => target = pc - 4
        let bits = 0x1800u16 | (1 << 7) | 0x7E;
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
    fn bl_32() {
        // 1001 oooo 1ooo ... ; encode a small positive offset of +4 (o=2)
        let w = 0x9000_0000u32 | 0x0080_0000 | 2;
        let bytes = [(w >> 16) as u16, w as u16];
        let mut raw = [0u8; 4];
        raw[..2].copy_from_slice(&bytes[0].to_le_bytes());
        raw[2..].copy_from_slice(&bytes[1].to_le_bytes());
        assert_eq!(
            decode(&raw, 0x8000_0000).op,
            Op::BranchImm {
                cond: Cond::Al,
                link: true,
                target: 0x8000_0004
            }
        );
    }

    #[test]
    fn triadic_add_reg() {
        // 1100 00pp pppd dddd aaaaa CCCC 00 bbbbb ; p=add(2) d=1 a=2 cond=al(0xe) b=3
        let w = 0xC000_0000u32 | (2 << 21) | (1 << 16) | (2 << 11) | (0xE << 7) | 3;
        let mut raw = [0u8; 4];
        raw[..2].copy_from_slice(&((w >> 16) as u16).to_le_bytes());
        raw[2..].copy_from_slice(&(w as u16).to_le_bytes());
        assert_eq!(
            decode(&raw, 0).op,
            Op::Alu3 {
                op: AluOp::Add,
                cond: Cond::Al,
                rd: 1,
                ra: 2,
                b: RegOrImm::Reg(3),
                set_flags: false,
            }
        );
    }

    #[test]
    fn unknown_48bit_is_sized() {
        // 0xE400 is an "undefined" 48-bit scalar encoding (videocoreiv.arch).
        let bytes = [0x00, 0xE4, 0, 0, 0, 0];
        let insn = decode(&bytes, 0);
        assert_eq!(insn.len, 6);
        assert!(matches!(insn.op, Op::Unimpl { len: 6, .. }));
    }

    #[test]
    fn j_absolute_48bit() {
        // 1110 0000 0000 0000  <imm32 = 0x0ec0_1234>
        // imm32 packing: LE(bytes[4..6]) << 16 | LE(bytes[2..4])
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
    fn version_16bit() {
        // 0000 0000 111d dddd, d = 5
        assert_eq!(
            decode(&0x00E5u16.to_le_bytes(), 0).op,
            Op::Version { rd: 5 }
        );
    }

    #[test]
    fn ld_gp_relative() {
        // 1010 1000 ww0d dddd o16 : ld rd,(gp + o).  ww=word, d=7, o=0x20
        let w = 0xA800_0000u32 | (7 << 16) | 0x20;
        let mut raw = [0u8; 4];
        raw[..2].copy_from_slice(&((w >> 16) as u16).to_le_bytes());
        raw[2..].copy_from_slice(&(w as u16).to_le_bytes());
        assert_eq!(
            decode(&raw, 0).op,
            Op::Load {
                w: MemWidth::Word,
                rd: 7,
                addr: AddrMode {
                    base: Base::Gp,
                    offset: 0x20
                },
            }
        );
    }
}
