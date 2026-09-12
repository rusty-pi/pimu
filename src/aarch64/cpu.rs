//! Architectural state of one Cortex-A72 core and the interface it executes
//! against.
//!
//! The core never owns memory. [`Cpu::step`] takes `&mut dyn Memory`, the same
//! shape as the VPU's [`Bus`](crate::bus::Bus), so the machine can hand both
//! processors the same RAM and peripherals. Addresses here are 64-bit: the
//! A72 sees the full 35-bit physical map of the BCM2711 and, once the MMU is
//! modelled, 48-bit virtual addresses.

use super::exec;

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
    /// A data access the bus refused, or an alignment fault.
    DataAbort {
        addr: u64,
        write: bool,
    },
    /// The instruction fetch itself failed.
    InsnAbort {
        addr: u64,
    },
    PcAlignment,
}

/// Result of one [`Cpu::step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Retired; `pc` is the next instruction.
    Retired,
    /// `wfi` retired; the core may sleep until an interrupt is pending.
    Wfi,
    /// `wfe` retired; the core may sleep until an event or interrupt.
    Wfe,
    /// The instruction raised a synchronous exception. `pc` is unchanged; the
    /// caller either takes it into the guest or services it itself (the user
    /// mode test harness handles `svc` as a Linux syscall).
    Exception(Exception),
    /// An encoding the architecture defines but this core does not implement
    /// yet. `pc` is unchanged. Kept apart from [`Exception::Undefined`] so a
    /// gap in the model never passes for guest behaviour.
    Unimplemented(u32),
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
    /// The local exclusive monitor: the address `ldxr` marked, if any.
    pub(super) exclusive: Option<u64>,
    /// Set by the executor for the instruction in flight: where to go next.
    pub(super) next_pc: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// A core out of reset in EL3 with everything zeroed. Reset values of the
    /// system registers are the machine's business, not this constructor's.
    pub fn new() -> Cpu {
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
            exclusive: None,
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

    /// Execute one instruction.
    pub fn step(&mut self, mem: &mut dyn Memory) -> Step {
        let pc = self.pc;
        if pc & 3 != 0 {
            return Step::Exception(Exception::PcAlignment);
        }
        let insn = match mem.fetch(pc) {
            Ok(i) => i,
            Err(a) => return Step::Exception(Exception::InsnAbort { addr: a.addr }),
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
}
