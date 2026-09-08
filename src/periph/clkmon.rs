//! BCM2711 VPU clock block at `0x7D5D_0000` — PLL control + clock-frequency
//! monitors, used by `start4.elf`'s clock manager when the SoC-type switch at
//! `0x3EC635F0` sets `[gp+5476] = 1` (which it does for the Pi 4 / BCM2711).
//!
//! This window sits *outside* the `0x7E…` legacy peripheral aperture, so
//! without this device its reads fold onto (unmapped) DRAM and every "is the
//! measurement ready" / "has the PLL locked" poll spins forever.
//!
//! We do not model the analogue PLLs or the frequency counters — we store
//! writes (stripping the `0x5A` password byte for read-back) and force the
//! handful of status bits the firmware actually waits on:
//!
//! - `0x2200` (+ its 16-byte neighbourhood): the oscillator-count monitor
//!   `measure_clock` (`0x3ED7C8DA`) polls — bit 10 ("measurement valid") set,
//!   count field `[9:0]` zero.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

pub const BASE: u32 = 0x7D5D_0000;
pub const SIZE: u32 = 0x0001_0000;

const PASSWD: u32 = 0x5A00_0000;

/// Oscillator-count monitor: bit 10 = measurement valid, bit 4 = settled,
/// `[9:0]` = count. `0x3ED603E2` polls for bits 10 **and** 4 (≤10 retries)
/// before it trusts the count; `measure_clock` (`0x3ED7C8DA`) only checks
/// bit 10.
const FREQ_MON: u32 = 0x2200;
const FREQ_MON_READY: u32 = (1 << 10) | (1 << 4);

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
        if off & !0xF == FREQ_MON {
            return Ok(FREQ_MON_READY);
        }
        Ok(self.storage.get(&off).copied().unwrap_or(0) & !PASSWD)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset & !3, value);
        Ok(())
    }
}
