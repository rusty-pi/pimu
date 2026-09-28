//! BCM2711 clock manager (`0x7E10_1000`), plus the A2W PLL control aliased into
//! the same window.
//!
//! Registers and fields: `specs/cm.toml`.
//!
//! The EEPROM bootloader programs a PLL and then polls for it to lock before it
//! trusts the SPI / peripheral clocks. The analogue PLLs are not modelled, so
//! every "is it ready yet" bit reads back as ready: `CM_LOCK` answers every
//! PLL locked, every `*_CTL` register reads `BUSY` clear, and the self-clearing
//! `DELAY` register — where the bootloader's register-write helper parks for a
//! number of clocks — reads 0 at once. Everything else is the last value
//! written, with the `0x5A` password byte stripped so the firmware's
//! read/modify/write sequences converge.
//!
//! `BUSY` is the one forced difference from silicon, which holds it at 1 while
//! a generator runs: the firmware's clock-shutdown path spins on `BUSY` (with
//! a 1000-iteration escape) waiting for clocks the model stops instantly. Its
//! two consumers of `CM_UARTCTL` — the console writer, which polls
//! `UART_FR.TXFF` only while the UART clock runs, and the PL011 clock-change
//! callback, which drains `UART_FR.BUSY` before clearing `UARTCR` — gate on
//! `ENAB`, not on `BUSY`.
//!
//! `ENAB` and `SRC` are plain read/write state, so the stored-write read-back
//! reproduces whatever the firmware programs. A Raspberry Pi 4B d03115 reads
//! `CM_UARTCTL` as `0x296` (`MASH` 1, `BUSY` 1, `ENAB` 1, `SRC` 6 / PLLD) and
//! `CM_UARTDIV` as `0xFA00` (`DIVI` 250) with Linux up; the modelled boot has
//! `SRC` 1 (oscillator) because that is what start4 writes, with the same
//! `ENAB` bit set. The measured `SRC` 6 is Linux's later reprogramming, not a
//! divergence.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

// `BUSY` is bit 7 of every `CM_*_CTL` register, not only `UARTCTL`'s.
use crate::spec::cm::{
    DELAY as CM_DELAY, GEN_1E8_CTL, GEN_210_CTL, LOCK as CM_LOCK, UARTCTL,
    UARTCTL_BUSY_MASK as CTL_BUSY, UARTCTL_ENAB_MASK as CTL_ENAB, UARTCTL_SRC_MASK as CTL_SRC,
    UARTDIV,
};
use crate::spec::Coverage;

/// `LOCK` and `DELAY` are modelled; `UARTCTL` / `UARTDIV` get the storage
/// every `*_CTL` / `*_DIV` register does.
pub const COVERAGE: Coverage = Coverage {
    block: "cm",
    decoded: &[UARTCTL, UARTDIV, CM_DELAY, CM_LOCK],
};
/// Password byte the firmware ORs into every clock-manager write.
const PASSWD: u32 = 0x5A00_0000;

#[derive(Default)]
pub struct ClockManager {
    storage: BTreeMap<u32, u32>,
    uart_disturbed: bool,
}

impl ClockManager {
    pub fn new() -> ClockManager {
        ClockManager::default()
    }

    fn generator_running(&self, ctl: u32) -> bool {
        let v = self.storage.get(&ctl).copied().unwrap_or(0);
        v & CTL_ENAB != 0 && v & CTL_SRC != 0
    }

    /// Whether the two generators the Ethernet block runs from are both going.
    /// The stock bootloader starts them (`0x3000` and `0x6000` off `SRC` 6, i.e.
    /// 250 and 125 MHz from PLLD's peripheral channel) immediately before its
    /// first GENET access, and nothing else in a boot writes them, so this says
    /// whether GENET has a clock at all.
    pub fn eth_clocks_running(&self) -> bool {
        self.generator_running(GEN_1E8_CTL) && self.generator_running(GEN_210_CTL)
    }

    /// Whether `UARTCLK` is running: a PL011 programmed against a stopped
    /// generator never transmits. The boot ROM leaves the generator going, so
    /// an untouched register counts as live rather than as a stopped one.
    pub fn uart_clock_live(&self) -> bool {
        self.storage.get(&UARTCTL).is_none_or(|v| v & CTL_ENAB != 0)
    }

    /// Whether the UART clock has been stopped or retuned since the last call.
    pub fn take_uart_disturbance(&mut self) -> bool {
        core::mem::take(&mut self.uart_disturbed)
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
        // Every `*_CTL` register sits at a `0x00` / `0x08` / `0x10` … slot;
        // clearing `BUSY` unconditionally is harmless for the others.
        v &= !CTL_BUSY;
        Ok(v)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        let was = self.storage.get(&off).copied().unwrap_or(0);
        if off == UARTCTL && was & CTL_ENAB != 0 && (was ^ value) & !PASSWD != 0 {
            self.uart_disturbed = true;
        }
        if off == UARTDIV && self.uart_clock_live() && (was ^ value) & !PASSWD != 0 {
            self.uart_disturbed = true;
        }
        self.storage.insert(off, value);
        Ok(())
    }
}
