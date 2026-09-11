//! VideoCore IV scalar VPU model.
//!
//! Scope today (milestone M1): a fetch/decode/execute interpreter for a subset
//! of the 16- and 32-bit scalar instruction forms — enough to run small
//! hand-written console payloads and simple control flow. The dual-issue
//! pipeline, the vector unit, MMU/cache, and interrupts are all future work.
//!
//! Instruction encoding is transcribed from the community reverse engineering
//! (Herman Hermitage's `videocoreiv.arch`, the vc4 binutils port). Anything not
//! confirmed against real firmware is flagged in-line.

pub mod decode;
pub mod exec;
pub mod fmath;
pub mod insn;
pub mod length;
pub mod reg;
pub mod vrf;

pub use exec::{Fault, HaltReason, Step, Stop, UnimplHit, UnimplPolicy, Vpu};
pub use reg::{Cond, Flags, Regs};
