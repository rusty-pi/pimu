//! BCM2711 power-management block (`0x7E10_0000`): reset control + watchdog.
//!
//! Registers and fields: `specs/pm.toml` ([`crate::spec::pm`]).
//!
//! The SoC reset the bootloader triggers after a self-update and the watchdog
//! start4 arms at the ARM hand-off for `dtparam=watchdog=on` are the same
//! hardware feature: `WDOG` then `RSTC` with `WRCFG_FULL_RESET`, differing only
//! in the timeout. So the model counts down — the `RSTC` write starts the timer
//! from the last `WDOG` value and [`Pm::take_reset`] reports the moment it
//! expires. **The arm itself is not a reset**; treating it as one reboots every
//! `watchdog=on` boot at the hand-off.
//!
//! **`WDOG` is the counter, not a shadow of it**, so writing it reloads a
//! running countdown. That is how the firmware's own boot watchdog is fed: it
//! is armed once and every heartbeat after that is a bare `WDOG` write, with no
//! second `RSTC` anywhere, so a write that does not reload resets the boot one
//! window in however large its budget was.
//!
//! Everything else is sticky storage with the password byte masked on read-back.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

// `RSTS`' power-on value is exactly `HADWRF`, the full watchdog reset the
// power sequencing performs, and not `HADPOR` — matching the bootloader's own
// `power-on-reset 0`. That bit is odd, so it adds nothing to the partition the
// bootloader packs into the even bits 0..10.
use crate::spec::pm::{
    DOMAIN_STATUS, DOMAIN_STATUS_COUNT, DOMAIN_STATUS_RESET, DOMAIN_STATUS_STRIDE, GRAFX, IMAGE,
    RSTC, RSTC_PASSWD_MASK as PASSWD_MASK, RSTC_WRCFG_SHIFT, RSTS, RSTS_RESET, RSTS_TRYBOOT_MASK,
    SPARER, SPAREW, WDOG, WDOG_TIME_MASK,
};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "pm",
    decoded: &[
        RSTC,
        RSTS,
        WDOG,
        DOMAIN_STATUS,
        IMAGE,
        GRAFX,
        SPAREW,
        SPARER,
    ],
};

const PASSWD: u32 = 0x5A00_0000;

const RSTC_WRCFG_FULL_RESET: u32 = 2 << RSTC_WRCFG_SHIFT;

/// The watchdog counts at 65536 Hz: one `WDOG` tick is 1 s / 65536 ≈ 15.26 µs.
const WDOG_HZ: u64 = 65_536;

#[derive(Default)]
pub struct Pm {
    storage: BTreeMap<u32, u32>,
    now_us: u64,
    deadline_us: Option<u64>,
    reset_pending: bool,
}

impl Pm {
    pub fn new() -> Pm {
        let mut pm = Pm::default();
        // Power-on state of `RSTS`, as measured on a Raspberry Pi 4B d03115.
        // The firmware latches it early and prints it as `PM_RSTS %08x`.
        pm.storage.insert(RSTS, RSTS_RESET);
        pm
    }

    pub fn advance(&mut self, now_us: u64) {
        self.now_us = now_us;
        if self.deadline_us.is_some_and(|d| now_us >= d) {
            self.deadline_us = None;
            self.reset_pending = true;
        }
    }

    pub fn watchdog_running(&self) -> bool {
        self.deadline_us.is_some()
    }

    fn us_for(ticks: u32) -> u64 {
        (u64::from(ticks) * 1_000_000).div_ceil(WDOG_HZ)
    }

    fn ticks_left(&self) -> u32 {
        match self.deadline_us {
            Some(d) => {
                let us = d.saturating_sub(self.now_us);
                ((us * WDOG_HZ).div_ceil(1_000_000)).min(WDOG_TIME_MASK as u64) as u32
            }
            None => 0,
        }
    }

    pub fn reset_pending(&self) -> bool {
        self.reset_pending
    }

    pub fn take_reset(&mut self) -> bool {
        std::mem::take(&mut self.reset_pending)
    }

    /// The partition bits of `RSTS`, which a watchdog reset leaves alone:
    /// that is how Linux's `bcm2835_restart` tells the bootloader which
    /// partition to boot, and its power-off asks for partition 63 (`0x555`)
    /// to mean "halt".
    pub fn partition_bits(&self) -> u32 {
        self.storage.get(&RSTS).copied().unwrap_or(0) & RSTS_PARTITION
    }

    pub fn keep_partition_bits(&mut self, bits: u32) {
        self.storage
            .insert(RSTS, RSTS_RESET | (bits & RSTS_PARTITION));
    }

    /// Ask for a tryboot before the machine has booted at all, which is
    /// otherwise only reachable through a boot of its own. The bootcode takes
    /// the request off as it reads it, so it lasts exactly one boot.
    pub fn request_tryboot(&mut self) {
        let rsts = self.storage.get(&RSTS).copied().unwrap_or(RSTS_RESET);
        self.storage.insert(RSTS, rsts | RSTS_TRYBOOT_MASK);
    }
}

/// `RSTS` bits 0, 2, .. 10: the partition field, and bit 1, the tryboot
/// request, which a reboot has to carry as well.
const RSTS_PARTITION: u32 = 0x555 | RSTS_TRYBOOT_MASK;

impl MmioDevice for Pm {
    fn name(&self) -> &'static str {
        "pm"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        if off == WDOG {
            return Ok(self.ticks_left());
        }
        // The spare word is written at one offset and read at the next:
        // the bootloader leaves the partition it booted there for start4.
        let off = if off == SPARER { SPAREW } else { off };
        if let Some(&v) = self.storage.get(&off) {
            return Ok(v & !PASSWD_MASK);
        }
        // Power-domain status registers (`PM_GRAFX`, `PM_IMAGE`, ...): report
        // the domain powered and its clocks stable so that branch runs.
        // (`RSTS` is seeded in [`Pm::new`]; the rest of the block reads as 0.)
        if (DOMAIN_STATUS..DOMAIN_STATUS + DOMAIN_STATUS_COUNT * DOMAIN_STATUS_STRIDE)
            .contains(&off)
        {
            return Ok(DOMAIN_STATUS_RESET);
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
                self.deadline_us = Some(self.now_us + Self::us_for(ticks));
            } else {
                self.deadline_us = None;
            }
        }
        // The register is the counter: writing it while the dog runs reloads
        // it, which is the firmware's whole heartbeat (module docs).
        if off == WDOG && passworded && self.deadline_us.is_some() {
            self.deadline_us = Some(self.now_us + Self::us_for(value & WDOG_TIME_MASK));
        }
        self.storage.insert(off, value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsts_powers_up_as_a_watchdog_reset() {
        let mut pm = Pm::new();
        let rsts = pm.read(RSTS, Width::Word).unwrap();
        // What a Raspberry Pi 4B d03115 reports: `PM_RSTS 00000020`.
        assert_eq!(rsts, 0x0000_0020);
        // ...and it must not disturb the partition the bootloader decodes.
        assert_eq!(rsts & RSTS_PARTITION, 0);
    }

    #[test]
    fn the_partition_survives_a_reset() {
        let mut pm = Pm::new();
        pm.write(RSTS, Width::Word, PASSWD | 0x555).unwrap();
        let mut next = Pm::new();
        next.keep_partition_bits(pm.partition_bits());
        assert_eq!(next.read(RSTS, Width::Word).unwrap(), 0x0000_0575);
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
    fn a_tryboot_request_survives_a_reset() {
        let mut pm = Pm::new();
        pm.write(RSTS, Width::Word, PASSWD | 0x22).unwrap();
        let kept = pm.partition_bits();
        let mut after = Pm::new();
        after.keep_partition_bits(kept);
        assert_eq!(after.read(RSTS, Width::Word).unwrap(), RSTS_RESET | 0x2);
    }

    /// `--tryboot`: the same request, seeded before the first boot instead of
    /// left behind by one. It reads as the power-on value plus the bit, and
    /// the partition field is untouched.
    #[test]
    fn a_tryboot_can_be_asked_for_at_power_on() {
        let mut pm = Pm::new();
        pm.request_tryboot();
        assert_eq!(pm.read(RSTS, Width::Word).unwrap(), RSTS_RESET | 0x2);
        assert_eq!(pm.partition_bits(), RSTS_TRYBOOT_MASK);
    }

    #[test]
    fn the_spare_word_reads_back_on_the_next_offset() {
        let mut pm = Pm::new();
        assert_eq!(pm.read(SPARER, Width::Word).unwrap(), 0);
        pm.write(SPAREW, Width::Word, 0x5A40_0002).unwrap();
        assert_eq!(pm.read(SPARER, Width::Word).unwrap(), 0x0040_0002);
        assert_eq!(pm.read(SPAREW, Width::Word).unwrap(), 0x0040_0002);
    }

    #[test]
    fn a_short_watchdog_resets_when_it_expires() {
        let mut pm = Pm::new();
        pm.advance(1_000);
        pm.write(WDOG, Width::Word, PASSWD | 10).unwrap();
        pm.write(RSTC, Width::Word, PASSWD | RSTC_WRCFG_FULL_RESET)
            .unwrap();
        assert!(pm.watchdog_running());
        assert!(!pm.take_reset());
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
        assert_eq!(
            pm.read(RSTC, Width::Word).unwrap() & RSTC_WRCFG_FULL_RESET,
            0x20
        );
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

    /// The firmware's boot watchdog: armed once, then fed with `WDOG` writes
    /// alone. Each one has to reload the countdown, or the boot resets 16 s
    /// in however large its budget was.
    #[test]
    fn a_wdog_write_reloads_a_running_countdown() {
        let mut pm = Pm::new();
        pm.write(WDOG, Width::Word, PASSWD | WDOG_TIME_MASK)
            .unwrap();
        pm.write(RSTC, Width::Word, PASSWD | RSTC_WRCFG_FULL_RESET)
            .unwrap();
        for beat in 1..=6 {
            pm.advance(beat * 10_000_000);
            assert!(!pm.take_reset(), "reset at beat {beat}");
            pm.write(WDOG, Width::Word, PASSWD | WDOG_TIME_MASK)
                .unwrap();
            assert_eq!(pm.read(WDOG, Width::Word).unwrap(), WDOG_TIME_MASK);
        }
        let window = Pm::us_for(WDOG_TIME_MASK);
        pm.advance(60_000_000 + window - 1);
        assert!(!pm.take_reset());
        pm.advance(60_000_000 + window);
        assert!(pm.take_reset());
    }

    /// A `WDOG` write with the dog not armed only loads the counter: nothing
    /// starts counting until `RSTC` says so. The bootcode writes `WDOG`
    /// before it arms.
    #[test]
    fn a_wdog_write_alone_starts_nothing() {
        let mut pm = Pm::new();
        pm.write(WDOG, Width::Word, PASSWD | 10).unwrap();
        assert!(!pm.watchdog_running());
        pm.advance(10_000_000);
        assert!(!pm.take_reset());
        pm.write(RSTC, Width::Word, PASSWD | RSTC_WRCFG_FULL_RESET)
            .unwrap();
        assert!(pm.watchdog_running());
        pm.advance(10_000_153);
        assert!(pm.take_reset());
    }

    /// A password-less `WDOG` write is ignored by the hardware, so it must not
    /// reload the countdown either.
    #[test]
    fn an_unpassworded_wdog_write_does_not_reload() {
        let mut pm = Pm::new();
        pm.write(WDOG, Width::Word, PASSWD | 65_536).unwrap();
        pm.write(RSTC, Width::Word, PASSWD | RSTC_WRCFG_FULL_RESET)
            .unwrap();
        pm.advance(500_000);
        pm.write(WDOG, Width::Word, 65_536).unwrap();
        pm.advance(1_000_000);
        assert!(pm.take_reset());
    }
}
