//! BCM2711 EMMC2 — the SD Host Controller (an Arasan SDHCI v3.00) the main
//! bootloader drives once it picks "Boot mode: SD", and Linux's `sdhci-iproc`
//! after it. Register block at `0x7E34_0000` (`0xFE34_0000` to the ARM).
//!
//! Models the clock / reset / present-state plumbing *and* a working command
//! engine wired to an [`SdCard`], with three data paths:
//!
//! * **PIO** through the Buffer Data Port, both directions — what the
//!   bootloader and start4 use (CMD17/CMD18 + CMD12) to pull `start4.elf` and
//!   the kernel;
//! * **SDMA** — a single system address, pausing with a DMA interrupt at each
//!   buffer boundary until the host writes the next address;
//! * **ADMA2**, 32-bit descriptors (CAPS0 bit 28 says no 64-bit system bus) —
//!   what Linux uses (`mmc0: SDHCI controller on fe340000.mmc using ADMA` on
//!   the real board).
//!
//! DMA needs the RAM, so a command (or an SDMA address write) that starts one
//! only marks it pending; [`crate::machine::Machine`] then calls
//! [`Emmc2::run_dma`] straight after the register write, which completes the
//! whole transfer at once. Also: auto-CMD12 / auto-CMD23, CMD11 1.8 V
//! switching (the card holds CMD/DAT low until the host's clock comes back at
//! 1.8 V), tuning (one CMD19 succeeds), the interrupt output
//! ([`Emmc2::irq_asserted`], INTID 158 on the GIC) and status gating by
//! INT_STATUS_EN, as the SDHCI spec has it.
//!
//! The same engine is the chip's other Arasan host too, the legacy EMMC at
//! `0x7E30_0000` ([`Emmc2::new_legacy`], `specs/emmc.toml`): the WiFi chip's
//! SDIO host on a Pi 4, and the host 2020-era bootcode reads the SD card
//! through. Nothing is on its bus — the SD slot reaches it only through a mux
//! the model does not follow — so every command that expects a response times
//! out there (#64).
//!
//! SDHCI register map (word offsets):
//! ```text
//!   0x00 SDMA address / arg2      0x04 block size[11:0] | SDMA boundary[14:12] | count[31:16]
//!   0x08 argument                 0x0C transfer mode[15:0] | command[31:16]
//!   0x10..0x1C RESPONSE0..3       0x20 buffer data port
//!   0x24 present state            0x28 host/power/gap/wakeup control
//!   0x2C clock ctl[15:0] | timeout[23:16] | sw-reset[26:24]
//!   0x30 int status               0x34 int status enable   0x38 int signal enable
//!   0x3C auto-CMD error[15:0] | host control 2[31:16]
//!   0x40/0x44 capabilities        0x48 max current
//!   0x54 ADMA error status        0x58 ADMA system address  0xFC controller version
//! ```

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::mem::Ram;
use crate::periph::sdcard::SdCard;

// The read-only identity (`CAPABILITIES_*`, `MAX_CURRENT`,
// `CONTROLLER_VERSION`), the idle `PRESENT_STATE` and `HOST_CONTROL.FIXED` are
// measured on a Pi 4B rev 1.5 (`specs/emmc2.toml`).
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
    INT_STATUS_BUF_WRITE_RDY_MASK as INT_BUF_WRITE_RDY,
    INT_STATUS_CMD_COMPLETE_MASK as INT_CMD_COMPLETE, INT_STATUS_DMA_MASK as INT_DMA,
    INT_STATUS_EN, INT_STATUS_ERROR_MASK as INT_ERROR, INT_STATUS_ERR_ADMA_MASK as INT_ERR_ADMA,
    INT_STATUS_ERR_CMD_TIMEOUT_MASK as INT_ERR_CMD_TIMEOUT,
    INT_STATUS_XFER_COMPLETE_MASK as INT_XFER_COMPLETE, MAX_CURRENT,
    MAX_CURRENT_RESET as MAX_CURRENT_VALUE, PRESENT_STATE,
    PRESENT_STATE_BUF_READ_EN_MASK as PS_BUF_READ_EN,
    PRESENT_STATE_BUF_WRITE_EN_MASK as PS_BUF_WRITE_EN, PRESENT_STATE_CMD_LINE_MASK,
    PRESENT_STATE_DAT_LINES_MASK, PRESENT_STATE_RESET as PRESENT_STATE_IDLE, RESPONSE0, RESPONSE1,
    RESPONSE2, RESPONSE3, SDMA_ADDR,
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

/// The legacy EMMC: the command engine. Its identity registers are not
/// measured, so they stay stubbed.
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

/// EMMC2's, measured on a Pi 4B rev 1.5 (`specs/emmc2.toml`).
const EMMC2_ID: Identity = Identity {
    name: "emmc2",
    caps0: CAPS0,
    caps1: CAPS1,
    max_current: MAX_CURRENT_VALUE,
    version: VERSION,
    host_control_fixed: HOST_CONTROL_FIXED,
};

/// The legacy EMMC's. Real boards' bootloader logs show its HOST_CONTROL
/// reading back just what was written (`specs/emmc.toml`); the capability and
/// version registers are unmeasured there, and read 0 rather than EMMC2's.
const LEGACY_ID: Identity = Identity {
    name: "emmc",
    caps0: 0,
    caps1: 0,
    max_current: 0,
    version: 0,
    host_control_fixed: 0,
};

/// DAT[3:0] and CMD line levels.
const PS_LINES_CMD_DAT: u32 = PRESENT_STATE_DAT_LINES_MASK | PRESENT_STATE_CMD_LINE_MASK;

/// `CMD_XFER.AUTO_CMD` values.
const TM_AUTO_CMD12: u32 = 1;
const TM_AUTO_CMD23: u32 = 2;

/// `HOST_CONTROL.DMA_SELECT`: 32-bit ADMA2.
const HC_DMA_ADMA2_32: u32 = 2;

/// Software-reset bits — self-clearing in the model.
const SRST_MASK: u32 = SRST_ALL | SRST_CMD | SRST_DATA;

/// Data-circuit interrupt bits cleared by a DAT software reset (spec: buffer
/// ready both ways, DMA, block-gap, transfer complete) — command complete is
/// explicitly preserved.
const INT_DATA_BITS: u32 =
    INT_XFER_COMPLETE | INT_DMA | INT_BLOCK_GAP | INT_BUF_WRITE_RDY | INT_BUF_READ_RDY;

/// ADMA2 descriptor attributes.
const ADMA_VALID: u16 = 1 << 0;
const ADMA_END: u16 = 1 << 1;
const ADMA_INT: u16 = 1 << 2;
const ADMA_ACT_SHIFT: u16 = 4;
const ADMA_ACT_TRAN: u16 = 2;
const ADMA_ACT_LINK: u16 = 3;
/// `ADMA_ERROR.STATE` at the error: fetching a descriptor, transferring.
const ADMA_ST_FDS: u32 = 1;
const ADMA_ST_TFR: u32 = 3;
/// Descriptors walked per transfer before the engine gives up (a link loop).
const ADMA_MAX_DESCRIPTORS: usize = 1 << 16;

/// A DMA address as the RAM sees it. The emmc2bus's `dma-ranges` is 1:1, so
/// an address inside the board's RAM is physical: on a 2 GB board that
/// includes `0x4000_0000..0x8000_0000`, where Linux's DMA32 zone puts block
/// buffers. Only an address past the RAM is one of the VPU's cache aliases,
/// and folds onto the first gigabyte.
fn dma_ram_addr(addr: u32, ram_len: usize) -> u32 {
    if (addr as usize) < ram_len {
        addr
    } else {
        addr & 0x3FFF_FFFF
    }
}

/// An SDMA or ADMA2 transfer in flight: the data (read from the card, or to be
/// written to it) and how far the engine has got.
struct Dma {
    adma: bool,
    write: bool,
    buf: Vec<u8>,
    pos: usize,
    /// SDMA: the next system address.
    sdma_addr: u32,
    /// Where a write's blocks go, and in what size.
    lba: u32,
    block_size: usize,
    /// Issue CMD12 once the data is through.
    auto_cmd12: bool,
}

/// A PIO write in flight: blocks the host pushes through the Buffer Data Port.
struct PioWrite {
    lba: u32,
    /// `None` = open-ended (until CMD12).
    blocks_left: Option<u32>,
    auto_cmd12: bool,
}

pub struct Emmc2 {
    id: Identity,
    /// Sticky storage for offsets without special behaviour.
    reg: BTreeMap<u32, u32>,
    /// The inserted card, if any. `None` is an empty slot: nothing drives
    /// CMD, so every command that expects a response times out.
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
    /// A register block (CMD6 status, SCR, ...) the card sends instead of
    /// image data; one block.
    read_synthetic: Option<Vec<u8>>,
    /// Issue CMD12 when the PIO read's last block has been drained.
    read_auto_cmd12: bool,
    /// PIO write state and the partly filled block.
    pio_write: Option<PioWrite>,
    wbuf: Vec<u8>,
    /// DMA transfer in flight, and whether [`Self::run_dma`] has work.
    dma: Option<Dma>,
    dma_pending: bool,
    /// RAM ranges `[start, end)` the last DMA run wrote, for an ARM core that
    /// caches translated code to drop (bounded to one transfer).
    dma_written: Vec<(u32, u32)>,
    /// CMD11 accepted: the card holds CMD/DAT low until the host restarts the
    /// SD clock with 1.8 V signalling on.
    switching_1v8: bool,
    /// Debug: 32-bit words handed out through the Buffer Data Port this transfer.
    words_out: u64,
    dbg: bool,
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
            pio_write: None,
            wbuf: Vec::new(),
            dma: None,
            dma_pending: false,
            dma_written: Vec::new(),
            switching_1v8: false,
            words_out: 0,
            dbg: std::env::var_os("EMMC_DBG").is_some(),
        }
    }
}

impl Emmc2 {
    pub fn new() -> Emmc2 {
        Emmc2::default()
    }

    /// The legacy EMMC at `0x7E30_0000`. Its bus is empty until the SD-slot
    /// mux routes the card to it (`Machine::route_sd_slot`).
    pub fn new_legacy() -> Emmc2 {
        Emmc2 {
            id: LEGACY_ID,
            ..Emmc2::default()
        }
    }

    /// Insert a card backed by `image` (a raw block device: MBR + FAT + files).
    pub fn insert_card(&mut self, image: Vec<u8>) {
        self.card = Some(SdCard::new(image));
    }

    /// Insert a card on `disk` — an image file read on demand, so a big card
    /// costs only the blocks the guest touches.
    pub fn insert_disk(&mut self, disk: crate::periph::disk::Disk) {
        self.card = Some(SdCard::with_disk(disk));
    }

    /// Disconnect the card from this host, state and all, the way the SD-slot
    /// mux takes its lines away: to this host the slot is then empty.
    pub fn take_card(&mut self) -> Option<SdCard> {
        self.card.take()
    }

    /// Connect a card (or nothing) that another host had: the card keeps the
    /// state it was in, since only the lines to it moved.
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

    /// Latch interrupt status bits — only those INT_STATUS_EN lets through.
    fn set_int(&mut self, bits: u32) {
        let cur = self.get(INT_STATUS);
        let en = self.get(INT_STATUS_EN);
        self.reg.insert(INT_STATUS, cur | (bits & en));
    }

    /// INT_STATUS as read: the latched bits plus the error summary.
    fn int_status(&self) -> u32 {
        let st = self.get(INT_STATUS) & !INT_ERROR;
        if st & 0xFFFF_0000 != 0 {
            st | INT_ERROR
        } else {
            st
        }
    }

    /// The controller's interrupt output: a level, high while any latched
    /// status bit is enabled in INT_SIGNAL_EN (normal bits in the low half,
    /// error bits in the high half, the summary bit 15 included).
    pub fn irq_asserted(&self) -> bool {
        self.int_status() & self.get(INT_SIGNAL_EN) != 0
    }

    /// A DMA transfer is waiting for [`Self::run_dma`].
    pub fn dma_pending(&self) -> bool {
        self.dma_pending
    }

    /// RAM ranges `[start, end)` the DMA engine has written since the last
    /// call.
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

    /// SDMA buffer boundary: 4 KiB << BLOCK_SIZE[14:12].
    fn sdma_boundary(&self) -> u32 {
        4096 << ((self.get(BLOCK_SIZE_COUNT) >> 12) & 7)
    }

    /// Stop whatever data transfer is in flight.
    fn reset_data(&mut self) {
        self.data.clear();
        self.data_pos = 0;
        self.read_blocks_left = 0;
        self.read_open_ended = false;
        self.read_synthetic = None;
        self.read_auto_cmd12 = false;
        self.pio_write = None;
        self.wbuf.clear();
        self.dma = None;
        self.dma_pending = false;
    }

    /// The controller sends CMD12 itself after a multi-block transfer; the
    /// card's R1b lands in RESPONSE3.
    fn auto_cmd12(&mut self) {
        if let Some(card) = self.card.as_mut() {
            self.resp[3] = card.command(12, 0).r1.unwrap_or(0);
        }
    }

    /// The data phase is over: transfer complete, and the auto-CMD12 if one
    /// was asked for.
    fn finish_data(&mut self, auto_cmd12: bool) {
        if auto_cmd12 {
            self.auto_cmd12();
        }
        self.set_int(INT_XFER_COMPLETE);
    }

    /// Dispatch the command in the `0x0C` word to the card and latch its result.
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
            // Auto-CMD23: the block count goes to the card first, from
            // ARGUMENT2.
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

        if self.dbg {
            eprintln!(
                "  emmc CMD{index} arg={arg:#010x} mode={mode:#06x} rt={resp_type} data={data_present} \
                 -> r1={:?} silent={} rd={}@{:#x} wr={}@{:#x}",
                response.r1,
                response.no_response,
                response.read_blocks,
                response.read_lba,
                response.write_blocks,
                response.write_lba,
            );
        }

        if response.no_response && resp_type != 0 {
            // Nothing answered: command timeout, no completion.
            self.set_int(INT_ERR_CMD_TIMEOUT);
            return;
        }

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

        if index == 11 && response.r1.is_some() {
            self.switching_1v8 = true;
        }

        if index == 19 && self.get(HOST_CONTROL2) & HC2_EXEC_TUNING != 0 {
            // Tuning: the controller swallows the tuning block and raises
            // only Buffer Read Ready. One pass finds a sampling point.
            let hc2 = self.get(HOST_CONTROL2);
            self.reg
                .insert(HOST_CONTROL2, (hc2 & !HC2_EXEC_TUNING) | HC2_TUNED_CLK);
            self.set_int(INT_BUF_READ_RDY);
            return;
        }

        self.set_int(INT_CMD_COMPLETE);
        if resp_type == 3 {
            // R1b: busy released immediately in the model. CMD12 (STOP) also
            // lands here — ending the open-ended read above covers the rest.
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
    }

    /// Set up an SDMA / ADMA2 transfer for a data command; [`Self::run_dma`]
    /// moves the bytes.
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
                        card.read_block(response.read_lba.wrapping_add(i as u32), &mut b);
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

    /// Run the pending DMA transfer against `ram`: to completion, to the next
    /// SDMA boundary, or to an ADMA error.
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
                            card.write_block(d.lba.wrapping_add(i as u32), block);
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

    /// Move `n` bytes between the transfer buffer and RAM at `addr`.
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

    /// SDMA: one system address, stopping with a DMA interrupt when it
    /// reaches a buffer boundary with data left. `Some(true)` = done,
    /// `Some(false)` = paused.
    fn run_sdma(&mut self, d: &mut Dma, ram: &mut Ram) -> Option<bool> {
        let boundary = self.sdma_boundary();
        while d.pos < d.buf.len() {
            let to_boundary = (boundary - d.sdma_addr % boundary) as usize;
            let n = to_boundary.min(d.buf.len() - d.pos);
            if !self.dma_copy(d, ram, d.sdma_addr, n) {
                // No RAM there: the data goes nowhere (SDHCI 3.00 has no SDMA
                // error status), but the transfer still ends.
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

    /// ADMA2 with 32-bit descriptors: `{attr: u16, len: u16, addr: u32}`,
    /// walked from ADMA_ADDR. `Some(true)` = done, `None` = ADMA error.
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
                // The descriptors ran out before the data did.
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

    /// Pull one block from the card (or a synthetic register block) into the PIO
    /// buffer and flag Buffer Read Ready.
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
                card.read_block(lba, &mut block);
                card.block_done();
            }
            self.read_lba = lba.wrapping_add(1);
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
            } else if !self.data.is_empty() {
                let cur = self.get(INT_STATUS) & !INT_BUF_READ_RDY;
                self.reg.insert(INT_STATUS, cur);
                self.data.clear();
                self.data_pos = 0;
                let auto = std::mem::take(&mut self.read_auto_cmd12);
                self.finish_data(auto);
            }
        }
        u32::from_le_bytes(w)
    }

    /// Push bytes the host wrote to the Buffer Data Port; each full block goes
    /// to the card.
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
            card.write_block(lba, &self.wbuf[..bs]);
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

    /// The current value of the word at `off`, as a read returns it.
    fn read_word(&mut self, off: u32) -> u32 {
        match off {
            RESPONSE0 => self.resp[0],
            RESPONSE1 => self.resp[1],
            RESPONSE2 => self.resp[2],
            RESPONSE3 => self.resp[3],
            BUFFER_DATA => self.read_buffer_word(),
            CLOCK_CONTROL => {
                // Clock control and the timeout byte (bits 16..23) read
                // back; the real board prints `arasan_emmc_set_clock ... C1:
                // 0x000e0047` (sd-card-boot.log).
                let clk = self.get(CLOCK_CONTROL) & 0x00FF_FFFF;
                // Internal clock reports stable as soon as it is enabled; the
                // software-reset bits (high byte) always read back done.
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
                if self.pio_write.is_some() {
                    ps |= PS_BUF_WRITE_EN;
                }
                if self.switching_1v8 {
                    ps &= !PS_LINES_CMD_DAT;
                }
                ps
            }
            INT_STATUS => self.int_status(),
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
                // Software resets self-clear immediately. A DAT reset tears down
                // the data path (FIFO + data-circuit interrupts) but leaves
                // command-complete alone; a CMD reset clears command-complete; an
                // ALL reset returns every register to its reset value (SDHCI
                // 3.00, 2.2.18) — HOST_CONTROL included, which is why the real
                // board's start4 prints `arasan_emmc_set_clock C0: 0x00800000`
                // right after its reset (sd-card-boot.log).
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
                // The card lets go of CMD/DAT once the SD clock runs again at
                // 1.8 V after CMD11.
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
                // The command register is the upper half: writing it issues.
                if lanes & 0xFFFF_0000 != 0 {
                    self.issue_command(value);
                }
            }
            SDMA_ADDR => {
                self.reg.insert(SDMA_ADDR, value);
                // A paused SDMA resumes from the address the host writes.
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
                    // SD bus power off: the card loses VDD.
                    if let Some(card) = self.card.as_mut() {
                        card.power_off();
                    }
                    self.switching_1v8 = false;
                }
            }
            HOST_CONTROL2 => {
                // The low half is the read-only Auto CMD Error Status.
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
        if self.dbg {
            if off == BUFFER_DATA {
                if self.words_out % 64 == 1 {
                    eprintln!("  emmc R [0x20] -> {word:#010x}  (word {})", self.words_out);
                }
            } else {
                eprintln!("  emmc R [{off:#04x}] -> {word:#010x}");
            }
        }
        // Narrow reads get their lane, right-aligned.
        Ok(match width {
            Width::Word => word,
            Width::Half => (word >> ((offset & 2) * 8)) & 0xFFFF,
            Width::Byte => (word >> ((offset & 3) * 8)) & 0xFF,
        })
    }

    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        if self.dbg && off != BUFFER_DATA {
            eprintln!("  emmc W [{off:#04x}] <- {value:#010x} ({width:?})");
        }
        if off == BUFFER_DATA {
            let n = width.bytes() as usize;
            self.write_buffer(&value.to_le_bytes()[..n]);
            return Ok(());
        }
        // Narrow writes merge into the word; the lane mask says which bytes
        // were actually written (for W1C and for issuing a command).
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

    /// Issue `index` with `arg`, transfer mode `mode`, command flags `flags`
    /// (response type, data-present...).
    fn cmd(e: &mut Emmc2, index: u32, arg: u32, flags: u32, mode: u32) {
        wr(e, ARGUMENT, arg);
        wr(e, CMD_XFER, ((index << 8 | flags) << 16) | mode);
    }

    const R1: u32 = 0x1A;
    const R1_DATA: u32 = 0x3A;

    /// CMD0, ACMD41 ×2 with `ocr`, CMD2, CMD3, CMD7: card in tran.
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

    /// On a 2 GB board Linux's DMA32 buffers sit above the first gigabyte;
    /// they must not fold onto it. Past the RAM, the VPU's aliases still do.
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

    /// With no card, CMD0 (no response) completes and CMD8 times out, which is
    /// how edk2's `ArasanMmcHostDxe` decides the slot is empty. A phantom card
    /// that answered zeros kept its `MmcDxe` retrying for the whole boot.
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

        // Errors: the high half, plus the summary bit 15.
        cmd(&mut e, 5, 0, 0x02, 0);
        assert_eq!(rd(&mut e, INT_STATUS), INT_ERR_CMD_TIMEOUT | INT_ERROR);
        assert!(!e.irq_asserted());
        wr(&mut e, INT_SIGNAL_EN, INT_ERR_CMD_TIMEOUT);
        assert!(e.irq_asserted());
        wr(&mut e, INT_STATUS, INT_ERR_CMD_TIMEOUT);
        assert_eq!(rd(&mut e, INT_STATUS), 0);

        // INT_STATUS_EN gates what latches at all.
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
        // 3 blocks at LBA 5: 100 bytes, a link to a second table, 1436 bytes.
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
        // Auto-CMD12 put the card back in tran and its R1b in RESPONSE3.
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
        // CMD23's count ended the transfer: the card is back in tran.
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
        // 4 KiB boundary, 16 blocks, starting 1 KiB below a boundary.
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

    /// edk2's MmcDxe follows every write with CMD55 + ACMD22 on a 4-byte block
    /// and fails the write unless Buffer Read Ready comes (#51).
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
        // One block, most significant byte first on the bus: 00 00 00 01.
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
        // Clock gated, 1.8 V on, clock back: lines released.
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
        // The driver checks the command register for CMD19 in its handler.
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
        // A byte write-1-to-clear touches only its own lane.
        e.write(INT_STATUS + 1, Width::Byte, 0xFF).unwrap();
        assert_eq!(rd(&mut e, INT_STATUS), INT_CMD_COMPLETE);
        e.write(INT_STATUS, Width::Byte, 0x01).unwrap();
        assert_eq!(rd(&mut e, INT_STATUS), 0);
    }

    /// The legacy EMMC's two waits in 2020-era bootcode (#64): its software
    /// reset clears, and the internal clock comes up stable once enabled. On
    /// the catch-all stub neither happened and that boot hung.
    #[test]
    fn legacy_host_finishes_the_reset_and_clock_waits_of_2020_bootcode() {
        let mut e = Emmc2::new_legacy();
        wr(&mut e, CLOCK_CONTROL, SRST_MASK);
        assert_eq!(rd(&mut e, CLOCK_CONTROL) & SRST_MASK, 0);
        wr(&mut e, CLOCK_CONTROL, 0x000E_E201);
        assert_eq!(rd(&mut e, CLOCK_CONTROL), 0x000E_E201 | CLK_STABLE);
    }

    /// What real boards' logs show of this host with no card: HOST_CONTROL
    /// reads back just what was written, PRESENT_STATE idles at 0x1fff0000,
    /// and CMD55 goes unanswered. None of EMMC2's identity shows through.
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
