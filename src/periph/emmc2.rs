//! BCM2711 EMMC2 — the SD Host Controller (SDHCI v3) the main bootloader drives
//! once it picks "Boot mode: SD". Register block at `0x7E34_0000`.
//!
//! Models the clock / reset / present-state plumbing *and* a working command +
//! PIO-data engine wired to an [`SdCard`]. The bootloader enumerates the card
//! (CMD0/8/55/ACMD41/2/3/9/7/ACMD51/CMD6), then reads blocks with CMD18/CMD12
//! through the Buffer Data Port — enough to parse the MBR, mount the FAT32 boot
//! partition and pull `start4.elf`.
//!
//! SDHCI register map (word offsets):
//! ```text
//!   0x00 SDMA address / arg2      0x04 block size[11:0] | block count[31:16]
//!   0x08 argument                 0x0C transfer mode[15:0] | command[31:16]
//!   0x10..0x1C RESPONSE0..3       0x20 buffer data port
//!   0x24 present state            0x28 host/power/gap/wakeup control
//!   0x2C clock ctl[15:0] | timeout[23:16] | sw-reset[26:24]
//!   0x30 int status               0x34 int status enable   0x38 int signal enable
//!   0x40/0x44 capabilities        0xFC controller version
//! ```
//!
//! Only PIO reads are implemented (no SDMA / ADMA, no writes) — that is all the
//! bootloader uses for the SD path.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::periph::sdcard::{SdCard, SCR_MAGIC_LBA, SWITCH_FUNC_MAGIC_LBA};

// SDHCI register offsets (byte), each a 32-bit word.
const SDMA_ADDR: u32 = 0x00;
const BLOCK_SIZE_COUNT: u32 = 0x04;
const ARGUMENT: u32 = 0x08;
const CMD_XFER: u32 = 0x0C;
const RESPONSE0: u32 = 0x10;
const RESPONSE1: u32 = 0x14;
const RESPONSE2: u32 = 0x18;
const RESPONSE3: u32 = 0x1C;
const BUFFER_DATA: u32 = 0x20;
const PRESENT_STATE: u32 = 0x24;
const HOST_CONTROL: u32 = 0x28;
const CLOCK_CONTROL: u32 = 0x2C;
const INT_STATUS: u32 = 0x30;
const INT_STATUS_EN: u32 = 0x34;
const INT_SIGNAL_EN: u32 = 0x38;
const CAPABILITIES_0: u32 = 0x40;
const CAPABILITIES_1: u32 = 0x44;
const CONTROLLER_VERSION: u32 = 0xFC;

/// CLOCK_CONTROL (low 16 bits of `0x2C`): internal-clock enable / stable.
const CLK_INTLEN: u32 = 1 << 0;
const CLK_STABLE: u32 = 1 << 1;
/// Software-reset bits (`0x2C` bits 24..26) — self-clearing in the model.
const SRST_MASK: u32 = 0x0700_0000;
const SRST_ALL: u32 = 1 << 24;
const SRST_CMD: u32 = 1 << 25;
const SRST_DATA: u32 = 1 << 26;

/// PRESENT_STATE: a stable, inserted, idle card, no command inhibit.
const PRESENT_STATE_IDLE: u32 = (1 << 16) | (1 << 17) | (1 << 20) | (1 << 24);
/// PRESENT_STATE bit 11: Buffer Read Enable (data available in the FIFO).
const PS_BUF_READ_EN: u32 = 1 << 11;

/// INT_STATUS (Normal Interrupt Status, low 16 bits of `0x30`).
const INT_CMD_COMPLETE: u32 = 1 << 0;
const INT_XFER_COMPLETE: u32 = 1 << 1;
const INT_BUF_WRITE_RDY: u32 = 1 << 4;
const INT_BUF_READ_RDY: u32 = 1 << 5;
/// Data-circuit interrupt bits cleared by a DAT software reset (spec: buffer
/// ready both ways, DMA, block-gap, transfer complete) — command complete is
/// explicitly preserved.
const INT_DATA_BITS: u32 = INT_XFER_COMPLETE | (1 << 3) | INT_BUF_WRITE_RDY | INT_BUF_READ_RDY;

pub struct Emmc2 {
    /// Sticky storage for offsets without special behaviour.
    reg: BTreeMap<u32, u32>,
    /// The inserted card, if any. `None` ⇒ commands still "complete" but every
    /// data read returns zeros (keeps the driver from hanging).
    card: Option<SdCard>,
    /// Latched response words the RESPONSE0..3 registers expose.
    resp: [u32; 4],
    /// PIO read buffer: bytes of the current block not yet read out via the
    /// Buffer Data Port. Drained 32 bits at a time, LSB-first.
    data: Vec<u8>,
    data_pos: usize,
    /// Next block address to pull from the card.
    read_lba: u32,
    /// Blocks still owed on a counted transfer (CMD17, or CMD18 with a non-zero
    /// block count).
    read_blocks_left: u32,
    /// CMD18 with block count 0 / block-count-enable off: keep streaming blocks
    /// until CMD12 stops it.
    read_open_ended: bool,
    /// Debug: 32-bit words handed out through the Buffer Data Port this transfer.
    words_out: u64,
    dbg: bool,
}

impl Default for Emmc2 {
    fn default() -> Self {
        Emmc2 {
            reg: BTreeMap::new(),
            card: None,
            resp: [0; 4],
            data: Vec::new(),
            data_pos: 0,
            read_lba: 0,
            read_blocks_left: 0,
            read_open_ended: false,
            words_out: 0,
            dbg: std::env::var_os("EMMC_DBG").is_some(),
        }
    }
}

impl Emmc2 {
    pub fn new() -> Emmc2 {
        Emmc2::default()
    }

    /// Insert a card backed by `image` (a raw block device: MBR + FAT + files).
    pub fn insert_card(&mut self, image: Vec<u8>) {
        self.card = Some(SdCard::new(image));
    }

    pub fn has_card(&self) -> bool {
        self.card.is_some()
    }

    fn get(&self, off: u32) -> u32 {
        self.reg.get(&off).copied().unwrap_or(0)
    }

    fn set_int(&mut self, bits: u32) {
        let cur = self.get(INT_STATUS);
        self.reg.insert(INT_STATUS, cur | bits);
    }

    fn block_size(&self) -> usize {
        let v = (self.get(BLOCK_SIZE_COUNT) & 0xFFF) as usize;
        if v == 0 {
            512
        } else {
            v
        }
    }

    fn block_count(&self) -> u32 {
        (self.get(BLOCK_SIZE_COUNT) >> 16) & 0xFFFF
    }

    /// Dispatch the command in the `0x0C` word to the card and latch its result.
    fn issue_command(&mut self, cmd_xfer: u32) {
        let arg = self.get(ARGUMENT);
        let command = cmd_xfer >> 16;
        let index = ((command >> 8) & 0x3F) as u8;
        let resp_type = command & 0x3; // 0 none, 1 R2(136), 2 R(48), 3 R1b(48+busy)
        let data_present = (command >> 5) & 1 != 0;

        self.resp = [0; 4];

        let response = match self.card.as_mut() {
            Some(card) => card.command(index, arg),
            None => crate::periph::sdcard::SdResponse::default(),
        };

        match resp_type {
            1 => {
                // R2: RESPONSE0..3 hold CID/CSD bits [127:8].
                let v = response.r2.unwrap_or(0);
                self.resp[0] = (v >> 8) as u32;
                self.resp[1] = (v >> 40) as u32;
                self.resp[2] = (v >> 72) as u32;
                self.resp[3] = (v >> 104) as u32;
            }
            2 | 3 => {
                self.resp[0] = response.r1.unwrap_or(0);
            }
            _ => {}
        }

        self.set_int(INT_CMD_COMPLETE);
        if resp_type == 3 {
            // R1b: busy released immediately in the model. CMD12 (STOP) also
            // lands here — ending the open-ended read below covers the rest.
            self.set_int(INT_XFER_COMPLETE);
        }

        // Any new command tears down a previous transfer's PIO state (CMD12
        // included, via the block above).
        self.data.clear();
        self.data_pos = 0;
        self.read_blocks_left = 0;
        self.read_open_ended = false;
        self.words_out = 0;
        if data_present && response.read_blocks > 0 {
            self.read_lba = response.read_lba;
            let count = self.block_count();
            if response.read_blocks == u32::MAX {
                // CMD18: bounded by the block-count register if set, else runs
                // until CMD12.
                if count > 0 {
                    self.read_blocks_left = count;
                } else {
                    self.read_open_ended = true;
                }
            } else {
                self.read_blocks_left = response.read_blocks;
            }
            self.fill_next_block();
        }

        if self.dbg {
            eprintln!(
                "  emmc CMD{index} arg={arg:#010x} rt={resp_type} data={data_present} \
                 -> r0={:#010x} r1={:#010x} r2={:#010x} r3={:#010x} blocks={} open={}",
                self.resp[0],
                self.resp[1],
                self.resp[2],
                self.resp[3],
                self.read_blocks_left,
                self.read_open_ended
            );
        }
    }

    /// Pull one block from the card (or a synthetic register block) into the PIO
    /// buffer and flag Buffer Read Ready.
    fn fill_next_block(&mut self) {
        if self.read_blocks_left == 0 && !self.read_open_ended {
            return;
        }
        let bs = self.block_size();
        let mut block = [0u8; 512];
        match self.read_lba {
            SWITCH_FUNC_MAGIC_LBA => {
                // CMD6 switch-function status: 64 bytes, byte 13 bit0..3 =
                // supported group-1 functions; leave benign zeros but mark the
                // "function 1 = high-speed" as selectable.
                block[13] = 0x02;
                block[16] = 0x01; // selected function group 1 = 1
            }
            SCR_MAGIC_LBA => {
                // SCR: SD spec 2.00, 1-bit + 4-bit bus width, 8 bytes.
                block[0] = 0x02; // SCR_STRUCTURE=0, SD_SPEC=2
                block[1] = 0x05; // bus widths: 1-bit | 4-bit
            }
            lba => {
                if let Some(card) = self.card.as_ref() {
                    card.read_block(lba, &mut block);
                }
                self.read_lba = lba.wrapping_add(1);
            }
        }
        self.data = block[..bs.min(512)].to_vec();
        self.data_pos = 0;
        self.read_blocks_left = self.read_blocks_left.saturating_sub(1);
        self.set_int(INT_BUF_READ_RDY);
        if self.dbg {
            let lba = self.read_lba.wrapping_sub(1);
            eprintln!(
                "  emmc  block lba={lba:#x} ({} bytes) first={:02x}{:02x}{:02x}{:02x} left={} open={}",
                self.data.len(),
                block[0],
                block[1],
                block[2],
                block[3],
                self.read_blocks_left,
                self.read_open_ended
            );
        }
    }

    /// Read the next little-endian word out of the PIO buffer.
    fn read_buffer_word(&mut self) -> u32 {
        let mut w = [0u8; 4];
        for b in w.iter_mut() {
            *b = self.data.get(self.data_pos).copied().unwrap_or(0);
            self.data_pos += 1;
        }
        self.words_out += 1;
        if self.data_pos >= self.data.len() {
            // Block drained: fetch the next one, or finish the transfer.
            if self.read_blocks_left > 0 || self.read_open_ended {
                self.fill_next_block();
            } else {
                let cur = self.get(INT_STATUS) & !INT_BUF_READ_RDY;
                self.reg.insert(INT_STATUS, cur | INT_XFER_COMPLETE);
                self.data.clear();
                self.data_pos = 0;
            }
        }
        u32::from_le_bytes(w)
    }
}

impl MmioDevice for Emmc2 {
    fn name(&self) -> &'static str {
        "emmc2"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        let v = match off {
            RESPONSE0 => self.resp[0],
            RESPONSE1 => self.resp[1],
            RESPONSE2 => self.resp[2],
            RESPONSE3 => self.resp[3],
            BUFFER_DATA => self.read_buffer_word(),
            CLOCK_CONTROL => {
                let clk = self.get(CLOCK_CONTROL) & 0xFFFF;
                // Internal clock reports stable as soon as it is enabled; the
                // software-reset bits (high half) always read back done.
                if clk & CLK_INTLEN != 0 {
                    clk | CLK_STABLE
                } else {
                    clk
                }
            }
            PRESENT_STATE => {
                let mut ps = PRESENT_STATE_IDLE;
                if !self.data.is_empty() && self.data_pos < self.data.len() {
                    ps |= PS_BUF_READ_EN;
                }
                ps
            }
            // v3 host, base clock 100 MHz, 3.3 V, high-speed, SDMA.
            CAPABILITIES_0 => (100 << 8) | (1 << 21) | (1 << 22) | (1 << 24) | (1 << 25),
            CAPABILITIES_1 => 0,
            CONTROLLER_VERSION => 0x0002,
            _ => self.get(off),
        };
        if self.dbg {
            if off == BUFFER_DATA {
                if self.words_out % 64 == 1 {
                    eprintln!("  emmc R [0x20] -> {v:#010x}  (word {})", self.words_out);
                }
            } else {
                eprintln!("  emmc R [{off:#04x}] -> {v:#010x}");
            }
        }
        Ok(v)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        if self.dbg && off != BUFFER_DATA {
            eprintln!("  emmc W [{off:#04x}] <- {value:#010x}");
        }
        match off {
            CLOCK_CONTROL => {
                self.reg.insert(CLOCK_CONTROL, value & !SRST_MASK);
                // Software resets self-clear immediately. A DAT reset tears down
                // the data path (FIFO + data-circuit interrupts) but leaves
                // command-complete alone; a CMD reset clears command-complete; an
                // ALL reset clears everything.
                if value & (SRST_ALL | SRST_DATA) != 0 {
                    self.data.clear();
                    self.data_pos = 0;
                    self.read_blocks_left = 0;
                    self.read_open_ended = false;
                    let keep = if value & SRST_ALL != 0 {
                        0
                    } else {
                        self.get(INT_STATUS) & !INT_DATA_BITS
                    };
                    self.reg.insert(INT_STATUS, keep);
                }
                if value & (SRST_ALL | SRST_CMD) != 0 {
                    let cur = self.get(INT_STATUS);
                    self.reg.insert(INT_STATUS, cur & !INT_CMD_COMPLETE);
                }
            }
            INT_STATUS => {
                let cur = self.get(INT_STATUS);
                self.reg.insert(INT_STATUS, cur & !value); // write-1-to-clear
            }
            CMD_XFER => {
                self.reg.insert(CMD_XFER, value);
                self.issue_command(value);
            }
            SDMA_ADDR | BLOCK_SIZE_COUNT | ARGUMENT | HOST_CONTROL | INT_STATUS_EN
            | INT_SIGNAL_EN => {
                self.reg.insert(off, value);
            }
            RESPONSE0 | RESPONSE1 | RESPONSE2 | RESPONSE3 | BUFFER_DATA | PRESENT_STATE
            | CAPABILITIES_0 | CAPABILITIES_1 | CONTROLLER_VERSION => {}
            _ => {
                self.reg.insert(off, value);
            }
        }
        Ok(())
    }
}
