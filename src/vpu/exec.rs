//! The VPU scalar executor: fetch → decode → execute one instruction.

use crate::bus::{Bus, BusError, Width};

/// How many control transfers [`Vpu::cf_trace`] keeps. The run report prints
/// the tail of it when a boot derails, which is the main thing it is for.
const CF_TRACE_LEN: usize = 512;

use super::decode::decode;
use super::insn::{AddrMode, AluOp, Base, MemWidth, Op, RegOrImm, VecExec, VecInsn, Writeback};
use super::length::InsnClass;
use super::reg::{Cond, Flags, Regs, GP, LR, SP};

/// Sign-extend the low `bits` of `v` to 32 bits.
#[inline]
fn sext_to(v: u32, bits: u8) -> u32 {
    if bits >= 32 {
        return v;
    }
    let shift = 32 - bits as u32;
    (((v << shift) as i32) >> shift) as u32
}

/// Zero-extend the low `bits` of `v` to 32 bits.
#[inline]
fn zext_to(v: u32, bits: u8) -> u32 {
    if bits >= 32 {
        return v;
    }
    v & ((1u32 << bits) - 1)
}

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
    Bus {
        pc: u32,
        err: BusError,
    },
    Unimplemented {
        pc: u32,
        raw: u128,
        class: InsnClass,
    },
    UnimplementedAlu {
        pc: u32,
        op: &'static str,
    },
}

/// What to do when the core meets an instruction the decoder/executor does not
/// implement.
///
/// The variants also select how strict the core is about the *other* ways a run
/// can wander off the rails — `bkpt` padding, `sleep` with no wakeup source, and
/// a `swi` with no handler installed. A scenario wants those to halt; a whole
/// firmware boot has to step over them to get anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnimplPolicy {
    /// Stop with a [`Fault::Unimplemented`], and halt on `bkpt`/`sleep`/an
    /// unhandled `swi`. The default — silence here means silently wrong
    /// execution.
    #[default]
    Fault,
    /// Fault on an unknown instruction, but keep the reconnaissance leniencies
    /// so a whole boot can run. What `recon` uses unless told otherwise.
    ReconFault,
    /// Advance past it (correct length) and keep going. For "how far does the
    /// firmware get / what does it touch" reconnaissance runs on firmware the
    /// decoder has not been taught yet.
    Skip,
}

impl UnimplPolicy {
    /// Step over `bkpt` padding, treat `sleep` as a nop, and ignore a `swi`
    /// with no handler, rather than halting the core.
    #[inline]
    pub fn recon_lenient(self) -> bool {
        matches!(self, UnimplPolicy::Skip | UnimplPolicy::ReconFault)
    }
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
    pub raw: u128,
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
    /// Consecutive `bkpt` (`0x0000`) parcels stepped over, and whether the
    /// resulting nop-slide has already been reported.
    bkpt_run: u32,
    derail_reported: bool,
    /// Previous instruction's `pc`, and the `pc` that branched into the current
    /// nop-slide (for [`Self::bkpt_run`] derail reporting).
    prev_pc: u32,
    slide_from: u32,
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
    /// `RVF_DBG_SLEEP` counter: how many times the idle loop's `sleep` has
    /// been reached.
    pub sleep_dbg: u64,
    /// System-coprocessor register file (`mov p<n>,r` / `mov r,p<n>`). Not real
    /// hardware behaviour — reads return the last written value (0 at reset),
    /// which is enough to clear the early-boot "wait for p16 == 0" loops.
    pub coproc: [u32; 32],
    /// Ring of recent taken control transfers `(from_pc, to_pc)`.
    ///
    /// A `VecDeque`, not a `Vec`: this is a ring, and dropping the oldest entry
    /// with `Vec::remove(0)` memmoved the whole buffer on every control
    /// transfer once it filled. That single line was 6.8% of the emulator's
    /// total run time, measured with `perf` — the largest cost in `Vpu::step`
    /// after decode.
    pub cf_trace: std::collections::VecDeque<(u32, u32)>,
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
    /// Diagnostic switches, read once at construction. Reading them from the
    /// environment inside the step loop instead costs a `getenv` per `sleep`
    /// instruction, and ThreadX's idle loop is nothing but `sleep`.
    dbg_tick: bool,
    dbg_vec: bool,
    dbg_sleep: bool,
    dbg_derail: bool,
}

impl Vpu {
    pub fn new(entry: u32) -> Vpu {
        let mut v = Vpu::default();
        v.regs.pc = entry;
        v.version_value = DEFAULT_VERSION;
        v.cf_trace = std::collections::VecDeque::with_capacity(CF_TRACE_LEN);
        v.trace_cap = 20_000;
        v.dbg_tick = std::env::var_os("RVF_DBG_TICK").is_some();
        v.dbg_vec = std::env::var_os("RVF_DBG_VEC").is_some();
        v.dbg_sleep = std::env::var_os("RVF_DBG_SLEEP").is_some();
        v.dbg_derail = std::env::var_os("RVF_DBG_DERAIL").is_some();
        // VC4 comes out of reset with interrupts enabled; ThreadX runs threads
        // that way too. `di`/`ei` toggle it from here.
        v.regs.set(30, 1 << 30);
        v.regs.sr = 1 << 30;
        v
    }

    fn note_unimpl(&mut self, pc: u32, raw: u128, len: u8, class: InsnClass) {
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

    /// Record an instruction the model cannot carry out and apply
    /// [`Self::on_unimpl`].
    ///
    /// Returns `Some(step)` when the run must stop; `None` when the policy is
    /// to skip, in which case the pc has already been advanced past it.
    fn unimpl(&mut self, pc: u32, raw: u128, len: u8, class: InsnClass, next: u32) -> Option<Step> {
        self.note_unimpl(pc, raw, len, class);
        match self.on_unimpl {
            UnimplPolicy::Fault | UnimplPolicy::ReconFault => {
                Some(self.stop(Stop::Fault(Fault::Unimplemented { pc, raw, class })))
            }
            UnimplPolicy::Skip => {
                self.skipped += 1;
                self.regs.pc = next;
                None
            }
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

    /// Vector into the firmware's interrupt handler for `slot` (== the source's
    /// enabled priority): push SR + the current `pc` as the resume address (like
    /// `swi`, so the handler's `rti` unwinds) and jump to
    /// `*(exc_vbase + slot*4)`. No-op if already in an exception or the vector
    /// entry is null. Used for both the `sleep`-instruction wake and the run
    /// loop's periodic ThreadX tick.
    pub fn vector_irq(&mut self, bus: &mut dyn Bus, slot: u32) {
        if !self.irq_enabled() {
            return;
        }
        self.vector_irq_forced(bus, slot);
    }

    /// [`Self::vector_irq`] without the interrupt-enable check, for the `sleep`
    /// wake. ThreadX's scheduler idle loop parks as `…; sleep; di; b …` with
    /// interrupts already off and relies on the wake itself to service the
    /// pending periodic tick — nothing in that loop ever runs `ei`.
    pub fn vector_irq_forced(&mut self, bus: &mut dyn Bus, slot: u32) {
        if self.exc_vbase == 0 {
            return;
        }
        let handler = bus
            .load32(self.exc_vbase.wrapping_add(slot.wrapping_mul(4)))
            .ok()
            .filter(|&h| h != 0)
            .map(|h| h & !1);
        if let Some(mut h) = handler {
            // start4's dispatching vector stubs begin with a `0x0000` guard
            // parcel; the stub body follows.
            if bus.load16(h) == Ok(0x0000) {
                h = h.wrapping_add(2);
            }
            if self.dbg_vec {
                eprintln!(
                    "[vec] slot={slot} vbase={:#x} entry={:#x} h={h:#x} pc={:#x} sp={:#x} cur={:#x} exec={:#x} nest={}",
                    self.exc_vbase,
                    bus.load32(self.exc_vbase.wrapping_add(slot.wrapping_mul(4))).unwrap_or(0),
                    self.regs.pc,
                    self.regs.get(SP),
                    bus.load32(0x3EE35900).unwrap_or(0),
                    bus.load32(0x3EE35904).unwrap_or(0),
                    self.in_exception,
                );
            }
            let resume = self.regs.pc;
            let sp = self.regs.get(SP).wrapping_sub(8);
            if bus.store32(sp, self.sr()).is_ok() && bus.store32(sp.wrapping_add(4), resume).is_ok()
            {
                self.regs.set(SP, sp);
                self.in_exception = self.in_exception.wrapping_add(1);
                // Taking an exception clears the interrupt-enable bit; the
                // handler re-enables it explicitly (`ei`) or implicitly, by
                // restoring the saved SR through `rti`. This — not a nesting
                // count — is what serialises delivery, and it is the only model
                // that works for ThreadX: `_tx_thread_schedule` enters its idle
                // loop (`0x3EC3FFCA`..`0x3EC40016`) from *inside* the tick ISR
                // and never returns from it, so any depth counter stays pinned
                // above zero and wedges every later tick.
                self.regs.pc = h;
            }
        }
    }

    /// The VC4 status register value to save on an exception: `r30` (carrying
    /// the interrupt-enable bit, [`Op::SetIrqEnable`]) with the live N/Z/C/V
    /// condition flags folded into the low nibble (SR layout `… ZNCV`), so a
    /// handler's `rti` restores the interrupted context's flags — ThreadX
    /// preempts threads mid-`cmp`/`b<cond>` and the tick handler clobbers the
    /// flags in between.
    fn sr(&mut self) -> u32 {
        let v = (self.regs.get(30) & !0xF) | nzcv_to_sr(self.regs.flags);
        self.regs.set(30, v);
        self.regs.sr = v;
        v
    }

    /// True when interrupts are enabled (SR / `r30` bit 30). Firmware `msleep`
    /// and the run loop's periodic ThreadX tick gate on this.
    pub fn irq_enabled(&self) -> bool {
        self.regs.get(30) & (1 << 30) != 0
    }

    /// Fetch the instruction bytes at `pc` into a 10-byte buffer.
    fn fetch(&self, bus: &mut dyn Bus, pc: u32) -> Result<([u8; 10], u8), BusError> {
        let mut buf = [0u8; 10];
        let len = bus.read_insn(pc, &mut buf)?;
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
        let trace_before =
            if self.trace && self.trace_armed && self.trace_log.len() < self.trace_cap {
                Some(self.regs.clone())
            } else {
                None
            };

        if !matches!(insn.op, Op::Bkpt) {
            self.bkpt_run = 0;
        }
        let prev_pc = self.prev_pc;
        self.prev_pc = pc;

        match insn.op {
            Op::Nop => self.regs.pc = next,
            Op::SetIrqEnable(on) => {
                // Track only the interrupt-enable bit of the VC4 status
                // register (`r30`, bit 30). NZCV stay in `regs.flags`; the
                // exception save/restore path (`sr()` / `rti`) carries this bit
                // across handlers. `regs.sr` is kept as a mirror.
                let m = 1u32 << 30;
                let v = if on {
                    self.regs.get(30) | m
                } else {
                    self.regs.get(30) & !m
                };
                self.regs.set(30, v);
                self.regs.sr = v;
                self.regs.pc = next;
            }
            Op::Sleep => {
                // `sleep` waits for an interrupt. We model no async wakeups, so
                // in recon (skip) mode treat it as a nop — firmware idle/dispatch
                // loops (`sleep; b loop`) then just spin and the run's step limit
                // or spin detector ends things cleanly. Otherwise halt.
                if self.on_unimpl.recon_lenient() {
                    self.regs.pc = next;
                    if self.exc_vbase != 0 && self.core_id == 0 {
                        // The ThreadX idle loop parks here with interrupts
                        // disabled, so the run loop's gated delivery never
                        // fires; service a device interrupt here too.
                        //
                        // Core 0 only: the pending queue and the system-timer
                        // compare are the *shared* bus's, which in this model
                        // stands for core 0's half of the interrupt controller
                        // (CoreCtl keeps per-core enable and pending words a
                        // `0x800` stride apart). Core 1 idles in the same
                        // ThreadX `sleep; di; b` loop, so once it has a vector
                        // base it would otherwise steal every interrupt core 0
                        // is waiting for and wedge the boot right after
                        // `Starting start4.elf`.
                        if let Some(src) = bus.take_pending_irq() {
                            // `pc` was already advanced past the `sleep` above,
                            // so `vector_irq` records the right resume point.
                            self.vector_irq_forced(bus, src);
                            return Step::Ran;
                        }
                        // The ThreadX scheduler idle loop parks here as
                        // `sleep; di; b` — interrupts already disabled, relying
                        // on the wake to service the pending periodic tick. The
                        // run loop's tick delivery gates on the SR interrupt-
                        // enable bit and so never fires once the idle loop has
                        // run its `di`; deliver the pending tick here instead.
                        // Only when a compare has actually fired (not on every
                        // `sleep`) so time isn't raced forward.
                        // Peek the slot *before* consuming the pending flag:
                        // the slot encodes which compare channel matched
                        // (source `64 + channel`), so consuming first would
                        // mis-route the interrupt.
                        let slot = bus.timer_tick_slot();
                        let took = slot.is_some() && bus.take_tick_pending();
                        if self.dbg_sleep {
                            self.sleep_dbg += 1;
                            if self.sleep_dbg <= 20 || self.sleep_dbg.is_multiple_of(20000) {
                                eprintln!(
                                    "[sleep] #{} pc={:#x} slot={slot:?} took={took} retired={}",
                                    self.sleep_dbg, self.regs.pc, self.retired
                                );
                            }
                        }
                        if let (Some(slot), true) = (slot, took) {
                            self.vector_irq_forced(bus, slot);
                        } else {
                            // Nothing to service: `sleep` halts the core on real
                            // hardware, so jump to the next armed compare rather
                            // than spinning through ThreadX's `sleep; di; b`
                            // idle loop in real time.
                            bus.sleep_advance();
                        }
                    }
                } else {
                    return self.stop(Stop::Halt(HaltReason::Sleep));
                }
            }
            Op::Bkpt => {
                // start4 emits the `0x0000` parcel as inline 2-byte padding /
                // "unreachable" guards inside functions and at the head of its
                // exception stubs — real VC4 slides through it. A test payload
                // uses `bkpt` as a deliberate stop, so only step over it in
                // reconnaissance mode, and only on core 0 (a mis-entered core 1
                // hitting `0x0000` should still halt rather than nop-slide
                // through DRAM).
                if self.on_unimpl.recon_lenient() && self.core_id == 0 {
                    self.skipped += 1;
                    // A derail into zeroed RAM shows up as a long nop-slide of
                    // `0x0000` parcels. Report the first one, once, so the run
                    // that produced it can be traced back to its last real
                    // instruction instead of only reporting a garbage final pc.
                    self.bkpt_run += 1;
                    if self.bkpt_run == 1 {
                        self.slide_from = prev_pc;
                    }
                    if self.bkpt_run == 64 && !self.derail_reported {
                        self.derail_reported = true;
                        eprintln!(
                            "[derail] nop-slide at pc={pc:#x} from={:#x} lr={:#x} sp={:#x} retired={} regs=[{}]",
                            self.slide_from,
                            self.regs.get(LR),
                            self.regs.get(SP),
                            self.retired,
                            (0..16)
                                .map(|r| format!("{:x}", self.regs.get(r)))
                                .collect::<Vec<_>>()
                                .join(",")
                        );
                    }
                    self.regs.pc = next;
                } else {
                    return self.stop(Stop::Halt(HaltReason::Breakpoint));
                }
            }
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
                        let sr = self.sr();
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
                        if self.on_unimpl.recon_lenient() {
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
                if self.dbg_tick && !(0x3E00_0000..0x3F00_0000).contains(&ret) {
                    eprintln!(
                        "[rti-bad] pc={pc:#x} sp={sp:#x} -> ret={ret:#x} sr={sr:#x} nest={} frame=[{:#x} {:#x} {:#x} {:#x}]",
                        self.in_exception,
                        bus.load32(sp).unwrap_or(0),
                        bus.load32(sp.wrapping_add(4)).unwrap_or(0),
                        bus.load32(sp.wrapping_add(8)).unwrap_or(0),
                        bus.load32(sp.wrapping_add(12)).unwrap_or(0),
                    );
                }
                self.regs.sr = sr;
                self.regs.set(30, sr);
                // Restore the interrupted context's condition flags from the
                // saved SR low nibble (see [`Vpu::sr`]).
                self.regs.flags = sr_to_nzcv(sr);
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
                let b = self.regs.get(rs as usize);
                // `brev` reverses its source operand, which `alu()` takes in
                // `a`; the 16-bit form carries it in `Rs` with no shift.
                let a = if matches!(op, AluOp::Bitrev) {
                    b
                } else {
                    self.regs.get(rd as usize)
                };
                let b = if matches!(op, AluOp::Bitrev) { 0 } else { b };
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
                            // `ld sp, (r0+8)` restores a thread's saved stack
                            // pointer — ThreadX's `_tx_thread_schedule` /
                            // `_tx_thread_context_restore` dispatching a thread
                            // (`tx_thread_stack_ptr` is TCB field +8). Reaching
                            // it while a handler is still "pending" means the
                            // tick ISR concluded by switching threads rather
                            // than running `rti` (the `b r26` cooperative-
                            // restore path), so the pending-exception count
                            // would otherwise leak and wedge periodic-tick
                            // delivery for good. `ld sp, (r29+32)` (switch to
                            // the ISR's own system stack) is *not* that — keep
                            // the count until the real return.
                            if rd as usize == SP
                                && self.in_exception != 0
                                && matches!(addr.base, super::insn::Base::R0)
                            {
                                if self.dbg_tick {
                                    eprintln!(
                                        "[ctx-switch] pc={pc:#x} clear in_exc (was {}) sp<-{v:#x}",
                                        self.in_exception
                                    );
                                }
                                self.in_exception = 0;
                            }
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
                // The register list is stored **highest register at the lowest
                // address**, with `lr` in the top word of the frame. (`count ==
                // 0` is the `stm lr` / `ldm pc` form — see `decode.rs`.)
                //
                // This ordering is what makes ThreadX's interrupt frame
                // compose: the ISR stub does `push {r0-r5, lr}` and
                // `_tx_thread_context_save` (`0x3EC3FA34`) then does
                // `push {r6-r15}; push {r16-r23}`, and `_tx_thread_schedule`
                // (`0x3EC40040`) restores the lot with
                // `pop {r16-r23}; pop {r0-r15}; ld r26,(sp)++; rti`. A single
                // 16-register pop can only undo those two separate pushes if
                // each block runs downwards in register number, so that the
                // `{r6-r15}` block lands exactly where `pop {r0-r15}` looks for
                // r15..r6 and the stub's `{r0-r5}` block where it looks for
                // r5..r0. Ascending order rotates the register file by 6 on
                // every preemptive context switch.
                for w in 0..count as u32 {
                    let r = ((first as usize) + (count as usize - 1 - w as usize)) & 31;
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
                    let r = ((first as usize) + (count as usize - 1 - w as usize)) & 31;
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

            Op::Vector(ref v) => {
                match v.executable() {
                    VecExec::DiscardedLoad {
                        base,
                        offset,
                        bytes,
                    } => {
                        // The destination is a dash, so nothing lands in a
                        // register — but the read still happens on the bus, and
                        // an MMIO read can have side effects. Errors go nowhere:
                        // there is no destination to fault into.
                        let addr = self.regs.get(base as usize).wrapping_add(offset);
                        for i in 0..bytes {
                            let _ = bus.load8(addr.wrapping_add(i));
                        }
                    }
                    VecExec::SumOfBroadcast { src, dst, signed } => {
                        // `v<w>mov -, rN SUM{S,U} rK`: rN is broadcast across
                        // all 16 lanes at the operation width, the vector result
                        // is discarded, and the scalar result unit writes the
                        // sum of the lanes back to rK.
                        let lane = self.regs.get(src as usize);
                        let lane = if signed {
                            sext_to(lane, v.lane_bits)
                        } else {
                            zext_to(lane, v.lane_bits)
                        };
                        let sum = lane.wrapping_mul(VecInsn::LANES);
                        self.regs.set(dst as usize, sum);
                        // The SRU writeback also updates the scalar N and Z
                        // flags (`videocoreiv.arch`, "<sru> modifier").
                        self.regs.flags.z = sum == 0;
                        self.regs.flags.n = (sum as i32) < 0;
                    }
                    VecExec::NeedsVrf => {
                        if let Some(step) = self.unimpl(pc, v.raw, v.len, InsnClass::Vector48, next)
                        {
                            return step;
                        }
                        // `unimpl` already placed the pc.
                        return Step::Ran;
                    }
                }
                self.regs.pc = next;
            }

            Op::Unimpl {
                raw,
                class,
                len: ilen,
            } => {
                if let Some(step) = self.unimpl(pc, raw as u128, ilen, class, next) {
                    return step;
                }
                // VC4 libc `memcpy` (`0x3EDA28C0`) vectorises its aligned bulk
                // copy with `v32` vld/vst (`0x3EDA28F2` load, `0x3EDA2904`
                // store) the model doesn't decode — skipping them silently
                // corrupts every large aligned copy (e.g. gpioman's built-in
                // dt-blob). Emulate the copy at the store: `r1` has been
                // advanced past the chunk, `r3` still points at its start, `r0`
                // counts 64-byte rows.
                if pc == 0x3EDA_2904 {
                    let n = self.regs.get(0).wrapping_shl(6);
                    let dst = self.regs.get(3);
                    let src = self.regs.get(1).wrapping_sub(n);
                    for i in 0..n {
                        let b = bus.load8(src.wrapping_add(i)).unwrap_or(0);
                        let _ = bus.store8(dst.wrapping_add(i), b);
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
                // Reconnaissance: flag the exact instruction that first jumps
                // out of start4's code range (a derail — bad computed branch,
                // corrupt return address). `RVF_DBG_DERAIL=1`.
                let in_code = |a: u32| (0x3E00_0000..0x3F00_0000).contains(&a);
                if in_code(pc) && !in_code(self.regs.pc) && self.core_id == 0 && self.dbg_derail {
                    eprintln!(
                        "[derail] {pc:#x} ({:?}) -> {:#x}  regs r0-9: {:08x?}",
                        insn.op,
                        self.regs.pc,
                        (0..10).map(|i| self.regs.get(i)).collect::<Vec<_>>(),
                    );
                }
                // A taken control transfer. Keep a bounded ring for tracing;
                // collapse an immediately-repeating transfer (tight loop /
                // memset) into a single entry with a count so the ring keeps
                // the history that led into it.
                match self.cf_trace.back_mut() {
                    Some((f, t)) if *f == pc && *t == self.regs.pc => {}
                    _ => {
                        if self.cf_trace.len() == CF_TRACE_LEN {
                            self.cf_trace.pop_front();
                        }
                        self.cf_trace.push_back((pc, self.regs.pc));
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

/// Pack N/Z/C/V into the VC4 status-register low nibble. SR bit layout (per the
/// VideoCore IV programmer's manual): `bit 0 = V`, `bit 1 = C`, `bit 2 = N`,
/// `bit 3 = Z`.
fn nzcv_to_sr(f: Flags) -> u32 {
    (f.v as u32) | ((f.c as u32) << 1) | ((f.n as u32) << 2) | ((f.z as u32) << 3)
}

/// Inverse of [`nzcv_to_sr`].
fn sr_to_nzcv(sr: u32) -> Flags {
    Flags {
        v: sr & 0b0001 != 0,
        c: sr & 0b0010 != 0,
        n: sr & 0b0100 != 0,
        z: sr & 0b1000 != 0,
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
        // `msb Rd, Rs` / `msb Rd, Ra, Rb` — position of the most-significant set
        // bit of the source. The source is the second ALU operand (`Rs` in the
        // 16-bit form, `Rb` — set equal to `Ra` by the assembler — in the
        // triadic form), i.e. `b`, not the destination.
        Msb => {
            if b == 0 {
                u32::MAX
            } else {
                31 - b.leading_zeros()
            }
        }
        // `brev Rd, Ra[, Rb]` — bit-reverse `Ra`, then shift right by `Rb`
        // (0 in the 16-bit form and almost always in the triadic form). The
        // reversible operand is `a`; the 16-bit form feeds `Rs` there (see the
        // `Op::Alu2` handler) with `b == 0`.
        Bitrev => a.reverse_bits().wrapping_shr(b & 31),
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
        // Not `checked_div`: this mirrors `DivUS` above, which cannot use it
        // (the divisor goes through a signed cast), and the pair reads as one
        // rule — the VPU yields 0 rather than trapping on a zero divisor.
        #[allow(clippy::manual_checked_ops)]
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
