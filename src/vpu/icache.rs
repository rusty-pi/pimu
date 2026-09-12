//! A direct-mapped cache of decoded instructions (#43).
//!
//! Fetch + decode was a quarter of the firmware boot's host time, nearly all
//! of it decoding the same few loops again (inflating `kernel8.img` alone is
//! 38% of the VPU's instructions). An entry is keyed by the full `pc` —
//! `decode` resolves pc-relative targets into the `Op`, so an alias of the
//! same physical page is a different entry — and remembers the write
//! generation of its RAM page ([`crate::mem::Ram::page_gen`]) at the time it
//! was decoded. Any store into that page since, by either side or by DMA,
//! makes it stale.

use super::insn::{Insn, Op};

const ENTRIES: usize = 1 << 13;

/// No page's generation ever gets this high, so an entry holding it is empty.
const EMPTY: u64 = u64::MAX;

struct Entry {
    pc: u32,
    gen: u64,
    insn: Insn,
}

pub struct DecodeCache {
    entries: Box<[Entry]>,
    /// Instructions served from the cache.
    pub hits: u64,
    /// Instructions decoded into it.
    pub fills: u64,
    /// Fills that replaced an entry for the same `pc` because its page had
    /// been written since: stores into code, or into data sharing its page.
    pub stale: u64,
}

impl Default for DecodeCache {
    fn default() -> Self {
        let empty = || Entry {
            pc: 0,
            gen: EMPTY,
            insn: Insn {
                op: Op::Nop,
                len: 2,
            },
        };
        DecodeCache {
            entries: (0..ENTRIES).map(|_| empty()).collect(),
            hits: 0,
            fills: 0,
            stale: 0,
        }
    }
}

impl DecodeCache {
    #[inline]
    fn slot(pc: u32) -> usize {
        (pc as usize >> 1) & (ENTRIES - 1)
    }

    /// The generation the entry for `pc` was decoded under, if there is one.
    #[inline]
    pub fn cached_gen(&self, pc: u32) -> Option<u64> {
        let e = &self.entries[Self::slot(pc)];
        (e.pc == pc && e.gen != EMPTY).then_some(e.gen)
    }

    /// The entry for `pc`; only after [`Self::cached_gen`] said it is current.
    #[inline]
    pub fn get(&mut self, pc: u32) -> Insn {
        self.hits += 1;
        self.entries[Self::slot(pc)].insn.clone()
    }

    /// Remember `insn`, decoded at `pc` while its page was at generation `gen`.
    ///
    /// Not cached: an instruction running into the next page, whose bytes
    /// that page's generation does not cover, and vector instructions, whose
    /// `Op` is boxed — cloning it out again would cost about what decoding
    /// does, and the boot barely runs any.
    pub fn fill(&mut self, pc: u32, gen: u64, insn: &Insn) {
        if (pc & 0xFFF) + u32::from(insn.len) > 0x1000 || matches!(insn.op, Op::Vector(_)) {
            return;
        }
        let e = &mut self.entries[Self::slot(pc)];
        if e.pc == pc && e.gen != EMPTY {
            self.stale += 1;
        }
        self.fills += 1;
        *e = Entry {
            pc,
            gen,
            insn: insn.clone(),
        };
    }
}
