//! Multicore-sync block at `0x7E00_0000`: thirty-two hardware semaphores the
//! two VPU cores claim, plus the mailbox words and ack registers around them.
//!
//! Register map: `specs/mcsync.toml` ([`crate::spec::mcsync`]). The block names
//! itself `MULT` in every reserved word of its window, the way the GPIO block
//! names itself `gpio`.
//!
//! [`SEMA`]`[slot]` is test-and-set, measured on a Raspberry Pi 4B d03115: a
//! read answers 0 if the semaphore was free **and takes it**, or 1 if it was
//! already held, and any write releases it whatever value is written. So
//! start4's `0x3ED3A00C`, which spins **while** the word is non-zero, is an
//! acquire loop, and its `0x3ED3A114`, which writes 1, is a release.
//!
//! That corrects what this file used to say. The old note claimed storing the
//! slots "fails -- with nothing on the far side to clear a slot, `0x3ED3A00C`
//! spins forever", and concluded that modelling the doorbell honestly needed
//! core 1 to service it. What actually fails is storing the *written* value and
//! handing it back: hardware never does that. Nothing on the far side is
//! required, because the acquire loop ends as soon as the semaphore is free.
//!
//! [`STATUS`] is the hardware's bitmap of which semaphores are held, not a
//! separate pending set. The firmware treats a held semaphore as a message
//! waiting, which is why its ISR at `0x3ED3A098` clears exactly these bits from
//! its ack word; neither core enables source 76 or 77 on the pinned boot
//! (`--log irqen`), so the doorbell interrupt is not how start4 is woken.
//!
//! Everything else in the window reads 0.

use crate::bus::{BusResult, MmioDevice, Width};
use crate::spec::mcsync::{SEMA, SEMA_COUNT, SEMA_STRIDE, STATUS};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "mcsync",
    decoded: &[SEMA, STATUS],
};

#[derive(Default)]
pub struct McSync {
    /// Which semaphores are held, one bit per slot: what [`STATUS`] answers.
    held: u32,
    /// How many semaphores were taken over the run. Nothing reads it yet; it
    /// is the one number worth having if the block ever needs a report.
    pub posts: u64,
}

impl McSync {
    pub fn new() -> McSync {
        McSync::default()
    }

    /// The slot `offset` addresses, if it is one of the semaphores.
    fn slot(offset: u32) -> Option<u32> {
        let rel = offset.checked_sub(SEMA)?;
        (rel % SEMA_STRIDE == 0 && rel / SEMA_STRIDE < SEMA_COUNT).then_some(rel / SEMA_STRIDE)
    }
}

impl MmioDevice for McSync {
    fn name(&self) -> &'static str {
        "mc-sync"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        if let Some(slot) = McSync::slot(offset) {
            let mask = 1 << slot;
            if self.held & mask != 0 {
                return Ok(1);
            }
            self.held |= mask;
            self.posts += 1;
            return Ok(0);
        }
        if offset == STATUS {
            return Ok(self.held);
        }
        Ok(0)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let _ = value;
        if let Some(slot) = McSync::slot(offset) {
            self.held &= !(1 << slot);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Raspberry Pi 4B d03115: with every slot released, six reads of slot 5
    /// answer `0 1 1 1 1 1`.
    #[test]
    fn a_read_takes_a_free_semaphore_and_reports_it_held_after() {
        let mut m = McSync::new();
        let slot5 = SEMA + 5 * SEMA_STRIDE;
        assert_eq!(m.read(slot5, Width::Word).unwrap(), 0);
        for _ in 0..5 {
            assert_eq!(m.read(slot5, Width::Word).unwrap(), 1);
        }
        assert_eq!(m.read(STATUS, Width::Word).unwrap(), 1 << 5);
    }

    /// A write releases whatever value it carries: the board answers 0 to the
    /// next read after a write of 0 and after a write of 1 alike.
    #[test]
    fn any_write_releases() {
        let mut m = McSync::new();
        for value in [0, 1, 0xFFFF_FFFF] {
            assert_eq!(m.read(SEMA, Width::Word).unwrap(), 0, "taken");
            m.write(SEMA, Width::Word, value).unwrap();
            assert_eq!(m.read(SEMA, Width::Word).unwrap(), 0, "{value:#x} released");
            m.write(SEMA, Width::Word, 0).unwrap();
        }
    }

    /// Taking slots 0, 7 and 31 leaves `STATUS` at exactly `0x80000081`, and
    /// releasing slot 7 at `0x80000001`.
    #[test]
    fn status_is_the_bitmap_of_held_semaphores() {
        let mut m = McSync::new();
        for slot in [0, 7, 31] {
            m.read(SEMA + slot * SEMA_STRIDE, Width::Word).unwrap();
        }
        assert_eq!(m.read(STATUS, Width::Word).unwrap(), 0x8000_0081);
        m.write(SEMA + 7 * SEMA_STRIDE, Width::Word, 0).unwrap();
        assert_eq!(m.read(STATUS, Width::Word).unwrap(), 0x8000_0001);
    }

    /// The word past the last semaphore is `STATUS`, not a 33rd slot.
    #[test]
    fn the_array_stops_at_32() {
        assert_eq!(SEMA + SEMA_COUNT * SEMA_STRIDE, STATUS);
        assert_eq!(McSync::slot(STATUS), None);
    }
}
