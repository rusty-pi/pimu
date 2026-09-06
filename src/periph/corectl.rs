//! VPU core-control block at `0x7E00_2000`.
//!
//! `start4.elf`'s entry trampoline runs on VPU core 0 and brings up core 1 by
//! writing core 1's start vector to offset `0x30` (core 0 writes `0x30`, a core-1
//! copy would write `0x38`). A separate 2-bit-per-lane run-state field lives in
//! the four words at `0x10..0x20`; the firmware pokes `0b11` there to release a
//! core. We don't model the exact bitfield semantics — we treat *any* nonzero
//! start-vector write as "release core 1 at this address" and let the emulator's
//! run loop pick it up.
//!
//! Everything is sticky read/write storage so the firmware's read-modify-write
//! sequences behave; the only special case is latching the start vector.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

/// Offset (within the `0x7E00_2000` block) of core 1's start-vector register.
const CORE1_START_VECTOR: u32 = 0x30;

#[derive(Default)]
pub struct CoreCtl {
    storage: BTreeMap<u32, u32>,
    /// Set when the firmware writes a nonzero start vector for core 1 and not
    /// yet consumed by the run loop.
    pending_core1_start: Option<u32>,
    /// Latches so a rewrite of the same vector doesn't re-spawn the core.
    core1_started: bool,
}

impl CoreCtl {
    pub fn new() -> CoreCtl {
        CoreCtl::default()
    }

    /// Consume a pending "release core 1 at <addr>" request, if any.
    pub fn take_core1_start(&mut self) -> Option<u32> {
        self.pending_core1_start.take()
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
        if offset == CORE1_START_VECTOR && value != 0 && !self.core1_started {
            self.core1_started = true;
            self.pending_core1_start = Some(value);
        }
        Ok(())
    }
}
