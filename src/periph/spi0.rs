//! BCM2835/BCM2711 SPI0 master (`0x7E20_4000`) + attached serial-NOR flash.
//!
//! The EEPROM bootloader drives this in polled single-byte-FIFO mode: for every
//! byte it waits for `CS.TXD`, writes the byte to `FIFO`, waits for `CS.RXD`,
//! then reads the miso byte back out of `FIFO`. `CS.TA` stays asserted for the
//! whole command; deasserting it ends the transaction.
//!
//! We model enough of a serial-NOR flash for the bootloader to scan the
//! `pieeprom.bin` image it was itself loaded from and to apply an EEPROM
//! self-update: `READ` (0x03) / `FAST_READ` (0x0B) stream image bytes, `RDID`
//! (0x9F) returns a JEDEC id, `RDSR` (0x05) reports the WIP bit, `WREN` (0x06)
//! sets the write-enable latch, `SE` (0x20) erases a 4 KiB sector to `0xFF`,
//! `PP` (0x02) programs up to a page. Erase/program are instantaneous in the
//! model (WIP always reads clear). Everything else returns `0xFF`.

use std::collections::VecDeque;

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

/// Byte returned when nothing better applies (MISO idle-high).
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
    /// Bytes clocked since `CS.TA` was asserted.
    beat: u64,
    /// Command byte (first beat of the transaction).
    cmd: u8,
    /// Address accumulator for read/erase/program commands.
    addr: u32,
    /// Write-enable latch (set by `WREN`, cleared after an erase / program).
    wel: bool,
    /// Response bytes queued for the CPU to read back out of the FIFO.
    rx: VecDeque<u8>,
    /// `true` once anything wrote to `flash` — a signal to the run loop that an
    /// EEPROM self-update landed and a re-run from the new image is due.
    pub dirty: bool,
}

impl Spi0 {
    pub fn new() -> Spi0 {
        Spi0::default()
    }

    /// Attach the serial-NOR flash contents (the EEPROM image).
    pub fn attach_flash(&mut self, image: Vec<u8>) {
        self.flash = image;
    }

    /// The current flash contents — reflects any EEPROM self-update writes.
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
        // With TA asserted and no byte mid-flight, the controller reports DONE
        // (nothing left to shift). The bootloader polls this right after
        // asserting TA and again at the end of the command.
        if self.cs & CS_TA != 0 && self.rx.is_empty() {
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

    /// Clock one byte out (`mosi`) and one byte in (`miso`).
    fn shift(&mut self, mosi: u8) {
        let n = self.beat;
        self.beat += 1;

        let miso = match (n, self.cmd) {
            (0, _) => {
                self.cmd = mosi;
                // Single-byte commands act now — there is no later beat.
                match mosi {
                    0x06 => self.wel = true,  // WREN
                    0x04 => self.wel = false, // WRDI
                    _ => {}
                }
                MISO_IDLE
            }
            // READ (0x03): 3 address bytes, then a stream of data.
            (1..=3, 0x03) => {
                self.addr = (self.addr << 8) | mosi as u32;
                MISO_IDLE
            }
            (4, 0x03) if std::env::var_os("RVF_DBG_SPI").is_some() => {
                eprintln!("[spi0] READ {:#08x}", self.addr);
                self.read_flash_byte()
            }
            (_, 0x03) => self.read_flash_byte(),
            // FAST_READ (0x0B): 3 address bytes + 1 dummy, then data.
            (1..=3, 0x0B) => {
                self.addr = (self.addr << 8) | mosi as u32;
                MISO_IDLE
            }
            (4, 0x0B) => MISO_IDLE,
            (_, 0x0B) => self.read_flash_byte(),
            // RDID (0x9F): three id bytes then 0xFF.
            (1..=3, 0x9F) => JEDEC_ID[(n - 1) as usize],
            // RDSR (0x05): status register. Bit 0 = WIP (always clear — erase /
            // program complete instantly); bit 1 = WEL.
            (_, 0x05) => u8::from(self.wel) << 1,
            // SE (0x20): 3 address bytes, then erase the enclosing 4 KiB sector.
            (1..=3, 0x20) => {
                self.addr = (self.addr << 8) | mosi as u32;
                if n == 3 {
                    self.erase_sector();
                }
                MISO_IDLE
            }
            // PP (0x02): 3 address bytes, then a stream of data bytes to program.
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

    /// Erase the 4 KiB sector containing `self.addr` to all-`0xFF`.
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
        let b = self.flash.get(self.addr as usize).copied().unwrap_or(MISO_IDLE);
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
                    if std::env::var_os("RVF_DBG_SPI").is_some() {
                        eprintln!("[spi0] TA begin: CS={value:#x} (cs-select={})", value & 3);
                    }
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
            _ => return Err(BusError::Unmapped { addr: offset, width: _width, write: true }),
        }
        Ok(())
    }
}
