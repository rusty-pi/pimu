//! AUX peripheral: mini-UART (transmit capture) + SPI master register stubs.
//!
//! The mini-UART at offset `0x40` is the default early console on many Pi
//! configurations. Like the PL011 model this is transmit-only with a
//! never-stalling line status.

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::aux::{
    ENABLES as AUX_ENABLES, IRQ as AUX_IRQ, MU_BAUD, MU_CNTL, MU_IER, MU_IIR, MU_IO, MU_LCR,
    MU_LSR, MU_LSR_TX_EMPTY_MASK as LSR_TX_EMPTY, MU_LSR_TX_IDLE_MASK as LSR_TX_IDLE, MU_MCR,
    MU_MSR, MU_SCRATCH, MU_STAT,
};
use crate::spec::Coverage;

/// Every register in `specs/aux.toml` is modelled; the SPI masters are not in
/// it.
pub const COVERAGE: Coverage = Coverage {
    block: "aux",
    decoded: &[
        AUX_IRQ,
        AUX_ENABLES,
        MU_IO,
        MU_IER,
        MU_IIR,
        MU_LCR,
        MU_MCR,
        MU_LSR,
        MU_MSR,
        MU_SCRATCH,
        MU_CNTL,
        MU_STAT,
        MU_BAUD,
    ],
};

#[derive(Default)]
pub struct Aux {
    pub out: Vec<u8>,
    enables: u32,
    ier: u32,
    lcr: u32,
    mcr: u32,
    scratch: u32,
    cntl: u32,
    baud: u32,
}

impl Aux {
    pub fn new() -> Aux {
        Aux::default()
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }
}

impl MmioDevice for Aux {
    fn name(&self) -> &'static str {
        "aux(mini-uart)"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset {
            AUX_IRQ => 0,
            AUX_ENABLES => self.enables,
            MU_IO => 0,
            MU_IER => self.ier,
            MU_IIR => 0xC1, // fifo enabled, no interrupt pending
            MU_LCR => self.lcr,
            MU_MCR => self.mcr,
            MU_LSR => LSR_TX_EMPTY | LSR_TX_IDLE, // always ready to send, never rx
            MU_MSR => 0,
            MU_SCRATCH => self.scratch,
            MU_CNTL => self.cntl,
            MU_STAT => LSR_TX_EMPTY | LSR_TX_IDLE,
            MU_BAUD => self.baud,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        match offset {
            AUX_ENABLES => self.enables = value & 0x7,
            MU_IO => self.out.push(value as u8),
            MU_IER => self.ier = value,
            MU_LCR => self.lcr = value,
            MU_MCR => self.mcr = value,
            MU_SCRATCH => self.scratch = value,
            MU_CNTL => self.cntl = value,
            MU_BAUD => self.baud = value & 0xFFFF,
            MU_IIR => {} // fifo clear requests: ignored
            _ => {}
        }
        Ok(())
    }
}
