//! A minimal SD card (SDHC / v3, high-capacity, UHS-I) state machine, backed by
//! a block image. Enough of the physical-layer command set for the
//! main bootloader to read `start4.elf` off a FAT partition, and for Linux's
//! `mmc` core to bring the card up at 1.8 V DDR50 and mount a filesystem on it:
//!
//! ```text
//!   CMD0  GO_IDLE_STATE          -> idle
//!   CMD8  SEND_IF_COND           -> R7  (echo voltage + check pattern)
//!   CMD55 APP_CMD                -> R1  (next command is an ACMD)
//!   ACMD41 SD_SEND_OP_COND       -> R3  (OCR; busy bit set once "powered up",
//!                                        S18A when the host asked for 1.8 V)
//!   CMD11 VOLTAGE_SWITCH         -> R1                     (1.8 V signalling)
//!   CMD2  ALL_SEND_CID           -> R2  (CID)                     idle -> ident
//!   CMD3  SEND_RELATIVE_ADDR     -> R6  (RCA)                    ident -> stby
//!   CMD9  SEND_CSD               -> R2  (CSD)
//!   CMD7  SELECT_CARD            -> R1b                     stby <-> tran
//!   ACMD6 SET_BUS_WIDTH          -> R1
//!   ACMD13 SD_STATUS             -> R1 + 64-byte status block
//!   ACMD22 SEND_NUM_WR_BLOCKS    -> R1 + 4-byte count of the last write's blocks
//!   ACMD51 SEND_SCR              -> R1 + 8-byte SCR
//!   CMD6  SWITCH_FUNC            -> R1 + 64-byte status block
//!   CMD16 SET_BLOCKLEN           -> R1
//!   CMD17/18 READ_SINGLE/MULTIPLE_BLOCK  -> R1 + data blocks
//!   CMD19 SEND_TUNING_BLOCK      -> R1 + the 64-byte tuning pattern
//!   CMD23 SET_BLOCK_COUNT        -> R1  (bounds the next CMD18/CMD25)
//!   CMD24/25 WRITE_BLOCK/MULTIPLE_BLOCK  -> R1, host sends data blocks
//!   CMD32/33/38 ERASE            -> R1/R1b (erased blocks read as zero)
//!   CMD12 STOP_TRANSMISSION      -> R1b
//!   CMD13 SEND_STATUS            -> R1
//! ```
//!
//! The SDIO / MMC probes Linux sends first (CMD5, CMD52, CMD1) get no response,
//! as from a real SD memory card.
//!
//! The same type also plays an **e-MMC** part ([`CardKind::Mmc`]), the flash
//! soldered to a Compute Module, which answers a different identification
//! sequence (JEDEC JESD84-B51):
//!
//! ```text
//!   CMD0  GO_IDLE_STATE          -> idle
//!   CMD1  SEND_OP_COND           -> R3  (OCR; busy bit set once "powered up",
//!                                        sector mode when the host offers it)
//!   CMD2  ALL_SEND_CID           -> R2  (CID)                    ready -> ident
//!   CMD3  SET_RELATIVE_ADDR      -> R1  (the *host* picks the RCA) ident -> stby
//!   CMD9  SEND_CSD               -> R2  (CSD; C_SIZE saturated, see EXT_CSD)
//!   CMD7  SELECT_CARD            -> R1b                          stby <-> tran
//!   CMD8  SEND_EXT_CSD           -> R1 + the 512-byte EXT_CSD
//!   CMD6  SWITCH                 -> R1b (writes one EXT_CSD byte)
//! ```
//!
//! An e-MMC has no application commands (CMD55 is illegal), does not answer
//! CMD8 as SEND_IF_COND, and has no SCR or SD status; CMD12, CMD13, CMD16 and
//! the block transfers are the same as for an SD card.
//!
//! Responses are returned as the 32-bit payload the SDHCI RESPONSE registers
//! expose (bits `[39:8]` of the card response) — for R2 the caller passes the
//! full 120-bit CID/CSD out through [`SdResponse::r2`].
//!
//! The card's blocks are a [`Disk`]: the image is read on demand and writes stay
//! in memory, so the file the image was loaded from is never touched.

use crate::periph::disk::Disk;

/// What is in the slot: a removable SD memory card, or an e-MMC part soldered
/// to the board (a Compute Module's flash).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CardKind {
    #[default]
    Sd,
    Mmc,
}

/// SD card operating states (subset), per the physical-layer spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardState {
    Idle,
    Ready,
    Ident,
    Stby,
    Tran,
    Data,
    Rcv,
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
    /// The card did not answer at all (an SDIO/MMC probe, or a command that is
    /// illegal in the current state) — the host sees a command timeout.
    pub no_response: bool,
    /// Number of 512-byte blocks the card will now stream to the host
    /// (`u32::MAX` = until CMD12 or a CMD23 count runs out).
    pub read_blocks: u32,
    /// Block address (SDHC: block units) the read starts at.
    pub read_lba: u32,
    /// Number of blocks the card will now accept from the host (`u32::MAX` =
    /// open-ended, as for `read_blocks`).
    pub write_blocks: u32,
    /// Block address the write starts at.
    pub write_lba: u32,
    /// A register-sized data block the card sends instead of image data
    /// (CMD6 switch status, ACMD13 SD status, ACMD51 SCR, CMD19 tuning block).
    pub data: Option<Vec<u8>>,
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
    fn silent() -> SdResponse {
        SdResponse {
            no_response: true,
            ..Default::default()
        }
    }
    fn with_data(status: u32, data: Vec<u8>) -> SdResponse {
        SdResponse {
            r1: Some(status),
            data: Some(data),
            ..Default::default()
        }
    }
}

/// R1 card-status bits the hosts look at.
const R1_APP_CMD: u32 = 1 << 5;
const R1_ILLEGAL_COMMAND: u32 = 1 << 22;
const R1_READY_FOR_DATA: u32 = 1 << 8;
const R1_CURRENT_STATE_SHIFT: u32 = 9; // bits [12:9]

/// OCR bits.
const OCR_BUSY_DONE: u32 = 1 << 31;
const OCR_CCS: u32 = 1 << 30;
/// ACMD41 argument: the host can switch to 1.8 V (S18R); in the response, the
/// card accepts (S18A).
const OCR_S18: u32 = 1 << 24;
const OCR_VOLTAGE_WINDOW: u32 = 0x00FF_8000;
/// CMD1 access mode `10b` in OCR `[30:29]`: block (sector) addressing. A part
/// of 2 GiB or less is byte-addressed and never sets it.
const MMC_OCR_SECTOR: u32 = 0b10 << 29;
/// 2 GiB in 512-byte blocks.
const BYTE_ADDR_BLOCKS: u64 = 2 * 1024 * 1024 * 1024 / 512;

/// EXT_CSD byte offsets this part implements (JESD84-B51 table 39).
const EXT_CSD_PARTITION_CONFIG: usize = 179;
const EXT_CSD_BUS_WIDTH: usize = 183;
const EXT_CSD_HS_TIMING: usize = 185;
const EXT_CSD_REV: usize = 192;
const EXT_CSD_CARD_TYPE: usize = 196;
const EXT_CSD_SEC_COUNT: usize = 212;
const EXT_CSD_HC_ERASE_GRP_SIZE: usize = 224;
const EXT_CSD_BOOT_SIZE_MULT: usize = 226;

/// CMD6 group-1 (bus speed) functions this card supports: SDR12, SDR25/high
/// speed, SDR50 and DDR50. At 3.3 V signalling a UHS card offers only the
/// first two; the UHS modes appear once CMD11 has moved it to 1.8 V.
const SWITCH_G1_UHS: u16 = 0x8017;
const SWITCH_G1_3V3: u16 = 0x8003;

/// The 64-byte tuning block a card sends for CMD19 on a 4-bit bus (SD
/// physical layer spec 3.01, 4.2.4.5).
const TUNING_BLOCK_4BIT: [u8; 64] = [
    0xff, 0x0f, 0xff, 0x00, 0xff, 0xcc, 0xc3, 0xcc, 0xc3, 0x3c, 0xcc, 0xff, 0xfe, 0xff, 0xfe, 0xef,
    0xff, 0xdf, 0xff, 0xdd, 0xff, 0xfb, 0xff, 0xfb, 0xbf, 0xff, 0x7f, 0xff, 0x77, 0xf7, 0xbd, 0xef,
    0xff, 0xf0, 0xff, 0xf0, 0x0f, 0xfc, 0xcc, 0x3c, 0xcc, 0x33, 0xcc, 0xcf, 0xff, 0xef, 0xff, 0xee,
    0xff, 0xfd, 0xff, 0xfd, 0xdf, 0xff, 0xbf, 0xff, 0xbb, 0xff, 0xf7, 0xff, 0xf7, 0x7f, 0x7b, 0xde,
];

pub struct SdCard {
    /// The card's contents, 512-byte blocks. The FAT image lives here.
    disk: Disk,
    /// SD card or e-MMC part: which identification sequence it answers.
    kind: CardKind,
    /// An e-MMC's EXT_CSD, the 512-byte register CMD8 reads and CMD6 writes
    /// a byte of at a time. Empty for an SD card.
    ext_csd: Vec<u8>,
    /// Transfer addresses are byte offsets, not block numbers: where an
    /// e-MMC starts, and where one of 2 GiB or less stays. CMD1 moves a
    /// larger part to sector addressing when the host offers it.
    byte_addressed: bool,
    state: CardState,
    /// Relative card address, assigned by CMD3.
    rca: u16,
    /// Set by CMD55; cleared after the following (A)CMD is dispatched.
    app_cmd: bool,
    /// OCR "powered up / busy" latch — ACMD41 reports not-ready once, then ready,
    /// mimicking the card's power-up ramp so the host's poll loop runs at least
    /// one iteration.
    powered_up: bool,
    /// The last ACMD41 offered 1.8 V (S18A) — CMD11 is legal only then.
    s18a_offered: bool,
    /// 1.8 V signalling (after CMD11). Survives CMD0; only a power cycle
    /// drops the card back to 3.3 V.
    signal_1v8: bool,
    /// ACMD6 bus width: `false` = 1-bit, `true` = 4-bit.
    wide_bus: bool,
    /// CMD6 functions currently selected, groups 1..=6.
    functions: [u8; 6],
    /// CMD23 block count, consumed by the next CMD18/CMD25.
    preset_count: Option<u32>,
    /// Blocks left before a counted transfer (CMD17/CMD24, or CMD18/CMD25 after
    /// CMD23) ends by itself; `None` = open-ended, ended by CMD12.
    blocks_left: Option<u32>,
    /// Blocks the last CMD24/CMD25 wrote, which ACMD22 reports.
    written_blocks: u32,
    /// CMD32/CMD33 erase range, inclusive block addresses.
    erase_start: u32,
    erase_end: u32,
    /// 128-bit CID, aligned with the CRC slot ([7:0]) zeroed.
    cid: u128,
    /// 128-bit CSD, same alignment.
    csd: u128,
}

impl SdCard {
    /// Wrap a raw card image (must be a multiple of 512 bytes; padded if not).
    pub fn new(mut image: Vec<u8>) -> SdCard {
        if !image.len().is_multiple_of(512) {
            image.resize(image.len().next_multiple_of(512), 0);
        }
        if image.is_empty() {
            image.resize(512, 0);
        }
        SdCard::with_disk(Disk::from_vec(image))
    }

    /// A card on `disk`, typically a file opened with [`Disk::open`].
    pub fn with_disk(disk: Disk) -> SdCard {
        SdCard::with_disk_kind(disk, CardKind::Sd)
    }

    /// An e-MMC part on `disk`: the flash soldered to a Compute Module.
    pub fn mmc_with_disk(disk: Disk) -> SdCard {
        SdCard::with_disk_kind(disk, CardKind::Mmc)
    }

    /// A card or an e-MMC part on `disk`.
    pub fn with_disk_kind(disk: Disk, kind: CardKind) -> SdCard {
        let blocks = disk.blocks();
        SdCard {
            disk,
            kind,
            ext_csd: match kind {
                CardKind::Sd => Vec::new(),
                CardKind::Mmc => ext_csd(blocks),
            },
            byte_addressed: kind == CardKind::Mmc,
            state: CardState::Idle,
            rca: 0,
            app_cmd: false,
            powered_up: false,
            s18a_offered: false,
            signal_1v8: false,
            wide_bus: false,
            functions: [0; 6],
            preset_count: None,
            blocks_left: None,
            written_blocks: 0,
            erase_start: 0,
            erase_end: 0,
            cid: match kind {
                CardKind::Sd => default_cid(),
                CardKind::Mmc => mmc_cid(),
            },
            csd: match kind {
                CardKind::Sd => csd_v2(blocks),
                CardKind::Mmc => mmc_csd(),
            },
        }
    }

    pub fn block_count(&self) -> u64 {
        self.disk.blocks()
    }

    /// SD card or e-MMC part.
    pub fn kind(&self) -> CardKind {
        self.kind
    }

    pub fn state(&self) -> CardState {
        self.state
    }

    /// The card signals at 1.8 V (a completed CMD11).
    pub fn signal_1v8(&self) -> bool {
        self.signal_1v8
    }

    /// The card's blocks, writes included.
    pub fn disk(&self) -> &Disk {
        &self.disk
    }

    /// Copy one 512-byte block out of the card (zeros past the end).
    pub fn read_block(&self, lba: u32, out: &mut [u8; 512]) {
        self.disk.read_block(u64::from(lba), out);
    }

    /// Store one block (up to 512 bytes) the host sent; ignored past the end.
    /// A stored block counts towards what ACMD22 reports.
    pub fn write_block(&mut self, lba: u32, data: &[u8]) {
        let mut block = [0u8; 512];
        // A short block only replaces the start of what is there.
        if !self.disk.peek_block(u64::from(lba), &mut block) {
            return;
        }
        let n = data.len().min(512);
        block[..n].copy_from_slice(&data[..n]);
        self.disk.write(u64::from(lba), &block);
        self.written_blocks += 1;
    }

    /// One data block of the current transfer went over the bus. A counted
    /// transfer returns the card to `tran` once its last block is through.
    pub fn block_done(&mut self) {
        if let Some(n) = self.blocks_left.as_mut() {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.blocks_left = None;
                if matches!(self.state, CardState::Data | CardState::Rcv) {
                    self.state = CardState::Tran;
                }
            }
        }
    }

    /// Card VDD removed: everything, the signalling voltage included, starts
    /// over at the next power-up.
    pub fn power_off(&mut self) {
        self.state = CardState::Idle;
        self.rca = 0;
        self.app_cmd = false;
        self.powered_up = false;
        self.s18a_offered = false;
        self.signal_1v8 = false;
        self.wide_bus = false;
        self.functions = [0; 6];
        self.preset_count = None;
        self.blocks_left = None;
        if self.kind == CardKind::Mmc {
            self.ext_csd = ext_csd(self.disk.blocks());
        }
    }

    /// The block a transfer argument names: an e-MMC of 2 GiB or less is
    /// addressed in bytes, everything else in 512-byte sectors.
    fn lba(&self, arg: u32) -> u32 {
        if self.byte_addressed {
            arg / 512
        } else {
            arg
        }
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
            CardState::Rcv => 6,
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

        if self.kind == CardKind::Mmc {
            if let Some(response) = self.mmc_command(cmd, arg) {
                return response;
            }
        }

        match cmd {
            0 => {
                // GO_IDLE_STATE. The signalling voltage is not reset (spec
                // 4.2.4.3: only a power cycle returns the card to 3.3 V).
                self.state = CardState::Idle;
                self.powered_up = false;
                self.s18a_offered = false;
                self.preset_count = None;
                self.blocks_left = None;
                self.functions = [0; 6];
                self.wide_bus = false;
                SdResponse::none()
            }
            // SEND_OP_COND (MMC), IO_SEND_OP_COND and IO_RW_DIRECT/EXTENDED
            // (SDIO): an SD memory card does not answer.
            1 | 5 | 52 | 53 => SdResponse::silent(),
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
                // SWITCH_FUNC -> R1 + 64-byte status block.
                let status = self.status();
                SdResponse::with_data(status, self.switch_func(arg))
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
            11 => {
                // VOLTAGE_SWITCH: legal only right after an ACMD41 that
                // accepted S18R. The card then drives CMD/DAT low until the
                // host has moved to 1.8 V (modelled by the host controller).
                if self.state == CardState::Ready && self.s18a_offered && !self.signal_1v8 {
                    self.signal_1v8 = true;
                    self.s18a_offered = false;
                    SdResponse::r1(self.status())
                } else {
                    SdResponse::silent()
                }
            }
            12 => {
                // STOP_TRANSMISSION -> R1b
                if matches!(self.state, CardState::Data | CardState::Rcv) {
                    self.state = CardState::Tran;
                }
                self.blocks_left = None;
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
                let status = self.status();
                self.state = CardState::Data;
                let count = if cmd == 17 {
                    Some(1)
                } else {
                    self.preset_count.take()
                };
                self.blocks_left = count;
                SdResponse {
                    r1: Some(status),
                    read_lba: self.lba(arg),
                    read_blocks: count.unwrap_or(u32::MAX),
                    ..Default::default()
                }
            }
            19 => {
                // SEND_TUNING_BLOCK -> R1 + tuning pattern; the card stays in
                // tran.
                SdResponse::with_data(self.status(), TUNING_BLOCK_4BIT.to_vec())
            }
            23 => {
                // SET_BLOCK_COUNT -> R1
                self.preset_count = Some(arg & 0xFFFF);
                SdResponse::r1(self.status())
            }
            24 | 25 => {
                // WRITE_BLOCK / WRITE_MULTIPLE_BLOCK -> R1, then the host
                // sends data.
                let status = self.status();
                self.state = CardState::Rcv;
                self.written_blocks = 0;
                let count = if cmd == 24 {
                    Some(1)
                } else {
                    self.preset_count.take()
                };
                self.blocks_left = count;
                SdResponse {
                    r1: Some(status),
                    write_lba: self.lba(arg),
                    write_blocks: count.unwrap_or(u32::MAX),
                    ..Default::default()
                }
            }
            32 => {
                self.erase_start = self.lba(arg);
                SdResponse::r1(self.status())
            }
            33 => {
                self.erase_end = self.lba(arg);
                SdResponse::r1(self.status())
            }
            38 => {
                // ERASE -> R1b. Erased blocks read back as zeros
                // (SCR.DATA_STAT_AFTER_ERASE = 0), discard likewise.
                let (lo, hi) = (self.erase_start, self.erase_end);
                if lo <= hi {
                    self.disk.zero(u64::from(lo), u64::from(hi));
                }
                SdResponse::r1(self.status())
            }
            55 => {
                // APP_CMD -> R1 with the APP_CMD bit set; next command is an ACMD
                self.app_cmd = true;
                SdResponse::r1(self.status() | R1_APP_CMD)
            }
            _ => SdResponse::r1(self.status()),
        }
    }

    /// The commands an e-MMC part answers differently from an SD card;
    /// `None` leaves the command to the shared arms.
    fn mmc_command(&mut self, cmd: u8, arg: u32) -> Option<SdResponse> {
        match cmd {
            1 => {
                // SEND_OP_COND -> R3. Busy once, then ready. `[30:29]` is
                // the access mode: the host offers what it supports and the
                // part answers with what it will use, which for anything over
                // 2 GiB is sectors rather than bytes whatever the host said
                // (JESD84-B51 7.4.3) — the stock firmware's own driver offers
                // byte addressing and is expected to follow.
                let ocr = if self.powered_up {
                    self.state = CardState::Ready;
                    let sector = self.disk.blocks() > BYTE_ADDR_BLOCKS;
                    self.byte_addressed = !sector;
                    let mode = if sector { MMC_OCR_SECTOR } else { 0 };
                    OCR_BUSY_DONE | mode | OCR_VOLTAGE_WINDOW
                } else {
                    self.powered_up = true;
                    OCR_VOLTAGE_WINDOW
                };
                Some(SdResponse::r1(ocr))
            }
            3 => {
                // SET_RELATIVE_ADDR -> R1: the host picks the address, and an
                // e-MMC only acknowledges it. ident -> stby.
                self.rca = (arg >> 16) as u16;
                self.state = CardState::Stby;
                Some(SdResponse::r1(self.status()))
            }
            6 => {
                // SWITCH -> R1b: access [25:24], EXT_CSD index [23:16], value
                // [15:8]. Access 0 selects a command set, which this part does
                // not have.
                let access = (arg >> 24) & 3;
                let index = ((arg >> 16) & 0xFF) as usize;
                let value = ((arg >> 8) & 0xFF) as u8;
                if let Some(byte) = self.ext_csd.get_mut(index) {
                    match access {
                        1 => *byte |= value,
                        2 => *byte &= !value,
                        3 => *byte = value,
                        _ => {}
                    }
                }
                self.wide_bus = self.ext_csd[EXT_CSD_BUS_WIDTH] & 0xF != 0;
                Some(SdResponse::r1(self.status()))
            }
            8 => {
                // SEND_EXT_CSD -> R1 + the 512-byte register. (CMD8 on an SD
                // card is SEND_IF_COND, which an e-MMC does not have.)
                Some(SdResponse::with_data(self.status(), self.ext_csd.clone()))
            }
            // SEND_TUNING_BLOCK is CMD21 here, not CMD19.
            19 => Some(SdResponse::silent()),
            21 => Some(SdResponse::with_data(
                self.status(),
                TUNING_BLOCK_4BIT.to_vec(),
            )),
            // No application commands: APP_CMD is illegal and latches nothing.
            55 => Some(SdResponse::r1(self.status() | R1_ILLEGAL_COMMAND)),
            _ => None,
        }
    }

    fn app_command(&mut self, cmd: u8, arg: u32) -> SdResponse {
        match cmd {
            6 => {
                // SET_BUS_WIDTH -> R1
                self.wide_bus = arg & 3 == 2;
                SdResponse::r1(self.status())
            }
            13 => {
                // SD_STATUS -> R1 + 64-byte status block. Only DAT_BUS_WIDTH
                // is filled in; a zero AU size tells the host nothing about
                // erase geometry, which it then leaves alone.
                let mut ssr = vec![0u8; 64];
                if self.wide_bus {
                    ssr[0] = 0x80;
                }
                SdResponse::with_data(self.status(), ssr)
            }
            22 => {
                // SEND_NUM_WR_BLOCKS -> R1 + the number of blocks the last
                // write stored, 32 bits, most significant byte first. edk2's
                // MmcDxe asks after every write and fails the write without
                // an answer (#51).
                SdResponse::with_data(self.status(), self.written_blocks.to_be_bytes().to_vec())
            }
            41 => {
                // SD_SEND_OP_COND -> R3 (OCR). Report busy once, then ready with
                // CCS=1 (high-capacity) — and S18A if the host asked for 1.8 V
                // and the card is still at 3.3 V.
                let ocr = if self.powered_up {
                    self.state = CardState::Ready;
                    let s18a = arg & OCR_S18 != 0 && !self.signal_1v8;
                    self.s18a_offered = s18a;
                    let s18 = if s18a { OCR_S18 } else { 0 };
                    OCR_BUSY_DONE | OCR_CCS | s18 | OCR_VOLTAGE_WINDOW
                } else {
                    self.powered_up = true;
                    OCR_VOLTAGE_WINDOW // still powering up (bit31 clear)
                };
                SdResponse::r1(ocr)
            }
            51 => {
                // SEND_SCR -> R1 + 8-byte SCR: SCR_STRUCTURE 0, SD_SPEC 2 with
                // SD_SPEC3 (a UHS-I card: Linux reads the CMD6 bus modes only
                // then), 1- and 4-bit bus, CMD23 supported — the parts of the
                // real board's card's `0x02858082` this card implements
                // (erased data reads 0, no SD_SPECX).
                let scr = vec![0x02, 0x05, 0x80, 0x02, 0, 0, 0, 0];
                SdResponse::with_data(self.status(), scr)
            }
            _ => SdResponse::r1(self.status()),
        }
    }

    /// CMD6: build the 512-bit switch-function status for `arg` and, in set
    /// mode (bit 31), select every requested function the card supports.
    fn switch_func(&mut self, arg: u32) -> Vec<u8> {
        let set = arg & (1 << 31) != 0;
        let g1 = if self.signal_1v8 {
            SWITCH_G1_UHS
        } else {
            SWITCH_G1_3V3
        };
        // Groups 2..6 support only their default function 0.
        let support: [u16; 6] = [g1, 0x8001, 0x8001, 0x8001, 0x8001, 0x8001];
        let mut result = [0u8; 6];
        for (g, r) in result.iter_mut().enumerate() {
            let want = ((arg >> (4 * g)) & 0xF) as u8;
            *r = if want == 0xF {
                self.functions[g]
            } else if support[g] & (1 << want) != 0 {
                want
            } else {
                0xF
            };
        }
        if set && result.iter().all(|&r| r != 0xF) {
            self.functions = result;
        }
        let mut st = vec![0u8; 64];
        // [511:496] maximum current consumption, mA.
        st[0..2].copy_from_slice(&100u16.to_be_bytes());
        // [495:400] support bits, group 6 first.
        for (i, g) in (0..6).rev().enumerate() {
            st[2 + 2 * i..4 + 2 * i].copy_from_slice(&support[g].to_be_bytes());
        }
        // [399:376] function selection, a nibble per group, group 6 first.
        st[14] = (result[5] << 4) | result[4];
        st[15] = (result[3] << 4) | result[2];
        st[16] = (result[1] << 4) | result[0];
        // [375:368] data structure version 1: busy status follows (all clear).
        st[17] = 1;
        st
    }
}

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
/// 512-byte sectors. Apart from C_SIZE, every field is the value the SD spec
/// fixes for CSD 2.0 — which is also what the real board's card reports
/// (`CSD: 400e00325b590000e9277f800a400000`, sd-card-boot.log).
fn csd_v2(blocks: u64) -> u128 {
    // CSD v2: C_SIZE counts (512 KiB) units, minus 1.
    let c_size = (blocks / 1024).saturating_sub(1) as u128;
    let mut csd: u128 = 0;
    csd |= 1 << 126; // [127:126] CSD_STRUCTURE = 1
    csd |= 0x0E << 112; // [119:112] TAAC = 1 ms
    csd |= 0x00 << 104; // [111:104] NSAC
    csd |= 0x32 << 96; // [103:96] TRAN_SPEED = 25 MHz (default speed)
    csd |= 0x5B5 << 84; // [95:84] CCC: classes 0, 2, 4, 5, 7, 8, 10
    csd |= 0x9 << 80; // [83:80] READ_BL_LEN = 512
    csd |= (c_size & 0x3F_FFFF) << 48; // [69:48] C_SIZE
    csd |= 1 << 46; // ERASE_BLK_EN
    csd |= 0x7F << 39; // [45:39] SECTOR_SIZE
    csd |= 0x2 << 26; // [28:26] R2W_FACTOR
    csd |= 0x9 << 22; // [25:22] WRITE_BL_LEN = 512
    csd
}

/// An e-MMC CID: MID 0x15 with PNM "VIRTF" and the same alignment as the SD
/// one. An e-MMC's PNM is 6 characters, not 5, and CBX ([113:112]) says the
/// part is embedded.
fn mmc_cid() -> u128 {
    let mut cid: u128 = 0;
    cid |= 0x15 << 120; // MID
    cid |= 0x01 << 112; // CBX = 1 (BGA)
    cid |= 0x00 << 104; // OID
    for (i, c) in b"VIRTF4".iter().enumerate() {
        cid |= u128::from(*c) << (96 - 8 * i as u32);
    }
    cid |= 0x01 << 48; // PRV = 1.0
    cid |= 0x1234_5678 << 16; // PSN
    cid |= 0x15 << 8; // MDT
    cid
}

/// An e-MMC CSD (JESD84-B51 7.3): CSD_STRUCTURE 3 and SPEC_VERS 4 say the
/// real capacity and features are in the EXT_CSD, so C_SIZE is left at its
/// saturated 0xFFF and the host reads SEC_COUNT instead.
fn mmc_csd() -> u128 {
    let mut csd: u128 = 0;
    csd |= 3 << 126; // [127:126] CSD_STRUCTURE = 3 (EXT_CSD)
    csd |= 4 << 122; // [125:122] SPEC_VERS = 4 (v4.1 and later)
    csd |= 0x0E << 112; // [119:112] TAAC = 1 ms
    csd |= 0x01 << 104; // [111:104] NSAC
    csd |= 0x32 << 96; // [103:96] TRAN_SPEED = 25 MHz
    csd |= 0x0F5 << 84; // [95:84] CCC: classes 0, 2, 4, 5, 6, 7
    csd |= 0x9 << 80; // [83:80] READ_BL_LEN = 512
    csd |= 0xFFF << 62; // [73:62] C_SIZE saturated: see EXT_CSD SEC_COUNT
    csd |= 0x7 << 47; // [49:47] C_SIZE_MULT
    csd |= 0x1F << 42; // [46:42] ERASE_GRP_SIZE
    csd |= 0x1F << 37; // [41:37] ERASE_GRP_MULT
    csd |= 0x2 << 26; // [28:26] R2W_FACTOR
    csd |= 0x9 << 22; // [25:22] WRITE_BL_LEN = 512
    csd
}

/// An e-MMC's EXT_CSD for a part of `blocks` sectors: the handful of bytes a
/// bootloader or Linux reads, and zero everywhere else.
fn ext_csd(blocks: u64) -> Vec<u8> {
    let mut e = vec![0u8; 512];
    e[EXT_CSD_PARTITION_CONFIG] = 0; // boot from the user area
    e[EXT_CSD_BUS_WIDTH] = 0; // 1-bit until a CMD6 widens it
    e[EXT_CSD_HS_TIMING] = 0; // backwards-compatible timing
    e[EXT_CSD_REV] = 8; // v5.1
    e[EXT_CSD_CARD_TYPE] = 0x57; // HS26, HS52, HS_DDR 1.8 V, HS200 1.8 V
    let sectors = u32::try_from(blocks).unwrap_or(u32::MAX);
    e[EXT_CSD_SEC_COUNT..EXT_CSD_SEC_COUNT + 4].copy_from_slice(&sectors.to_le_bytes());
    e[EXT_CSD_HC_ERASE_GRP_SIZE] = 1; // 512 KiB erase groups
    e[EXT_CSD_BOOT_SIZE_MULT] = 0; // no boot partitions
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> SdCard {
        SdCard::new(vec![0u8; 1024 * 1024])
    }

    /// CMD0 → ACMD41 (twice: busy, then ready) with `arg`, returning the OCR.
    fn init_ocr(c: &mut SdCard, arg: u32) -> u32 {
        c.command(0, 0);
        let mut ocr = 0;
        for _ in 0..2 {
            c.command(55, 0);
            ocr = c.command(41, arg).r1.unwrap();
        }
        ocr
    }

    fn mmc(blocks: usize) -> SdCard {
        SdCard::mmc_with_disk(Disk::from_vec(vec![0u8; blocks * 512]))
    }

    /// CMD0 then CMD1 until the part reports it is ready; returns the OCR.
    fn mmc_ocr(c: &mut SdCard, arg: u32) -> u32 {
        c.command(0, 0);
        let mut ocr = 0;
        for _ in 0..8 {
            ocr = c.command(1, arg).r1.unwrap();
            if ocr & OCR_BUSY_DONE != 0 {
                break;
            }
        }
        ocr
    }

    #[test]
    fn mmc_answers_cmd1_and_sizes_itself_by_capacity() {
        // A small part stays byte-addressed however the host asks, so the
        // transfer argument is divided down to a block number.
        let mut c = mmc(2048);
        assert_eq!(mmc_ocr(&mut c, 0x4010_0000) & MMC_OCR_SECTOR, 0);
        c.command(3, 0x0001_0000);
        c.command(7, 0x0001_0000);
        assert_eq!(c.command(17, 4 * 512).read_lba, 4);

        // Over 2 GiB it answers sector mode even though this host offered
        // byte addressing, and the argument is already a block number.
        let mut big = mmc(5 * 1024 * 1024);
        assert_ne!(mmc_ocr(&mut big, 0x0020_0000) & MMC_OCR_SECTOR, 0);
        big.command(3, 0x0001_0000);
        big.command(7, 0x0001_0000);
        assert_eq!(big.command(17, 4).read_lba, 4);
    }

    #[test]
    fn mmc_has_no_app_commands_and_an_ext_csd() {
        let mut c = mmc(2048);
        mmc_ocr(&mut c, 0x4010_0000);
        assert_ne!(c.command(55, 0).r1.unwrap() & R1_ILLEGAL_COMMAND, 0);
        // ...and the command after it is not taken as an ACMD.
        assert_eq!(c.command(13, 0).r1.unwrap() & R1_ILLEGAL_COMMAND, 0);

        let ext = c.command(8, 0).data.expect("EXT_CSD");
        assert_eq!(ext.len(), 512);
        assert_eq!(
            u32::from_le_bytes(
                ext[EXT_CSD_SEC_COUNT..EXT_CSD_SEC_COUNT + 4]
                    .try_into()
                    .unwrap()
            ),
            2048
        );
        // CMD6 writes one byte of it; the host's own width follows.
        c.command(6, 0x03b7_0100);
        let ext = c.command(8, 0).data.unwrap();
        assert_eq!(ext[EXT_CSD_BUS_WIDTH], 1);
        c.command(6, 0x03b9_0100);
        assert_eq!(c.command(8, 0).data.unwrap()[EXT_CSD_HS_TIMING], 1);
    }

    #[test]
    fn s18a_only_when_asked_and_only_until_the_switch() {
        let mut c = card();
        assert_eq!(init_ocr(&mut c, 0x40FF_8000) & OCR_S18, 0);
        assert!(c.command(11, 0).no_response, "CMD11 without S18A");

        let ocr = init_ocr(&mut c, 0x41FF_8000);
        assert_eq!(ocr & (OCR_BUSY_DONE | OCR_CCS | OCR_S18), 0xC100_0000);
        assert!(c.command(11, 0).r1.is_some());
        assert!(c.signal_1v8());

        // CMD0 keeps 1.8 V, so S18A is not offered again...
        assert_eq!(init_ocr(&mut c, 0x41FF_8000) & OCR_S18, 0);
        // ...until a power cycle.
        c.power_off();
        assert!(!c.signal_1v8());
        assert_ne!(init_ocr(&mut c, 0x41FF_8000) & OCR_S18, 0);
    }

    #[test]
    fn switch_function_offers_ddr50_only_at_1v8() {
        let mut c = card();
        let st = c.command(6, 0x00FF_FFF0).data.unwrap();
        assert_eq!(st[13], 0x03, "SDR12 | SDR25 at 3.3 V");
        assert_eq!(c.command(6, 0x80FF_FFF4).data.unwrap()[16] & 0xF, 0xF);

        init_ocr(&mut c, 0x41FF_8000);
        c.command(11, 0);
        let st = c.command(6, 0x00FF_FFF0).data.unwrap();
        assert_eq!(st[13], 0x17);
        let st = c.command(6, 0x80FF_FFF4).data.unwrap();
        assert_eq!(st[16] & 0xF, 4, "DDR50 selected");
        // Group 1 left alone (0xF) reports the current function.
        assert_eq!(c.command(6, 0x00FF_FFFF).data.unwrap()[16] & 0xF, 4);
    }

    #[test]
    fn write_then_read_back_and_counted_transfer_returns_to_tran() {
        let mut c = card();
        c.state = CardState::Tran;
        c.command(23, 2);
        let r = c.command(25, 7);
        assert_eq!((r.write_lba, r.write_blocks), (7, 2));
        assert_eq!(c.state(), CardState::Rcv);
        c.write_block(7, &[0xAA; 512]);
        c.block_done();
        assert_eq!(c.state(), CardState::Rcv);
        c.write_block(8, &[0xBB; 512]);
        c.block_done();
        assert_eq!(c.state(), CardState::Tran);

        let mut b = [0u8; 512];
        c.read_block(8, &mut b);
        assert_eq!(b, [0xBB; 512]);

        // Open-ended: stays in rcv until CMD12.
        assert_eq!(c.command(25, 9).write_blocks, u32::MAX);
        c.block_done();
        assert_eq!(c.state(), CardState::Rcv);
        c.command(12, 0);
        assert_eq!(c.state(), CardState::Tran);
    }

    #[test]
    fn erase_zeroes_the_range() {
        let mut c = SdCard::new(vec![0x5A; 8 * 512]);
        c.command(32, 2);
        c.command(33, 3);
        c.command(38, 0);
        let mut b = [0u8; 512];
        c.read_block(1, &mut b);
        assert_eq!(b[0], 0x5A);
        c.read_block(3, &mut b);
        assert_eq!(b, [0; 512]);
        c.read_block(4, &mut b);
        assert_eq!(b[511], 0x5A);
    }

    #[test]
    fn csd_fields_sit_where_the_spec_puts_them() {
        let csd = csd_v2(524288);
        assert_eq!((csd >> 84) & 0xFFF, 0x5B5, "CCC");
        assert_ne!(csd & (1 << (84 + 2)), 0, "class 2: block read");
        assert_eq!((csd >> 80) & 0xF, 9, "READ_BL_LEN");
        assert_eq!((csd >> 48) & 0x3F_FFFF, 511, "C_SIZE: 256 MiB");
        // Everything but C_SIZE as the real board's card reports it.
        let c_size = 0x3F_FFFFu128 << 48;
        let real = 0x400e_0032_5b59_0000_e927_7f80_0a40_0000u128;
        assert_eq!(csd & !c_size, real & !c_size);
    }

    #[test]
    fn scr_claims_spec3_and_cmd23() {
        let mut c = card();
        c.command(55, 0);
        let scr = c.command(51, 0).data.unwrap();
        assert_eq!(&scr[..4], &[0x02, 0x05, 0x80, 0x02]);
    }

    #[test]
    fn acmd22_counts_the_blocks_the_last_write_stored() {
        let mut c = card();
        c.state = CardState::Tran;
        c.command(25, 4);
        for lba in 4..7 {
            c.write_block(lba, &[0x11; 512]);
            c.block_done();
        }
        c.command(12, 0);
        c.command(55, 0);
        let r = c.command(22, 0);
        assert_eq!(r.data.unwrap(), vec![0, 0, 0, 3], "big-endian count");
        assert_eq!(r.r1.unwrap() >> R1_CURRENT_STATE_SHIFT & 0xF, 4, "tran");

        // The next write starts the count again.
        c.command(24, 9);
        c.write_block(9, &[0x22; 512]);
        c.block_done();
        c.command(55, 0);
        assert_eq!(c.command(22, 0).data.unwrap(), vec![0, 0, 0, 1]);
    }

    #[test]
    fn sdio_and_mmc_probes_get_no_response() {
        let mut c = card();
        for cmd in [1, 5, 52] {
            assert!(c.command(cmd, 0).no_response, "CMD{cmd}");
        }
    }
}
