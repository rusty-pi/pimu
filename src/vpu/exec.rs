//! The VPU scalar executor: fetch → decode → execute one instruction.

use crate::bus::{Bus, BusError, Width};

use super::decode::decode;
use super::insn::{AddrMode, AluOp, Base, MemWidth, Op, RegOrImm};
use super::length::{insn_len_bytes, InsnClass};
use super::reg::{Cond, Flags, Regs, GP, LR, SP};

/// Why the core stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    Halt(HaltReason),
    Fault(Fault),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HaltReason {
    /// `swi` executed. Test payloads use this to signal completion.
    Swi(u32),
    Breakpoint,
    Sleep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    Bus { pc: u32, err: BusError },
    Unimplemented { pc: u32, raw: u64, class: InsnClass },
    UnimplementedAlu { pc: u32, op: &'static str },
}

/// What to do when the core meets an instruction the decoder/executor does not
/// implement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnimplPolicy {
    /// Stop with a [`Fault::Unimplemented`]. The default — silence here means
    /// silently wrong execution.
    #[default]
    Fault,
    /// Advance past it (correct length) and keep going. For "how far does the
    /// firmware get / what does it touch" reconnaissance runs.
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Ran,
    Stopped,
}

#[derive(Default)]
pub struct Vpu {
    pub regs: Regs,
    pub cycles: u64,
    pub retired: u64,
    pub stopped: Option<Stop>,
    pub on_unimpl: UnimplPolicy,
    /// Count of instructions skipped under [`UnimplPolicy::Skip`].
    pub skipped: u64,
}

impl Vpu {
    pub fn new(entry: u32) -> Vpu {
        let mut v = Vpu::default();
        v.regs.pc = entry;
        v
    }

    #[inline]
    pub fn pc(&self) -> u32 {
        self.regs.pc
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.is_some()
    }

    fn stop(&mut self, s: Stop) -> Step {
        self.stopped = Some(s);
        Step::Stopped
    }

    /// Fetch the instruction bytes at `pc` into a 10-byte buffer.
    fn fetch(&self, bus: &mut dyn Bus, pc: u32) -> Result<([u8; 10], u8), BusError> {
        let p0 = bus.load16(pc)?;
        let len = insn_len_bytes(p0);
        let mut buf = [0u8; 10];
        buf[0..2].copy_from_slice(&p0.to_le_bytes());
        let mut i = 2u32;
        while i < len as u32 {
            let h = bus.load16(pc.wrapping_add(i))?;
            buf[i as usize..i as usize + 2].copy_from_slice(&h.to_le_bytes());
            i += 2;
        }
        Ok((buf, len))
    }

    pub fn step(&mut self, bus: &mut dyn Bus) -> Step {
        if self.stopped.is_some() {
            return Step::Stopped;
        }
        let pc = self.regs.pc;

        let (buf, len) = match self.fetch(bus, pc) {
            Ok(v) => v,
            Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
        };
        let insn = decode(&buf[..len as usize], pc);
        let next = pc.wrapping_add(insn.len as u32);

        self.cycles += 1;

        match insn.op {
            Op::Nop => self.regs.pc = next,
            Op::Sleep => return self.stop(Stop::Halt(HaltReason::Sleep)),
            Op::Bkpt => return self.stop(Stop::Halt(HaltReason::Breakpoint)),
            Op::Swi { vector } => return self.stop(Stop::Halt(HaltReason::Swi(vector))),

            Op::Rti => {
                let sp = self.regs.get(SP);
                let sr = match bus.load32(sp) {
                    Ok(v) => v,
                    Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
                };
                let ret = match bus.load32(sp.wrapping_add(4)) {
                    Ok(v) => v,
                    Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
                };
                self.regs.sr = sr;
                self.regs.set(SP, sp.wrapping_add(8));
                self.regs.pc = ret;
            }

            Op::BranchReg { link, rd } => {
                if link {
                    self.regs.set(LR, next);
                }
                self.regs.pc = self.regs.get(rd as usize);
            }
            Op::BranchImm { cond, link, target } => {
                if self.regs.flags.test(cond) {
                    if link {
                        self.regs.set(LR, next);
                    }
                    self.regs.pc = target;
                } else {
                    self.regs.pc = next;
                }
            }

            Op::Alu2 {
                op,
                rd,
                rs,
                set_flags,
            } => {
                let a = self.regs.get(rd as usize);
                let b = self.regs.get(rs as usize);
                match self.apply_alu(pc, op, a, b, set_flags, Some(rd as usize)) {
                    Ok(()) => self.regs.pc = next,
                    Err(step) => return step,
                }
            }
            Op::AluImm {
                op,
                rd,
                imm,
                set_flags,
            } => {
                let a = self.regs.get(rd as usize);
                match self.apply_alu(pc, op, a, imm as u32, set_flags, Some(rd as usize)) {
                    Ok(()) => self.regs.pc = next,
                    Err(step) => return step,
                }
            }
            Op::Alu3 {
                op,
                cond,
                rd,
                ra,
                b,
                set_flags,
            } => {
                if !self.regs.flags.test(cond) {
                    self.regs.pc = next;
                } else {
                    let a = self.regs.get(ra as usize);
                    let bv = match b {
                        RegOrImm::Reg(r) => self.regs.get(r as usize),
                        RegOrImm::Imm(i) => i as u32,
                    };
                    match self.apply_alu(pc, op, a, bv, set_flags, Some(rd as usize)) {
                        Ok(()) => self.regs.pc = next,
                        Err(step) => return step,
                    }
                }
            }

            Op::AddSp { imm } => {
                let v = self.regs.get(SP).wrapping_add(imm as u32);
                self.regs.set(SP, v);
                self.regs.pc = next;
            }
            Op::AddRegSp { rd, imm } => {
                let v = self.regs.get(SP).wrapping_add(imm as u32);
                self.regs.set(rd as usize, v);
                self.regs.pc = next;
            }
            Op::AddRegPc { rd, imm } => {
                self.regs.set(rd as usize, pc.wrapping_add(imm as u32));
                self.regs.pc = next;
            }

            Op::Load { w, rd, addr } => {
                let ea = self.effective_addr(pc, addr);
                match self.load_width(bus, ea, w) {
                    Ok(v) => {
                        self.regs.set(rd as usize, v);
                        self.regs.pc = next;
                    }
                    Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
                }
            }
            Op::Store { w, rd, addr } => {
                let ea = self.effective_addr(pc, addr);
                let v = self.regs.get(rd as usize);
                let width = match w {
                    MemWidth::Word => Width::Word,
                    MemWidth::Half | MemWidth::SignedHalf => Width::Half,
                    MemWidth::Byte => Width::Byte,
                };
                match bus.store(ea, width, v) {
                    Ok(()) => self.regs.pc = next,
                    Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
                }
            }

            Op::Unimpl { raw, class, .. } => match self.on_unimpl {
                UnimplPolicy::Fault => {
                    return self.stop(Stop::Fault(Fault::Unimplemented { pc, raw, class }))
                }
                UnimplPolicy::Skip => {
                    self.skipped += 1;
                    self.regs.pc = next;
                }
            },
        }

        if !self.is_stopped() {
            self.retired += 1;
        }
        Step::Ran
    }

    fn effective_addr(&self, pc: u32, addr: AddrMode) -> u32 {
        let base = match addr.base {
            Base::Reg(r) => self.regs.get(r as usize),
            Base::Sp => self.regs.get(SP),
            Base::Gp => self.regs.get(GP),
            Base::R0 => self.regs.get(0),
            Base::Pc => pc,
        };
        base.wrapping_add(addr.offset as u32)
    }

    fn load_width(&self, bus: &mut dyn Bus, ea: u32, w: MemWidth) -> Result<u32, BusError> {
        Ok(match w {
            MemWidth::Word => bus.load32(ea)?,
            MemWidth::Half => bus.load16(ea)? as u32,
            MemWidth::Byte => bus.load8(ea)? as u32,
            MemWidth::SignedHalf => bus.load16(ea)? as i16 as i32 as u32,
        })
    }

    /// Apply an ALU op, writing `dst` (unless it is a compare) and flags (if
    /// `set_flags`). Returns `Err(Step)` if the op is not implemented.
    fn apply_alu(
        &mut self,
        pc: u32,
        op: AluOp,
        a: u32,
        b: u32,
        set_flags: bool,
        dst: Option<usize>,
    ) -> Result<(), Step> {
        let cin = self.regs.flags.c;
        let (res, flags) = match alu(op, a, b, cin) {
            Some(v) => v,
            None => {
                let name = match op {
                    AluOp::Unimpl(n) => n,
                    _ => "alu",
                };
                return Err(self.stop(Stop::Fault(Fault::UnimplementedAlu { pc, op: name })));
            }
        };
        if !op.is_compare() {
            if let Some(d) = dst {
                self.regs.set(d, res);
            }
        }
        if set_flags {
            self.regs.flags = flags;
        }
        Ok(())
    }
}

fn nz(r: u32, carry: bool, overflow: bool) -> Flags {
    Flags {
        n: (r >> 31) & 1 == 1,
        z: r == 0,
        c: carry,
        v: overflow,
    }
}

fn add_with_carry(a: u32, b: u32, cin: u32) -> (u32, bool, bool) {
    let (s1, c1) = a.overflowing_add(b);
    let (s2, c2) = s1.overflowing_add(cin);
    let carry = c1 || c2;
    let overflow = (!(a ^ b) & (a ^ s2)) >> 31 & 1 == 1;
    (s2, carry, overflow)
}

/// Core ALU. Returns `(result, flags)`, or `None` for ops not implemented yet.
/// `flags` is always computed; the caller decides whether to commit it.
pub fn alu(op: AluOp, a: u32, b: u32, cin: bool) -> Option<(u32, Flags)> {
    use AluOp::*;
    let sh = b & 31;
    let r = match op {
        Mov => {
            let r = b;
            return Some((r, nz(r, cin, false)));
        }
        Add | Cmn => {
            let (r, c, v) = add_with_carry(a, b, 0);
            return Some((r, nz(r, c, v)));
        }
        Sub | Cmp => {
            let (r, c, v) = add_with_carry(a, !b, 1);
            return Some((r, nz(r, c, v)));
        }
        Rsub => {
            let (r, c, v) = add_with_carry(b, !a, 1);
            return Some((r, nz(r, c, v)));
        }
        Neg => {
            let (r, c, v) = add_with_carry(0, !b, 1);
            return Some((r, nz(r, c, v)));
        }
        And => a & b,
        Or => a | b,
        Eor => a ^ b,
        Bic => a & !b,
        Not => !b,
        Mul => a.wrapping_mul(b),
        Lsr => {
            if sh == 0 {
                a
            } else {
                a >> sh
            }
        }
        Shl => {
            if sh == 0 {
                a
            } else {
                a << sh
            }
        }
        Asr => ((a as i32) >> sh) as u32,
        Ror => a.rotate_right(sh),
        Min => (a as i32).min(b as i32) as u32,
        Max => (a as i32).max(b as i32) as u32,
        Btest => {
            let r = a & (1u32 << sh);
            return Some((r, nz(r, cin, false)));
        }
        Bitset => a | (1u32 << sh),
        Bitclear => a & !(1u32 << sh),
        Bitflip => a ^ (1u32 << sh),
        Bmask => {
            if sh == 0 {
                0
            } else {
                a & ((1u32 << sh) - 1)
            }
        }
        Signext => {
            let shift = 31 - sh;
            (((a << shift) as i32) >> shift) as u32
        }
        Msb => {
            if a == 0 {
                u32::MAX
            } else {
                31 - a.leading_zeros()
            }
        }
        Bitrev => b.reverse_bits(),
        Abs => (b as i32).unsigned_abs(),
        AddScale(m) => a.wrapping_add(b.wrapping_mul(m as u32)),
        Unimpl(_) => return None,
    };
    Some((r, nz(r, cin, false)))
}

/// Evaluate a condition against flags — thin re-export for callers holding a
/// `Cond` without a `Flags`.
pub fn cond_holds(flags: &Flags, cond: Cond) -> bool {
    flags.test(cond)
}
