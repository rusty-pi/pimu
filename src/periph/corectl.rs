//! VPU core-control block at `0x7E00_2000`. This region carries both the
//! per-core boot handshake and the VPU interrupt controller.
//!
//! `start4.elf`'s entry trampoline runs on both VPU cores; they diverge on
//! `version` bit 16. Core 0 writes its vector base to offset `0x30` (core 1's
//! copy would use `0x38`) early on, then continues the main boot.
//!
//! Interrupt controller: `enable_irq_source(src, prio)` (start4 `0x3ED72374`)
//! stores a 4-bit priority/enable field per source into the words at
//! `0x10..0x20` (core 0) / `0x810..0x820` (core 1): `word = (src >> 3) & 3`,
//! `field = (src & 7) * 4`. A nonzero field means "enabled, dispatch through
//! vector-table slot `prio`". start4 enables source 64 (systimer, [`SYS_IRQ_SRC`])
//! at priority 1 and arms a system-timer compare as its ThreadX tick.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

/// Run-state field words. A write of a code address here releases core 1 (small
/// values are the interrupt-controller priority words, not a release vector).
const RUNSTATE_LO: u32 = 0x10;
const RUNSTATE_HI: u32 = 0x14;
/// Core 0 / core 1 exception-vector-base registers.
const VBASE_CORE0: u32 = 0x30;
const VBASE_CORE1: u32 = 0x38;

/// Interrupt-priority words for sources 0..31 (core 0). start4 numbers its
/// sources from 64, folded back into these four words by `(src >> 3) & 3`.
const IRQ_PRIO_BASE: u32 = 0x10;

/// The interrupt source start4 wires to the BCM system timer (compare channel
/// `src - SYS_IRQ_SRC`). Enabled via `enable_irq_source(64, 1)`.
pub const SYS_IRQ_SRC: u32 = 64;

#[derive(Default)]
pub struct CoreCtl {
    storage: BTreeMap<u32, u32>,
    pending_core1_release: bool,
    core1_started: bool,
    /// Last exception-vector base the firmware wrote for core 0 / core 1.
    pub vbase: [u32; 2],
}

impl CoreCtl {
    pub fn new() -> CoreCtl {
        CoreCtl::default()
    }

    pub fn take_core1_release(&mut self) -> bool {
        std::mem::take(&mut self.pending_core1_release)
    }

    /// The 4-bit priority/enable field for interrupt source `src` (as numbered by
    /// start4, i.e. 64.. for the first word). 0 = disabled.
    pub fn irq_priority(&self, src: u32) -> u8 {
        let word = IRQ_PRIO_BASE + ((src >> 3) & 3) * 4;
        let field = (src & 7) * 4;
        ((self.storage.get(&word).copied().unwrap_or(0) >> field) & 0xF) as u8
    }
}

impl MmioDevice for CoreCtl {
    fn name(&self) -> &'static str {
        "core-ctl"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(self.storage.get(&offset).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset, value);
        match offset {
            VBASE_CORE0 => self.vbase[0] = value,
            VBASE_CORE1 => self.vbase[1] = value,
            _ => {}
        }
        // A code-address write to the run-state words releases core 1. The
        // interrupt-controller priority words live at the same offsets but only
        // ever hold small bitfields, so a small value is an IRQ-enable, not a
        // release vector.
        if matches!(offset, RUNSTATE_LO | RUNSTATE_HI) && value >= 0x1000 && !self.core1_started {
            self.core1_started = true;
            self.pending_core1_release = true;
        }
        Ok(())
    }
}
