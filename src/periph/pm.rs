//! BCM2711 power-management block (`0x7E10_0000`): reset control + watchdog.
//!
//! Two things drive the model here, and they are the same hardware feature:
//!
//! * the SoC reset the EEPROM bootloader triggers after applying a self-update
//!   ("EEPROMs updated. Rebooting / RESET"): `WDOG = PASSWORD | 10` then
//!   `RSTC = PASSWORD | WRCFG_FULL_RESET`, after which it spins in a delay loop
//!   expecting the chip to reboot;
//! * the watchdog `start4` arms at the ARM hand-off when `config.txt` carries
//!   `dtparam=watchdog=on`: `WDOG = PASSWORD | 0xFFFFF` (the 20-bit maximum,
//!   16 s at the watchdog's 65536 Hz) then `RSTC = PASSWORD | 0x3222`, seen at
//!   `0x3ED62334`/`0x3ED62342` right after `arm_loader: Starting ARM`. That is
//!   byte for byte Linux's `bcm2835_wdt_start`, and Linux's probe then finds
//!   the dog running (`bcm2835_wdt_is_running`) and keeps it fed.
//!
//! Both are "arm the countdown"; only the timeout differs. So the model counts
//! down: a `RSTC` write with `WRCFG_FULL_RESET` set starts the timer from the
//! last `WDOG` value, `WDOG` reads back the ticks left (`get_timeleft`), and
//! [`Pm::take_reset`] reports the moment it expires. Treating the arm itself as
//! the reset made every `watchdog=on` boot reboot at the hand-off.
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

/// `RSTS` bit 5, `HADWRF`: "had a watchdog reset, full". `RSTS` latches which
/// reset source last fired — the debug (`HADDR*`, bits 0..2), watchdog
/// (`HADWR*`, bits 4..6) and software (`HADSR*`, bits 8..10) groups plus
/// `HADPOR` (bit 12). A Pi 4 coming out of the power-on sequence reports
/// exactly `HADWRF`: the last reset was the full watchdog reset the power
/// sequencing performs, and `HADPOR` is *not* set (matching the bootloader's
/// own `power-on-reset 0`).
///
/// Note the overlap with the "partition to boot" field, which the bootloader
/// packs into the *even* bits 0, 2, 4, 6, 8 and 10 (mask `0x555`; see
/// `FUN_00000578`/`FUN_00000666` in `firmware/source/pieeprom.bin.c`). Bit 5 is
/// odd, so it contributes nothing to the decoded partition — `0x20` still
/// decodes to partition 0, same as the all-zero value did.
const RSTS_HADWRF: u32 = 0x20;

/// The watchdog counts at 65536 Hz: one `WDOG` tick is 1 s / 65536 ≈ 15.26 µs.
const WDOG_HZ: u64 = 65_536;
/// `WDOG` timeout field.
const WDOG_TIME_MASK: u32 = 0x000F_FFFF;

#[derive(Default)]
pub struct Pm {
    storage: BTreeMap<u32, u32>,
    /// Model time, in microseconds, as of the last [`Pm::advance`].
    now_us: u64,
    /// When the armed countdown expires, in model microseconds.
    deadline_us: Option<u64>,
    reset_pending: bool,
}

impl Pm {
    pub fn new() -> Pm {
        let mut pm = Pm::default();
        // Power-on state of `RSTS`, as measured on a real Pi 4: see the
        // `RSTS_HADWRF` comment. The firmware latches this value early and
        // prints it as `PM_RSTS %08x`.
        pm.storage.insert(RSTS, RSTS_HADWRF);
        pm
    }

    /// Feed the watchdog the model clock. Called from `Machine::tick`; the
    /// countdown fires when the deadline the last arm set has passed.
    pub fn advance(&mut self, now_us: u64) {
        self.now_us = now_us;
        if self.deadline_us.is_some_and(|d| now_us >= d) {
            self.deadline_us = None;
            self.reset_pending = true;
        }
    }

    /// True while the countdown is armed and running.
    pub fn watchdog_running(&self) -> bool {
        self.deadline_us.is_some()
    }

    /// `WDOG` ticks remaining, which is what the register reads back as.
    fn ticks_left(&self) -> u32 {
        match self.deadline_us {
            Some(d) => {
                let us = d.saturating_sub(self.now_us);
                ((us * WDOG_HZ).div_ceil(1_000_000)).min(WDOG_TIME_MASK as u64) as u32
            }
            None => 0,
        }
    }

    /// True once, after the firmware has asked for a SoC reset.
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
        if off == WDOG {
            return Ok(self.ticks_left());
        }
        if let Some(&v) = self.storage.get(&off) {
            return Ok(v & !PASSWD_MASK);
        }
        // Power-domain status registers (`PM_GRAFX`, `PM_IMAGE`, ...): report
        // the domain powered and its clocks stable so that branch runs.
        // (`RSTS` is seeded in [`Pm::new`]; the rest of the block reads as 0.)
        if (0x40..0x60).contains(&off) {
            return Ok(0x0000_7040);
        }
        Ok(0)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        let passworded = value & PASSWD_MASK == PASSWD;
        if off == RSTC && passworded {
            // `WRCFG_FULL_RESET` arms the countdown from the current `WDOG`
            // value; clearing it (`bcm2835_wdt_stop` writes `0x102`) stops it.
            // Startup pokes `RSTC` with `0x200`/`0x202` — no WRCFG bits — and
            // that must not count as anything.
            if value & RSTC_WRCFG_FULL_RESET != 0 {
                let ticks = self.storage.get(&WDOG).copied().unwrap_or(0) & WDOG_TIME_MASK;
                let us = (u64::from(ticks) * 1_000_000).div_ceil(WDOG_HZ);
                self.deadline_us = Some(self.now_us + us);
            } else {
                self.deadline_us = None;
            }
        }
        self.storage.insert(off, value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Partition field: `RSTS` bits 0, 2, 4, 6, 8, 10.
    const RSTS_PARTITION: u32 = 0x555;

    #[test]
    fn rsts_powers_up_as_a_watchdog_reset() {
        let mut pm = Pm::new();
        let rsts = pm.read(RSTS, Width::Word).unwrap();
        // What a real Pi 4 reports: `PM_RSTS 00000020`.
        assert_eq!(rsts, 0x0000_0020);
        // ...and it must not disturb the partition the bootloader decodes.
        assert_eq!(rsts & RSTS_PARTITION, 0);
    }

    #[test]
    fn rsts_is_writable_and_drops_the_password() {
        let mut pm = Pm::new();
        pm.write(RSTS, Width::Word, PASSWD).unwrap();
        assert_eq!(pm.read(RSTS, Width::Word).unwrap(), 0);
        assert!(!pm.take_reset());
    }

    /// The bootloader's reboot: a 10-tick timeout, then the arm. The reset
    /// lands when the countdown expires, not on the arm itself.
    #[test]
    fn a_short_watchdog_resets_when_it_expires() {
        let mut pm = Pm::new();
        pm.advance(1_000);
        pm.write(WDOG, Width::Word, PASSWD | 10).unwrap();
        pm.write(RSTC, Width::Word, PASSWD | RSTC_WRCFG_FULL_RESET).unwrap();
        assert!(pm.watchdog_running());
        assert!(!pm.take_reset());
        // 10 ticks at 65536 Hz is 153 µs.
        pm.advance(1_000 + 152);
        assert!(!pm.take_reset());
        pm.advance(1_000 + 153);
        assert!(pm.take_reset());
        assert!(!pm.watchdog_running());
    }

    /// What `start4` does at the ARM hand-off with `dtparam=watchdog=on`, and
    /// what Linux's `bcm2835_wdt` then reads: a running dog with ~16 s left.
    /// Stopping it the way `bcm2835_wdt_stop` does must not reset either.
    #[test]
    fn the_hand_off_watchdog_runs_for_sixteen_seconds_and_can_be_stopped() {
        let mut pm = Pm::new();
        pm.write(WDOG, Width::Word, PASSWD | 0xF_FFFF).unwrap();
        pm.write(RSTC, Width::Word, PASSWD | 0x3222).unwrap();
        assert_eq!(pm.read(RSTC, Width::Word).unwrap() & RSTC_WRCFG_FULL_RESET, 0x20);
        assert_eq!(pm.read(WDOG, Width::Word).unwrap(), 0xF_FFFF);
        pm.advance(8_000_000);
        assert!(!pm.take_reset());
        let left = pm.read(WDOG, Width::Word).unwrap();
        assert!((0x7_FFF0..=0x8_0010).contains(&left), "{left:#x}");
        pm.write(RSTC, Width::Word, PASSWD | 0x102).unwrap();
        assert!(!pm.watchdog_running());
        pm.advance(60_000_000);
        assert!(!pm.take_reset());
    }
}
