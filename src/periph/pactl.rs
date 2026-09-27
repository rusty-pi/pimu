//! `PACTL_CS` at `0x7E20_4E00`: which SPI, I²C or PL011 behind an ORed
//! interrupt line is the one asking.
//!
//! Registers and fields: `specs/pactl.toml` ([`crate::spec::pactl`]).
//!
//! [`super::spi0`] drives its line while Linux talks to the boot flash, so bit 0
//! answers it; the machine reads the master out as it decodes the address. The
//! [`super::bsc`] masters and the other [`super::uart_pl011`]s are polled by
//! the firmware and drive nothing, so their bits are 0.

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::pactl::CS;
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "pactl",
    decoded: &[CS],
};

#[derive(Default)]
pub struct Pactl {
    spi0: bool,
}

impl Pactl {
    pub fn new() -> Pactl {
        Pactl::default()
    }

    /// Bit 0: SPI0's own interrupt line, as the machine last saw it.
    pub fn set_spi0(&mut self, on: bool) {
        self.spi0 = on;
    }
}

impl MmioDevice for Pactl {
    fn name(&self) -> &'static str {
        "pactl"
    }

    fn read(&mut self, _offset: u32, _width: Width) -> BusResult<u32> {
        Ok(u32::from(self.spi0) << crate::spec::pactl::CS_SPI_SHIFT)
    }

    fn write(&mut self, _offset: u32, _width: Width, _value: u32) -> BusResult<()> {
        Ok(())
    }
}
