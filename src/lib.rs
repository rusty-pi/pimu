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

//! # `std` and `no_std`
//!
//! With the default `std` feature this is an ordinary hosted library. With
//! `--no-default-features` it is `no_std` + `alloc`: the models, the VPU core
//! and the run loop compile for a bare-metal aarch64 target, which is what
//! lets the same crate be loaded as a QEMU `-kernel` (#32 stage 1). Three
//! things are hosted-only and stay behind the feature:
//!
//! * [`harness`] — scenario TOML, golden transcripts, everything that reads
//!   the filesystem. The core never opens a file; blobs arrive as `&[u8]`.
//! * [`diag::DiagConfig::from_env`] — the `RVF_*` switches. A bare-metal
//!   frontend builds a [`diag::DiagConfig`] itself.
//! * `std::time::Instant` behind [`time::Stopwatch`], and `eprintln!` behind
//!   [`diag_eprintln!`].

#![cfg_attr(not(feature = "std"), no_std)]

// `#[macro_use]` for `format!` and `vec!`: they are prelude macros under `std`
// and come from `alloc` without one, and every module in the crate uses them.
#[macro_use]
extern crate alloc;

pub mod block;
pub mod bus;
pub mod diag;
pub mod emulator;
pub mod error;
pub mod fdt;
pub mod firmware;
#[cfg(feature = "std")]
pub mod harness;
pub mod identity;
pub mod machine;
pub mod mem;
pub mod payloads;
pub mod periph;
pub mod soc;
pub mod time;
pub mod vpu;

pub use emulator::{Emulator, RunEnd, RunLimits, RunReport};
pub use error::{Error, Result};
pub use machine::{Console, Machine};
pub use vpu::Vpu;
