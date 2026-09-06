//! PL011 UART (`UART0`), modelled well enough to be a firmware debug console.
//!
//! Transmit only: bytes written to `DR` are appended to an output buffer that the
//! harness drains. The flag register always reports "ready to transmit, FIFO
//! empty, not busy", so polling firmware never stalls. Receive returns 0.

use crate::bus::{BusResult, MmioDevice, Width};

// Register offsets.
const DR: u32 = 0x00;
const FR: u32 = 0x18;
const IBRD: u32 = 0x24;
const FBRD: u32 = 0x28;
const LCRH: u32 = 0x2C;
const CR: u32 = 0x30;
const IMSC: u32 = 0x38;
const RIS: u32 = 0x3C;
const MIS: u32 = 0x40;
const ICR: u32 = 0x44;

// FR bits.
const FR_BUSY: u32 = 1 << 3;
const FR_RXFE: u32 = 1 << 4;
const FR_TXFF: u32 = 1 << 5;
const FR_RXFF: u32 = 1 << 6;
const FR_TXFE: u32 = 1 << 7;

#[derive(Default)]
pub struct Pl011 {
    pub out: Vec<u8>,
    ibrd: u32,
    fbrd: u32,
    lcrh: u32,
    cr: u32,
    imsc: u32,
}

impl Pl011 {
    pub fn new() -> Pl011 {
        Pl011 {
            cr: 0x0301,
            ..Default::default()
        } // UARTEN|TXE|RXE at reset-ish
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }
}

impl MmioDevice for Pl011 {
    fn name(&self) -> &'static str {
        "uart0(pl011)"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset {
            DR => 0,
            FR => FR_TXFE | FR_RXFE, // tx empty, rx empty, never busy/full
            IBRD => self.ibrd,
            FBRD => self.fbrd,
            LCRH => self.lcrh,
            CR => self.cr,
            IMSC => self.imsc,
            RIS | MIS => 0,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        match offset {
            DR => self.out.push(value as u8),
            IBRD => self.ibrd = value & 0xFFFF,
            FBRD => self.fbrd = value & 0x3F,
            LCRH => self.lcrh = value,
            CR => self.cr = value,
            IMSC => self.imsc = value,
            ICR => {}
            _ => {}
        }
        Ok(())
    }
}

#[allow(dead_code)]
const _UNUSED_FLAGS: u32 = FR_BUSY | FR_TXFF | FR_RXFF;
