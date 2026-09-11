//! `rpi-virt-fw` — a virtual bench for Raspberry Pi VideoCore boot firmware.
//!
//! The goal is to execute the real `pieeprom.bin` / `start4.elf` / `fixup4.dat`
//! blobs in a modelled BCM2711 and regression-test their behaviour (primarily
//! serial output) against a known-good baseline.
//!
//! Milestone M1 (current): a VideoCore IV scalar interpreter, a small set of
//! peripherals (UARTs, system timer), and the regression harness. Proven with
//! hand-assembled payloads, not yet real firmware.
//!
//! ```text
//!   Emulator
//!   ├── Vpu        scalar core (fetch/decode/execute)   src/vpu/
//!   └── Machine    RAM + peripherals + address decode    src/machine.rs
//!                  ├── SysTimer                          src/periph/systimer.rs
//!                  ├── Pl011 / Aux (UART capture)        src/periph/
//!                  └── StubRegion (catch-all + log)      src/periph/stub.rs
//! ```

pub mod block;
pub mod bus;
pub mod diag;
pub mod emulator;
pub mod fdt;
pub mod firmware;
pub mod harness;
pub mod identity;
pub mod machine;
pub mod mem;
pub mod payloads;
pub mod periph;
pub mod soc;
pub mod vpu;

pub use emulator::{Emulator, RunEnd, RunLimits, RunReport};
pub use machine::{Console, Machine};
pub use vpu::Vpu;
