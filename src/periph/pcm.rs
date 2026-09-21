//! The PCM / I²S interface at `0x7E20_3000` (#131).
//!
//! One serial-audio module, which a HAT reaches on GPIO 18 to 21. Nothing on
//! a 4B is wired to it and no firmware in a boot touches it, so — as for
//! [`super::pwm`] — the model is the register map rather than an interface:
//! the control word is kept, both FIFOs are empty, and the interrupt and the
//! two DREQs never go up.
//!
//! What that gives a driver is the transmit path finishing rather than
//! blocking. `CS_A` reads with `TXE`, `TXD` and `TXW` set and `RXD`, `RXR` and
//! `RXF` clear, which is an interface that can always take another word and
//! never has one to hand back. On the catch-all stub the same read answered 0:
//! a transmit FIFO that is full and a receive FIFO that will never fill, which
//! is a spin either way a driver looks at it.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::pcm::{
    CS_A, CS_A_RESET, DREQ_A, FIFO_A, GRAY, INTEN_A, INTSTC_A, MODE_A, RXC_A, TXC_A,
};
use crate::spec::Coverage;

/// Every register of the block.
pub const COVERAGE: Coverage = Coverage {
    block: "pcm",
    decoded: &[
        CS_A, FIFO_A, MODE_A, RXC_A, TXC_A, DREQ_A, INTEN_A, INTSTC_A, GRAY,
    ],
};

/// What a write to `CS_A` keeps: everything but the read-only flags and the
/// two write-1-to-clear error bits, which `CS_A_RESET` supplies on read.
const CS_A_WRITABLE: u32 = 0x0138_03FF;

/// The two error flags, cleared by writing them back.
const CS_A_W1C: u32 = 0x0001_8000;

#[derive(Default)]
pub struct Pcm {
    storage: BTreeMap<u32, u32>,
    /// `CS_A`'s writable bits, and the error flags on top of them.
    cs: u32,
}

impl Pcm {
    pub fn new() -> Pcm {
        Pcm::default()
    }
}

impl MmioDevice for Pcm {
    fn name(&self) -> &'static str {
        "pcm"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        Ok(match off {
            // The control bits as written, with the flags of an idle interface
            // whose transmit FIFO is empty and whose receive FIFO never fills.
            CS_A => CS_A_RESET | self.cs,
            // Nothing was ever received.
            FIFO_A => 0,
            _ => self.storage.get(&off).copied().unwrap_or(0),
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        match off {
            CS_A => {
                // `TXCLR` and `RXCLR` are self-clearing, and there is nothing
                // in either FIFO to clear.
                self.cs = (self.cs & CS_A_W1C & !value) | (value & CS_A_WRITABLE & !0x18);
            }
            // The transmitted word goes nowhere: no pin carries it.
            FIFO_A => {}
            INTSTC_A => {
                let was = self.storage.get(&off).copied().unwrap_or(0);
                self.storage.insert(off, was & !value);
            }
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

    fn rd(p: &mut Pcm, off: u32) -> u32 {
        p.read(off, Width::Word).unwrap()
    }

    fn wr(p: &mut Pcm, off: u32, value: u32) {
        p.write(off, Width::Word, value).unwrap();
    }

    /// An untouched interface reads as ready to transmit and with nothing to
    /// receive — the flags, and nothing else.
    #[test]
    fn an_idle_interface_can_always_take_another_word() {
        let mut p = Pcm::new();
        assert_eq!(rd(&mut p, CS_A), 0x0028_0000);
        assert_eq!(rd(&mut p, FIFO_A), 0);
    }

    /// Enabling the interface keeps the control bits and leaves the flags
    /// alone; the two self-clearing FIFO-clear bits do not stick.
    #[test]
    fn the_control_bits_are_kept_and_the_clears_are_not() {
        let mut p = Pcm::new();
        wr(&mut p, CS_A, 0x1 | 0x4 | 0x8 | 0x10); // EN, TXON, TXCLR, RXCLR
        assert_eq!(rd(&mut p, CS_A), 0x0028_0005);
    }

    /// A driver that fills the transmit FIFO is never told it is full.
    #[test]
    fn transmitting_never_blocks() {
        let mut p = Pcm::new();
        wr(&mut p, CS_A, 0x5);
        for i in 0..128 {
            wr(&mut p, FIFO_A, i);
        }
        assert_eq!(rd(&mut p, CS_A) & 0x0028_0000, 0x0028_0000, "TXE and TXD");
    }

    /// The configuration words are plain storage, and the interrupt status is
    /// write-1-to-clear with nothing to clear.
    #[test]
    fn the_configuration_words_read_back() {
        let mut p = Pcm::new();
        wr(&mut p, MODE_A, 0x0102_0304);
        wr(&mut p, RXC_A, 0x8000_1234);
        wr(&mut p, TXC_A, 0x4000_5678);
        assert_eq!(rd(&mut p, MODE_A), 0x0102_0304);
        assert_eq!(rd(&mut p, RXC_A), 0x8000_1234);
        assert_eq!(rd(&mut p, TXC_A), 0x4000_5678);
        wr(&mut p, INTSTC_A, 0xF);
        assert_eq!(rd(&mut p, INTSTC_A), 0);
    }
}
