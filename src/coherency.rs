//! A check, not a cache: which lines of memory are in one master's hands and out
//! of date in another's, so that a read of one can be reported.
//!
//! The model folds the four VC4 aliases onto one backing store, which hides the
//! one thing silicon does not forgive: a line written through a cached alias
//! sits in a cache until something writes it back, so a master reading the same
//! bytes uncached sees the *old* memory — and the other way round for a line a
//! DMA engine writes behind the caches. Firmware that runs cached has to flush
//! and invalidate, and a folded model cannot tell it from firmware that does
//! not, so `dirty` and `stale` lines are tracked alongside.
//!
//! Which masters sit behind the VPU's caches is not a guess: the VC4-side DMA
//! engines read what the VPU wrote and never flushed, while the ARM-side masters
//! (PCIe endpoint, EMMC2, GENET) reach DRAM on their own and stock reads their
//! buffers back uncached. Granularity is the bootbox flush's 32-byte line. Off
//! by default; `--check-coherency` reports on the `coherency` log channel.

use std::cell::RefCell;

use crate::log::{Channel, Log};

/// The cache line the L2's maintenance port works in (`specs/bootbox.toml`).
const LINE: u32 = 32;
const LINE_SHIFT: u32 = 5;
const SPACE: u32 = 1 << 30;
const LINES: usize = (SPACE >> LINE_SHIFT) as usize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Master {
    Vpu,
    VpuUncached,
    Arm,
    /// An engine that reaches memory directly: the ARM-side masters, and a
    /// VC4-side engine whose bus address is in the uncached alias.
    Dma(&'static str),
    /// A VC4-side engine going through the L2: the legacy DMA at a cached
    /// alias, and the 40-bit channel, whose control block is never flushed.
    Vc4Dma(&'static str),
}

impl Master {
    fn name(self) -> &'static str {
        match self {
            Master::Vpu => "the VPU",
            Master::VpuUncached => "the VPU through the uncached alias",
            Master::Arm => "an ARM core",
            Master::Dma(which) | Master::Vc4Dma(which) => which,
        }
    }

    /// Whether it goes through the VPU's caches, so nothing it touches can be
    /// out of date either way.
    pub fn is_coherent(self) -> bool {
        matches!(self, Master::Vpu | Master::Vc4Dma(_))
    }
}

/// Behind a cell because a read records as much as a write, on a `&self` path.
struct State {
    dirty: Vec<u64>,
    stale: Vec<u64>,
    reported: Vec<u64>,
    reports: usize,
    /// Lines written through a cached alias, so a run that reports nothing is
    /// distinguishable from one that watched nothing.
    marks: usize,
    dma_marks: usize,
}

pub struct Coherency {
    state: RefCell<State>,
    /// Who is reading and who is writing RAM. Two, because one transfer can
    /// have each end on a different side of the caches.
    reader: Master,
    writer: Master,
    on: bool,
    log: Log,
}

impl Coherency {
    pub fn off() -> Coherency {
        Coherency {
            state: RefCell::new(State {
                dirty: Vec::new(),
                stale: Vec::new(),
                reported: Vec::new(),
                reports: 0,
                marks: 0,
                dma_marks: 0,
            }),
            reader: Master::Vpu,
            writer: Master::Vpu,
            on: false,
            log: Log::default(),
        }
    }

    pub fn on(log: Log) -> Coherency {
        Coherency {
            state: RefCell::new(State {
                dirty: vec![0; LINES / 64],
                stale: vec![0; LINES / 64],
                reported: vec![0; LINES / 64],
                reports: 0,
                marks: 0,
                dma_marks: 0,
            }),
            reader: Master::Vpu,
            writer: Master::Vpu,
            on: true,
            log,
        }
    }

    #[inline]
    pub fn is_on(&self) -> bool {
        self.on
    }

    pub fn reports(&self) -> usize {
        self.state.borrow().reports
    }

    pub fn marks(&self) -> usize {
        self.state.borrow().marks
    }

    pub fn dma_marks(&self) -> usize {
        self.state.borrow().dma_marks
    }

    /// Who uses RAM until the next call; the machine sets it around the
    /// peripherals it hands `&mut Ram` to.
    #[inline]
    pub fn set_master(&mut self, master: Master) {
        self.reader = master;
        self.writer = master;
    }

    #[inline]
    pub fn set_masters(&mut self, reader: Master, writer: Master) {
        self.reader = reader;
        self.writer = writer;
    }

    #[inline]
    pub fn reader(&self) -> Master {
        self.reader
    }

    #[inline]
    pub fn writer(&self) -> Master {
        self.writer
    }

    #[inline]
    pub fn masters(&self) -> (Master, Master) {
        (self.reader, self.writer)
    }

    #[inline]
    pub fn wrote_cached(&self, phys: u32, len: u32) {
        if !self.on {
            return;
        }
        let s = &mut *self.state.borrow_mut();
        for i in Coherency::lines(phys, len) {
            let bit = 1 << (i % 64);
            if s.dirty[i / 64] & bit == 0 {
                s.marks += 1;
            }
            s.dirty[i / 64] |= bit;
            s.reported[i / 64] &= !bit;
        }
    }

    /// A VC4-side engine wrote through the caches, where a cached VPU read
    /// lands too: nothing is out of date afterwards.
    #[inline]
    pub fn wrote_through_l2(&self, phys: u32, len: u32) {
        if !self.on {
            return;
        }
        let s = &mut *self.state.borrow_mut();
        for i in Coherency::lines(phys, len) {
            s.stale[i / 64] &= !(1 << (i % 64));
        }
    }

    /// Something other than the VPU wrote there: the VPU's cached copy of
    /// those lines is now out of date.
    #[inline]
    pub fn wrote_by_other(&self, phys: u32, len: u32) {
        if !self.on {
            return;
        }
        let s = &mut *self.state.borrow_mut();
        for i in Coherency::lines(phys, len) {
            let bit = 1 << (i % 64);
            if s.stale[i / 64] & bit == 0 {
                s.dma_marks += 1;
            }
            s.stale[i / 64] |= bit;
            s.reported[i / 64] &= !bit;
        }
    }

    /// The VPU read through a cached alias; a line something else has written
    /// since the last invalidate is reported, since on silicon that read comes
    /// out of the cache.
    #[inline]
    pub fn read_cached(&self, phys: u32, len: u32, pc: u32) {
        if !self.on {
            return;
        }
        let mut hit = None;
        {
            let s = &mut *self.state.borrow_mut();
            for i in Coherency::lines(phys, len) {
                let bit = 1 << (i % 64);
                if s.stale[i / 64] & bit != 0 && s.reported[i / 64] & bit == 0 {
                    s.reported[i / 64] |= bit;
                    s.reports += 1;
                    hit = Some(i);
                    break;
                }
            }
        }
        if let Some(i) = hit {
            crate::log!(
                self.log,
                Channel::Coherency,
                "{:#010x}: read through a cached alias at pc {:#010x} after a DMA \
                 engine wrote it, with no invalidate in between",
                (i as u32) << LINE_SHIFT,
                pc
            );
        }
    }

    pub fn flushed(&self, first: u32, last: u32) {
        if !self.on {
            return;
        }
        let (first, last) = (first & !(LINE - 1), last.min(SPACE - 1));
        let len = last.saturating_sub(first).saturating_add(1);
        let s = &mut *self.state.borrow_mut();
        for i in Coherency::lines(first, len) {
            let bit = 1 << (i % 64);
            s.dirty[i / 64] &= !bit;
            s.stale[i / 64] &= !bit;
        }
    }

    /// `who` read at `phys`; a line the VPU left in cache is reported once.
    #[inline]
    pub fn read_by(&self, phys: u32, len: u32, who: Master) {
        if !self.on {
            return;
        }
        let mut hit = None;
        {
            let s = &mut *self.state.borrow_mut();
            for i in Coherency::lines(phys, len) {
                let bit = 1 << (i % 64);
                if s.dirty[i / 64] & bit != 0 && s.reported[i / 64] & bit == 0 {
                    s.reported[i / 64] |= bit;
                    s.reports += 1;
                    hit = Some(i);
                    break;
                }
            }
        }
        if let Some(i) = hit {
            crate::log!(
                self.log,
                Channel::Coherency,
                "{:#010x}: read by {} while the VPU still holds it in cache \
                 (written through a cached alias, not flushed)",
                (i as u32) << LINE_SHIFT,
                who.name()
            );
        }
    }

    #[inline]
    fn lines(phys: u32, len: u32) -> impl Iterator<Item = usize> {
        let empty = phys >= SPACE || len == 0;
        let first = phys >> LINE_SHIFT;
        let last = phys.saturating_add(len.max(1) - 1).min(SPACE - 1) >> LINE_SHIFT;
        (if empty { 1 } else { first }..=if empty { 0 } else { last }).map(|l| l as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> Coherency {
        Coherency::on(Log::default())
    }

    #[test]
    fn a_cached_write_is_seen_by_a_later_uncached_read() {
        let c = tracker();
        c.wrote_cached(0x1000, 4);
        c.read_by(0x1000, 4, Master::VpuUncached);
        assert_eq!(c.reports(), 1);
        c.read_by(0x1000, 4, Master::VpuUncached);
        assert_eq!(c.reports(), 1);
    }

    #[test]
    fn a_flush_makes_the_line_clean_again() {
        let c = tracker();
        c.wrote_cached(0x2000, 64);
        c.flushed(0x2000, 0x203f);
        c.read_by(0x2000, 64, Master::Arm);
        assert_eq!(c.reports(), 0);
    }

    #[test]
    fn only_the_lines_written_are_dirty() {
        let c = tracker();
        c.wrote_cached(0x3000, 4);
        c.read_by(0x3020, 4, Master::Dma("emmc2"));
        assert_eq!(c.reports(), 0);
        c.read_by(0x3010, 4, Master::Dma("emmc2"));
        assert_eq!(c.reports(), 1, "the rest of the line is dirty too");
    }

    #[test]
    fn a_dma_write_is_seen_by_a_later_cached_read() {
        let mut c = tracker();
        c.set_master(Master::Dma("the EMMC2 DMA"));
        c.wrote_by_other(0x5000, 4);
        c.set_master(Master::Vpu);
        assert_eq!(c.dma_marks(), 1);
        c.read_cached(0x5000, 4, 0x8000_0000);
        assert_eq!(c.reports(), 1);
        c.read_cached(0x5000, 4, 0x8000_0000);
        assert_eq!(c.reports(), 1);
    }

    #[test]
    fn an_invalidate_makes_a_dma_written_line_readable() {
        let c = tracker();
        c.wrote_by_other(0x6000, 64);
        c.flushed(0x6000, 0x603f);
        c.read_cached(0x6000, 64, 0x8000_0000);
        assert_eq!(c.reports(), 0);
    }

    #[test]
    fn an_access_past_the_vpus_gigabyte_is_ignored() {
        let c = tracker();
        c.wrote_by_other(SPACE - 16, 64);
        assert_eq!(c.dma_marks(), 1, "only the last line, not past the end");
        c.wrote_cached(SPACE, 4);
        c.read_cached(SPACE, 4, 0);
        assert_eq!((c.marks(), c.reports()), (0, 0));
    }

    #[test]
    fn a_vc4_side_engine_is_coherent_and_an_arm_side_one_is_not() {
        assert!(Master::Vc4Dma("the 40-bit DMA").is_coherent());
        assert!(!Master::Dma("the EMMC2 DMA").is_coherent());
        assert!(!Master::Arm.is_coherent());
    }

    #[test]
    fn a_write_through_the_l2_settles_a_stale_line() {
        let c = tracker();
        c.wrote_by_other(0x7200, 32);
        c.wrote_through_l2(0x7200, 32);
        c.read_cached(0x7200, 32, 0);
        assert_eq!(c.reports(), 0);
    }

    #[test]
    fn writing_again_after_a_report_makes_it_reportable_again() {
        let c = tracker();
        c.wrote_cached(0x4000, 4);
        c.read_by(0x4000, 4, Master::Arm);
        c.wrote_cached(0x4000, 4);
        c.read_by(0x4000, 4, Master::Arm);
        assert_eq!(c.reports(), 2);
    }
}
