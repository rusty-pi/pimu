//! A check, not a behaviour: the scalar accesses the VPU makes that are not
//! naturally aligned.
//!
//! The model reads and writes memory by offset, so a word load two bytes into
//! a word costs it nothing. The hardware is not so generous — GCC's VC4 port
//! is `STRICT_ALIGNMENT`, and lays a packed 32-bit access out as four byte
//! accesses, because the core cannot do it in one. A single `ld` at an odd
//! address therefore reads *something else* on silicon and nothing at all in
//! the model, which is the kind of difference no console diff can show.
//!
//! Off by default ([`Alignment::off`]); with `--check-alignment` every access
//! goes to the `alignment` log channel and the run ends with a count. The
//! vector forms (`v8ld`/`v8st`) are excluded: those do copy arbitrary byte
//! alignments, and stock's libc uses them for exactly that.

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

    /// One scalar access at `addr`, `width` wide, from `pc`.
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
