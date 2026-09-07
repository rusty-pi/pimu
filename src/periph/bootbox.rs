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

/// Low control bits (ready / busy / trigger) — always read back clear, i.e.
/// "idle, request already serviced".
const CONTROL_BITS: u32 = 0xF;

/// `start4.elf`'s VPU interrupt handler (exception vector 12, `0x3ED1804E`)
/// reads the pending interrupt source from a 3-word window here: `+0x00` bit 0
/// = "a source is pending", `+0x04` / `+0x08` = source id / payload, and it
/// acks by writing 0 back to `+0x00`.
const IRQ_STATUS: u32 = 0x1080;
const IRQ_SOURCE: u32 = 0x1084;
const IRQ_PAYLOAD: u32 = 0x1088;

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
        self.storage.insert(IRQ_STATUS, 1);
        self.storage.insert(IRQ_SOURCE, source);
        self.storage.insert(IRQ_PAYLOAD, payload);
    }

    /// True while a raised source has not yet been acked by the handler.
    pub fn irq_pending(&self) -> bool {
        self.storage.get(&IRQ_STATUS).copied().unwrap_or(0) & 1 != 0
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
