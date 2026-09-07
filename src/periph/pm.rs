//! BCM2711 power-management block (`0x7E10_0000`): reset control + watchdog.
//!
//! The only behaviour the model needs is the SoC reset the EEPROM bootloader
//! triggers after applying a self-update ("EEPROMs updated. Rebooting / RESET"):
//! it writes `RSTC` (`+0x1C`) with the `0x5A` password and a reset config, then
//! spins in a delay loop expecting the chip to reboot. [`Pm::take_reset`] lets
//! the run loop notice that and restart from a fresh machine.
//!
//! Everything else is sticky storage with the password byte masked on read-back.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

const RSTC: u32 = 0x1C;
const RSTS: u32 = 0x20;
const WDOG: u32 = 0x24;

const PASSWD: u32 = 0x5A00_0000;
const PASSWD_MASK: u32 = 0xFF00_0000;

/// `RSTC` WRCFG field: `0b10` in bits [5:4] = full reset.
const RSTC_WRCFG_FULL_RESET: u32 = 0x20;

#[derive(Default)]
pub struct Pm {
    storage: BTreeMap<u32, u32>,
    /// Set once the watchdog is armed with a timeout — the bootloader does this
    /// only as the first half of a reboot.
    wdog_armed: bool,
    reset_pending: bool,
}

impl Pm {
    pub fn new() -> Pm {
        Pm::default()
    }

    /// Consume a pending reset request.
    pub fn take_reset(&mut self) -> bool {
        std::mem::take(&mut self.reset_pending)
    }
}

impl MmioDevice for Pm {
    fn name(&self) -> &'static str {
        "pm"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        Ok(self.storage.get(&off).copied().unwrap_or(0) & !PASSWD_MASK)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        let passworded = value & PASSWD_MASK == PASSWD;
        // The bootloader's reboot is `WDOG = PASSWORD | <ticks>` then
        // `RSTC = PASSWORD | WRCFG_FULL_RESET`. Startup also pokes RSTC (with
        // `0x200`/`0x202`, no WRCFG bits) — that must not count as a reset.
        if off == WDOG && passworded && value & 0x000F_FFFF != 0 {
            self.wdog_armed = true;
        }
        if off == RSTC && passworded && (value & RSTC_WRCFG_FULL_RESET != 0 || self.wdog_armed) {
            self.reset_pending = true;
        }
        let _ = RSTS;
        self.storage.insert(off, value);
        Ok(())
    }
}
