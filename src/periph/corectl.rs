//! VPU core-control block at `0x7E00_2000`.
//!
//! `start4.elf`'s entry trampoline runs on both VPU cores; they diverge on
//! `version` bit 16. Core 0 writes its vector base to offset `0x30` (core 1's
//! copy would use `0x38`) early on, then continues the main boot. Much later
//! (~250k instructions in, after the shared globals are set up) it pokes a
//! per-lane run-state field in the words at `0x10..0x20` to actually **release**
//! core 1.
//!
//! We model that: a nonzero write to `0x10`/`0x14` latches "release core 1", and
//! the run loop then starts it at `entry` (the shared ELF entry — it re-runs the
//! trampoline, takes the bit-16 path, and resumes at its own target). Storage is
//! otherwise sticky so the firmware's read-modify-write sequences behave.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

/// Run-state field words. A nonzero write here releases core 1.
const RUNSTATE_LO: u32 = 0x10;
const RUNSTATE_HI: u32 = 0x14;

#[derive(Default)]
pub struct CoreCtl {
    storage: BTreeMap<u32, u32>,
    /// Set once the firmware arms core 1's run-state; cleared by the run loop
    /// when it actually brings the core up.
    pending_core1_release: bool,
    /// Latches so repeated pokes don't re-spawn the core.
    core1_started: bool,
}

impl CoreCtl {
    pub fn new() -> CoreCtl {
        CoreCtl::default()
    }

    /// True once (and only once) after the firmware arms core 1's run-state.
    pub fn take_core1_release(&mut self) -> bool {
        std::mem::take(&mut self.pending_core1_release)
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
        if matches!(offset, RUNSTATE_LO | RUNSTATE_HI) && value != 0 && !self.core1_started {
            self.core1_started = true;
            self.pending_core1_release = true;
        }
        Ok(())
    }
}
