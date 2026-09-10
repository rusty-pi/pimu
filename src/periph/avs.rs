//! BCM2711 AVS monitor at `0x7D5D_2000` — the on-die temperature sensor and the
//! ring-oscillator / voltage monitors that start4's DVFS code reads.
//!
//! The block is a real device tree node on the Pi 4:
//!
//! ```text
//! avs-monitor@7d5d2000 {
//!     compatible = "brcm,bcm2711-avs-monitor", "syscon", "simple-mfd";
//!     reg = <0x7d5d2000 0xf00>;
//!     thermal { compatible = "brcm,bcm2711-thermal"; };
//! };
//! ```
//!
//! Linux's `bcm2711_thermal` only touches `+0x200`: bit 10 = valid,
//! bits `[9:0]` = count, and `temp_mC = 410040 - 487 * count`. start4 uses the
//! same formula — `FUN_0ecc6488` multiplies by `-498739` and shifts right by 10
//! (`-498739 / 1024 = -487.05`) before adding `410040` — which is how we know
//! `+0x200` is the temperature channel here too.
//!
//! ## What start4 reads
//!
//! * `+0x03C` selects a channel: `~(1 << ch) & 0x7F`. `+0x040` / `+0x044` are
//!   enable masks, written all-ones for the duration of a measurement
//!   (`FUN_0ed6040e`).
//! * `+0x200 + ch*4` (ch 0..5) is that channel's result. `FUN_0ed603e2` spins
//!   until **both** bit 10 (valid) and bit 16 (settled) are set, then takes
//!   `[9:0]`; `FUN_0ed6040e` averages up to 150 such samples.
//! * `+0x220 + ch*4` (ch 0..0x17) are the per-rail monitors `FUN_0ec3007a`
//!   polls for bit 16, taking `[14:0]`. `FUN_0ec303e8` skips every channel that
//!   reads back 0 and returns 0 if they all do — which makes the whole DVFS
//!   voltage calculation (`FUN_0ec31bb2`) bail out with 0.
//! * `+0xD00 + ch*4` / `+0xE00 + ch*4` are plain read/write storage
//!   (`FUN_0ec302c2` writes them, `FUN_0ec300a4` reads them back).
//!
//! ## Where the counts come from
//!
//! Measured on the reference Pi 4 (`rpi-dev`) while idle:
//!
//! ```text
//! $ cat /sys/class/thermal/thermal_zone0/temp   ->  43816   (43.816 degC)
//! $ vcgencmd measure_volts core                 ->  0.9260V
//! ```
//!
//! Inverting the firmware's own conversions gives the raw counts:
//!
//! * channel 0 (temperature): `(410040 - 43816) / 487 = 752`.
//! * channel 3 (core voltage): start4 reports `(count * 100571) >> 13` in
//!   tenths of a millivolt, so `926.0 mV` is `754` counts. One count is
//!   12.3 mV, so 754 reads back as 925.6 mV — the sensor's own granularity, not
//!   a modelling error.
//!
//! Channels 1, 2, 4 and 5 share channel 3's monitor and have no Linux-visible
//! ground truth, so they carry the same count.
//!
//! The `+0x220` rail monitors have no ground truth at all, but their magnitude
//! cancels: start4 reads each rail twice and passes both to `FUN_0ec72aae`,
//! which returns its baseline unchanged whenever the two samples differ by at
//! most 1. A stable count therefore means "no adaptive correction", which is
//! the right answer for a model that does not simulate silicon speed. What
//! matters is only that they are non-zero and valid.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

pub const BASE: u32 = 0x7D5D_2000;
/// `reg = <0x7d5d2000 0xf00>` in the Pi 4 device tree.
pub const SIZE: u32 = 0x0000_0F00;

/// Channel results `FUN_0ed603e2` polls, `+0x200 + ch*4` for ch 0..5.
const RESULT_BASE: u32 = 0x200;
const RESULT_CHANNELS: u32 = 6;
/// Per-rail monitors `FUN_0ec3007a` polls, `+0x220 + ch*4` for ch 0..0x17.
const RAIL_BASE: u32 = 0x220;
const RAIL_CHANNELS: u32 = 0x18;

/// Bit 10: the reading is valid (Linux calls this `BCM2711_TS_VALID_MASK`).
const VALID: u32 = 1 << 10;
/// Bit 16: the reading has settled. Linux ignores it; start4 will not accept a
/// sample without it, which is why a zero-returning stub stalled the boot.
const SETTLED: u32 = 1 << 16;

/// Temperature count, from `rpi-dev`'s 43.816 degC: `(410040 - 43816) / 487`.
const TEMP_COUNT: u32 = 752;
/// Ring-oscillator count, from `rpi-dev`'s core voltage of 0.9260 V:
/// `926.0 mV * 8192 / 100571 * 10`.
const RO_COUNT: u32 = 754;
/// Per-rail monitor count. Only "non-zero and stable" is load-bearing.
const RAIL_COUNT: u32 = 754;

#[derive(Default)]
pub struct Avs {
    storage: BTreeMap<u32, u32>,
}

impl Avs {
    pub fn new() -> Avs {
        Avs::default()
    }
}

impl MmioDevice for Avs {
    fn name(&self) -> &'static str {
        "avs-monitor"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        if (RESULT_BASE..RESULT_BASE + RESULT_CHANNELS * 4).contains(&off) {
            let ch = (off - RESULT_BASE) / 4;
            let count = if ch == 0 { TEMP_COUNT } else { RO_COUNT };
            return Ok(SETTLED | VALID | count);
        }
        if (RAIL_BASE..RAIL_BASE + RAIL_CHANNELS * 4).contains(&off) {
            return Ok(SETTLED | RAIL_COUNT);
        }
        Ok(self.storage.get(&off).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset & !3, value);
        Ok(())
    }
}
