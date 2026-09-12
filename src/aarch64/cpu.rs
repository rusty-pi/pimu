//! Architectural state of one Cortex-A72 core and the interface it executes
//! against.
//!
//! The core never owns memory. [`Cpu::step`] takes `&mut dyn Memory`, the same
//! shape as the VPU's [`Bus`](crate::bus::Bus), so the machine can hand both
//! processors the same RAM and peripherals. Addresses here are 64-bit: the
//! A72 sees the full 35-bit physical map of the BCM2711 and, with the MMU on,
//! 48-bit virtual addresses; [`Memory`] only ever sees physical ones
//! (translation is `src/aarch64/mmu.rs`).
//!
//! Two ways to run it: [`Cpu::step`] executes one instruction and reports a
//! synchronous exception without taking it — what a user-mode harness wants,
//! servicing `svc` itself — and [`Cpu::step_system`] is the whole core,
//! taking exceptions and interrupts into the guest the way the hardware does.

use super::exec;
use super::mmu::Tlb;
use super::sysreg::{SysRegs, HCR_TGE, HCR_TSC, HCR_VM, SCR_HCE, SCR_NS, SCR_SMD};
use super::{irq_target, vector_group, VECTOR_FIQ, VECTOR_IRQ, VECTOR_SYNC};

/// A memory access the bus refused. The address is the one the core asked
/// for; the core turns it into a data or instruction abort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Abort {
    pub addr: u64,
    pub write: bool,
}

/// What the core reads and writes. `size` is 1, 2, 4 or 8 bytes; values are
/// zero-extended on read and truncated on write. 16-byte accesses are split
/// into two 8-byte ones by the core.
pub trait Memory {
    fn read(&mut self, addr: u64, size: u32) -> Result<u64, Abort>;
    fn write(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort>;

    /// Fetch the instruction at `addr`. Separate so a machine can serve it
    /// from a faster path than a data read.
    fn fetch(&mut self, addr: u64) -> Result<u32, Abort> {
        self.read(addr, 4).map(|v| v as u32)
    }

    /// System registers the core does not keep itself — the generic timer,
    /// which the machine owns. `key` is [`super::sysreg::key`]. `None` = not
    /// a register this machine knows.
    fn sysreg_read(&mut self, _key: u32) -> Option<u64> {
        None
    }

    /// See [`Self::sysreg_read`]; `false` = not handled.
    fn sysreg_write(&mut self, _key: u32, _value: u64) -> bool {
        false
    }
}

/// A synchronous exception, raised by the instruction at [`Cpu::pc`] (which is
/// left pointing at it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    /// UNDEFINED encoding, or one not available at the current EL.
    Undefined,
    Svc(u16),
    Hvc(u16),
    Smc(u16),
    Brk(u16),
    /// A data access that faulted: translation, permission, the bus, or
    /// alignment. `addr` is the virtual address; `fsc` the `DFSC`
    /// ([`super::mmu`]'s `FSC_*`).
    DataAbort {
        addr: u64,
        write: bool,
        fsc: u8,
    },
    /// The instruction fetch itself faulted; `fsc` is the `IFSC`.
    InsnAbort {
        addr: u64,
        fsc: u8,
    },
    PcAlignment,
}

impl Exception {
    /// `ESR_ELx` for this exception taken from `from_el` to `to_el`, and the
    /// fault address for `FAR_ELx`, if any (ARM ARM D17.2.37). All
    /// instructions here are 32-bit, so `IL` is always set.
    fn syndrome(self, from_el: u32, to_el: u32, pc: u64) -> (u64, Option<u64>) {
        use super::{EC_BRK64, EC_HVC64, EC_SMC64, EC_SVC64, EC_UNKNOWN, ESR_IL};
        let lower = from_el < to_el;
        let ec = |c: u64| (c << 26) | ESR_IL;
        match self {
            Exception::Undefined => (ec(EC_UNKNOWN), None),
            Exception::Svc(i) => (ec(EC_SVC64) | u64::from(i), None),
            Exception::Hvc(i) => (ec(EC_HVC64) | u64::from(i), None),
            Exception::Smc(i) => (ec(EC_SMC64) | u64::from(i), None),
            Exception::Brk(i) => (ec(EC_BRK64) | u64::from(i), None),
            Exception::InsnAbort { addr, fsc } => (
                ec(if lower { 0x20 } else { 0x21 }) | u64::from(fsc),
                Some(addr),
            ),
            Exception::DataAbort { addr, write, fsc } => {
                let wnr = (write as u64) << 6;
                (
                    ec(if lower { 0x24 } else { 0x25 }) | wnr | u64::from(fsc),
                    Some(addr),
                )
            }
            Exception::PcAlignment => (ec(0x22), Some(pc)),
        }
    }
}

/// Result of one [`Cpu::step`] or [`Cpu::step_system`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Retired; `pc` is the next instruction.
    Retired,
    /// `wfi` retired; the core may sleep until an interrupt is pending.
    Wfi,
    /// `wfe` retired; the core may sleep until an event or interrupt.
    Wfe,
    /// [`Cpu::step`] only: the instruction raised a synchronous exception.
    /// `pc` is unchanged; the caller services it (the user-mode test
    /// harness handles `svc` as a Linux syscall).
    Exception(Exception),
    /// [`Cpu::step_system`] only: the instruction raised this exception and
    /// the core took it; `pc` is at the vector.
    Took(Exception),
    /// [`Cpu::step_system`] only: an interrupt was taken instead of
    /// executing an instruction.
    Interrupt { fiq: bool },
    /// An encoding the architecture defines but this core does not implement
    /// yet. `pc` is unchanged. Kept apart from [`Exception::Undefined`] so a
    /// gap in the model never passes for guest behaviour.
    Unimplemented(u32),
    /// The guest turned on something the model does not do yet; `pc` is
    /// unchanged.
    Unsupported(&'static str),
}

/// `PSTATE.{N,Z,C,V}` in `NZCV` register layout (bits 31..28).
pub const NZCV_N: u32 = 1 << 31;
pub const NZCV_Z: u32 = 1 << 30;
pub const NZCV_C: u32 = 1 << 29;
pub const NZCV_V: u32 = 1 << 28;

/// One A72 core.
#[derive(Clone)]
pub struct Cpu {
    /// X0..X30. Register number 31 is SP or XZR depending on the encoding.
    pub x: [u64; 31],
    /// `SP_EL0`..`SP_EL3`.
    pub sp_el: [u64; 4],
    pub pc: u64,
    /// `PSTATE.NZCV`, bits 31..28.
    pub nzcv: u32,
    /// `PSTATE.{D,A,I,F}`, bits 9..6 (the `DAIF` register layout).
    pub daif: u32,
    /// `PSTATE.EL`.
    pub el: u32,
    /// `PSTATE.SP`: use `SP_ELx` rather than `SP_EL0`.
    pub spsel: bool,
    /// V0..V31, the SIMD&FP registers.
    pub v: [u128; 32],
    pub fpcr: u32,
    pub fpsr: u32,
    /// `TPIDR_EL0`, `TPIDRRO_EL0`.
    pub tpidr_el0: u64,
    pub tpidrro_el0: u64,
    /// Everything else from EL1 up (`src/aarch64/sysreg.rs`).
    pub sys: SysRegs,
    /// The core's IRQ and FIQ inputs, as the interrupt controller drives
    /// them. [`Cpu::step_system`] takes them when routing and masking allow.
    pub irq_line: bool,
    pub fiq_line: bool,
    /// Stage 1 translations (`src/aarch64/mmu.rs`).
    pub tlb: Tlb,
    /// Set by the executor while an `LDTR`/`STTR` accesses memory.
    pub(super) unprivileged: bool,
    /// The exclusive monitor: the address `ldxr` marked, if any, as
    /// `(virtual, physical)`.
    pub(super) exclusive: Option<(u64, u64)>,
    /// The physical address of the last data read, for `ldxr` to mark.
    pub(super) last_pa: u64,
    /// Set by the executor for the instruction in flight: where to go next.
    pub(super) next_pc: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// Core 0 out of reset: EL3, `SP_EL3`, `DAIF` masked, PC 0 (the BCM2711's
    /// `RVBAR`), general registers zeroed.
    pub fn new() -> Cpu {
        Cpu::with_id(0)
    }

    /// Core `id` out of reset (`MPIDR_EL1.Aff0 = id`).
    pub fn with_id(id: u32) -> Cpu {
        Cpu {
            x: [0; 31],
            sp_el: [0; 4],
            pc: 0,
            nzcv: 0,
            daif: 0xF << 6,
            el: 3,
            spsel: true,
            v: [0; 32],
            fpcr: 0,
            fpsr: 0,
            tpidr_el0: 0,
            tpidrro_el0: 0,
            sys: SysRegs::new(id),
            irq_line: false,
            fiq_line: false,
            tlb: Tlb::new(),
            unprivileged: false,
            exclusive: None,
            last_pa: 0,
            next_pc: 0,
        }
    }

    /// A core at EL0 with `SP_EL0` selected, the way a Linux process sees it.
    pub fn new_el0() -> Cpu {
        Cpu {
            el: 0,
            spsel: false,
            daif: 0,
            ..Cpu::new()
        }
    }

    /// The stack pointer the current `PSTATE` selects.
    #[inline]
    pub fn sp(&self) -> u64 {
        self.sp_el[self.sp_index()]
    }

    #[inline]
    pub fn set_sp(&mut self, value: u64) {
        let i = self.sp_index();
        self.sp_el[i] = value;
    }

    #[inline]
    fn sp_index(&self) -> usize {
        if self.spsel {
            self.el as usize
        } else {
            0
        }
    }

    /// `PSTATE` in `SPSR` layout (AArch64): NZCV, DAIF, `M[3:0]` = EL:SP.
    pub fn pstate(&self) -> u64 {
        u64::from(self.nzcv) | u64::from(self.daif) | (u64::from(self.el) << 2) | self.spsel as u64
    }

    fn set_pstate(&mut self, spsr: u64) {
        self.nzcv = spsr as u32 & 0xF000_0000;
        self.daif = spsr as u32 & (0xF << 6);
        self.el = ((spsr >> 2) & 3) as u32;
        self.spsel = self.el != 0 && spsr & 1 != 0;
    }

    /// Is the translation regime the core is in now running with its MMU on?
    pub fn mmu_on(&self) -> bool {
        self.regime().is_some()
    }

    /// Another core wrote physical `[lo, hi)`: the global monitor clears this
    /// core's exclusive mark if the write touched its 64-byte granule (the
    /// A72's reservation granule, `CTR_EL0.ERG`).
    pub fn snoop_write(&mut self, lo: u64, hi: u64) {
        if let Some((_, pa)) = self.exclusive {
            let granule = pa & !63;
            if lo < granule + 64 && hi > granule {
                self.exclusive = None;
            }
        }
    }

    /// Exception entry to AArch64 `target` (ARM ARM D1.10.2): save `PSTATE`
    /// and the return address, switch to ELxh with `DAIF` masked, and branch
    /// to the vector. `esr`/`far` only for synchronous exceptions.
    fn enter(&mut self, target: u32, kind: u64, esr: Option<u64>, far: Option<u64>, ret: u64) {
        let t = target as usize;
        let group = vector_group(self.el, target, self.spsel);
        self.sys.spsr[t] = self.pstate();
        self.sys.elr[t] = ret;
        if let Some(esr) = esr {
            self.sys.esr[t] = esr;
        }
        if let Some(far) = far {
            self.sys.far[t] = far;
        }
        self.el = target;
        self.spsel = true;
        self.daif = 0xF << 6;
        self.pc = self.sys.vbar[t].wrapping_add(group + kind);
    }

    /// Take a synchronous exception raised by the instruction at `pc`:
    /// route it (ARM ARM D1.10.3), turning `hvc`/`smc` into UNDEFINED where
    /// `SCR_EL3` disables them, and enter the vector.
    pub fn take_sync(&mut self, e: Exception) {
        let el = self.el;
        let pc = self.pc;
        let secure = el == 3 || self.sys.scr_el3 & SCR_NS == 0;
        let tge = !secure && self.sys.hcr_el2 & HCR_TGE != 0;
        let default = match el {
            0 if tge => 2,
            0 => 1,
            _ => el,
        };
        let next = pc.wrapping_add(4);
        let (e, target, ret) = match e {
            Exception::Svc(_) => (e, default, next),
            Exception::Hvc(_) => {
                if self.sys.scr_el3 & SCR_HCE == 0 || (el == 1 && secure) {
                    (Exception::Undefined, default, pc)
                } else {
                    (e, el.max(2), next)
                }
            }
            Exception::Smc(_) => {
                if self.sys.scr_el3 & SCR_SMD != 0 {
                    (Exception::Undefined, default, pc)
                } else if el == 1 && !secure && self.sys.hcr_el2 & HCR_TSC != 0 {
                    // Trapped to EL2: returns to the `smc` itself.
                    (e, 2, pc)
                } else {
                    (e, 3, next)
                }
            }
            _ => (e, default, pc),
        };
        let (esr, far) = e.syndrome(el, target, pc);
        self.enter(target, VECTOR_SYNC, Some(esr), far, ret);
    }

    /// Take an IRQ (or FIQ) if routing and `PSTATE` allow it now; the return
    /// address is the instruction that would have executed next.
    pub fn take_interrupt(&mut self, fiq: bool) -> bool {
        match irq_target(self.sys.scr_el3, self.sys.hcr_el2, self.pstate(), fiq) {
            Some(target) => {
                let kind = if fiq { VECTOR_FIQ } else { VECTOR_IRQ };
                let ret = self.pc;
                self.enter(target, kind, None, None, ret);
                self.exclusive = None;
                true
            }
            None => false,
        }
    }

    /// `ERET`: back to what `SPSR_ELx` / `ELR_ELx` describe. `false` for an
    /// AArch32 target or an illegal return (to a higher EL), which the model
    /// does not do.
    pub(super) fn eret(&mut self) -> bool {
        let el = self.el as usize;
        let spsr = self.sys.spsr[el];
        if spsr & (1 << 4) != 0 || ((spsr >> 2) & 3) as u32 > self.el {
            return false;
        }
        self.set_pstate(spsr);
        self.next_pc = self.sys.elr[el];
        self.exclusive = None;
        true
    }

    /// Execute one instruction.
    pub fn step(&mut self, mem: &mut dyn Memory) -> Step {
        let pc = self.pc;
        if pc & 3 != 0 {
            return Step::Exception(Exception::PcAlignment);
        }
        let insn = match self.fetch(mem, pc) {
            Ok(i) => i,
            Err(e) => return Step::Exception(e),
        };
        self.next_pc = pc.wrapping_add(4);
        match exec::execute(self, insn, mem) {
            Ok(()) => {
                self.pc = self.next_pc;
                Step::Retired
            }
            Err(exec::Stop::Wfi) => {
                self.pc = self.next_pc;
                Step::Wfi
            }
            Err(exec::Stop::Wfe) => {
                self.pc = self.next_pc;
                Step::Wfe
            }
            Err(exec::Stop::Exception(e)) => Step::Exception(e),
            Err(exec::Stop::Unimplemented) => Step::Unimplemented(insn),
        }
    }

    /// One step of the whole core: take a pending interrupt if it can be
    /// taken, else execute an instruction and take any exception it raises.
    pub fn step_system(&mut self, mem: &mut dyn Memory) -> Step {
        if self.fiq_line && self.take_interrupt(true) {
            return Step::Interrupt { fiq: true };
        }
        if self.irq_line && self.take_interrupt(false) {
            return Step::Interrupt { fiq: false };
        }
        if self.el < 2 && self.sys.scr_el3 & SCR_NS != 0 && self.sys.hcr_el2 & HCR_VM != 0 {
            return Step::Unsupported("stage 2 translation (HCR_EL2.VM = 1)");
        }
        match self.step(mem) {
            Step::Exception(e) => {
                self.take_sync(e);
                Step::Took(e)
            }
            s => s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A few words of code at 0 and a vector table at 0x800 (`VBAR_EL3`).
    struct Mem(Vec<u8>);
    impl Memory for Mem {
        fn read(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
            let a = addr as usize;
            let s = self
                .0
                .get(a..a + size as usize)
                .ok_or(Abort { addr, write: false })?;
            let mut b = [0u8; 8];
            b[..s.len()].copy_from_slice(s);
            Ok(u64::from_le_bytes(b))
        }
        fn write(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
            let a = addr as usize;
            let d = self
                .0
                .get_mut(a..a + size as usize)
                .ok_or(Abort { addr, write: true })?;
            d.copy_from_slice(&value.to_le_bytes()[..size as usize]);
            Ok(())
        }
    }

    fn mem(code: &[u32]) -> Mem {
        let mut m = Mem(vec![0; 0x2000]);
        for (i, w) in code.iter().enumerate() {
            m.write(4 * i as u64, 4, u64::from(*w)).unwrap();
        }
        m
    }

    #[test]
    fn drops_from_el3_to_el2_the_way_the_armstub_does() {
        // msr scr_el3, x0 ; msr spsr_el3, x1 ; msr elr_el3, x2 ; eret
        let mut m = mem(&[0xd51e_1100, 0xd51e_4001, 0xd51e_4022, 0xd69f_03e0]);
        let mut c = Cpu::new();
        c.x[0] = 0x5b1;
        c.x[1] = 0x3c9; // EL2h, DAIF masked
        c.x[2] = 0x1000;
        for _ in 0..4 {
            assert_eq!(c.step_system(&mut m), Step::Retired);
        }
        assert_eq!((c.el, c.spsel, c.pc, c.daif), (2, true, 0x1000, 0xF << 6));
    }

    #[test]
    fn hvc_needs_scr_hce() {
        // hvc #0 at EL2
        let mut m = mem(&[0xd400_0002]);
        let mut c = Cpu::new();
        c.el = 2;
        c.sys.scr_el3 = SCR_NS;
        c.sys.vbar[2] = 0x800;
        assert_eq!(c.step_system(&mut m), Step::Took(Exception::Hvc(0)));
        // Without HCE: UNDEFINED, at the current EL, returning to the hvc.
        assert_eq!(c.sys.esr[2] >> 26, 0x00);
        assert_eq!((c.pc, c.sys.elr[2]), (0x800 + 0x200, 0));
        let mut c = Cpu::new();
        c.el = 2;
        c.sys.scr_el3 = SCR_NS | SCR_HCE;
        c.sys.vbar[2] = 0x800;
        c.step_system(&mut m);
        assert_eq!(c.sys.esr[2] >> 26, 0x16);
        assert_eq!(c.sys.elr[2], 4);
    }

    #[test]
    fn irq_from_el1_vectors_and_returns() {
        let mut m = mem(&[0xd503_201f]); // nop
        let mut c = Cpu::new();
        c.el = 1;
        c.daif = 0;
        c.sys.scr_el3 = SCR_NS;
        c.sys.vbar[1] = 0x1000;
        c.irq_line = true;
        assert_eq!(c.step_system(&mut m), Step::Interrupt { fiq: false });
        assert_eq!(c.pc, 0x1000 + 0x200 + 0x80);
        assert_eq!(c.sys.elr[1], 0);
        assert_eq!(c.sys.spsr[1], 0b0101);
        // Masked now: the next step executes the handler.
        assert_eq!(c.daif, 0xF << 6);
    }
}
