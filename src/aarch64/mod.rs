//! AArch64 architecture facts the ARM core will need (#40): exception entry,
//! interrupt routing, and the system-register move encoding.
//!
//! Lifted from the in-process Unicorn core on the `arm-unicorn` branch
//! (PR #36). Unicorn takes no exception into the guest and has no interrupt
//! input, so that core did exception and IRQ entry by hand, and these are the
//! pieces of it that are pure architecture rather than Unicorn glue. With them
//! it ran the firmware's armstub and the kernel through `hvc`s to its own EL2
//! stub, the scheduler, and thousands of timer and mailbox interrupts.
//!
//! Exception entry (ARM ARM D1.10.2), for an interpreter to do the same way:
//! save `PSTATE` to `SPSR_ELx` and the preferred return address to `ELR_ELx`,
//! write `ESR_ELx` for a synchronous exception, bank the live stack pointer
//! (`SP_EL0` if `PSTATE.SP` was 0, else the old EL's), switch to ELxh with
//! `DAIF` masked, and branch to `VBAR_ELx` + [`vector_group`] + the kind
//! ([`VECTOR_SYNC`] / [`VECTOR_IRQ`] / [`VECTOR_FIQ`]). The preferred return
//! address is the faulting instruction for UNDEFINED and BRK, the next one for
//! SVC/HVC/SMC, and the next instruction to execute for an interrupt (for a
//! core in `wfi`, the one after it).
//!
//! An interrupt pending while the target EL has it masked must be taken soon
//! after the unmask, or Linux livelocks: its idle loop runs `wfi` with IRQs
//! masked and unmasks for only a few instructions after waking
//! (`default_idle_call`: `cpu_do_idle()` then `raw_local_irq_enable()`).
//!
//! The interpreter itself is [`Cpu`] (state, [`Cpu::step`]) and `exec`
//! (decode and execute). `tests/a64_diff.rs` runs random instruction streams
//! through it and through `qemu-aarch64 -cpu cortex-a72` and compares the
//! results.

mod cpu;
mod exec;
pub mod fp;
mod fpinsn;
mod simd;
pub mod sysreg;

pub use cpu::{Abort, Cpu, Exception, Memory, Step, NZCV_C, NZCV_N, NZCV_V, NZCV_Z};

/// ESR_ELx exception classes (ARM ARM D17.2.37).
pub const EC_UNKNOWN: u64 = 0x00;
pub const EC_SVC64: u64 = 0x15;
pub const EC_HVC64: u64 = 0x16;
pub const EC_SMC64: u64 = 0x17;
pub const EC_BRK64: u64 = 0x3C;
/// ESR_ELx.IL: the trapped instruction was 32 bits.
pub const ESR_IL: u64 = 1 << 25;

/// Offsets of the exception kinds within each group of four vectors
/// (ARM ARM D1.10.2).
pub const VECTOR_SYNC: u64 = 0x000;
pub const VECTOR_IRQ: u64 = 0x080;
pub const VECTOR_FIQ: u64 = 0x100;

/// `PSTATE.{D,A,I,F}`.
pub const PSTATE_DAIF: u64 = 0xF << 6;
pub const PSTATE_I: u64 = 1 << 7;
pub const PSTATE_F: u64 = 1 << 6;

/// `PSTATE.M` for ELx using `SP_ELx` ("ELxh").
pub fn pstate_elh(el: u32) -> u64 {
    (u64::from(el) << 2) | 1
}

/// The exception level in a `PSTATE`/`SPSR` value.
pub fn pstate_el(pstate: u64) -> u32 {
    ((pstate >> 2) & 3) as u32
}

/// Interrupt routing bits (ARM ARM D17.2.117 `SCR_EL3`, D17.2.48 `HCR_EL2`).
pub const SCR_NS: u64 = 1 << 0;
pub const SCR_IRQ: u64 = 1 << 1;
pub const SCR_FIQ: u64 = 1 << 2;
pub const HCR_FMO: u64 = 1 << 3;
pub const HCR_IMO: u64 = 1 << 4;
pub const HCR_TGE: u64 = 1 << 27;

/// `wfi`.
pub const INSN_WFI: u32 = 0xd503_207f;

/// The offset of the vector group an exception from `from_el` to `to_el`
/// uses: `+0x000` current EL with `SP_EL0`, `+0x200` current EL with `SP_ELx`,
/// `+0x400` lower EL using AArch64 (everything here is AArch64).
pub fn vector_group(from_el: u32, to_el: u32, spsel: bool) -> u64 {
    match (from_el == to_el, spsel) {
        (true, false) => 0x000,
        (true, true) => 0x200,
        (false, _) => 0x400,
    }
}

/// The EL an IRQ (or FIQ) is taken to from a core in `pstate`, or `None` if
/// it stays pending there (ARM ARM D1.13.4). `SCR_EL3.IRQ` sends it to EL3;
/// in non-secure state `HCR_EL2.IMO` or `TGE` send it to EL2; otherwise it
/// goes to EL1. Only an interrupt to the current EL is masked by `PSTATE.I`;
/// one to a higher EL is taken regardless, and one to a lower EL waits —
/// so at EL2 or EL3 an interrupt nobody routed there is never taken.
pub fn irq_target(scr: u64, hcr: u64, pstate: u64, fiq: bool) -> Option<u32> {
    let el = pstate_el(pstate);
    let (scr_bit, hcr_bit, mask) = if fiq {
        (SCR_FIQ, HCR_FMO, PSTATE_F)
    } else {
        (SCR_IRQ, HCR_IMO, PSTATE_I)
    };
    let to = if scr & scr_bit != 0 {
        3
    } else if el != 3 && scr & SCR_NS != 0 && hcr & (hcr_bit | HCR_TGE) != 0 {
        2
    } else {
        1
    };
    match to.cmp(&el) {
        std::cmp::Ordering::Less => None,
        std::cmp::Ordering::Equal => (pstate & mask == 0).then_some(to),
        std::cmp::Ordering::Greater => Some(to),
    }
}

/// A decoded `MRS`/`MSR` (register) instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SysregMove {
    pub read: bool,
    pub op0: u32,
    pub op1: u32,
    pub crn: u32,
    pub crm: u32,
    pub op2: u32,
    pub rt: u32,
}

impl SysregMove {
    /// `1101010100 L 1 o0 op1 CRn CRm op2 Rt` — the `op0` 2/3 system-register
    /// moves (ARM ARM C5.2).
    pub fn decode(insn: u32) -> Option<SysregMove> {
        if insn & 0xFFD0_0000 != 0xD510_0000 {
            return None;
        }
        Some(SysregMove {
            read: insn & (1 << 21) != 0,
            op0: 2 | ((insn >> 19) & 1),
            op1: (insn >> 16) & 7,
            crn: (insn >> 12) & 0xF,
            crm: (insn >> 8) & 0xF,
            op2: (insn >> 5) & 7,
            rt: insn & 0x1F,
        })
    }

    /// The encoding space the architecture reserves for IMPLEMENTATION
    /// DEFINED registers (ARM ARM D12.3.2: `op0 == 3`, `CRn` 11 or 15), where
    /// the A72 keeps `L2CTLR_EL1`, `CPUECTLR_EL1` and friends.
    pub fn is_impdef(&self) -> bool {
        self.op0 == 3 && (self.crn == 11 || self.crn == 15)
    }

    /// `S<op0>_<op1>_C<n>_C<m>_<op2>`, the assembler's generic name.
    pub fn name(&self) -> String {
        format!(
            "S{}_{}_C{}_C{}_{}",
            self.op0, self.op1, self.crn, self.crm, self.op2
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EL1H: u64 = 0b0101;
    const EL2H: u64 = 0b1001;
    const EL3H: u64 = 0b1101;

    #[test]
    fn linux_at_el1_takes_irqs_at_el1_unless_masked() {
        // After the armstub: SCR_EL3 = 0x5b1 (NS, no IRQ routing to EL3);
        // Linux at EL1 with HCR_EL2 routing off.
        let scr = 0x5b1;
        assert_eq!(irq_target(scr, 0, EL1H, false), Some(1));
        assert_eq!(irq_target(scr, 0, EL1H | PSTATE_I, false), None);
    }

    #[test]
    fn hcr_imo_sends_el1_irqs_to_el2_even_when_masked_at_el1() {
        let scr = SCR_NS;
        assert_eq!(irq_target(scr, HCR_IMO, EL1H | PSTATE_I, false), Some(2));
        // FIQ follows FMO, not IMO.
        assert_eq!(irq_target(scr, HCR_IMO, EL1H, true), Some(1));
        assert_eq!(irq_target(scr, HCR_FMO, EL1H | PSTATE_F, true), Some(2));
    }

    #[test]
    fn scr_irq_sends_everything_to_el3() {
        // Taken from EL1 even though EL1 has it masked: EL3 is higher.
        assert_eq!(irq_target(SCR_IRQ, 0, EL1H | PSTATE_I, false), Some(3));
        // At EL3 itself it is masked by PSTATE.I.
        assert_eq!(irq_target(SCR_IRQ, 0, EL3H | PSTATE_I, false), None);
    }

    #[test]
    fn with_no_routing_irqs_go_to_el1_and_wait_above_it() {
        // Linux's EL2 hyp stub never sees an IRQ it did not route to itself.
        assert_eq!(irq_target(SCR_NS, 0, EL2H, false), None);
        assert_eq!(irq_target(SCR_NS, HCR_IMO, EL2H, false), Some(2));
        // EL3 takes only what SCR_EL3 sends there.
        assert_eq!(irq_target(0, HCR_IMO, EL3H, false), None);
    }

    #[test]
    fn vector_groups() {
        assert_eq!(vector_group(1, 1, false), 0x000);
        assert_eq!(vector_group(1, 1, true), 0x200);
        assert_eq!(vector_group(1, 2, true), 0x400);
    }

    #[test]
    fn decodes_the_armstubs_impdef_moves() {
        // msr s3_1_c15_c2_1, x0 (CPUECTLR_EL1)
        let m = SysregMove::decode(0xd519_f220).unwrap();
        assert!(!m.read && m.is_impdef());
        assert_eq!(m.name(), "S3_1_C15_C2_1");
        assert_eq!(m.rt, 0);
        // mrs x0, s3_1_c11_c0_2 (L2CTLR_EL1)
        let m = SysregMove::decode(0xd539_b040).unwrap();
        assert!(m.read && m.is_impdef());
        assert_eq!(m.name(), "S3_1_C11_C0_2");
        // mrs x1, cntfrq_el0 — architectural, not IMPLEMENTATION DEFINED.
        let m = SysregMove::decode(0xd53b_e001).unwrap();
        assert!(m.read && !m.is_impdef());
        assert_eq!(
            (m.op0, m.op1, m.crn, m.crm, m.op2, m.rt),
            (3, 3, 14, 0, 0, 1)
        );
        // Not a system-register move.
        assert_eq!(SysregMove::decode(INSN_WFI), None);
    }
}
