//! Boot-info handoff doorbells (`0x7EE0_0000` region).
//!
//! After DRAM is up and the OTP / key state has been read, the EEPROM
//! bootloader stages a tagged boot-info block (`BSTE` / `BVER` / … carrying the
//! firmware commit hash and build date) into DRAM at `0xC004_0000`, clears a
//! response area at `0xC002_0000`, then rings a doorbell here: `0x80009594`
//! sets bit 1 of `0x7EE0_2000` and spins until it reads back clear. A short
//! stub the bootloader relocates to DRAM (`0x6001_0000`) does the same against
//! `0x7EE0_2100` and `0x7EE0_1000` — poke a "size" word (`+0x08`), write a
//! trigger (bits 1..2) to `+0x00`, poll for it to clear.
//!
//! We do not model whatever consumes these (a VPU sub-core / secure
//! processor). Every doorbell self-clears its low control bits so the
//! handshakes complete; parameter words the firmware writes read straight back.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

// Every doorbell has its ready / busy / trigger bits in the same place.
use crate::spec::bootbox::{
    DOORBELL_A, DOORBELL_A_SIZE, DOORBELL_B, DOORBELL_B_CONTROL_MASK as CONTROL_BITS, DOORBELL_C,
    DOORBELL_C_SIZE, IRQ_PAYLOAD, IRQ_SOURCE, IRQ_STATUS, IRQ_STATUS_PENDING_MASK,
};
use crate::spec::Coverage;

/// Every doorbell reads its control bits back clear ("idle, request already
/// serviced"); the interrupt window `start4.elf`'s exception-12 handler
/// (`0x3ED1804E`) reads reports verbatim.
pub const COVERAGE: Coverage = Coverage {
    block: "bootbox",
    decoded: &[
        DOORBELL_A,
        DOORBELL_A_SIZE,
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

    /// Latch a pending VPU interrupt source for the exc-12 handler to pick up.
    pub fn raise_irq(&mut self, source: u32, payload: u32) {
        self.storage.insert(IRQ_STATUS, IRQ_STATUS_PENDING_MASK);
        self.storage.insert(IRQ_SOURCE, source);
        self.storage.insert(IRQ_PAYLOAD, payload);
    }

    /// True while a raised source has not yet been acked by the handler.
    pub fn irq_pending(&self) -> bool {
        self.storage.get(&IRQ_STATUS).copied().unwrap_or(0) & IRQ_STATUS_PENDING_MASK != 0
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
