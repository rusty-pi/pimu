//! Boot-info handoff doorbells (`0x7EE0_0000` region).
//!
//! Registers: `specs/bootbox.toml`.
//!
//! The EEPROM bootloader stages a tagged boot-info block into DRAM and then
//! rings a doorbell here: poke a size word at `+0x08`, write a trigger to
//! `+0x00`, poll for it to clear.
//!
//! Whatever consumes these (a VPU sub-core or secure processor) is not
//! modelled. Every doorbell self-clears its low control bits so the
//! handshakes complete; parameter words the firmware writes read straight back.
//!
//! `0x7EE0_1000` looks like the L2 cache's maintenance port rather than a
//! doorbell: a range at `+0x04` / `+0x08`, then a command. The machine acts on
//! its flush (`Machine::store_device`, [`crate::l2`]); here it is storage like
//! the rest.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::bootbox::{
    DOORBELL_B, DOORBELL_B_CONTROL_MASK as CONTROL_BITS, DOORBELL_C, DOORBELL_C_SIZE, IRQ_PAYLOAD,
    IRQ_SOURCE, IRQ_STATUS, L2_CTRL, L2_FLUSH_END, L2_FLUSH_START,
};
use crate::spec::Coverage;

/// Every doorbell reads its control bits back clear — idle, request already
/// serviced — except the interrupt window, which reports verbatim.
pub const COVERAGE: Coverage = Coverage {
    block: "bootbox",
    decoded: &[
        L2_CTRL,
        L2_FLUSH_START,
        L2_FLUSH_END,
        IRQ_STATUS,
        IRQ_SOURCE,
        IRQ_PAYLOAD,
        DOORBELL_B,
        DOORBELL_C,
        DOORBELL_C_SIZE,
    ],
};

#[derive(Default)]
pub struct BootBox {
    storage: BTreeMap<u32, u32>,
}

impl BootBox {
    pub fn new() -> BootBox {
        BootBox::default()
    }

    pub fn word(&self, off: u32) -> u32 {
        self.storage.get(&(off & !3)).copied().unwrap_or(0)
    }
}

impl MmioDevice for BootBox {
    fn name(&self) -> &'static str {
        "bootbox"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        let raw = self.storage.get(&off).copied().unwrap_or(0);
        // The interrupt-status window must report its low bits verbatim; every
        // other doorbell reads its control bits back clear.
        if matches!(off, IRQ_STATUS | IRQ_SOURCE | IRQ_PAYLOAD) {
            Ok(raw)
        } else {
            Ok(raw & !CONTROL_BITS)
        }
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset & !3, value);
        Ok(())
    }
}
