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
/// one of the 15180 vector instructions there spells its slots the same way.
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
/// `v<w>bitplanes -,rN SETF` sets the per-lane flags from the bits of `rN`;
/// these two predicates then select one polarity. Both are pinned by the
/// firmware: `memcpy`'s tail (`0x3EDA292C`) builds `~0 << n` and transfers
/// under predicate 2, so predicate 2 is "the lane's bit was 0"; `memset`'s
/// (`0x3EDA2B5E`) builds a band of set bits and stores under predicate 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecPred {
    All,
    /// Lanes whose `bitplanes` bit was 0.
    IfZero,
    /// Lanes whose `bitplanes` bit was 1.
    IfNonZero,
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
    Add,
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
    /// The same, rounded: `(a * b + half) >> bits`.
    Mulhn {
        sa: bool,
        sb: bool,
    },
}

impl VecAluOp {
    /// The `v` sub-op field, as `insn-vecops` numbers it.
    pub fn from_subop(subop: u8) -> Option<VecAluOp> {
        use VecAluOp::*;
        Some(match subop {
            0 => Mov,
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
            20 => Count,
            21 => Msb,
            24 => Min,
            25 => Max,
            26 => Dist,
            27 => Dists,
            28 => Clip,
            29 => Sign,
            32 => Add,
            33 => Adds,
            36 => Sub,
            37 => Subs,
            40 => Rsub,
            41 => Rsubs,
            _ => return None,
        })
    }

    /// The multiply group, `insn-vecmulops` (sub-ops 48 and up with the `L`
    /// bit clear). `L` set selects a different family, which is not modelled.
    pub fn from_mul_subop(subop: u8) -> Option<VecAluOp> {
        use VecAluOp::*;
        let signs = |n: u8| (n & 2 == 0, n & 1 == 0);
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
/// operation, the result is added to it (or taken off it, with `SUB`), read
/// signed or unsigned as `SIGN` says, and `WBA` makes the destination take the
/// accumulator rather than the raw result. Measured on a Raspberry Pi 4B
/// d03115: `v16add -,A,B CLRA UACC` followed by `v16add D,A,B UACC` leaves
/// twice the sum in `D`, and the `0xffff` case proves the unsigned reading.
///
/// The `HIGH` forms — `UACCH` and friends — are *not* here: what they do did
/// not fall out of the same runs, and a wrong guess would corrupt a register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VecAcc {
    pub clear: bool,
    pub signed: bool,
    pub sub: bool,
    pub writeback: bool,
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
        if f & ENA == 0 || f & HIGH != 0 {
            return None;
        }
        Some(VecAcc {
            clear: f & CLRA != 0,
            signed: f & SIGN != 0,
            sub: f & SUB != 0,
            writeback: f & WBA != 0,
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
    Scalar(u8),
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
        reg: VecReg,
        /// `++` on the vector slot.
        step: bool,
        base: u8,
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
    },
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
            if let Some(e) = self.mem_transfer() {
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
    /// a whole-word template: what it refuses is `SETF` (the lane flags a
    /// transfer would write are not modelled), a `*` on the vector slot, and a
    /// lane predicate other than the two `bitplanes` feeds.
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
        // The inert slot must be a dash, and carries nothing of its own except
        // — in the 80-bit encoding — the addend nibble the address reads as its
        // `+=` step.
        if !dash.is_dash() || dash.star || dash.inc {
            return None;
        }
        if !self.wide && dash.addend != 15 {
            return None;
        }
        // What `*` means on a slot is not established.
        if vec_slot.star || self.setf {
            return None;
        }
        let addr = self.addr?;
        Some(VecExec::Mem {
            store,
            reg: vec_slot.window()?,
            step: vec_slot.inc,
            base: addr.base,
            offset: addr.offset,
            addend: (vec_slot.addend != 15).then_some(vec_slot.addend),
            incr: addr.incr,
            width: self.lane_bits as u32 / 8,
            reps: match self.rep {
                7 => VecRep::FromR0,
                n => VecRep::Fixed(1 << n),
            },
            pred: match self.pred {
                0 => VecPred::All,
                2 => VecPred::IfZero,
                3 => VecPred::IfNonZero,
                _ => return None,
            },
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
                VecOperandB::Imm(i) => RegOrImm::Imm(i as i32),
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
            VecOperandB::Imm(i) => RegOrImm::Imm(i as i32),
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
            if self.lane_bits != 16 {
                return None; // `L` set is the other family, not modelled
            }
            // The width comes from whichever slot names a real register; a
            // dash has none of its own. The multiplies carry no width of their
            // own to convert to, so every register they touch has to agree.
            let mut slots = [self.d, self.a]
                .into_iter()
                .chain(match self.b {
                    VecOperandB::Slot(s) => Some(s),
                    VecOperandB::Imm(_) => None,
                })
                .filter(|s| !s.is_dash());
            let w = slots.next()?.elem_bytes() as u32;
            if slots.any(|s| s.elem_bytes() as u32 != w) {
                return None;
            }
            (VecAluOp::from_mul_subop(self.subop)?, w)
        } else {
            let op = VecAluOp::from_subop(self.subop)?;
            let width = self.lane_bits as u32 / 8;
            // `v32count` writes a zero into every lane on hardware, whatever
            // its operands — whatever it counts, it is not the bits of a
            // 32-bit element, so it is left to fault.
            if op == VecAluOp::Count && width == 4 {
                return None;
            }
            (op, width)
        };
        let acc = match self.sru {
            VecSru::None => None,
            VecSru::Acc(f) => Some(VecAcc::from_field(f)?),
            VecSru::Scalar { .. } => return None,
        };
        if self.setf {
            return None;
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
        // `mov` and the unary ops read only B; the rest need a real A.
        let a = if self.a.is_dash() {
            if !matches!(op, VecAluOp::Mov | VecAluOp::Msb | VecAluOp::Count) {
                return None;
            }
            None
        } else {
            Some(self.operand(self.a, width)?)
        };
        let b = match self.b {
            VecOperandB::Imm(i) => VecSource::Imm(i as i32),
            VecOperandB::Slot(sl) if sl.is_dash() => {
                if sl.disp != 0 || sl.scalar >= 32 {
                    return None;
                }
                VecSource::Scalar(sl.scalar)
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
            pred: match self.pred {
                0 => VecPred::All,
                2 => VecPred::IfZero,
                3 => VecPred::IfNonZero,
                _ => return None,
            },
            width,
            acc,
        })
    }

    /// One slot as an execution operand: its window — whose elements may be
    /// narrower than the operation — plus its `+rN`.
    fn operand(&self, slot: VecSlot, width: u32) -> Option<VecOperand> {
        if slot.star || slot.elem_bytes() as u32 > width {
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

/// `define-table M` in `videocoreiv.arch` — the memory-class sub-ops.
pub const VEC_MEM_OPS: [&str; 32] = [
    "ld",
    "lookupm",
    "lookupml",
    "mem03",
    "st",
    "indexwritem",
    "indexwriteml",
    "mem07",
    "memread",
    "memwrite",
    "mem10",
    "mem11",
    "mem12",
    "mem13",
    "mem14",
    "mem15",
    "mem16",
    "mem17",
    "mem18",
    "mem19",
    "mem20",
    "mem21",
    "mem22",
    "mem23",
    "getacc",
    "mem25",
    "mem26",
    "mem27",
    "mem28",
    "mem29",
    "mem30",
    "mem31",
];

/// `define-table v` in `videocoreiv.arch` — the ALU-class sub-ops.
pub const VEC_ALU_OPS: [&str; 64] = [
    "mov",
    "bitplanes",
    "even",
    "odd",
    "interl",
    "interh",
    "brev",
    "ror",
    "shl",
    "shls",
    "lsr",
    "asr",
    "signshl",
    "op13",
    "signasl",
    "signasls",
    "and",
    "or",
    "eor",
    "bic",
    "count",
    "msb",
    "op22",
    "op23",
    "min",
    "max",
    "dist",
    "dists",
    "clip",
    "sign",
    "clips",
    "testmag",
    "add",
    "adds",
    "addc",
    "addsc",
    "sub",
    "subs",
    "subc",
    "subsc",
    "rsub",
    "rsubs",
    "rsubc",
    "rsubsc",
    "op44",
    "op45",
    "op46",
    "op47",
    "mull",
    "mulls",
    "mulm",
    "mulms",
    "mulhd.ss",
    "mulhd.su",
    "mulhd.us",
    "mulhd.uu",
    "mulhn.ss",
    "mulhn.su",
    "mulhn.us",
    "mulhn.uu",
    "mulht.ss",
    "mulht.su",
    "op62",
    "op63",
];

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
