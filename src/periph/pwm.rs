//! The two PWM blocks, `0x7E20_C000` and `0x7E20_C800`.
//!
//! Registers and fields: `specs/pwm.toml` ([`crate::spec::pwm`]).
//!
//! Each has two channels that either pulse-width modulate a value or shift a
//! word out bit by bit, fed from `DAT1` / `DAT2` or from the FIFO they share.
//! On a 4B, PWM0's channels are the analogue audio pins, GPIO 40 and 41 on
//! ALT0 — the same pads the SPI NOR flash uses on ALT4 ([`super::spi0`]), which
//! is why the firmware keeps moving `GPFSEL4` around.
//!
//! No firmware in a boot programs either block, and Linux plays its analogue
//! audio through the firmware rather than here, so there is nothing to imitate
//! and nothing measured to imitate it from. What the model gives a driver is
//! the register map, the reset values the datasheet lists, and a FIFO that is
//! always empty: a write to `FIF1` goes nowhere, `STA` answers `EMPT1`, and no
//! channel ever runs — `STA.STA1` and `STA.STA2` stay clear, so a driver that
//! waits for a transmission to finish is told it already has. Answering 0
//! everywhere instead would read `EMPT1` clear — a FIFO that is neither empty
//! nor draining — and `RNG1` 0 rather than the 32 the block powers up with.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::pwm::{
    CTL, DAT1, DAT1_RESET, DAT2, DAT2_RESET, DMAC, DMAC_RESET, FIF1, RNG1, RNG1_RESET, RNG2,
    RNG2_RESET, STA, STA_RESET,
};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "pwm",
    decoded: &[CTL, STA, DMAC, RNG1, DAT1, FIF1, RNG2, DAT2],
};

const STA_W1C: u32 = 0x13C;

pub struct Pwm {
    name: &'static str,
    storage: BTreeMap<u32, u32>,
    sta: u32,
}

impl Pwm {
    pub fn new(name: &'static str) -> Pwm {
        Pwm {
            name,
            storage: BTreeMap::from([
                (DMAC, DMAC_RESET),
                (RNG1, RNG1_RESET),
                (DAT1, DAT1_RESET),
                (RNG2, RNG2_RESET),
                (DAT2, DAT2_RESET),
            ]),
            sta: 0,
        }
    }
}

impl MmioDevice for Pwm {
    fn name(&self) -> &'static str {
        self.name
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        Ok(match off {
            STA => STA_RESET | self.sta,
            FIF1 => 0,
            _ => self.storage.get(&off).copied().unwrap_or(0),
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        match off {
            STA => self.sta &= !(value & STA_W1C),
            // The FIFO is never read back, so the word goes nowhere. A real
            // block would raise `STA.WERR1` once 16 words are in it.
            FIF1 => {}
            _ => {
                self.storage.insert(off, value);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pwm() -> Pwm {
        Pwm::new("pwm0")
    }

    fn rd(p: &mut Pwm, off: u32) -> u32 {
        p.read(off, Width::Word).unwrap()
    }

    fn wr(p: &mut Pwm, off: u32, value: u32) {
        p.write(off, Width::Word, value).unwrap();
    }

    #[test]
    fn the_block_powers_up_the_way_the_datasheet_says() {
        let mut p = pwm();
        assert_eq!(rd(&mut p, RNG1), 0x20);
        assert_eq!(rd(&mut p, RNG2), 0x20);
        assert_eq!(rd(&mut p, DMAC), 0x707);
        assert_eq!(rd(&mut p, CTL), 0);
        assert_eq!(rd(&mut p, STA), 0x2, "EMPT1, and no channel running");
    }

    #[test]
    fn the_fifo_takes_what_it_is_given_and_stays_empty() {
        let mut p = pwm();
        wr(&mut p, CTL, 0x81); // channel 1 on, mark/space
        for i in 0..64 {
            wr(&mut p, FIF1, i);
        }
        assert_eq!(rd(&mut p, STA), 0x2);
        assert_eq!(rd(&mut p, FIF1), 0);
        assert_eq!(rd(&mut p, CTL), 0x81, "the control word is kept");
    }

    #[test]
    fn a_status_write_clears_rather_than_sets() {
        let mut p = pwm();
        wr(&mut p, STA, 0xFFFF_FFFF);
        assert_eq!(rd(&mut p, STA), 0x2);
    }
}
