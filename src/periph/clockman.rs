//! BCM2711 clock manager (`0x7E10_1000`), plus the A2W PLL control aliased into
//! the same window.
//!
//! The EEPROM bootloader programs a PLL and then polls for it to lock before it
//! trusts the SPI / peripheral clocks. We do not model the analogue PLL — we
//! just make every "is it ready yet" bit read back as ready:
//!
//! - `*_CTL` registers: the `BUSY` bit (7) always reads 0.
//! - `CM_LOCK` (`0x114`): every PLL-locked bit reads 1.
//! - `A2W_PLL*_ANA` / frac / ctrl: sticky (last written value, password masked).
//!
//! Writes carry the `0x5A` password in the top byte; we strip it on read-back so
//! the firmware's `read / modify / write` sequences converge.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

/// `CM_LOCK` — one bit per PLL, set when that PLL has locked.
const CM_LOCK: u32 = 0x114;
/// `BUSY` bit in every `CM_*_CTL` register.
const CTL_BUSY: u32 = 1 << 7;
/// `KILL` bit — a write with it set stops the clock at once; the firmware then
/// polls the register for 0 (`0x00081dc0` gates a display clock this way).
const CTL_KILL: u32 = 1 << 5;
/// Password byte the firmware ORs into every clock-manager write.
const PASSWD: u32 = 0x5A00_0000;

#[derive(Default)]
pub struct ClockManager {
    storage: BTreeMap<u32, u32>,
}

impl ClockManager {
    pub fn new() -> ClockManager {
        ClockManager::default()
    }
}

impl MmioDevice for ClockManager {
    fn name(&self) -> &'static str {
        "clock-manager"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        if off == CM_LOCK {
            return Ok(0xFFFF_FFFF);
        }
        let mut v = self.storage.get(&off).copied().unwrap_or(0) & !PASSWD;
        // Any register whose name ends in _CTL sits at a 0x00/0x08/0x10... slot;
        // clearing BUSY unconditionally is harmless for the others.
        v &= !CTL_BUSY;
        Ok(v)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        // A KILL write stops the clock immediately — read-back is 0.
        let stored = if value & CTL_KILL != 0 { 0 } else { value };
        self.storage.insert(offset & !3, stored);
        Ok(())
    }
}
