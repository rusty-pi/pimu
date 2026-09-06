//! Decoded-instruction representation for the implemented VPU subset.
//!
//! The decoder always determines the correct *length* of every instruction (see
//! [`length`](super::length)); it only produces a rich [`Op`] for the subset the
//! executor understands. Everything else becomes [`Op::Unimpl`], which the
//! executor can either fault on or skip (trace mode).

use super::length::InsnClass;
use super::reg::Cond;

/// ALU operation, unified across the 16-bit (`p`/`q` tables), 32-bit imm and
/// 32-bit triadic encodings. Names follow `videocoreiv.arch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AluOp {
    Mov,
    Cmn,
    Add,
    Bic,
    Mul,
    Eor,
    Sub,
    And,
    Not,
    Ror,
    Cmp,
    Rsub,
    Btest,
    Or,
    Bmask,
    Max,
    Bitset,
    Min,
    Bitclear,
    Bitflip,
    Signext,
    Neg,
    Lsr,
    Msb,
    Shl,
    Bitrev,
    Asr,
    Abs,
    AddScale(u8), // add with ra shifted left by n
    /// Recognised mnemonic, semantics not implemented yet.
    Unimpl(&'static str),
}

impl AluOp {
    /// The 32-entry `p` table (`010p pppp` 16-bit reg/reg and `1011 00pp ppp`
    /// 16-bit-imm16 and `1100 00pp ppp` triadic forms).
    pub fn from_p(idx: u32) -> AluOp {
        use AluOp::*;
        match idx & 31 {
            0 => Mov,
            1 => Cmn,
            2 => Add,
            3 => Bic,
            4 => Mul,
            5 => Eor,
            6 => Sub,
            7 => And,
            8 => Not,
            9 => Ror,
            10 => Cmp,
            11 => Rsub,
            12 => Btest,
            13 => Or,
            14 => Bmask,
            15 => Max,
            16 => Bitset,
            17 => Min,
            18 => Bitclear,
            19 => AddScale(2),
            20 => Bitflip,
            21 => AddScale(4),
            22 => AddScale(8),
            23 => AddScale(16),
            24 => Signext,
            25 => Neg,
            26 => Lsr,
            27 => Msb,
            28 => Shl,
            29 => Bitrev,
            30 => Asr,
            _ => Abs,
        }
    }

    /// The 16-entry `q` table (`011q qqq` 16-bit reg / 5-bit-imm form).
    pub fn from_q(idx: u32) -> AluOp {
        use AluOp::*;
        match idx & 15 {
            0 => Mov,
            1 => Add,
            2 => Mul,
            3 => Sub,
            4 => Not,
            5 => Cmp,
            6 => Btest,
            7 => Bmask,
            8 => Bitset,
            9 => Bitclear,
            10 => Bitflip,
            11 => AddScale(8),
            12 => Signext,
            13 => Lsr,
            14 => Shl,
            _ => Asr,
        }
    }

    /// Does this op discard its result and exist only for its flag effect?
    pub fn is_compare(self) -> bool {
        matches!(self, AluOp::Cmp | AluOp::Cmn | AluOp::Btest)
    }
}

/// Memory access width, from the 2-bit `ww` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemWidth {
    Word,
    Half,
    Byte,
    SignedHalf,
}

impl MemWidth {
    pub fn from_ww(ww: u32) -> MemWidth {
        match ww & 3 {
            0 => MemWidth::Word,
            1 => MemWidth::Half,
            2 => MemWidth::Byte,
            _ => MemWidth::SignedHalf,
        }
    }
}

/// Base register for an address calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    Reg(u8),
    Sp,
    Pc,
    Gp,
    R0,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddrMode {
    pub base: Base,
    pub offset: i32,
}

/// Right-hand operand of a triadic ALU op: register or 6-bit immediate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegOrImm {
    Reg(u8),
    Imm(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Nop,
    Bkpt,
    Sleep,
    /// Return from interrupt (pops SR and PC).
    Rti,
    /// Software interrupt / syscall. `vector` is the trap number.
    Swi {
        vector: u32,
    },
    /// `b`/`bl` to the address held in a register.
    BranchReg {
        link: bool,
        rd: u8,
    },
    /// `b<cond>`/`bl` with a pc-relative displacement. `target` is absolute,
    /// already resolved against the instruction address.
    BranchImm {
        cond: Cond,
        link: bool,
        target: u32,
    },
    /// `rd = rd op rs` (16-bit `p` table).
    Alu2 {
        op: AluOp,
        rd: u8,
        rs: u8,
        set_flags: bool,
    },
    /// `rd = rd op imm` (16-bit 5-bit-imm `q` table, and 32-bit imm16 form).
    AluImm {
        op: AluOp,
        rd: u8,
        imm: i32,
        set_flags: bool,
    },
    /// `rd = ra op b` predicated on `cond` (32-bit triadic form).
    Alu3 {
        op: AluOp,
        cond: Cond,
        rd: u8,
        ra: u8,
        b: RegOrImm,
        set_flags: bool,
    },
    /// `sp = sp + imm`.
    AddSp {
        imm: i32,
    },
    /// `rd = sp + imm`.
    AddRegSp {
        rd: u8,
        imm: i32,
    },
    /// `rd = pc + imm`.
    AddRegPc {
        rd: u8,
        imm: i32,
    },
    Load {
        w: MemWidth,
        rd: u8,
        addr: AddrMode,
    },
    Store {
        w: MemWidth,
        rd: u8,
        addr: AddrMode,
    },
    /// `version rd` — read the chip/VPU version register.
    Version {
        rd: u8,
    },
    /// `addcmpb`: `rd = rd + a; compare rd with b; if <cond> branch to target`.
    AddCmpB {
        cond: Cond,
        rd: u8,
        a: RegOrImm,
        b: RegOrImm,
        target: u32,
    },
    /// `stm` — push `r[first ..= last]` (+ optional `lr`) to `(--sp)`.
    PushMulti {
        first: u8,
        last: u8,
        include_lr: bool,
    },
    /// `ldm` — pop `r[first ..= last]` (+ optional `pc`) from `(sp++)`.
    PopMulti {
        first: u8,
        last: u8,
        include_pc: bool,
    },
    /// Correctly sized but not decoded to semantics.
    Unimpl {
        raw: u64,
        len: u8,
        class: InsnClass,
    },
}

/// A fully decoded instruction plus its byte length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insn {
    pub op: Op,
    pub len: u8,
}
