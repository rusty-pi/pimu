//! `PACTL_CS` at `0x7E20_4E00`: which SPI, I²C or PL011 behind an ORed
//! interrupt line is the one asking.
//!
//! Registers and fields: `specs/pactl.toml` ([`crate::spec::pactl`]).
//!
//! Nothing in the model raises any of those lines — [`super::spi0`],
//! [`super::bsc`] and [`super::uart_pl011`] are all polled by the firmware and
//! none of them drives its interrupt — so the honest answer is 0. The block is
//! modelled anyway so that the address is decoded rather than counted as a stub
//! hit, and so the spec can say what the bits mean once one of those devices
//! does grow an interrupt.

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::pactl::CS;
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "pactl",
    decoded: &[CS],
};

#[derive(Default)]
pub struct Pactl;

impl Pactl {
    pub fn new() -> Pactl {
        Pactl
    }
}

impl MmioDevice for Pactl {
    fn name(&self) -> &'static str {
        "pactl"
    }

    fn read(&mut self, _offset: u32, _width: Width) -> BusResult<u32> {
        Ok(0)
    }

    fn write(&mut self, _offset: u32, _width: Width, _value: u32) -> BusResult<()> {
        Ok(())
    }
}
