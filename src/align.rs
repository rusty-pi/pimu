//! A check, not a behaviour: scalar VPU accesses that are not naturally
//! aligned. The model serves them from any offset; the core cannot, so on
//! silicon such an access reads something else entirely. Off by default;
//! `--check-alignment` reports each one on the `alignment` log channel. The
//! vector forms (`v8ld`/`v8st`) are excluded — they do copy arbitrary byte
//! alignments.

use std::cell::Cell;

use crate::bus::Width;
use crate::log::{Channel, Log};

pub struct Alignment {
    on: bool,
    reports: Cell<usize>,
    log: Log,
}

impl Alignment {
    pub fn off() -> Alignment {
        Alignment {
            on: false,
            reports: Cell::new(0),
            log: Log::default(),
        }
    }

    pub fn on(log: Log) -> Alignment {
        Alignment {
            on: true,
            reports: Cell::new(0),
            log,
        }
    }

    #[inline]
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// How many misaligned accesses have been reported.
    pub fn reports(&self) -> usize {
        self.reports.get()
    }

    #[inline]
    pub fn note(&self, addr: u32, width: Width, pc: u32, write: bool) {
        if !self.on || addr & (width.bytes() - 1) == 0 {
            return;
        }
        self.reports.set(self.reports.get() + 1);
        crate::log!(
            self.log,
            Channel::Alignment,
            "{:#010x}: {} {} bytes wide at pc {:#010x}, which the core cannot do in one access",
            addr,
            if write { "written" } else { "read" },
            width.bytes(),
            pc
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_misaligned_accesses_are_counted() {
        let a = Alignment::on(Log::default());
        a.note(0x1000, Width::Word, 0, false);
        a.note(0x1002, Width::Half, 0, false);
        a.note(0x1003, Width::Byte, 0, true);
        assert_eq!(a.reports(), 0);
        a.note(0x1001, Width::Word, 0, false);
        a.note(0x1003, Width::Half, 0, true);
        assert_eq!(a.reports(), 2);
    }

    #[test]
    fn a_tracker_that_is_off_counts_nothing() {
        let a = Alignment::off();
        a.note(0x1001, Width::Word, 0, false);
        assert_eq!(a.reports(), 0);
    }
}
