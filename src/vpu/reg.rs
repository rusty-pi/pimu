//! VPU scalar register file and status register.

/// Number of scalar general-purpose registers (r0..r31).
pub const NUM_GPR: usize = 32;

/// Global/GOT base register. 16-bit load/store forms can address `(gp + imm)`.
///
/// _(Register roles below r24 are not officially documented; these follow the
/// community reverse engineering — Hermitage's notes and the vc4 toolchain ABI —
/// and should be re-checked against real firmware behaviour.)_
pub const GP: usize = 24;
/// Stack pointer. The 16-bit `add sp, #imm` form encodes destination field 25.
pub const SP: usize = 25;
/// Link register. `bl`/`jl` write the return address here.
pub const LR: usize = 26;

/// Condition codes, as encoded in the 4-bit `cccc` field of conditional
/// instructions. Source: `videocoreiv.arch` condition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Cond {
    Eq = 0x0,
    Ne = 0x1,
    Cs = 0x2, // carry set / unsigned lower  (aka lo)
    Cc = 0x3, // carry clear / unsigned higher-or-same (aka hs)
    Mi = 0x4,
    Pl = 0x5,
    Vs = 0x6,
    Vc = 0x7,
    Hi = 0x8,
    Ls = 0x9,
    Ge = 0xA,
    Lt = 0xB,
    Gt = 0xC,
    Le = 0xD,
    Al = 0xE, // always
    F = 0xF,  // "never" / special
}

impl Cond {
    #[inline]
    pub fn from_bits(bits: u32) -> Cond {
        use Cond::*;
        match bits & 0xF {
            0x0 => Eq,
            0x1 => Ne,
            0x2 => Cs,
            0x3 => Cc,
            0x4 => Mi,
            0x5 => Pl,
            0x6 => Vs,
            0x7 => Vc,
            0x8 => Hi,
            0x9 => Ls,
            0xA => Ge,
            0xB => Lt,
            0xC => Gt,
            0xD => Le,
            0xE => Al,
            _ => F,
        }
    }

    pub fn mnemonic(self) -> &'static str {
        use Cond::*;
        match self {
            Eq => "eq",
            Ne => "ne",
            Cs => "cs",
            Cc => "cc",
            Mi => "mi",
            Pl => "pl",
            Vs => "vs",
            Vc => "vc",
            Hi => "hi",
            Ls => "ls",
            Ge => "ge",
            Lt => "lt",
            Gt => "gt",
            Le => "le",
            Al => "",
            F => "f",
        }
    }
}

/// Status register flags. Bit positions in the real SR are not all confirmed;
/// what matters for execution is the N/Z/C/V semantics, which match ARM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    pub n: bool,
    pub z: bool,
    pub c: bool,
    pub v: bool,
}

impl Flags {
    /// Evaluate a condition code against the current flags (ARM semantics).
    pub fn test(&self, cond: Cond) -> bool {
        use Cond::*;
        match cond {
            Eq => self.z,
            Ne => !self.z,
            Cs => self.c,
            Cc => !self.c,
            Mi => self.n,
            Pl => !self.n,
            Vs => self.v,
            Vc => !self.v,
            Hi => self.c && !self.z,
            Ls => !self.c || self.z,
            Ge => self.n == self.v,
            Lt => self.n != self.v,
            Gt => !self.z && (self.n == self.v),
            Le => self.z || (self.n != self.v),
            Al => true,
            F => false,
        }
    }
}

/// The scalar register file plus PC and flags.
#[derive(Debug, Clone)]
pub struct Regs {
    r: [u32; NUM_GPR],
    /// Program counter. Not part of the GPR space; pc-relative forms use
    /// dedicated encodings.
    pub pc: u32,
    pub flags: Flags,
    /// Raw status register value (bits beyond N/Z/C/V, e.g. mode/irq-enable).
    pub sr: u32,
}

impl Default for Regs {
    fn default() -> Self {
        Regs {
            r: [0; NUM_GPR],
            pc: 0,
            flags: Flags::default(),
            sr: 0,
        }
    }
}

impl Regs {
    #[inline]
    pub fn get(&self, i: usize) -> u32 {
        self.r[i & 31]
    }

    #[inline]
    pub fn set(&mut self, i: usize, v: u32) {
        self.r[i & 31] = v;
    }

    #[inline]
    pub fn sp(&self) -> u32 {
        self.r[SP]
    }

    #[inline]
    pub fn set_sp(&mut self, v: u32) {
        self.r[SP] = v;
    }

    #[inline]
    pub fn lr(&self) -> u32 {
        self.r[LR]
    }
}
