//! BCM2711 EMMC2 — the Arasan SDHCI v3.00 host at `0x7E34_0000` that the main
//! bootloader and Linux's `sdhci-iproc` drive. Registers and measured reset
//! values: `specs/emmc2.toml`.
//!
//! A working command engine wired to an [`SdCard`], with three data paths: PIO
//! through the Buffer Data Port (what the firmware uses), SDMA, and 32-bit
//! ADMA2 (what Linux uses). Also auto-CMD12 / auto-CMD23, CMD11 1.8 V
//! switching, tuning, the interrupt output and status gating by
//! `INT_STATUS_EN`.
//!
//! Three deliberate behaviours:
//!
//! * Between PIO read blocks `BUF_READ_EN` drops, as on silicon: the next block
//!   arrives on the second status poll or 21 µs later, whichever comes first.
//!   A driver pacing on a `BUF_READ_RDY` it never clears would otherwise read
//!   the gap.
//! * DMA needs the RAM, so a command that starts one only marks it pending;
//!   [`crate::machine::Machine`] then calls [`Emmc2::run_dma`] right after the
//!   register write, which completes the whole transfer at once.
//! * The same engine is the chip's legacy EMMC at `0x7E30_0000`
//!   ([`Emmc2::new_legacy`], `specs/emmc.toml`) — the WiFi chip's SDIO host and
//!   the host 2020-era bootcode reads the card through. Nothing is on its bus
//!   (the SD slot reaches it only through a mux the model does not follow), so
//!   every command expecting a response times out there.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};
use crate::mem::Ram;
use crate::periph::sdcard::SdCard;

// The read-only identity, the idle `PRESENT_STATE` and `HOST_CONTROL.FIXED`
// are measured on a Raspberry Pi 4B d03115.
use crate::spec::emmc as legacy;
use crate::spec::emmc2::{
    ADMA_ADDR, ADMA_ERROR, ADMA_ERROR_LEN_MISMATCH_MASK as ADMA_LEN_MISMATCH, ARGUMENT,
    BLOCK_SIZE_COUNT, BUFFER_DATA, CAPABILITIES_0, CAPABILITIES_0_RESET as CAPS0, CAPABILITIES_1,
    CAPABILITIES_1_RESET as CAPS1, CLOCK_CONTROL, CLOCK_CONTROL_INTERNAL_EN_MASK as CLK_INTLEN,
    CLOCK_CONTROL_SD_EN_MASK as CLK_SD_EN, CLOCK_CONTROL_SRST_ALL_MASK as SRST_ALL,
    CLOCK_CONTROL_SRST_CMD_MASK as SRST_CMD, CLOCK_CONTROL_SRST_DATA_MASK as SRST_DATA,
    CLOCK_CONTROL_STABLE_MASK as CLK_STABLE, CMD_XFER,
    CMD_XFER_AUTO_CMD_SHIFT as TM_AUTO_CMD_SHIFT,
    CMD_XFER_BLOCK_COUNT_EN_MASK as TM_BLOCK_COUNT_EN, CMD_XFER_DMA_MASK as TM_DMA,
    CMD_XFER_MULTI_MASK as TM_MULTI, CMD_XFER_READ_MASK as TM_READ, CONTROLLER_VERSION,
    CONTROLLER_VERSION_RESET as VERSION, HOST_CONTROL, HOST_CONTROL2,
    HOST_CONTROL2_EXEC_TUNING_MASK as HC2_EXEC_TUNING, HOST_CONTROL2_SIGNAL_1V8_MASK as HC2_1V8,
    HOST_CONTROL2_TUNED_CLK_MASK as HC2_TUNED_CLK, HOST_CONTROL_BUS_POWER_MASK as HC_BUS_POWER,
    HOST_CONTROL_DMA_SELECT_SHIFT as HC_DMA_SHIFT, HOST_CONTROL_FIXED_MASK as HOST_CONTROL_FIXED,
    INT_SIGNAL_EN, INT_STATUS, INT_STATUS_BLOCK_GAP_MASK as INT_BLOCK_GAP,
    INT_STATUS_BUF_READ_RDY_MASK as INT_BUF_READ_RDY,
    INT_STATUS_BUF_WRITE_RDY_MASK as INT_BUF_WRITE_RDY, INT_STATUS_CARD_MASK as INT_CARD,
    INT_STATUS_CMD_COMPLETE_MASK as INT_CMD_COMPLETE, INT_STATUS_DMA_MASK as INT_DMA,
    INT_STATUS_EN, INT_STATUS_ERROR_MASK as INT_ERROR, INT_STATUS_ERR_ADMA_MASK as INT_ERR_ADMA,
    INT_STATUS_ERR_CMD_TIMEOUT_MASK as INT_ERR_CMD_TIMEOUT,
    INT_STATUS_XFER_COMPLETE_MASK as INT_XFER_COMPLETE, MAX_CURRENT,
    MAX_CURRENT_RESET as MAX_CURRENT_VALUE, PRESENT_STATE,
    PRESENT_STATE_BUF_READ_EN_MASK as PS_BUF_READ_EN,
    PRESENT_STATE_BUF_WRITE_EN_MASK as PS_BUF_WRITE_EN, PRESENT_STATE_CMD_LINE_MASK,
    PRESENT_STATE_DAT_INHIBIT_MASK as PS_DAT_INHIBIT, PRESENT_STATE_DAT_LINES_MASK,
    PRESENT_STATE_RESET as PRESENT_STATE_IDLE, RESPONSE0, RESPONSE1, RESPONSE2, RESPONSE3,
    SDMA_ADDR,
};
use crate::spec::Coverage;

/// Every register in `specs/emmc2.toml` is modelled.
pub const COVERAGE: Coverage = Coverage {
    block: "emmc2",
    decoded: &[
        SDMA_ADDR,
        BLOCK_SIZE_COUNT,
        ARGUMENT,
        CMD_XFER,
        RESPONSE0,
        RESPONSE1,
        RESPONSE2,
        RESPONSE3,
        BUFFER_DATA,
        PRESENT_STATE,
        HOST_CONTROL,
        CLOCK_CONTROL,
        INT_STATUS,
        INT_STATUS_EN,
        INT_SIGNAL_EN,
        HOST_CONTROL2,
        CAPABILITIES_0,
        CAPABILITIES_1,
        MAX_CURRENT,
        ADMA_ERROR,
        ADMA_ADDR,
        CONTROLLER_VERSION,
    ],
};

/// The legacy EMMC: the command engine, with unmeasured identity registers.
pub const COVERAGE_LEGACY: Coverage = Coverage {
    block: "emmc",
    decoded: &[
        legacy::SDMA_ADDR,
        legacy::BLOCK_SIZE_COUNT,
        legacy::ARGUMENT,
        legacy::CMD_XFER,
        legacy::RESPONSE0,
        legacy::RESPONSE1,
        legacy::RESPONSE2,
        legacy::RESPONSE3,
        legacy::BUFFER_DATA,
        legacy::PRESENT_STATE,
        legacy::HOST_CONTROL,
        legacy::CLOCK_CONTROL,
        legacy::INT_STATUS,
        legacy::INT_STATUS_EN,
        legacy::INT_SIGNAL_EN,
        legacy::HOST_CONTROL2,
    ],
};

// The engine decodes by EMMC2's offsets and bits, so the legacy spec has to
// agree with them on everything it lists.
const _: () = {
    let same = [
        (legacy::SDMA_ADDR, SDMA_ADDR),
        (legacy::BLOCK_SIZE_COUNT, BLOCK_SIZE_COUNT),
        (legacy::ARGUMENT, ARGUMENT),
        (legacy::CMD_XFER, CMD_XFER),
        (legacy::RESPONSE0, RESPONSE0),
        (legacy::RESPONSE1, RESPONSE1),
        (legacy::RESPONSE2, RESPONSE2),
        (legacy::RESPONSE3, RESPONSE3),
        (legacy::BUFFER_DATA, BUFFER_DATA),
        (legacy::PRESENT_STATE, PRESENT_STATE),
        (legacy::PRESENT_STATE_RESET, PRESENT_STATE_IDLE),
        (legacy::HOST_CONTROL, HOST_CONTROL),
        (legacy::CLOCK_CONTROL, CLOCK_CONTROL),
        (legacy::CLOCK_CONTROL_INTERNAL_EN_MASK, CLK_INTLEN),
        (legacy::CLOCK_CONTROL_STABLE_MASK, CLK_STABLE),
        (legacy::CLOCK_CONTROL_SD_EN_MASK, CLK_SD_EN),
        (legacy::CLOCK_CONTROL_SRST_ALL_MASK, SRST_ALL),
        (legacy::CLOCK_CONTROL_SRST_CMD_MASK, SRST_CMD),
        (legacy::CLOCK_CONTROL_SRST_DATA_MASK, SRST_DATA),
        (legacy::INT_STATUS, INT_STATUS),
        (legacy::INT_STATUS_CMD_COMPLETE_MASK, INT_CMD_COMPLETE),
        (legacy::INT_STATUS_ERROR_MASK, INT_ERROR),
        (legacy::INT_STATUS_ERR_CMD_TIMEOUT_MASK, INT_ERR_CMD_TIMEOUT),
        (legacy::INT_STATUS_EN, INT_STATUS_EN),
        (legacy::INT_SIGNAL_EN, INT_SIGNAL_EN),
        (legacy::HOST_CONTROL2, HOST_CONTROL2),
        (legacy::CAPABILITIES_0, CAPABILITIES_0),
        (legacy::CAPABILITIES_1, CAPABILITIES_1),
        (legacy::MAX_CURRENT, MAX_CURRENT),
        (legacy::CONTROLLER_VERSION, CONTROLLER_VERSION),
    ];
    let mut i = 0;
    while i < same.len() {
        assert!(same[i].0 == same[i].1);
        i += 1;
    }
};

/// What a host says about itself: the read-only registers that tell the
/// chip's two Arasan hosts apart.
#[derive(Clone, Copy)]
struct Identity {
    name: &'static str,
    caps0: u32,
    caps1: u32,
    max_current: u32,
    version: u32,
    /// HOST_CONTROL bits that read 1 whatever is written.
    host_control_fixed: u32,
}

/// EMMC2's, measured on a Raspberry Pi 4B d03115 (`specs/emmc2.toml`).
const EMMC2_ID: Identity = Identity {
    name: "emmc2",
    caps0: CAPS0,
    caps1: CAPS1,
    max_current: MAX_CURRENT_VALUE,
    version: VERSION,
    host_control_fixed: HOST_CONTROL_FIXED,
};

/// The legacy EMMC's: `HOST_CONTROL` reads back what was written, and the
/// capability and version registers are unmeasured, so they read 0.
const LEGACY_ID: Identity = Identity {
    name: "emmc",
    caps0: 0,
    caps1: 0,
    max_current: 0,
    version: 0,
    host_control_fixed: 0,
};

const PS_LINES_CMD_DAT: u32 = PRESENT_STATE_DAT_LINES_MASK | PRESENT_STATE_CMD_LINE_MASK;

const TM_AUTO_CMD12: u32 = 1;
const TM_AUTO_CMD23: u32 = 2;

const HC_DMA_ADMA2_32: u32 = 2;

const SRST_MASK: u32 = SRST_ALL | SRST_CMD | SRST_DATA;

/// Interrupt bits a DAT software reset clears; command complete is preserved.
const INT_DATA_BITS: u32 =
    INT_XFER_COMPLETE | INT_DMA | INT_BLOCK_GAP | INT_BUF_WRITE_RDY | INT_BUF_READ_RDY;

const ADMA_VALID: u16 = 1 << 0;
const ADMA_END: u16 = 1 << 1;
const ADMA_INT: u16 = 1 << 2;
const ADMA_ACT_SHIFT: u16 = 4;
const ADMA_ACT_TRAN: u16 = 2;
const ADMA_ACT_LINK: u16 = 3;
const ADMA_ST_FDS: u32 = 1;
const ADMA_ST_TFR: u32 = 3;
/// Descriptors walked per transfer before the engine gives up (a link loop).
const ADMA_MAX_DESCRIPTORS: usize = 1 << 16;

/// A DMA address as the RAM sees it: the emmc2bus's `dma-ranges` is 1:1, so an
/// address inside RAM is physical (Linux's DMA32 buffers live above the first
/// gigabyte). Only an address past the RAM is a VPU cache alias.
fn dma_ram_addr(addr: u32, ram_len: usize) -> u32 {
    if (addr as usize) < ram_len {
        addr
    } else {
        addr & 0x3FFF_FFFF
    }
}

/// An SDMA or ADMA2 transfer in flight.
struct Dma {
    adma: bool,
    write: bool,
    buf: Vec<u8>,
    pos: usize,
    sdma_addr: u32,
    lba: u32,
    block_size: usize,
    auto_cmd12: bool,
}

/// Status polls after a PIO block is drained until the next arrives; both
/// `PRESENT_STATE` and `INT_STATUS` reads count, since drivers use either.
const BLOCK_POLLS: u8 = 2;

/// When the next PIO block arrives with nothing polling for it: a block on a
/// 4-bit bus is 1042 clocks, 21 µs at the 50 MHz both stock stages use.
const BLOCK_WIRE_US: u64 = 21;

struct NextBlock {
    polls_left: u8,
    due_us: u64,
}

/// A PIO write in flight: blocks the host pushes through the Buffer Data Port.
struct PioWrite {
    lba: u32,
    blocks_left: Option<u32>,
    auto_cmd12: bool,
}

pub struct Emmc2 {
    id: Identity,
    reg: BTreeMap<u32, u32>,
    /// The inserted card. `None` is an empty slot: nothing drives CMD.
    card: Option<SdCard>,
    resp: [u32; 4],
    /// PIO read buffer, drained 32 bits at a time, LSB-first.
    data: Vec<u8>,
    data_pos: usize,
    read_lba: u32,
    /// Blocks still owed on a counted transfer (CMD17, or CMD18 with a non-zero
    /// block count).
    read_blocks_left: u32,
    /// Open-ended CMD18: stream blocks until CMD12 stops it.
    read_open_ended: bool,
    /// A one-block register response (CMD6 status, SCR, ...).
    read_synthetic: Option<Vec<u8>>,
    read_auto_cmd12: bool,
    /// The next block is on its way; `BUF_READ_EN` stays clear until it lands.
    next_block: Option<NextBlock>,
    now_us: u64,
    pio_write: Option<PioWrite>,
    wbuf: Vec<u8>,
    dma: Option<Dma>,
    dma_pending: bool,
    /// RAM ranges the last DMA run wrote, for translated-code invalidation.
    dma_written: Vec<(u32, u32)>,
    /// CMD11 accepted: the card holds CMD/DAT low until the clock comes back
    /// at 1.8 V.
    switching_1v8: bool,
    words_out: u64,
    pub log: Log,
}

impl Default for Emmc2 {
    fn default() -> Self {
        Emmc2 {
            id: EMMC2_ID,
            reg: BTreeMap::new(),
            card: None,
            resp: [0; 4],
            data: Vec::new(),
            data_pos: 0,
            read_lba: 0,
            read_blocks_left: 0,
            read_open_ended: false,
            read_synthetic: None,
            read_auto_cmd12: false,
            next_block: None,
            now_us: 0,
            pio_write: None,
            wbuf: Vec::new(),
            dma: None,
            dma_pending: false,
            dma_written: Vec::new(),
            switching_1v8: false,
            words_out: 0,
            log: Log::default(),
        }
    }
}

impl Emmc2 {
    pub fn new() -> Emmc2 {
        Emmc2::default()
    }

    /// The legacy EMMC at `0x7E30_0000`; its bus is empty until the SD-slot
    /// mux routes the card to it.
    pub fn new_legacy() -> Emmc2 {
        Emmc2 {
            id: LEGACY_ID,
            ..Emmc2::default()
        }
    }

    pub fn insert_card(&mut self, image: Vec<u8>) {
        self.card = Some(SdCard::new(image));
    }

    pub fn insert_disk(&mut self, disk: crate::periph::disk::Disk) {
        self.card = Some(SdCard::with_disk(disk));
    }

    pub fn insert_mmc_disk(&mut self, disk: crate::periph::disk::Disk) {
        self.card = Some(SdCard::mmc_with_disk(disk));
    }

    pub fn take_card(&mut self) -> Option<SdCard> {
        self.card.take()
    }

    pub fn put_card(&mut self, card: Option<SdCard>) {
        self.card = card;
    }

    pub fn has_card(&self) -> bool {
        self.card.is_some()
    }

    pub fn card(&self) -> Option<&SdCard> {
        self.card.as_ref()
    }

    fn get(&self, off: u32) -> u32 {
        self.reg.get(&off).copied().unwrap_or(0)
    }

    fn set_int(&mut self, bits: u32) {
        let cur = self.get(INT_STATUS);
        let en = self.get(INT_STATUS_EN);
        self.reg.insert(INT_STATUS, cur | (bits & en));
    }

    /// `INT_STATUS` as read. The card interrupt is a level, not a latch (SDHCI
    /// 3.00, 1.8): writing a one clears nothing and only the card can drop it.
    fn int_status(&self) -> u32 {
        let mut st = self.get(INT_STATUS) & !INT_ERROR;
        if self.card.as_ref().is_some_and(|c| c.io_irq()) {
            st |= INT_CARD & self.get(INT_STATUS_EN);
        }
        if st & 0xFFFF_0000 != 0 {
            st | INT_ERROR
        } else {
            st
        }
    }

    /// The interrupt output: high while a latched status bit is enabled in
    /// `INT_SIGNAL_EN`.
    pub fn irq_asserted(&self) -> bool {
        self.int_status() & self.get(INT_SIGNAL_EN) != 0
    }

    pub fn dma_pending(&self) -> bool {
        self.dma_pending
    }

    pub fn take_dma_written(&mut self) -> Vec<(u32, u32)> {
        std::mem::take(&mut self.dma_written)
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

    fn sdma_boundary(&self) -> u32 {
        4096 << ((self.get(BLOCK_SIZE_COUNT) >> 12) & 7)
    }

    fn transfer_active(&self) -> bool {
        !self.data.is_empty()
            || self.read_blocks_left > 0
            || self.read_open_ended
            || self.pio_write.is_some()
            || self.dma.is_some()
    }

    /// Bring the host to `now_us`: a PIO block due by then arrives.
    #[inline]
    pub fn advance_to(&mut self, now_us: u64) {
        self.now_us = now_us;
        if self.next_block.as_ref().is_some_and(|n| n.due_us <= now_us) {
            self.block_arrives();
        }
        if let Some(card) = self.card.as_mut() {
            card.advance_to(now_us);
        }
    }

    fn poll(&mut self) {
        if let Some(n) = self.next_block.as_mut() {
            n.polls_left -= 1;
            if n.polls_left == 0 {
                self.block_arrives();
            }
        }
    }

    fn block_arrives(&mut self) {
        self.next_block = None;
        self.fill_next_block();
    }

    fn reset_data(&mut self) {
        self.data.clear();
        self.data_pos = 0;
        self.read_blocks_left = 0;
        self.read_open_ended = false;
        self.read_synthetic = None;
        self.read_auto_cmd12 = false;
        self.next_block = None;
        self.pio_write = None;
        self.wbuf.clear();
        self.dma = None;
        self.dma_pending = false;
    }

    /// Auto-CMD12 after a multi-block transfer; its R1b lands in `RESPONSE3`.
    fn auto_cmd12(&mut self) {
        if let Some(card) = self.card.as_mut() {
            self.resp[3] = card.command(12, 0).r1.unwrap_or(0);
        }
    }

    fn finish_data(&mut self, auto_cmd12: bool) {
        if auto_cmd12 {
            self.auto_cmd12();
        }
        self.set_int(INT_XFER_COMPLETE);
    }

    fn issue_command(&mut self, cmd_xfer: u32) {
        let arg = self.get(ARGUMENT);
        let mode = cmd_xfer & 0xFFFF;
        let command = cmd_xfer >> 16;
        let index = ((command >> 8) & 0x3F) as u8;
        let resp_type = command & 0x3; // 0 none, 1 R2(136), 2 R(48), 3 R1b(48+busy)
        let data_present = (command >> 5) & 1 != 0;
        let multi = mode & TM_MULTI != 0;
        let auto_cmd = (mode >> TM_AUTO_CMD_SHIFT) & 3;

        // Any new command tears down a previous transfer's state (CMD12
        // included — ending an open-ended read is what it is for).
        self.reset_data();
        self.words_out = 0;
        self.resp = [0; 4];

        if data_present && multi && auto_cmd == TM_AUTO_CMD23 {
            let arg2 = self.get(SDMA_ADDR);
            if let Some(card) = self.card.as_mut() {
                card.command(23, arg2);
            }
        }

        let response = match self.card.as_mut() {
            Some(card) => card.command(index, arg),
            None => crate::periph::sdcard::SdResponse {
                no_response: true,
                ..Default::default()
            },
        };

        crate::log!(
            self.log,
            Channel::Emmc,
            "CMD{index} arg={arg:#010x} mode={mode:#06x} rt={resp_type} data={data_present} \
             -> r1={:?} silent={} rd={}@{:#x} wr={}@{:#x}",
            response.r1,
            response.no_response,
            response.read_blocks,
            response.read_lba,
            response.write_blocks,
            response.write_lba,
        );

        if response.no_response && resp_type != 0 {
            self.set_int(INT_ERR_CMD_TIMEOUT);
            return;
        }

        match resp_type {
            1 => {
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

        if index == 11 && response.r1.is_some() {
            self.switching_1v8 = true;
        }

        if index == 19 && self.get(HOST_CONTROL2) & HC2_EXEC_TUNING != 0 {
            let hc2 = self.get(HOST_CONTROL2);
            self.reg
                .insert(HOST_CONTROL2, (hc2 & !HC2_EXEC_TUNING) | HC2_TUNED_CLK);
            self.set_int(INT_BUF_READ_RDY);
            return;
        }

        self.set_int(INT_CMD_COMPLETE);
        if resp_type == 3 {
            self.set_int(INT_XFER_COMPLETE);
        }

        if !data_present {
            return;
        }
        let auto_cmd12 = multi && auto_cmd == TM_AUTO_CMD12;
        if mode & TM_DMA != 0 {
            self.start_dma(&response, mode, auto_cmd12);
            return;
        }
        if response.write_blocks > 0 {
            let count = self.block_count();
            self.pio_write = Some(PioWrite {
                lba: response.write_lba,
                blocks_left: if !multi {
                    Some(1)
                } else if count > 0 || mode & TM_BLOCK_COUNT_EN != 0 {
                    Some(count)
                } else {
                    None
                },
                auto_cmd12,
            });
            self.set_int(INT_BUF_WRITE_RDY);
        } else if let Some(block) = response.data {
            self.read_synthetic = Some(block);
            self.read_blocks_left = 1;
            self.fill_next_block();
        } else if response.read_blocks > 0 {
            self.read_lba = response.read_lba;
            self.read_auto_cmd12 = auto_cmd12;
            let count = self.block_count();
            if response.read_blocks == u32::MAX {
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
    }

    fn start_dma(
        &mut self,
        response: &crate::periph::sdcard::SdResponse,
        mode: u32,
        auto_cmd12: bool,
    ) {
        let bs = self.block_size();
        let blocks = if mode & TM_MULTI != 0 {
            self.block_count() as usize
        } else {
            1
        };
        let write = mode & TM_READ == 0 && response.write_blocks > 0;
        let mut buf = vec![0u8; bs * blocks];
        if !write {
            if let Some(block) = &response.data {
                let n = block.len().min(buf.len());
                buf[..n].copy_from_slice(&block[..n]);
            } else if response.read_blocks > 0 {
                if let Some(card) = self.card.as_mut() {
                    let mut b = [0u8; 512];
                    for (i, chunk) in buf.chunks_mut(bs).enumerate() {
                        card.transfer_read(response.read_lba.wrapping_add(i as u32), &mut b);
                        let n = chunk.len().min(512);
                        chunk[..n].copy_from_slice(&b[..n]);
                        card.block_done();
                    }
                }
            }
        }
        let adma = (self.get(HOST_CONTROL) >> HC_DMA_SHIFT) & 3 == HC_DMA_ADMA2_32;
        self.dma = Some(Dma {
            adma,
            write,
            buf,
            pos: 0,
            sdma_addr: self.get(SDMA_ADDR),
            lba: response.write_lba,
            block_size: bs,
            auto_cmd12,
        });
        self.dma_pending = true;
        self.dma_written.clear();
    }

    pub fn run_dma(&mut self, ram: &mut Ram) {
        self.dma_pending = false;
        let Some(mut d) = self.dma.take() else {
            return;
        };
        let done = if d.adma {
            self.run_adma2(&mut d, ram)
        } else {
            self.run_sdma(&mut d, ram)
        };
        match done {
            Some(true) => {
                if d.write {
                    if let Some(card) = self.card.as_mut() {
                        for (i, block) in d.buf.chunks(d.block_size).enumerate() {
                            card.transfer_write(d.lba.wrapping_add(i as u32), block);
                            card.block_done();
                        }
                    }
                }
                self.finish_data(d.auto_cmd12);
            }
            Some(false) => self.dma = Some(d),
            None => {}
        }
    }

    fn dma_copy(&mut self, d: &mut Dma, ram: &mut Ram, addr: u32, n: usize) -> bool {
        let a = dma_ram_addr(addr, ram.len());
        let ok = if d.write {
            match ram.read_slice(a, n) {
                Ok(src) => {
                    d.buf[d.pos..d.pos + n].copy_from_slice(src);
                    true
                }
                Err(_) => false,
            }
        } else {
            let ok = ram.write_slice(a, &d.buf[d.pos..d.pos + n]).is_ok();
            if ok {
                let end = a + n as u32;
                match self.dma_written.last_mut() {
                    Some(last) if last.1 == a => last.1 = end,
                    _ => self.dma_written.push((a, end)),
                }
            }
            ok
        };
        if ok {
            d.pos += n;
        }
        ok
    }

    /// SDMA: one system address, pausing with a DMA interrupt at a buffer
    /// boundary. `Some(true)` = done, `Some(false)` = paused.
    fn run_sdma(&mut self, d: &mut Dma, ram: &mut Ram) -> Option<bool> {
        let boundary = self.sdma_boundary();
        while d.pos < d.buf.len() {
            let to_boundary = (boundary - d.sdma_addr % boundary) as usize;
            let n = to_boundary.min(d.buf.len() - d.pos);
            if !self.dma_copy(d, ram, d.sdma_addr, n) {
                d.pos += n;
            }
            d.sdma_addr = d.sdma_addr.wrapping_add(n as u32);
            if d.pos < d.buf.len() && d.sdma_addr.is_multiple_of(boundary) {
                self.reg.insert(SDMA_ADDR, d.sdma_addr);
                self.set_int(INT_DMA);
                return Some(false);
            }
        }
        self.reg.insert(SDMA_ADDR, d.sdma_addr);
        Some(true)
    }

    /// ADMA2, 32-bit descriptors walked from `ADMA_ADDR`. `None` = error.
    fn run_adma2(&mut self, d: &mut Dma, ram: &mut Ram) -> Option<bool> {
        let mut desc = self.get(ADMA_ADDR);
        let mut result = None;
        let mut state = ADMA_ST_FDS;
        let mut mismatch = false;
        for _ in 0..ADMA_MAX_DESCRIPTORS {
            let Ok(raw) = ram.read_slice(dma_ram_addr(desc, ram.len()), 8) else {
                break;
            };
            let attr = u16::from_le_bytes([raw[0], raw[1]]);
            let len = match u16::from_le_bytes([raw[2], raw[3]]) {
                0 => 65536,
                n => n as usize,
            };
            let addr = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
            if attr & ADMA_VALID == 0 {
                break;
            }
            let act = (attr >> ADMA_ACT_SHIFT) & 3;
            if act == ADMA_ACT_TRAN {
                let n = len.min(d.buf.len() - d.pos);
                if n > 0 && !self.dma_copy(d, ram, addr, n) {
                    state = ADMA_ST_TFR;
                    break;
                }
            }
            if attr & ADMA_INT != 0 {
                self.set_int(INT_DMA);
            }
            if d.pos == d.buf.len() {
                result = Some(true);
                break;
            }
            if attr & ADMA_END != 0 {
                state = ADMA_ST_TFR;
                mismatch = true;
                break;
            }
            desc = if act == ADMA_ACT_LINK {
                addr
            } else {
                desc.wrapping_add(8)
            };
        }
        self.reg.insert(ADMA_ADDR, desc);
        if result.is_none() {
            let mm = if mismatch { ADMA_LEN_MISMATCH } else { 0 };
            self.reg.insert(ADMA_ERROR, state | mm);
            self.set_int(INT_ERR_ADMA);
        }
        result
    }

    fn fill_next_block(&mut self) {
        if self.read_blocks_left == 0 && !self.read_open_ended {
            return;
        }
        let bs = self.block_size();
        let mut block = [0u8; 512];
        if let Some(reg) = self.read_synthetic.take() {
            let n = reg.len().min(512);
            block[..n].copy_from_slice(&reg[..n]);
        } else {
            let lba = self.read_lba;
            if let Some(card) = self.card.as_mut() {
                card.transfer_read(lba, &mut block);
                card.block_done();
            }
            self.read_lba = lba.wrapping_add(1);
        }
        self.data = block[..bs.min(512)].to_vec();
        self.data_pos = 0;
        self.read_blocks_left = self.read_blocks_left.saturating_sub(1);
        self.set_int(INT_BUF_READ_RDY);
        if self.log.on(Channel::Emmc) {
            let lba = self.read_lba.wrapping_sub(1);
            crate::log!(
                self.log,
                Channel::Emmc,
                "block lba={lba:#x} ({} bytes) first={:02x}{:02x}{:02x}{:02x} left={} open={}",
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

    /// The Buffer Data Port as the *external* DMA engine sees it. The legacy
    /// host has no SDHCI DMA, so `mmc-bcm2835` moves transfers with the legacy
    /// DMA controller, paced by the card's DREQ — which is the difference from
    /// the PIO path: the engine only asks for a word once the FIFO has one, so
    /// the next block is always there rather than arriving after a poll.
    pub fn dma_fifo_read(&mut self) -> u32 {
        if !self.buf_read_en() && (self.read_blocks_left > 0 || self.read_open_ended) {
            self.block_arrives();
        }
        self.read_buffer_word()
    }

    pub fn dma_fifo_write(&mut self, value: u32) {
        self.write_buffer(&value.to_le_bytes());
    }

    fn buf_read_en(&self) -> bool {
        self.data_pos < self.data.len()
    }

    fn read_buffer_word(&mut self) -> u32 {
        if !self.buf_read_en() {
            crate::log!(
                self.log,
                Channel::Emmc,
                "R [0x20] with BUF_READ_EN clear (guest bug) -> 0, {}",
                if self.next_block.is_some() {
                    "next block not there yet"
                } else {
                    "no read in progress"
                }
            );
            return 0;
        }
        let mut w = [0u8; 4];
        for b in w.iter_mut() {
            *b = self.data.get(self.data_pos).copied().unwrap_or(0);
            self.data_pos += 1;
        }
        self.words_out += 1;
        let word = u32::from_le_bytes(w);
        if self.words_out % 64 == 1 {
            crate::log!(
                self.log,
                Channel::Emmc,
                "R [0x20] -> {word:#010x}  (word {})",
                self.words_out
            );
        }
        if self.data_pos >= self.data.len() {
            if self.read_blocks_left > 0 || self.read_open_ended {
                self.next_block = Some(NextBlock {
                    polls_left: BLOCK_POLLS,
                    due_us: self.now_us
                        + crate::jitter::stretch_slow(BLOCK_WIRE_US, "a card block"),
                });
            } else if !self.data.is_empty() {
                let cur = self.get(INT_STATUS) & !INT_BUF_READ_RDY;
                self.reg.insert(INT_STATUS, cur);
                self.data.clear();
                self.data_pos = 0;
                let auto = std::mem::take(&mut self.read_auto_cmd12);
                self.finish_data(auto);
            }
        }
        word
    }

    fn write_buffer(&mut self, bytes: &[u8]) {
        let bs = self.block_size();
        let Some(pw) = self.pio_write.as_mut() else {
            return;
        };
        self.wbuf.extend_from_slice(bytes);
        if self.wbuf.len() < bs {
            return;
        }
        let lba = pw.lba;
        pw.lba = lba.wrapping_add(1);
        let more = match pw.blocks_left.as_mut() {
            Some(n) => {
                *n = n.saturating_sub(1);
                *n > 0
            }
            None => true,
        };
        let auto = pw.auto_cmd12;
        if let Some(card) = self.card.as_mut() {
            card.transfer_write(lba, &self.wbuf[..bs]);
            card.block_done();
        }
        self.wbuf.clear();
        if more {
            self.set_int(INT_BUF_WRITE_RDY);
        } else {
            self.pio_write = None;
            self.finish_data(auto);
        }
    }

    fn read_word(&mut self, off: u32) -> u32 {
        match off {
            RESPONSE0 => self.resp[0],
            RESPONSE1 => self.resp[1],
            RESPONSE2 => self.resp[2],
            RESPONSE3 => self.resp[3],
            BUFFER_DATA => self.read_buffer_word(),
            CLOCK_CONTROL => {
                let clk = self.get(CLOCK_CONTROL) & 0x00FF_FFFF;
                if clk & CLK_INTLEN != 0 {
                    clk | CLK_STABLE
                } else {
                    clk
                }
            }
            PRESENT_STATE => {
                self.poll();
                let mut ps = PRESENT_STATE_IDLE;
                if self.buf_read_en() {
                    ps |= PS_BUF_READ_EN;
                }
                if self.pio_write.is_some() {
                    ps |= PS_BUF_WRITE_EN;
                }
                if self.transfer_active() {
                    ps |= PS_DAT_INHIBIT;
                }
                if self.switching_1v8 {
                    ps &= !PS_LINES_CMD_DAT;
                }
                ps
            }
            INT_STATUS => {
                self.poll();
                self.int_status()
            }
            HOST_CONTROL => self.get(HOST_CONTROL) | self.id.host_control_fixed,
            CAPABILITIES_0 => self.id.caps0,
            CAPABILITIES_1 => self.id.caps1,
            MAX_CURRENT => self.id.max_current,
            CONTROLLER_VERSION => self.id.version,
            _ => self.get(off),
        }
    }

    fn write_word(&mut self, off: u32, value: u32, lanes: u32) {
        match off {
            CLOCK_CONTROL => {
                self.reg.insert(CLOCK_CONTROL, value & !SRST_MASK);
                // Software resets self-clear. A DAT reset tears down the data
                // path but leaves command-complete; a CMD reset clears it; an
                // ALL reset returns every register, `HOST_CONTROL` included, to
                // its reset value (SDHCI 3.00, 2.2.18).
                if value & (SRST_ALL | SRST_DATA) != 0 {
                    self.reset_data();
                    let keep = if value & SRST_ALL != 0 {
                        0
                    } else {
                        self.get(INT_STATUS) & !INT_DATA_BITS
                    };
                    self.reg.insert(INT_STATUS, keep);
                }
                if value & SRST_ALL != 0 {
                    self.reg.clear();
                    self.switching_1v8 = false;
                }
                if value & (SRST_ALL | SRST_CMD) != 0 {
                    let cur = self.get(INT_STATUS);
                    self.reg.insert(INT_STATUS, cur & !INT_CMD_COMPLETE);
                }
                if self.switching_1v8
                    && value & CLK_SD_EN != 0
                    && self.get(HOST_CONTROL2) & HC2_1V8 != 0
                {
                    self.switching_1v8 = false;
                }
            }
            INT_STATUS => {
                let cur = self.get(INT_STATUS);
                self.reg.insert(INT_STATUS, cur & !(value & lanes)); // write-1-to-clear
            }
            CMD_XFER => {
                self.reg.insert(CMD_XFER, value);
                if lanes & 0xFFFF_0000 != 0 {
                    self.issue_command(value);
                }
            }
            SDMA_ADDR => {
                self.reg.insert(SDMA_ADDR, value);
                if let Some(d) = self.dma.as_mut() {
                    if !d.adma {
                        d.sdma_addr = value;
                        self.dma_pending = true;
                    }
                }
            }
            HOST_CONTROL => {
                let was = self.get(HOST_CONTROL);
                self.reg.insert(HOST_CONTROL, value);
                if was & HC_BUS_POWER != 0 && value & HC_BUS_POWER == 0 {
                    if let Some(card) = self.card.as_mut() {
                        card.power_off();
                    }
                    self.switching_1v8 = false;
                }
            }
            HOST_CONTROL2 => {
                self.reg.insert(HOST_CONTROL2, value & 0xFFFF_0000);
            }
            RESPONSE0 | RESPONSE1 | RESPONSE2 | RESPONSE3 | BUFFER_DATA | PRESENT_STATE
            | CAPABILITIES_0 | CAPABILITIES_1 | MAX_CURRENT | CONTROLLER_VERSION => {}
            _ => {
                self.reg.insert(off, value);
            }
        }
    }
}

impl MmioDevice for Emmc2 {
    fn name(&self) -> &'static str {
        self.id.name
    }

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32> {
        let off = offset & !3;
        let word = self.read_word(off);
        if off != BUFFER_DATA {
            crate::log!(self.log, Channel::Emmc, "R [{off:#04x}] -> {word:#010x}");
        }
        Ok(match width {
            Width::Word => word,
            Width::Half => (word >> ((offset & 2) * 8)) & 0xFFFF,
            Width::Byte => (word >> ((offset & 3) * 8)) & 0xFF,
        })
    }

    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        if off != BUFFER_DATA {
            crate::log!(
                self.log,
                Channel::Emmc,
                "W [{off:#04x}] <- {value:#010x} ({width:?})"
            );
        }
        if off == BUFFER_DATA {
            let n = width.bytes() as usize;
            self.write_buffer(&value.to_le_bytes()[..n]);
            return Ok(());
        }
        let (value, lanes) = match width {
            Width::Word => (value, u32::MAX),
            _ => {
                let shift = (offset & 3) * 8;
                let mask = if width == Width::Half { 0xFFFF } else { 0xFF } << shift;
                let old = match off {
                    INT_STATUS => 0,
                    _ => self.get(off),
                };
                ((old & !mask) | ((value << shift) & mask), mask)
            }
        };
        self.write_word(off, value, lanes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAM: u32 = 0x10_0000;

    fn host() -> Emmc2 {
        let mut e = Emmc2::new();
        let mut img = vec![0u8; 64 * 512];
        for (i, b) in img.iter_mut().enumerate() {
            *b = (i / 512) as u8 ^ (i as u8);
        }
        e.insert_card(img);
        wr(&mut e, INT_STATUS_EN, 0xFFFF_FFFF);
        e
    }

    fn wr(e: &mut Emmc2, off: u32, v: u32) {
        e.write(off, Width::Word, v).unwrap();
    }

    fn rd(e: &mut Emmc2, off: u32) -> u32 {
        e.read(off, Width::Word).unwrap()
    }

    fn cmd(e: &mut Emmc2, index: u32, arg: u32, flags: u32, mode: u32) {
        wr(e, ARGUMENT, arg);
        wr(e, CMD_XFER, ((index << 8 | flags) << 16) | mode);
    }

    const R1: u32 = 0x1A;
    const R1_DATA: u32 = 0x3A;

    fn enumerate(e: &mut Emmc2, ocr: u32) -> u32 {
        cmd(e, 0, 0, 0, 0);
        let mut r = 0;
        for _ in 0..2 {
            cmd(e, 55, 0, R1, 0);
            cmd(e, 41, ocr, 0x02, 0);
            r = rd(e, RESPONSE0);
        }
        r
    }

    fn select(e: &mut Emmc2) {
        cmd(e, 2, 0, 0x09, 0);
        cmd(e, 3, 0, R1, 0);
        cmd(e, 7, 0x0001_0000, 0x1B, 0);
        wr(e, INT_STATUS, 0xFFFF_FFFF);
    }

    fn adma_table(ram: &mut Ram, at: u32, descs: &[(u16, u16, u32)]) {
        for (i, &(attr, len, addr)) in descs.iter().enumerate() {
            let a = at + 8 * i as u32;
            ram.store(a, Width::Half, attr as u32).unwrap();
            ram.store(a + 2, Width::Half, len as u32).unwrap();
            ram.store(a + 4, Width::Word, addr).unwrap();
        }
    }

    const TRAN: u16 = 0x21;
    const TRAN_END: u16 = 0x23;
    const NOP_END: u16 = 0x03;

    /// Linux's DMA32 buffers sit above the first gigabyte and must not fold
    /// onto it; past the RAM, the VPU's aliases still do.
    #[test]
    fn dma_addresses_inside_the_ram_are_physical() {
        let two_gb = 2 << 30;
        assert_eq!(dma_ram_addr(0x7F00_1000, two_gb), 0x7F00_1000);
        assert_eq!(dma_ram_addr(0xC000_1000, two_gb), 0x1000);
        assert_eq!(dma_ram_addr(0x4000_1000, 1 << 30), 0x1000);
    }

    #[test]
    fn identity_registers_are_the_measured_ones() {
        let mut e = host();
        assert_eq!(rd(&mut e, CAPABILITIES_0), 0x45EE_6432);
        assert_eq!(rd(&mut e, CAPABILITIES_1), 0x0000_A525);
        assert_eq!(rd(&mut e, CONTROLLER_VERSION), 0x1002_0000);
        assert_eq!(e.read(0xFE, Width::Half).unwrap(), 0x1002);
        assert_eq!(rd(&mut e, PRESENT_STATE), 0x1FFF_0000);
        wr(&mut e, HOST_CONTROL, 0x0000_0F00);
        assert_eq!(rd(&mut e, HOST_CONTROL), 0x0080_0F00, "bit 23 reads 1");
        assert_eq!(CAPS0 & (1 << 28), 0, "no 64-bit ADMA");
    }

    /// With no card CMD0 completes and CMD8 times out, which is how edk2's
    /// `ArasanMmcHostDxe` decides the slot is empty.
    #[test]
    fn an_empty_slot_times_out_every_command_that_expects_a_response() {
        let mut e = Emmc2::new();
        wr(&mut e, INT_STATUS_EN, 0xFFFF_FFFF);
        cmd(&mut e, 0, 0, 0, 0);
        assert_eq!(rd(&mut e, INT_STATUS), INT_CMD_COMPLETE);
        wr(&mut e, INT_STATUS, 0xFFFF_FFFF);
        cmd(&mut e, 8, 0x1AA, R1, 0);
        assert_eq!(rd(&mut e, INT_STATUS), INT_ERR_CMD_TIMEOUT | INT_ERROR);
    }

    #[test]
    fn interrupt_line_follows_status_and_signal_enable() {
        let mut e = host();
        cmd(&mut e, 13, 0, R1, 0);
        assert_eq!(rd(&mut e, INT_STATUS), INT_CMD_COMPLETE);
        assert!(!e.irq_asserted(), "nothing signalled yet");
        wr(&mut e, INT_SIGNAL_EN, INT_CMD_COMPLETE);
        assert!(e.irq_asserted());
        wr(&mut e, INT_STATUS, INT_CMD_COMPLETE);
        assert!(!e.irq_asserted(), "W1C drops the level");

        cmd(&mut e, 5, 0, 0x02, 0);
        assert_eq!(rd(&mut e, INT_STATUS), INT_ERR_CMD_TIMEOUT | INT_ERROR);
        assert!(!e.irq_asserted());
        wr(&mut e, INT_SIGNAL_EN, INT_ERR_CMD_TIMEOUT);
        assert!(e.irq_asserted());
        wr(&mut e, INT_STATUS, INT_ERR_CMD_TIMEOUT);
        assert_eq!(rd(&mut e, INT_STATUS), 0);

        wr(&mut e, INT_SIGNAL_EN, 0xFFFF_FFFF);
        wr(&mut e, INT_STATUS_EN, 0);
        cmd(&mut e, 13, 0, R1, 0);
        assert_eq!(rd(&mut e, INT_STATUS), 0);
        assert!(!e.irq_asserted());
    }

    #[test]
    fn adma2_read_scatters_blocks_over_the_descriptors() {
        let mut e = host();
        let mut ram = Ram::new(0, 4 << 20);
        enumerate(&mut e, 0x40FF_8000);
        select(&mut e);
        wr(&mut e, HOST_CONTROL, HC_DMA_ADMA2_32 << HC_DMA_SHIFT);
        adma_table(
            &mut ram,
            RAM,
            &[(TRAN, 100, RAM + 0x1000), (0x31, 0, RAM + 0x100)],
        );
        adma_table(
            &mut ram,
            RAM + 0x100,
            &[(TRAN, 1436, RAM + 0x2000), (NOP_END, 0, 0)],
        );
        wr(&mut e, ADMA_ADDR, RAM);
        wr(&mut e, BLOCK_SIZE_COUNT, (3 << 16) | 512);
        let mode =
            TM_DMA | TM_BLOCK_COUNT_EN | TM_READ | TM_MULTI | (TM_AUTO_CMD12 << TM_AUTO_CMD_SHIFT);
        cmd(&mut e, 18, 5, R1_DATA, mode);
        assert!(e.dma_pending());
        e.run_dma(&mut ram);
        assert_eq!(rd(&mut e, INT_STATUS), INT_CMD_COMPLETE | INT_XFER_COMPLETE);

        let mut want = Vec::new();
        for lba in 5..8 {
            let mut b = [0u8; 512];
            e.card().unwrap().read_block(lba, &mut b);
            want.extend_from_slice(&b);
        }
        assert_eq!(ram.read_slice(RAM + 0x1000, 100).unwrap(), &want[..100]);
        assert_eq!(ram.read_slice(RAM + 0x2000, 1436).unwrap(), &want[100..]);
        assert_eq!(
            e.take_dma_written(),
            vec![
                (RAM + 0x1000, RAM + 0x1064),
                (RAM + 0x2000, RAM + 0x2000 + 1436)
            ]
        );
        assert_eq!((rd(&mut e, RESPONSE3) >> 9) & 0xF, 4);
    }

    #[test]
    fn adma2_errors_on_an_invalid_descriptor_and_on_short_tables() {
        let mut e = host();
        let mut ram = Ram::new(0, 4 << 20);
        enumerate(&mut e, 0x40FF_8000);
        select(&mut e);
        wr(&mut e, HOST_CONTROL, HC_DMA_ADMA2_32 << HC_DMA_SHIFT);
        wr(&mut e, BLOCK_SIZE_COUNT, (2 << 16) | 512);
        let mode = TM_DMA | TM_BLOCK_COUNT_EN | TM_READ | TM_MULTI;

        adma_table(
            &mut ram,
            RAM,
            &[(TRAN, 512, RAM + 0x1000), (0x20, 512, RAM + 0x2000)],
        );
        wr(&mut e, ADMA_ADDR, RAM);
        cmd(&mut e, 18, 0, R1_DATA, mode);
        e.run_dma(&mut ram);
        assert_eq!(
            rd(&mut e, INT_STATUS),
            INT_CMD_COMPLETE | INT_ERR_ADMA | INT_ERROR
        );
        assert_eq!(rd(&mut e, ADMA_ERROR), ADMA_ST_FDS);
        assert_eq!(
            rd(&mut e, ADMA_ADDR),
            RAM + 8,
            "points at the bad descriptor"
        );

        wr(&mut e, INT_STATUS, 0xFFFF_FFFF);
        adma_table(&mut ram, RAM, &[(TRAN_END, 512, RAM + 0x1000)]);
        wr(&mut e, ADMA_ADDR, RAM);
        cmd(&mut e, 18, 0, R1_DATA, mode);
        e.run_dma(&mut ram);
        assert_eq!(rd(&mut e, INT_STATUS) & INT_ERR_ADMA, INT_ERR_ADMA);
        assert_eq!(rd(&mut e, ADMA_ERROR), ADMA_ST_TFR | ADMA_LEN_MISMATCH);
    }

    #[test]
    fn adma2_write_with_auto_cmd23_lands_in_the_card() {
        let mut e = host();
        let mut ram = Ram::new(0, 4 << 20);
        enumerate(&mut e, 0x40FF_8000);
        select(&mut e);
        wr(&mut e, HOST_CONTROL, HC_DMA_ADMA2_32 << HC_DMA_SHIFT);
        let data: Vec<u8> = (0..1024u32).map(|i| (i * 7) as u8).collect();
        ram.write_slice(RAM + 0x3000, &data).unwrap();
        adma_table(&mut ram, RAM, &[(TRAN_END, 1024, RAM + 0x3000)]);
        wr(&mut e, ADMA_ADDR, RAM);
        wr(&mut e, BLOCK_SIZE_COUNT, (2 << 16) | 512);
        wr(&mut e, SDMA_ADDR, 2); // ARGUMENT2: the CMD23 count
        let mode = TM_DMA | TM_BLOCK_COUNT_EN | TM_MULTI | (TM_AUTO_CMD23 << TM_AUTO_CMD_SHIFT);
        cmd(&mut e, 25, 40, R1_DATA, mode);
        e.run_dma(&mut ram);
        assert_eq!(rd(&mut e, INT_STATUS), INT_CMD_COMPLETE | INT_XFER_COMPLETE);
        assert_eq!(e.card().unwrap().disk().read(40, 2).unwrap(), &data[..]);
        assert_eq!(
            e.card().unwrap().state(),
            crate::periph::sdcard::CardState::Tran
        );
        assert!(e.take_dma_written().is_empty(), "a write only reads RAM");
    }

    #[test]
    fn sdma_pauses_at_the_boundary_until_the_next_address() {
        let mut e = host();
        let mut ram = Ram::new(0, 4 << 20);
        enumerate(&mut e, 0x40FF_8000);
        select(&mut e);
        wr(&mut e, BLOCK_SIZE_COUNT, (16 << 16) | 512);
        wr(&mut e, SDMA_ADDR, RAM + 0x0C00);
        cmd(
            &mut e,
            18,
            0,
            R1_DATA,
            TM_DMA | TM_BLOCK_COUNT_EN | TM_READ | TM_MULTI,
        );
        e.run_dma(&mut ram);
        assert_eq!(rd(&mut e, INT_STATUS), INT_CMD_COMPLETE | INT_DMA);
        wr(&mut e, INT_STATUS, INT_DMA);
        let mut resumes = 0;
        while rd(&mut e, INT_STATUS) & INT_XFER_COMPLETE == 0 {
            wr(&mut e, SDMA_ADDR, RAM + 0x1000 + 0x1000 * resumes);
            assert!(e.dma_pending());
            e.run_dma(&mut ram);
            wr(&mut e, INT_STATUS, INT_DMA);
            resumes += 1;
        }
        assert_eq!(resumes, 2, "1 KiB + 4 KiB + 3 KiB");
        let mut b = [0u8; 512];
        e.card().unwrap().read_block(15, &mut b);
        assert_eq!(
            ram.read_slice(RAM + 0x0C00 + 15 * 512, 512).unwrap(),
            &b[..]
        );
    }

    #[test]
    fn pio_write_then_pio_read_round_trips() {
        let mut e = host();
        enumerate(&mut e, 0x40FF_8000);
        select(&mut e);
        wr(&mut e, BLOCK_SIZE_COUNT, (2 << 16) | 512);
        cmd(
            &mut e,
            25,
            3,
            R1_DATA,
            TM_BLOCK_COUNT_EN | TM_MULTI | (TM_AUTO_CMD12 << TM_AUTO_CMD_SHIFT),
        );
        assert_ne!(rd(&mut e, PRESENT_STATE) & PS_BUF_WRITE_EN, 0);
        for i in 0..128u32 {
            wr(&mut e, BUFFER_DATA, i.wrapping_mul(0x0101_0101));
        }
        assert_eq!(rd(&mut e, INT_STATUS) & INT_XFER_COMPLETE, 0);
        for i in 128..256u32 {
            wr(&mut e, BUFFER_DATA, i.wrapping_mul(0x0101_0101));
        }
        assert_eq!(
            rd(&mut e, INT_STATUS) & INT_XFER_COMPLETE,
            INT_XFER_COMPLETE
        );
        assert_eq!(rd(&mut e, PRESENT_STATE) & PS_BUF_WRITE_EN, 0);
        assert_eq!(
            e.card().unwrap().state(),
            crate::periph::sdcard::CardState::Tran
        );

        wr(&mut e, INT_STATUS, 0xFFFF_FFFF);
        cmd(&mut e, 17, 4, R1_DATA, TM_READ);
        let words: Vec<u32> = (0..128).map(|_| rd(&mut e, BUFFER_DATA)).collect();
        let want: Vec<u32> = (128..256u32).map(|i| i.wrapping_mul(0x0101_0101)).collect();
        assert_eq!(words, want);
        assert_eq!(
            rd(&mut e, INT_STATUS) & INT_XFER_COMPLETE,
            INT_XFER_COMPLETE
        );
    }

    /// The WiFi chip on the legacy host, where `brcmfmac` is before it
    /// downloads anything.
    fn wifi_host(window: u32) -> Emmc2 {
        let mut e = Emmc2::new_legacy();
        e.put_card(Some(crate::periph::sdcard::SdCard::sdio()));
        wr(&mut e, INT_STATUS_EN, 0xFFFF_FFFF);
        cmd(&mut e, 5, 0, 0x02, 0);
        cmd(&mut e, 5, 0x00FF_8000, 0x02, 0);
        cmd(&mut e, 3, 0, R1, 0);
        cmd(&mut e, 7, 0x0001_0000, 0x1B, 0);
        let mut wb = |func: u32, addr: u32, v: u8| {
            let arg = (1 << 31) | (func << 28) | (addr << 9) | u32::from(v);
            cmd(&mut e, 52, arg, R1, 0);
        };
        for (i, b) in 64u16.to_le_bytes().iter().enumerate() {
            wb(0, 0x110 + i as u32, *b);
        }
        let v = (window & 0xFFFF_8000) >> 8;
        for i in 0..3 {
            wb(1, 0x1000A + i, (v >> (8 * i)) as u8);
        }
        wr(&mut e, INT_STATUS, 0xFFFF_FFFF);
        e
    }

    /// The firmware download: CMD53 to function 1, block mode, through the
    /// backplane window and back out again.
    #[test]
    fn a_block_write_through_the_window_reaches_the_chips_memory() {
        let base = crate::periph::cyw43455::RAM_BASE;
        let mut e = wifi_host(base);
        let blocks = 511u32; // the count field is nine bits
        let bs = 64u32;
        let data: Vec<u8> = (0..blocks * bs).map(|i| (i / 7) as u8).collect();

        wr(&mut e, BLOCK_SIZE_COUNT, (blocks << 16) | bs);
        let arg = (1 << 31) | (1 << 28) | (1 << 27) | (1 << 26) | (0x8000 << 9) | blocks;
        cmd(&mut e, 53, arg, R1_DATA, TM_BLOCK_COUNT_EN | TM_MULTI);
        for (i, word) in data.chunks(4).enumerate() {
            assert_ne!(
                rd(&mut e, PRESENT_STATE) & PS_BUF_WRITE_EN,
                0,
                "the buffer stopped taking bytes at word {i}"
            );
            wr(
                &mut e,
                BUFFER_DATA,
                u32::from_le_bytes(word.try_into().unwrap()),
            );
        }
        assert_eq!(
            rd(&mut e, INT_STATUS) & INT_XFER_COMPLETE,
            INT_XFER_COMPLETE,
            "the transfer never finished"
        );
        assert_eq!(rd(&mut e, PRESENT_STATE) & PS_BUF_WRITE_EN, 0);
        assert_eq!(
            &e.card().unwrap().chip().unwrap().ram()[..data.len()],
            &data[..]
        );

        wr(&mut e, INT_STATUS, 0xFFFF_FFFF);
        let arg = (1 << 28) | (1 << 27) | (1 << 26) | (0x8000 << 9) | blocks;
        cmd(
            &mut e,
            53,
            arg,
            R1_DATA,
            TM_READ | TM_BLOCK_COUNT_EN | TM_MULTI,
        );
        let mut back = Vec::new();
        while back.len() < data.len() {
            let mut polls = 0;
            while rd(&mut e, PRESENT_STATE) & PS_BUF_READ_EN == 0 {
                polls += 1;
                assert!(polls < 100, "block {} never arrived", back.len() / 64);
            }
            back.extend_from_slice(&rd(&mut e, BUFFER_DATA).to_le_bytes());
        }
        assert_eq!(back, data);
    }

    fn io_bytes(e: &mut Emmc2, func: u32, off: u32, write: bool, incr: bool, data: &mut Vec<u8>) {
        let n = data.len() as u32;
        assert!(n.is_multiple_of(4) && n <= 512);
        wr(e, BLOCK_SIZE_COUNT, (1 << 16) | n);
        let arg = (u32::from(write) << 31)
            | (func << 28)
            | (u32::from(incr) << 26)
            | (off << 9)
            | (n & 0x1FF);
        let mode = if write { 0 } else { TM_READ };
        cmd(e, 53, arg, R1_DATA, mode);
        if write {
            for word in data.chunks(4) {
                wr(e, BUFFER_DATA, u32::from_le_bytes(word.try_into().unwrap()));
            }
        } else {
            data.clear();
            for _ in 0..n / 4 {
                let mut polls = 0;
                while rd(e, PRESENT_STATE) & PS_BUF_READ_EN == 0 {
                    polls += 1;
                    assert!(polls < 100, "the block never arrived");
                }
                data.extend_from_slice(&rd(e, BUFFER_DATA).to_le_bytes());
            }
        }
        wr(
            e,
            INT_STATUS,
            INT_CMD_COMPLETE | INT_XFER_COMPLETE | INT_DATA_BITS,
        );
    }

    /// The WiFi chip with its frame FIFO open, where
    /// `brcmf_sdio_firmware_callback` leaves it.
    fn io_byte(e: &mut Emmc2, func: u32, addr: u32, v: u8) {
        let arg = (1 << 31) | (func << 28) | (addr << 9) | u32::from(v);
        cmd(e, 52, arg, R1, 0);
    }

    fn backplane_wr(e: &mut Emmc2, addr: u32, value: u32) {
        let v = (addr & 0xFFFF_8000) >> 8;
        for i in 0..3 {
            io_byte(e, 1, 0x1000A + i, (v >> (8 * i)) as u8);
        }
        let off = (addr & 0x7FFF) | 0x8000;
        let mut word = value.to_le_bytes().to_vec();
        io_bytes(e, 1, off, true, true, &mut word);
    }

    /// The SDIO device core's registers (`struct sdpcmd_regs`), where the host
    /// interrupt comes from.
    const SD_CORE: u32 = 0x1800_1000;
    const SD_INTSTATUS: u32 = 0x20;
    const SD_HOSTINTMASK: u32 = 0x24;
    const HOSTINTMASK: u32 = 0x0000_00F0 | 1 << 29;
    const I_HMB_FRAME_IND: u32 = 1 << 6;

    fn wifi_host_up() -> Emmc2 {
        let mut e = wifi_host(0x1800_0000);
        for (i, b) in 512u16.to_le_bytes().iter().enumerate() {
            io_byte(&mut e, 0, 0x210 + i as u32, *b);
        }
        io_byte(&mut e, 0, 0x02, 0x06);
        backplane_wr(&mut e, SD_CORE + SD_HOSTINTMASK, HOSTINTMASK);
        io_byte(&mut e, 0, 0x04, 0x03);
        wr(&mut e, INT_SIGNAL_EN, INT_CARD);
        wr(&mut e, INT_STATUS, 0xFFFF_FFFF);
        e
    }

    fn ctrl_frame(seq: u8, id: u16, cmd: u32, payload: &[u8]) -> Vec<u8> {
        let mut bcdc = Vec::new();
        bcdc.extend_from_slice(&cmd.to_le_bytes());
        bcdc.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bcdc.extend_from_slice(&(u32::from(id) << 16 | 0x02).to_le_bytes());
        bcdc.extend_from_slice(&0u32.to_le_bytes());
        bcdc.extend_from_slice(payload);

        let len = (12 + bcdc.len()) as u16;
        let mut frame = Vec::new();
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&(!len).to_le_bytes());
        frame.extend_from_slice(&(u32::from(seq) | 12 << 24).to_le_bytes());
        frame.extend_from_slice(&0u32.to_le_bytes());
        frame.extend_from_slice(&bcdc);
        frame.resize(frame.len().next_multiple_of(4), 0);
        frame
    }

    /// The whole control path: a frame out on function 2, the card's
    /// interrupt, and the answer back.
    #[test]
    fn a_control_frame_is_answered_and_the_card_says_so() {
        let mut e = wifi_host_up();
        assert_eq!(rd(&mut e, INT_STATUS) & INT_CARD, 0, "nothing pending yet");

        let mut iovar = b"bus:txglomalign\0".to_vec();
        iovar.extend_from_slice(&4u32.to_le_bytes());
        let mut frame = ctrl_frame(255, 1, 263, &iovar);
        io_bytes(&mut e, 2, 0x8000, true, true, &mut frame);

        assert_eq!(
            rd(&mut e, INT_STATUS) & INT_CARD,
            INT_CARD,
            "the card interrupt never reached the host"
        );
        assert!(e.irq_asserted());
        // A level: only the chip can drop it.
        wr(&mut e, INT_STATUS, 0xFFFF_FFFF);
        assert_eq!(rd(&mut e, INT_STATUS) & INT_CARD, INT_CARD);
        cmd(&mut e, 52, 0x05 << 9, R1, 0);
        assert_eq!(rd(&mut e, RESPONSE0) & 0xFF, 0x02);

        backplane_wr(&mut e, SD_CORE + SD_INTSTATUS, I_HMB_FRAME_IND);
        assert_eq!(rd(&mut e, INT_STATUS) & INT_CARD, 0);
        assert!(!e.irq_asserted());

        let mut head = vec![0u8; 64];
        io_bytes(&mut e, 2, 0x8000, false, false, &mut head);
        let len = u16::from_le_bytes([head[0], head[1]]);
        assert_eq!(len ^ u16::from_le_bytes([head[2], head[3]]), u16::MAX);
        assert!(len as usize <= head.len(), "the answer fits in one read");
        let sw = u32::from_le_bytes(head[4..8].try_into().unwrap());
        assert_eq!(sw & 0x0F00, 0, "the control channel");
        assert_eq!(sw >> 24, 12, "the payload starts after the header");
        let window = (u32::from_le_bytes(head[8..12].try_into().unwrap()) >> 8) as u8;
        assert_ne!(window.wrapping_sub(0), 0);
        let flags = u32::from_le_bytes(head[20..24].try_into().unwrap());
        assert_eq!(flags >> 16, 1);
        assert_eq!(flags & 0x01, 0);

        // The chip raises the indication once per frame.
        assert_eq!(rd(&mut e, INT_STATUS) & INT_CARD, 0);
        assert!(!e.irq_asserted());
    }

    /// One block through the Buffer Data Port the way the stock stages read
    /// it: `PRESENT_STATE` before every word.
    fn pio_block(e: &mut Emmc2) -> Vec<u8> {
        let mut out = Vec::new();
        while out.len() < 512 {
            let mut polls = 0;
            while rd(e, PRESENT_STATE) & PS_BUF_READ_EN == 0 {
                polls += 1;
                assert!(polls < 100, "the block never arrived");
            }
            out.extend_from_slice(&rd(e, BUFFER_DATA).to_le_bytes());
        }
        out
    }

    fn card_block(e: &Emmc2, lba: u32) -> Vec<u8> {
        let mut b = [0u8; 512];
        e.card().unwrap().read_block(lba, &mut b);
        b.to_vec()
    }

    /// `BUF_READ_EN` is a level: it drops after a block's last word and the
    /// next block is there on the second status poll.
    #[test]
    fn pio_read_drops_buffer_read_enable_between_blocks() {
        let mut e = host();
        enumerate(&mut e, 0x40FF_8000);
        select(&mut e);
        wr(&mut e, BLOCK_SIZE_COUNT, (3 << 16) | 512);
        cmd(
            &mut e,
            18,
            5,
            R1_DATA,
            TM_BLOCK_COUNT_EN | TM_READ | TM_MULTI | (TM_AUTO_CMD12 << TM_AUTO_CMD_SHIFT),
        );
        assert_eq!(rd(&mut e, INT_STATUS), INT_CMD_COMPLETE | INT_BUF_READ_RDY);
        wr(&mut e, INT_STATUS, INT_CMD_COMPLETE | INT_BUF_READ_RDY);
        assert_eq!(pio_block(&mut e), card_block(&e, 5));

        assert_eq!(rd(&mut e, INT_STATUS), 0);
        assert_eq!(
            rd(&mut e, PRESENT_STATE) & (PS_BUF_READ_EN | PS_DAT_INHIBIT),
            PS_BUF_READ_EN | PS_DAT_INHIBIT
        );
        assert_eq!(rd(&mut e, INT_STATUS), INT_BUF_READ_RDY);
        assert_eq!(pio_block(&mut e), card_block(&e, 6));

        assert_eq!(
            rd(&mut e, PRESENT_STATE) & (PS_BUF_READ_EN | PS_DAT_INHIBIT),
            PS_DAT_INHIBIT
        );
        assert_eq!(pio_block(&mut e), card_block(&e, 7));

        assert_eq!(
            rd(&mut e, PRESENT_STATE) & (PS_BUF_READ_EN | PS_DAT_INHIBIT),
            0
        );
        assert_ne!(rd(&mut e, INT_STATUS) & INT_XFER_COMPLETE, 0);
    }

    #[test]
    fn a_buffer_read_in_the_gap_does_not_advance() {
        let mut e = host();
        enumerate(&mut e, 0x40FF_8000);
        select(&mut e);
        wr(&mut e, BLOCK_SIZE_COUNT, 512);
        cmd(&mut e, 18, 9, R1_DATA, TM_READ | TM_MULTI);
        assert_eq!(pio_block(&mut e), card_block(&e, 9));
        for _ in 0..3 {
            assert_eq!(rd(&mut e, BUFFER_DATA), 0);
        }
        assert_eq!(pio_block(&mut e), card_block(&e, 10));
        cmd(&mut e, 12, 0, 0x1B, 0);
        assert_eq!(rd(&mut e, BUFFER_DATA), 0);
    }

    #[test]
    fn the_next_block_arrives_on_time_without_polls() {
        let mut e = host();
        enumerate(&mut e, 0x40FF_8000);
        select(&mut e);
        wr(&mut e, BLOCK_SIZE_COUNT, 512);
        e.advance_to(1000);
        cmd(&mut e, 18, 0, R1_DATA, TM_READ | TM_MULTI);
        wr(&mut e, INT_STATUS, 0xFFFF_FFFF);
        wr(&mut e, INT_SIGNAL_EN, INT_BUF_READ_RDY);
        assert_eq!(pio_block(&mut e), card_block(&e, 0));
        assert!(!e.irq_asserted());
        e.advance_to(1000 + BLOCK_WIRE_US - 1);
        assert!(!e.irq_asserted());
        e.advance_to(1000 + BLOCK_WIRE_US);
        assert!(e.irq_asserted());
        assert_eq!(rd(&mut e, BUFFER_DATA), u32::from_le_bytes([1, 0, 3, 2]));
    }

    #[test]
    fn data_inhibit_follows_the_transfer() {
        let mut e = host();
        enumerate(&mut e, 0x40FF_8000);
        select(&mut e);
        wr(&mut e, BLOCK_SIZE_COUNT, 512);
        assert_eq!(rd(&mut e, PRESENT_STATE) & PS_DAT_INHIBIT, 0);

        cmd(&mut e, 17, 5, R1_DATA, TM_READ);
        assert_ne!(rd(&mut e, PRESENT_STATE) & PS_DAT_INHIBIT, 0);
        for _ in 0..128 {
            rd(&mut e, BUFFER_DATA);
        }
        assert_eq!(rd(&mut e, PRESENT_STATE) & PS_DAT_INHIBIT, 0, "CMD17 done");

        cmd(&mut e, 18, 5, R1_DATA, TM_READ | TM_MULTI);
        for _ in 0..2 {
            pio_block(&mut e);
        }
        assert_ne!(
            rd(&mut e, PRESENT_STATE) & PS_DAT_INHIBIT,
            0,
            "still reading ahead"
        );
        cmd(&mut e, 12, 0, 0x1B, TM_READ);
        assert_eq!(rd(&mut e, PRESENT_STATE) & PS_DAT_INHIBIT, 0, "stopped");

        cmd(&mut e, 18, 5, R1_DATA, TM_READ | TM_MULTI);
        wr(&mut e, CLOCK_CONTROL, SRST_DATA);
        assert_eq!(rd(&mut e, PRESENT_STATE) & PS_DAT_INHIBIT, 0, "reset");
    }

    /// edk2's MmcDxe follows every write with CMD55 + ACMD22 on a 4-byte block
    /// and fails the write unless Buffer Read Ready comes.
    #[test]
    fn acmd22_reads_the_written_block_count_through_pio() {
        let mut e = host();
        enumerate(&mut e, 0x40FF_8000);
        select(&mut e);
        wr(&mut e, BLOCK_SIZE_COUNT, 512);
        cmd(&mut e, 24, 5, R1_DATA, 0);
        for i in 0..128u32 {
            wr(&mut e, BUFFER_DATA, i);
        }
        wr(&mut e, INT_STATUS, 0xFFFF_FFFF);

        cmd(&mut e, 55, 0x0001_0000, R1, 0);
        wr(&mut e, BLOCK_SIZE_COUNT, 4);
        cmd(&mut e, 22, 0, R1_DATA, TM_READ);
        assert_ne!(rd(&mut e, INT_STATUS) & INT_BUF_READ_RDY, 0);
        assert_eq!(rd(&mut e, BUFFER_DATA), 0x0100_0000);
        assert_ne!(rd(&mut e, INT_STATUS) & INT_XFER_COMPLETE, 0);
    }

    #[test]
    fn voltage_switch_holds_the_lines_low_until_the_clock_returns_at_1v8() {
        let mut e = host();
        let ocr = enumerate(&mut e, 0x41FF_8000);
        assert_eq!(ocr & 0xC100_0000, 0xC100_0000, "ready, CCS, S18A");
        wr(&mut e, CLOCK_CONTROL, CLK_INTLEN | CLK_SD_EN);
        cmd(&mut e, 11, 0, R1, 0);
        assert_eq!(rd(&mut e, PRESENT_STATE) & PS_LINES_CMD_DAT, 0, "card busy");
        wr(&mut e, CLOCK_CONTROL, CLK_INTLEN);
        assert_eq!(rd(&mut e, PRESENT_STATE) & PS_LINES_CMD_DAT, 0);
        wr(&mut e, HOST_CONTROL2, HC2_1V8);
        assert_eq!(rd(&mut e, HOST_CONTROL2) & HC2_1V8, HC2_1V8);
        wr(&mut e, CLOCK_CONTROL, CLK_INTLEN | CLK_SD_EN);
        assert_eq!(
            rd(&mut e, PRESENT_STATE) & PS_LINES_CMD_DAT,
            PRESENT_STATE_IDLE & PS_LINES_CMD_DAT,
            "lines back at their idle levels"
        );
        assert!(e.card().unwrap().signal_1v8());
    }

    #[test]
    fn voltage_switch_fails_if_the_host_stays_at_3v3_and_power_off_resets_it() {
        let mut e = host();
        enumerate(&mut e, 0x41FF_8000);
        cmd(&mut e, 11, 0, R1, 0);
        wr(&mut e, CLOCK_CONTROL, CLK_INTLEN | CLK_SD_EN);
        assert_eq!(
            rd(&mut e, PRESENT_STATE) & PS_LINES_CMD_DAT,
            0,
            "still held low"
        );
        wr(&mut e, HOST_CONTROL, HC_BUS_POWER | (7 << 9));
        wr(&mut e, HOST_CONTROL, 0);
        assert_eq!(
            rd(&mut e, PRESENT_STATE) & PS_LINES_CMD_DAT,
            PRESENT_STATE_IDLE & PS_LINES_CMD_DAT,
            "lines back at their idle levels"
        );
        assert!(!e.card().unwrap().signal_1v8());
    }

    #[test]
    fn tuning_succeeds_on_the_first_block() {
        let mut e = host();
        enumerate(&mut e, 0x40FF_8000);
        select(&mut e);
        wr(&mut e, HOST_CONTROL2, HC2_1V8 | HC2_EXEC_TUNING);
        wr(&mut e, INT_STATUS_EN, INT_BUF_READ_RDY);
        wr(&mut e, INT_SIGNAL_EN, INT_BUF_READ_RDY);
        wr(&mut e, BLOCK_SIZE_COUNT, 64);
        cmd(&mut e, 19, 0, R1_DATA, TM_READ);
        assert_eq!(rd(&mut e, INT_STATUS), INT_BUF_READ_RDY);
        assert!(e.irq_asserted());
        let hc2 = rd(&mut e, HOST_CONTROL2);
        assert_eq!(hc2 & (HC2_EXEC_TUNING | HC2_TUNED_CLK), HC2_TUNED_CLK);
        assert_eq!(e.read(0x0E, Width::Half).unwrap() >> 8, 19);
    }

    #[test]
    fn reset_all_clears_interrupt_enables_but_a_data_reset_does_not() {
        let mut e = host();
        wr(&mut e, INT_SIGNAL_EN, 1);
        wr(&mut e, CLOCK_CONTROL, SRST_DATA | CLK_INTLEN);
        assert_eq!(rd(&mut e, INT_STATUS_EN), 0xFFFF_FFFF);
        wr(&mut e, HOST_CONTROL, HC_BUS_POWER | (7 << 9) | 2);
        wr(&mut e, CLOCK_CONTROL, SRST_ALL | CLK_INTLEN);
        assert_eq!(rd(&mut e, INT_STATUS_EN), 0);
        assert_eq!(rd(&mut e, INT_SIGNAL_EN), 0);
        assert_eq!(rd(&mut e, CLOCK_CONTROL), 0, "clock back at reset too");
        assert_eq!(rd(&mut e, HOST_CONTROL), HOST_CONTROL_FIXED);
    }

    #[test]
    fn narrow_writes_merge_and_only_the_command_half_issues() {
        let mut e = host();
        e.write(CMD_XFER, Width::Half, TM_READ).unwrap();
        assert_eq!(
            rd(&mut e, INT_STATUS),
            0,
            "transfer mode alone issues nothing"
        );
        e.write(CMD_XFER + 2, Width::Half, (13 << 8) | R1).unwrap();
        assert_eq!(rd(&mut e, INT_STATUS), INT_CMD_COMPLETE);
        assert_eq!(rd(&mut e, CMD_XFER), ((13 << 8 | R1) << 16) | TM_READ);
        e.write(INT_STATUS + 1, Width::Byte, 0xFF).unwrap();
        assert_eq!(rd(&mut e, INT_STATUS), INT_CMD_COMPLETE);
        e.write(INT_STATUS, Width::Byte, 0x01).unwrap();
        assert_eq!(rd(&mut e, INT_STATUS), 0);
    }

    /// 2020-era bootcode's two waits on the legacy EMMC.
    #[test]
    fn legacy_host_finishes_the_reset_and_clock_waits_of_2020_bootcode() {
        let mut e = Emmc2::new_legacy();
        wr(&mut e, CLOCK_CONTROL, SRST_MASK);
        assert_eq!(rd(&mut e, CLOCK_CONTROL) & SRST_MASK, 0);
        wr(&mut e, CLOCK_CONTROL, 0x000E_E201);
        assert_eq!(rd(&mut e, CLOCK_CONTROL), 0x000E_E201 | CLK_STABLE);
    }

    /// What real boards' logs show of this host with no card; none of EMMC2's
    /// identity shows through.
    #[test]
    fn legacy_host_is_an_empty_bus_with_its_own_identity() {
        let mut e = Emmc2::new_legacy();
        assert_eq!(e.name(), "emmc");
        wr(&mut e, CLOCK_CONTROL, SRST_ALL);
        assert_eq!(rd(&mut e, HOST_CONTROL), 0, "CTL0: 0x00000000");
        wr(&mut e, HOST_CONTROL, 0x0000_0F00);
        assert_eq!(rd(&mut e, HOST_CONTROL), 0x0000_0F00, "CTL0: 0x00000f00");
        assert_eq!(rd(&mut e, PRESENT_STATE), 0x1FFF_0000);
        for off in [
            CAPABILITIES_0,
            CAPABILITIES_1,
            MAX_CURRENT,
            CONTROLLER_VERSION,
        ] {
            assert_eq!(rd(&mut e, off), 0, "{off:#x}");
        }

        wr(&mut e, INT_STATUS_EN, 0xFFFF_FFFF);
        cmd(&mut e, 0, 0, 0, 0);
        assert_eq!(rd(&mut e, INT_STATUS), INT_CMD_COMPLETE);
        wr(&mut e, INT_STATUS, 0xFFFF_FFFF);
        cmd(&mut e, 55, 0, R1, 0);
        assert_eq!(rd(&mut e, INT_STATUS), INT_ERR_CMD_TIMEOUT | INT_ERROR);
    }
}
