//! Legacy BCM2711 DMA controller — 15 channels at `0x7E00_7000 + ch * 0x100`.
//!
//! Registers, fields and the control-block layout: `specs/dma.toml`, and
//! `specs/dma_vpu.toml` for the second controller.
//!
//! start4 drives this for bulk memory-to-memory copies: `dma_memcpy` copies
//! anything under 1 KiB with the scalar loop and queues everything larger
//! here.
//!
//! Note the control-block layout is *not* the DMA4 ("dma40") one — channel 11
//! is modelled separately by [`Dma4`], which the address decoder keeps ahead
//! of this device.
//!
//! [`Dma4`]: super::dma4::Dma4

use crate::bus::{BusResult, MmioDevice, Width};

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

pub const NUM_CHAN: usize = dma_vpu::CS_COUNT as usize;
/// Interrupt lines the GIC has for the `0x7E00_7000` controller: one per
/// channel for 0..=6, then one for 7/8 and one for 9/10
/// ([`DmaLegacy::irq_lines`]).
pub const NUM_GIC_LINES: usize = 9;
const NUM_REGS: usize = ((DEBUG - CS) / 4 + 1) as usize;

const _: () =
    assert!(dma_vpu::CS == CS && dma_vpu::DEBUG == DEBUG && dma_vpu::CS_STRIDE == CHAN_STRIDE);

pub const COVERAGE: Coverage = Coverage {
    block: "dma",
    decoded: &[
        CS, CONBLK_AD, TI, SOURCE_AD, DEST_AD, TXFR_LEN, STRIDE, NEXTCONBK, DEBUG, INT_STATUS,
        ENABLE,
    ],
};

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
    enable: u32,
    int_status: u32,
    /// Channel whose `CS.ACTIVE` was just set; [`crate::machine::Machine`]
    /// consumes it and walks the control-block chain.
    start_pending: Option<usize>,
}

impl DmaLegacy {
    pub fn new() -> DmaLegacy {
        DmaLegacy {
            global_regs: true,
            ..DmaLegacy::default()
        }
    }

    /// The `0x7EE0_4100` controller start4's dmalib drives. Its channel 15 is
    /// at `0x7EE0_5000`, so all 16 channel slots are used and there is no room
    /// for the global words.
    pub fn new_vpu() -> DmaLegacy {
        DmaLegacy {
            global_regs: false,
            ..DmaLegacy::default()
        }
    }

    pub fn conblk_ad(&self, ch: usize) -> u32 {
        self.regs[ch][1]
    }

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

    /// One output per GIC line: a channel's is up while its `CS.INT` is, and
    /// channels 7/8 and 9/10 share a line each.
    pub fn irq_lines(&self) -> [bool; NUM_GIC_LINES] {
        let int = |ch: usize| self.regs[ch][0] & CS_INT != 0;
        [
            int(0),
            int(1),
            int(2),
            int(3),
            int(4),
            int(5),
            int(6),
            int(7) || int(8),
            int(9) || int(10),
        ]
    }

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
        if reg == 0 {
            // `CS.END` and `CS.INT` are write-1-to-clear: a driver acking its
            // completion interrupt writes just `CS.INT`, and storing that as
            // the whole register would leave the interrupt up for ever.
            let keep = self.regs[ch][0] & (CS_END | CS_INT) & !value;
            self.regs[ch][0] = (value & !(CS_END | CS_INT)) | keep;
        } else {
            self.regs[ch][reg] = value;
        }
        // Two ways a transfer starts, and the firmware uses the second:
        // `CS.ACTIVE` set with a chain already in `CONBLK_AD`, or `CONBLK_AD`
        // written while `ACTIVE` is set. `ACTIVE` with a null `CONBLK_AD`
        // idles.
        let active = self.regs[ch][0] & CS_ACTIVE != 0;
        let armed = self.regs[ch][1] != 0;
        if (reg == 0 && active && armed) || (reg == 1 && active && value != 0) {
            self.start_pending = Some(ch);
        }
        Ok(())
    }
}
