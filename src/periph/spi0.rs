//! Minimal BCM2835/BCM2711 SPI0 master (`0x7E20_4000`).
//!
//! The EEPROM bootloader drives this to talk to an SPI device and polls the
//! `CS` status bits (`DONE` / `RXD` / `TXD`). We model just enough for those
//! polls to make progress: every byte written to the FIFO immediately produces
//! a response byte in the RX FIFO (MISO idle-high, so `0xFF`), `DONE` sets once
//! `DLEN` bytes have moved, and the FIFOs never block.
//!
//! This is not a real SPI flash — data read back is `0xFF`. Enough to get the
//! bootloader past the transfer loop; a real flash model comes later.

use crate::bus::{BusError, BusResult, MmioDevice, Width};

const CS: u32 = 0x00;
const FIFO: u32 = 0x04;
const CLK: u32 = 0x08;
const DLEN: u32 = 0x0C;
const LTOH: u32 = 0x10;
const DC: u32 = 0x14;

// CS register bits.
const CS_TA: u32 = 1 << 7; // transfer active
const CS_CLEAR_RX: u32 = 1 << 5;
const CS_CLEAR_TX: u32 = 1 << 4;
const CS_DONE: u32 = 1 << 16;
const CS_RXD: u32 = 1 << 17; // RX FIFO contains data
const CS_TXD: u32 = 1 << 18; // TX FIFO has space
const CS_RXR: u32 = 1 << 19; // RX FIFO ¾ full
const CS_RXF: u32 = 1 << 20; // RX FIFO full

/// Byte returned for every read (MISO idle-high — an unconnected/again bus).
const MISO_IDLE: u8 = 0xFF;

#[derive(Default)]
pub struct Spi0 {
    cs: u32,
    clk: u32,
    dlen: u32,
    ltoh: u32,
    dc: u32,
    /// Bytes still expected to move in the current transfer.
    remaining: u32,
    /// Response bytes queued for the CPU to read out of the FIFO.
    rx: std::collections::VecDeque<u8>,
}

impl Spi0 {
    pub fn new() -> Spi0 {
        Spi0::default()
    }

    fn status(&self) -> u32 {
        let mut cs = self.cs & !(CS_DONE | CS_RXD | CS_TXD | CS_RXR | CS_RXF);
        cs |= CS_TXD; // always room to write
        if !self.rx.is_empty() {
            cs |= CS_RXD;
        }
        if self.rx.len() >= 16 {
            cs |= CS_RXR | CS_RXF;
        }
        // DONE once the active transfer has moved every DLEN byte and the CPU
        // isn't mid-drain.
        if self.cs & CS_TA != 0 && self.remaining == 0 {
            cs |= CS_DONE;
        }
        cs
    }

    fn begin_transfer(&mut self) {
        self.remaining = self.dlen.max(if self.cs & CS_TA != 0 { 1 } else { 0 });
        self.rx.clear();
    }

    fn push_tx(&mut self, _byte: u8) {
        // Every clocked-out byte clocks a response byte in.
        self.rx.push_back(MISO_IDLE);
        if self.remaining > 0 {
            self.remaining -= 1;
        }
    }
}

impl MmioDevice for Spi0 {
    fn name(&self) -> &'static str {
        "spi0"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset & !3 {
            CS => self.status(),
            FIFO => self.rx.pop_front().unwrap_or(MISO_IDLE) as u32,
            CLK => self.clk,
            DLEN => self.dlen,
            LTOH => self.ltoh,
            DC => self.dc,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        match offset & !3 {
            CS => {
                let was_ta = self.cs & CS_TA != 0;
                self.cs = value & !(CS_CLEAR_RX | CS_CLEAR_TX);
                if value & CS_CLEAR_RX != 0 {
                    self.rx.clear();
                }
                if value & CS_CLEAR_TX != 0 {
                    self.remaining = 0;
                }
                if !was_ta && value & CS_TA != 0 {
                    self.begin_transfer();
                }
            }
            FIFO => self.push_tx(value as u8),
            CLK => self.clk = value,
            DLEN => self.dlen = value,
            LTOH => self.ltoh = value,
            DC => self.dc = value,
            _ => return Err(BusError::Unmapped { addr: offset, width: _width, write: true }),
        }
        Ok(())
    }
}
