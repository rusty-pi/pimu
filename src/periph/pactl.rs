//! `PACTL_CS` at `0x7E20_4E00` (#131).
//!
//! The BCM2711 has more peripherals than the VideoCore's interrupt controller
//! has lines, so the seven SPI masters share one, the eight I²C masters
//! another and the five PL011 UARTs a third (VC peripheral IRQs 54, 53 and 57
//! — VPU sources 118, 117 and 121, see `specs/corectl.toml`). This register is
//! how a driver finds out which member of a group raised the shared line.
//!
//! Nothing in the model raises any of them: [`super::spi0`], [`super::bsc`]
//! and [`super::uart_pl011`] are all polled by the firmware and none of them
//! drives its interrupt. So the honest answer is 0 — no peripheral has
//! anything pending — which is also what the catch-all stub answered. The
//! block is modelled anyway so that the address is decoded rather than
//! counted as a stub hit, and so the spec can say what the bits mean when one
//! of those devices does grow an interrupt.

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
        // Read-only: every bit follows a peripheral's own interrupt line.
        Ok(())
    }
}
