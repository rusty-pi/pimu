//! VPU scalar register file and status register.

/// Number of scalar general-purpose registers (r0..r31).
pub const NUM_GPR: usize = 32;

/// Global/GOT base register. 16-bit load/store forms can address `(gp + imm)`.
///
/// _(No register role here is officially documented; they follow the community
/// reverse engineering — Hermitage's notes and the vc4 toolchain ABI — and the
/// way `start4.elf` uses them.)_
pub const GP: usize = 24;
/// Stack pointer. The 16-bit `add sp, #imm` form encodes destination field 25.
pub const SP: usize = 25;
/// Link register. `bl`/`jl` write the return address here.
pub const LR: usize = 26;
/// Status register. `mov`ing to or from `r30` reads or writes SR: the low
/// nibble is N/Z/C/V (measured on a 4B d03115, `vpu-probe/`), and the upper
/// bits carry the interrupt-enable / mode flags. Reading `r30` reflects the
/// live condition flags; writing it updates them.
pub const SR: usize = 30;

/// Condition codes, as encoded in the 4-bit `cccc` field of conditional
/// instructions.
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

/// Status register flags. N/Z/C/V carry ARM's meanings, except that the VC4
/// sets carry to *borrow* on a subtraction — see [`Flags::test`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    pub n: bool,
    pub z: bool,
    pub c: bool,
    pub v: bool,
}

impl Flags {
    /// Evaluate a condition code. Note the VC4 carry convention: `c` is *borrow*
    /// on subtraction, so `cs` aliases `lo` (unsigned below) and `cc` aliases
    /// `hs` (unsigned higher-or-same) — the opposite of ARM.
    pub fn test(&self, cond: Cond) -> bool {
        use Cond::*;
        match cond {
            Eq => self.z,
            Ne => !self.z,
            Cs => self.c,  // == lo  (unsigned <)
            Cc => !self.c, // == hs  (unsigned >=)
            Mi => self.n,
            Pl => !self.n,
            Vs => self.v,
            Vc => !self.v,
            Hi => !self.c && !self.z, // unsigned >
            Ls => self.c || self.z,   // unsigned <=
            Ge => self.n == self.v,
            Lt => self.n != self.v,
            Gt => !self.z && (self.n == self.v),
            Le => self.z || (self.n != self.v),
            Al => true,
            F => false,
        }
    }

    /// Pack N/Z/C/V into the SR low nibble: `V`, `C`, `N`, `Z` from bit 0 up
    /// (measured on a 4B d03115 — `cmp` equal reads `...8`, negative `...6`).
    pub fn to_sr_nibble(self) -> u32 {
        (self.v as u32) | ((self.c as u32) << 1) | ((self.n as u32) << 2) | ((self.z as u32) << 3)
    }

    /// Inverse of [`Flags::to_sr_nibble`].
    pub fn from_sr_nibble(sr: u32) -> Flags {
        Flags {
            v: sr & 0b0001 != 0,
            c: sr & 0b0010 != 0,
            n: sr & 0b0100 != 0,
            z: sr & 0b1000 != 0,
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
        let i = i & 31;
        if i == SR {
            (self.r[SR] & !0xF) | self.flags.to_sr_nibble()
        } else {
            self.r[i]
        }
    }

    #[inline]
    pub fn set(&mut self, i: usize, v: u32) {
        let i = i & 31;
        self.r[i] = v;
        if i == SR {
            // Writing SR (`mov sr, rX`) loads the condition flags from the low
            // nibble, the way silicon does; `di`/`ei` and the exception path
            // pass the live nibble straight through, so this is a no-op there.
            self.flags = Flags::from_sr_nibble(v);
        }
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
