//! A check, not a cache: which lines of memory are in one master's hands and
//! out of date in another's, so that a read of one can be reported.
//!
//! The model folds the four VC4 aliases onto one backing store, which is right
//! for what the firmware computes but hides the one thing real silicon does
//! not forgive. A line written through `0x0`, `0x4000_0000` or `0x8000_0000`
//! sits in a cache until something writes it back, and a DMA engine, the ARM,
//! or the VPU itself reading the same physical bytes through `0xC000_0000`
//! sees the *old* memory. The other way round is as bad: a line a DMA engine
//! writes lands in memory behind the caches, and the VPU reading it back
//! through a cached alias sees what it held before. Firmware that runs cached
//! has to flush and invalidate; firmware that runs uncached never has to.
//! Nothing in a folded model can tell the two apart, so this tracks it
//! alongside:
//!
//! - `dirty` — the VPU wrote the line through a cached alias and has not
//!   flushed it, so a master that does not read through those caches gets
//!   stale memory.
//! - `stale` — such a master wrote the line straight to memory, so the VPU
//!   reading it through a cached alias gets what its cache still holds.
//!
//! Which masters those are is not a guess: stock's own boot says so. The
//! VC4-side DMA engines read what the VPU has written and not flushed —
//! stock's `dma_memcpy` copies megabytes it wrote through `0x0` a moment
//! earlier, and the 40-bit channel's control block is written through
//! `0x8000_0000` and never flushed — so they are behind the same caches. The
//! ARM-side masters are not: the PCIe endpoint, EMMC2 and GENET reach DRAM
//! on their own, and stock reads every buffer they fill back through
//! `0xC000_0000`.
//!
//! Granularity is the 32-byte line the bootbox's flush works in, over the
//! gigabyte the VPU can address. Off by default ([`Coherency::off`]); with
//! `--check-coherency` every report goes to the `coherency` log channel and
//! the run ends with a count.

use std::cell::RefCell;

use crate::log::{Channel, Log};

/// The cache line the L2's maintenance port works in (`specs/bootbox.toml`:
/// the bootcode's flush ends on `0x0FFF_FFE0`).
const LINE: u32 = 32;
const LINE_SHIFT: u32 = 5;
/// What a 32-bit VPU can address.
const SPACE: u32 = 1 << 30;
const LINES: usize = (SPACE >> LINE_SHIFT) as usize;

/// Who is reading or writing a line, and how far down it sees.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Master {
    /// The VPU itself, writing or reading through a cached alias.
    Vpu,
    /// The VPU itself, through the uncached alias.
    VpuUncached,
    /// An ARM core.
    Arm,
    /// An engine that reaches memory directly: the ARM-side masters, which
    /// are not behind the VPU's L2 at all (the PCIe endpoint writing DRAM
    /// through the inbound window, EMMC2, GENET), and a VC4-side engine
    /// whose bus address is in the uncached alias.
    Dma(&'static str),
    /// A VC4-side engine going through the L2: the legacy DMA at a cached
    /// alias, and the 40-bit channel, whose control block stock writes
    /// through `0x8000_0000` and never flushes.
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

    /// Whether it reads and writes through the VPU's caches, so that nothing
    /// it touches can be out of date in either direction.
    pub fn is_coherent(self) -> bool {
        matches!(self, Master::Vpu | Master::Vc4Dma(_))
    }
}

/// The bitmaps and counts, behind a cell because a read has to record as much
/// as a write does and the model's read paths take `&self`.
struct State {
    /// One bit per line: the VPU wrote it through a cached alias and nothing
    /// has written it back since.
    dirty: Vec<u64>,
    /// One bit per line: something other than the VPU wrote it, so whatever
    /// the VPU still holds in cache for it is out of date until an
    /// invalidate.
    stale: Vec<u64>,
    /// Lines reported already, so a poll of the same address says it once.
    reported: Vec<u64>,
    reports: usize,
    /// How many lines have been written through a cached alias, so a run that
    /// reports nothing can be told from one that watched nothing.
    marks: usize,
    /// How many lines something other than the VPU has written.
    dma_marks: usize,
}

pub struct Coherency {
    state: RefCell<State>,
    /// Who is reading and who is writing RAM at the moment. Two, because one
    /// transfer can have each end on a different side of the caches: the
    /// legacy DMA takes a bus address per end and its alias says whether that
    /// end goes through the L2.
    reader: Master,
    writer: Master,
    on: bool,
    log: Log,
}

impl Coherency {
    /// A tracker that costs one predictable branch per access.
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

    /// How many reads of a line held elsewhere have been reported.
    pub fn reports(&self) -> usize {
        self.state.borrow().reports
    }

    /// How many lines the VPU has written through a cached alias.
    pub fn marks(&self) -> usize {
        self.state.borrow().marks
    }

    /// How many lines something other than the VPU has written.
    pub fn dma_marks(&self) -> usize {
        self.state.borrow().dma_marks
    }

    /// Who uses RAM until the next call. The machine sets this around the
    /// peripherals it hands `&mut Ram` to, so their accesses can be told from
    /// the VPU's own.
    #[inline]
    pub fn set_master(&mut self, master: Master) {
        self.reader = master;
        self.writer = master;
    }

    /// The same, for an engine whose two ends are on different sides of the
    /// caches.
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

    /// The VPU wrote `len` bytes at physical `phys` through a cached alias.
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

    /// A VC4-side engine wrote `len` bytes at `phys` through the caches,
    /// which is where a cached VPU read lands too: nothing is out of date
    /// afterwards.
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

    /// Something other than the VPU wrote `len` bytes at `phys`: what the VPU
    /// holds in cache for those lines is now out of date.
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

    /// The VPU read `len` bytes at `phys` through a cached alias. A line
    /// something else has written since the last invalidate is reported: on
    /// real silicon that read comes out of the cache and misses what landed
    /// in memory.
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

    /// A flush covering `first..=last` wrote those lines back and dropped
    /// them, so neither side holds anything out of date for them.
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

    /// `who` read `len` bytes at physical `phys`. A line the VPU has left in
    /// its cache is reported once.
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

    /// The lines `len` bytes at `phys` touch, clipped to what the VPU can
    /// address. Empty for an access outside it.
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
        // The same line is not reported twice.
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
        // Once said, not said again for the same line.
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
