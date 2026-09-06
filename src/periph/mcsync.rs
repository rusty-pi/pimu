//! Multicore-sync block at `0x7E00_0000` (doorbells / semaphores between the two
//! VPU cores).
//!
//! `start4.elf`'s core 0 posts work to core 1 by writing a nonzero token to
//! `0x7E00_00C0` and then spins until core 1 writes it back to 0. Until we run
//! core 1 for real, model the register as *auto-acknowledged*: writes are
//! accepted, reads always return 0 ("the other core already drained it"). This
//! is a stub — it lets core 0 make progress but does not actually perform the
//! work core 1 would have done.

use crate::bus::{BusResult, MmioDevice, Width};

#[derive(Default)]
pub struct McSync {
    /// Number of nonzero tokens written (a rough count of core-1 requests).
    pub posts: u64,
}

impl McSync {
    pub fn new() -> McSync {
        McSync::default()
    }
}

impl MmioDevice for McSync {
    fn name(&self) -> &'static str {
        "mc-sync"
    }

    fn read(&mut self, _offset: u32, _width: Width) -> BusResult<u32> {
        Ok(0)
    }

    fn write(&mut self, _offset: u32, _width: Width, value: u32) -> BusResult<()> {
        if value != 0 {
            self.posts += 1;
        }
        Ok(())
    }
}
