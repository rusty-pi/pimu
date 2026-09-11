//! BCM2711 clock manager (`0x7E10_1000`), plus the A2W PLL control aliased into
//! the same window.
//!
//! The EEPROM bootloader programs a PLL and then polls for it to lock before it
//! trusts the SPI / peripheral clocks. We do not model the analogue PLL — we
//! just make every "is it ready yet" bit read back as ready:
//!
//! - `*_CTL` registers: the `BUSY` bit (7) always reads 0.
//! - `CM_LOCK` (`0x114`): every PLL-locked bit reads 1.
//! - `+0x100`: a self-clearing "delay N clocks" register — the SDHCI driver's
//!   register-write helper (`0x00081dc0`) writes `password | <cycle count>`
//!   here and spins until it reads back 0 (`0x00081de2`).
//! - `A2W_PLL*_ANA` / frac / ctrl: sticky (last written value, password masked).
//!
//! Writes carry the `0x5A` password in the top byte; we strip it on read-back so
//! the firmware's `read / modify / write` sequences converge.
//!
//! ## `CM_UARTCTL` (`0x7E10_10F0`) — measured, and why nothing is forced here
//!
//! Two places in `start4.elf` gate on bit 4 (`ENAB`) of this register:
//!
//! - the console writer `0x3ED85E9C`, which polls `UART_FR.TXFF` before each
//!   byte only while the UART clock is running, and
//! - the PL011 clock-change callback `0x3EC799BC`, whose phase-0 leg drains
//!   `UART_FR.BUSY` before it writes `UARTCR = 0`.
//!
//! Read off `rpi-dev` (Pi 4, Linux up, idle) at the `0xFE10_10F0` alias, one
//! enumerated offset at a time through `/dev/mem`:
//!
//! ```text
//! 0x7e1010f0  CM_UARTCTL  0x00000296     (MASH=1, BUSY=1, ENAB=1, SRC=6/PLLD)
//! 0x7e1010f4  CM_UARTDIV  0x0000fa00     (DIVI=250)
//! ```
//!
//! `ENAB` and `SRC` are plain read/write state, so the stored-write read-back
//! above reproduces whatever the firmware programs. In the modelled boot both
//! gates read `0x11` — `SRC=1` (oscillator) because that is what start4 itself
//! writes, and crucially the same `ENAB` bit set that hardware shows; the
//! measured `SRC=6` / `MASH=1` is Linux's later reprogramming, not a
//! divergence. Both gates therefore take the same branch here as on silicon.
//!
//! The single forced difference is `BUSY`, which real silicon holds at 1 while
//! the generator runs and this device always reports as 0 — deliberately,
//! because the shutdown path `0x3EC7F0BA` spins on `BUSY` (with a
//! 1000-iteration escape) waiting for clocks the model stops instantly.
//! Neither consumer above reads `BUSY` at all. Recorded so `0x7E10_10F0` is
//! not re-investigated — it is not a `SCALER_DISPID`-style blanked status
//! register.

use alloc::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

/// `CM_LOCK` — one bit per PLL, set when that PLL has locked.
const CM_LOCK: u32 = 0x114;
/// Self-clearing calibrated-delay register — always reads back 0.
const CM_DELAY: u32 = 0x100;
/// `BUSY` bit in every `CM_*_CTL` register.
const CTL_BUSY: u32 = 1 << 7;
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
        if off == CM_DELAY {
            return Ok(0);
        }
        let mut v = self.storage.get(&off).copied().unwrap_or(0) & !PASSWD;
        // Any register whose name ends in _CTL sits at a 0x00/0x08/0x10... slot;
        // clearing BUSY unconditionally is harmless for the others.
        v &= !CTL_BUSY;
        Ok(v)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset & !3, value);
        Ok(())
    }
}
