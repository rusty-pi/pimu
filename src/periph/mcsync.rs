//! Multicore-sync block at `0x7E00_0000` (doorbells / semaphores between the two
//! VPU cores).
//!
//! Register map: `specs/mcsync.toml` ([`crate::spec::mcsync`]), decoded from
//! start4 (2026-09-10):
//!
//! * [`DOORBELL`]`[slot]`, `slot < 0x20` — the doorbell slots. `0x3ED3A114(slot)`
//!   posts by writing 1; `0x3ED3A00C(slot)` waits by spinning **while** the
//!   word is non-zero, i.e. the receiving core clears it once the work is done.
//! * `PENDING` — pending mask; `ACK76` / `ACK77` — ack words for interrupt
//!   sources 76 and 77. The ISR at `0x3ED3A098` (handler table `gp+58004`)
//!   only does `[ACK76] &= ~[PENDING]` (`ACK77` for source 77) and returns, so
//!   it is an acknowledge path; the request itself is processed by a thread the
//!   interrupt wakes.
//!
//! Until we run core 1 for real, the register is modelled as
//! *auto-acknowledged*: writes are accepted, reads always return 0 ("the other
//! core already drained it"). This is a stub — it lets core 0 make progress but
//! does not perform the work core 1 would have done.
//!
//! Storing the slots for real and returning them was tried and **fails**: with
//! nothing on the far side to clear a slot, `0x3ED3A00C` spins forever and the
//! boot emits no `MESS:` output at all. Modelling the doorbell honestly
//! requires core 1 actually servicing it. Note also that neither core ever
//! calls `enable_irq_source` for 76 or 77 (`RVF_DBG_IRQEN`), so the doorbell
//! interrupt is *not* how start4 is woken here — do not build on that theory.

use crate::bus::{BusResult, MmioDevice, Width};
use crate::spec::mcsync::{DOORBELL, DOORBELL_COUNT, DOORBELL_STRIDE};
use crate::spec::Coverage;

/// Only the doorbells are modelled; the pending / ack words read back 0 along
/// with everything else.
pub const COVERAGE: Coverage = Coverage {
    block: "mcsync",
    decoded: &[DOORBELL],
};

#[derive(Default)]
pub struct McSync {
    /// Number of nonzero doorbell writes (a rough count of core-1 requests).
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

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let doorbell = (DOORBELL..DOORBELL + DOORBELL_COUNT * DOORBELL_STRIDE).contains(&offset);
        if doorbell && value != 0 {
            self.posts += 1;
        }
        Ok(())
    }
}
