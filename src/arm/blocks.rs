//! `RVF_ARM_BLOCKS`: the shape of the straight-line runs the ARM cores
//! execute, and how often each one is re-entered (#117).
//!
//! A *run* here is what a block translator would translate as one unit: the
//! instructions from a control-flow transfer's destination up to and including
//! the next transfer. Every instruction in such a run is fetched, decoded and
//! dispatched separately today; translating the run once and executing the
//! result would spend one lookup and one set of the run loop's per-instruction
//! bookkeeping on the whole of it.
//!
//! Two numbers decide whether that can pay, and this counts both:
//!
//! 1. **How long the runs are**, weighted by instructions rather than by runs.
//!    The mean over runs is the wrong average — one 500-instruction run is
//!    worth as much as a hundred five-instruction ones, and the question is
//!    what fraction of *executed instructions* sit in runs long enough for the
//!    translation to be worth its own cost.
//! 2. **How often a run is re-entered.** A run translated once and executed
//!    once has saved nothing and paid for a hash lookup; the instructions that
//!    matter are the ones in runs entered many times.
//!
//! A run is keyed by the physical address and EL of its first instruction,
//! which is what a cache would key on: physical, so that a block survives a
//! context switch and is not confused by two address spaces sharing a virtual
//! address, and per EL because the EL picks the translation regime.
//!
//! Measure with `RVF_NO_PARK=1 RVF_NO_SHA_SKIP=1`. A parked core's skipped
//! passes and a natively hashed SHA-256 block are never stepped, so without
//! those the counts miss exactly the hottest loops.

use std::collections::HashMap;

/// The straight-line runs one core executed.
#[derive(Default)]
pub struct Blocks {
    /// The run under way: where it started, and how long it is so far.
    start: (u64, u32),
    len: u32,
    /// Runs of each length.
    lens: HashMap<u32, u64>,
    /// Runs started at each `(physical PC, EL)`: how many entries, and the
    /// instructions they executed between them.
    starts: HashMap<(u64, u32), (u64, u64)>,
    /// Instructions counted, and runs closed.
    pub insns: u64,
    pub runs: u64,
    /// Steps that did not retire an instruction — an exception or an interrupt
    /// entry — and so cut a run short without adding to it.
    pub cuts: u64,
}

impl Blocks {
    /// One step of a core: `pa` and `el` are where the instruction was
    /// fetched from, `retired` whether it was executed at all, and `straight`
    /// whether the PC went on to the next word rather than being transferred.
    pub fn step(&mut self, pa: u64, el: u32, retired: bool, straight: bool) {
        if !retired {
            // An exception or an interrupt entry: the run ends where it was.
            self.cuts += 1;
            self.close();
            return;
        }
        if self.len == 0 {
            self.start = (pa, el);
        }
        self.len += 1;
        self.insns += 1;
        if !straight {
            self.close();
        }
    }

    fn close(&mut self) {
        if self.len == 0 {
            return;
        }
        *self.lens.entry(self.len).or_default() += 1;
        let s = self.starts.entry(self.start).or_default();
        s.0 += 1;
        s.1 += self.len as u64;
        self.runs += 1;
        self.len = 0;
    }

    /// Fold another core's counts into these, for a whole-machine report.
    pub fn merge(&mut self, other: &Blocks) {
        for (&len, &n) in &other.lens {
            *self.lens.entry(len).or_default() += n;
        }
        for (&key, &(entries, insns)) in &other.starts {
            let s = self.starts.entry(key).or_default();
            s.0 += entries;
            s.1 += insns;
        }
        self.insns += other.insns;
        self.runs += other.runs;
        self.cuts += other.cuts;
    }

    /// Distinct runs entered — one translation each, for a cache that never
    /// evicts and is never invalidated. The optimistic end of the range.
    pub fn distinct(&self) -> u64 {
        self.starts.len() as u64
    }

    /// The share of executed instructions that sit in runs of at least `len`
    /// instructions, as a percentage.
    pub fn insns_in_runs_of(&self, len: u32) -> f64 {
        let n: u64 = self
            .lens
            .iter()
            .filter(|(&l, _)| l >= len)
            .map(|(&l, &n)| l as u64 * n)
            .sum();
        pct(n, self.insns)
    }

    /// The share of executed instructions whose run was entered at least `n`
    /// times over the whole run — how much of the work a translation is
    /// amortised over.
    pub fn insns_in_runs_entered(&self, n: u64) -> f64 {
        let hot: u64 = self
            .starts
            .values()
            .filter(|&&(entries, _)| entries >= n)
            .map(|&(_, insns)| insns)
            .sum();
        pct(hot, self.insns)
    }

    /// The run starts that executed the most instructions first, as
    /// `((physical PC, EL), entries, instructions)`.
    pub fn hottest(&self) -> Vec<((u64, u32), u64, u64)> {
        let mut v: Vec<_> = self
            .starts
            .iter()
            .map(|(&key, &(entries, insns))| (key, entries, insns))
            .collect();
        v.sort_by_key(|&(_, _, insns)| std::cmp::Reverse(insns));
        v
    }

    /// How many runs of each length there were and how many instructions they
    /// covered, the biggest share of instructions first.
    pub fn by_length(&self) -> Vec<(u32, u64, u64)> {
        let mut v: Vec<(u32, u64, u64)> = self
            .lens
            .iter()
            .map(|(&l, &n)| (l, n, l as u64 * n))
            .collect();
        v.sort_by_key(|&(_, _, insns)| std::cmp::Reverse(insns));
        v
    }
}

fn pct(n: u64, of: u64) -> f64 {
    if of == 0 {
        0.0
    } else {
        100.0 * n as f64 / of as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three instructions then a branch, twice round, is one run of 4 entered
    /// twice.
    #[test]
    fn a_loop_is_one_run_entered_once_per_pass() {
        let mut b = Blocks::default();
        for _ in 0..2 {
            for i in 0..4 {
                b.step(0x1000 + i * 4, 1, true, i != 3);
            }
        }
        assert_eq!((b.insns, b.runs, b.distinct()), (8, 2, 1));
        assert_eq!(b.insns_in_runs_of(4), 100.0);
        assert_eq!(b.insns_in_runs_of(5), 0.0);
        assert_eq!(b.insns_in_runs_entered(2), 100.0);
        assert_eq!(b.insns_in_runs_entered(3), 0.0);
    }

    /// An exception ends the run where it stands, and the entry itself is not
    /// an instruction.
    #[test]
    fn an_exception_cuts_the_run_without_extending_it() {
        let mut b = Blocks::default();
        b.step(0x2000, 1, true, true);
        b.step(0x2004, 1, true, true);
        b.step(0x2008, 1, false, false);
        assert_eq!((b.insns, b.runs, b.cuts), (2, 1, 1));
        assert_eq!(b.by_length(), vec![(2, 1, 2)]);
    }

    /// The same address at two exception levels is two runs: the EL picks the
    /// translation regime, so a cache cannot share them.
    #[test]
    fn the_el_is_part_of_the_key() {
        let mut b = Blocks::default();
        b.step(0x3000, 1, true, false);
        b.step(0x3000, 2, true, false);
        assert_eq!((b.runs, b.distinct()), (2, 2));
    }
}
