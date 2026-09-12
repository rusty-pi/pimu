//! Legacy BCM2711 DMA controller — 15 channels at `0x7E00_7000 + ch * 0x100`.
//!
//! start4 drives this for bulk memory-to-memory copies. `dma_memcpy`
//! (`helpers/dmalib/dmalib.c`, `0x3EC981CC`) switches on length:
//!
//! ```text
//! if (len < 0x400) memcpy(dst, src, len);   // scalar
//! else             <queue a DMA transfer and block on its completion>
//! ```
//!
//! so every copy of 1024 bytes or more goes through here. `dma_set_cs`
//! (`0x3EC98E7C`) confirms the layout: `base = ch < 15 ? 0x7E007000 : 0x7EE04100`,
//! `*(base + ch * 0x100) = flags | 1` to start.
//!
//! Channel registers (words): `+0x00 CS  +0x04 CONBLK_AD  +0x08 TI
//! +0x0C SOURCE_AD  +0x10 DEST_AD  +0x14 TXFR_LEN  +0x18 STRIDE
//! +0x1C NEXTCONBK  +0x20 DEBUG`.
//!
//! Control block (32 bytes): `+0x00 TI  +0x04 SOURCE_AD  +0x08 DEST_AD
//! +0x0C TXFR_LEN  +0x10 STRIDE  +0x14 NEXTCONBK`. Note this is *not* the DMA4
//! ("dma40") layout — channel 11 is modelled separately by [`Dma4`], which the
//! address decoder keeps ahead of this device.
//!
//! [`Dma4`]: super::dma4::Dma4

use crate::bus::{BusResult, MmioDevice, Width};

// Control blocks use the `TI` register's bit layout.
use crate::spec::dma::{
    CONBLK_AD, CS, DEBUG, DEST_AD, ENABLE, INT_STATUS, NEXTCONBK, SOURCE_AD, STRIDE, TI,
    TI_DEST_INC_MASK as TI_DEST_INC, TI_SRC_INC_MASK as TI_SRC_INC, TI_TDMODE_MASK as TI_TDMODE,
    TXFR_LEN,
};
pub use crate::spec::dma::{
    CS_ACTIVE_MASK as CS_ACTIVE, CS_END_MASK as CS_END, CS_INT_MASK as CS_INT,
    CS_STRIDE as CHAN_STRIDE,
};
use crate::spec::{dma_vpu, Coverage};

/// Channel slots in the larger of the two controllers.
pub const NUM_CHAN: usize = dma_vpu::CS_COUNT as usize;
/// Registers modelled per channel (CS .. DEBUG).
const NUM_REGS: usize = ((DEBUG - CS) / 4 + 1) as usize;

// One model serves both controllers, so their channel layouts must agree.
const _: () =
    assert!(dma_vpu::CS == CS && dma_vpu::DEBUG == DEBUG && dma_vpu::CS_STRIDE == CHAN_STRIDE);

/// The `0x7E00_7000` controller: every register is modelled.
pub const COVERAGE: Coverage = Coverage {
    block: "dma",
    decoded: &[
        CS, CONBLK_AD, TI, SOURCE_AD, DEST_AD, TXFR_LEN, STRIDE, NEXTCONBK, DEBUG, INT_STATUS,
        ENABLE,
    ],
};

/// The `0x7EE0_4100` controller: every register is modelled.
pub const COVERAGE_VPU: Coverage = Coverage {
    block: "dma_vpu",
    decoded: &[
        dma_vpu::CS,
        dma_vpu::CONBLK_AD,
        dma_vpu::TI,
        dma_vpu::SOURCE_AD,
        dma_vpu::DEST_AD,
        dma_vpu::TXFR_LEN,
        dma_vpu::STRIDE,
        dma_vpu::NEXTCONBK,
        dma_vpu::DEBUG,
    ],
};

#[derive(Default)]
pub struct DmaLegacy {
    /// Whether offsets `>= 0xFE0` are the controller-wide INT_STATUS / ENABLE
    /// words. True for the `0x7E00_7000` controller; false for the `0x7EE0_4100`
    /// one, whose channel 15 occupies `0xF00..0xFFF`.
    global_regs: bool,
    regs: [[u32; NUM_REGS]; NUM_CHAN],
    /// Global `ENABLE` (`+0xFF0`) and `INT_STATUS` (`+0xFE0`).
    enable: u32,
    int_status: u32,
    /// Channel whose `CS.ACTIVE` was just set; [`crate::machine::Machine`]
    /// consumes it and walks the control-block chain.
    start_pending: Option<usize>,
}

impl DmaLegacy {
    /// The `0x7E00_7000` controller: 15 channels plus the global words.
    pub fn new() -> DmaLegacy {
        DmaLegacy {
            global_regs: true,
            ..DmaLegacy::default()
        }
    }

    /// The `0x7EE0_4100` controller start4's dmalib actually drives. Channel 15
    /// lives at `0x7EE0_5000` (`dma_set_cs` / `dma_chain_start`:
    /// `base = ch < 15 ? 0x7E007000 : 0x7EE04100`, register block at
    /// `base + ch * 0x100`), so all 16 channel slots are used and there is no
    /// room for the global words.
    pub fn new_vpu() -> DmaLegacy {
        DmaLegacy {
            global_regs: false,
            ..DmaLegacy::default()
        }
    }

    /// The control-block address channel `ch` is armed with.
    pub fn conblk_ad(&self, ch: usize) -> u32 {
        self.regs[ch][1]
    }

    /// If a start was just requested, consume it.
    pub fn take_start(&mut self) -> Option<usize> {
        self.start_pending.take()
    }

    /// Mark channel `ch` complete: ACTIVE clear, END set, `CONBLK_AD` = 0 (the
    /// engine walks the chain to its null terminator).
    pub fn finish(&mut self, ch: usize) {
        self.regs[ch][0] = (self.regs[ch][0] & !CS_ACTIVE) | CS_END | CS_INT;
        self.regs[ch][1] = 0;
        self.int_status |= 1 << ch;
    }

    /// Decode one control block. Returns `(ti, src, dest, len, stride, next)`.
    pub fn decode_cb(words: [u32; 6]) -> Cb {
        Cb {
            ti: words[0],
            src: words[1],
            dest: words[2],
            len: words[3],
            stride: words[4],
            next: words[5],
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Cb {
    pub ti: u32,
    pub src: u32,
    pub dest: u32,
    pub len: u32,
    pub stride: u32,
    pub next: u32,
}

impl Cb {
    pub fn src_inc(&self) -> bool {
        self.ti & TI_SRC_INC != 0
    }
    pub fn dest_inc(&self) -> bool {
        self.ti & TI_DEST_INC != 0
    }
    pub fn tdmode(&self) -> bool {
        self.ti & TI_TDMODE != 0
    }
    /// 2D mode splits `TXFR_LEN` into `YLENGTH` (bits 30:16) and `XLENGTH`
    /// (bits 15:0); the transfer is `YLENGTH + 1` rows of `XLENGTH` bytes.
    pub fn rows(&self) -> (u32, u32) {
        if self.tdmode() {
            (((self.len >> 16) & 0x3FFF) + 1, self.len & 0xFFFF)
        } else {
            (1, self.len)
        }
    }
    /// Per-row address advance after each row, as signed 16-bit values.
    pub fn strides(&self) -> (i32, i32) {
        (
            (self.stride & 0xFFFF) as u16 as i16 as i32,
            ((self.stride >> 16) & 0xFFFF) as u16 as i16 as i32,
        )
    }
}

impl MmioDevice for DmaLegacy {
    fn name(&self) -> &'static str {
        "dma-legacy"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        if self.global_regs && offset >= INT_STATUS {
            return Ok(match offset & !3 {
                INT_STATUS => self.int_status,
                ENABLE => self.enable,
                _ => 0,
            });
        }
        let ch = (offset / CHAN_STRIDE) as usize;
        let reg = ((offset % CHAN_STRIDE) / 4) as usize;
        if ch >= NUM_CHAN || reg >= NUM_REGS {
            return Ok(0);
        }
        Ok(self.regs[ch][reg])
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        if self.global_regs && offset >= INT_STATUS {
            match offset & !3 {
                INT_STATUS => self.int_status &= !value,
                ENABLE => self.enable = value,
                _ => {}
            }
            return Ok(());
        }
        let ch = (offset / CHAN_STRIDE) as usize;
        let reg = ((offset % CHAN_STRIDE) / 4) as usize;
        if ch >= NUM_CHAN || reg >= NUM_REGS {
            return Ok(());
        }
        self.regs[ch][reg] = value;
        // Two ways a transfer starts, and the firmware uses the second one:
        //
        //  * `CS.ACTIVE` set while `CONBLK_AD` already holds a chain, or
        //  * `CONBLK_AD` written to a non-null chain while `CS.ACTIVE` is
        //    already set. With ACTIVE set and a null `CONBLK_AD` the channel
        //    simply idles; writing the CB address is what makes it fetch and
        //    run.
        //
        // dmalib does exactly the latter: `dma_subchan_request_specificchannel`
        // -> `dma_set_cs` (`0x3EC98E7C`) sets `CS = flags | 1` up front, and
        // `dma_chain_start` (`0x3EC97544`) then only writes
        // `*(base + ch*0x100 + 4) = cb`. Starting solely on the CS write meant
        // the transfer never ran.
        let active = self.regs[ch][0] & CS_ACTIVE != 0;
        let armed = self.regs[ch][1] != 0;
        if (reg == 0 && active && armed) || (reg == 1 && active && value != 0) {
            self.start_pending = Some(ch);
        }
        Ok(())
    }
}
