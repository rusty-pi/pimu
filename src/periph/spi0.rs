//! BCM2835/BCM2711 SPI0 master (`0x7E20_4000`) + attached serial-NOR flash.
//!
//! Registers and fields: `specs/spi0.toml`.
//!
//! The EEPROM bootloader drives this in polled single-byte-FIFO mode: for every
//! byte it waits for `CS.TXD`, writes the byte to `FIFO`, waits for `CS.RXD`,
//! then reads the miso byte back out of `FIFO`. `CS.TA` stays asserted for the
//! whole command; deasserting it ends the transaction.
//!
//! The master's pads are GPIO 40..43, which carry the flash only on ALT4 — a
//! 4B has PWM audio on 40/41 and the activity LED on 42 the rest of the time —
//! so every session moves the four pins there and back, and a transfer made
//! with them elsewhere clocks its bytes into the air and reads MISO idle-high
//! ([`crate::machine::Machine::route_gpio_pins`]).
//!
//! The attached serial-NOR flash is modelled far enough for the bootloader to
//! scan the `pieeprom.bin` image it was loaded from and to apply an EEPROM
//! self-update: `READ`, `FAST_READ`, `RDID`, `RDSR`, `WREN`, `SE` and `PP`.
//! Erase and program are instantaneous, so WIP always reads clear; everything
//! else returns `0xFF`.

use std::collections::VecDeque;

use crate::bus::{BusError, BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};

use crate::spec::spi0::{
    CLK, CS, CS_CLEAR_RX_MASK as CS_CLEAR_RX, CS_CLEAR_TX_MASK as CS_CLEAR_TX,
    CS_DONE_MASK as CS_DONE, CS_RXD_MASK as CS_RXD, CS_RXF_MASK as CS_RXF, CS_RXR_MASK as CS_RXR,
    CS_TA_MASK as CS_TA, CS_TXD_MASK as CS_TXD, DC, DLEN, FIFO, LTOH,
};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "spi0",
    decoded: &[CS, FIFO, CLK, DLEN, LTOH, DC],
};

const MISO_IDLE: u8 = 0xFF;

/// JEDEC id reported for `RDID` — Winbond W25Q128 (16 MiB), close enough to the
/// real Pi 4 boot flash that the bootloader is happy.
const JEDEC_ID: [u8; 3] = [0xEF, 0x40, 0x18];

#[derive(Default)]
pub struct Spi0 {
    cs: u32,
    clk: u32,
    dlen: u32,
    ltoh: u32,
    dc: u32,
    /// The attached flash image (the `pieeprom.bin` bytes). Empty ⇒ no flash.
    flash: Vec<u8>,
    beat: u64,
    cmd: u8,
    addr: u32,
    wel: bool,
    rx: VecDeque<u8>,
    /// `true` once anything wrote to `flash` — a signal to the run loop that an
    /// EEPROM self-update landed and a re-run from the new image is due.
    pub dirty: bool,
    /// Whether GPIO 40..43 are on ALT4, which is what puts the master's pads
    /// on the flash ([`crate::periph::gpio`]). Clear: the bytes go to pins
    /// that are somebody else's, and MISO reads idle-high.
    pins: bool,
    pub log: Log,
}

impl Spi0 {
    pub fn new() -> Spi0 {
        Spi0 {
            pins: true,
            ..Spi0::default()
        }
    }

    pub fn attach_flash(&mut self, image: Vec<u8>) {
        self.flash = image;
    }

    /// Say whether GPIO 40..43 carry the master (`ALT4`). The machine follows
    /// the pin functions and tells the device; a session with the pins
    /// elsewhere clocks bytes into the air.
    pub fn set_pins(&mut self, on: bool) {
        self.pins = on;
    }

    pub fn flash_bytes(&self) -> &[u8] {
        &self.flash
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
        // `CS.DONE` reflects the TX side only — "transfer complete, nothing
        // left to shift", cleared by writing more TX data or by `TA = 0`, and
        // unrelated to the RX FIFO. Every shift is instantaneous here, so with
        // `TA` asserted there is never a byte mid-flight. start4 spins on
        // `DONE` with no timeout as soon as its byte loop ends, so it must not
        // depend on the RX FIFO being drained.
        if self.cs & CS_TA != 0 {
            cs |= CS_DONE;
        }
        cs
    }

    fn begin(&mut self) {
        self.beat = 0;
        self.cmd = 0;
        self.addr = 0;
        self.rx.clear();
    }

    fn shift(&mut self, mosi: u8) {
        // The pads are not on the flash: nothing hears the byte, and MISO is
        // whatever holds the pin.
        if !self.pins {
            self.rx.push_back(MISO_IDLE);
            return;
        }
        let n = self.beat;
        self.beat += 1;

        let miso = match (n, self.cmd) {
            (0, _) => {
                self.cmd = mosi;
                match mosi {
                    0x06 => self.wel = true,  // WREN
                    0x04 => self.wel = false, // WRDI
                    _ => {}
                }
                MISO_IDLE
            }
            (1..=3, 0x03) => {
                self.addr = (self.addr << 8) | mosi as u32;
                MISO_IDLE
            }
            (4, 0x03) if self.log.on(Channel::Spi) => {
                crate::log!(self.log, Channel::Spi, "READ {:#08x}", self.addr);
                self.read_flash_byte()
            }
            (_, 0x03) => self.read_flash_byte(),
            (1..=3, 0x0B) => {
                self.addr = (self.addr << 8) | mosi as u32;
                MISO_IDLE
            }
            (4, 0x0B) => MISO_IDLE,
            (_, 0x0B) => self.read_flash_byte(),
            (1..=3, 0x9F) => JEDEC_ID[(n - 1) as usize],
            (_, 0x05) => u8::from(self.wel) << 1,
            (1..=3, 0x20) => {
                self.addr = (self.addr << 8) | mosi as u32;
                if n == 3 {
                    self.erase_sector();
                }
                MISO_IDLE
            }
            (1..=3, 0x02) => {
                self.addr = (self.addr << 8) | mosi as u32;
                MISO_IDLE
            }
            (_, 0x02) => {
                self.program_byte(mosi);
                MISO_IDLE
            }
            _ => MISO_IDLE,
        };
        self.rx.push_back(miso);
    }

    fn erase_sector(&mut self) {
        if !self.wel {
            return;
        }
        let base = (self.addr & !0xFFF) as usize;
        if let Some(sector) = self.flash.get_mut(base..base + 0x1000) {
            sector.fill(0xFF);
            self.dirty = true;
        }
        self.wel = false;
    }

    /// Program one byte (NOR: bits can only 1→0, so AND into place).
    fn program_byte(&mut self, b: u8) {
        if self.wel {
            if let Some(cell) = self.flash.get_mut(self.addr as usize) {
                *cell &= b;
                self.dirty = true;
            }
        }
        self.addr = self.addr.wrapping_add(1);
    }

    fn read_flash_byte(&mut self) -> u8 {
        let b = self
            .flash
            .get(self.addr as usize)
            .copied()
            .unwrap_or(MISO_IDLE);
        self.addr = self.addr.wrapping_add(1);
        b
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
                if !was_ta && value & CS_TA != 0 {
                    crate::log!(
                        self.log,
                        Channel::Spi,
                        "TA begin: CS={value:#x} (cs-select={})",
                        value & 3
                    );
                    self.begin();
                }
                if was_ta && value & CS_TA == 0 {
                    // Transaction ended: a page-program run completes here and
                    // clears the write-enable latch (erase clears it inline).
                    if self.cmd == 0x02 {
                        self.wel = false;
                    }
                }
            }
            FIFO => self.shift(value as u8),
            CLK => self.clk = value,
            DLEN => self.dlen = value,
            LTOH => self.ltoh = value,
            DC => self.dc = value,
            _ => {
                return Err(BusError::Unmapped {
                    addr: offset,
                    width: _width,
                    write: true,
                })
            }
        }
        Ok(())
    }
}
