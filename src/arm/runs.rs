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
use crate::aarch64::predecode::{Decoded, RunOp};

const SLOTS_LOG2: u32 = 15;
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

pub struct Runs {
    slots: Box<[Slot]>,
    /// Never grown past its capacity, so a run's instructions stay where
    /// they are until [`Self::epoch`] moves.
    ops: Vec<RunOp>,
    /// Counts the times every run was dropped at once.
    pub epoch: u64,
}

// The generation pointers point into the board's RAM, which outlives every
// core that runs out of it.
unsafe impl Send for Runs {}

impl Default for Runs {
    fn default() -> Self {
        Runs {
            slots: vec![EMPTY; 1 << SLOTS_LOG2].into_boxed_slice(),
            ops: Vec::with_capacity(OPS_MAX),
            epoch: 0,
        }
    }
}

impl Runs {
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
        let i = (((pa >> 2) ^ (pa >> 17)) as usize) & ((1 << SLOTS_LOG2) - 1);
        let s = self.slots[i];
        if s.pa == pa && s.genp == genp && unsafe { *genp } == s.gen {
            return &self.ops[s.start as usize..(s.start + s.len) as usize];
        }
        self.decode(i, pa, code, genp, decoded)
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
            self.epoch += 1;
        }
        let start = self.ops.len();
        let gen = unsafe { *genp };
        let words = MAX_LEN as usize;
        for w in ((pa & 0xFFF) >> 2) as usize..words {
            let op = decoded.get(unsafe { code.add(w).read_unaligned() });
            let last = op.ends_run() || w == words - 1;
            self.ops.push(RunOp::new::<ArmBus<'static>>(op, last));
            if last {
                break;
            }
        }
        let len = self.ops.len() - start;
        self.slots[i] = Slot {
            pa,
            gen,
            genp,
            start: start as u32,
            len: len as u32,
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
        let (mut runs, mut d) = (Runs::default(), Decoded::new());
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
        let (mut runs, mut d) = (Runs::default(), Decoded::new());
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
