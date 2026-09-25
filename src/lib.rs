//! `pimu` — a virtual bench for Raspberry Pi VideoCore boot firmware: it runs
//! the real `pieeprom.bin` / `start4.elf` / `fixup4.dat` blobs in a modelled
//! BCM2711, on into Linux, and regression-tests the serial output. The command
//! line is a separate binary crate in `src/cli/`.
//!
//! ```text
//!   Emulator     the run loop                              emulator
//!   ├── Vpu      VC4 VPU, cores 0 and 1                    vpu/
//!   ├── ArmSide  the four Cortex-A72 cores                 arm/, aarch64/
//!   └── Machine  RAM + peripherals + address decode        machine
//!                ├── one model per peripheral block        periph/
//!                ├── the SoC and board around them         soc/
//!                └── the other end of the Ethernet cable   net/
//! ```

pub mod aarch64;
pub mod align;
pub mod arm;
pub mod armstub;
pub mod bus;
pub mod coherency;
pub mod diag;
pub mod emulator;
pub mod fat;
pub mod fdt;
pub mod firmware;
pub mod harness;
pub mod identity;
pub mod isa;
pub mod jitter;
pub mod l2;
pub mod log;
pub mod machine;
pub mod mem;
pub mod net;
pub mod periph;
pub mod sheet;
pub mod soc;
pub mod spec;
pub mod stdio;
pub mod vpu;

pub use emulator::{Emulator, RunEnd, RunLimits, RunReport};
pub use machine::{Console, Machine};
pub use vpu::Vpu;
