//! VC4 VPU model — the BCM2711's boot processor, scalar side and vector unit.
//!
//! Scope today: a fetch/decode/execute interpreter for the scalar instruction
//! forms — 16-, 32- and 48-bit — plus exceptions and interrupt delivery, which
//! together run the boot ROM, the EEPROM bootloader and `start4.elf` through to
//! the ARM. The vector unit is decoded in full and executed for the forms that
//! `insn::VecInsn::executable` matches; the dual-issue pipeline and the
//! MMU/caches are not modelled. See `docs/vpu-isa.md`.
//!
//! Instruction encoding is transcribed from the community reverse engineering
//! (Herman Hermitage's `videocoreiv.arch`, the vc4 binutils port). Anything not
//! confirmed against real firmware is flagged in-line.

pub mod decode;
pub mod exec;
pub mod icache;
pub mod insn;
pub mod length;
pub mod reg;
pub mod vrf;

pub use exec::{Fault, HaltReason, Step, Stop, UnimplHit, UnimplPolicy, Vpu};
pub use reg::{Cond, Flags, Regs};
