//! `rpi-virt-fw` — a virtual bench for Raspberry Pi VideoCore boot firmware.
//!
//! It executes the real `pieeprom.bin` / `start4.elf` / `fixup4.dat` blobs in
//! a modelled BCM2711, on into Linux, and regression-tests their behaviour
//! (primarily serial output) against a known-good baseline.
//!
//! ```text
//!   Emulator     the run loop                              emulator
//!   ├── Vpu      VideoCore IV scalar core, cores 0 and 1   vpu/
//!   ├── ArmSide  the four Cortex-A72 cores                 arm/, aarch64/
//!   └── Machine  RAM + peripherals + address decode        machine
//!                ├── one model per peripheral block        periph/
//!                ├── the SoC and board around them         soc/
//!                └── the other end of the Ethernet cable   net/
//! ```
//!
//! Around those:
//!
//! - [`bus`], [`mem`] and [`l2`]: what the cores load and store through.
//! - [`firmware`]: loading firmware images, and the boot ROM stage.
//! - [`armstub`], [`fdt`] and [`identity`]: what `arm_loader` hands the ARM,
//!   and the board identity (`rpi-machine-id`) in it.
//! - [`harness`]: scenario files in, pass/fail and a transcript out.
//! - [`log`], [`diag`] and [`stdio`]: the log channels (`--log`), the `RVF_*`
//!   diagnostics, and the host terminal as the serial console.
//! - [`spec`]: register maps generated from `specs/*.toml`.
//!
//! The command line is a separate binary crate, in `src/cli/`.

pub mod aarch64;
pub mod arm;
pub mod armstub;
pub mod bus;
pub mod diag;
pub mod emulator;
pub mod fdt;
pub mod firmware;
pub mod harness;
pub mod identity;
pub mod l2;
pub mod log;
pub mod machine;
pub mod mem;
pub mod net;
pub mod periph;
pub mod soc;
pub mod spec;
pub mod stdio;
pub mod vpu;

pub use emulator::{Emulator, RunEnd, RunLimits, RunReport};
pub use machine::{Console, Machine};
pub use vpu::Vpu;
