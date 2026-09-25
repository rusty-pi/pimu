//! VC4 VPU model — the BCM2711's boot processor, scalar side and vector unit.
//!
//! A fetch/decode/execute interpreter for the scalar forms plus exceptions and
//! interrupt delivery, which together run the boot ROM, the EEPROM bootloader
//! and `start4.elf` through to the ARM. The vector unit is decoded in full and
//! executed for the forms `insn::VecInsn::executable` matches; the dual-issue
//! pipeline and the MMU/caches are not modelled.
//!
//! Encoding, semantics and the evidence behind each: `isa/vpu.toml`, rendered
//! as `docs/vpu-isa.md`.

pub mod decode;
pub mod exec;
pub mod icache;
pub mod insn;
pub mod length;
pub mod reg;
pub mod vrf;

pub use exec::{Fault, HaltReason, Step, Stop, UnimplHit, UnimplPolicy, Vpu};
pub use reg::{Cond, Flags, Regs};
