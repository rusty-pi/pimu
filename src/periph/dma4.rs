//! BCM2711 DMA4 ("dma40") channel at `0x7E00_7B00` — the 40-bit DMA engine the
//! main bootloader drives to scrub / move DRAM.
//!
//! The bootloader (`0x0008b0xx` submit, `0x0008b3f4` status check):
//!   1. writes `DEBUG` (`+0x0C`) = `0x400` to clear the error latch,
//!   2. writes `CB` (`+0x04`) = control-block address `>> 5`,
//!   3. writes `CS` (`+0x00`) with bit 0 (ACTIVE) set to start,
//!   4. polls `CS`: bit 1 (END) set + bit 0 (ACTIVE) clear + bit 10 (ERROR)
//!      clear ⇒ success; ERROR set ⇒ `rc -1`; otherwise keep waiting.
//!
//! Control block (32 bytes, little-endian words):
//!   `+0x00 TI  +0x04 SRC  +0x08 SRCI  +0x0C DEST  +0x10 DESTI  +0x14 LEN
//!    +0x18 NEXT_CB(>>5)  +0x1C —`
//! A `SRC` of 0 is a zero-fill (DRAM scrub); otherwise SRC→DEST is copied.
//! Addresses are folded onto the model's flat DRAM by the caller.
//!
//! The actual `SRC`/`DEST` transfer needs bus access, so [`Machine`] pulls the
//! pending descriptor out of here after the `CS` write and runs it.
//!
//! [`Machine`]: crate::machine::Machine

use crate::bus::{BusResult, MmioDevice, Width};

pub const CS: u32 = 0x00;
pub const CB: u32 = 0x04;
pub const DEBUG: u32 = 0x0C;

pub const CS_ACTIVE: u32 = 1 << 0;
pub const CS_END: u32 = 1 << 1;
pub const CS_ERROR: u32 = 1 << 10;
/// Bit the status check (`0x0008b3f4`) treats as "still busy" alongside ACTIVE.
pub const CS_BUSY_EXTRA: u32 = 1 << 24;

/// One decoded control block, ready for [`Machine`](crate::machine::Machine) to
/// execute.
#[derive(Debug, Clone, Copy)]
pub struct Dma4Cb {
    pub src: u32,
    pub dest: u32,
    pub len: u32,
    pub next: u32,
}

#[derive(Default)]
pub struct Dma4 {
    cs: u32,
    cb: u32,
    debug: u32,
    /// Set when `CS` is written with ACTIVE; [`Machine`] clears it by calling
    /// [`Dma4::take_start`].
    start_pending: bool,
}

impl Dma4 {
    pub fn new() -> Dma4 {
        Dma4::default()
    }

    /// The control-block address the channel is armed with (`CB` reg `<< 5`).
    pub fn cb_addr(&self) -> u32 {
        self.cb << 5
    }

    /// If a start was just requested, consume it. [`Machine`] then walks the CB
    /// chain and calls [`Dma4::finish`].
    pub fn take_start(&mut self) -> bool {
        std::mem::take(&mut self.start_pending)
    }

    /// Mark the transfer complete: END set, ACTIVE / ERROR / busy-extra clear,
    /// error latch clear.
    pub fn finish(&mut self) {
        self.cs = (self.cs & !(CS_ACTIVE | CS_ERROR | CS_BUSY_EXTRA)) | CS_END;
        self.debug = 0;
    }
}

impl MmioDevice for Dma4 {
    fn name(&self) -> &'static str {
        "dma4"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset & !3 {
            CS => self.cs,
            CB => self.cb,
            DEBUG => self.debug,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        match offset & !3 {
            CS => {
                self.cs = value;
                if value & CS_ACTIVE != 0 {
                    self.start_pending = true;
                }
            }
            CB => self.cb = value,
            DEBUG => self.debug &= !value, // write-1-to-clear
            _ => {}
        }
        Ok(())
    }
}
