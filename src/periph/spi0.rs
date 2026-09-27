//! BCM2835/BCM2711 SPI0 master (`0x7E20_4000`) + attached serial-NOR flash.
//!
//! Registers and fields: `specs/spi0.toml`.
//!
//! The EEPROM bootloader drives this in polled single-byte-FIFO mode: for every
//! byte it waits for `CS.TXD`, writes the byte to `FIFO`, waits for `CS.RXD`,
//! then reads the miso byte back out of `FIFO`. `CS.TA` stays asserted for the
//! whole command; deasserting it ends the transaction.
//!
//! Linux drives the same master through `spi-bcm2835`, which is a different
//! shape in three ways ([`crate::machine::Machine::route_gpio_pins`], #161):
//!
//! 1. the chip select is a GPIO output, never the native one — the driver
//!    parks `CS.CS` at the invalid `0b11` on purpose — so the flash follows
//!    GPIO 43 and not `CS.TA`, which the driver raises and drops once per
//!    transfer while a message holds the select down across several;
//! 2. a transfer of 96 bytes or more runs off the legacy DMA, and `CS.DMAEN`
//!    makes `FIFO` 32 bits wide: four bytes a word, low byte first, for the
//!    `DLEN` bytes the transfer is long and no more;
//! 3. anything in between runs off `CS.INTD` / `CS.INTR`, so the master has to
//!    drive its interrupt line.
//!
//! The master's pads are GPIO 40..42 (miso, mosi, sclk) and the flash's select
//! GPIO 43, and only ALT4 puts the first three on the flash — a 4B has PWM
//! audio on 40/41 and the activity LED on 42 the rest of the time — so every
//! session moves the pins there and back, and a transfer made with them
//! elsewhere clocks its bytes into the air and reads MISO idle-high.
//!
//! The attached serial-NOR flash is modelled far enough for the bootloader to
//! scan the `pieeprom.bin` image it was loaded from, for an EEPROM self-update,
//! and for the probe and the full write `flashrom` does over `/dev/spidev0.0`:
//! `READ`, `FAST_READ`, `RDID`, `REMS`, `RES`, `RDSR`, `WRSR`, `WREN` / `WRDI`,
//! `PP`, the three erases and chip erase. Erase and program are instantaneous,
//! so `WIP` always reads clear; everything else returns `0xFF`, which is what a
//! real part leaves on a miso nobody is driving.

use std::collections::VecDeque;

use crate::bus::{BusError, BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};

use crate::spec::spi0::{
    CLK, CS, CS_CLEAR_RX_MASK as CS_CLEAR_RX, CS_CLEAR_TX_MASK as CS_CLEAR_TX,
    CS_CS_MASK as CS_SELECT, CS_DMAEN_MASK as CS_DMAEN, CS_DONE_MASK as CS_DONE,
    CS_INTD_MASK as CS_INTD, CS_INTR_MASK as CS_INTR, CS_RXD_MASK as CS_RXD, CS_RXF_MASK as CS_RXF,
    CS_RXR_MASK as CS_RXR, CS_TA_MASK as CS_TA, CS_TXD_MASK as CS_TXD, DC, DLEN, FIFO, LTOH,
};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "spi0",
    decoded: &[CS, FIFO, CLK, DLEN, LTOH, DC],
};

const MISO_IDLE: u8 = 0xFF;

/// The receive FIFO: 16 words, and `RXR` is the three-quarter mark.
const FIFO_BYTES: usize = 64;
const FIFO_BYTES_3_4: usize = 48;

/// JEDEC id reported for `RDID`: Winbond W25X40, the 512 KiB part a Raspberry
/// Pi 4B d03115 carries and the one `flashrom` names in
/// `flashrom -p linux_spi:dev=/dev/spidev0.0 --flash-name`.
const JEDEC_ID: [u8; 3] = [0xEF, 0x30, 0x13];
/// `RES` / `REMS` answer the same part's one-byte electronic id.
const ELECTRONIC_ID: u8 = 0x12;

const READ: u8 = 0x03;
const FAST_READ: u8 = 0x0B;
const RDID: u8 = 0x9F;
const REMS: u8 = 0x90;
const RES: u8 = 0xAB;
const RDSR: u8 = 0x05;
const WRSR: u8 = 0x01;
const WREN: u8 = 0x06;
const WRDI: u8 = 0x04;
const PP: u8 = 0x02;
const SE: u8 = 0x20;
const BE32: u8 = 0x52;
const BE64: u8 = 0xD8;
const CE: u8 = 0x60;
const CE_ALT: u8 = 0xC7;

/// Which of the flash's commands end with the write-enable latch spent.
const WRITE_CMDS: [u8; 7] = [PP, SE, BE32, BE64, CE, CE_ALT, WRSR];

/// Where the flash's chip select comes from, which is what GPIO 43 is set to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ChipSelect {
    /// GPIO 43 is somebody else's, or an input: nothing selects the flash.
    #[default]
    Off,
    /// GPIO 43 on ALT4, the master's own `CE0`: the select follows `CS.TA`.
    Native,
    /// GPIO 43 driven as an output, active low — how Linux's `spi-bcm2835`
    /// works the select, whatever `CS.TA` is doing.
    Gpio(bool),
}

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
    /// `WRSR`'s bits 7:2 — the block-protect latches. Nothing here enforces
    /// them; `flashrom` reads them to decide whether it has to unlock first.
    sr: u8,
    rx: VecDeque<u8>,
    /// `true` once anything wrote to `flash` — a signal to the run loop that an
    /// EEPROM self-update landed and a re-run from the new image is due.
    pub dirty: bool,
    /// Whether GPIO 40..42 are on ALT4, which is what puts the master's pads
    /// on the flash ([`crate::periph::gpio`]). Clear: the bytes go to pins
    /// that are somebody else's, and MISO reads idle-high.
    pins: bool,
    cs_src: ChipSelect,
    /// Whether the flash's select is down, so a command is in flight.
    selected: bool,
    /// What `DLEN` still allows a `CS.DMAEN` transfer to shift. The DMA writes
    /// whole words either way, so the last one of an odd length is partial and
    /// a cyclic filler descriptor keeps writing long after the transfer's end.
    dma_left: u32,
    pub log: Log,
}

impl Spi0 {
    pub fn new() -> Spi0 {
        Spi0 {
            pins: true,
            cs_src: ChipSelect::Native,
            ..Spi0::default()
        }
    }

    pub fn attach_flash(&mut self, image: Vec<u8>) {
        self.flash = image;
    }

    /// Say where the master's pads and the flash's select are: `pins` is GPIO
    /// 40..42 on ALT4, `cs` what GPIO 43 is. The machine follows the pin
    /// functions and their levels and tells the device; a session with the pins
    /// elsewhere clocks bytes into the air.
    pub fn set_bus(&mut self, pins: bool, cs: ChipSelect) {
        self.pins = pins;
        self.cs_src = cs;
        self.settle();
    }

    pub fn flash_bytes(&self) -> &[u8] {
        &self.flash
    }

    /// The line into `PACTL_CS` bit 0 and GIC 150: `DONE` under `INTD`, or the
    /// receive FIFO past its three-quarter mark under `INTR`.
    pub fn irq_line(&self) -> bool {
        let cs = self.status();
        (cs & CS_INTD != 0 && cs & CS_DONE != 0) || (cs & CS_INTR != 0 && cs & CS_RXR != 0)
    }

    fn status(&self) -> u32 {
        let mut cs = self.cs & !(CS_DONE | CS_RXD | CS_TXD | CS_RXR | CS_RXF);
        cs |= CS_TXD; // always room to write
        if !self.rx.is_empty() {
            cs |= CS_RXD;
        }
        if self.rx.len() >= FIFO_BYTES_3_4 {
            cs |= CS_RXR;
        }
        if self.rx.len() >= FIFO_BYTES {
            cs |= CS_RXF;
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

    /// Whether the flash's select is down: the pins have to carry the master,
    /// and GPIO 43 has to say so.
    fn asserted(&self) -> bool {
        self.pins
            && match self.cs_src {
                ChipSelect::Off => false,
                // The firmware's own sessions: `CS.TA` frames the command, and
                // `CS.CS` picks a select the master really has.
                ChipSelect::Native => self.cs & CS_TA != 0 && self.cs & CS_SELECT != CS_SELECT,
                ChipSelect::Gpio(level) => !level,
            }
    }

    /// Follow the select after anything that can move it.
    fn settle(&mut self) {
        let now = self.asserted();
        if now == self.selected {
            return;
        }
        self.selected = now;
        if now {
            crate::log!(
                self.log,
                Channel::Spi,
                "select: CS={:#x} src={:?}",
                self.cs,
                self.cs_src
            );
            self.beat = 0;
            self.cmd = 0;
            self.addr = 0;
            self.rx.clear();
        } else if WRITE_CMDS.contains(&self.cmd) {
            // A write command takes effect as the select goes back up, and
            // spends the write-enable latch with it.
            self.wel = false;
        }
    }

    fn shift(&mut self, mosi: u8) {
        // The flash is not listening: nothing hears the byte, and MISO is
        // whatever holds the pin.
        if !self.selected {
            self.rx.push_back(MISO_IDLE);
            return;
        }
        let n = self.beat;
        self.beat += 1;

        let miso = match (n, self.cmd) {
            (0, _) => {
                self.cmd = mosi;
                match mosi {
                    WREN => self.wel = true,
                    WRDI => self.wel = false,
                    CE | CE_ALT => self.erase(0, self.flash.len()),
                    _ => {}
                }
                MISO_IDLE
            }
            (1..=3, READ) => {
                self.addr = (self.addr << 8) | mosi as u32;
                MISO_IDLE
            }
            (4, READ) if self.log.on(Channel::Spi) => {
                crate::log!(self.log, Channel::Spi, "READ {:#08x}", self.addr);
                self.read_flash_byte()
            }
            (_, READ) => self.read_flash_byte(),
            (1..=3, FAST_READ) => {
                self.addr = (self.addr << 8) | mosi as u32;
                MISO_IDLE
            }
            (4, FAST_READ) => MISO_IDLE, // the dummy byte
            (_, FAST_READ) => self.read_flash_byte(),
            (1..=3, RDID) => JEDEC_ID[(n - 1) as usize],
            // `REMS` takes two dummy bytes and an address whose bit 0 says
            // which of the two ids comes first; `RES` takes three dummies.
            (1..=3, REMS) => {
                self.addr = mosi as u32;
                MISO_IDLE
            }
            (_, REMS) if (n - 4 + self.addr as u64).is_multiple_of(2) => JEDEC_ID[0],
            (_, REMS) => ELECTRONIC_ID,
            (4.., RES) => ELECTRONIC_ID,
            (_, RDSR) => (self.sr & !3) | (u8::from(self.wel) << 1),
            (1, WRSR) => {
                if self.wel {
                    self.sr = mosi;
                }
                MISO_IDLE
            }
            (1..=3, SE | BE32 | BE64) => {
                self.addr = (self.addr << 8) | mosi as u32;
                if n == 3 {
                    let size = match self.cmd {
                        SE => 0x1000,
                        BE32 => 0x8000,
                        _ => 0x10000,
                    };
                    self.erase(self.addr as usize & !(size - 1), size);
                }
                MISO_IDLE
            }
            (1..=3, PP) => {
                self.addr = (self.addr << 8) | mosi as u32;
                MISO_IDLE
            }
            (_, PP) => {
                self.program_byte(mosi);
                MISO_IDLE
            }
            _ => MISO_IDLE,
        };
        self.rx.push_back(miso);
    }

    fn erase(&mut self, base: usize, size: usize) {
        if !self.wel {
            return;
        }
        if let Some(block) = self.flash.get_mut(base..base + size) {
            block.fill(0xFF);
            self.dirty = true;
        }
    }

    /// Program one byte (NOR: bits can only 1→0, so AND into place). The
    /// address wraps inside the 256-byte page, as the part does.
    fn program_byte(&mut self, b: u8) {
        if self.wel {
            if let Some(cell) = self.flash.get_mut(self.addr as usize) {
                *cell &= b;
                self.dirty = true;
            }
        }
        self.addr = (self.addr & !0xFF) | (self.addr.wrapping_add(1) & 0xFF);
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

    /// One `FIFO` access is four bytes wide while `CS.DMAEN` is set, low byte
    /// first, and `DLEN` is what says where the transfer ends.
    fn shift_word(&mut self, value: u32) {
        let n = self.dma_left.min(4);
        self.dma_left -= n;
        for i in 0..n {
            self.shift((value >> (8 * i)) as u8);
        }
    }

    fn take_word(&mut self) -> u32 {
        let mut w = 0;
        for i in 0..4 {
            w |= (self.rx.pop_front().unwrap_or(MISO_IDLE) as u32) << (8 * i);
        }
        w
    }
}

impl MmioDevice for Spi0 {
    fn name(&self) -> &'static str {
        "spi0"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset & !3 {
            CS => self.status(),
            FIFO if self.cs & CS_DMAEN != 0 => self.take_word(),
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
                        value & CS_SELECT
                    );
                    // A DMA transfer shifts `DLEN` bytes from here.
                    if value & CS_DMAEN != 0 {
                        self.dma_left = self.dlen;
                    }
                }
                self.settle();
            }
            FIFO if self.cs & CS_DMAEN != 0 => self.shift_word(value),
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
