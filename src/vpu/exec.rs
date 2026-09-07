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
    /// Base of the exception vector table. `swi #u` raises exception `0x20 + u`
    /// and jumps to the **4-byte** entry `*(exc_vbase + exc*4)`, after pushing SR
    /// and the return address (so the handler's `rti` unwinds). start4.elf's
    /// trampoline writes this base to CoreCtl `0x7E00_2030` (core 0); the table
    /// lives at `0xCEC0_1E00` with the `swi` (0x20) slot at `+0x80`.
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
            Op::Sleep => {
                // `sleep` waits for an interrupt. We model no async wakeups, so
                // in recon (skip) mode treat it as a nop — firmware idle/dispatch
                // loops (`sleep; b loop`) then just spin and the run's step limit
                // or spin detector ends things cleanly. Otherwise halt.
                if matches!(self.on_unimpl, UnimplPolicy::Skip) {
                    self.regs.pc = next;
                    // `sleep` = wait for an interrupt. Ask the bus to advance to
                    // the next armed timer compare; if that raises an enabled
                    // interrupt source, dispatch through the firmware's vector
                    // table (same stack frame convention as `swi`: push SR then
                    // the resume address, so the handler's `rti` unwinds).
                    if self.in_exception == 0 && self.exc_vbase != 0 {
                        if let Some(slot) = bus.timer_wake() {
                            let handler = bus
                                .load32(self.exc_vbase.wrapping_add(slot.wrapping_mul(4)))
                                .ok()
                                .filter(|&h| h != 0)
                                .map(|h| h & !1);
                            if let Some(mut h) = handler {
                                // start4's dispatching vector stubs begin with a
                                // `0x0000` guard parcel; the stub body follows.
                                if bus.load16(h) == Ok(0x0000) {
                                    h = h.wrapping_add(2);
                                }
                                let sp = self.regs.get(SP).wrapping_sub(8);
                                if bus.store32(sp, self.regs.sr).is_ok()
                                    && bus.store32(sp.wrapping_add(4), next).is_ok()
                                {
                                    self.regs.set(SP, sp);
                                    self.in_exception = self.in_exception.wrapping_add(1);
                                    self.regs.pc = h;
                                }
                            }
                        }
                    }
                } else {
                    return self.stop(Stop::Halt(HaltReason::Sleep));
                }
            }
            Op::Bkpt => return self.stop(Stop::Halt(HaltReason::Breakpoint)),
            Op::Swi { vector } => {
                // `vector` is already `0x20 + u`. With a vector table configured,
                // trap to `*(exc_vbase + vector*8)` after pushing SR + return
                // address (so the handler's `rti` unwinds). Otherwise halt — the
                // test payloads use `swi` as a clean "done".
                let handler = if self.exc_vbase != 0 {
                    bus.load32(self.exc_vbase.wrapping_add(vector.wrapping_mul(4)))
                        .ok()
                        .filter(|&h| h != 0)
                        // Vector-table entries carry a flag in bit 0 (start4's
                        // dynamically-installed `swi` dispatcher is stored as
                        // `addr | 1`); the entry PC is the even address.
                        .map(|h| h & !1)
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
                    None => {
                        // No handler installed. In recon (skip) mode, treat the
                        // trap as a no-op so exploration continues past syscall
                        // stubs (start4's atomic/priv helpers) — it is counted
                        // like a skipped instruction. Otherwise halt.
                        if matches!(self.on_unimpl, UnimplPolicy::Skip) {
                            self.skipped += 1;
                            self.regs.pc = next;
                        } else {
                            return self.stop(Stop::Halt(HaltReason::Swi(vector)));
                        }
                    }
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

            Op::FpAlu3 {
                op,
                cond,
                rd,
                ra,
                b,
            } => {
                if !self.regs.flags.test(cond) {
                    self.regs.pc = next;
                } else {
                    use super::insn::FpOp::*;
                    let a = f32::from_bits(self.regs.get(ra as usize));
                    // The `0xCA00` convert ops (ftrunc/floor/flts/fltu) take an
                    // integer shift as the operand; the `0xC800` triadic ops take
                    // a float. Register operands are always f32 bit-patterns; an
                    // immediate is a raw shift count for the converts, and a
                    // 6-bit minifloat (`s eee mm`, bias 3, implicit `1.mm`) for
                    // the triadic ops — e.g. `0b011001` is `1.25 × 2³ = 10.0`,
                    // which the decimal formatter at `0x80009728` relies on.
                    let is_convert = matches!(op, Ftrunc | FtruncFloor | Flts | Fltu);
                    let (bv, bf) = match b {
                        RegOrImm::Reg(r) => {
                            let raw = self.regs.get(r as usize);
                            (raw, f32::from_bits(raw))
                        }
                        RegOrImm::Imm(i) if is_convert => (i as u32, i as f32),
                        RegOrImm::Imm(i) => (i as u32, fp_minifloat((i as u32) & 0x3F)),
                    };
                    let scale = |sh: u32| 2f32.powi(sh as i32);
                    let rai = self.regs.get(ra as usize);
                    let res: Option<u32> = match op {
                        Fadd => Some((a + bf).to_bits()),
                        Fsub => Some((a - bf).to_bits()),
                        Fmul => Some((a * bf).to_bits()),
                        Fdiv => Some((a / bf).to_bits()),
                        Fabs => Some(a.abs().to_bits()),
                        Frsub => Some((bf - a).to_bits()),
                        Fmax => Some(a.max(bf).to_bits()),
                        Fmin => Some(a.min(bf).to_bits()),
                        Frcp => Some((1.0 / bf).to_bits()),
                        Frsqrt => Some((1.0 / bf.sqrt()).to_bits()),
                        Fnmul => Some((-(a * bf)).to_bits()),
                        Fceil => Some(bf.ceil().to_bits()),
                        Ffloor => Some(bf.floor().to_bits()),
                        Flog2 => Some(bf.log2().to_bits()),
                        Fexp2 => Some(bf.exp2().to_bits()),
                        Ftrunc => Some(((a * scale(bv)) as i64 as i32) as u32),
                        FtruncFloor => Some((((a * scale(bv)).floor()) as i64 as i32) as u32),
                        Flts => Some(((rai as i32 as f32) / scale(bv)).to_bits()),
                        Fltu => Some(((rai as f32) / scale(bv)).to_bits()),
                        Fcmp => {
                            let lt = a < bf;
                            self.regs.flags.n = lt;
                            self.regs.flags.c = lt;
                            self.regs.flags.z = a == bf;
                            self.regs.flags.v = false;
                            None
                        }
                    };
                    if let Some(v) = res {
                        self.regs.set(rd as usize, v);
                        // FP ALU ops update N/Z from the (float) result.
                        let fv = f32::from_bits(v);
                        self.regs.flags.n = fv.is_sign_negative() && fv != 0.0;
                        self.regs.flags.z = fv == 0.0;
                        self.regs.flags.c = false;
                        self.regs.flags.v = false;
                    }
                    self.regs.pc = next;
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
                // entry[idx] is a *signed* displacement (in halfwords) from that
                // base — handlers defined before the `switch` are reached with a
                // negative entry, and the default/unknown case is a small
                // negative offset back to the literal-`%` fallback.
                let idx = self.regs.get(rd as usize);
                let entry_addr = next.wrapping_add(if byte { idx } else { idx * 2 });
                let disp = if byte {
                    match bus.load8(entry_addr) {
                        Ok(v) => v as i8 as i32,
                        Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
                    }
                } else {
                    match bus.load16(entry_addr) {
                        Ok(v) => v as i16 as i32,
                        Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
                    }
                };
                self.regs.pc = next.wrapping_add((disp * 2) as u32);
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
            } => {
                // `stm {rlist, lr}, (--sp)`: register list in ascending memory
                // order, then `lr` at the top word of the frame.
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
                // Register list in ascending memory order, then `lr` at the top
                // word of the frame. (`count == 0` is the `stm lr` / `ldm pc`
                // form — see `decode.rs`.)
                for w in 0..count as u32 {
                    let r = ((first as usize) + w as usize) & 31;
                    if !put(self, bus, r, sp.wrapping_add(4 * w)) {
                        return Step::Stopped;
                    }
                }
                if include_lr && !put(self, bus, LR, sp.wrapping_add(4 * count as u32)) {
                    return Step::Stopped;
                }
                self.regs.set(SP, sp);
                self.regs.pc = next;
            }

            Op::PopMulti {
                first,
                count,
                include_pc,
            } => {
                // Mirror of `PushMulti`: `pc` comes from the top word.
                let sp = self.regs.get(SP);
                let total = count as u32 + include_pc as u32;
                let sp_after = sp.wrapping_add(4 * total);
                let mut new_pc = next;
                let mut popped_sp = false;
                for w in 0..count as u32 {
                    let at = sp.wrapping_add(4 * w);
                    let v = match bus.load32(at) {
                        Ok(v) => v,
                        Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
                    };
                    let r = ((first as usize) + w as usize) & 31;
                    self.regs.set(r, v);
                    if r == SP {
                        popped_sp = true;
                    }
                }
                if include_pc {
                    match bus.load32(sp.wrapping_add(4 * count as u32)) {
                        Ok(v) => new_pc = v,
                        Err(err) => return self.stop(Stop::Fault(Fault::Bus { pc, err })),
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

/// Decode the 6-bit floating-point immediate of a `0xC800` triadic FP op.
///
/// Layout `s eee mm`: sign, 3-bit exponent (bias 3), 2-bit mantissa with an
/// implicit leading 1. `eee == 0` is zero. So `0b011001` → `+1.25 × 2³ = 10.0`,
/// `0b001100` → `1.0`, `0b010000` → `2.0`, `0b001000` → `0.5`.
fn fp_minifloat(i: u32) -> f32 {
    let sign = if i & 0x20 != 0 { -1.0 } else { 1.0 };
    let exp = ((i >> 2) & 7) as i32;
    let mant = (i & 3) as f32;
    if exp == 0 {
        return sign * (mant / 4.0) * 2f32.powi(-2); // subnormal; 0 when mant==0
    }
    sign * (1.0 + mant / 4.0) * 2f32.powi(exp - 3)
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
