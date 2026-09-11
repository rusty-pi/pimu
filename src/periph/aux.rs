//! AUX peripheral: mini-UART (transmit capture) + SPI master register stubs.
//!
//! The mini-UART at offset `0x40` is the default early console on many Pi
//! configurations. Like the PL011 model this is transmit-only with a
//! never-stalling line status.

use crate::bus::{BusResult, MmioDevice, Width};
use alloc::vec::Vec;

const AUX_IRQ: u32 = 0x00;
const AUX_ENABLES: u32 = 0x04;
const MU_IO: u32 = 0x40;
const MU_IER: u32 = 0x44;
const MU_IIR: u32 = 0x48;
const MU_LCR: u32 = 0x4C;
const MU_MCR: u32 = 0x50;
const MU_LSR: u32 = 0x54;
const MU_MSR: u32 = 0x58;
const MU_SCRATCH: u32 = 0x5C;
const MU_CNTL: u32 = 0x60;
const MU_STAT: u32 = 0x64;
const MU_BAUD: u32 = 0x68;

const LSR_RX_READY: u32 = 1 << 0;
const LSR_TX_EMPTY: u32 = 1 << 5;
const LSR_TX_IDLE: u32 = 1 << 6;

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
        core::mem::take(&mut self.out)
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

#[allow(dead_code)]
const _UNUSED: u32 = LSR_RX_READY;
