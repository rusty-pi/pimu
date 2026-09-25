//! Catch-all device for the peripheral window.
//!
//! Real firmware touches more blocks than are modelled. Rather than fault, the
//! stub records every access and reads 0 where nothing was written, so an
//! unmodelled poke becomes a triage note instead of a crash. Per-offset sticky
//! storage makes the common write-then-read-back pattern behave.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

#[derive(Debug, Clone)]
pub struct StubAccess {
    pub offset: u32,
    pub width: Width,
    pub value: u32,
    pub write: bool,
}

pub struct StubRegion {
    name: &'static str,
    storage: BTreeMap<u32, u32>,
    pub log: Vec<StubAccess>,
    pub log_limit: usize,
}

impl StubRegion {
    pub fn new(name: &'static str) -> StubRegion {
        StubRegion {
            name,
            storage: BTreeMap::new(),
            log: Vec::new(),
            log_limit: 4096,
        }
    }

    fn record(&mut self, a: StubAccess) {
        if self.log.len() < self.log_limit {
            self.log.push(a);
        }
    }
}

impl MmioDevice for StubRegion {
    fn name(&self) -> &'static str {
        self.name
    }

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32> {
        let value = self.storage.get(&offset).copied().unwrap_or(0);
        self.record(StubAccess {
            offset,
            width,
            value,
            write: false,
        });
        Ok(value)
    }

    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset, value);
        self.record(StubAccess {
            offset,
            width,
            value,
            write: true,
        });
        Ok(())
    }
}
