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
//! The AVS monitor at `0x7D5D_2000` sits inside this window and is decoded
//! ahead of it; see [`crate::periph::avs`].

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

pub const BASE: u32 = 0x7D5D_0000;
pub const SIZE: u32 = 0x0001_0000;

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
