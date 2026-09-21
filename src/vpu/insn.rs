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
    AddScale(u8), // ra + (b << n)  — field is the shift amount n
    SubScale(u8), // ra - (b << n)  — field is the shift amount n
    Count,        // popcount(b)
    MulhdSS,      // high word of ra * b, signed*signed
    MulhdSU,      // signed*unsigned
    MulhdUS,      // unsigned*signed
    MulhdUU,      // unsigned*unsigned
    DivS,         // ra / b, signed
    DivSU,        // signed ra / unsigned b
    DivUS,        // unsigned ra / signed b
    DivU,         // unsigned / unsigned
    Clamp16,      // clamp ra to the signed 16-bit range
    /// Recognised mnemonic, semantics not implemented yet.
    Unimpl(&'static str),
}

/// Scalar floating-point op (`define-table f` in `videocoreiv.arch`, plus the
/// `0xCA00` convert block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpOp {
    Fadd,
    Fsub,
    Fmul,
    Fdiv,
    Fcmp,
    Fabs,
    Frsub,
    Fmax,
    Frcp,
    Frsqrt,
    Fnmul,
    Fmin,
    Fceil,
    Ffloor,
    Flog2,
    Fexp2,
    /// float -> int, truncating, after `<< shift` (`b` is the shift amount).
    Ftrunc,
    /// float -> int, flooring, after `<< shift`.
    FtruncFloor,
    /// signed int -> float, then `>> shift` (scale by 2^-shift).
    Flts,
    /// unsigned int -> float, then `>> shift`.
    Fltu,
}

impl FpOp {
    pub fn mnemonic(self) -> &'static str {
        use FpOp::*;
        match self {
            Fadd => "fadd",
            Fsub => "fsub",
            Fmul => "fmul",
            Fdiv => "fdiv",
            Fcmp => "fcmp",
            Fabs => "fabs",
            Frsub => "frsub",
            Fmax => "fmax",
            Frcp => "frcp",
            Frsqrt => "frsqrt",
            Fnmul => "fnmul",
            Fmin => "fmin",
            Fceil => "fceil",
            Ffloor => "ffloor",
            Flog2 => "flog2",
            Fexp2 => "fexp2",
            Ftrunc => "ftrunc",
            FtruncFloor => "floor",
            Flts => "flts",
            Fltu => "fltu",
        }
    }

    pub fn from_f_table(idx: u32) -> FpOp {
        use FpOp::*;
        [
            Fadd, Fsub, Fmul, Fdiv, Fcmp, Fabs, Frsub, Fmax, Frcp, Frsqrt, Fnmul, Fmin, Fceil,
            Ffloor, Flog2, Fexp2,
        ][(idx & 0xF) as usize]
    }
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
            19 => AddScale(1),
            20 => Bitflip,
            21 => AddScale(2),
            22 => AddScale(3),
            23 => AddScale(4),
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

    /// The 6-bit sub-op field of the triadic conditional ALU (`1100 0ppp pppd
    /// dddd`), covering `0xC000..=0xC7E0`. Extends `from_p` past index 31 with
    /// the wide-ALU ops (mulhd / div / count / adds / subscale). Source:
    /// `vciv.py` ISACC 0xC group.
    pub fn from_c_triadic(idx6: u32) -> AluOp {
        use AluOp::*;
        match idx6 & 0x3f {
            0x20 => MulhdSS,
            0x21 => MulhdSU,
            0x22 => MulhdUS,
            0x23 => MulhdUU,
            0x24 => DivS,
            0x25 => DivSU,
            0x26 => DivUS,
            0x27 => DivU,
            0x28 => Add, // adds
            0x29 => Sub, // subs
            0x2a => Shl, // shls
            0x2b => Clamp16,
            0x2c => AddScale(5),
            0x2d => AddScale(6),
            0x2e => AddScale(7),
            0x2f => AddScale(8),
            0x30 => Count,
            0x31 => SubScale(1),
            0x32 => SubScale(2),
            0x33 => SubScale(3),
            0x34 => SubScale(4),
            0x35 => SubScale(5),
            0x36 => SubScale(6),
            0x37 => SubScale(7),
            0x38 => SubScale(8),
            n if n < 0x20 => Self::from_p(n),
            _ => Unimpl("c-triadic"),
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
            // `rd += #imm << 3` (imm scaled by 8 = one 64-byte block / 16 words).
            // The bootcode's SHA-256 schedule uses `r += 64` and this form
            // interchangeably (e.g. `0x80004632`, `0x80006214`).
            11 => AddScale(3),
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
    /// Only ever a load: the `ww = 11` store encoding (see `decode::ldst`).
    SignedByte,
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
    /// `(rA + rB)` — two-register addressing (16-bit `0xA000` / 48-bit forms).
    RegReg(u8, u8),
}

/// Base-register update for an addressing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Writeback {
    #[default]
    None,
    /// `(--rN)`: decrement the base by the access size *before* the access,
    /// write it back.
    PreDec,
    /// `(rN++)`: use the base, then increment it by the access size and write
    /// it back.
    PostInc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddrMode {
    pub base: Base,
    pub offset: i32,
    pub writeback: Writeback,
}

impl AddrMode {
    pub fn simple(base: Base, offset: i32) -> AddrMode {
        AddrMode {
            base,
            offset,
            writeback: Writeback::None,
        }
    }
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
    /// `ei` / `di` — enable / disable interrupts. The model tracks only the
    /// interrupt-enable bit (SR / `r30` bit 30); firmware `msleep`
    /// (`0x3ED6504C`) reads it (`mov rX, r30; btest rX, #30`) to choose the
    /// yield-to-scheduler path over a CLO busy-wait.
    SetIrqEnable(bool),
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
    /// Scalar floating-point triadic (`0xC800..=0xCA7F`): `rd = ra fop b`,
    /// predicated on `cond`. `fcmp` sets flags. For the convert ops
    /// (`ftrunc`/`ffloor`/`flts`/`fltu`) `b` is the shift amount.
    FpAlu3 {
        op: FpOp,
        cond: Cond,
        rd: u8,
        ra: u8,
        b: RegOrImm,
    },
    /// `rd = <effective address of `addr`>` — `lea` / `add rd, base, #imm`.
    /// With `rd == sp` and a `Sp` base this is the stack-adjust form.
    Lea {
        rd: u8,
        addr: AddrMode,
    },
    Load {
        w: MemWidth,
        rd: u8,
        addr: AddrMode,
        cond: Cond,
    },
    Store {
        w: MemWidth,
        rd: u8,
        addr: AddrMode,
        cond: Cond,
    },
    /// `version rd` — read the chip/VPU version register.
    Version {
        rd: u8,
    },
    /// `switch`/`switch.b rd` — indexed jump through a table that starts right
    /// after the instruction. `byte` selects 8-bit vs 16-bit table entries;
    /// each entry is a signed displacement (in halfwords) from the table base.
    Switch {
        rd: u8,
        byte: bool,
    },
    /// `mov p<preg>, r<rs>` — write a system-coprocessor register.
    MovToCoproc {
        preg: u8,
        rs: u8,
    },
    /// `mov r<rd>, p<preg>` — read a system-coprocessor register.
    MovFromCoproc {
        rd: u8,
        preg: u8,
    },
    /// `addcmpb`: `rd = rd + a; compare rd with b; if <cond> branch to target`.
    AddCmpB {
        cond: Cond,
        rd: u8,
        a: RegOrImm,
        b: RegOrImm,
        target: u32,
    },
    /// `stm` — push `count` registers starting at `first` (wrapping r31->r0)
    /// to `(--sp)` in ascending memory order, then `lr` (if `include_lr`) at
    /// the top word of the frame.
    PushMulti {
        first: u8,
        count: u8,
        include_lr: bool,
    },
    /// `ldm` — pop `count` registers starting at `first` (wrapping) from
    /// `(sp++)`, then `pc` (if `include_pc`) from the top word of the frame.
    PopMulti {
        first: u8,
        count: u8,
        include_pc: bool,
    },
    /// A vector-unit instruction (48- or 80-bit, `0xF000..`), decoded to
    /// operands. Only the subset that touches no vector register is
    /// *executable* — see [`VecInsn::executable`].
    /// Boxed: `VecInsn` is 56 bytes and inflates `Op` — and so every decoded
    /// `Insn` — for a class of instruction the boot barely executes.
    Vector(Box<VecInsn>),
    /// Correctly sized but not decoded to semantics.
    Unimpl {
        raw: u64,
        len: u8,
        class: InsnClass,
    },
}

/// One operand slot of a vector instruction.
///
/// The Vector Register File is a 64x64 array of bytes; a vector register is a
/// 16-element window into it. The slot names that window with a 4-bit type
/// nibble — direction in bit 0, the granularity of the coordinate it carries in
/// the rest — plus a coordinate, a `+rN` scalar addend, and the `*` and `++`
/// modifiers. Types 14 and 15 are the "dash" slot, which names no register at
/// all: `videocoreiv.arch` spells its meaning per position as "Discard result
/// (D), Ignore (A), Use coordinate as Scalar (B)".
///
/// The fields are `binutils-vc4`'s (`print_vector_reg_1`, `opcodes/vc4-dis.c`),
/// checked against that disassembler over `start4.elf`'s whole `.text`: every
/// one of the 14650 vector instructions there spells its slots the same way.
///
/// The type nibble is not an element width. It says how coarse the `x` it
/// encodes is — H in steps of 16 bytes, HX in steps of 32, HY only 0 — and the
/// width of an element comes from the operation (`v8`/`v16`/`v32`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VecSlot {
    /// Type nibble. Bit 0 set = vertical (a column); >= 14 is the dash slot.
    pub ty: u8,
    /// Byte column of the window, as `binutils-vc4` objdump spells it. What
    /// the hardware addresses with is [`Self::e0`].
    pub x: u8,
    /// Index of the slot's first element within its row, in elements of
    /// [`Self::elem_bytes`] — the band the type nibble selects, times sixteen,
    /// plus the fine coordinate. Measured on a Raspberry Pi 4B d03115.
    pub e0: u8,
    /// Row of the window (horizontal), or the 16-row band it starts at
    /// (vertical).
    pub y: u8,
    /// `+rN`: a scalar register added to the coordinate. 15 = none.
    pub addend: u8,
    /// `*` — the coordinate is a column offset.
    pub star: bool,
    /// `++` — post-increment the coordinate.
    pub inc: bool,
    /// The scalar register a dash in the B position names: the 80-bit slot
    /// spells it in the same nibble as [`Self::addend`] (`r0..r15`), the 48-bit
    /// one in the coordinate field (`r0..r63`).
    pub scalar: u8,
    /// A dash B slot's signed displacement beside that register. Zero in the
    /// 48-bit forms, which have no room for it.
    pub disp: i32,
}

impl VecSlot {
    /// Decode a 16-bit (D/B) or 20-bit (A) slot composite, exactly as
    /// `print_vector_reg_1` reads it. `areg` selects the A slot's four extra
    /// low-order x bits.
    pub fn from_composite(comp: u32, areg: bool) -> VecSlot {
        let ty = ((comp >> 6) & 15) as u8;
        let fine = if areg { ((comp >> 16) & 15) as u8 } else { 0 };
        let star = (comp >> 10) & 1 != 0;
        let inc = (comp >> 11) & 1 != 0;
        let addend = ((comp >> 12) & 15) as u8;
        let low = (comp & 15) as u8;
        let band2 = (((comp >> 7) & 3) << 4) as u8;
        let band1 = (((comp >> 7) & 1) << 5) as u8;
        // A vertical slot's y is the 16-aligned base of the sixteen rows it
        // covers, and the low nibble of the coordinate is part of its x
        // instead — `binutils-vc4`'s V-direction fix, which `vc4.slaspec`
        // agrees with (`row = VaHi << 4, column = VaLo + 16 * base`).
        // The band is the same field in every family — two bits for the 8-bit
        // types, one for the 16-bit ones, none for the 32-bit — and it counts
        // in *sixteens of elements*, which is why the printed `x` (bytes) and
        // the element index part company as soon as an element is wider than a
        // byte. The fine coordinate is the A slot's extra nibble, or, for a
        // vertical operand, the low nibble of the coordinate itself.
        let vfine = if areg { fine } else { low };
        let (x, y, e0) = match ty {
            0 | 2 | 4 | 6 => (band2 | fine, (comp & 63) as u8, band2 | fine),
            8 | 10 => (band1 | fine, (comp & 63) as u8, (band1 >> 1) | fine),
            12 => (fine, (comp & 63) as u8, fine),
            1 | 3 | 5 | 7 => (
                band2 | vfine,
                (comp & if areg { 0x3F } else { 0x30 }) as u8,
                band2 | vfine,
            ),
            9 | 11 => (
                band1 | vfine,
                (comp & if areg { 0x3F } else { 0x30 }) as u8,
                (band1 >> 1) | vfine,
            ),
            13 => (vfine, (comp & if areg { 0x3F } else { 0x30 }) as u8, vfine),
            // Dash. The B position reads the addend nibble as a scalar register
            // and the rest as a signed 9-bit displacement.
            _ => (0, 0, 0),
        };
        let disp = if ty >= 14 {
            let raw = (comp & 0x7F) | (((comp >> 10) & 3) << 7);
            ((raw as i32) << 23) >> 23
        } else {
            0
        };
        VecSlot {
            ty,
            x,
            e0,
            y,
            addend,
            star,
            inc,
            scalar: addend,
            disp,
        }
    }

    /// Names no vector register.
    pub fn is_dash(self) -> bool {
        self.ty >= 14
    }

    /// A dash with no addend and no modifiers. Both types 14 and 15 spell one —
    /// a dash beside a vertical operand comes out as 15, since the direction
    /// bit is shared. `scalar`/`disp` are not part of it: they mean something
    /// only in the B position.
    pub fn is_bare_dash(self) -> bool {
        self.is_dash() && self.addend == 15 && !self.star && !self.inc
    }

    /// A column of the file rather than a row.
    pub fn is_vertical(self) -> bool {
        self.ty & 1 != 0
    }

    /// Width of one element of this slot's register, in bytes. The type nibble
    /// says it: `H`/`V` are 8-bit, `HX`/`VX` 16-bit, `HY`/`VY` 32-bit.
    ///
    /// This is the width of the *register*, not of the operation. A `v8ld` into
    /// an `HY` slot zero-extends each byte into a 32-bit element, and a `v32st`
    /// out of an `H` slot writes each 8-bit element as a word — measured both
    /// ways on a Raspberry Pi 4B d03115.
    pub fn elem_bytes(self) -> u8 {
        match self.ty >> 1 {
            0..=3 => 1,
            4 | 5 => 2,
            _ => 4,
        }
    }

    /// The 16-lane window this slot names.
    ///
    /// A dash names none. Everything else names sixteen elements of
    /// [`Self::elem_bytes`], laid along row `y` from element `e0`, or — for a
    /// vertical slot — down the sixteen rows from the band `y` names, all at
    /// element `e0`.
    pub fn window(self) -> Option<VecReg> {
        if self.is_dash() {
            return None;
        }
        Some(VecReg {
            y: self.y % 64,
            e0: self.e0,
            elem_bytes: self.elem_bytes(),
            vertical: self.is_vertical(),
        })
    }
}

/// A VRF register window: 16 elements of `elem_bytes` bytes each, laid along
/// row `y` from element `e0`, or down sixteen rows from `y` at element `e0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VecReg {
    pub y: u8,
    pub e0: u8,
    pub elem_bytes: u8,
    pub vertical: bool,
}

impl VecReg {
    /// Which row and which element of it lane `lane` lives in, after `step`
    /// repetitions of a `++`: horizontally `++` walks the rows, vertically it
    /// walks the elements (measured on a Raspberry Pi 4B d03115).
    pub fn lane(self, lane: u32, step: u32, addend: u32) -> (u8, u32) {
        let per_row = 64 / self.elem_bytes as u32;
        let e0 = self.e0 as u32 + addend;
        if self.vertical {
            ((self.y as u32 + lane) as u8 % 64, (e0 + step) % per_row)
        } else {
            ((self.y as u32 + step) as u8 % 64, (e0 + lane) % per_row)
        }
    }
}

/// How many times a vector instruction repeats (the `REP` field).
///
/// The fixed counts are powers of two; the top encoding takes the count from
/// `r0`. That reading is forced by `memcpy` itself: at `0x3EDA28EC` it computes
/// `r0 = min(blocks, 64)`, runs the `REP` load/store pair, then advances the
/// pointers by exactly `r0 * 64` bytes — which is only consistent if the pair
/// transferred `r0` rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecRep {
    Fixed(u32),
    FromR0,
}

/// Lane predication: which lanes of a vector instruction actually execute.
///
/// Each lane carries a zero, a negative and a carry flag, and an ALU op with
/// `SETF` writes them. The eight field values are `binutils-vc4`'s, and the
/// three flags they name are the ones a probe can see: `ALL`, `NONE`, `IFZ`,
/// `IFNZ`, `IFN`, `IFNN`, `IFC`, `IFNC` — measured on a Raspberry Pi 4B
/// d03115 by setting the flags with a known operation and marking the lanes
/// each predicate lets through.
///
/// `v<w>bitplanes -,rN SETF` is the firmware's own producer: its lane result
/// is that lane's bit of `rN`, so `IFZ` selects the lanes whose bit was 0.
/// Both polarities are pinned by code as well — `memcpy`'s tail
/// (`0x3EDA292C`) builds `~0 << n` and transfers under `IFZ`; `memset`'s
/// (`0x3EDA2B5E`) builds a band of set bits and stores under `IFNZ`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecPred {
    All,
    /// Field 1: no lane executes at all.
    NoLanes,
    /// Lanes whose zero flag is set — a `bitplanes` bit of 0.
    IfZero,
    IfNonZero,
    /// Lanes whose negative flag is set.
    IfNeg,
    IfNotNeg,
    /// Lanes whose carry flag is set.
    IfCarry,
    IfNotCarry,
}

impl VecPred {
    pub fn from_field(p: u8) -> Option<VecPred> {
        Some(match p {
            0 => VecPred::All,
            1 => VecPred::NoLanes,
            2 => VecPred::IfZero,
            3 => VecPred::IfNonZero,
            4 => VecPred::IfNeg,
            5 => VecPred::IfNotNeg,
            6 => VecPred::IfCarry,
            7 => VecPred::IfNotCarry,
            _ => return None,
        })
    }
}

/// The memory operand of a vector load/store: `(rbase + offset [+= rincr])`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VecAddr {
    pub base: u8,
    pub offset: u32,
    /// Scalar register added to the base after the transfer (80-bit forms).
    pub incr: Option<u8>,
}

/// Third operand of a vector instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecOperandB {
    /// A VRF register, or — when the slot is a dash — the scalar register named
    /// by its coordinate (an ALU source, or the base register of a memory op).
    Slot(VecSlot),
    /// 6-bit immediate (48-bit encodings) or 16-bit immediate (80-bit).
    Imm(u32),
}

/// Where a `readlut`/`writelut` takes its index from.
///
/// Measured with `probes/lut2.s` on a Raspberry Pi 4B d03115: a scalar
/// register or an immediate in the B position indexes every lane's table
/// alike — `v8memwrite -,A,(r2)` with `r2` = 3 and a vector index of threes
/// read the same byte back — which is the form `start4.elf` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecLutIndex {
    /// Each lane takes its own element of a vector slot.
    Lanes(VecOperand),
    /// Every lane takes the same index: a scalar register plus the
    /// displacement beside it, or — with no register — a bare immediate.
    Scalar { reg: Option<u8>, disp: i32 },
}

/// What the scalar result unit does with the sixteen lane results.
///
/// Measured on a Raspberry Pi 4B d03115 with `probes/sru.s` and `sru2.s`:
/// `SUMU` adds the lanes up reading each unsigned at the operation's width and
/// `SUMS` reading each signed; `MAX` answers the largest, signed; `IMIN` and
/// `IMAX` answer an *index*, the first of the smallest and the last of the
/// largest. `max2`, `max4` and `max6` answered exactly what `MAX` did in every
/// vector tried, so they are carried as the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecSruFunc {
    SumUnsigned,
    SumSigned,
    Max,
    IndexOfMin,
    IndexOfMax,
}

impl VecSruFunc {
    pub fn from_func(f: u8) -> Option<VecSruFunc> {
        Some(match f {
            0 => VecSruFunc::SumUnsigned,
            1 => VecSruFunc::SumSigned,
            2 | 4 | 6 | 7 => VecSruFunc::Max,
            3 => VecSruFunc::IndexOfMin,
            5 => VecSruFunc::IndexOfMax,
            _ => return None,
        })
    }
}

/// The scalar-result-unit / accumulator field of an 80-bit vector op.
///
/// Bit 6 selects the SRU (scalar writeback) group; then bits 3..5 pick the
/// function and bits 0..2 the scalar register. This split is confirmed by
/// `binutils-vc4`'s `print_vec80mods` and by `videocoreiv.arch`'s `S` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecSru {
    /// No scalar writeback, and no accumulator update.
    None,
    /// Accumulator update (`UADD`/`SACC`/...). Not modelled.
    Acc(u8),
    /// `SUMU`/`SUMS`/`IMIN`/`IMAX`/`MAX`... writing scalar `r<reg>`.
    Scalar { func: u8, reg: u8 },
}

impl VecSru {
    pub const SUMU: u8 = 0;
    pub const SUMS: u8 = 1;

    pub fn from_field(f: u8) -> VecSru {
        if f & 0x40 != 0 {
            VecSru::Scalar {
                func: (f >> 3) & 7,
                reg: f & 7,
            }
        } else if f & 0x3F != 0 {
            VecSru::Acc(f & 0x3F)
        } else {
            VecSru::None
        }
    }
}

/// A decoded vector-unit instruction.
///
/// Field layout transcribed from Herman Hermitage's `videocoreiv.arch` and
/// cross-checked, byte for byte, against `binutils-vc4`'s gas test corpus
/// (`gas/testsuite/gas/vc4/{dash,accmods,alu80-setf,wide,vldst}.d`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VecInsn {
    /// 80-bit encoding (`0xF800..`) rather than 48-bit (`0xF000..`).
    pub wide: bool,
    /// Memory class (`vld`/`vst`/`vmemread`/... `M` sub-op) rather than the
    /// ALU class (`vadd`/`vmov`/... `v` sub-op).
    pub mem: bool,
    /// `M` (0..31) for the memory class, `v` (0..63) for the ALU class.
    pub subop: u8,
    /// Element width in bits: 8, 16 or 32.
    pub lane_bits: u8,
    pub d: VecSlot,
    pub a: VecSlot,
    pub b: VecOperandB,
    /// Set for a memory-class op whose B slot is a dash, i.e. one that
    /// addresses memory rather than naming a third vector register.
    pub addr: Option<VecAddr>,
    /// `SETF` — update the per-lane vector flags.
    pub setf: bool,
    /// `REP` field: 0 = execute once.
    pub rep: u8,
    /// Lane predication (`IFZ`/`IFNZ`/...): 0 = all lanes.
    pub pred: u8,
    /// Accumulator / scalar-writeback modifier (80-bit encodings only).
    pub sru: VecSru,
    /// The instruction word, most significant parcel first.
    pub raw: u128,
    pub len: u8,
}

/// A vector ALU operation whose semantics have been measured.
///
/// Every one of these was run on a Raspberry Pi 4B d03115 against two vectors
/// of edge cases — `0x7fff`, `0x8000`, `0xffff`, shift counts of 0 and 15 —
/// and the result read back out of the register file
/// (`examples-on-real-hardware/vpu-probe/probes/alu.s`). The ops not listed
/// here are the ones those runs did not pin down: the carry forms, `clips`,
/// `testmag`, the `sign*` shifts, the multiplies and the unnamed sub-ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecAluOp {
    /// `d = b`.
    Mov,
    /// `d = a[2i]` / `d = a[2i+1]`: the even or odd elements of A, packed down.
    Even,
    Odd,
    /// Alternate elements of A and B, from the low half or the high half.
    Interl,
    Interh,
    /// Reverse the low `n` bits of A, `n` being B's low nibble — or all 16
    /// (32, 8) of them when that nibble is zero.
    Brev,
    Ror,
    Shl,
    /// Shift left, saturating signed.
    Shls,
    Lsr,
    Asr,
    And,
    Or,
    Eor,
    /// `a & !b`.
    Bic,
    /// `popcount(a) + popcount(b)`.
    Count,
    /// Index of B's highest set bit.
    Msb,
    /// Signed.
    Min,
    Max,
    /// `|a - b|`, wrapping, and its saturating form.
    Dist,
    Dists,
    /// `a` clamped to `0 ..= b`, signed.
    Clip,
    /// `b + signum(a)`.
    Sign,
    /// Transpose the lanes' bits: lane `i` of the result is the word whose bit
    /// `j` is bit `i` of lane `j` of B. With a scalar or immediate B — which
    /// every lane sees alike — that comes out as "all ones where B's bit `i`
    /// is set", which is what the firmware uses it for, one lane flag per bit.
    Bitplanes,
    /// `b * signum(a)`, `signum(0)` counting as `+1` — the `v32` form of the
    /// sub-op whose `v16` form writes zeros.
    Clips,
    /// `1` where `|a| >= b`, `0` otherwise — B read signed, A's magnitude
    /// against it, so a negative B always answers 1.
    Testmag,
    /// Shift by a *signed*, unmasked count: left when B is positive, right
    /// when it is negative, and a count past the element's width empties it.
    /// `Signshl` shifts in zeros on the way right, `Signasl` copies the sign,
    /// and `Signasls` is the saturating form of `Signasl`.
    Signshl,
    Signasl,
    Signasls,
    /// A sub-op whose result is a lane of zeros, at this width: the unit
    /// writes the register, it just writes nothing in it. Measured over a
    /// destination preset to all-ones, so this is a write and not a skip.
    Zero,
    Add,
    /// The `c` forms take the lane's carry flag in as well: `a + b + c` and
    /// `a - b - c`, the carry being a borrow on the way out.
    Addc,
    Addsc,
    Subc,
    Subsc,
    Rsubc,
    Rsubsc,
    /// Saturating signed.
    Adds,
    Sub,
    Subs,
    /// `b - a`.
    Rsub,
    Rsubs,
    /// The low half of the product, and its saturating form.
    Mull,
    Mulls,
    /// The product shifted right by 8 — a fixed-point multiply — and its
    /// saturating form.
    Mulm,
    Mulms,
    /// The high half of the product: `(a * b) >> bits`, with each operand read
    /// signed or unsigned as the mnemonic's suffix says.
    Mulhd {
        sa: bool,
        sb: bool,
    },
    /// `vmul32.xx`: the `L`-bit family. A **16 x 16 into 32** multiply — the
    /// low halfword of each operand, read signed or unsigned as the suffix
    /// says, and the whole 32-bit product. Measured: `vmul32.ss` over
    /// `0x12345678 * 0x10` answers `0x00056780`, the low halfword's product.
    Mul32 {
        sa: bool,
        sb: bool,
    },
    /// The high half **truncated** instead of floored: the community
    /// programmers manual calls sub-ops 60 and 61 `mulhdt`, "round to zero",
    /// and the board agrees — `0x0ff0 * 0xfff1` answers `0x0000` where
    /// `mulhd` answers `0xffff`. Only the `ss` and `su` sign pairs exist.
    Mulhdt {
        sa: bool,
        sb: bool,
    },
    /// The same, rounded: `(a * b + half) >> bits`.
    Mulhn {
        sa: bool,
        sb: bool,
    },
}

impl VecAluOp {
    /// The `v` sub-op field, as `insn-vecops` numbers it.
    /// Some sub-ops mean one thing at one width and write zeros at the other,
    /// so the width is part of the identity: `count` computes at `v16` and
    /// writes zeros at `v32`, `testmag` likewise, and sub-op 30 is the other
    /// way round — zeros at `v16`, `b * signum(a)` at `v32`.
    pub fn from_subop(subop: u8, width: u32) -> Option<VecAluOp> {
        use VecAluOp::*;
        Some(match subop {
            0 => Mov,
            1 if width == 2 => Bitplanes,
            2 => Even,
            3 => Odd,
            4 => Interl,
            5 => Interh,
            6 => Brev,
            7 => Ror,
            8 => Shl,
            9 => Shls,
            10 => Lsr,
            11 => Asr,
            16 => And,
            17 => Or,
            18 => Eor,
            19 => Bic,
            20 if width == 2 => Count,
            21 => Msb,
            24 => Min,
            25 => Max,
            26 => Dist,
            27 => Dists,
            28 => Clip,
            29 => Sign,
            30 if width == 4 => Clips,
            31 if width == 2 => Testmag,
            32 => Add,
            33 => Adds,
            34 => Addc,
            35 => Addsc,
            36 => Sub,
            37 => Subs,
            38 => Subc,
            39 => Subsc,
            40 => Rsub,
            41 => Rsubs,
            42 => Rsubc,
            43 => Rsubsc,
            12 => Signshl,
            14 => Signasl,
            15 => Signasls,
            // Measured on a Raspberry Pi 4B d03115: each of these leaves every
            // lane zero, over a destination preset to all-ones.
            1 | 13 | 20 | 22 | 23 | 30 | 31 | 44..=47 => Zero,
            _ => return None,
        })
    }

    /// The multiply group, `insn-vecmulops` (sub-ops 48 and up with the `L`
    /// bit clear). `L` set selects a different family, which is not modelled.
    pub fn from_mul_subop(subop: u8, width: u32) -> Option<VecAluOp> {
        use VecAluOp::*;
        let signs = |n: u8| (n & 2 == 0, n & 1 == 0);
        // The `L` bit — a `v32` width on a multiply — selects the 16x16 family
        // instead, and only its four sign combinations are known.
        if width == 4 {
            let (sa, sb) = signs(subop);
            return (52..=55).contains(&subop).then_some(Mul32 { sa, sb });
        }
        Some(match subop {
            48 => Mull,
            49 => Mulls,
            50 => Mulm,
            51 => Mulms,
            52..=55 => {
                let (sa, sb) = signs(subop - 52);
                Mulhd { sa, sb }
            }
            56..=59 => {
                let (sa, sb) = signs(subop - 56);
                Mulhn { sa, sb }
            }
            // 60 and 61 truncate where `mulhd` floors; 62 and 63 are unused
            // and write a lane of zeros, like the other blanks.
            60 | 61 => {
                let (sa, sb) = signs(subop - 60);
                Mulhdt { sa, sb }
            }
            62 | 63 => Zero,
            _ => return None,
        })
    }

    /// Which element of A and of B lane `i` reads. All but the four shuffles
    /// read their own lane.
    ///
    /// `even` and `odd` pack A's alternate elements into the low eight lanes
    /// and B's into the high eight; the two interleaves alternate between the
    /// two registers. Measured, like the rest of it, on a Pi 4B d03115.
    pub fn sources(self, i: u32) -> (u32, u32) {
        use VecAluOp::*;
        let half = i % 8;
        match self {
            Even => (2 * half, 2 * half),
            Odd => (2 * half + 1, 2 * half + 1),
            Interl => (i / 2, i / 2),
            Interh => (8 + i / 2, 8 + i / 2),
            _ => (i, i),
        }
    }

    /// Whether lane `i` of a shuffle takes its result from B rather than A.
    pub fn takes_b(self, i: u32) -> bool {
        use VecAluOp::*;
        match self {
            Even | Odd => i >= 8,
            Interl | Interh => i % 2 == 1,
            _ => false,
        }
    }
}

/// The accumulator side of an ALU op, as the `ENA` group of the modifier field
/// spells it.
///
/// Each lane has an accumulator of its own. `CLRA` clears it before the
/// operation, the result is added to it, read signed or unsigned as `SIGN`
/// says, and `WBA` makes the destination take the accumulator rather than the
/// raw result. `SUB` is not a subtracting accumulate: it leaves the
/// accumulator untouched and hands the destination `accumulator - result`. Measured on a Raspberry Pi 4B
/// d03115: `v16add -,A,B CLRA UACC` followed by `v16add D,A,B UACC` leaves
/// twice the sum in `D`, and the `0xffff` case proves the unsigned reading.
///
/// The `HIGH` forms — `UACCH` and friends — accumulate into the high half:
/// the result is shifted left by sixteen on the way in, and a write-back
/// shifts it back down on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VecAcc {
    /// `ENA`. Without it nothing is accumulated and nothing is written back —
    /// but `CLRA` still clears, measured — so the modifier is carried rather
    /// than dropped.
    pub enable: bool,
    pub clear: bool,
    pub signed: bool,
    pub sub: bool,
    pub writeback: bool,
    /// The `...H` forms: the result goes into the accumulator's **high** half
    /// — shifted left by sixteen — and a write-back reads it back shifted
    /// right by sixteen, saturated into the destination's signed range.
    /// Measured with `probes/acch.s`, reading the accumulator itself with
    /// `vgetacc`: `v16mov -,A CLRA UACCH` leaves `A << 16` in it.
    pub high: bool,
}

impl VecAcc {
    /// From the 7-bit modifier field, or `None` when it is not a plain
    /// accumulate this model knows.
    pub fn from_field(f: u8) -> Option<VecAcc> {
        const ENA: u8 = 0x20;
        const HIGH: u8 = 0x10;
        const SIGN: u8 = 0x08;
        const CLRA: u8 = 0x04;
        const WBA: u8 = 0x02;
        const SUB: u8 = 0x01;
        // `ENA` off is not an error: `CLRA` alone clears the accumulator and
        // leaves the destination the raw result, and `WBA` alone does nothing
        // at all. Measured with `probes/noena.s`.
        // A masked-off lane keeps the flags it had: `SETF` under `IFZ` leaves
        // the lanes the predicate dropped exactly as the last `SETF` left
        // them, measured.
        // `SUB` hands the destination `accumulator - result` whether or not
        // `WBA` is set — `SDEC` in `probes/accmix.s` is that combination, and
        // the board answers the same difference. With the high half it is the
        // same difference taken sixteen bits up, `(acc - (result << 16)) >> 16`
        // — for the signed form. The unsigned one answered neither that nor a
        // clamped version of it, lane for lane, so it is left to fault.
        if f & SUB != 0 && f & HIGH != 0 && f & SIGN == 0 {
            return None;
        }
        Some(VecAcc {
            enable: f & ENA != 0,
            clear: f & CLRA != 0,
            signed: f & SIGN != 0,
            sub: f & SUB != 0,
            writeback: f & WBA != 0,
            high: f & HIGH != 0,
        })
    }
}

/// One operand slot resolved for execution: the window, and the scalar register
/// whose value is added to its element index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VecOperand {
    pub reg: VecReg,
    pub addend: Option<u8>,
}

/// The third operand of an ALU op: another register, a scalar, or an immediate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecSource {
    Reg(VecOperand),
    /// A dash in the B position names a scalar register, and may carry a
    /// signed displacement beside it — `r2-1`, as `binutils-vc4` prints it.
    /// The operand is `reg + disp`, measured with `probes/sdisp.s`: with
    /// `r2` = 100, `r2-1` reaches the lanes as 99 and `r2+100` as 200.
    Scalar {
        reg: u8,
        disp: i32,
    },
    Imm(i32),
}

/// How much of a vector instruction this model can actually carry out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecExec {
    /// A memory read whose destination is discarded: `v<w>ld -, (rN)`. Reads
    /// `16 * lane_bytes` bytes from `rN + offset` and throws them away.
    DiscardedLoad { base: u8, offset: u32, bytes: u32 },
    /// `v<w>mov -, rN SUM{S,U} rK`: broadcast a scalar across the 16 lanes,
    /// discard the vector result, write the sum of the lanes back to a scalar
    /// register.
    SumOfBroadcast { src: u8, dst: u8, signed: bool },
    /// `v<w>{ld,st} <reg>[++][+rA],(r<base>+off[+=r<incr>]) [REP n]` — transfer
    /// 16 elements between the register file and memory, `reps` times. Each
    /// repetition steps the address by `r<incr>` and, with `++`, the register
    /// by one (a row horizontally, an element vertically).
    Mem {
        store: bool,
        /// Absent when the vector slot is a dash: the load still reads its
        /// bytes — the firmware uses that to fence outstanding reads — and
        /// writes none of them anywhere.
        reg: Option<VecReg>,
        /// `++` on the vector slot.
        step: bool,
        /// The scalar register the address starts from, or `None` for the
        /// forms that name no address at all — a `vld` whose B slot holds a
        /// vector. Those read from **zero**: measured with `probes/mld.s`,
        /// four such forms all came back with the same sixteen words from
        /// address 0, whatever their operands held.
        base: Option<u8>,
        /// Byte displacement on the address, measured as such.
        offset: u32,
        /// `+rN` on the vector slot: a scalar added to its element index.
        addend: Option<u8>,
        incr: Option<u8>,
        reps: VecRep,
        pred: VecPred,
        /// Element width of the *operation*, in bytes — what each lane moves to
        /// or from memory, converted to the register's own element width.
        width: u32,
    },
    /// `v<w>mov <reg>[++],r<n>` / `v<w>mov <reg>[++],#imm [REP n]` — broadcast a
    /// scalar or an immediate across the 16 lanes of a VRF register, for `reps`
    /// consecutive rows when `step_row` (the `++` modifier) is set. The 48-bit
    /// form is a single row (`reps = Fixed(1)`, `step_row = false`); the 80-bit
    /// `REP` form clears a band of rows, which is how the boot ROM zeroes memory.
    Broadcast {
        reg: VecReg,
        src: RegOrImm,
        reps: VecRep,
        step: bool,
        addend: Option<u8>,
    },
    /// `v<w>bitplanes -,r<n> SETF` — set the per-lane flags from the low 16 bits
    /// of a scalar; the vector result goes to a dash and is discarded.
    Bitplanes { src: u8 },
    /// `v<w><op> <d>,<a>,<b>` — the measured ALU ops, lane by lane.
    Alu {
        op: VecAluOp,
        /// Absent when the destination is a dash: the lanes are computed for
        /// the accumulator's sake and the result itself is dropped.
        d: Option<VecOperand>,
        /// Absent for the ops that read only B.
        a: Option<VecOperand>,
        b: VecSource,
        reps: VecRep,
        /// `++` on the D slot, and on A when it has one.
        step_d: bool,
        step_a: bool,
        pred: VecPred,
        /// Element width the operation works at, in bytes.
        width: u32,
        /// The accumulator, when the op carries one.
        acc: Option<VecAcc>,
        /// `SETF`: leave the lane flags holding this result.
        setf: bool,
        /// The scalar result unit: which aggregate of the sixteen lane results
        /// to write, and the scalar register to write it to.
        sru: Option<(VecSruFunc, u8)>,
    },
    /// `v<w>lookupm[l] <d>,(r<base>+off)` — a **gather**: each lane reads the
    /// element `index` of the table at that address, `index` being its own
    /// accumulator's high half (`lookupm`) or low half (`lookupml`) and the
    /// element as wide as the operation. `v8lookupml` with `4` in a lane's
    /// accumulator reads the byte at `base + 4`; `v16lookupml` reads the
    /// halfword at `base + 8`.
    Gather {
        /// A dash destination reads and discards — measured with
        /// `probes/r63c.s`: a witness register came back untouched, and the
        /// board went on running. The shape `start4.elf` uses.
        d: Option<VecOperand>,
        /// `None` where the address names `r63`, which is no register at all:
        /// the gather then reads from zero, measured with `probes/r63.s`.
        base: Option<u8>,
        offset: u32,
        /// Take the index from the accumulator's high half.
        high: bool,
        width: u32,
        reps: VecRep,
        step_d: bool,
        pred: VecPred,
    },
    /// `v<w>indexwritem[l] <a>,(r<base>+off)` — the **scatter** that mirrors
    /// it: each lane writes its element to `base + index * width`.
    Scatter {
        src: VecOperand,
        base: u8,
        offset: u32,
        high: bool,
        width: u32,
        reps: VecRep,
        step_a: bool,
        pred: VecPred,
    },
    /// `v<w>mem07 <d>,<a>,<b>` — writes a lane of zeros and does nothing else
    /// a probe can see. Measured with `probes/m07.s` and `probes/addr07.s` on
    /// a Raspberry Pi 4B d03115: an operand that is a valid bus address is not
    /// written to, the lookup table does not change, and the board goes on
    /// running — unlike its neighbours 11-15, 17, 18 and 20, which take the
    /// firmware down with them.
    Zeros {
        d: VecOperand,
        width: u32,
        reps: VecRep,
        step_d: bool,
        pred: VecPred,
    },
    /// `v<w>memread <d>,<a>,<b>` / `v<w>memwrite <d>,<a>,<b>` — the unit's own
    /// 1 KiB lookup table, which is what those two mnemonics really address.
    /// A read answers the lane's own `lut[b * width]` and ignores A; a write
    /// puts A's element there, and hands the destination the same value. The
    /// table is banked per lane — see [`crate::vpu::vrf::LUT_LANE`].
    Lut {
        write: bool,
        d: Option<VecOperand>,
        a: Option<VecOperand>,
        index: VecLutIndex,
        width: u32,
        reps: VecRep,
        step: bool,
        pred: VecPred,
    },
    /// `vgetacc[s16|s32] <d>,<a>,<b>` — each lane's accumulator, shifted right
    /// by `b & 31`. The A slot is read and discarded, `sat` says which signed
    /// range the value is clamped into on the way out, and the destination's
    /// own element width truncates what is left.
    GetAcc {
        /// A dash destination discards the value and leaves only the scalar
        /// result unit's aggregate — the form `start4.elf` uses.
        d: Option<VecOperand>,
        b: VecSource,
        /// Bytes the result saturates into: `s16` and `s32`, or nothing at all.
        sat: Option<u32>,
        reps: VecRep,
        step_d: bool,
        pred: VecPred,
        /// `SUMU`/`SUMS`/`MAX`/`IMIN`/`IMAX` over the sixteen lane values,
        /// written to a scalar register.
        sru: Option<(VecSruFunc, u8)>,
    },
    /// Runs and leaves nothing behind that a probe could find: the stores and
    /// scatters that name no address. A `v<w>st` whose B slot holds a vector
    /// writes neither where its operands point, nor to address 0 — where its
    /// *load* counterpart reads — nor anywhere in the 64 KiB a probe watches
    /// byte for byte; a `v<w>indexwritem` with a dash source is the same.
    /// Measured with `probes/st63.s`, `st63b.s`, `st63c.s` and `st64.s` on a
    /// Raspberry Pi 4B d03115, the last of them through `vpuprobe3.py`, which
    /// compares the whole allocation before and after.
    NoEffect,
    /// Needs a part of the vector unit this model does not implement.
    NeedsVrf,
}

/// Read a field of `n` bits out of an instruction word, `pos` bits from its
/// most significant end — how `videocoreiv.arch` writes its patterns.
const fn vfield(raw: u128, width: u32, pos: u32, n: u32) -> u32 {
    ((raw >> (width - pos - n)) & ((1u128 << n) - 1)) as u32
}

impl VecInsn {
    /// Number of lanes in a vector register. Fixed by the architecture.
    pub const LANES: u32 = 16;

    /// Classify this instruction for the executor.
    ///
    /// Only a few forms are executable, and every one of them is matched
    /// *exactly*: a whole-word template with only the fields whose meaning is
    /// established left free, and a value whitelist on each of those. The
    /// encoding has plenty of corners this decoder renders only approximately
    /// (per-slot `+rN` addends, fine-x coordinate bits, vertical slots, the
    /// accumulator modifiers), and anything outside the template — a set bit in
    /// a field this model does not interpret, a vertical slot, an unknown
    /// predicate — falls through to [`VecExec::NeedsVrf`] and faults.
    ///
    /// The two `-`-destination forms come from `FUN_0edc9e20` in `start4.elf`,
    /// the routine that flushes the vector unit's outstanding reads before the
    /// VRF semaphore is released. The load/store, broadcast and `bitplanes`
    /// forms are the ones VC4 libc's `memcpy` (`0x3EDA28D6`), `memmove`
    /// (`0x3EDA2A00`) and `memset` (`0x3EDA2AB4`) are built out of; all were
    /// read back with `binutils-vc4` objdump to confirm the decoding.
    pub fn executable(&self) -> VecExec {
        // `v<w>ld -,(rN)` — a load with a discarded destination. Free fields:
        // the 2-bit width (bits 11..12) and the base register (bits 42..47).
        const LD48: u128 = 0xF000_E038_0380;
        const LD48_FREE: u128 = (3 << 35) | 0x3F;
        // `v<w>mov -,rN SUM{U,S} rK` — broadcast a scalar over the lanes,
        // discard the vector result, sum the lanes back into a scalar. Free
        // fields: the operation size L (bit 6), the source register
        // (bits 42..47) and the SRU selector (bits 67..73).
        const MOV80: u128 = 0xFC00_E038_0380_F3C0_1200;
        const MOV80_FREE: u128 = (1 << 73) | (0x3F << 32) | (0x7F << 6);

        if self.len == 6 && self.raw & !LD48_FREE == LD48 & !LD48_FREE {
            return VecExec::DiscardedLoad {
                base: (self.raw & 0x3F) as u8,
                offset: 0,
                bytes: Self::LANES * (self.lane_bits as u32 / 8),
            };
        }
        if self.len == 10 && self.raw & !MOV80_FREE == MOV80 & !MOV80_FREE {
            // Only the two sum functions; the others (IMIN/IMAX/MAX) write a
            // lane *index*, which needs real lanes.
            if let VecSru::Scalar { func, reg } = self.sru {
                if func == VecSru::SUMU || func == VecSru::SUMS {
                    return VecExec::SumOfBroadcast {
                        src: ((self.raw >> 32) & 0x3F) as u8,
                        dst: reg,
                        signed: func == VecSru::SUMS,
                    };
                }
            }
        }
        if self.mem {
            if let Some(e) = self
                .mem_transfer()
                .or_else(|| self.getacc())
                .or_else(|| self.gather())
                .or_else(|| self.lut())
                .or_else(|| self.zeros())
            {
                return e;
            }
        } else if let Some(e) = self.alu48().or_else(|| self.alu80()).or_else(|| self.alu()) {
            return e;
        }
        VecExec::NeedsVrf
    }

    /// `v<w>ld` / `v<w>st` between the register file and memory, in both the
    /// 48-bit and the 80-bit encoding.
    ///
    /// The vector operand is the D slot for a load and the A slot for a store;
    /// the other slot must be a dash, whose addend nibble — in the 80-bit form
    /// — is the register the address steps by between repetitions. Every field
    /// of this encoding has a meaning now, so this is a field test rather than
    /// a whole-word template: all it refuses is a `*` on the vector slot.
    /// `SETF` on a transfer is accepted and ignored — measured: a load or a
    /// store with the bit set leaves all three lane flags exactly as they
    /// were.
    /// Whether the B slot is a dash — the shape that spells an address.
    fn b_is_dash(&self) -> bool {
        matches!(self.b, VecOperandB::Slot(s) if s.is_dash())
    }

    fn mem_transfer(&self) -> Option<VecExec> {
        let width = if self.wide { 80 } else { 48 };
        // `WW` 3 is not a width this decoder knows; 0/1/2 are 8/16/32.
        if vfield(self.raw, width, 11, 2) > 2 {
            return None;
        }
        let store = match self.subop {
            0 => false,
            4 => true,
            _ => return None,
        };
        let (vec_slot, dash) = if store {
            (self.a, self.d)
        } else {
            (self.d, self.a)
        };
        // The inert slot carries nothing of its own except — in the 80-bit
        // encoding — the addend nibble the address reads as its `+=` step.
        // In the 48-bit encoding a **load** ignores that slot altogether:
        // measured, `v16ld HX(1,0),-+r5,(r4)` and `v16ld HX(3,0),HX(20,0),(r4)`
        // both move exactly what the plain load moves. A store is another
        // matter — one with an addend wrote nothing where the plain one wrote
        // — so there the slot still has to be a bare dash.
        let inert_free = !self.wide && !store;
        if !inert_free && (!dash.is_dash() || dash.star || dash.inc) {
            // A store that names no address at all writes nothing, whatever
            // its slots hold — measured with `probes/st64.s`, which watched
            // the whole allocation and address 0 either side of one.
            if store && self.addr.is_none() && !self.b_is_dash() {
                return Some(VecExec::NoEffect);
            }
            return None;
        }
        if !inert_free && !self.wide && dash.addend != 15 {
            return None;
        }
        if inert_free && dash.inc {
            return None; // what `++` on the inert slot steps was not measured
        }

        // A transfer whose B slot holds a vector names no address: those bits
        // *are* the address composite in the forms that have one. The load
        // reads from zero and the store writes nowhere a probe can find —
        // both measured.
        let addr = match self.addr {
            Some(addr) => addr,
            None if self.b_is_dash() => return None,
            None if store => return Some(VecExec::NoEffect),
            None => VecAddr {
                base: 63,
                offset: 0,
                incr: None,
            },
        };
        let no_base = addr.base == 63 && self.addr.is_none();
        // A dash in the vector position discards the transfer's data. That is
        // a load with nowhere to put it — `FUN_0edc9e20`'s read fence — and a
        // store with nothing to write, which is not modelled.
        if vec_slot.is_dash() && store {
            return None;
        }
        Some(VecExec::Mem {
            store,
            reg: if vec_slot.is_dash() {
                None
            } else {
                Some(vec_slot.window()?)
            },
            step: vec_slot.inc,
            base: (!no_base).then_some(addr.base),
            offset: addr.offset,
            addend: (vec_slot.addend != 15).then_some(vec_slot.addend),
            incr: addr.incr,
            width: self.lane_bits as u32 / 8,
            reps: match self.rep {
                7 => VecRep::FromR0,
                n => VecRep::Fixed(1 << n),
            },
            pred: VecPred::from_field(self.pred)?,
        })
    }

    /// The gather and scatter sub-ops, `lookupm`/`lookupml` and
    /// `indexwritem`/`indexwriteml`.
    ///
    /// Measured on a Raspberry Pi 4B d03115 with `probes/mem5.s`..`mem7.s`:
    /// the index is the lane's own accumulator — its low half for the `l`
    /// forms, its high half for the others — scaled by the operation's element
    /// width, and the address is the ordinary base plus displacement.
    fn gather(&self) -> Option<VecExec> {
        let (scatter, high) = match self.subop {
            1 => (false, true),
            2 => (false, false),
            5 => (true, true),
            6 => (true, false),
            _ => return None,
        };
        let width = match self.lane_bits {
            8 => 1,
            16 => 2,
            32 => 4,
            _ => return None,
        };
        let (vec_slot, dash) = if scatter {
            (self.a, self.d)
        } else {
            (self.d, self.a)
        };
        // A scatter with a dash *source* has nothing to write, and writes
        // nothing — measured with `probes/st64.s`, which watched the whole
        // allocation and address 0 either side of two of them. What its other
        // slots hold makes no difference.
        if scatter && vec_slot.is_dash() {
            return Some(VecExec::NoEffect);
        }
        // A gather reads its inert slot for nothing — measured with
        // `probes/r63.s`: the same address answered the same bytes with a
        // vector of junk in that slot as with one of zeros. A scatter's inert
        // slot was not measured, so there it still has to be a bare dash.
        if scatter && (!dash.is_dash() || dash.inc) {
            return None;
        }
        if !scatter && dash.inc {
            return None;
        }
        // A gather whose B slot holds a vector names no address either — the
        // bits are the slot — and reads from zero, the same as `(r63)`.
        // Measured with `probes/lkc.s`: with 16 in the accumulator's high
        // half, both forms answer the byte at address 16.
        let addr = match self.addr {
            Some(addr) => addr,
            None if !scatter => VecAddr {
                base: 63,
                offset: 0,
                incr: None,
            },
            None => return None,
        };
        let reps = match self.rep {
            7 => VecRep::FromR0,
            n => VecRep::Fixed(1 << n),
        };
        // A `+=` step only means something across repetitions; what it does
        // between them was not measured.
        if addr.incr.is_some() && reps != VecRep::Fixed(1) {
            return None;
        }
        let operand = if vec_slot.is_dash() {
            if scatter {
                // Nothing to write, and measured to write nothing.
                return Some(VecExec::NoEffect);
            }
            None
        } else {
            Some(VecOperand {
                reg: vec_slot.window()?,
                addend: (vec_slot.addend != 15).then_some(vec_slot.addend),
            })
        };
        let pred = VecPred::from_field(self.pred)?;
        Some(if scatter {
            VecExec::Scatter {
                src: operand?,
                base: addr.base,
                offset: addr.offset,
                high,
                width,
                reps,
                step_a: vec_slot.inc,
                pred,
            }
        } else {
            VecExec::Gather {
                d: operand,
                // `r63` is the encoding's way of naming no base at all.
                base: (addr.base != 63).then_some(addr.base),
                offset: addr.offset,
                high,
                width,
                reps,
                step_d: vec_slot.inc,
                pred,
            }
        })
    }

    /// `memread` and `memwrite`: the vector unit's 1 KiB lookup table.
    ///
    /// Measured with `probes/lut.s` on a Raspberry Pi 4B d03115 —
    /// `v8memwrite -,A,B` then `v8memread D,A,B` over the same indices hands
    /// back exactly what was written, and the `v16` pair round-trips halfwords
    /// at twice the index. The index is the B slot, scaled by the operation's
    /// element width; the A slot is the data a write stores and is read for
    /// nothing by a read.
    fn lut(&self) -> Option<VecExec> {
        let write = match self.subop {
            8 => false,
            9 => true,
            _ => return None,
        };
        let width = match self.lane_bits {
            8 => 1,
            16 => 2,
            32 => 4,
            _ => return None,
        };
        let operand = |slot: VecSlot| -> Option<VecOperand> {
            Some(VecOperand {
                reg: slot.window()?,
                addend: (slot.addend != 15).then_some(slot.addend),
            })
        };
        let a = if self.a.is_dash() {
            if write {
                return None; // nothing to write
            }
            None
        } else {
            Some(operand(self.a)?)
        };
        let index = match self.b {
            VecOperandB::Imm(v) => VecLutIndex::Scalar {
                reg: None,
                disp: v as i32,
            },
            // A dash in the B position of the 80-bit form names a scalar
            // register — plus a displacement, which adds to it — and every
            // lane then indexes its own table with that.
            VecOperandB::Slot(s) if s.is_dash() => VecLutIndex::Scalar {
                reg: Some(s.scalar),
                disp: s.disp,
            },
            VecOperandB::Slot(s) => VecLutIndex::Lanes(operand(s)?),
        };
        Some(VecExec::Lut {
            write,
            d: (!self.d.is_dash()).then(|| operand(self.d)).flatten(),
            a,
            index,
            width,
            reps: match self.rep {
                7 => VecRep::FromR0,
                n => VecRep::Fixed(1 << n),
            },
            step: self.d.inc || self.a.inc,
            pred: VecPred::from_field(self.pred)?,
        })
    }

    /// `vgetacc[s16|s32] <d>,<a>,<b>` — the memory-class sub-op that reads the
    /// accumulator back out.
    ///
    /// Measured on a Raspberry Pi 4B d03115: the value is the lane's whole
    /// accumulator shifted right by `b & 31` — five bits, because the
    /// accumulator is wider than an element — and the A slot is read for
    /// nothing, a register of zeros giving the same answer as one of data.
    /// The width field picks the saturation rather than an element size:
    /// `v8` is the plain form, `v16` clamps into a signed 32-bit range and
    /// `v32` into a signed 16-bit one.
    /// Memory sub-op 7, the one blank in the class that is safe to carry out.
    fn zeros(&self) -> Option<VecExec> {
        if self.subop != 7 || !self.mem || self.d.is_dash() {
            return None;
        }
        let width = match self.lane_bits {
            8 => 1,
            16 => 2,
            32 => 4,
            _ => return None,
        };
        Some(VecExec::Zeros {
            d: VecOperand {
                reg: self.d.window()?,
                addend: (self.d.addend != 15).then_some(self.d.addend),
            },
            width,
            reps: match self.rep {
                7 => VecRep::FromR0,
                n => VecRep::Fixed(1 << n),
            },
            step_d: self.d.inc,
            pred: VecPred::from_field(self.pred)?,
        })
    }

    fn getacc(&self) -> Option<VecExec> {
        if self.subop != 24 || !self.mem {
            return None;
        }
        let sat = match self.lane_bits {
            8 => None,
            16 => Some(4),
            32 => Some(2),
            _ => return None,
        };
        // The accumulator's own update field means nothing here; only the
        // scalar aggregate does.
        let sru = match self.sru {
            VecSru::None => None,
            VecSru::Scalar { func, reg } => Some((VecSruFunc::from_func(func)?, reg)),
            VecSru::Acc(_) => return None,
        };
        let b = match self.b {
            VecOperandB::Imm(i) => VecSource::Imm(self.imm_value(i)),
            VecOperandB::Slot(sl) if sl.is_dash() => {
                if sl.scalar >= 32 {
                    return None;
                }
                VecSource::Scalar {
                    reg: sl.scalar,
                    disp: sl.disp,
                }
            }
            VecOperandB::Slot(sl) => VecSource::Reg(VecOperand {
                reg: sl.window()?,
                addend: (sl.addend != 15).then_some(sl.addend),
            }),
        };
        Some(VecExec::GetAcc {
            d: if self.d.is_dash() {
                None
            } else {
                Some(VecOperand {
                    reg: self.d.window()?,
                    addend: (self.d.addend != 15).then_some(self.d.addend),
                })
            },
            b,
            sat,
            reps: match self.rep {
                7 => VecRep::FromR0,
                n => VecRep::Fixed(1 << n),
            },
            step_d: self.d.inc,
            pred: VecPred::from_field(self.pred)?,
            sru,
        })
    }

    /// The two 48-bit ALU-class forms the libc string routines use:
    ///
    /// ```text
    ///   v<w>mov <reg>,r<n>    1111 01Lv vvvv v000 VVV0 dddddd 1110 000000 0 0 111 0 bbbbbb
    ///   v<w>mov <reg>,#imm    1111 01Lv vvvv v000 VVV0 dddddd 1110 000000 0 1 000 0 iiiiii
    ///   v<w>bitplanes -,r<n>  1111 01L0 0000 1000 1110 000000 1110 000000 0 0 111 1 bbbbbb
    /// ```
    ///
    /// `bitplanes` writes no register (its destination is a dash) — the whole
    /// point of it here is `SETF`, which leaves one flag per lane holding the
    /// corresponding bit of the scalar operand.
    fn alu48(&self) -> Option<VecExec> {
        if self.len != 6 {
            return None;
        }
        let b_slot = match self.b {
            VecOperandB::Slot(s) => Some(s),
            VecOperandB::Imm(_) => None,
        };
        // `bitplanes` writes no register: its point is `SETF`, which leaves one
        // flag per lane holding the corresponding bit of the scalar.
        if self.subop == 1 && self.setf && self.d.is_bare_dash() && self.a.is_bare_dash() {
            if let Some(b) = b_slot {
                if b.is_dash() && b.scalar < 32 && self.pred == 0 {
                    return Some(VecExec::Bitplanes { src: b.scalar });
                }
            }
            return None;
        }
        if self.subop != 0 || self.setf || self.pred != 0 {
            return None; // only `vmov` broadcasts, and only unpredicated
        }
        if !self.a.is_bare_dash() || self.d.is_dash() || self.d.star {
            return None;
        }
        let src = match b_slot {
            None => match self.b {
                VecOperandB::Imm(i) => RegOrImm::Imm(self.imm_value(i)),
                VecOperandB::Slot(_) => unreachable!(),
            },
            Some(b) if b.is_dash() && b.scalar < 32 => RegOrImm::Reg(b.scalar),
            Some(_) => return None,
        };
        Some(VecExec::Broadcast {
            reg: self.d.window()?,
            src,
            reps: VecRep::Fixed(1),
            step: false,
            addend: (self.d.addend != 15).then_some(self.d.addend),
        })
    }

    /// The 80-bit broadcast the boot ROM clears memory with:
    ///
    /// ```text
    ///   v<w>mov <reg>[++],#imm  [REP n]   1111 11L0 0000 0RRR DDDD dddddd 1110 000000 F1 ...
    ///   v<w>mov <reg>[++],r<n>  [REP n]   1111 11L0 0000 0RRR DDDD dddddd 1110 000000 00 1110 nnnnnn ...
    /// ```
    ///
    /// The A slot is a bare dash and the B slot is either a 16-bit immediate or a
    /// scalar register; the destination is a horizontal register stepped by `++`
    /// (or nothing) each of `REP` repetitions. Anything with an accumulator/SRU
    /// writeback, `SETF`, lane predication, or a vector-register source falls
    /// through to [`VecExec::NeedsVrf`] — those need the real vector ALU.
    fn alu80(&self) -> Option<VecExec> {
        if self.len != 10 || self.mem || self.subop != 0 {
            return None; // `subop == 0` is `vmov`; only that broadcasts here.
        }
        let reg = self.d.window()?;
        if self.d.star {
            return None;
        }
        // A must be a bare dash; no scalar writeback, flag update, or predication.
        if !self.a.is_bare_dash() {
            return None;
        }
        if self.setf || self.sru != VecSru::None || self.pred != 0 {
            return None;
        }
        let src = match self.b {
            VecOperandB::Imm(i) => RegOrImm::Imm(self.imm_value(i)),
            // A dash in the B slot names a scalar register in its addend
            // nibble; only r0..r15 can be spelled there.
            VecOperandB::Slot(s) if s.is_dash() && s.disp == 0 && s.scalar < 32 => {
                RegOrImm::Reg(s.scalar)
            }
            _ => return None,
        };
        let reps = match self.rep {
            7 => VecRep::FromR0,
            n => VecRep::Fixed(1 << n),
        };
        Some(VecExec::Broadcast {
            reg,
            src,
            reps,
            step: self.d.inc,
            addend: (self.d.addend != 15).then_some(self.d.addend),
        })
    }

    /// The ALU-class ops whose semantics are measured, in either encoding.
    ///
    /// A register may be *narrower* than the operation, and mostly is: the unit
    /// reads the element at the register's own width and widens it — a byte
    /// unsigned, a halfword signed — works at the operation's width, and
    /// narrows the result back into the destination. A register wider than the
    /// operation is refused: `binutils-vc4` cannot even spell one, and
    /// `start4.elf` has thirteen. `SETF`, a scalar writeback, a `*` and the
    /// five unpinned lane predicates all still fault.
    fn alu(&self) -> Option<VecExec> {
        // Sub-ops from 48 up are the multiply group, and there the `L` bit
        // selects the family rather than the element width — which then comes
        // from the registers themselves.
        let (op, width) = if self.subop >= 48 {
            // A multiply carries no width of its own, so it works at the
            // widest register it names and converts the narrower ones into it
            // — `vmull.ss HX(0,0),HX(62,0),H(57,0)` multiplies a halfword by
            // an unsigned byte and keeps sixteen bits of the product.
            let w = [self.d, self.a]
                .into_iter()
                .chain(match self.b {
                    VecOperandB::Slot(s) => Some(s),
                    VecOperandB::Imm(_) => None,
                })
                .filter(|s| !s.is_dash())
                .map(|s| s.elem_bytes() as u32)
                .max()?;
            let w = if self.lane_bits == 32 { 4 } else { w };
            (
                VecAluOp::from_mul_subop(self.subop, self.lane_bits as u32 / 8)?,
                w,
            )
        } else {
            let width = self.lane_bits as u32 / 8;
            (VecAluOp::from_subop(self.subop, width)?, width)
        };
        let (acc, sru) = match self.sru {
            VecSru::None => (None, None),
            VecSru::Acc(f) => (Some(VecAcc::from_field(f)?), None),
            // The scalar register field is three bits wide, so it names
            // `r0`..`r7`. A predicate applies to the aggregate as well: the
            // lanes it masks off contribute nothing, measured — `SUMU` under
            // `IFZ` and under `IFNZ` add back up to `SUMU` under `ALL`.
            VecSru::Scalar { func, reg } => (None, Some((VecSruFunc::from_func(func)?, reg))),
        };
        // `SETF` leaves the lane flags holding this result. Zero and negative
        // come from every op, but the carry does not: only the ops below were
        // measured, and the rest keep whatever carry was already there — which
        // is not something to guess at from the others.
        if self.setf {
            use VecAluOp::*;
            if !matches!(
                op,
                Mov | Bitplanes
                    | Zero
                    | And
                    | Or
                    | Eor
                    | Bic
                    | Dist
                    | Count
                    | Msb
                    | Brev
                    | Clip
                    | Sign
                    | Even
                    | Interl
                    | Mull
                    | Add
                    | Sub
                    | Rsub
                    | Adds
                    | Subs
                    | Rsubs
                    | Dists
                    | Shls
                    | Min
                    | Max
                    | Shl
                    | Lsr
                    | Asr
                    | Ror
                    | Odd
                    | Interh
                    | Clips
                    | Testmag
                    | Signshl
                    | Signasl
                    | Signasls
                    | Addc
                    | Addsc
                    | Subc
                    | Subsc
                    | Rsubc
                    | Rsubsc
                    | Mulls
                    | Mulhd { .. }
                    | Mulhn { .. }
            ) {
                return None;
            }
        }
        // A dash destination discards the result — which is the point when an
        // accumulator is carrying it.
        let d = if self.d.is_dash() {
            if !self.d.is_bare_dash() {
                return None;
            }
            None
        } else {
            Some(self.operand(self.d, width)?)
        };
        // A dash in the A position is an operand of zeros, whatever the op:
        // `v16sub HX(0,0),-,HX(63,0)` negates B, and `v16and` with one comes
        // out empty. Measured.
        let a = if self.a.is_dash() {
            None
        } else {
            Some(self.operand(self.a, width)?)
        };
        let b = match self.b {
            VecOperandB::Imm(i) => VecSource::Imm(self.imm_value(i)),
            VecOperandB::Slot(sl) if sl.is_dash() => {
                if sl.scalar >= 32 {
                    return None;
                }
                VecSource::Scalar {
                    reg: sl.scalar,
                    disp: sl.disp,
                }
            }
            VecOperandB::Slot(sl) => VecSource::Reg(self.operand(sl, width)?),
        };
        Some(VecExec::Alu {
            op,
            d,
            a,
            b,
            reps: match self.rep {
                7 => VecRep::FromR0,
                n => VecRep::Fixed(1 << n),
            },
            step_d: self.d.inc,
            step_a: self.a.inc,
            pred: VecPred::from_field(self.pred)?,
            width,
            acc,
            setf: self.setf,
            sru,
        })
    }

    /// A B-position immediate, sign-extended out of its field: six bits in the
    /// 48-bit encoding, sixteen in the 80-bit one. Both are signed, measured:
    /// `v32mov HY(0,0),#0x20` leaves `0xffffffe0` in every lane, and the
    /// 80-bit `#0xffff` leaves `0xffffffff`.
    fn imm_value(&self, i: u32) -> i32 {
        let bits = if self.wide { 16 } else { 6 };
        ((i << (32 - bits)) as i32) >> (32 - bits)
    }

    /// One slot as an execution operand: its window — whose elements may be
    /// narrower than the operation — plus its `+rN`.
    fn operand(&self, slot: VecSlot, width: u32) -> Option<VecOperand> {
        if slot.elem_bytes() as u32 > width {
            return None;
        }
        Some(VecOperand {
            reg: slot.window()?,
            addend: (slot.addend != 15).then_some(slot.addend),
        })
    }

    /// Mnemonic, in `binutils-vc4` objdump spelling.
    pub fn mnemonic(&self) -> String {
        let op = if self.mem {
            VEC_MEM_OPS
                .get(self.subop as usize)
                .copied()
                .unwrap_or("mem?")
        } else {
            VEC_ALU_OPS
                .get(self.subop as usize)
                .copied()
                .unwrap_or("op?")
        };
        format!("v{}{}", self.lane_bits, op)
    }
}

/// The sub-op mnemonics, generated from `isa/vpu.toml` by `build.rs`: the
/// reference page and the disassembler print the same names because they are
/// the same table.
pub use crate::isa::{VEC_ALU_OPS, VEC_MEM_OPS};

/// One slot in `binutils-vc4` objdump's spelling: `HX(3,32)++`, `V(16,12)+r4*`,
/// `-`, or — for a dash in the B position — the scalar register it names.
fn slot_str(s: VecSlot, scalar: bool) -> String {
    if s.is_dash() {
        if !scalar {
            return "-".to_string();
        }
        let mut out = format!("r{}", s.scalar);
        if s.disp != 0 {
            out += &format!("{:+}", s.disp);
        }
        return out;
    }
    let name = match s.ty >> 1 {
        0..=3 => "",
        4 | 5 => "X",
        _ => "Y",
    };
    let vertical = s.is_vertical();
    let (y, x) = if s.inc && !vertical {
        (format!("{}++", s.y), format!("{}", s.x))
    } else if s.inc {
        (format!("{}", s.y), format!("{}++", s.x))
    } else {
        (format!("{}", s.y), format!("{}", s.x))
    };
    let mut out = format!("{}{name}({y},{x})", if vertical { "V" } else { "H" });
    if s.addend != 15 {
        out += &format!("+r{}", s.addend);
    }
    if s.star {
        out.push('*');
    }
    out
}

impl std::fmt::Debug for VecInsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ", self.mnemonic())?;
        // A store's source sits in the A slot with a dash destination; a load's
        // destination sits in D. Print whichever is the real register first,
        // the way `binutils-vc4` objdump does.
        let first = if self.d.is_dash() && !self.a.is_dash() {
            self.a
        } else {
            self.d
        };
        write!(f, "{}", slot_str(first, false))?;
        if self.mem {
            if self.addr.is_none() && !self.a.is_dash() && !self.d.is_dash() {
                write!(f, ",{}", slot_str(self.a, false))?;
            }
            match self.addr {
                Some(a) => {
                    write!(f, ",(r{}", a.base)?;
                    if a.offset != 0 {
                        write!(f, "+{}", a.offset)?;
                    }
                    if let Some(i) = a.incr {
                        write!(f, "+=r{i}")?;
                    }
                    write!(f, ")")?;
                }
                None => match self.b {
                    VecOperandB::Slot(s) => write!(f, ",{}", slot_str(s, false))?,
                    VecOperandB::Imm(i) => write!(f, ",{i:#x}")?,
                },
            }
        } else {
            if !self.a.is_dash() && !self.d.is_dash() {
                write!(f, ",{}", slot_str(self.a, false))?;
            }
            match self.b {
                VecOperandB::Slot(s) => write!(f, ",{}", slot_str(s, true))?,
                VecOperandB::Imm(i) => write!(f, ",{i:#x}")?,
            }
        }
        match self.rep {
            0 => {}
            7 => write!(f, " REP r0")?,
            n => write!(f, " REP{}", 1u32 << n)?,
        }
        if self.setf {
            write!(f, " SETF")?;
        }
        match self.sru {
            VecSru::None => {}
            VecSru::Acc(v) => write!(f, " ACC{v:#x}")?,
            VecSru::Scalar { func, reg } => write!(
                f,
                " {} r{reg}",
                ["SUMU", "SUMS", "max2", "IMIN", "max4", "IMAX", "max6", "MAX"][func as usize]
            )?,
        }
        Ok(())
    }
}

impl AluOp {
    pub fn mnemonic(self) -> &'static str {
        use AluOp::*;
        match self {
            Mov => "mov",
            Cmn => "cmn",
            Add => "add",
            Bic => "bic",
            Mul => "mul",
            Eor => "eor",
            Sub => "sub",
            And => "and",
            Not => "not",
            Ror => "ror",
            Cmp => "cmp",
            Rsub => "rsub",
            Btest => "btest",
            Or => "or",
            Bmask => "bmask",
            Max => "max",
            Bitset => "bitset",
            Min => "min",
            Bitclear => "bitclear",
            Bitflip => "bitflip",
            Signext => "signext",
            Neg => "neg",
            Lsr => "lsr",
            Msb => "msb",
            Shl => "shl",
            Bitrev => "bitrev",
            Asr => "asr",
            Abs => "abs",
            AddScale(_) => "addscale",
            SubScale(_) => "subscale",
            Count => "count",
            MulhdSS | MulhdSU | MulhdUS | MulhdUU => "mulhd",
            DivS | DivSU | DivUS | DivU => "div",
            Clamp16 => "clamp16",
            Unimpl(n) => n,
        }
    }
}

impl MemWidth {
    /// `ld`/`st` mnemonic suffix.
    pub fn suffix(self) -> &'static str {
        match self {
            MemWidth::Word => "",
            MemWidth::Half => "h",
            MemWidth::Byte => "b",
            MemWidth::SignedHalf => "s",
            MemWidth::SignedByte => "sb",
        }
    }
}

impl Op {
    /// A canonical short mnemonic, for cross-checking against a reference
    /// disassembler. Not a full textual form — no operands.
    pub fn mnemonic(&self) -> String {
        use Op::*;
        match self {
            Nop => "nop".into(),
            Bkpt => "bkpt".into(),
            Sleep => "sleep".into(),
            SetIrqEnable(true) => "ei".into(),
            SetIrqEnable(false) => "di".into(),
            Rti => "rti".into(),
            Swi { .. } => "swi".into(),
            BranchReg { link, .. } => if *link { "bl" } else { "b" }.into(),
            BranchImm { cond, link, .. } => {
                let base = if *link { "bl" } else { "b" };
                format!("{base}{}", cond.mnemonic())
            }
            Alu2 { op, .. } => op.mnemonic().into(),
            AluImm { op, .. } => op.mnemonic().into(),
            Alu3 { op, cond, .. } => format!("{}{}", op.mnemonic(), cond.mnemonic()),
            FpAlu3 { op, cond, .. } => format!("{}{}", op.mnemonic(), cond.mnemonic()),
            Lea { .. } => "lea".into(),
            Load { w, cond, .. } => format!("ld{}{}", w.suffix(), cond.mnemonic()),
            Store { w, cond, .. } => format!("st{}{}", w.suffix(), cond.mnemonic()),
            Version { .. } => "version".into(),
            Switch { byte, .. } => if *byte { "switch.b" } else { "switch" }.into(),
            MovToCoproc { .. } | MovFromCoproc { .. } => "mov".into(),
            AddCmpB { cond, .. } => format!("addcmpb{}", cond.mnemonic()),
            PushMulti { .. } => "stm".into(),
            PopMulti { .. } => "ldm".into(),
            Vector(v) => v.mnemonic(),
            Unimpl { .. } => "??".into(),
        }
    }
}

/// A fully decoded instruction plus its byte length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insn {
    pub op: Op,
    pub len: u8,
}
