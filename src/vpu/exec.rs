//! The VPU scalar executor: fetch → decode → execute one instruction.

use crate::bus::{Bus, BusError, Width};

use super::decode::decode;
use super::insn::{AddrMode, AluOp, Base, MemWidth, Op, RegOrImm, Writeback};
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

/// One distinct instruction the model does not implement, with a hit count.
/// Collected for reconnaissance runs ([`UnimplPolicy::Skip`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnimplHit {
    pub pc: u32,
    pub raw: u64,
    pub len: u8,
    pub class: InsnClass,
    pub count: u64,
}

/// Default chip-version value returned by `version rd`.
///
/// The `start4.elf` entry trampoline (`.crypto`) compares `version` (after
/// masking bits 3 and 16) against one of `{0x0400_0162, 0x0400_0161,
/// 0x0400_0160, 0x0400_0140, 0x0400_0104}` and `bkpt`s otherwise. `0x0400_0162`
/// is the newest accepted revision — the BCM2711 VPU value.
pub const DEFAULT_VERSION: u32 = 0x0400_0162;

#[derive(Default)]
pub struct Vpu {
    pub regs: Regs,
    pub cycles: u64,
    pub retired: u64,
    pub stopped: Option<Stop>,
    pub on_unimpl: UnimplPolicy,
    /// Count of instructions skipped under [`UnimplPolicy::Skip`].
    pub skipped: u64,
    /// Distinct unimplemented instructions seen (bounded).
    pub unimpl: Vec<UnimplHit>,
    /// Value returned by `version rd` (before the core-id bit is OR'd in).
    pub version_value: u32,
    /// VPU core index (0 or 1). Reported in bit 16 of `version` — `start4.elf`
    /// keys its per-core branches (which control register to poke, which stack
    /// to use) off that bit.
    pub core_id: u32,
    /// Base of the 64-entry exception vector table (`.isr_vectors`). `swi #u`
    /// raises exception `0x20 + u` and jumps to `*(exc_vbase + exc*8)`, after
    /// pushing SR and the return address (so the handler's `rti` unwinds).
    pub exc_vbase: u32,
    /// True while executing inside an exception handler (before `rti`).
    pub in_exception: u32,
    /// System-coprocessor register file (`mov p<n>,r` / `mov r,p<n>`). Not real
    /// hardware behaviour — reads return the last written value (0 at reset),
    /// which is enough to clear the early-boot "wait for p16 == 0" loops.
    pub coproc: [u32; 32],
    /// Ring of recent taken control transfers `(from_pc, to_pc)`.
    pub cf_trace: Vec<(u32, u32)>,
    /// When set, `step` pushes a one-line disassembly + delta of every
    /// instruction it retires (bounded by `trace_cap`) into `trace_log`.
    pub trace: bool,
    pub trace_cf_only: bool,
    pub trace_cap: usize,
    pub trace_log: Vec<String>,
    /// If non-zero, tracing stays dormant until `pc` first reaches this address
    /// (lets a run skip past millions of uninteresting early instructions).
    pub trace_from: u32,
    /// Flips true once `trace_from` has been reached (always true when it is 0).
    pub trace_armed: bool,
}

impl Vpu {
    pub fn new(entry: u32) -> Vpu {
        let mut v = Vpu::default();
        v.regs.pc = entry;
        v.version_value = DEFAULT_VERSION;
        v.cf_trace = Vec::with_capacity(512);
        v.trace_cap = 20_000;
        v
    }

    fn note_unimpl(&mut self, pc: u32, raw: u64, len: u8, class: InsnClass) {
        if let Some(h) = self.unimpl.iter_mut().find(|h| h.pc == pc && h.raw == raw) {
            h.count += 1;
        } else if self.unimpl.len() < 512 {
            self.unimpl.push(UnimplHit {
                pc,
                raw,
                len,
                class,
                count: 1,
            });
        }
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

        if !self.trace_armed && (self.trace_from == 0 || pc == self.trace_from) {
            self.trace_armed = true;
        }
        let trace_before = if self.trace && self.trace_armed && self.trace_log.len() < self.trace_cap
        {
            Some(self.regs.clone())
        } else {
            None
        };

        match insn.op {
            Op::Nop => self.regs.pc = next,
            Op::Sleep => return self.stop(Stop::Halt(HaltReason::Sleep)),
            Op::Bkpt => return self.stop(Stop::Halt(HaltReason::Breakpoint)),
            Op::Swi { vector } => {
                // `vector` is already `0x20 + u`. With a vector table configured,
                // trap to `*(exc_vbase + vector*8)` after pushing SR + return
                // address (so the handler's `rti` unwinds). Otherwise halt — the
                // test payloads use `swi` as a clean "done".
                let handler = if self.exc_vbase != 0 {
                    bus.load32(self.exc_vbase.wrapping_add(vector.wrapping_mul(8)))
                        .ok()
                        .filter(|&h| h != 0)
                } else {
                    None
                };
                match handler {
                    Some(h) => {
                        let sp = self.regs.get(SP).wrapping_sub(8);
                        let sr = self.regs.sr;
                        if bus.store32(sp, sr).is_err()
                            || bus.store32(sp.wrapping_add(4), next).is_err()
                        {
                            return self.stop(Stop::Halt(HaltReason::Swi(vector)));
                        }
                        self.regs.set(SP, sp);
                        self.in_exception = self.in_exception.wrapping_add(1);
                        self.regs.pc = h;
                    }
                    None => return self.stop(Stop::Halt(HaltReason::Swi(vector))),
                }
            }

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
                self.in_exception = self.in_exception.saturating_sub(1);
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

            Op::Lea { rd, addr } => {
                // Address arithmetic only — never touches memory or writeback.
                let base = match addr.base {
                    Base::Reg(r) => self.regs.get(r as usize),
                    Base::Sp => self.regs.get(SP),
                    Base::Gp => self.regs.get(GP),
                    Base::R0 => self.regs.get(0),
                    Base::Pc => pc,
                    Base::RegReg(a, b) => self
                        .regs
                        .get(a as usize)
                        .wrapping_add(self.regs.get(b as usize)),
                };
                self.regs
                    .set(rd as usize, base.wrapping_add(addr.offset as u32));
                self.regs.pc = next;
            }

            Op::Load { w, rd, addr, cond } => {
                if !self.regs.flags.test(cond) {
                    self.regs.pc = next;
                } else {
                    let ea = self.resolve_addr(pc, addr, w);
                    match self.load_width(bus, ea, w) {
                        Ok(v) => {
                            self.regs.set(rd as usize, v);
                            self.regs.pc = next;
                        }
                        Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
                    }
                }
            }
            Op::Store { w, rd, addr, cond } => {
                if !self.regs.flags.test(cond) {
                    self.regs.pc = next;
                } else {
                    let ea = self.resolve_addr(pc, addr, w);
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
            }

            Op::Version { rd } => {
                self.regs
                    .set(rd as usize, self.version_value | (self.core_id << 16));
                self.regs.pc = next;
            }

            Op::MovToCoproc { preg, rs } => {
                self.coproc[preg as usize & 31] = self.regs.get(rs as usize);
                self.regs.pc = next;
            }
            Op::MovFromCoproc { rd, preg } => {
                let v = self.coproc[preg as usize & 31];
                self.regs.set(rd as usize, v);
                self.regs.pc = next;
            }

            Op::Switch { rd, byte } => {
                // Table starts at `next` (right after the 2-byte instruction);
                // entry[idx] is a halfword displacement from that base.
                let idx = self.regs.get(rd as usize);
                let entry_addr = next.wrapping_add(if byte { idx } else { idx * 2 });
                let disp = if byte {
                    match bus.load8(entry_addr) {
                        Ok(v) => v as u32,
                        Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
                    }
                } else {
                    match bus.load16(entry_addr) {
                        Ok(v) => v as u32,
                        Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
                    }
                };
                self.regs.pc = next.wrapping_add(disp * 2);
            }

            Op::AddCmpB {
                cond,
                rd,
                a,
                b,
                target,
            } => {
                // `rd += a; compare (rd) with b; branch if <cond>`. The compare
                // is internal to the instruction — `addcmpb` writes `rd` and the
                // PC but does *not* commit N/Z/C/V (per `vciv.py`: `CF_CHG1`
                // only, no flag change). Firmware relies on this: a `btest` /
                // `cmp` result survives across intervening `addcmpb` loops.
                let av = self.reg_or_imm(a);
                let sum = self.regs.get(rd as usize).wrapping_add(av);
                self.regs.set(rd as usize, sum);
                let bv = self.reg_or_imm(b);
                let (diff, no_borrow, v) = add_with_carry(sum, !bv, 1);
                let flags = nz(diff, !no_borrow, v);
                self.regs.pc = if flags.test(cond) { target } else { next };
            }

            Op::PushMulti {
                first,
                count,
                include_lr,
                lr_slot,
            } => {
                // `stm {rlist, lr}, (--sp)`: `lr` occupies word `lr_slot` in the
                // frame; the register list fills the remaining words in order.
                // (The slot is bank-dependent — see `decode.rs`.)
                let total = count as u32 + include_lr as u32;
                let sp = self.regs.get(SP).wrapping_sub(4 * total);
                let put = |exec: &mut Self, bus: &mut dyn Bus, r: usize, at: u32| -> bool {
                    let v = exec.regs.get(r);
                    match bus.store32(at, v) {
                        Ok(()) => true,
                        Err(err) => {
                            exec.stop(Stop::Fault(Fault::Bus { pc, err }));
                            false
                        }
                    }
                };
                let mut reg_i = 0usize;
                for w in 0..total {
                    let at = sp.wrapping_add(4 * w);
                    let ok = if include_lr && w == lr_slot as u32 {
                        put(self, bus, LR, at)
                    } else {
                        let r = ((first as usize) + reg_i) & 31;
                        reg_i += 1;
                        put(self, bus, r, at)
                    };
                    if !ok {
                        return Step::Stopped;
                    }
                }
                self.regs.set(SP, sp);
                self.regs.pc = next;
            }

            Op::PopMulti {
                first,
                count,
                include_pc,
                lr_slot,
            } => {
                // Mirror of `PushMulti`: `pc` comes from word `lr_slot`.
                let sp = self.regs.get(SP);
                let total = count as u32 + include_pc as u32;
                let sp_after = sp.wrapping_add(4 * total);
                let mut new_pc = next;
                let mut reg_i = 0usize;
                let mut popped_sp = false;
                for w in 0..total {
                    let at = sp.wrapping_add(4 * w);
                    let v = match bus.load32(at) {
                        Ok(v) => v,
                        Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
                    };
                    if include_pc && w == lr_slot as u32 {
                        new_pc = v;
                    } else {
                        let r = ((first as usize) + reg_i) & 31;
                        reg_i += 1;
                        self.regs.set(r, v);
                        if r == SP {
                            popped_sp = true;
                        }
                    }
                }
                // A context-restore `ldm {…, sp, …}` loads sp from the stack;
                // that value wins over the auto-increment.
                if !popped_sp {
                    self.regs.set(SP, sp_after);
                }
                self.regs.pc = new_pc;
            }

            Op::Unimpl {
                raw,
                class,
                len: ilen,
            } => {
                self.note_unimpl(pc, raw, ilen, class);
                match self.on_unimpl {
                    UnimplPolicy::Fault => {
                        return self.stop(Stop::Fault(Fault::Unimplemented { pc, raw, class }))
                    }
                    UnimplPolicy::Skip => {
                        self.skipped += 1;
                        self.regs.pc = next;
                    }
                }
            }
        }

        if let Some(before) = trace_before {
            let took_branch = before.pc.wrapping_add(insn.len as u32) != self.regs.pc;
            let notable = matches!(
                insn.op,
                Op::Swi { .. }
                    | Op::MovFromCoproc { .. }
                    | Op::MovToCoproc { .. }
                    | Op::Version { .. }
                    | Op::Unimpl { .. }
                    | Op::Rti
            );
            // Control-flow trace: skip the millions of straight-line ops inside
            // memset/memcpy, keep every transfer and every notable op.
            if !self.trace_cf_only || took_branch || notable || self.is_stopped() {
                let mut line = format!("{pc:#010x}  {:<26}", format!("{:?}", insn.op));
                for r in 0..32 {
                    if before.get(r) != self.regs.get(r) {
                        line.push_str(&format!("  r{r}={:#x}", self.regs.get(r)));
                    }
                }
                if took_branch && !self.is_stopped() {
                    line.push_str(&format!("  -> {:#010x}", self.regs.pc));
                }
                self.trace_log.push(line);
            }
        }

        if !self.is_stopped() {
            self.retired += 1;
            if self.regs.pc != next {
                // A taken control transfer. Keep a bounded ring for tracing;
                // collapse an immediately-repeating transfer (tight loop /
                // memset) into a single entry with a count so the ring keeps
                // the history that led into it.
                match self.cf_trace.last_mut() {
                    Some((f, t)) if *f == pc && *t == self.regs.pc => {}
                    _ => {
                        if self.cf_trace.len() == self.cf_trace.capacity()
                            && !self.cf_trace.is_empty()
                        {
                            self.cf_trace.remove(0);
                        }
                        self.cf_trace.push((pc, self.regs.pc));
                    }
                }
            }
        }
        Step::Ran
    }

    fn reg_or_imm(&self, x: RegOrImm) -> u32 {
        match x {
            RegOrImm::Reg(r) => self.regs.get(r as usize),
            RegOrImm::Imm(i) => i as u32,
        }
    }

    /// Compute the effective address for a load/store and apply any base-register
    /// writeback. `w` gives the access size, used by pre-dec / post-inc.
    fn resolve_addr(&mut self, pc: u32, addr: AddrMode, w: MemWidth) -> u32 {
        let size = match w {
            MemWidth::Word => 4u32,
            MemWidth::Half | MemWidth::SignedHalf => 2,
            MemWidth::Byte => 1,
        };
        let base_reg = match addr.base {
            Base::Reg(r) => Some(r as usize),
            Base::Sp => Some(SP),
            Base::Gp => Some(GP),
            Base::R0 => Some(0),
            Base::Pc | Base::RegReg(..) => None,
        };
        let base_val = match addr.base {
            Base::Reg(r) => self.regs.get(r as usize),
            Base::Sp => self.regs.get(SP),
            Base::Gp => self.regs.get(GP),
            Base::R0 => self.regs.get(0),
            Base::Pc => pc,
            // `ld{w} rd, (ra + rb)` — the index register is scaled by the
            // access size (word → *4, half → *2, byte → *1), per the VC4 ISA
            // note ("#todo rb<<size" in Hermitage's `videocoreiv.arch`).
            Base::RegReg(a, b) => {
                let scale = size.trailing_zeros();
                self.regs
                    .get(a as usize)
                    .wrapping_add(self.regs.get(b as usize).wrapping_shl(scale))
            }
        };

        match addr.writeback {
            Writeback::None => base_val.wrapping_add(addr.offset as u32),
            Writeback::PreDec => {
                let ea = base_val.wrapping_sub(size);
                if let Some(r) = base_reg {
                    self.regs.set(r, ea);
                }
                ea
            }
            Writeback::PostInc => {
                if let Some(r) = base_reg {
                    self.regs.set(r, base_val.wrapping_add(size));
                }
                base_val
            }
        }
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
        // VC4 sets the carry flag to *borrow* on subtraction (x86-style, and
        // per the arch's `cs/lo` + `cc/hs` aliasing), so `carry` here is the
        // negation of the ARM-style no-borrow result.
        Sub | Cmp => {
            let (r, c, v) = add_with_carry(a, !b, 1);
            return Some((r, nz(r, !c, v)));
        }
        Rsub => {
            let (r, c, v) = add_with_carry(b, !a, 1);
            return Some((r, nz(r, !c, v)));
        }
        Neg => {
            let (r, c, v) = add_with_carry(0, !b, 1);
            return Some((r, nz(r, !c, v)));
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
        AddScale(s) => a.wrapping_add(b.wrapping_shl(s as u32)),
        SubScale(s) => a.wrapping_sub(b.wrapping_shl(s as u32)),
        Count => b.count_ones(),
        MulhdSS => (((a as i32 as i64) * (b as i32 as i64)) >> 32) as u32,
        MulhdSU => (((a as i32 as i64) * (b as i64)) >> 32) as u32,
        MulhdUS => (((a as i64) * (b as i32 as i64)) >> 32) as u32,
        MulhdUU => (((a as u64) * (b as u64)) >> 32) as u32,
        DivS => {
            if b == 0 {
                0
            } else {
                (a as i32).wrapping_div(b as i32) as u32
            }
        }
        DivSU => {
            if b == 0 {
                0
            } else {
                ((a as i32 as i64) / (b as i64)) as u32
            }
        }
        DivUS => {
            if b == 0 {
                0
            } else {
                ((a as i64) / (b as i32 as i64)) as u32
            }
        }
        DivU => {
            if b == 0 {
                0
            } else {
                a / b
            }
        }
        Clamp16 => (a as i32).clamp(-32768, 32767) as u32,
        Unimpl(_) => return None,
    };
    Some((r, nz(r, cin, false)))
}

/// Evaluate a condition against flags — thin re-export for callers holding a
/// `Cond` without a `Flags`.
pub fn cond_holds(flags: &Flags, cond: Cond) -> bool {
    flags.test(cond)
}
