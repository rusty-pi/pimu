//! Straight runs, decoded once: the instructions from a physical address up
//! to and including the first that always moves the PC anywhere but on by
//! 4, or to the end of its page. A conditional branch is inside a run: where
//! it is taken, the run ends there.
//!
//! A run is keyed by the physical address of its first instruction and holds
//! the write generation of its page from when it was decoded. Any write to
//! the page moves the generation, so a run that is looked up again after one
//! is decoded afresh, and one under way stops after the store that moved it.
//!
//! A branch out of a run into its own page can be linked to the run at its
//! target, which holds as long as the page's generation does.

use super::ArmBus;
use crate::aarch64::predecode::{Decoded, RunBus, RunOp};
use std::marker::PhantomData;

const SLOTS_LOG2: u32 = 17;
/// The most instructions a run holds: a page's.
pub const MAX_LEN: u64 = 1024;
/// How many decoded instructions the runs share before they are all dropped
/// and decoded again as they are reached.
const OPS_MAX: usize = 1 << 20;

#[derive(Clone, Copy)]
struct Slot {
    pa: u64,
    gen: u64,
    genp: *const u64,
    start: u32,
    len: u32,
}

const EMPTY: Slot = Slot {
    pa: u64::MAX,
    gen: 0,
    genp: std::ptr::null(),
    start: 0,
    len: 0,
};

const INDIRECT_LOG2: u32 = 12;

#[derive(Clone, Copy)]
struct Target {
    pa: u64,
    gen: u64,
    run: *const RunOp,
    len: usize,
}

const NO_TARGET: Target = Target {
    pa: u64::MAX,
    gen: 0,
    run: std::ptr::null(),
    len: 0,
};

/// The runs looked up lately, by physical address: few enough to stay in
/// the host's caches, where [`Runs`]' own slots are not.
pub struct Targets {
    slots: Box<[Target]>,
}

impl Targets {
    #[inline(always)]
    fn slot(pa: u64) -> usize {
        ((pa >> 2) ^ (pa >> 14)) as usize & ((1 << INDIRECT_LOG2) - 1)
    }

    /// The run at `pa`, if its page is still at generation `gen`.
    #[inline(always)]
    pub fn get(&self, pa: u64, gen: u64) -> Option<*const RunOp> {
        let t = self.slots[Self::slot(pa)];
        (t.pa == pa && t.gen == gen).then_some(t.run)
    }

    fn clear(&mut self) {
        self.slots.fill(NO_TARGET);
    }
}

/// The runs of cores on bus `M`, whose handlers they hold.
pub struct Runs<M = ArmBus<'static>> {
    slots: Box<[Slot]>,
    /// Never grown past its capacity, so a run's instructions stay where
    /// they are until [`Self::epoch`] moves.
    ops: Vec<RunOp>,
    /// Counts the times every run was dropped at once.
    pub epoch: u64,
    /// The runs looked up lately, which go when the runs do.
    pub targets: Targets,
    bus: PhantomData<fn(M)>,
}

// The generation pointers point into the board's RAM, which outlives every
// core that runs out of it.
unsafe impl<M> Send for Runs<M> {}

impl<M> Default for Runs<M> {
    fn default() -> Self {
        Runs {
            slots: vec![EMPTY; 1 << SLOTS_LOG2].into_boxed_slice(),
            ops: Vec::with_capacity(OPS_MAX),
            epoch: 0,
            targets: Targets {
                slots: vec![NO_TARGET; 1 << INDIRECT_LOG2].into_boxed_slice(),
            },
            bus: PhantomData,
        }
    }
}

impl<M: RunBus> Runs<M> {
    /// The run that starts at `pa`. `code` is its page's words and `genp` its
    /// page's write generation, both from [`crate::mem::Ram`] and valid while
    /// the cores run.
    #[inline(always)]
    pub fn get(
        &mut self,
        pa: u64,
        code: *const u32,
        genp: *const u64,
        decoded: &mut Decoded,
    ) -> &[RunOp] {
        let gen = unsafe { *genp };
        let t = self.targets.slots[Targets::slot(pa)];
        if t.pa == pa && t.gen == gen {
            // In `ops`, which stays put until the epoch moves and drops the
            // targets with it.
            return unsafe { std::slice::from_raw_parts(t.run, t.len) };
        }
        let i = (((pa >> 1) ^ (pa >> 18)) as usize) & ((1 << SLOTS_LOG2) - 2);
        for i in [i, i ^ 1] {
            let s = self.slots[i];
            if s.pa == pa && s.genp == genp && gen == s.gen {
                let run = &self.ops[s.start as usize..(s.start + s.len) as usize];
                self.targets.slots[Targets::slot(pa)] = Target {
                    pa,
                    gen,
                    run: run.as_ptr(),
                    len: run.len(),
                };
                return run;
            }
        }
        self.decode(i, pa, code, genp, decoded)
    }

    /// The first instruction of the run at `pa`, if it is decoded from its
    /// page at `gen`, as the page is now.
    #[cfg(pimu_wasm_jit)]
    pub fn first(&self, pa: u64, genp: *const u64, gen: u64) -> Option<*const RunOp> {
        let now = unsafe { *genp };
        let i = (((pa >> 1) ^ (pa >> 18)) as usize) & ((1 << SLOTS_LOG2) - 2);
        [i, i ^ 1]
            .into_iter()
            .map(|i| self.slots[i])
            .find(|s| s.pa == pa && s.genp == genp && s.gen == gen && now == gen)
            .map(|s| &self.ops[s.start as usize] as *const RunOp)
    }

    #[inline(never)]
    fn decode(
        &mut self,
        i: usize,
        pa: u64,
        code: *const u32,
        genp: *const u64,
        decoded: &mut Decoded,
    ) -> &[RunOp] {
        if self.ops.len() + MAX_LEN as usize > OPS_MAX {
            self.ops.clear();
            self.slots.fill(EMPTY);
            self.targets.clear();
            self.epoch += 1;
        }
        let start = self.ops.len();
        let gen = unsafe { *genp };
        let words = MAX_LEN as usize;
        for w in ((pa & 0xFFF) >> 2) as usize..words {
            let op = decoded.get(unsafe { code.add(w).read_unaligned() });
            let last = op.ends_run() || w == words - 1;
            self.ops.push(RunOp::new::<M>(op, last));
            if last {
                break;
            }
        }
        let len = self.ops.len() - start;
        // Two ways a set: the newer run goes first, the older one second,
        // and the one before that is dropped.
        if self.slots[i].pa != pa {
            self.slots[i ^ 1] = self.slots[i];
        }
        self.slots[i] = Slot {
            pa,
            gen,
            genp,
            start: start as u32,
            len: len as u32,
        };
        self.targets.slots[Targets::slot(pa)] = Target {
            pa,
            gen,
            run: self.ops[start..].as_ptr(),
            len,
        };
        &self.ops[start..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_ends_at_its_branch_and_follows_its_page() {
        // add x0, x0, #1; add x1, x1, #1; b .-8; then add x2, x2, #1
        let mut page = [0x9100_0442u32; 1024];
        page[..3].copy_from_slice(&[0x9100_0400, 0x9100_0421, 0x17FF_FFFE]);
        let mut gen = 7u64;
        let (mut runs, mut d) = (Runs::<ArmBus>::default(), Decoded::new());
        let run = |runs: &mut Runs, d: &mut Decoded, page: &[u32; 1024], gen: &u64| {
            runs.get(0x8000, page.as_ptr(), gen, d)
                .iter()
                .map(|o| o.op.insn())
                .collect::<Vec<_>>()
        };
        let before = page[..3].to_vec();
        assert_eq!(run(&mut runs, &mut d, &page, &gen), before);
        page[1] = 0x1400_0000; // b .
        assert_eq!(
            run(&mut runs, &mut d, &page, &gen),
            before,
            "same generation, same run"
        );
        gen += 1;
        assert_eq!(run(&mut runs, &mut d, &page, &gen), page[..2]);
        let tail = runs
            .get(0x8000 + 4 * 1020, page.as_ptr(), &gen, &mut d)
            .len();
        assert_eq!(tail, 4, "a run without a branch ends with its page");
    }

    #[test]
    fn a_link_holds_while_the_page_generation_does() {
        let page = [0x9100_0400u32, 0x17FF_FFFF, 0x9100_0421, 0xD65F_03C0]
            .into_iter()
            .chain(std::iter::repeat(0xD503_201F))
            .take(1024)
            .collect::<Vec<_>>();
        let gen = 3u64;
        let (mut runs, mut d) = (Runs::<ArmBus>::default(), Decoded::new());
        let branch: *const RunOp = &runs.get(0x8000, page.as_ptr(), &gen, &mut d)[1];
        let target: *const RunOp = &runs.get(0x8008, page.as_ptr(), &gen, &mut d)[0];
        // The arena stays put while the epoch does.
        let branch = unsafe { &*branch };
        assert_eq!(branch.linked(gen), None);
        branch.link(unsafe { &*target }, gen);
        assert_eq!(branch.linked(gen), Some(target));
        assert_eq!(branch.linked(gen + 1), None);
    }
}
