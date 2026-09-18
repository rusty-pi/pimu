//! A check, not a cache: which lines of memory the VPU has written through a
//! *cached* alias and not yet flushed, so that a read of one by anybody else
//! can be reported.
//!
//! The model folds the four VC4 aliases onto one backing store, which is right
//! for what the firmware computes but hides the one thing real silicon does
//! not forgive: a line written through `0x0`, `0x4000_0000` or `0x8000_0000`
//! sits in a cache until something writes it back, and a DMA engine, the ARM,
//! or the VPU itself reading the same physical bytes through `0xC000_0000`
//! sees the *old* memory. Firmware that runs cached has to flush; firmware
//! that runs uncached never has to. Nothing in a folded model can tell the two
//! apart, so this tracks it alongside.
//!
//! Granularity is the 32-byte line the bootbox's flush works in, over the
//! gigabyte the VPU can address. Off by default ([`Coherency::off`]); with
//! `--check-coherency` every report goes to the `coherency` log channel and
//! the run ends with a count.

use crate::log::{Channel, Log};

/// The cache line the L2's maintenance port works in (`specs/bootbox.toml`:
/// the bootcode's flush ends on `0x0FFF_FFE0`).
const LINE: u32 = 32;
const LINE_SHIFT: u32 = 5;
/// What a 32-bit VPU can address.
const SPACE: u32 = 1 << 30;
const LINES: usize = (SPACE >> LINE_SHIFT) as usize;

/// Who read a line that the VPU had left dirty.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Master {
    /// The VPU itself, through the uncached alias.
    VpuUncached,
    /// An ARM core.
    Arm,
    /// One of the DMA engines, by the name the model gives it.
    Dma(&'static str),
}

impl Master {
    fn name(self) -> &'static str {
        match self {
            Master::VpuUncached => "the VPU through the uncached alias",
            Master::Arm => "an ARM core",
            Master::Dma(which) => which,
        }
    }
}

pub struct Coherency {
    /// One bit per line: the VPU wrote it through a cached alias and nothing
    /// has written it back since.
    dirty: Vec<u64>,
    on: bool,
    /// Lines reported already, so a poll of the same address says it once.
    reported: Vec<u64>,
    reports: usize,
    /// How many lines have been written through a cached alias, so a run that
    /// reports nothing can be told from one that watched nothing.
    marks: usize,
    log: Log,
}

impl Coherency {
    /// A tracker that costs one predictable branch per access.
    pub fn off() -> Coherency {
        Coherency {
            dirty: Vec::new(),
            on: false,
            reported: Vec::new(),
            reports: 0,
            marks: 0,
            log: Log::default(),
        }
    }

    pub fn on(log: Log) -> Coherency {
        Coherency {
            dirty: vec![0; LINES / 64],
            on: true,
            reported: vec![0; LINES / 64],
            reports: 0,
            marks: 0,
            log,
        }
    }

    #[inline]
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// How many reads of unflushed lines have been reported.
    pub fn reports(&self) -> usize {
        self.reports
    }

    /// How many lines the VPU has written through a cached alias.
    pub fn marks(&self) -> usize {
        self.marks
    }

    /// The VPU wrote `len` bytes at physical `phys` through a cached alias.
    #[inline]
    pub fn wrote_cached(&mut self, phys: u32, len: u32) {
        if !self.on {
            return;
        }
        let mut fresh = 0;
        self.each_line(phys, len, |dirty, reported, i| {
            let bit = 1 << (i % 64);
            if dirty[i / 64] & bit == 0 {
                fresh += 1;
            }
            dirty[i / 64] |= bit;
            reported[i / 64] &= !bit;
        });
        self.marks += fresh;
    }

    /// A flush covering `first..=last` wrote those lines back.
    pub fn flushed(&mut self, first: u32, last: u32) {
        if !self.on {
            return;
        }
        let (first, last) = (first & !(LINE - 1), last.min(SPACE - 1));
        let len = last.saturating_sub(first).saturating_add(1);
        self.each_line(first, len, |dirty, _reported, i| {
            dirty[i / 64] &= !(1 << (i % 64));
        });
    }

    /// `who` read `len` bytes at physical `phys`. A line the VPU has left in
    /// its cache is reported once.
    #[inline]
    pub fn read_by(&mut self, phys: u32, len: u32, who: Master) {
        if !self.on {
            return;
        }
        let mut hit = None;
        self.each_line(phys, len, |dirty, reported, i| {
            let bit = 1 << (i % 64);
            if dirty[i / 64] & bit != 0 && reported[i / 64] & bit == 0 {
                reported[i / 64] |= bit;
                if hit.is_none() {
                    hit = Some(i);
                }
            }
        });
        if let Some(i) = hit {
            self.reports += 1;
            let at = (i as u32) << LINE_SHIFT;
            crate::log!(
                self.log,
                Channel::Coherency,
                "{:#010x}: read by {} while the VPU still holds it in cache \
                 (written through a cached alias, not flushed)",
                at,
                who.name()
            );
        }
    }

    #[inline]
    fn each_line(&mut self, phys: u32, len: u32, mut f: impl FnMut(&mut [u64], &mut [u64], usize)) {
        if phys >= SPACE || len == 0 {
            return;
        }
        let last = phys.saturating_add(len - 1).min(SPACE - 1);
        for line in (phys >> LINE_SHIFT)..=(last >> LINE_SHIFT) {
            f(&mut self.dirty, &mut self.reported, line as usize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> Coherency {
        let mut c = Coherency::on(Log::default());
        c.log = Log::default();
        c
    }

    #[test]
    fn a_cached_write_is_seen_by_a_later_uncached_read() {
        let mut c = tracker();
        c.wrote_cached(0x1000, 4);
        c.read_by(0x1000, 4, Master::VpuUncached);
        assert_eq!(c.reports(), 1);
        // The same line is not reported twice.
        c.read_by(0x1000, 4, Master::VpuUncached);
        assert_eq!(c.reports(), 1);
    }

    #[test]
    fn a_flush_makes_the_line_clean_again() {
        let mut c = tracker();
        c.wrote_cached(0x2000, 64);
        c.flushed(0x2000, 0x203f);
        c.read_by(0x2000, 64, Master::Arm);
        assert_eq!(c.reports(), 0);
    }

    #[test]
    fn only_the_lines_written_are_dirty() {
        let mut c = tracker();
        c.wrote_cached(0x3000, 4);
        c.read_by(0x3020, 4, Master::Dma("emmc2"));
        assert_eq!(c.reports(), 0);
        c.read_by(0x3010, 4, Master::Dma("emmc2"));
        assert_eq!(c.reports(), 1, "the rest of the line is dirty too");
    }

    #[test]
    fn writing_again_after_a_report_makes_it_reportable_again() {
        let mut c = tracker();
        c.wrote_cached(0x4000, 4);
        c.read_by(0x4000, 4, Master::Arm);
        c.wrote_cached(0x4000, 4);
        c.read_by(0x4000, 4, Master::Arm);
        assert_eq!(c.reports(), 2);
    }
}
