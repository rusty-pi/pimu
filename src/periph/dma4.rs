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
//!
//! `SRCI` / `DESTI` are not just the increment flag: bits `[7:0]` are address
//! bits `[39:32]`, which is what makes this the 40-bit channel. The bootloader
//! uses that to reach the PCIe outbound window at `0x6_0000_0000` — the only
//! way a 32-bit VPU can touch the VL805's registers at all; the caller composes
//! the full address and routes it ([`crate::periph::pcie`]).
//!
//! start4's dmalib drives the channel too (channel 11 of its `0x7E00_7000`
//! controller, when it takes the xHCI over for a USB mass-storage boot), and
//! the other way round: `dma_set_cs` (`0x3EC98E7C`) sets `CS.ACTIVE` with no
//! chain loaded, and `dma_chain_start` (`0x3EC97544`) then only writes `CB`. So
//! a transfer starts either way — ACTIVE set with a chain in `CB`, or `CB`
//! written while ACTIVE — and ACTIVE with a null `CB` just idles, the same as
//! the legacy channels ([`super::dma_legacy`]). A finished chain leaves `CB`
//! null and, if a control block asked for it (`TI` bit 0, INTEN), `CS.INT` set
//! and the completion interrupt raised; dmalib's `dma_interrupt` acks it and
//! re-arms ACTIVE for the next chain. Starting on every ACTIVE write ran an
//! empty chain instead, cleared ACTIVE, and start4's first transfer never ran.
//!
//! The actual `SRC`/`DEST` transfer needs bus access, so [`Machine`] pulls the
//! pending descriptor out of here after the register write and runs it.
//!
//! [`Machine`]: crate::machine::Machine

use crate::bus::{BusResult, MmioDevice, Width};

pub use crate::spec::dma4::{
    CB, CS, CS_ACTIVE_MASK as CS_ACTIVE, CS_BUSY_MASK as CS_BUSY_EXTRA, CS_END_MASK as CS_END,
    CS_ERROR_MASK as CS_ERROR, CS_INT_MASK as CS_INT, DEBUG,
};
use crate::spec::Coverage;

/// Every register in `specs/dma4.toml` is modelled.
pub const COVERAGE: Coverage = Coverage {
    block: "dma4",
    decoded: &[CS, CB, DEBUG],
};

/// `TI` bit 0 of a control block: raise the completion interrupt when this
/// control block is done.
pub const TI_INTEN: u32 = 1 << 0;

#[derive(Default)]
pub struct Dma4 {
    cs: u32,
    cb: u32,
    debug: u32,
    /// Set when a register write starts a chain;
    /// [`Machine`](crate::machine::Machine) clears it by calling
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

    /// If a start was just requested, consume it.
    /// [`Machine`](crate::machine::Machine) then walks the CB chain and calls
    /// [`Dma4::finish`].
    pub fn take_start(&mut self) -> bool {
        std::mem::take(&mut self.start_pending)
    }

    /// Mark the transfer complete: END set, INT set if a control block had
    /// INTEN, ACTIVE / ERROR / busy-extra clear, the chain pointer null, the
    /// error latch clear.
    pub fn finish(&mut self, interrupt: bool) {
        self.cs = (self.cs & !(CS_ACTIVE | CS_ERROR | CS_BUSY_EXTRA)) | CS_END;
        if interrupt {
            self.cs |= CS_INT;
        }
        self.cb = 0;
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
                if value & CS_ACTIVE != 0 && self.cb != 0 {
                    self.start_pending = true;
                }
            }
            CB => {
                self.cb = value;
                if self.cs & CS_ACTIVE != 0 && value != 0 {
                    self.start_pending = true;
                }
            }
            DEBUG => self.debug &= !value, // write-1-to-clear
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wr(d: &mut Dma4, off: u32, v: u32) {
        d.write(off, Width::Word, v).unwrap();
    }

    #[test]
    fn the_bootloader_loads_cb_then_sets_active() {
        let mut d = Dma4::new();
        wr(&mut d, DEBUG, 0x400);
        wr(&mut d, CB, 0x1234);
        assert!(!d.take_start(), "not active yet");
        wr(&mut d, CS, CS_ACTIVE);
        assert!(d.take_start());
        assert_eq!(d.cb_addr(), 0x1234 << 5);
        d.finish(false);
        let cs = d.read(CS, Width::Word).unwrap();
        assert_eq!(
            cs & (CS_ACTIVE | CS_END | CS_INT),
            CS_END,
            "polled, no interrupt"
        );
        assert_eq!(d.read(CB, Width::Word).unwrap(), 0);
    }

    #[test]
    fn dmalib_sets_active_first_and_the_cb_write_starts_the_chain() {
        let mut d = Dma4::new();
        // dma_set_cs: ACTIVE with nothing loaded idles.
        wr(&mut d, CS, 0x2000_0001);
        assert!(!d.take_start());
        // dma_chain_start: the CB write runs it.
        wr(&mut d, CB, 0x25f7_36a5);
        assert!(d.take_start());
        d.finish(true);
        let cs = d.read(CS, Width::Word).unwrap();
        assert_eq!(cs & (CS_ACTIVE | CS_END | CS_INT), CS_END | CS_INT);
        // dma_chan_interrupt: `CS | 7` re-arms ACTIVE; no chain, no start.
        wr(&mut d, CS, cs | 7);
        assert!(!d.take_start());
        wr(&mut d, CB, 0x25f7_36a6);
        assert!(d.take_start(), "the next chain");
    }
}
