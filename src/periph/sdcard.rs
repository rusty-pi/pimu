//! A minimal SD card (SDHC / v2, high-capacity) state machine, backed by a flat
//! block image in RAM. Enough of the physical-layer command set for the main
//! bootloader to enumerate the card and read `start4.elf` off a FAT partition:
//!
//! ```text
//!   CMD0  GO_IDLE_STATE          -> idle
//!   CMD8  SEND_IF_COND           -> R7  (echo voltage + check pattern)
//!   CMD55 APP_CMD                -> R1  (next command is an ACMD)
//!   ACMD41 SD_SEND_OP_COND       -> R3  (OCR; busy bit set once "powered up")
//!   CMD2  ALL_SEND_CID           -> R2  (CID)                     idle -> ident
//!   CMD3  SEND_RELATIVE_ADDR     -> R6  (RCA)                    ident -> stby
//!   CMD9  SEND_CSD               -> R2  (CSD)
//!   CMD7  SELECT_CARD            -> R1b                     stby <-> tran
//!   CMD6  (ACMD) SET_BUS_WIDTH   -> R1
//!   CMD16 SET_BLOCKLEN           -> R1
//!   CMD17 READ_SINGLE_BLOCK      -> R1 + 1 data block
//!   CMD18 READ_MULTIPLE_BLOCK    -> R1 + N data blocks
//!   CMD12 STOP_TRANSMISSION      -> R1b
//!   CMD13 SEND_STATUS            -> R1
//!   CMD6  SWITCH_FUNC (non-app)  -> R1 + 64-byte status block
//! ```
//!
//! Responses are returned as the 32-bit payload the SDHCI RESPONSE registers
//! expose (bits `[39:8]` of the card response) — for R2 the caller passes the
//! full 120-bit CID/CSD out through [`SdResponse::r2`].
//!
//! Writes are not modelled — the bootloader only reads.
//!
//! The card contents come from a [`BlockDevice`] rather than a byte array, so
//! the same state machine serves a disk image in RAM (hosted) and QEMU's own SD
//! controller (bare-metal, #32).

use crate::block::{BlockDevice, MemoryBlocks, BLOCK_LEN};

use alloc::boxed::Box;
use alloc::vec::Vec;

/// SD card operating states (subset), per the physical-layer spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardState {
    Idle,
    Ready,
    Ident,
    Stby,
    Tran,
    Data,
}

/// What the host controller needs back after dispatching a command.
#[derive(Debug, Clone, Default)]
pub struct SdResponse {
    /// 32-bit payload for a 48-bit response (R1/R1b/R3/R6/R7). `None` = the
    /// command produces no response.
    pub r1: Option<u32>,
    /// Full 128-bit CID/CSD for an R2 response, already aligned so that
    /// `bits[127:8]` are the card register and `bits[7:0]` are zero (the CRC
    /// slot the SDHCI drops).
    pub r2: Option<u128>,
    /// Number of 512-byte blocks the card will now stream to the host.
    pub read_blocks: u32,
    /// Block address (SDHC: block units) the read starts at.
    pub read_lba: u32,
}

impl SdResponse {
    fn r1(status: u32) -> SdResponse {
        SdResponse {
            r1: Some(status),
            ..Default::default()
        }
    }
    fn none() -> SdResponse {
        SdResponse::default()
    }
}

/// R1 card-status bits the bootloader is likely to look at.
const R1_APP_CMD: u32 = 1 << 5;
const R1_READY_FOR_DATA: u32 = 1 << 8;
const R1_CURRENT_STATE_SHIFT: u32 = 9; // bits [12:9]

pub struct SdCard {
    /// Where the sectors come from. The FAT image lives behind this.
    medium: Box<dyn BlockDevice>,
    state: CardState,
    /// Relative card address, assigned by CMD3.
    rca: u16,
    /// Set by CMD55; cleared after the following (A)CMD is dispatched.
    app_cmd: bool,
    /// OCR "powered up / busy" latch — ACMD41 reports not-ready once, then ready,
    /// mimicking the card's power-up ramp so the host's poll loop runs at least
    /// one iteration.
    powered_up: bool,
    /// 128-bit CID, aligned with the CRC slot ([7:0]) zeroed.
    cid: u128,
    /// 128-bit CSD, same alignment.
    csd: u128,
}

impl SdCard {
    /// Wrap a raw card image held in RAM (must be a multiple of 512 bytes;
    /// padded if not, and never empty — a zero-capacity card would give the
    /// bootloader a CSD it cannot make sense of).
    pub fn new(mut image: Vec<u8>) -> SdCard {
        if !image.len().is_multiple_of(BLOCK_LEN) {
            image.resize(image.len().next_multiple_of(BLOCK_LEN), 0);
        }
        if image.is_empty() {
            image.resize(BLOCK_LEN, 0);
        }
        SdCard::with_medium(Box::new(MemoryBlocks::new(image)))
    }

    /// Wrap any block medium — the seam the bare-metal frontend plugs QEMU's
    /// SD controller into.
    pub fn with_medium(medium: Box<dyn BlockDevice>) -> SdCard {
        let blocks = medium.block_count();
        SdCard {
            medium,
            state: CardState::Idle,
            rca: 0,
            app_cmd: false,
            powered_up: false,
            cid: default_cid(),
            csd: csd_v2(blocks),
        }
    }

    pub fn block_count(&self) -> u64 {
        self.medium.block_count()
    }

    /// Fetch one 512-byte block. A read past the end of the medium (or a
    /// backend that failed) leaves `out` zeroed, as it always has — the
    /// bootloader probes beyond the partition and must see quiet zeros, not a
    /// stalled transfer.
    pub fn read_block(&self, lba: u32, out: &mut [u8; BLOCK_LEN]) {
        self.medium.read_block(u64::from(lba), out);
    }

    fn status(&self) -> u32 {
        R1_READY_FOR_DATA | ((self.state_code()) << R1_CURRENT_STATE_SHIFT)
    }

    fn state_code(&self) -> u32 {
        match self.state {
            CardState::Idle => 0,
            CardState::Ready => 1,
            CardState::Ident => 2,
            CardState::Stby => 3,
            CardState::Tran => 4,
            CardState::Data => 5,
        }
    }

    /// Dispatch one command. `acmd` is the CMD55-app flag latched by the host
    /// (we also track it internally; either is accepted).
    pub fn command(&mut self, cmd: u8, arg: u32) -> SdResponse {
        let is_app = self.app_cmd;
        self.app_cmd = false;

        if is_app {
            return self.app_command(cmd, arg);
        }

        match cmd {
            0 => {
                // GO_IDLE_STATE
                self.state = CardState::Idle;
                self.powered_up = false;
                SdResponse::none()
            }
            2 => {
                // ALL_SEND_CID -> R2, idle/ready -> ident
                self.state = CardState::Ident;
                SdResponse {
                    r2: Some(self.cid),
                    ..Default::default()
                }
            }
            3 => {
                // SEND_RELATIVE_ADDR -> R6, ident -> stby
                self.rca = 0x0001;
                self.state = CardState::Stby;
                // R6: [31:16] RCA, [15:0] status bits (mirrors card status
                // 23,22,19,12:0). Report "ready, stby".
                let r6 = ((self.rca as u32) << 16) | 0x0500;
                SdResponse::r1(r6)
            }
            6 => {
                // SWITCH_FUNC (non-app CMD6) -> R1 + 64-byte status block.
                SdResponse {
                    r1: Some(self.status()),
                    read_blocks: 1,
                    read_lba: SWITCH_FUNC_MAGIC_LBA,
                    ..Default::default()
                }
            }
            7 => {
                // SELECT/DESELECT_CARD -> R1b
                if (arg >> 16) as u16 == self.rca {
                    self.state = CardState::Tran;
                } else {
                    self.state = CardState::Stby;
                }
                SdResponse::r1(self.status())
            }
            8 => {
                // SEND_IF_COND -> R7: echo [11:8] voltage + [7:0] check pattern.
                SdResponse::r1(arg & 0xFFF)
            }
            9 => {
                // SEND_CSD -> R2
                SdResponse {
                    r2: Some(self.csd),
                    ..Default::default()
                }
            }
            10 => {
                // SEND_CID -> R2
                SdResponse {
                    r2: Some(self.cid),
                    ..Default::default()
                }
            }
            12 => {
                // STOP_TRANSMISSION -> R1b
                if self.state == CardState::Data {
                    self.state = CardState::Tran;
                }
                SdResponse::r1(self.status())
            }
            13 => {
                // SEND_STATUS -> R1
                SdResponse::r1(self.status())
            }
            16 => {
                // SET_BLOCKLEN -> R1 (SDHC ignores; always 512)
                SdResponse::r1(self.status())
            }
            17 | 18 => {
                // READ_SINGLE / READ_MULTIPLE_BLOCK -> R1 + data
                self.state = CardState::Data;
                SdResponse {
                    r1: Some(self.status()),
                    read_lba: arg,
                    read_blocks: if cmd == 17 { 1 } else { u32::MAX },
                    ..Default::default()
                }
            }
            55 => {
                // APP_CMD -> R1 with the APP_CMD bit set; next command is an ACMD
                self.app_cmd = true;
                SdResponse::r1(self.status() | R1_APP_CMD)
            }
            _ => SdResponse::r1(self.status()),
        }
    }

    fn app_command(&mut self, cmd: u8, _arg: u32) -> SdResponse {
        match cmd {
            6 => {
                // SET_BUS_WIDTH -> R1
                SdResponse::r1(self.status())
            }
            13 => {
                // SD_STATUS -> R1 + 64-byte status block
                SdResponse {
                    r1: Some(self.status()),
                    read_blocks: 1,
                    read_lba: SWITCH_FUNC_MAGIC_LBA,
                    ..Default::default()
                }
            }
            41 => {
                // SD_SEND_OP_COND -> R3 (OCR). Report busy once, then ready with
                // CCS=1 (high-capacity).
                let ocr = if self.powered_up {
                    self.state = CardState::Ready;
                    0xC0FF_8000 // busy-done (bit31) + CCS (bit30) + voltage window
                } else {
                    self.powered_up = true;
                    0x00FF_8000 // still powering up (bit31 clear)
                };
                SdResponse::r1(ocr)
            }
            51 => {
                // SEND_SCR -> R1 + 8-byte SCR block
                SdResponse {
                    r1: Some(self.status()),
                    read_blocks: 1,
                    read_lba: SCR_MAGIC_LBA,
                    ..Default::default()
                }
            }
            _ => SdResponse::r1(self.status()),
        }
    }
}

/// Sentinel LBAs the host controller maps to synthetic register blocks rather
/// than card data (CMD6 switch-status, ACMD51 SCR). Chosen far outside any real
/// image.
pub const SWITCH_FUNC_MAGIC_LBA: u32 = 0xFFFF_FFFE;
pub const SCR_MAGIC_LBA: u32 = 0xFFFF_FFFD;

/// A plausible CID (Manufacturer "PI", product "VIRTF", serial, date), aligned
/// so bits `[7:0]` (the CRC slot) are zero.
fn default_cid() -> u128 {
    let mut cid: u128 = 0;
    cid |= 0x00 << 120; // MID = 0 (unknown mfr)
    cid |= (u128::from(b'P') << 112) | (u128::from(b'I') << 104); // OID "PI"
                                                                  // PNM "VIRTF"
    for (i, c) in b"VIRTF".iter().enumerate() {
        cid |= u128::from(*c) << (96 - 8 * i as u32);
    }
    cid |= 0x01 << 56; // PRV = 1.0
    cid |= 0x1234_5678 << 24; // PSN
    cid |= 0x015 << 8; // MDT ~ 2021-05
    cid
}

/// CSD version 2.0 (CSD_STRUCTURE = 1), high-capacity, describing `blocks`
/// 512-byte sectors. Only the fields the bootloader reads are meaningful.
fn csd_v2(blocks: u64) -> u128 {
    // CSD v2: C_SIZE counts (512 KiB) units, minus 1.
    let c_size = (blocks / 1024).saturating_sub(1) as u128;
    let mut csd: u128 = 0;
    csd |= 1 << 126; // CSD_STRUCTURE = 1
    csd |= 0x0E << 112; // TAAC = 1ms
    csd |= 0x00 << 104; // NSAC
    csd |= 0x5A << 96; // TRAN_SPEED = 50 Mbit/s
    csd |= 0x05B5 << 80; // CCC
    csd |= 0x09 << 76; // READ_BL_LEN = 9 (512)
    csd |= (c_size & 0x3F_FFFF) << 48;
    csd |= 0x09 << 22; // WRITE_BL_LEN = 9
    csd |= 1 << 21; // WRITE_BL_PARTIAL? keep 0 normally; harmless
    csd
}
