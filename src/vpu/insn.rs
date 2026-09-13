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

/// One VRF operand slot of a vector instruction.
///
/// The Vector Register File is a 64x64 array of bytes; a vector register is a
/// 16-element window into it, named by a 4-bit "type" descriptor (element width
/// plus horizontal/vertical direction plus the column band) and a 6-bit
/// coordinate. Descriptors 14 and 15 are the "dash" slot, which names no VRF
/// register at all: `videocoreiv.arch` spells its meaning per position as
/// "Discard result (D), Ignore (A), Use coordinate as Scalar (B)".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VecSlot {
    /// 4-bit descriptor: `(type3 << 1) | vertical`. >= 14 is the dash slot.
    pub desc: u8,
    /// 6-bit coordinate. For a dash slot in the B position this is a *scalar*
    /// register number instead (`r0..r63`, though only `r0..r31` exist).
    pub coord: u8,
}

impl VecSlot {
    /// Names no vector register.
    pub fn is_dash(self) -> bool {
        self.desc >= 14
    }

    /// Resolve a *horizontal* slot — 16 consecutive elements of one VRF row —
    /// whose element width matches `lane_bits`.
    ///
    /// The descriptor's column band fixes both the element width and where in
    /// the row the window starts, and the band always spans exactly 16 elements:
    /// four 16-byte bands for 8-bit elements, two 32-byte bands for 16-bit, one
    /// 64-byte band for 32-bit. A vertical slot (a *column* of the file), or a
    /// band that disagrees with the operation width, returns `None` — the
    /// executor faults on those rather than guessing.
    pub fn horizontal(self, lane_bits: u8) -> Option<VecReg> {
        if self.desc & 1 != 0 {
            return None;
        }
        let (bits, x0) = match self.desc >> 1 {
            band @ 0..=3 => (8, band * 16),
            band @ (4 | 5) => (16, (band - 4) * 32),
            6 => (32, 0),
            _ => return None,
        };
        if bits != lane_bits {
            return None;
        }
        Some(VecReg {
            row: self.coord % 64,
            x0,
            lane_bytes: lane_bits / 8,
        })
    }
}

/// A horizontal VRF register window: 16 lanes of `lane_bytes` bytes each,
/// starting at byte column `x0` of row `row`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VecReg {
    pub row: u8,
    pub x0: u8,
    pub lane_bytes: u8,
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
    /// `*` / `++` coordinate modifiers on the D and A slots (80-bit forms).
    pub d_mod: u8,
    pub a_mod: u8,
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
    /// `v<w>{ld,st} <reg>[++],(r<base>[+=r<incr>]) [REP n]` — transfer 16 lanes
    /// between a VRF row and memory, `reps` times. Each repetition steps the
    /// address by `r<incr>` and, with `++`, the register down one row.
    Mem {
        store: bool,
        reg: VecReg,
        /// `++` on the vector slot: advance to the next row each repetition.
        step_row: bool,
        base: u8,
        incr: Option<u8>,
        reps: VecRep,
        pred: VecPred,
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
        step_row: bool,
    },
    /// `v<w>bitplanes -,r<n> SETF` — set the per-lane flags from the low 16 bits
    /// of a scalar; the vector result goes to a dash and is discarded.
    Bitplanes { src: u8 },
    /// Needs a part of the vector unit this model does not implement.
    NeedsVrf,
}

/// Mask of `n` bits at bit position `pos`, counted from the most significant
/// bit of a `width`-bit instruction word (how `videocoreiv.arch` writes them).
const fn vmask(width: u32, pos: u32, n: u32) -> u128 {
    ((1u128 << n) - 1) << (width - pos - n)
}

/// The same field, carrying `v`.
const fn vbits(width: u32, pos: u32, n: u32, v: u128) -> u128 {
    v << (width - pos - n)
}

/// Read such a field out of an instruction word.
const fn vfield(raw: u128, width: u32, pos: u32, n: u32) -> u32 {
    ((raw >> (width - pos - n)) & ((1u128 << n) - 1)) as u32
}

impl VecInsn {
    /// Does any slot name a real vector register?
    pub fn touches_vrf(&self) -> bool {
        let b_vrf = match self.b {
            VecOperandB::Slot(s) => !s.is_dash(),
            VecOperandB::Imm(_) => false,
        };
        !self.d.is_dash() || !self.a.is_dash() || b_vrf
    }

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
        } else if let Some(e) = self.alu48().or_else(|| self.alu80()) {
            return e;
        }
        VecExec::NeedsVrf
    }

    /// `v<w>ld`/`v<w>st` between one VRF row and memory, in both the 48-bit and
    /// the 80-bit encoding.
    ///
    /// ```text
    ///   48: 1111 00MM MMMW Weee VVV0 dddddd TTTx aaaaaa z 0 111 0 bbbbbb
    ///   80: 1111 10MM MMMW WRRR DDDD dddddd AAAA aaaaaa 0 0 1110 000000
    ///       gggg GG hhhh HH 0000 PPP 0000000 ssss 00
    /// ```
    ///
    /// The vector operand is the D slot for a load and the A slot for a store;
    /// the other slot must be a bare dash. In the 80-bit form each slot also
    /// carries a 4-bit register addend and a 2-bit coordinate modifier: on the
    /// vector slot the addend must be "none" (15) and the modifier is `++` or
    /// nothing, while the *dash* slot's addend is the register the address is
    /// stepped by between repetitions. Any offset field must be zero — the
    /// address fields' placement is Hermitage's, and no executed instruction
    /// exercises a non-zero one.
    fn mem_transfer(&self) -> Option<VecExec> {
        const M48_FREE: u128 = vmask(48, 6, 5)
            | vmask(48, 11, 2)
            | vmask(48, 16, 3)
            | vmask(48, 20, 6)
            | vmask(48, 26, 3)
            | vmask(48, 30, 6)
            | vmask(48, 42, 6);
        const M48: u128 = vbits(48, 0, 6, 0b111100) | vbits(48, 38, 3, 7);
        const M80_FREE: u128 = vmask(80, 6, 5)
            | vmask(80, 11, 2)
            | vmask(80, 13, 3)
            | vmask(80, 16, 10)
            | vmask(80, 26, 10)
            | vmask(80, 48, 12)
            | vmask(80, 64, 3)
            | vmask(80, 74, 4);
        const M80: u128 = vbits(80, 0, 6, 0b111110) | vbits(80, 38, 4, 0b1110);

        let wide = match self.len {
            6 => false,
            10 => true,
            _ => return None,
        };
        let (free, template, width) = if wide {
            (M80_FREE, M80, 80)
        } else {
            (M48_FREE, M48, 48)
        };
        if self.raw & !free != template {
            return None;
        }
        // `WW` 3 is not a width this decoder knows; 0/1/2 are 8/16/32.
        if vfield(self.raw, width, 11, 2) > 2 {
            return None;
        }
        let store = match self.subop {
            0 => false,
            4 => true,
            _ => return None,
        };
        // Addend/modifier pairs: g/G belong to D, h/H to A.
        let (g, h) = if wide {
            (
                vfield(self.raw, 80, 48, 4) as u8,
                vfield(self.raw, 80, 54, 4) as u8,
            )
        } else {
            (15, 15)
        };
        let ((vec_slot, vec_mod, vec_addend), (dash, dash_mod, dash_addend)) = if store {
            ((self.a, self.a_mod, h), (self.d, self.d_mod, g))
        } else {
            ((self.d, self.d_mod, g), (self.a, self.a_mod, h))
        };
        if dash.desc != 14 || dash.coord != 0 || dash_mod != 0 || vec_addend != 15 {
            return None;
        }
        let step_row = match vec_mod {
            0 => false,
            2 => true,
            _ => return None,
        };
        let addr = self.addr?;
        Some(VecExec::Mem {
            store,
            reg: vec_slot.horizontal(self.lane_bits)?,
            step_row,
            base: addr.base,
            incr: if dash_addend == 15 {
                None
            } else {
                Some(dash_addend)
            },
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
        const MOV_FREE: u128 =
            vmask(48, 6, 1) | vmask(48, 16, 3) | vmask(48, 20, 6) | vmask(48, 42, 6);
        const MOV_REG: u128 = vbits(48, 0, 6, 0b111101) | vbits(48, 26, 3, 7) | vbits(48, 38, 3, 7);
        const MOV_IMM: u128 = vbits(48, 0, 6, 0b111101) | vbits(48, 26, 3, 7) | vbits(48, 37, 1, 1);
        const BITPLANES_FREE: u128 = vmask(48, 6, 1) | vmask(48, 42, 6);
        const BITPLANES: u128 = vbits(48, 0, 6, 0b111101)
            | vbits(48, 7, 6, 1)
            | vbits(48, 16, 3, 7)
            | vbits(48, 26, 3, 7)
            | vbits(48, 38, 3, 7)
            | vbits(48, 41, 1, 1);

        if self.len != 6 {
            return None;
        }
        let operand = vfield(self.raw, 48, 42, 6) as u8;
        if self.raw & !BITPLANES_FREE == BITPLANES {
            // Scalar registers are r0..r31; the field is six bits wide.
            return (operand < 32).then_some(VecExec::Bitplanes { src: operand });
        }
        let masked = self.raw & !MOV_FREE;
        let src = if masked == MOV_REG {
            if operand >= 32 {
                return None;
            }
            RegOrImm::Reg(operand)
        } else if masked == MOV_IMM {
            RegOrImm::Imm(operand as i32)
        } else {
            return None;
        };
        Some(VecExec::Broadcast {
            reg: self.d.horizontal(self.lane_bits)?,
            src,
            reps: VecRep::Fixed(1),
            step_row: false,
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
        let reg = self.d.horizontal(self.lane_bits)?;
        let step_row = match self.d_mod {
            0 => false,
            2 => true,
            _ => return None,
        };
        // A must be a bare dash; no scalar writeback, flag update, or predication.
        if !self.a.is_dash() || self.a.coord != 0 || self.a_mod != 0 {
            return None;
        }
        if self.setf || self.sru != VecSru::None || self.pred != 0 {
            return None;
        }
        let src = match self.b {
            VecOperandB::Imm(i) => RegOrImm::Imm(i as i32),
            VecOperandB::Slot(s) if s.is_dash() && (s.coord as u32) < 32 => RegOrImm::Reg(s.coord),
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
            step_row,
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

fn slot_str(s: VecSlot, scalar: bool) -> String {
    if s.is_dash() {
        return if scalar {
            format!("r{}", s.coord)
        } else {
            "-".to_string()
        };
    }
    // desc = (type3 << 1) | vertical; type3 picks width and column band.
    let vertical = s.desc & 1 != 0;
    let (name, col) = match s.desc >> 1 {
        0 => ("", 0),
        1 => ("", 16),
        2 => ("", 32),
        3 => ("", 48),
        4 => ("X", 0),
        5 => ("X", 32),
        _ => ("Y", 0),
    };
    format!(
        "{}{}({},{})",
        if vertical { "V" } else { "H" },
        name,
        s.coord,
        col
    )
}

impl std::fmt::Debug for VecInsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ", self.mnemonic())?;
        // A store's source sits in the A slot with a dash destination; a load's
        // destination sits in D. Print whichever is the real register first,
        // the way `binutils-vc4` objdump does.
        let first = if self.d.is_dash() && !self.a.is_dash() {
            (self.a, self.a_mod)
        } else {
            (self.d, self.d_mod)
        };
        write!(
            f,
            "{}{}",
            slot_str(first.0, false),
            MOD_STR[first.1 as usize]
        )?;
        if self.mem {
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
                write!(
                    f,
                    ",{}{}",
                    slot_str(self.a, false),
                    MOD_STR[self.a_mod as usize]
                )?;
            }
            match self.b {
                VecOperandB::Slot(s) => write!(f, ",{}", slot_str(s, true))?,
                VecOperandB::Imm(i) => write!(f, ",{i:#x}")?,
            }
        }
        if self.rep != 0 {
            write!(f, " REP{}", 1u32 << self.rep)?;
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

/// `define-table G`/`H`/`K` — the per-slot coordinate modifiers.
const MOD_STR: [&str; 4] = ["", "*", "++", "*++"];

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
