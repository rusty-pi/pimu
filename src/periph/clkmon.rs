//! BCM2711 VPU clock block at `0x7D5D_0000` — PLL control and clock-frequency
//! monitors, which `start4.elf`'s clock manager uses on a BCM2711.
//!
//! Registers and fields: `specs/clkmon.toml` ([`crate::spec::clkmon`]).
//!
//! This window sits *outside* the `0x7E…` legacy peripheral aperture, so
//! without this device its reads fold onto (unmapped) DRAM and every "is the
//! measurement ready" / "has the PLL locked" poll spins forever.
//!
//! The analogue PLLs and the frequency counters are not modelled: writes are
//! stored, with the `0x5A` password byte stripped on read-back.
//!
//! Two sub-blocks sit inside this window and are decoded ahead of it: the AVS
//! monitor at `0x7D5D_2000` ([`crate::periph::avs`]) and the per-channel PVT
//! monitors at `0x7D5D_8000` ([`crate::periph::pvt`]).
//!
//! ## What is left as plain storage here, and why
//!
//! Beyond those two sub-blocks, start4 touches only a handful of registers in
//! this window, all of them measured on a Raspberry Pi 4B d03115 and recorded
//! with their values in `specs/clkmon.toml`.
//!
//! * **`0x7D5D_1820` reads 0 on hardware too**, which is what this device
//!   returns: `FUN_0ec30196` takes bit 10 of it as a predicate and
//!   `FUN_0ec301a6` decodes bits `[23:11]` into three fields, so both yield
//!   zero on silicon as well. It is not a status register the model is
//!   silently blanking.
//! * **`0x7D5D_1814` reads `0x16`, and the model deliberately keeps 0.**
//!   `FUN_0ec2ffdc` is `(reg & 0x14) != 0`, and `0x16 & 0x14` is non-zero, so
//!   hardware answers "true" where the model answers "false". That predicate
//!   is the condition of a loop in `FUN_0ec303e8` that raises the voltage a
//!   step at a time and re-tests it — nothing inside the loop writes
//!   `0x1814`, so it terminates only because real silicon settles, and a
//!   constant "true" would hang there. Returning 0 means "settled", the right
//!   steady state for a model with no analogue ramp. The loop is unreachable
//!   in any case: it sits behind a `FUN_0ec30196() != 0` guard, and that
//!   register genuinely reads 0.
//! * `0x7D5D_1800` (`0x04`) and `0x7D5D_183C` (`0x01`) are both written by the
//!   firmware before they are read (`FUN_0ec2ffb4` sets bit 2 of the former,
//!   `FUN_0ec302e2` writes the latter), so plain storage is correct and the
//!   measured values are post-start4 state rather than reset state anyway.
//! * `0x7D5D_2074` / `0x7D5D_2078` / `0x7D5D_206C` / `0x7D5D_A000` are only
//!   ever written, never read back. Storage is all they need.
//!
//! ## The PLLs
//!
//! Leaving the PLLs unmodelled would matter if the firmware measured
//! frequencies through them. It does not: a full boot to `arm_loader`, traced
//! over this whole window, touches no PLL divider, multiplier or lock-status
//! register outside the two sub-blocks — not one this device answers wrongly,
//! and not one it is asked about at all. The live inputs to start4's frequency
//! and voltage measurements are the AVS channels at `0x7D5D_2200`, and the
//! clock-rate calibration reads a cached rate table rather than taking a live
//! measurement. So no PLL model is invented to fill a gap the firmware never
//! probes.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::clkmon::{CHAR_DONE, REG_1800, REG_1814, REG_1820, REG_A000};
use crate::spec::Coverage;

/// All plain storage, which is what the traced boot and the measurements call
/// for; see the module docs.
pub const COVERAGE: Coverage = Coverage {
    block: "clkmon",
    decoded: &[REG_1800, REG_1814, REG_1820, CHAR_DONE, REG_A000],
};

const PASSWD: u32 = 0x5A00_0000;

#[derive(Default)]
pub struct ClkMon {
    storage: BTreeMap<u32, u32>,
}

impl ClkMon {
    pub fn new() -> ClkMon {
        ClkMon::default()
    }
}

impl MmioDevice for ClkMon {
    fn name(&self) -> &'static str {
        "clk-monitor"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        Ok(self.storage.get(&off).copied().unwrap_or(0) & !PASSWD)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset & !3, value);
        Ok(())
    }
}
