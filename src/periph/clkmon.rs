//! BCM2711 VPU clock block at `0x7D5D_0000` — PLL control + clock-frequency
//! monitors, used by `start4.elf`'s clock manager when the SoC-type switch at
//! `0x3EC635F0` sets `[gp+5476] = 1` (which it does for the Pi 4 / BCM2711).
//!
//! This window sits *outside* the `0x7E…` legacy peripheral aperture, so
//! without this device its reads fold onto (unmapped) DRAM and every "is the
//! measurement ready" / "has the PLL locked" poll spins forever.
//!
//! We do not model the analogue PLLs or the frequency counters — we store
//! writes (stripping the `0x5A` password byte for read-back).
//!
//! Two sub-blocks sit inside this window and are decoded ahead of it: the AVS
//! monitor at `0x7D5D_2000` ([`crate::periph::avs`]) and the per-channel PVT
//! monitors at `0x7D5D_8000` ([`crate::periph::pvt`]).
//!
//! ## What is left as plain storage here, and why
//!
//! Beyond those two sub-blocks, start4 touches only a handful of registers in
//! this window. All of them were read off a Raspberry Pi 4B d03115 through
//! `/dev/mem` at the `0xFD5D…` alias, on a Linux-booted, idle Pi 4:
//!
//! ```text
//! 0x7d5d1800  0x00000004     0x7d5d1818  0x00000000
//! 0x7d5d1814  0x00000016     0x7d5d1820  0x00000000
//! 0x7d5d183c  0x00000001
//! ```
//!
//! * **`0x7D5D_1820` reads 0 on real hardware**, which is what this device
//!   already returns. `FUN_0ec30196` takes bit 10 of it as a predicate and
//!   `FUN_0ec301a6` decodes bits `[23:11]` into three fields; both therefore
//!   yield zero on hardware too. This one looked like the `SCALER_DISPID` trap
//!   — a status register the model was blanking — and measurement says it is
//!   not. Worth recording as a negative result so it is not re-investigated.
//! * **`0x7D5D_1814` reads `0x16`, and the model deliberately keeps 0.**
//!   `FUN_0ec2ffdc` is `(reg & 0x14) != 0`, and `0x16 & 0x14` is non-zero, so
//!   hardware would answer "true" where the model answers "false". That
//!   predicate is the condition of a loop in `FUN_0ec303e8` that raises the
//!   voltage a step at a time and re-tests it — nothing inside the loop writes
//!   `0x1814`, so it terminates only because real silicon settles. A model that
//!   answered a constant "true" would hang there. Returning 0 means "settled",
//!   which is the right steady state for a model with no analogue ramp. The
//!   loop is in any case unreachable today: it sits behind a `FUN_0ec30196()
//!   != 0` guard, and that register genuinely reads 0. Recorded here so the
//!   `0x16` is not mistaken for a missing value later.
//! * `0x7D5D_1800` (`0x04`) and `0x7D5D_183C` (`0x01`) are both written by the
//!   firmware before they are read (`FUN_0ec2ffb4` sets bit 2 of the former,
//!   `FUN_0ec302e2` writes the latter), so plain storage is correct and the
//!   measured values are post-start4 state rather than reset state anyway.
//! * `0x7D5D_2074` / `0x7D5D_2078` / `0x7D5D_206C` / `0x7D5D_A000` are only
//!   ever written, never read back. Storage is all they need.
//!
//! ## The PLLs
//!
//! Point 4 of issue #1 asks for the PLLs themselves, on the grounds that
//! leaving them unmodelled makes every frequency measurement return one fixed
//! value. A full boot to `arm_loader` was traced with every access to this
//! window logged (`RVF_TRACE_MMIO=0x7d5d0000-0x7d5e0000`), and the complete
//! list of offsets the firmware touches outside the two sub-blocks is:
//!
//! ```text
//! 0x7d5d1800  R+W   FUN_0ec2ffb4 sets bit 2 from the measured core voltage
//! 0x7d5d1820  R     FUN_0ec30196's predicate; reads 0 on hardware too
//! 0x7d5d183c  R+W   start4's own "characterisation done" flag
//! 0x7d5da000  W     FUN_0ec30200 writes 0xA0, never reads it back
//! ```
//!
//! Plus, in the AVS page and covered by [`crate::periph::avs`], the write-only
//! `+0x203C` / `+0x2040` / `+0x2044` measurement masks and the `+0x206C` /
//! `+0x2074` / `+0x2078` enables.
//!
//! There is no PLL divider, multiplier or lock-status register among them —
//! not one this device answers wrongly, and not one it is asked about at all.
//! The live inputs to start4's frequency and voltage measurements are the AVS
//! channels at `0x7D5D_2200`, and the clock-rate calibration reads a cached
//! rate table rather than taking a live measurement. So there is nothing here
//! to model, and no PLL model is invented to fill a gap the firmware never
//! probes. If a later boot phase does read one of these registers, the trace
//! above is the way to find out.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::clkmon::{CHAR_DONE, REG_1800, REG_1814, REG_1820, REG_A000};
use crate::spec::Coverage;

/// All plain storage, which is what the traced boot and the measurements call
/// for (module docs).
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
