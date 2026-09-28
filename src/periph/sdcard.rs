//! A minimal SD card (SDHC / v3, high-capacity, UHS-I) state machine backed by
//! a block image — enough of the physical-layer command set for the main
//! bootloader to read `start4.elf` off a FAT partition and for Linux's `mmc`
//! core to bring the card up at 1.8 V DDR50 and mount a filesystem. The
//! commands answered are the `match` arms of [`SdCard::command`]; the SDIO and
//! MMC probes Linux sends first get no response, as from a real SD card.
//!
//! The same type plays two other things on the same bus:
//!
//! * the **SDIO** side of the WiFi chip ([`CardKind::Sdio`]), which the legacy
//!   EMMC host has when the SD slot is not muxed to it. It answers CMD5, CMD3,
//!   CMD7, CMD52 and CMD53 and nothing else — it has no memory, so every SD and
//!   MMC command times out. Function 0 is the card's (CCCR, FBRs, CIS chains);
//!   functions 1 and 2 are handed to [`Cyw43455`], which also pulls the card's
//!   interrupt line and names itself in `CCCR_INT_PENDING`.
//! * an **e-MMC** part ([`CardKind::Mmc`]), the flash soldered to a Compute
//!   Module, which answers the JESD84-B51 identification sequence instead: CMD1
//!   rather than ACMD41, a host-picked RCA, CMD8 as SEND_EXT_CSD, and no
//!   application commands, SCR or SD status.
//!
//! Responses are the 32-bit payload the SDHCI RESPONSE registers expose (card
//! response bits `[39:8]`); R2 passes the full 120-bit CID/CSD through
//! [`SdResponse::r2`]. The card's blocks are a [`Disk`], read on demand with
//! writes kept in memory, so the image file is never touched.

use std::collections::BTreeMap;

use crate::periph::cyw43455::Cyw43455;
use crate::periph::disk::Disk;

/// What is on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CardKind {
    #[default]
    Sd,
    Mmc,
    /// The CYW43455 on the legacy EMMC host, which every Pi 4B has
    /// ([`SdCard::sdio`]).
    Sdio,
}

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

#[derive(Debug, Clone, Default)]
pub struct SdResponse {
    /// 48-bit response payload; `None` = no response at all.
    pub r1: Option<u32>,
    /// R2's CID/CSD, aligned with the dropped CRC slot in `[7:0]`.
    pub r2: Option<u128>,
    /// The card did not answer: the host sees a command timeout.
    pub no_response: bool,
    /// Blocks the card will stream (`u32::MAX` = until CMD12).
    pub read_blocks: u32,
    pub read_lba: u32,
    /// Blocks the card will accept (`u32::MAX` = open-ended).
    pub write_blocks: u32,
    pub write_lba: u32,
    /// A register block the card sends instead of image data.
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

const R1_APP_CMD: u32 = 1 << 5;
const R1_ILLEGAL_COMMAND: u32 = 1 << 22;
const R1_READY_FOR_DATA: u32 = 1 << 8;
const R1_CURRENT_STATE_SHIFT: u32 = 9; // bits [12:9]

const OCR_BUSY_DONE: u32 = 1 << 31;
const OCR_CCS: u32 = 1 << 30;
const OCR_S18: u32 = 1 << 24;
const OCR_VOLTAGE_WINDOW: u32 = 0x00FF_8000;
/// OCR `[30:29]` sector addressing; a part of 2 GiB or less never sets it.
const MMC_OCR_SECTOR: u32 = 0b10 << 29;
const BYTE_ADDR_BLOCKS: u64 = 2 * 1024 * 1024 * 1024 / 512;

const EXT_CSD_PARTITION_CONFIG: usize = 179;
const EXT_CSD_BUS_WIDTH: usize = 183;
const EXT_CSD_HS_TIMING: usize = 185;
const EXT_CSD_REV: usize = 192;
const EXT_CSD_CARD_TYPE: usize = 196;
const EXT_CSD_SEC_COUNT: usize = 212;
const EXT_CSD_HC_ERASE_GRP_SIZE: usize = 224;
const EXT_CSD_BOOT_SIZE_MULT: usize = 226;

/// CMD6 group-1 bus speeds. At 3.3 V a UHS card offers only SDR12 and SDR25;
/// the rest appear once CMD11 has moved it to 1.8 V.
const SWITCH_G1_UHS: u16 = 0x8017;
const SWITCH_G1_3V3: u16 = 0x8003;

const IO_OCR_READY: u32 = 1 << 31;
/// I/O functions besides function 0; a Raspberry Pi 4B d03115 enumerates three.
const IO_FUNCTIONS: u32 = 3 << 28;
/// R4 bit 27, a memory part as well: the CYW43455 has none.
const IO_MEMORY_PRESENT: u32 = 1 << 27;

/// Where the CIS chains start; any function-0 address will do.
const CIS_COMMON: u32 = 0x1000;
const CIS_FUNC_STRIDE: u32 = 0x100;

/// A function's block size in its FBR (SDIO simplified specification 6.11),
/// which every CMD53 block transfer is made of.
const FBR_BLKSIZE: u32 = 0x10;

const CCCR_REV: u32 = 0x00;
const CCCR_SD_SPEC: u32 = 0x01;
const CCCR_IO_ENABLE: u32 = 0x02;
const CCCR_IO_READY: u32 = 0x03;
const CCCR_INT_ENABLE: u32 = 0x04;
const CCCR_INT_PENDING: u32 = 0x05;
const CCCR_IO_ABORT: u32 = 0x06;
const CCCR_CAPABILITY: u32 = 0x08;
const CCCR_CIS_PTR: u32 = 0x09;
const CCCR_SPEED: u32 = 0x13;

/// `CCCR_IO_ABORT` bit 3: reset the I/O side.
const IO_ABORT_RES: u8 = 1 << 3;

const IO_ENABLE_FUNC2: u8 = 1 << 2;

/// `CCCR_INT_ENABLE`: bit 0 the master enable, bit *n* function *n*. A card
/// holds the interrupt line off until both are set.
const INT_ENABLE_MASTER: u8 = 1 << 0;
const INT_ENABLE_FUNC1: u8 = 1 << 1;

/// `CCCR_INT_PENDING` bit 1, function 1: which function the MMC core calls.
const INT_PENDING_FUNC1: u8 = 1 << 1;

/// R5's `IO_CURRENT_STATE` in `[13:12]` — *not* where R1 keeps it.
const R5_STATE_SHIFT: u32 = 12;
const R5_STATE_CMD: u32 = 1;
const R5_STATE_TRN: u32 = 2;

const TPL_MANFID: u8 = 0x20;
const TPL_FUNCE: u8 = 0x22;
const TPL_END: u8 = 0xFF;

/// The CIS identity, measured on a Raspberry Pi 4B d03115.
const SDIO_VENDOR: u16 = 0x02d0;
const SDIO_DEVICE: u16 = 0xa9a6;

/// The CMD53 transfer the last command set up.
#[derive(Debug, Clone, Copy)]
struct IoXfer {
    func: u32,
    addr: u32,
    /// The address walks with the data (op code 1).
    incr: bool,
    /// Bytes per block; in byte mode the whole count, as one block.
    unit: u32,
    /// Blocks asked for: on function 2, where a frame ends.
    blocks: u32,
}

/// CMD19's tuning block on a 4-bit bus (SD physical layer 3.01, 4.2.4.5).
const TUNING_BLOCK_4BIT: [u8; 64] = [
    0xff, 0x0f, 0xff, 0x00, 0xff, 0xcc, 0xc3, 0xcc, 0xc3, 0x3c, 0xcc, 0xff, 0xfe, 0xff, 0xfe, 0xef,
    0xff, 0xdf, 0xff, 0xdd, 0xff, 0xfb, 0xff, 0xfb, 0xbf, 0xff, 0x7f, 0xff, 0x77, 0xf7, 0xbd, 0xef,
    0xff, 0xf0, 0xff, 0xf0, 0x0f, 0xfc, 0xcc, 0x3c, 0xcc, 0x33, 0xcc, 0xcf, 0xff, 0xef, 0xff, 0xee,
    0xff, 0xfd, 0xff, 0xfd, 0xdf, 0xff, 0xbf, 0xff, 0xbb, 0xff, 0xf7, 0xff, 0xf7, 0x7f, 0x7b, 0xde,
];

pub struct SdCard {
    disk: Disk,
    kind: CardKind,
    /// An e-MMC's EXT_CSD; empty for an SD card.
    ext_csd: Vec<u8>,
    /// Transfer addresses are byte offsets: where an e-MMC starts, and where
    /// one of 2 GiB or less stays.
    byte_addressed: bool,
    state: CardState,
    rca: u16,
    app_cmd: bool,
    /// OCR busy latch: not-ready once, then ready, so the host's poll loop runs
    /// at least one iteration.
    powered_up: bool,
    s18a_offered: bool,
    /// 1.8 V signalling: survives CMD0, and only a power cycle undoes it.
    signal_1v8: bool,
    wide_bus: bool,
    functions: [u8; 6],
    preset_count: Option<u32>,
    /// Blocks left of a counted transfer; `None` = open-ended, ended by CMD12.
    blocks_left: Option<u32>,
    written_blocks: u32,
    erase_start: u32,
    erase_end: u32,
    cid: u128,
    csd: u128,
    /// [`CardKind::Sdio`]: function 0's address space — CCCR, FBRs and CIS
    /// chains. Sparse; what is not here reads 0, as the unused space does.
    io: BTreeMap<u32, u8>,
    chip: Option<Cyw43455>,
    io_xfer: Option<IoXfer>,
}

impl SdCard {
    pub fn new(mut image: Vec<u8>) -> SdCard {
        if !image.len().is_multiple_of(512) {
            image.resize(image.len().next_multiple_of(512), 0);
        }
        if image.is_empty() {
            image.resize(512, 0);
        }
        SdCard::with_disk(Disk::from_vec(image))
    }

    pub fn with_disk(disk: Disk) -> SdCard {
        SdCard::with_disk_kind(disk, CardKind::Sd)
    }

    pub fn mmc_with_disk(disk: Disk) -> SdCard {
        SdCard::with_disk_kind(disk, CardKind::Mmc)
    }

    pub fn with_disk_kind(disk: Disk, kind: CardKind) -> SdCard {
        let blocks = disk.blocks();
        SdCard {
            disk,
            kind,
            ext_csd: match kind {
                CardKind::Mmc => ext_csd(blocks),
                _ => Vec::new(),
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
                CardKind::Sdio => 0,
            },
            csd: match kind {
                CardKind::Sd => csd_v2(blocks),
                CardKind::Mmc => mmc_csd(),
                CardKind::Sdio => 0,
            },
            io: match kind {
                CardKind::Sdio => io_space(),
                _ => BTreeMap::new(),
            },
            chip: match kind {
                CardKind::Sdio => Some(Cyw43455::new()),
                _ => None,
            },
            io_xfer: None,
        }
    }

    /// The WiFi chip's SDIO side: no memory, three I/O functions.
    pub fn sdio() -> SdCard {
        SdCard::with_disk_kind(Disk::from_vec(vec![0; 512]), CardKind::Sdio)
    }

    /// Whether `ACMD6` has put the card on the four-bit bus.
    pub fn on_wide_bus(&self) -> bool {
        self.wide_bus
    }

    /// Whether `CMD6` has put the card in high speed (group 1 function 1).
    /// A card in default speed is specified only to 25 MHz.
    pub fn high_speed(&self) -> bool {
        self.functions[0] == 1
    }

    pub fn block_count(&self) -> u64 {
        self.disk.blocks()
    }

    pub fn kind(&self) -> CardKind {
        self.kind
    }

    pub fn chip(&self) -> Option<&Cyw43455> {
        self.chip.as_ref()
    }

    /// Bring the WiFi chip's firmware to model time `now_us`.
    #[inline]
    pub fn advance_to(&mut self, now_us: u64) {
        if let Some(chip) = self.chip.as_mut() {
            chip.advance_to(now_us);
        }
    }

    pub fn state(&self) -> CardState {
        self.state
    }

    pub fn signal_1v8(&self) -> bool {
        self.signal_1v8
    }

    pub fn disk(&self) -> &Disk {
        &self.disk
    }

    pub fn read_block(&self, lba: u32, out: &mut [u8; 512]) {
        self.disk.read_block(u64::from(lba), out);
    }

    /// Store one block the host sent, ignored past the end; counts towards
    /// ACMD22.
    pub fn write_block(&mut self, lba: u32, data: &[u8]) {
        let mut block = [0u8; 512];
        if !self.disk.peek_block(u64::from(lba), &mut block) {
            return;
        }
        let n = data.len().min(512);
        block[..n].copy_from_slice(&data[..n]);
        self.disk.write(u64::from(lba), &block);
        self.written_blocks += 1;
    }

    /// One block of the transfer the last command set up, `index` blocks in.
    pub fn transfer_read(&mut self, index: u32, out: &mut [u8; 512]) {
        let Some(x) = self.io_xfer.filter(|_| self.kind == CardKind::Sdio) else {
            self.read_block(index, out);
            return;
        };
        let at = self.io_transfer_addr(&x, index);
        let n = (x.unit as usize).min(out.len());
        out.fill(0);
        if x.func == 0 {
            // Function 0 has no chip behind it: walk the card's own space.
            for (i, b) in out[..n].iter_mut().enumerate() {
                *b = self
                    .io
                    .get(&at.wrapping_add(i as u32))
                    .copied()
                    .unwrap_or(0);
            }
        } else if let Some(chip) = self.chip.as_mut() {
            chip.read_io(x.func, at, &mut out[..n]);
        }
    }

    pub fn transfer_write(&mut self, index: u32, data: &[u8]) {
        let Some(x) = self.io_xfer.filter(|_| self.kind == CardKind::Sdio) else {
            self.write_block(index, data);
            return;
        };
        let at = self.io_transfer_addr(&x, index);
        let n = (x.unit as usize).min(data.len());
        if x.func == 0 {
            for (i, b) in data[..n].iter().enumerate() {
                self.io_write(0, at.wrapping_add(i as u32), *b);
            }
        } else if let Some(chip) = self.chip.as_mut() {
            chip.write_io(x.func, at, &data[..n]);
            // The last block ends the transfer, and a frame with it.
            if index + 1 >= x.blocks {
                chip.end_io(x.func);
            }
        }
    }

    /// One data block went over the bus; a counted transfer ends at its last.
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

    /// Card VDD removed: the signalling voltage included, everything restarts.
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
        if self.kind == CardKind::Sdio {
            self.sdio_reset();
        }
    }

    /// The block a transfer argument names; a small e-MMC counts bytes.
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

    /// Dispatch one command; `acmd` is the host's own CMD55 latch.
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

        if self.kind == CardKind::Sdio {
            // An I/O-only card has no CID, CSD or memory.
            return self.sdio_command(cmd, arg);
        }

        match cmd {
            0 => {
                // GO_IDLE_STATE; the signalling voltage is not reset (4.2.4.3).
                self.state = CardState::Idle;
                self.powered_up = false;
                self.s18a_offered = false;
                self.preset_count = None;
                self.blocks_left = None;
                self.functions = [0; 6];
                self.wide_bus = false;
                SdResponse::none()
            }
            // The MMC and SDIO probes: an SD memory card does not answer.
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
                // R6: RCA above the mirrored status bits.
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
                // VOLTAGE_SWITCH, legal only after an ACMD41 that accepted
                // S18R. The card holds CMD/DAT low until the host is at 1.8 V.
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
                // READ_SINGLE / READ_MULTIPLE_BLOCK
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
                // SEND_TUNING_BLOCK; the card stays in tran.
                SdResponse::with_data(self.status(), TUNING_BLOCK_4BIT.to_vec())
            }
            23 => {
                // SET_BLOCK_COUNT -> R1
                self.preset_count = Some(arg & 0xFFFF);
                SdResponse::r1(self.status())
            }
            24 | 25 => {
                // WRITE_BLOCK / WRITE_MULTIPLE_BLOCK
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
                // ERASE; erased blocks read back as zeros.
                let (lo, hi) = (self.erase_start, self.erase_end);
                if lo <= hi {
                    self.disk.zero(u64::from(lo), u64::from(hi));
                }
                SdResponse::r1(self.status())
            }
            55 => {
                // APP_CMD; the next command is an ACMD.
                self.app_cmd = true;
                SdResponse::r1(self.status() | R1_APP_CMD)
            }
            _ => SdResponse::r1(self.status()),
        }
    }

    /// Dispatch one command to the WiFi chip's SDIO side.
    fn sdio_command(&mut self, cmd: u8, arg: u32) -> SdResponse {
        match cmd {
            0 => {
                // GO_IDLE_STATE, which an I/O-only card ignores.
                SdResponse::none()
            }
            3 => {
                // SEND_RELATIVE_ADDR -> R6: the card picks the address.
                self.rca = 1;
                self.state = CardState::Stby;
                SdResponse::r1(u32::from(self.rca) << 16)
            }
            5 => {
                // IO_SEND_OP_COND; ready on the second ask, so the host's poll
                // loop runs at least once.
                let ocr = if self.powered_up {
                    self.state = CardState::Ready;
                    IO_OCR_READY | IO_FUNCTIONS | OCR_VOLTAGE_WINDOW
                } else {
                    self.powered_up = true;
                    IO_FUNCTIONS | OCR_VOLTAGE_WINDOW
                };
                debug_assert_eq!(ocr & IO_MEMORY_PRESENT, 0, "the CYW43455 has no memory");
                SdResponse::r1(ocr)
            }
            7 => {
                // SELECT/DESELECT_CARD -> R1b.
                self.state = if arg >> 16 == u32::from(self.rca) && self.rca != 0 {
                    CardState::Tran
                } else {
                    CardState::Stby
                };
                SdResponse::r1(self.status())
            }
            52 => {
                // IO_RW_DIRECT: one byte at `[25:9]` of function `[30:28]`,
                // written when `[31]` is set.
                let write = arg >> 31 != 0;
                let func = (arg >> 28) & 7;
                let addr = (arg >> 9) & 0x1_FFFF;
                let value = (arg & 0xFF) as u8;
                let byte = if write {
                    self.io_write(func, addr, value);
                    // The read-after-write flag asks for the new value.
                    if arg >> 27 & 1 != 0 {
                        self.io_read(func, addr)
                    } else {
                        0
                    }
                } else {
                    self.io_read(func, addr)
                };
                SdResponse::r1(self.r5(byte))
            }
            53 => {
                // IO_RW_EXTENDED: write, function, block mode, op code,
                // address, then a count where 0 means 512.
                let write = arg >> 31 != 0;
                let func = (arg >> 28) & 7;
                let block_mode = (arg >> 27) & 1 != 0;
                let incr = (arg >> 26) & 1 != 0;
                let addr = (arg >> 9) & 0x1_FFFF;
                let count = arg & 0x1FF;
                let count = if count == 0 { 512 } else { count };
                let (blocks, unit) = if block_mode {
                    (count, self.io_block_size(func))
                } else {
                    (1, count)
                };
                self.io_xfer = Some(IoXfer {
                    func,
                    addr,
                    incr,
                    unit,
                    blocks,
                });
                // No data byte in R5: the bytes go over the data lines.
                let r1 = Some(self.r5(0));
                if write {
                    SdResponse {
                        r1,
                        write_blocks: blocks,
                        write_lba: 0,
                        ..Default::default()
                    }
                } else {
                    SdResponse {
                        r1,
                        read_blocks: blocks,
                        read_lba: 0,
                        ..Default::default()
                    }
                }
            }
            // The memory card's commands, which an I/O-only card lacks.
            _ => SdResponse::silent(),
        }
    }

    /// R5: the card state in `[13:12]`, the byte read in `[7:0]`.
    fn r5(&self, byte: u8) -> u32 {
        let state = if self.state == CardState::Tran {
            R5_STATE_TRN
        } else {
            R5_STATE_CMD
        };
        state << R5_STATE_SHIFT | u32::from(byte)
    }

    /// The I/O block size the host set for `func` in its FBR.
    fn io_block_size(&self, func: u32) -> u32 {
        let fbr = func * 0x100 + FBR_BLKSIZE;
        let size = u32::from(self.io_read(0, fbr)) | u32::from(self.io_read(0, fbr + 1)) << 8;
        // Unset means 512, the maximum this card's CIS offers.
        if size == 0 {
            512
        } else {
            size
        }
    }

    fn io_transfer_addr(&self, x: &IoXfer, index: u32) -> u32 {
        if x.incr {
            x.addr.wrapping_add(index * x.unit)
        } else {
            x.addr
        }
    }

    /// Put the I/O side back the way it comes up. The chip goes with it: the
    /// driver sets `SDIO_CCCR_BRCM_CARDCTRL_WLANRESET` precisely so an I/O
    /// reset resets the WLAN backplane too.
    fn sdio_reset(&mut self) {
        self.io = io_space();
        self.chip = Some(Cyw43455::new());
        self.io_xfer = None;
        self.state = CardState::Idle;
        self.rca = 0;
        self.powered_up = false;
    }

    /// The byte at `addr` in function `func`'s address space.
    fn io_read(&self, func: u32, addr: u32) -> u8 {
        if func != 0 {
            return match self.chip.as_ref() {
                Some(chip) => chip.read_byte(func, addr),
                None => 0,
            };
        }
        if addr == CCCR_INT_PENDING {
            // Not stored: the chip answers which functions are pending.
            return if self.io_irq_raw() {
                INT_PENDING_FUNC1
            } else {
                0
            };
        }
        self.io.get(&addr).copied().unwrap_or(0)
    }

    /// The card is pulling the SDIO interrupt line (DAT[1]), gated by
    /// `CCCR_INT_ENABLE`: the chip may have something to say long before the
    /// host has asked to be told.
    pub fn io_irq(&self) -> bool {
        let en = self.io.get(&CCCR_INT_ENABLE).copied().unwrap_or(0);
        en & (INT_ENABLE_MASTER | INT_ENABLE_FUNC1) == (INT_ENABLE_MASTER | INT_ENABLE_FUNC1)
            && self.io_irq_raw()
    }

    fn io_irq_raw(&self) -> bool {
        self.chip.as_ref().is_some_and(|c| c.irq_asserted())
    }

    /// Write one byte of function `func`'s space.
    fn io_write(&mut self, func: u32, addr: u32, value: u8) {
        if func != 0 {
            if let Some(chip) = self.chip.as_mut() {
                chip.write_byte(func, addr, value);
            }
            return;
        }
        match addr {
            // Read-only: revision, capabilities, CIS pointers and tuples.
            CCCR_REV | CCCR_SD_SPEC | CCCR_CAPABILITY => {}
            CCCR_IO_ENABLE => {
                let was = self.io_read(0, CCCR_IO_ENABLE);
                self.io.insert(CCCR_IO_ENABLE, value);
                // Ready as soon as it is asked for.
                self.io.insert(CCCR_IO_READY, value);
                // Function 2 coming up is what the firmware waits for.
                if value & !was & IO_ENABLE_FUNC2 != 0 {
                    if let Some(chip) = self.chip.as_mut() {
                        chip.enable_f2();
                    }
                }
            }
            CCCR_IO_READY | CCCR_INT_PENDING => {}
            CCCR_IO_ABORT => {
                // Self-clearing; bit 3 puts the I/O side back to idle.
                if value & IO_ABORT_RES != 0 {
                    self.sdio_reset();
                }
            }
            _ if (CCCR_CIS_PTR..CCCR_CIS_PTR + 3).contains(&addr) => {}
            _ if addr >= CIS_COMMON => {}
            _ => {
                self.io.insert(addr, value);
            }
        }
    }

    /// What an e-MMC answers differently; `None` falls through to the shared
    /// arms.
    fn mmc_command(&mut self, cmd: u8, arg: u32) -> Option<SdResponse> {
        match cmd {
            1 => {
                // SEND_OP_COND: busy once, then ready. A part over 2 GiB
                // answers sector mode whatever the host offered (JESD84-B51
                // 7.4.3), and the stock firmware follows.
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
                // SET_RELATIVE_ADDR: the host picks the address.
                self.rca = (arg >> 16) as u16;
                self.state = CardState::Stby;
                Some(SdResponse::r1(self.status()))
            }
            6 => {
                // SWITCH: access, EXT_CSD index, value. Access 0 selects a
                // command set, which this part does not have.
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
                // SEND_EXT_CSD (CMD8 is SEND_IF_COND on an SD card).
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
                // SD_STATUS: only DAT_BUS_WIDTH is filled in, and a zero AU
                // size leaves the host's erase geometry alone.
                let mut ssr = vec![0u8; 64];
                if self.wide_bus {
                    ssr[0] = 0x80;
                }
                SdResponse::with_data(self.status(), ssr)
            }
            22 => {
                // SEND_NUM_WR_BLOCKS, most significant byte first. edk2's
                // MmcDxe asks after every write and fails it without an answer.
                SdResponse::with_data(self.status(), self.written_blocks.to_be_bytes().to_vec())
            }
            41 => {
                // SD_SEND_OP_COND: busy once, then ready with CCS, plus S18A if
                // the host asked for 1.8 V and the card is still at 3.3 V.
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
                // SEND_SCR: SD_SPEC 2 with SD_SPEC3, without which Linux does
                // not read the CMD6 bus modes; 1- and 4-bit bus, CMD23.
                let scr = vec![0x02, 0x05, 0x80, 0x02, 0, 0, 0, 0];
                SdResponse::with_data(self.status(), scr)
            }
            _ => SdResponse::r1(self.status()),
        }
    }

    /// CMD6: the switch-function status, selecting functions in set mode.
    fn switch_func(&mut self, arg: u32) -> Vec<u8> {
        let set = arg & (1 << 31) != 0;
        let g1 = if self.signal_1v8 {
            SWITCH_G1_UHS
        } else {
            SWITCH_G1_3V3
        };
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

/// Function 0's address space: the CCCR, one FBR per function, and the CIS
/// chain each points at. The identity and function count are measured; the rest
/// is the smallest set of values `mmc_sdio_init_card` and `sdio_read_cis`
/// accept — a card claiming SDIO 3.00 needs a `FUNCE` tuple of at least 42
/// bytes per function or the core rejects it.
fn io_space() -> BTreeMap<u32, u8> {
    let mut io = BTreeMap::new();
    let mut put = |addr: u32, bytes: &[u8]| {
        for (i, b) in bytes.iter().enumerate() {
            io.insert(addr + i as u32, *b);
        }
    };

    put(CCCR_REV, &[0x43]);
    put(CCCR_SD_SPEC, &[0x03]);
    // Card capability: direct commands mid-transfer, multi-block, read wait,
    // 4-bit multiple-block interrupts.
    put(CCCR_CAPABILITY, &[0x17]);
    put(CCCR_CIS_PTR, &CIS_COMMON.to_le_bytes()[..3]);
    // Bus speed: high speed supported, not yet selected, as measured.
    put(CCCR_SPEED, &[0x01]);

    put(
        CIS_COMMON,
        &[
            TPL_MANFID,
            4,
            SDIO_VENDOR as u8,
            (SDIO_VENDOR >> 8) as u8,
            SDIO_DEVICE as u8,
            (SDIO_DEVICE >> 8) as u8,
            // Function 0's extended tuple: block size and top rate.
            TPL_FUNCE,
            4,
            0x00,
            0x00,
            0x02,
            0x32,
            TPL_END,
        ],
    );
    for func in 1..=(IO_FUNCTIONS >> 28) {
        let fbr = func * 0x100;
        let cis = CIS_COMMON + func * CIS_FUNC_STRIDE;
        // Interface code 0, and the CIS pointer at +9.
        put(fbr, &[0x00]);
        put(fbr + 0x09, &cis.to_le_bytes()[..3]);
        // A function's extended tuple, at least 42 bytes for SDIO 3.00.
        let mut funce = vec![0u8; 42];
        funce[0] = 0x01; // type 1: a function's own tuple
        funce[12] = 0x00;
        funce[13] = 0x02; // 512-byte blocks
        let mut tuple = vec![TPL_FUNCE, funce.len() as u8];
        tuple.extend_from_slice(&funce);
        tuple.push(TPL_END);
        put(cis, &tuple);
    }
    io
}

/// An invented CID, aligned with the CRC slot zeroed.
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

/// A high-capacity CSD v2.0 over `blocks` sectors. Apart from C_SIZE every
/// field is what the SD spec fixes for CSD 2.0, which is also what a real
/// board's card reports.
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

/// An e-MMC CID; its PNM is 6 characters, not 5, and CBX says it is embedded.
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

/// An e-MMC CSD (JESD84-B51 7.3): the real capacity is in the EXT_CSD, so
/// C_SIZE stays saturated and the host reads SEC_COUNT.
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

/// An e-MMC's EXT_CSD: the bytes a bootloader or Linux reads, zero elsewhere.
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

    fn init_ocr(c: &mut SdCard, arg: u32) -> u32 {
        c.command(0, 0);
        let mut ocr = 0;
        for _ in 0..2 {
            c.command(55, 0);
            ocr = c.command(41, arg).r1.unwrap();
        }
        ocr
    }

    fn io_arg(write: bool, func: u32, addr: u32, value: u8) -> u32 {
        (u32::from(write) << 31) | (func << 28) | (addr << 9) | u32::from(value)
    }

    fn io_read_byte(c: &mut SdCard, func: u32, addr: u32) -> u8 {
        let r5 = c.command(52, io_arg(false, func, addr, 0)).r1.unwrap();
        // Nothing in the flags but the state: an error bit here has the MMC
        // core give up with -EIO.
        assert_eq!(r5 & 0xFF00 & !(3 << R5_STATE_SHIFT), 0, "R5 flags {r5:#x}");
        r5 as u8
    }

    /// `mmc_sdio_init_card`: CMD5 twice, CMD3, CMD7.
    fn sdio_up() -> SdCard {
        let mut c = SdCard::sdio();
        let first = c.command(5, 0).r1.unwrap();
        assert_eq!(first & IO_OCR_READY, 0, "the card is still ramping up");
        let ocr = c.command(5, OCR_VOLTAGE_WINDOW).r1.unwrap();
        assert_eq!(ocr & IO_OCR_READY, IO_OCR_READY);
        assert_eq!(ocr >> 28 & 7, 3, "three I/O functions, as on a Pi 4B");
        assert_eq!(ocr & IO_MEMORY_PRESENT, 0, "no memory part");
        let rca = c.command(3, 0).r1.unwrap() >> 16;
        assert_eq!(rca, 1);
        c.command(7, rca << 16);
        assert_eq!(c.state, CardState::Tran);
        c
    }

    #[test]
    fn the_sdio_card_answers_the_identification_sequence() {
        sdio_up();
    }

    #[test]
    fn it_has_no_memory_side() {
        let mut c = sdio_up();
        for cmd in [1, 2, 8, 9, 41, 55] {
            assert!(c.command(cmd, 0).no_response, "CMD{cmd} was answered");
        }
    }

    #[test]
    fn the_cccr_reads_back() {
        let mut c = sdio_up();
        let rev = io_read_byte(&mut c, 0, CCCR_REV);
        assert_eq!(rev & 0x0f, 3);
        assert_eq!(rev >> 4, 4);
        c.command(52, io_arg(true, 0, CCCR_IO_ENABLE, 0x02));
        assert_eq!(io_read_byte(&mut c, 0, CCCR_IO_ENABLE), 0x02);
        assert_eq!(io_read_byte(&mut c, 0, CCCR_IO_READY), 0x02);
        c.command(52, io_arg(true, 0, CCCR_REV, 0xff));
        assert_eq!(io_read_byte(&mut c, 0, CCCR_REV), rev);
    }

    #[test]
    fn the_cis_names_the_chip() {
        let mut c = sdio_up();
        let mut ptr = 0u32;
        for i in 0..3 {
            ptr |= u32::from(io_read_byte(&mut c, 0, CCCR_CIS_PTR + i)) << (8 * i);
        }
        assert_eq!(ptr, CIS_COMMON);
        assert_eq!(io_read_byte(&mut c, 0, ptr), TPL_MANFID);
        assert_eq!(io_read_byte(&mut c, 0, ptr + 1), 4);
        let vendor = u16::from(io_read_byte(&mut c, 0, ptr + 2))
            | u16::from(io_read_byte(&mut c, 0, ptr + 3)) << 8;
        let device = u16::from(io_read_byte(&mut c, 0, ptr + 4))
            | u16::from(io_read_byte(&mut c, 0, ptr + 5)) << 8;
        assert_eq!((vendor, device), (0x02d0, 0xa9a6));
        for func in 1..=3 {
            let mut fptr = 0u32;
            for i in 0..3 {
                fptr |=
                    u32::from(io_read_byte(&mut c, 0, func * 0x100 + CCCR_CIS_PTR + i)) << (8 * i);
            }
            assert_eq!(io_read_byte(&mut c, 0, fptr), TPL_FUNCE);
            let len = io_read_byte(&mut c, 0, fptr + 1);
            assert!(len >= 42, "function {func} tuple is {len} bytes");
            let blksize = u32::from(io_read_byte(&mut c, 0, fptr + 2 + 12))
                | u32::from(io_read_byte(&mut c, 0, fptr + 2 + 13)) << 8;
            assert_eq!(blksize, 512);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn io_ext_arg(write: bool, func: u32, block_mode: bool, incr: bool, addr: u32, n: u32) -> u32 {
        (u32::from(write) << 31)
            | (func << 28)
            | (u32::from(block_mode) << 27)
            | (u32::from(incr) << 26)
            | (addr << 9)
            | (n & 0x1FF)
    }

    /// The card with function 1's block size agreed and the window aimed.
    fn sdio_with_window(window: u32) -> SdCard {
        let mut c = sdio_up();
        for (i, b) in 64u16.to_le_bytes().iter().enumerate() {
            c.command(52, io_arg(true, 0, 0x110 + i as u32, *b));
        }
        let v = (window & 0xFFFF_8000) >> 8;
        for i in 0..3 {
            c.command(52, io_arg(true, 1, 0x1000A + i, (v >> (8 * i)) as u8));
        }
        c
    }

    #[test]
    fn cmd53_reads_the_chip_id_through_the_window() {
        let mut c = sdio_with_window(0x1800_0000);
        let r = c.command(53, io_ext_arg(false, 1, false, true, 0x8000, 4));
        assert!(!r.no_response, "CMD53 was not answered");
        assert_eq!((r.read_blocks, r.read_lba), (1, 0), "one block of bytes");
        let mut block = [0u8; 512];
        c.transfer_read(0, &mut block);
        let id = u32::from_le_bytes(block[..4].try_into().unwrap());
        assert_eq!(id & 0xFFFF, 0x4345, "chip id");
        assert_ne!(id, 0xFFFF_FFFF, "which the driver reads as a dead bus");
    }

    #[test]
    fn cmd53_block_mode_walks_the_window_and_byte_mode_counts_bytes() {
        let mut c = sdio_with_window(crate::periph::cyw43455::RAM_BASE);
        let r = c.command(53, io_ext_arg(true, 1, true, true, 0x8000, 4));
        assert_eq!(r.write_blocks, 4);
        for b in 0..4u32 {
            c.transfer_write(b, &[b as u8 + 1; 64]);
        }
        let r = c.command(53, io_ext_arg(false, 1, true, true, 0x8000, 4));
        assert_eq!(r.read_blocks, 4);
        for b in 0..4u32 {
            let mut block = [0u8; 512];
            c.transfer_read(b, &mut block);
            assert_eq!(block[..64], [b as u8 + 1; 64], "block {b}");
            assert_eq!(block[64], 0, "only the block's own bytes");
        }
        let r = c.command(53, io_ext_arg(false, 1, false, true, 0x8000, 7));
        assert_eq!((r.read_blocks, r.read_lba), (1, 0));
        let mut block = [0u8; 512];
        c.transfer_read(0, &mut block);
        assert_eq!(block[..7], [1, 1, 1, 1, 1, 1, 1]);
        assert_eq!(block[7], 0, "the eighth byte was not asked for");
        c.command(53, io_ext_arg(false, 1, false, true, 0x8000, 0));
        c.transfer_read(0, &mut block);
        assert_eq!(block[..4], [1; 4]);
        assert_eq!(block[192], 4, "512 bytes: past the fourth block's start");
        assert_eq!(block[256], 0, "...and past everything that was written");
    }

    #[test]
    fn a_fixed_address_transfer_stays_on_one_register() {
        let mut c = sdio_with_window(crate::periph::cyw43455::RAM_BASE);
        c.command(53, io_ext_arg(true, 1, true, false, 0x8000, 4));
        for b in 0..4u32 {
            c.transfer_write(b, &[b as u8 + 1; 64]);
        }
        c.command(53, io_ext_arg(false, 1, true, true, 0x8000, 2));
        let mut block = [0u8; 512];
        c.transfer_read(0, &mut block);
        assert_eq!(block[..64], [4; 64], "the last write is what is there");
        c.transfer_read(1, &mut block);
        assert_eq!(block[..64], [0; 64], "and nothing walked past it");
    }

    #[test]
    fn the_window_is_the_three_sbaddr_bytes() {
        let mut c = sdio_with_window(crate::periph::cyw43455::RAM_BASE);
        c.command(53, io_ext_arg(true, 1, false, true, 0x8000, 4));
        c.transfer_write(0, &[0xAA; 4]);
        let mut c = sdio_with_window(crate::periph::cyw43455::RAM_BASE + 0x8000);
        c.command(53, io_ext_arg(false, 1, false, true, 0x8000, 4));
        let mut block = [0u8; 512];
        c.transfer_read(0, &mut block);
        assert_eq!(block[..4], [0; 4], "a different 32 KiB window");
        let mut c = sdio_with_window(crate::periph::cyw43455::RAM_BASE);
        c.command(53, io_ext_arg(true, 1, false, true, 0x8004, 4));
        c.transfer_write(0, &[0xBB; 4]);
        c.command(53, io_ext_arg(false, 1, false, true, 0x8000, 8));
        c.transfer_read(0, &mut block);
        assert_eq!(block[..8], [0, 0, 0, 0, 0xBB, 0xBB, 0xBB, 0xBB]);
    }

    #[test]
    fn function_2_is_not_the_backplane() {
        let mut c = sdio_with_window(crate::periph::cyw43455::RAM_BASE);
        c.command(53, io_ext_arg(true, 1, false, true, 0x8000, 4));
        c.transfer_write(0, &[0xCD; 4]);
        c.command(53, io_ext_arg(false, 2, false, true, 0x8000, 4));
        let mut block = [0xFFu8; 512];
        c.transfer_read(0, &mut block);
        assert_eq!(block[..4], [0; 4]);
        c.command(53, io_ext_arg(true, 2, false, true, 0x8000, 4));
        c.transfer_write(0, &[0x11; 4]);
        c.command(53, io_ext_arg(false, 1, false, true, 0x8000, 4));
        c.transfer_read(0, &mut block);
        assert_eq!(block[..4], [0xCD; 4], "function 1 still has its bytes");
    }

    #[test]
    fn the_abort_register_resets_the_io_side() {
        let mut c = sdio_up();
        c.command(52, io_arg(true, 0, CCCR_IO_ENABLE, 0x02));
        c.command(52, io_arg(true, 0, CCCR_IO_ABORT, IO_ABORT_RES));
        assert_eq!(c.state, CardState::Idle);
        assert_eq!(c.rca, 0);
        assert_eq!(io_read_byte(&mut c, 0, CCCR_IO_ABORT), 0);
        assert_eq!(io_read_byte(&mut c, 0, CCCR_IO_ENABLE), 0);
    }

    fn mmc(blocks: usize) -> SdCard {
        SdCard::mmc_with_disk(Disk::from_vec(vec![0u8; blocks * 512]))
    }

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
        let mut c = mmc(2048);
        assert_eq!(mmc_ocr(&mut c, 0x4010_0000) & MMC_OCR_SECTOR, 0);
        c.command(3, 0x0001_0000);
        c.command(7, 0x0001_0000);
        assert_eq!(c.command(17, 4 * 512).read_lba, 4);

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

        assert_eq!(init_ocr(&mut c, 0x41FF_8000) & OCR_S18, 0);
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
