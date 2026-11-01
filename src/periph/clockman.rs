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
//! `CM_PLLB.HOLDARM` is the one bit with an effect outside the block: while it
//! is set the ARM cores have no clock, and [`ClockManager::arm_held`] tells the
//! ARM side to leave them be ([`crate::arm::ArmSide::run`]). The register reads
//! it back like any other.
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
    DELAY as CM_DELAY, GEN_1E8_CTL, GEN_210_CTL, LOCK as CM_LOCK, PLLA_CORE, PLLA_CTRL,
    PLLA_CTRL_NDIV_MASK, PLLA_CTRL_PDIV_MASK, PLLA_CTRL_PDIV_SHIFT, PLLA_CTRL_PWRDN_MASK,
    PLLA_FRAC, PLLB, PLLB_HOLDARM_MASK, PLLC_CORE0, PLLC_CORE1, PLLC_CORE2,
    PLLC_CORE2_DISABLE_MASK, PLLC_CORE2_DIV_MASK, PLLC_CTRL, PLLC_FRAC, PLLD_CORE, PLLD_CTRL,
    PLLD_FRAC, UARTCTL, UARTCTL_BUSY_MASK as CTL_BUSY, UARTCTL_ENAB_MASK as CTL_ENAB,
    UARTCTL_SRC_MASK as CTL_SRC, UARTDIV, VPUCTL, VPUDIV,
};
use crate::spec::Coverage;

/// `LOCK`, `DELAY` and `PLLB` are modelled; `UARTCTL` / `UARTDIV` get the
/// storage every `*_CTL` / `*_DIV` register does; the VPU clock is worked out
/// from `VPUCTL`, `VPUDIV` and the PLLs they can select.
pub const COVERAGE: Coverage = Coverage {
    block: "cm",
    decoded: &[
        UARTCTL, UARTDIV, CM_DELAY, CM_LOCK, PLLB, VPUCTL, VPUDIV, PLLA_CTRL, PLLA_FRAC, PLLA_CORE,
        PLLC_CTRL, PLLC_FRAC, PLLC_CORE0, PLLC_CORE1, PLLC_CORE2, PLLD_CTRL, PLLD_FRAC, PLLD_CORE,
    ],
};
/// Password byte the firmware ORs into every clock-manager write.
const PASSWD: u32 = 0x5A00_0000;

/// The oscillator every PLL and the boot-time VPU clock run from.
pub const OSCILLATOR_HZ: u64 = 54_000_000;

/// Where the bootcode's copies of the PLL registers sit above the ones start4
/// and Linux use. They are one set of registers to the PLL, so the VPU clock
/// follows whichever was written last.
const PLL_COPY_BASE: u32 = 0x1900;
const PLL_COPY_OFFSET: u32 = 0x800;

/// The first register of the A2W half, where the PLL words start.
const A2W_BASE: u32 = 0x1100;

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct ClockManager {
    storage: BTreeMap<u32, u32>,
    /// The PLL words and channel dividers, under their `start4` offsets
    /// whichever copy they were written through.
    pll: BTreeMap<u32, u32>,
    uart_disturbed: bool,
    arm_held: bool,
    vpu_changed: bool,
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

    /// Whether PLLB's ARM channel is held, so the ARM cores have no clock.
    pub fn arm_held(&self) -> bool {
        self.arm_held
    }

    /// Whether `UARTCLK` is running: a PL011 programmed against a stopped
    /// generator never transmits. The boot ROM leaves the generator going, so
    /// an untouched register counts as live rather than as a stopped one.
    pub fn uart_clock_live(&self) -> bool {
        self.storage.get(&UARTCTL).is_none_or(|v| v & CTL_ENAB != 0)
    }

    fn pll_word(&self, off: u32) -> u32 {
        self.pll.get(&off).copied().unwrap_or(0) & !PASSWD
    }

    /// A PLL channel's output in Hz: `ctrl` and `frac` are the multiplier's
    /// words, `core` the channel taken from it. A PLL that is powered down, or
    /// a channel that is off, gives 0.
    fn pll_channel_hz(&self, ctrl: u32, frac: u32, core: u32) -> u64 {
        let (ctrl, frac, core) = (
            self.pll_word(ctrl),
            self.pll_word(frac),
            self.pll_word(core),
        );
        let ndiv = u64::from(ctrl & PLLA_CTRL_NDIV_MASK);
        let pdiv = u64::from((ctrl & PLLA_CTRL_PDIV_MASK) >> PLLA_CTRL_PDIV_SHIFT).max(1);
        let div = u64::from(core & PLLC_CORE2_DIV_MASK);
        let off = core & PLLC_CORE2_DISABLE_MASK != 0;
        if ctrl & PLLA_CTRL_PWRDN_MASK != 0 || off || div == 0 || ndiv == 0 {
            return 0;
        }
        let multiplier_q20 = (ndiv << 20) + u64::from(frac & 0xF_FFFF);
        (OSCILLATOR_HZ * multiplier_q20 / pdiv / div) >> 20
    }

    /// The clock the VPU runs from, as `VPUCTL` and `VPUDIV` and the PLL they
    /// select set it, in Hz. A source the model does not follow, or a PLL that
    /// is not running, leaves the VPU on the oscillator.
    pub fn vpu_hz(&self) -> u64 {
        let ctl = self.storage.get(&VPUCTL).copied().unwrap_or(0) & !PASSWD;
        let div = u64::from(self.storage.get(&VPUDIV).copied().unwrap_or(0) & 0xFF_FFFF);
        let pll = |ctrl, frac, core| self.pll_channel_hz(ctrl, frac, core) << 12;
        let src = match ctl & 0xF {
            1 => OSCILLATOR_HZ << 12,
            4 => pll(PLLA_CTRL, PLLA_FRAC, PLLA_CORE),
            5 => pll(PLLC_CTRL, PLLC_FRAC, PLLC_CORE0),
            6 => pll(PLLD_CTRL, PLLD_FRAC, PLLD_CORE),
            8 => pll(PLLC_CTRL, PLLC_FRAC, PLLC_CORE1),
            9 => pll(PLLC_CTRL, PLLC_FRAC, PLLC_CORE2),
            _ => 0,
        };
        match (src, div) {
            (0, _) | (_, 0) => OSCILLATOR_HZ,
            (src, div) => src / div,
        }
    }

    /// Whether a write since the last call may have moved [`Self::vpu_hz`].
    pub fn take_vpu_change(&mut self) -> bool {
        core::mem::take(&mut self.vpu_changed)
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
        if off == PLLB {
            self.arm_held = value & PLLB_HOLDARM_MASK != 0;
        }
        self.storage.insert(off, value);
        let canonical = if off >= PLL_COPY_BASE {
            off - PLL_COPY_OFFSET
        } else {
            off
        };
        if canonical >= A2W_BASE {
            self.pll.insert(canonical, value);
        }
        self.vpu_changed |= matches!(off, VPUCTL | VPUDIV) || canonical >= A2W_BASE;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holding_the_arm_channel_reads_back_and_is_reported() {
        let mut cm = ClockManager::new();
        assert!(!cm.arm_held());
        cm.write(PLLB, Width::Word, PASSWD | PLLB_HOLDARM_MASK)
            .unwrap();
        assert!(cm.arm_held());
        assert_eq!(cm.read(PLLB, Width::Word).unwrap(), PLLB_HOLDARM_MASK);
        cm.write(PLLB, Width::Word, PASSWD).unwrap();
        assert!(!cm.arm_held());
        assert_eq!(cm.read(PLLB, Width::Word).unwrap(), 0);
    }

    #[test]
    fn only_holdarm_holds_the_arm() {
        let mut cm = ClockManager::new();
        // `ANARST` with `LOADARM`, then with `HOLDARM`: the words start4 writes
        // while it brings PLLB up.
        cm.write(PLLB, Width::Word, PASSWD | 0x101).unwrap();
        assert!(!cm.arm_held());
        cm.write(PLLB, Width::Word, PASSWD | 0x102).unwrap();
        assert!(cm.arm_held());
    }
}
