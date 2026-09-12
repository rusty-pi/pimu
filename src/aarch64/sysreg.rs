//! System registers (#40, milestone 2): what the armstub and an early Linux
//! boot read and write, from EL3 down to EL0.
//!
//! Registers fall into four kinds:
//!
//! 1. The ones the core itself acts on — `SCTLR`, `VBAR`, `ELR`, `SPSR`,
//!    `ESR`, `FAR`, `SCR_EL3`, `HCR_EL2`, the stack pointers, `PSTATE` fields —
//!    kept as fields of [`SysRegs`] or of the [`Cpu`].
//! 2. Identification registers, constant. `MIDR_EL1` / `REVIDR_EL1` are read
//!    off the reference board (`/sys/devices/system/cpu/cpu0/regs/
//!    identification/` on `rpi-dev`: `0x410fd083`, `0`). The rest are the
//!    Cortex-A72 r0p3 TRM's reset values, with the Cryptographic Extension
//!    fields cleared: BCM2711 does not implement it (`/proc/cpuinfo` there
//!    lists `fp asimd evtstrm crc32 cpuid`).
//! 3. Plain storage: registers that only matter to whoever reads them back
//!    (translation-table and cache configuration until the MMU is modelled,
//!    debug and PMU registers, IMPLEMENTATION DEFINED ones like
//!    `L2CTLR_EL1`), kept in a map but only for encodings on an allowlist.
//! 4. The generic timer, which belongs to the machine rather than the core:
//!    forwarded to [`Memory::sysreg_read`] / [`Memory::sysreg_write`].
//!
//! Anything else is [`Stop::Unimplemented`] at EL1 and above (UNDEFINED at
//! EL0), so a register the model does not know about stops the run instead
//! of silently reading zero.
//!
//! Not modelled yet: the traps (`HCR_EL2.TVM` & co, `CPACR_EL1.FPEN`,
//! `CPTR_ELx.TFP`, `MDCR_ELx`), which a kernel that sets them up the usual
//! way never trips.

use std::collections::BTreeMap;

use super::cpu::{Cpu, Memory};
use super::exec::{Stop, UNDEF};

/// A system register's encoding, `op0:op1:CRn:CRm:op2` as in bits 20..5 of
/// `MRS`/`MSR`.
pub const fn key(op0: u32, op1: u32, crn: u32, crm: u32, op2: u32) -> u32 {
    (op0 << 14) | (op1 << 11) | (crn << 7) | (crm << 3) | op2
}

/// `MIDR_EL1`: ARM, variant 0, Cortex-A72 (`0xD08`), revision 3 — measured.
pub const MIDR: u64 = 0x410F_D083;

/// `SCR_EL3` bits the core consults.
pub const SCR_NS: u64 = 1 << 0;
pub const SCR_SMD: u64 = 1 << 7;
pub const SCR_HCE: u64 = 1 << 8;
/// `HCR_EL2` bits the core consults.
pub const HCR_TSC: u64 = 1 << 19;
pub const HCR_TGE: u64 = 1 << 27;
/// `SCTLR_ELx.M`: stage 1 translation on.
pub const SCTLR_M: u64 = 1 << 0;

/// The registers the core acts on. Arrays are indexed by exception level;
/// slot 0 is unused where the register has no EL0 instance.
#[derive(Clone)]
pub struct SysRegs {
    pub sctlr: [u64; 4],
    pub tcr: [u64; 4],
    pub ttbr0: [u64; 4],
    pub ttbr1_el1: u64,
    pub mair: [u64; 4],
    pub vbar: [u64; 4],
    pub elr: [u64; 4],
    pub spsr: [u64; 4],
    pub esr: [u64; 4],
    pub far: [u64; 4],
    pub scr_el3: u64,
    pub hcr_el2: u64,
    pub vpidr_el2: u64,
    pub vmpidr_el2: u64,
    pub mpidr: u64,
    pub cntfrq: u64,
    pub csselr: u64,
    /// Kind 3 in the module docs.
    pub plain: BTreeMap<u32, u64>,
}

impl SysRegs {
    /// Reset state of core `cpu`. `SCTLR_ELx` get their RES1 bits (A72 TRM
    /// reset values), everything the reset leaves UNKNOWN is 0, except that
    /// `VPIDR_EL2` / `VMPIDR_EL2` start as the real IDs so EL1 sees them even
    /// if EL2 never sets them.
    pub fn new(cpu: u32) -> SysRegs {
        let mpidr = 0x8000_0000 | u64::from(cpu);
        SysRegs {
            sctlr: [0, 0x30D0_0800, 0x30C5_0830, 0x30C5_0830],
            tcr: [0; 4],
            ttbr0: [0; 4],
            ttbr1_el1: 0,
            mair: [0; 4],
            vbar: [0; 4],
            elr: [0; 4],
            spsr: [0; 4],
            esr: [0; 4],
            far: [0; 4],
            scr_el3: 0,
            hcr_el2: 0,
            vpidr_el2: MIDR,
            vmpidr_el2: mpidr,
            mpidr,
            cntfrq: 0,
            csselr: 0,
            plain: BTreeMap::new(),
        }
    }
}

/// `ID_AA64*`, the AArch32 `ID_*` and `MVFR*` registers: `op0 = 3, op1 = 0,
/// CRn = 0, CRm = 1..7`. Unallocated slots in that space read as zero.
fn id_reg(crm: u32, op2: u32) -> u64 {
    match (crm, op2) {
        // AArch32 feature registers (EL0 AArch32 is supported by the A72).
        (1, 0) => 0x0000_0131, // ID_PFR0_EL1
        (1, 1) => 0x0001_1011, // ID_PFR1_EL1
        (1, 2) => 0x0301_0066, // ID_DFR0_EL1
        (1, 4) => 0x1020_1105, // ID_MMFR0_EL1
        (1, 5) => 0x4000_0000, // ID_MMFR1_EL1
        (1, 6) => 0x0126_0000, // ID_MMFR2_EL1
        (1, 7) => 0x0210_2211, // ID_MMFR3_EL1
        (2, 0) => 0x0210_1110, // ID_ISAR0_EL1
        (2, 1) => 0x1311_2111, // ID_ISAR1_EL1
        (2, 2) => 0x2123_2042, // ID_ISAR2_EL1
        (2, 3) => 0x0111_2131, // ID_ISAR3_EL1
        (2, 4) => 0x0001_1142, // ID_ISAR4_EL1
        (2, 5) => 0x0001_0001, // ID_ISAR5_EL1: SEVL, CRC32; no AES/SHA
        (3, 0) => 0x1011_0222, // MVFR0_EL1
        (3, 1) => 0x1211_1111, // MVFR1_EL1
        (3, 2) => 0x0000_0043, // MVFR2_EL1
        (4, 0) => 0x0000_2222, // ID_AA64PFR0_EL1: EL0-3 AArch64+32, FP, AdvSIMD
        (5, 0) => 0x1030_5106, // ID_AA64DFR0_EL1
        (6, 0) => 0x0001_0000, // ID_AA64ISAR0_EL1: CRC32 only
        (7, 0) => 0x0000_1124, // ID_AA64MMFR0_EL1: 44-bit PA, 16-bit ASID, 4K/64K
        _ => 0,
    }
}

/// `CCSIDR_EL1` for the cache `CSSELR_EL1` selects: 32 KiB 2-way L1 D,
/// 48 KiB 3-way L1 I, 1 MiB 16-way L2, 64-byte lines (A72 TRM, and the 1 MiB
/// L2 BCM2711 has).
fn ccsidr(csselr: u64) -> u64 {
    match csselr & 0xF {
        0 => 0x701F_E00A,
        1 => 0x201F_E012,
        2 => 0x707F_E07A,
        _ => 0,
    }
}

/// The lowest EL that may access a register, from its `op0`/`op1` (ARM ARM
/// D12.2), or `None` for the `op1 = 5` space (the v8.1 `_EL12` aliases).
fn min_el(op0: u32, op1: u32) -> Option<u32> {
    match (op0, op1) {
        (2, 3) | (3, 3) => Some(0),
        (_, 0..=2) | (_, 7) => Some(1),
        (_, 4) => Some(2),
        (_, 6) => Some(3),
        _ => None,
    }
}

/// Registers kept as plain storage (module docs, kind 3).
fn is_plain(k: u32) -> bool {
    let (op0, op1, crn, crm, op2) = (k >> 14, (k >> 11) & 7, (k >> 7) & 15, (k >> 3) & 15, k & 7);
    if op0 == 2 {
        // Debug: MDSCR, OSLAR/OSLSR/OSDLR, breakpoint and watchpoint
        // registers, claim tags.
        return true;
    }
    if crn == 11 || crn == 15 {
        // IMPLEMENTATION DEFINED (L2CTLR_EL1, CPUECTLR_EL1, ...).
        return true;
    }
    matches!(
        (op1, crn, crm, op2),
        (0 | 4 | 6, 1, 0, 1)            // ACTLR_ELx
            | (0, 1, 0, 2)              // CPACR_EL1
            | (0 | 4 | 6, 5, 1, 0 | 1)  // AFSR0/1_ELx
            | (0 | 4 | 6, 10, 3, 0)     // AMAIR_ELx
            | (0, 7, 4, 0)              // PAR_EL1
            | (0, 13, 0, 1)             // CONTEXTIDR_EL1
            | (0, 13, 0, 4)             // TPIDR_EL1
            | (4 | 6, 13, 0, 2)         // TPIDR_EL2/3
            | (0, 14, 1, 0)             // CNTKCTL_EL1
            | (4, 14, 1, 0)             // CNTHCTL_EL2
            | (4, 1, 1, 1 | 2 | 3 | 7)  // MDCR_EL2, CPTR_EL2, HSTR_EL2, HACR_EL2
            | (4, 2, 1, 0 | 2)          // VTTBR_EL2, VTCR_EL2
            | (4, 3, 0, 0)              // DACR32_EL2
            | (4, 4, 3, _)              // SPSR_irq/abt/und/fiq
            | (4, 5, 0, 1)              // IFSR32_EL2
            | (4, 5, 3, 0)              // FPEXC32_EL2
            | (4, 6, 0, 4)              // HPFAR_EL2
            | (6, 1, 1, 1 | 2)          // SDER32_EL3, CPTR_EL3
            | (6, 1, 3, 1)              // MDCR_EL3
            | (6, 12, 0, 2)             // RMR_EL3
            | (3, 9, 12..=14, _)        // PMU, EL0 view
            | (0, 9, 14, 1 | 2) // PMINTENSET/CLR_EL1
    )
}

/// Reset values for plain-storage registers that are not zero.
fn plain_reset(k: u32) -> u64 {
    match k {
        // PMCR_EL0: IMP = ARM, IDCODE = 3 (A72), N = 6 counters.
        k if k == key(3, 3, 9, 12, 0) => 0x4103_3000,
        _ => 0,
    }
}

/// `MRS`.
pub(super) fn read(cpu: &mut Cpu, k: u32, mem: &mut dyn Memory) -> Result<u64, Stop> {
    let (op0, op1, crn, crm, op2) = (k >> 14, (k >> 11) & 7, (k >> 7) & 15, (k >> 3) & 15, k & 7);
    let Some(min) = min_el(op0, op1) else {
        return Err(UNDEF);
    };
    if cpu.el < min {
        return Err(UNDEF);
    }
    let el0_ok = |ok: bool| if ok { Ok(()) } else { Err(UNDEF) };
    let s = &cpu.sys;
    Ok(match k {
        k if k == key(3, 3, 4, 2, 0) => cpu.nzcv as u64,
        k if k == key(3, 3, 4, 2, 1) => cpu.daif as u64,
        k if k == key(3, 3, 4, 4, 0) => cpu.fpcr as u64,
        k if k == key(3, 3, 4, 4, 1) => cpu.fpsr as u64,
        k if k == key(3, 3, 13, 0, 2) => cpu.tpidr_el0,
        k if k == key(3, 3, 13, 0, 3) => cpu.tpidrro_el0,
        k if k == key(3, 3, 0, 0, 7) => 4, // DCZID_EL0: 64-byte DC ZVA
        k if k == key(3, 3, 0, 0, 1) => 0x8444_C004, // CTR_EL0
        k if k == key(3, 3, 14, 0, 0) => s.cntfrq,
        _ if cpu.el == 0 => {
            // Beyond these, EL0 reaches only the counter and timers.
            el0_ok(op0 == 3 && op1 == 3 && crn == 14)?;
            return mem.sysreg_read(k).ok_or(UNDEF);
        }
        k if k == key(3, 0, 4, 2, 2) => (cpu.el as u64) << 2, // CurrentEL
        k if k == key(3, 0, 4, 2, 0) => cpu.spsel as u64,
        k if k == key(3, 0, 4, 1, 0) => cpu.sp_el[0],
        k if k == key(3, 4, 4, 1, 0) => cpu.sp_el[1],
        k if k == key(3, 6, 4, 1, 0) => cpu.sp_el[2],
        // EL1 sees the virtualised IDs EL2 chose; EL2 and EL3 the real ones.
        k if k == key(3, 0, 0, 0, 0) => {
            if cpu.el == 1 {
                s.vpidr_el2
            } else {
                MIDR
            }
        }
        k if k == key(3, 0, 0, 0, 5) => {
            if cpu.el == 1 {
                s.vmpidr_el2
            } else {
                s.mpidr
            }
        }
        k if k == key(3, 0, 0, 0, 6) => 0, // REVIDR_EL1, measured
        _ if op0 == 3 && op1 == 0 && crn == 0 && (1..=7).contains(&crm) => id_reg(crm, op2),
        k if k == key(3, 1, 0, 0, 0) => ccsidr(s.csselr),
        k if k == key(3, 1, 0, 0, 1) => 0x0A20_0023, // CLIDR_EL1: L1 I+D, L2
        k if k == key(3, 1, 0, 0, 7) => 0,           // AIDR_EL1
        k if k == key(3, 2, 0, 0, 0) => s.csselr,
        k if k == key(3, 0, 12, 1, 0) => {
            // ISR_EL1: the pending IRQ and FIQ lines.
            ((cpu.irq_line as u64) << 7) | ((cpu.fiq_line as u64) << 6)
        }
        k if k == key(3, 6, 12, 0, 1) => 0, // RVBAR_EL3: reset at 0
        _ => match banked(k) {
            Some((Banked::Sctlr, el)) => s.sctlr[el],
            Some((Banked::Tcr, el)) => s.tcr[el],
            Some((Banked::Ttbr0, el)) => s.ttbr0[el],
            Some((Banked::Ttbr1, _)) => s.ttbr1_el1,
            Some((Banked::Mair, el)) => s.mair[el],
            Some((Banked::Vbar, el)) => s.vbar[el],
            Some((Banked::Elr, el)) => s.elr[el],
            Some((Banked::Spsr, el)) => s.spsr[el],
            Some((Banked::Esr, el)) => s.esr[el],
            Some((Banked::Far, el)) => s.far[el],
            Some((Banked::Scr, _)) => s.scr_el3,
            Some((Banked::Hcr, _)) => s.hcr_el2,
            Some((Banked::Vpidr, _)) => s.vpidr_el2,
            Some((Banked::Vmpidr, _)) => s.vmpidr_el2,
            None if is_plain(k) => s.plain.get(&k).copied().unwrap_or_else(|| plain_reset(k)),
            None => return mem.sysreg_read(k).ok_or(Stop::Unimplemented),
        },
    })
}

/// `MSR` (register).
pub(super) fn write(cpu: &mut Cpu, k: u32, v: u64, mem: &mut dyn Memory) -> Result<(), Stop> {
    let (op0, op1, crn) = (k >> 14, (k >> 11) & 7, (k >> 7) & 15);
    let Some(min) = min_el(op0, op1) else {
        return Err(UNDEF);
    };
    if cpu.el < min {
        return Err(UNDEF);
    }
    match k {
        k if k == key(3, 3, 4, 2, 0) => cpu.nzcv = v as u32 & 0xF000_0000,
        k if k == key(3, 3, 4, 2, 1) => cpu.daif = v as u32 & (0xF << 6),
        // FPCR: AHP, DN, FZ, RMode, Stride and Len; bit 19 (FZ16 from v8.2)
        // is RES0, and the A72 implements no trapped exceptions, so the IxE
        // enables read as zero.
        k if k == key(3, 3, 4, 4, 0) => cpu.fpcr = v as u32 & 0x07F7_0000,
        k if k == key(3, 3, 4, 4, 1) => cpu.fpsr = v as u32 & 0xF800_009F,
        k if k == key(3, 3, 13, 0, 2) => cpu.tpidr_el0 = v,
        _ if cpu.el == 0 => {
            if op0 == 3 && op1 == 3 && crn == 14 && mem.sysreg_write(k, v) {
                return Ok(());
            }
            return Err(UNDEF);
        }
        k if k == key(3, 3, 13, 0, 3) => cpu.tpidrro_el0 = v,
        k if k == key(3, 3, 14, 0, 0) => cpu.sys.cntfrq = v & 0xFFFF_FFFF,
        k if k == key(3, 0, 4, 2, 0) => cpu.spsel = v & 1 != 0,
        k if k == key(3, 0, 4, 1, 0) => cpu.sp_el[0] = v,
        k if k == key(3, 4, 4, 1, 0) => cpu.sp_el[1] = v,
        k if k == key(3, 6, 4, 1, 0) => cpu.sp_el[2] = v,
        k if k == key(3, 2, 0, 0, 0) => cpu.sys.csselr = v & 0xF,
        _ => {
            let s = &mut cpu.sys;
            match banked(k) {
                Some((Banked::Sctlr, el)) => s.sctlr[el] = v,
                Some((Banked::Tcr, el)) => s.tcr[el] = v,
                Some((Banked::Ttbr0, el)) => s.ttbr0[el] = v,
                Some((Banked::Ttbr1, _)) => s.ttbr1_el1 = v,
                Some((Banked::Mair, el)) => s.mair[el] = v,
                Some((Banked::Vbar, el)) => s.vbar[el] = v & !0x7FF,
                Some((Banked::Elr, el)) => s.elr[el] = v,
                Some((Banked::Spsr, el)) => s.spsr[el] = v & 0xF03F_03DF,
                Some((Banked::Esr, el)) => s.esr[el] = v & 0xFFFF_FFFF,
                Some((Banked::Far, el)) => s.far[el] = v,
                Some((Banked::Scr, _)) => s.scr_el3 = v,
                Some((Banked::Hcr, _)) => s.hcr_el2 = v,
                Some((Banked::Vpidr, _)) => s.vpidr_el2 = v & 0xFFFF_FFFF,
                Some((Banked::Vmpidr, _)) => s.vmpidr_el2 = v,
                None if is_plain(k) => {
                    s.plain.insert(k, v);
                }
                None => {
                    if !mem.sysreg_write(k, v) {
                        return Err(Stop::Unimplemented);
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Banked {
    Sctlr,
    Tcr,
    Ttbr0,
    Ttbr1,
    Mair,
    Vbar,
    Elr,
    Spsr,
    Esr,
    Far,
    Scr,
    Hcr,
    Vpidr,
    Vmpidr,
}

/// The kind-1 registers, and the EL whose instance an encoding names.
fn banked(k: u32) -> Option<(Banked, usize)> {
    let (op0, op1, crn, crm, op2) = (k >> 14, (k >> 11) & 7, (k >> 7) & 15, (k >> 3) & 15, k & 7);
    if op0 != 3 {
        return None;
    }
    let el = match op1 {
        0 => 1,
        4 => 2,
        6 => 3,
        _ => return None,
    };
    let b = match (crn, crm, op2) {
        (1, 0, 0) => Banked::Sctlr,
        (2, 0, 0) => Banked::Ttbr0,
        (2, 0, 1) if el == 1 => Banked::Ttbr1,
        (2, 0, 2) => Banked::Tcr,
        (10, 2, 0) => Banked::Mair,
        (12, 0, 0) => Banked::Vbar,
        (4, 0, 0) => Banked::Spsr,
        (4, 0, 1) => Banked::Elr,
        (5, 2, 0) => Banked::Esr,
        (6, 0, 0) => Banked::Far,
        (1, 1, 0) if el == 3 => Banked::Scr,
        (1, 1, 0) if el == 2 => Banked::Hcr,
        (0, 0, 0) if el == 2 => Banked::Vpidr,
        (0, 0, 5) if el == 2 => Banked::Vmpidr,
        _ => return None,
    };
    Some((b, el))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_the_assembler() {
        // mrs x0, midr_el1 = 0xd5380000; the key is bits 20..5.
        assert_eq!(key(3, 0, 0, 0, 0), (0xd538_0000 >> 5) & 0xFFFF);
        // msr scr_el3, x0 = 0xd51e1100
        assert_eq!(key(3, 6, 1, 1, 0), (0xd51e_1100 >> 5) & 0xFFFF);
    }

    #[test]
    fn no_crypto_in_the_id_registers() {
        // ID_AA64ISAR0_EL1: AES [7:4], SHA1 [11:8], SHA2 [15:12] all zero.
        assert_eq!(id_reg(6, 0) & 0xFFF0, 0);
        assert_eq!(id_reg(6, 0) >> 16 & 0xF, 1, "CRC32");
    }
}
