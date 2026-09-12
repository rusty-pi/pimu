//! Differential test of the A64 interpreter against `qemu-aarch64 -cpu
//! cortex-a72` (#40, milestone 1).
//!
//! Each case is a random instruction stream wrapped in a prologue that loads
//! every register from a data block and an epilogue that stores them all,
//! plus a scratch buffer the stream loads from and stores to, and `write(2)`s
//! it all to stdout. The same static ELF runs under QEMU's user-mode emulator
//! and under [`Cpu`] with a two-syscall shim; the two outputs must match byte
//! for byte.
//!
//! An instruction one side treats as UNDEFINED must be UNDEFINED on the other
//! too. The program installs a `SIGILL` handler that logs the faulting PC and
//! skips the instruction; our harness does the same when the core raises
//! [`Exception::Undefined`], and the log is part of the compared output. So a
//! generator that emits some unallocated encodings on purpose tests the
//! decoder's edges for free, at one QEMU run per case.
//!
//! On a mismatch the culprit is found by bisection — `nop` out the tail of
//! the stream until the outputs agree — and reported with its encoding.
//!
//! `qemu-aarch64` is `apt install qemu-user`. Without it the test is skipped,
//! except under CI (`CI` set), where that is a failure. `RVF_A64_CASES`
//! overrides the number of random cases, `RVF_A64_SEED` the first seed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// SIMD&FP instructions (`op0` = x111) our core retired / found UNDEFINED
/// across all cases, to show the random words are not all reserved space.
static SIMD_RETIRED: AtomicU64 = AtomicU64::new(0);
static SIMD_UNDEF: AtomicU64 = AtomicU64::new(0);

use rpi_virt_fw::aarch64::{Abort, Cpu, Exception, Memory, Step};

// --- Layout of the test executable ------------------------------------------

const BASE: u64 = 0x40_0000;
const CODE: u64 = BASE + 0x1000;
const CODE_MAX: u64 = 0x1_F000;
const DATA: u64 = BASE + 0x2_0000;
/// Initial register values: x0..x30 at 0, SP / NZCV / TPIDR_EL0 after them,
/// q0..q31 at [`INIT_Q`].
const INIT_SP: u64 = 31 * 8;
const INIT_NZCV: u64 = 32 * 8;
const INIT_TPIDR: u64 = 33 * 8;
const INIT_FPCR: u64 = 34 * 8;
const INIT_FPSR: u64 = 35 * 8;
const INIT_Q: u64 = 0x200;
/// `struct sigaction` and `stack_t` for the SIGILL handler.
const INIT_SIGACT: u64 = 0x400;
const INIT_ALTSS: u64 = 0x440;
const SCRATCH: u64 = DATA + 0x1000;
const SCRATCH_LEN: u64 = 0x1_0000;
/// `x27` holds this for the whole stream; `x28` is reset to it plus an
/// offset before every memory access, so accesses stay inside the buffer.
const SCRATCH_MID: u64 = SCRATCH + SCRATCH_LEN / 2;
const DUMP: u64 = SCRATCH + SCRATCH_LEN;
/// Registers, then the SIGILL log: a count and the PC of every instruction
/// that raised it.
const DUMP_LEN: u64 = 0x800;
const DUMP_Q: u64 = 0x200;
const LOG: u64 = DUMP + 0x400;
const END: u64 = DUMP + 0x1000;
/// The SIGILL handler's stack, in a segment of its own well away from the
/// image: if the stream's SP ever pointed into it, the kernel would take
/// that as "already on the alternate stack" and push the frame at SP.
const ALTSTACK: u64 = 0x5555_0000_0000;
const ALTSTACK_LEN: u64 = 0x2000;

const NOP: u32 = 0xD503_201F;
const SVC0: u32 = 0xD400_0001;
const SYS_WRITE: u64 = 64;
const SYS_EXIT: u64 = 93;
const SYS_SIGALTSTACK: u64 = 132;
const SYS_RT_SIGACTION: u64 = 134;
const SIGILL: u64 = 4;
const SA_SIGINFO: u64 = 4;
const SA_ONSTACK: u64 = 0x0800_0000;

// --- A handful of encoders for the scaffolding --------------------------------

fn movz(rd: u32, imm: u16, hw: u32) -> u32 {
    0xD280_0000 | (hw << 21) | ((imm as u32) << 5) | rd
}
fn movk(rd: u32, imm: u16, hw: u32) -> u32 {
    0xF280_0000 | (hw << 21) | ((imm as u32) << 5) | rd
}
fn mov_imm32(rd: u32, v: u64) -> [u32; 2] {
    [movz(rd, v as u16, 0), movk(rd, (v >> 16) as u16, 1)]
}
fn ldp_x(rt: u32, rt2: u32, rn: u32, off: u64) -> u32 {
    0xA940_0000 | (((off / 8) as u32 & 0x7F) << 15) | (rt2 << 10) | (rn << 5) | rt
}
fn stp_x(rt: u32, rt2: u32, rn: u32, off: u64) -> u32 {
    ldp_x(rt, rt2, rn, off) & !(1 << 22)
}
fn ldp_q(rt: u32, rt2: u32, rn: u32, off: u64) -> u32 {
    0xAD40_0000 | (((off / 16) as u32 & 0x7F) << 15) | (rt2 << 10) | (rn << 5) | rt
}
fn stp_q(rt: u32, rt2: u32, rn: u32, off: u64) -> u32 {
    ldp_q(rt, rt2, rn, off) & !(1 << 22)
}
fn ldr_x(rt: u32, rn: u32, off: u64) -> u32 {
    0xF940_0000 | (((off / 8) as u32) << 10) | (rn << 5) | rt
}
fn str_x(rt: u32, rn: u32, off: u64) -> u32 {
    0xF900_0000 | (((off / 8) as u32) << 10) | (rn << 5) | rt
}
/// `add xd|sp, xn|sp, #imm{, lsl #12}`
fn add_imm(rd: u32, rn: u32, imm: u32, lsl12: bool) -> u32 {
    0x9100_0000 | ((lsl12 as u32) << 22) | (imm << 10) | (rn << 5) | rd
}
/// System register numbers as `o0:op1:CRn:CRm:op2` (bits 19..5 of MRS/MSR).
const NZCV: u32 = (1 << 14) | (3 << 11) | (4 << 7) | (2 << 3);
const FPCR: u32 = (1 << 14) | (3 << 11) | (4 << 7) | (4 << 3);
const FPSR: u32 = FPCR | 1;
const TPIDR_EL0: u32 = (1 << 14) | (3 << 11) | (13 << 7) | 2;
fn mrs(rt: u32, sysreg: u32) -> u32 {
    0xD530_0000 | (sysreg << 5) | rt
}
fn msr(sysreg: u32, rt: u32) -> u32 {
    0xD510_0000 | (sysreg << 5) | rt
}

fn prologue() -> Vec<u32> {
    let mut p = Vec::new();
    // sigaltstack(&ss, NULL), then rt_sigaction(SIGILL, &act, NULL, 8). The
    // stream may leave SP anywhere, so the handler gets its own stack.
    p.extend(mov_imm32(0, DATA + INIT_ALTSS));
    p.push(movz(1, 0, 0));
    p.push(movz(8, SYS_SIGALTSTACK as u16, 0));
    p.push(SVC0);
    p.push(movz(0, SIGILL as u16, 0));
    p.extend(mov_imm32(1, DATA + INIT_SIGACT));
    p.push(movz(2, 0, 0));
    p.push(movz(3, 8, 0));
    p.push(movz(8, SYS_RT_SIGACTION as u16, 0));
    p.push(SVC0);
    p.extend(mov_imm32(27, DATA));
    p.push(ldr_x(0, 27, INIT_SP));
    p.push(add_imm(31, 0, 0, false));
    p.push(ldr_x(0, 27, INIT_NZCV));
    p.push(msr(NZCV, 0));
    p.push(ldr_x(0, 27, INIT_TPIDR));
    p.push(msr(TPIDR_EL0, 0));
    p.push(ldr_x(0, 27, INIT_FPCR));
    p.push(msr(FPCR, 0));
    p.push(ldr_x(0, 27, INIT_FPSR));
    p.push(msr(FPSR, 0));
    for i in (0..32).step_by(2) {
        p.push(ldp_q(i, i + 1, 27, INIT_Q + 16 * i as u64));
    }
    for i in (0..26).step_by(2) {
        p.push(ldp_x(i, i + 1, 27, 8 * i as u64));
    }
    p.push(ldr_x(26, 27, 26 * 8));
    p.push(ldp_x(29, 30, 27, 29 * 8));
    p.push(add_imm(27, 27, ((SCRATCH_MID - DATA) >> 12) as u32, true));
    p.push(add_imm(28, 27, 0, false));
    p
}

fn epilogue() -> Vec<u32> {
    let mut p = Vec::new();
    p.extend(mov_imm32(27, DUMP));
    for i in (0..30).step_by(2) {
        p.push(stp_x(i, i + 1, 27, 8 * i as u64));
    }
    p.push(str_x(30, 27, 30 * 8));
    p.push(add_imm(0, 31, 0, false)); // mov x0, sp
    p.push(str_x(0, 27, 31 * 8));
    for (slot, reg) in [(32, NZCV), (33, TPIDR_EL0), (34, FPSR), (35, FPCR)] {
        p.push(mrs(0, reg));
        p.push(str_x(0, 27, slot * 8));
    }
    for i in (0..32).step_by(2) {
        p.push(stp_q(i, i + 1, 27, DUMP_Q + 16 * i as u64));
    }
    for (addr, len) in [(DUMP, DUMP_LEN), (SCRATCH, SCRATCH_LEN)] {
        p.push(movz(0, 1, 0));
        p.extend(mov_imm32(1, addr));
        p.extend(mov_imm32(2, len));
        p.push(movz(8, SYS_WRITE as u16, 0));
        p.push(SVC0);
    }
    p.push(movz(0, 0, 0));
    p.push(movz(8, SYS_EXIT as u16, 0));
    p.push(SVC0);
    p
}

/// The SIGILL handler: append the faulting PC to the log at [`LOG`] and
/// resume after it. `x2` is the `ucontext_t`, with `uc_mcontext.pc` at 440.
/// Registers need no saving; `rt_sigreturn` restores them all.
fn handler() -> Vec<u32> {
    const UC_PC: u64 = 440;
    let mut p = vec![ldr_x(9, 2, UC_PC)];
    p.extend(mov_imm32(10, LOG));
    p.push(ldr_x(11, 10, 0));
    p.push(0x8B0B_0D4C); // add x12, x10, x11, lsl #3
    p.push(str_x(9, 12, 8));
    p.push(add_imm(11, 11, 1, false));
    p.push(str_x(11, 10, 0));
    p.push(add_imm(9, 9, 4, false));
    p.push(str_x(9, 2, UC_PC));
    p.push(0xD65F_03C0); // ret
    p
}

// --- Random instruction streams -----------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn u32(&mut self) -> u32 {
        self.next() as u32
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn bits(&mut self, n: u32) -> u32 {
        self.u32() & ((1 << n) - 1)
    }
    fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }
    /// A value biased towards the edges flag computations care about.
    fn interesting(&mut self) -> u64 {
        match self.below(8) {
            0 => 0,
            1 => !0,
            2 => 1 << self.below(64),
            3 => (1 << self.below(64)) - 1,
            4 => 0x8000_0000 ^ self.below(3).wrapping_sub(1),
            5 => self.below(256),
            _ => self.next(),
        }
    }
}

/// A floating-point value of `bits` (32 or 64) biased towards the cases the
/// ARM rules single out: zeros, infinities, both kinds of NaN, denormals,
/// the extremes of the normal range, and small integers and halves.
fn fp_value(r: &mut Rng, bits: u32) -> u64 {
    let (e, f) = if bits == 32 { (8, 23) } else { (11, 52) };
    let sign = r.bits(1) as u64;
    let frac = r.next() & ((1u64 << f) - 1);
    let emax = (1u64 << e) - 1;
    let bias = emax >> 1;
    let (exp, frac) = match r.below(12) {
        0 => (0, 0),
        1 => (emax, 0),
        2 => (emax, frac | (1 << (f - 1))),
        3 => (emax, (frac & !(1 << (f - 1))).max(1)),
        4 => (0, frac.max(1)),
        5 => (1, frac),
        6 => (emax - 1, frac),
        7 => (bias + r.below(4), frac & (0xF << (f - 4))),
        8 => (bias - 1, 0),
        _ => (r.below(emax), frac),
    };
    (sign << (bits - 1)) | (exp << f) | frac
}

/// A destination register the stream may overwrite: anything but `x27`
/// (scratch base) and `x28` (address register). 31 is XZR in the encodings
/// this is used for.
fn dst(r: &mut Rng) -> u32 {
    loop {
        let v = r.bits(5);
        if v != 27 && v != 28 {
            return v;
        }
    }
}

/// A destination where 31 would mean SP: allowed, SP is dumped too, but only
/// sometimes so it is not clobbered all the time.
fn dst_sp(r: &mut Rng) -> u32 {
    if r.chance(16) {
        31
    } else {
        loop {
            let v = dst(r);
            if v != 31 {
                return v;
            }
        }
    }
}

/// A load destination: not 27/28 (and not the base, which is always 28).
fn ld_dst(r: &mut Rng) -> u32 {
    dst(r)
}

struct Gen {
    r: Rng,
    body: Vec<u32>,
    /// Branches to patch once the body length is known: (index, kind).
    branches: Vec<(usize, BranchKind)>,
    /// Where each emitted group starts; the only valid branch targets.
    starts: Vec<usize>,
}

#[derive(Clone, Copy)]
enum BranchKind {
    Imm26,
    Imm19,
    Imm14,
}

impl Gen {
    fn pc_of(&self, index: usize) -> u64 {
        CODE + 4 * (prologue().len() + index) as u64
    }

    /// `add x28, x27, #off` (or `sub`), to point `x28` into the buffer.
    fn reset_base(&mut self, align: u64) {
        let off = self.r.below(0x800) & !(align - 1);
        let sub = self.r.chance(2);
        self.body
            .push(add_imm(28, 27, off as u32, false) | ((sub as u32) << 30));
    }

    /// Fill bits [28:25]-selected group with random bits, keeping `x27`/`x28`
    /// out of the destination field.
    fn random_dp(&mut self, group: u32) -> u32 {
        let mut w = (self.r.u32() & !(0xF << 25)) | (group << 25);
        let rd = w & 0x1F;
        if rd == 27 || rd == 28 {
            w = (w & !0x1F) | (rd - 16);
        }
        w
    }

    /// A random word in the SIMD&FP space. Mostly it picks one of the
    /// encoding classes (fixed bits as `(value, mask)`, the same split
    /// `src/aarch64/simd.rs` decodes by) and randomises only the rest, so
    /// most words are allocated; one in four is anything with `op0` = x111.
    /// Crypto encodings are left out: QEMU's cortex-a72 has the extension,
    /// the Pi's does not (see `src/aarch64/simd.rs`).
    fn random_simd(&mut self) -> u32 {
        const CLASSES: &[(u32, u32)] = &[
            // Advanced SIMD, vector.
            (0x0e20_0400, 0x9f20_0400), // three same
            (0x0e20_0000, 0x9f20_0c00), // three different
            (0x0e20_0800, 0x9f3e_0c00), // two-register misc
            (0x0e30_0800, 0x9f3e_0c00), // across lanes
            (0x0e00_0400, 0x9fe0_8400), // copy
            (0x0f00_0000, 0x9f00_0400), // by element
            (0x0f00_0400, 0x9ff8_0400), // modified immediate
            (0x0f00_0400, 0x9f80_0400), // shift by immediate
            (0x0e00_0000, 0xbf20_8c00), // table lookup
            (0x0e00_0800, 0xbf20_8c00), // permute
            (0x2e00_0000, 0xbf20_8400), // extract
            // Advanced SIMD, scalar.
            (0x5e20_0400, 0xdf20_0400),
            (0x5e20_0000, 0xdf20_0c00),
            (0x5e20_0800, 0xdf3e_0c00),
            (0x5e30_0800, 0xdf3e_0c00), // pairwise
            (0x5e00_0400, 0xdfe0_8400), // copy
            (0x5f00_0000, 0xdf00_0400), // by element
            (0x5f00_0400, 0xdf80_0400), // shift by immediate
            // Scalar floating point.
            (0x1e00_0000, 0x7f20_0000), // <-> fixed point
            (0x1e20_0000, 0x7f20_fc00), // <-> integer
            (0x1e20_2000, 0xff20_3c00), // compare
            (0x1e20_1000, 0xff20_1c00), // immediate
            (0x1e20_0400, 0xff20_0c00), // conditional compare
            (0x1e20_0800, 0xff20_0c00), // 2-source
            (0x1e20_0c00, 0xff20_0c00), // conditional select
            (0x1e20_4000, 0xff20_7c00), // 1-source
            (0x1f00_0000, 0xff00_0000), // 3-source
        ];
        loop {
            let r = &mut self.r;
            let mut w = r.u32();
            w = if r.chance(4) {
                (w & !(7 << 25)) | (7 << 25)
            } else {
                let (value, mask) = CLASSES[r.below(CLASSES.len() as u64) as usize];
                let w = (w & !mask) | value;
                if value & 0x5e00_0000 == 0x1e00_0000 && !r.chance(8) {
                    // Scalar FP: single or double, mostly.
                    (w & !(3 << 22)) | (r.bits(1) << 22)
                } else {
                    w
                }
            };
            let crypto = w & 0xff3e_0c00 == 0x4e28_0800
                || w & 0xff20_8c00 == 0x5e00_0000
                || w & 0xff3e_0c00 == 0x5e28_0800
                || w & 0xbfe0_fc00 == 0x0ee0_e000;
            if crypto {
                continue;
            }
            // General-register destinations (FMOV, UMOV, FCVTZS, ...) must not
            // hit x27/x28.
            let rd = w & 0x1F;
            if rd == 27 || rd == 28 {
                w = (w & !0x1F) | (rd - 16);
            }
            return w;
        }
    }

    /// LD1-4 / ST1-4 / LD1R-4R, multiple or single structure, on `x28`.
    fn emit_simd_ldst(&mut self) {
        self.reset_base(1);
        let r = &mut self.r;
        let post = r.bits(1);
        let rm = if post == 0 {
            if r.chance(16) {
                r.bits(5)
            } else {
                0
            }
        } else if r.chance(2) {
            31
        } else {
            26
        };
        let w = (r.bits(1) << 30)
            | (0b0011 << 26)
            | (r.bits(1) << 24)
            | (post << 23)
            | (r.bits(2) << 21)
            | (rm << 16)
            | (r.bits(6) << 10)
            | (28 << 5)
            | r.bits(5);
        if rm == 26 {
            let k = r.bits(7) as u16;
            self.body.push(movz(26, k, 0));
        }
        self.body.push(w);
    }

    fn emit(&mut self) {
        self.starts.push(self.body.len());
        let r = &mut self.r;
        let sf = r.bits(1);
        match r.below(56) {
            36..=47 => {
                let w = self.random_simd();
                self.body.push(w);
            }
            48..=50 => self.emit_simd_ldst(),
            // Fully random data-processing words, reserved encodings included.
            0..=5 => {
                let g = [0b1000, 0b1001, 0b0101, 0b1101][r.below(4) as usize];
                let w = self.random_dp(g);
                self.body.push(w);
            }
            6 | 7 => {
                // ADD/SUB (immediate)
                let s = r.bits(1);
                let rd = if s == 1 { dst(r) } else { dst_sp(r) };
                let w = (sf << 31)
                    | (r.bits(2) << 29 & (1 << 30))
                    | (s << 29)
                    | (0b100010 << 23)
                    | (r.bits(13) << 10)
                    | (r.bits(5) << 5)
                    | rd;
                self.body.push(w);
            }
            8 | 9 => {
                // Logical (immediate); invalid masks are fine, they must UNDEF.
                let opc = r.bits(2);
                let rd = if opc == 3 { dst(r) } else { dst_sp(r) };
                let n = if sf == 1 {
                    r.bits(1)
                } else {
                    r.bits(1) & r.bits(1) & r.bits(1)
                };
                let w = (sf << 31)
                    | (opc << 29)
                    | (0b100100 << 23)
                    | (n << 22)
                    | (r.bits(12) << 10)
                    | (r.bits(5) << 5)
                    | rd;
                self.body.push(w);
            }
            10 | 11 => {
                // Move wide
                let w =
                    (sf << 31) | (r.bits(2) << 29) | (0b100101 << 23) | (r.bits(18) << 5) | dst(r);
                self.body.push(w);
            }
            12..=14 => {
                // Bitfield / extract, N = sf most of the time.
                let n = if r.chance(8) { r.bits(1) } else { sf };
                let hi = if sf == 1 || r.chance(8) { 6 } else { 5 };
                let (op, opc) = if r.chance(4) {
                    (0b100111, r.bits(2) & r.bits(2))
                } else {
                    (0b100110, r.bits(2))
                };
                let w = (sf << 31)
                    | (opc << 29)
                    | (op << 23)
                    | (n << 22)
                    | (r.bits(if op == 0b100111 { 1 } else { 0 }) << 21)
                    | (r.bits(hi) << 16)
                    | (r.bits(hi) << 10)
                    | (r.bits(5) << 5)
                    | dst(r);
                // EXTR has Rm at 16..21 and the bit 21 must be 0.
                let w = if op == 0b100111 {
                    w & !(1 << 21) | (r.bits(5) << 16)
                } else {
                    w
                };
                self.body.push(w);
            }
            15..=17 => {
                // Logical / add-sub (shifted register)
                let w = self.random_dp(0b0101) & !(1 << 28);
                let w = if self.r.chance(2) {
                    (w & !(0x1F << 24)) | (0b01010 << 24)
                } else {
                    (w & !(0x1F << 24) & !(1 << 21)) | (0b01011 << 24)
                };
                self.body.push(w);
            }
            18 => {
                // Add-sub (extended register)
                let s = r.bits(1);
                let rd = if s == 1 { dst(r) } else { dst_sp(r) };
                let w = (sf << 31)
                    | (r.bits(1) << 30)
                    | (s << 29)
                    | (0b01011001 << 21)
                    | (r.bits(5) << 16)
                    | (r.bits(3) << 13)
                    | ((r.below(6) as u32) << 10)
                    | (r.bits(5) << 5)
                    | rd;
                self.body.push(w);
            }
            19..=21 => {
                // ADC/SBC, CCMP/CCMN, CSEL family, 1/2/3-source
                let top = [
                    0x1A00_0000u32,
                    0x1A40_0000,
                    0x1A80_0000,
                    0x1AC0_0000,
                    0x5AC0_0000,
                    0x1B00_0000,
                ][r.below(6) as usize];
                let mut w = self.random_dp(0b1101);
                match top {
                    0x1A00_0000 => w = (w & 0xE01F_03FF) | top,
                    0x1A40_0000 => w = (w & 0xC01F_FFEF) | top | (1 << 29),
                    0x1A80_0000 => w = (w & 0xC01F_F7FF) | top,
                    0x1AC0_0000 => {
                        let op = [2, 3, 8, 9, 10, 11, 16, 17, 18, 19, 20, 21, 22, 23]
                            [self.r.below(14) as usize];
                        w = (w & 0x801F_03FF) | top | (op << 10)
                    }
                    0x5AC0_0000 => w = (w & 0x8000_FFFF & !(0x38 << 10)) | top,
                    _ => w = (w & 0x80FF_FFFF & !(0x6 << 21)) | top,
                }
                self.body.push(w);
            }
            22..=27 => self.emit_ldst(),
            28 => self.emit_exclusive(),
            29 => {
                // DC ZVA
                self.reset_base(1);
                self.body.push(0xD50B_7420 | 28);
            }
            30 => {
                // LDR (literal) into the data block.
                let target = DATA + (self.r.below(0x800) & !7);
                let pc = self.pc_of(self.body.len());
                let imm19 = (((target as i64 - pc as i64) >> 2) as u32) & 0x7_FFFF;
                let opc = self.r.bits(2);
                let (v, opc) = if self.r.chance(4) {
                    (1, opc.min(2))
                } else {
                    (0, opc)
                };
                let rt = if v == 1 {
                    self.r.bits(5)
                } else {
                    ld_dst(&mut self.r)
                };
                self.body
                    .push((opc << 30) | (0b011 << 27) | (v << 26) | (imm19 << 5) | rt);
            }
            31..=34 => {
                let kind = match self.r.below(5) {
                    0 => {
                        self.body
                            .push(0x1400_0000 | ((self.r.chance(2) as u32) << 31));
                        BranchKind::Imm26
                    }
                    1 => {
                        self.body.push(0x5400_0000 | self.r.bits(4));
                        BranchKind::Imm19
                    }
                    2 | 3 => {
                        let w = 0x3400_0000 | (self.r.bits(1) << 31) | (self.r.bits(1) << 24);
                        self.body.push(w | self.r.bits(5));
                        BranchKind::Imm19
                    }
                    _ => {
                        let w = 0x3600_0000 | (self.r.bits(1) << 31) | (self.r.bits(1) << 24);
                        self.body.push(w | (self.r.bits(5) << 19) | self.r.bits(5));
                        BranchKind::Imm14
                    }
                };
                self.branches.push((self.body.len() - 1, kind));
            }
            35 => {
                // MRS/MSR of the EL0-visible registers.
                let reg = [NZCV, TPIDR_EL0, FPCR, FPSR][self.r.below(4) as usize];
                let rt = self.r.bits(5);
                if self.r.chance(2) {
                    self.body.push(msr(reg, rt));
                } else {
                    let rt = if rt == 27 || rt == 28 { 0 } else { rt };
                    self.body.push(mrs(rt, reg));
                }
            }
            _ => self.emit_ldst(),
        }
    }

    fn emit_ldst(&mut self) {
        let r = &mut self.r;
        let v = r.chance(5) as u32;
        let size = r.bits(2);
        let opc = r.bits(2);
        let load = opc != 0;
        let rt = if load && v == 0 { ld_dst(r) } else { r.bits(5) };
        match r.below(5) {
            0 => {
                // Unsigned offset: keep it within the buffer half.
                let scale = if v == 1 && opc & 2 != 0 { 4 } else { size };
                let imm12 = r.below(0x400 >> scale) as u32;
                self.reset_base(1);
                self.body.push(
                    (size << 30)
                        | (0b111 << 27)
                        | (v << 26)
                        | (0b01 << 24)
                        | (opc << 22)
                        | (imm12 << 10)
                        | (28 << 5)
                        | rt,
                );
            }
            1 => {
                // Unscaled, post-index, unprivileged, pre-index. A store that
                // writes back must not store its own base (UNPREDICTABLE).
                let mode = r.bits(2);
                let rt = if mode & 1 == 1 && rt == 28 { 0 } else { rt };
                let imm9 = r.bits(9);
                self.reset_base(1);
                self.body.push(
                    (size << 30)
                        | (0b111 << 27)
                        | (v << 26)
                        | (opc << 22)
                        | (imm9 << 12)
                        | (mode << 10)
                        | (28 << 5)
                        | rt,
                );
            }
            2 => {
                // Register offset, index in x26.
                let option = [2, 3, 6, 7, r.bits(3)][r.below(5) as usize];
                let idx = if option & 4 != 0 && r.chance(2) {
                    0x9280_0000 | ((r.bits(8)) << 5) | 26 // movn x26, #k
                } else {
                    movz(26, r.bits(8) as u16, 0)
                };
                let rt = if rt == 26 && !load {
                    rt
                } else if rt == 26 {
                    0
                } else {
                    rt
                };
                let s = r.bits(1);
                self.body.push(idx);
                self.reset_base(1);
                self.body.push(
                    (size << 30)
                        | (0b111 << 27)
                        | (v << 26)
                        | (opc << 22)
                        | (1 << 21)
                        | (26 << 16)
                        | (option << 13)
                        | (s << 12)
                        | (0b10 << 10)
                        | (28 << 5)
                        | rt,
                );
            }
            _ => {
                // Pairs. For a load, rt != rt2.
                let opc = r.bits(2);
                let mode = r.bits(2);
                let l = r.bits(1);
                let rt = if l == 1 && v == 0 { ld_dst(r) } else { rt };
                let rt2 = loop {
                    let x = if l == 1 && v == 0 {
                        ld_dst(r)
                    } else {
                        r.bits(5)
                    };
                    if x != rt || l == 0 {
                        break x;
                    }
                };
                let wb = mode & 1 == 1;
                let (rt, rt2) = if wb && l == 0 && v == 0 {
                    (
                        if rt == 28 { 0 } else { rt },
                        if rt2 == 28 { 1 } else { rt2 },
                    )
                } else {
                    (rt, rt2)
                };
                let imm7 = r.bits(7);
                self.reset_base(1);
                self.body.push(
                    (opc << 30)
                        | (0b101 << 27)
                        | (v << 26)
                        | (mode << 23)
                        | (l << 22)
                        | (imm7 << 15)
                        | (rt2 << 10)
                        | (28 << 5)
                        | rt,
                );
            }
        }
    }

    /// `ld[a]x{r,p}` then `st[l]x{r,p}` straight after, on the same address.
    fn emit_exclusive(&mut self) {
        let r = &mut self.r;
        let pair = r.chance(3);
        let size = if pair { 2 + r.bits(1) } else { r.bits(2) };
        let rt = ld_dst(r);
        let rt2 = if pair {
            loop {
                let x = ld_dst(r);
                if x != rt {
                    break x;
                }
            }
        } else {
            31
        };
        let rs = loop {
            let x = dst(r);
            if x != rt && x != rt2 && x != 31 {
                break x;
            }
        };
        let acq = r.bits(1);
        let rel = r.bits(1);
        let o1 = pair as u32;
        self.reset_base(16);
        let base = (size << 30) | (0b001000 << 24) | (o1 << 21) | (rt2 << 10) | (28 << 5);
        self.body
            .push(base | (1 << 22) | (31 << 16) | (acq << 15) | rt);
        let (st, st2) = (self.r.bits(5), if pair { self.r.bits(5) } else { 31 });
        let (st, st2) = (
            if st == rs { 0 } else { st },
            if st2 == rs { 1 } else { st2 },
        );
        let base = (size << 30) | (0b001000 << 24) | (o1 << 21) | (st2 << 10) | (28 << 5);
        self.body.push(base | (rs << 16) | (rel << 15) | st);
        if self.r.chance(3) {
            // LDAR / STLR
            let size = self.r.bits(2);
            self.reset_base(8);
            let l = self.r.bits(1);
            let rt = if l == 1 {
                ld_dst(&mut self.r)
            } else {
                self.r.bits(5)
            };
            self.body.push(
                (size << 30)
                    | (0b001000 << 24)
                    | (1 << 23)
                    | (l << 22)
                    | (0x1F << 16)
                    | (1 << 15)
                    | (0x1F << 10)
                    | (28 << 5)
                    | rt,
            );
        }
    }

    /// Point every branch forward at the start of a later group (or the
    /// epilogue), never between a base reset and the access that uses it.
    fn patch_branches(&mut self) {
        let len = self.body.len();
        self.starts.push(len);
        for &(i, kind) in &self.branches {
            let later: Vec<usize> = self.starts.iter().copied().filter(|&s| s > i).collect();
            let target = later[self.r.below(later.len() as u64) as usize];
            let off = (target - i) as u32;
            let w = &mut self.body[i];
            *w |= match kind {
                BranchKind::Imm26 => off,
                BranchKind::Imm19 => off << 5,
                BranchKind::Imm14 => off << 5,
            };
        }
    }
}

struct Case {
    body: Vec<u32>,
    init: Vec<u8>,
    scratch: Vec<u8>,
}

fn generate(seed: u64, len: usize) -> Case {
    let mut g = Gen {
        r: Rng(seed),
        body: Vec::new(),
        branches: Vec::new(),
        starts: Vec::new(),
    };
    while g.body.len() < len {
        g.emit();
    }
    g.patch_branches();
    let r = &mut g.r;
    let mut init = vec![0u8; 0x1000];
    for i in 0..31 {
        init[i * 8..i * 8 + 8].copy_from_slice(&r.interesting().to_le_bytes());
    }
    let sp = (SCRATCH_MID - 0x100) & !15;
    init[INIT_SP as usize..][..8].copy_from_slice(&sp.to_le_bytes());
    let nzcv = (r.bits(4) as u64) << 28;
    init[INIT_NZCV as usize..][..8].copy_from_slice(&nzcv.to_le_bytes());
    init[INIT_TPIDR as usize..][..8].copy_from_slice(&r.next().to_le_bytes());
    // FPCR: AHP, DN, FZ and RMode, often left at the Linux default of 0.
    let fpcr = if r.chance(2) {
        0
    } else {
        r.u32() as u64 & 0x07C0_0000
    };
    init[INIT_FPCR as usize..][..8].copy_from_slice(&fpcr.to_le_bytes());
    let fpsr = if r.chance(2) {
        0
    } else {
        r.u32() as u64 & 0x0800_009F
    };
    init[INIT_FPSR as usize..][..8].copy_from_slice(&fpsr.to_le_bytes());
    for i in 0..32 {
        let off = INIT_Q as usize + i * 16;
        let q: u128 = match r.below(4) {
            0 => r.interesting() as u128 | ((r.interesting() as u128) << 64),
            1 => (0..2).fold(0, |q, l| q | ((fp_value(r, 64) as u128) << (64 * l))),
            _ => (0..4).fold(0, |q, l| q | ((fp_value(r, 32) as u128) << (32 * l))),
        };
        init[off..off + 16].copy_from_slice(&q.to_le_bytes());
    }
    let scratch = (0..SCRATCH_LEN).map(|_| r.next() as u8).collect();
    Case {
        body: g.body,
        init,
        scratch,
    }
}

/// The whole program as a static little-endian AArch64 ELF.
fn elf(case: &Case) -> Vec<u8> {
    let mut code = prologue();
    code.extend(&case.body);
    code.extend(epilogue());
    let handler_at = CODE + 4 * code.len() as u64;
    code.extend(handler());
    assert!((code.len() as u64) * 4 <= CODE_MAX, "stream too long");

    let size = (END - BASE) as usize;
    let mut f = vec![0u8; size];
    let put = |f: &mut Vec<u8>, off: usize, b: &[u8]| f[off..off + b.len()].copy_from_slice(b);
    put(&mut f, 0, b"\x7fELF\x02\x01\x01");
    put(&mut f, 16, &2u16.to_le_bytes()); // ET_EXEC
    put(&mut f, 18, &183u16.to_le_bytes()); // EM_AARCH64
    put(&mut f, 20, &1u32.to_le_bytes());
    put(&mut f, 24, &CODE.to_le_bytes());
    put(&mut f, 32, &64u64.to_le_bytes()); // e_phoff
    put(&mut f, 52, &64u16.to_le_bytes()); // e_ehsize
    put(&mut f, 54, &56u16.to_le_bytes()); // e_phentsize
    put(&mut f, 56, &2u16.to_le_bytes()); // e_phnum
                                          // The image (RWX), then the alternate signal stack (RW, zero-filled).
    for (i, (vaddr, filesz, memsz, flags)) in [
        (BASE, size as u64, size as u64, 7u32),
        (ALTSTACK, 0, ALTSTACK_LEN, 6),
    ]
    .into_iter()
    .enumerate()
    {
        let ph = 64 + 56 * i;
        put(&mut f, ph, &1u32.to_le_bytes()); // PT_LOAD
        put(&mut f, ph + 4, &flags.to_le_bytes());
        put(&mut f, ph + 16, &vaddr.to_le_bytes());
        put(&mut f, ph + 24, &vaddr.to_le_bytes());
        put(&mut f, ph + 32, &filesz.to_le_bytes());
        put(&mut f, ph + 40, &memsz.to_le_bytes());
        put(&mut f, ph + 48, &0x1000u64.to_le_bytes());
    }
    let words: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    put(&mut f, (CODE - BASE) as usize, &words);
    put(&mut f, (DATA - BASE) as usize, &case.init);
    // struct sigaction { handler, flags, restorer, mask } and stack_t
    // { ss_sp, ss_flags, ss_size }.
    let act = (DATA + INIT_SIGACT - BASE) as usize;
    put(&mut f, act, &handler_at.to_le_bytes());
    put(&mut f, act + 8, &(SA_SIGINFO | SA_ONSTACK).to_le_bytes());
    let ss = (DATA + INIT_ALTSS - BASE) as usize;
    put(&mut f, ss, &ALTSTACK.to_le_bytes());
    put(&mut f, ss + 16, &ALTSTACK_LEN.to_le_bytes());
    put(&mut f, (SCRATCH - BASE) as usize, &case.scratch);
    f
}

// --- Running both sides --------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Output(Vec<u8>),
    Other(String),
}

struct Flat {
    data: Vec<u8>,
}

impl Flat {
    fn range(&self, addr: u64, size: u32, write: bool) -> Result<usize, Abort> {
        let off = addr.wrapping_sub(BASE);
        if off
            .checked_add(size as u64)
            .is_some_and(|e| e <= self.data.len() as u64)
        {
            Ok(off as usize)
        } else {
            Err(Abort { addr, write })
        }
    }
}

impl Memory for Flat {
    fn read(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
        let off = self.range(addr, size, false)?;
        let mut b = [0u8; 8];
        b[..size as usize].copy_from_slice(&self.data[off..off + size as usize]);
        Ok(u64::from_le_bytes(b))
    }
    fn write(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
        let off = self.range(addr, size, true)?;
        self.data[off..off + size as usize].copy_from_slice(&value.to_le_bytes()[..size as usize]);
        Ok(())
    }
}

fn run_ours(image: &[u8]) -> Outcome {
    let mut mem = Flat {
        data: image.to_vec(),
    };
    let mut cpu = Cpu::new_el0();
    cpu.pc = CODE;
    let body_start = CODE + 4 * prologue().len() as u64;
    let mut out = Vec::new();
    // RVF_A64_TRACE=1: print every body instruction and the registers it
    // changed.
    let trace = std::env::var_os("RVF_A64_TRACE").is_some();
    for _ in 0..1_000_000 {
        let before = trace.then(|| (cpu.pc, cpu.x, cpu.sp(), cpu.nzcv));
        let simd = mem.read(cpu.pc, 4).is_ok_and(|w| (w >> 25) & 7 == 7);
        let step = cpu.step(&mut mem);
        if simd && step == Step::Retired {
            SIMD_RETIRED.fetch_add(1, Ordering::Relaxed);
        }
        if let Some((pc, x, sp, nzcv)) = before.filter(|b| b.0 >= body_start) {
            let insn = mem.read(pc, 4).unwrap();
            let mut s = format!("[{:3}] {insn:08x}", (pc - body_start) / 4);
            for (i, (now, was)) in cpu.x.iter().zip(x.iter()).enumerate() {
                if now != was {
                    s += &format!(" x{i}={now:#x}");
                }
            }
            if cpu.sp() != sp {
                s += &format!(" sp={:#x}", cpu.sp());
            }
            if cpu.nzcv != nzcv {
                s += &format!(" nzcv={:x}", cpu.nzcv >> 28);
            }
            eprintln!("{s}");
        }
        match step {
            Step::Retired => {}
            Step::Exception(Exception::Svc(0)) => {
                match cpu.x[8] {
                    SYS_WRITE => {
                        let (a, n) = (cpu.x[1], cpu.x[2] as usize);
                        let off = (a - BASE) as usize;
                        out.extend_from_slice(&mem.data[off..off + n]);
                        cpu.x[0] = n as u64;
                    }
                    SYS_EXIT => return Outcome::Output(out),
                    SYS_SIGALTSTACK | SYS_RT_SIGACTION => cpu.x[0] = 0,
                    n => return Outcome::Other(format!("syscall {n}")),
                }
                cpu.pc += 4;
            }
            Step::Exception(Exception::Undefined) => {
                if simd {
                    SIMD_UNDEF.fetch_add(1, Ordering::Relaxed);
                }
                // What the guest's SIGILL handler does.
                let n = mem.read(LOG, 8).unwrap();
                mem.write(LOG + 8 + 8 * n, 8, cpu.pc).unwrap();
                mem.write(LOG, 8, n + 1).unwrap();
                cpu.pc += 4;
            }
            other => {
                return Outcome::Other(format!("{other:x?} at {:#x}", cpu.pc));
            }
        }
    }
    Outcome::Other("step limit".into())
}

fn qemu() -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join("qemu-aarch64"))
        .find(|p| p.is_file())
}

fn run_qemu(qemu: &Path, image: &[u8], tag: &str) -> Outcome {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("a64-{tag}.elf"));
    std::fs::write(&path, image).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    // No core dumps: a crashed case costs a tenth of a second each otherwise.
    let out = Command::new("sh")
        .args(["-c", "ulimit -c 0; exec \"$0\" -cpu cortex-a72 \"$1\""])
        .arg(qemu)
        .arg(&path)
        .output()
        .expect("run qemu-aarch64");
    match (out.status.code(), out.status.signal()) {
        (Some(0), _) => Outcome::Output(out.stdout),
        (code, sig) => Outcome::Other(format!("qemu exit {code:?} signal {sig:?}")),
    }
}

fn with_nops_from(case: &Case, from: usize) -> Case {
    let mut body = case.body.clone();
    for w in &mut body[from..] {
        *w = NOP;
    }
    Case {
        body,
        init: case.init.clone(),
        scratch: case.scratch.clone(),
    }
}

fn same(a: &Outcome, b: &Outcome) -> bool {
    matches!((a, b), (Outcome::Output(x), Outcome::Output(y)) if x == y)
}

/// Describe how two register dumps differ.
fn diff_dumps(ours: &[u8], theirs: &[u8]) -> String {
    let word = |d: &[u8], i: usize| u64::from_le_bytes(d[i * 8..i * 8 + 8].try_into().unwrap());
    let mut s = String::new();
    let names = |i: usize| match i {
        0..=30 => format!("x{i}"),
        31 => "sp".into(),
        32 => "nzcv".into(),
        33 => "tpidr_el0".into(),
        34 => "fpsr".into(),
        35 => "fpcr".into(),
        64..=127 => format!(
            "q{}.{}",
            (i - 64) / 2,
            if i.is_multiple_of(2) { "lo" } else { "hi" }
        ),
        128 => "sigills".into(),
        129.. => format!("sigill[{}]", i - 129),
        _ => format!("dump[{i}]"),
    };
    let n = ours.len().min(theirs.len());
    let dump_words = (DUMP_LEN as usize / 8).min(n / 8);
    for i in 0..dump_words {
        let (a, b) = (word(ours, i), word(theirs, i));
        if a != b {
            s += &format!("  {:>10}: ours {a:#018x}  qemu {b:#018x}\n", names(i));
        }
    }
    let base = DUMP_LEN as usize;
    if let Some(first) = (base..n).find(|&i| ours[i] != theirs[i]) {
        s += &format!(
            "  scratch differs first at {:#x} (+{:#x} from x27)\n",
            SCRATCH + (first - base) as u64,
            (SCRATCH + (first - base) as u64) as i64 - SCRATCH_MID as i64
        );
    }
    if ours.len() != theirs.len() {
        s += &format!(
            "  output length ours {} qemu {}\n",
            ours.len(),
            theirs.len()
        );
    }
    s
}

fn disasm(words: &[u32], tag: u64) -> String {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("a64-{tag}.bin"));
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(&path, bytes).unwrap();
    Command::new("aarch64-linux-gnu-objdump")
        .args(["-D", "-b", "binary", "-m", "aarch64"])
        .arg(&path)
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.contains(":\t"))
                .map(|l| format!("    {l}\n"))
                .collect()
        })
        .unwrap_or_default()
}

/// Run one case. Returns how many SIGILLs both sides agreed on, or a report
/// of the first instruction they disagree about.
fn check(qemu: &Path, seed: u64, len: usize) -> Result<u64, String> {
    let case = generate(seed, len);
    let tag = format!("{seed}");
    let ours = run_ours(&elf(&case));
    let theirs = run_qemu(qemu, &elf(&case), &tag);
    if let (Outcome::Output(a), true) = (&ours, same(&ours, &theirs)) {
        let log = (LOG - DUMP) as usize;
        return Ok(u64::from_le_bytes(a[log..log + 8].try_into().unwrap()));
    }
    // Bisect for the first instruction whose inclusion breaks agreement.
    let (mut good, mut bad) = (0, case.body.len());
    while bad - good > 1 {
        let mid = (good + bad) / 2;
        let c = with_nops_from(&case, mid);
        if same(&run_ours(&elf(&c)), &run_qemu(qemu, &elf(&c), &tag)) {
            good = mid;
        } else {
            bad = mid;
        }
    }
    let k = bad - 1;
    let c = with_nops_from(&case, bad);
    let detail = match (run_ours(&elf(&c)), run_qemu(qemu, &elf(&c), &tag)) {
        (Outcome::Output(a), Outcome::Output(b)) => diff_dumps(&a, &b),
        (a, b) => format!("  ours {:?}\n  qemu {:?}\n", brief(&a), brief(&b)),
    };
    Err(report(seed, &case, k, &format!("results differ\n{detail}")))
}

fn brief(o: &Outcome) -> String {
    match o {
        Outcome::Output(v) => format!("output ({} bytes)", v.len()),
        o => format!("{o:?}"),
    }
}

fn report(seed: u64, case: &Case, k: usize, what: &str) -> String {
    let lo = k.saturating_sub(4);
    let ctx: Vec<u32> = case.body[lo..=k].to_vec();
    format!(
        "seed {seed}: body[{k}] = {:#010x}: {what}\n  context (body[{lo}..={k}]):\n{}",
        case.body[k],
        disasm(&ctx, seed)
    )
}

#[test]
fn random_streams_match_qemu() {
    let Some(qemu) = qemu() else {
        if std::env::var_os("CI").is_some() {
            panic!("qemu-aarch64 not found; CI must install qemu-user");
        }
        eprintln!("skipping: qemu-aarch64 not found (apt install qemu-user)");
        return;
    };
    let cases: u64 = std::env::var("RVF_A64_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);
    let first: u64 = std::env::var("RVF_A64_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    // Cases are independent and QEMU start-up dominates, so spread them over
    // the host's cores.
    let next = AtomicU64::new(first);
    let undefs = AtomicU64::new(0);
    let failures = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let seed = next.fetch_add(1, Ordering::Relaxed);
                if seed >= first + cases || failures.lock().unwrap().len() >= 5 {
                    break;
                }
                match check(&qemu, seed, 64) {
                    Ok(u) => {
                        undefs.fetch_add(u, Ordering::Relaxed);
                    }
                    Err(e) => failures.lock().unwrap().push(e),
                }
            });
        }
    });
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    eprintln!(
        "{cases} cases, {} UNDEFINED encodings agreed on; SIMD&FP: {} executed, {} UNDEFINED",
        undefs.into_inner(),
        SIMD_RETIRED.load(Ordering::Relaxed),
        SIMD_UNDEF.load(Ordering::Relaxed)
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
