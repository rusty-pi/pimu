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

#[derive(Default)]
pub struct BootBox {
    storage: BTreeMap<u32, u32>,
}

impl BootBox {
    pub fn new() -> BootBox {
        BootBox::default()
    }
}

impl MmioDevice for BootBox {
    fn name(&self) -> &'static str {
        "bootbox"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(self.storage.get(&(offset & !3)).copied().unwrap_or(0) & !CONTROL_BITS)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset & !3, value);
        Ok(())
    }
}
