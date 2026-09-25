//! "Always ready" stub for an unmodelled peripheral block whose firmware driver
//! spins on status bits the model does not produce.
//!
//! The `pieeprom.bin` bootloader feeds data through a FIFO at `0x7E20_F000`
//! (polling bit 18 = "TX has space", bit 17 = "RX has data") with a ~10 s
//! timeout. With no model behind it those bits never set and the transfer
//! loop times out. Reporting *both* ready lets the loop drain at once; the
//! data it reads back is meaningless — that would take the real crypto / FIFO
//! engine — but boot gets past the wait.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

/// Status reads: bits 17 and 18 set ("ready" in both directions), plus bit 7,
/// which the bootloader also checks in places.
const READY: u32 = (1 << 17) | (1 << 18) | (1 << 7);

#[derive(Default)]
pub struct ReadyStub {
    name: &'static str,
    /// Sticky, so read-after-write still works for config registers.
    storage: BTreeMap<u32, u32>,
}

impl ReadyStub {
    pub fn new(name: &'static str) -> ReadyStub {
        ReadyStub {
            name,
            storage: BTreeMap::new(),
        }
    }
}

impl MmioDevice for ReadyStub {
    fn name(&self) -> &'static str {
        self.name
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(self.storage.get(&offset).copied().unwrap_or(READY))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset, value);
        Ok(())
    }
}
