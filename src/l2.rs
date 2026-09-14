//! The VPU's L2 while the bootcode runs out of it (#70).
//!
//! The boot ROM stages the bootcode into the L2 at `0x8000_0000` before there
//! is any SDRAM behind it, and the bootcode keeps its code, data and stack
//! there until it flushes the L2 on its way to the next stage: the stub it
//! relocates to `0x6001_0000` writes a range to `0x7EE0_1004` / `0x7EE0_1008`
//! and a command to `0x7EE0_1000`. Until then a write through the uncached
//! alias (`0xC000_0000`) goes to SDRAM and leaves the line the L2 holds alone,
//! and old bootcode relies on that: 2020-12-11 clears a megabyte from
//! `0xC001_0000` across its own stack at `0x8001_A424`, and 2022-04-26 loads a
//! file to `0xC001_8000` across the config it hands to bootmain from
//! `0x8001_8020`.
//!
//! [`crate::machine::Machine`] folds every alias onto one backing store, which
//! is right once the L2 is an ordinary cache. Until the flush this sits in
//! front of it for the VPU's own loads and stores:
//!
//! * A line written through a cached alias, or staged by the ROM, is *held*:
//!   the backing store is the L2's copy of it.
//! * An uncached write to a held line goes to the SDRAM behind the L2 instead,
//!   and uncached reads of that line come from there.
//! * Everything else goes straight to the backing store, as before.
//! * A flush writes the held lines back, which leaves the backing store as it
//!   is, and forgets them: uncached writes to them are lost, as on hardware.
//!   Once a flush has covered the whole window the L2 is an ordinary cache.
//!
//! DMA engines address the backing store directly, so they see the L2's copy.
//! As far as the traces show, nothing running out of the L2 starts one.

use crate::bus::{BusResult, Width};
use crate::mem::Ram;

/// How much of physical memory, from 0, the bootcode's L2 can hold. The
/// 2023-05-11 bootcode decompresses into a buffer ending at `0x8002_0800`, so
/// it is more than 128 KiB.
pub const WINDOW: u32 = 0x4_0000;

/// The bootcode's flush ends on `0x0FFF_FFE0`: 32-byte lines.
const LINE_SHIFT: u32 = 5;
const LINE: u32 = 1 << LINE_SHIFT;
const LINES: usize = (WINDOW >> LINE_SHIFT) as usize;

#[derive(Default)]
pub struct CacheAsRam {
    /// The bootcode is running out of the L2: set by the boot ROM, cleared by
    /// a flush that covers the window.
    active: bool,
    /// Lines the L2 holds: staged by the ROM or written through a cached alias.
    held: Vec<bool>,
    /// Held lines an uncached write has reached: `behind` has SDRAM's version.
    diverged: Vec<bool>,
    /// The SDRAM behind the window, as the uncached alias sees it.
    behind: Option<Ram>,
}

impl CacheAsRam {
    /// The ROM puts `len` bytes at physical `phys` into the L2 (`len` 0 when
    /// the ROM itself runs and stages the bootcode through ordinary stores).
    pub fn hold(&mut self, phys: u32, len: usize) {
        if !self.active {
            self.active = true;
            self.held = vec![false; LINES];
            self.diverged = vec![false; LINES];
            self.behind = Some(Ram::new(0, WINDOW as usize));
        }
        let end = (phys as u64 + len as u64).min(WINDOW as u64) as u32;
        for line in phys >> LINE_SHIFT..end.div_ceil(LINE) {
            self.held[line as usize] = true;
        }
    }

    /// True while the bootcode's L2 may hold physical `phys`.
    #[inline]
    pub fn covers(&self, phys: u32) -> bool {
        self.active && phys < WINDOW
    }

    /// True when a read of `addr` has to come from the SDRAM behind the L2.
    #[inline]
    pub fn diverts(&self, addr: u32, phys: u32) -> bool {
        self.covers(phys) && uncached(addr) && self.diverged[line(phys)]
    }

    pub fn load(&self, ram: &Ram, addr: u32, phys: u32, width: Width) -> BusResult<u32> {
        if self.diverts(addr, phys) {
            return self.behind.as_ref().expect("armed").load(phys, width);
        }
        ram.load(phys, width)
    }

    pub fn store(
        &mut self,
        ram: &mut Ram,
        addr: u32,
        phys: u32,
        width: Width,
        value: u32,
    ) -> BusResult<()> {
        let l = line(phys);
        if !uncached(addr) {
            self.held[l] = true;
            return ram.store(phys, width, value);
        }
        if !self.held[l] {
            return ram.store(phys, width, value);
        }
        let behind = self.behind.as_mut().expect("armed");
        if !self.diverged[l] {
            // Until now SDRAM read back what the L2 holds.
            let base = phys & !(LINE - 1);
            behind.write_slice(base, ram.read_slice(base, LINE as usize)?)?;
            self.diverged[l] = true;
        }
        behind.store(phys, width, value)
    }

    /// Clean and invalidate `start..=end` (`0x7EE0_1000`): the held lines are
    /// written back, which the backing store already has, and forgotten.
    pub fn flush(&mut self, start: u32, end: u32) {
        let (start, end) = (start & 0x3FFF_FFFF, end & 0x3FFF_FFFF);
        if !self.active || start >= WINDOW || end < start {
            return;
        }
        for l in line(start)..=line(end.min(WINDOW - 1)) {
            self.held[l] = false;
            self.diverged[l] = false;
        }
        if start == 0 && end >= WINDOW - LINE {
            *self = CacheAsRam::default();
        }
    }
}

#[inline]
fn uncached(addr: u32) -> bool {
    addr >> 30 == 3
}

#[inline]
fn line(phys: u32) -> usize {
    (phys >> LINE_SHIFT) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    const STACK: u32 = 0x1_A420;

    fn armed() -> (CacheAsRam, Ram) {
        let mut l2 = CacheAsRam::default();
        l2.hold(0, 0x100);
        (l2, Ram::new(0, 1 << 20))
    }

    /// 2020-12-11 clears SDRAM from `0xC001_0000` across the stack it keeps
    /// at `0x8001_A424`.
    #[test]
    fn an_uncached_write_leaves_a_held_line_alone() {
        let (mut l2, mut ram) = armed();
        l2.store(&mut ram, 0x8000_0000 | STACK, STACK, Width::Word, 0x1234)
            .unwrap();
        l2.store(&mut ram, 0xC000_0000 | STACK, STACK, Width::Word, 0)
            .unwrap();
        assert_eq!(
            l2.load(&ram, 0x8000_0000 | STACK, STACK, Width::Word),
            Ok(0x1234)
        );
        assert_eq!(
            l2.load(&ram, 0xC000_0000 | STACK, STACK, Width::Word),
            Ok(0)
        );
        // The rest of the line still reads what the L2 had.
        l2.store(
            &mut ram,
            0x8000_0000 | (STACK + 4),
            STACK + 4,
            Width::Word,
            7,
        )
        .unwrap();
        assert_eq!(
            l2.load(&ram, 0xC000_0000 | (STACK + 4), STACK + 4, Width::Word),
            Ok(0)
        );
    }

    #[test]
    fn a_line_the_l2_does_not_hold_stays_coherent() {
        let (mut l2, mut ram) = armed();
        l2.store(&mut ram, 0xC000_0000 | STACK, STACK, Width::Word, 5)
            .unwrap();
        assert_eq!(
            l2.load(&ram, 0x8000_0000 | STACK, STACK, Width::Word),
            Ok(5)
        );
        // The ROM's staging is held.
        l2.store(&mut ram, 0xC000_0010, 0x10, Width::Word, 5)
            .unwrap();
        assert_eq!(l2.load(&ram, 0x8000_0010, 0x10, Width::Word), Ok(0));
    }

    #[test]
    fn a_flush_over_the_window_writes_back_and_ends_it() {
        let (mut l2, mut ram) = armed();
        l2.store(&mut ram, 0x8000_0000 | STACK, STACK, Width::Word, 0x1234)
            .unwrap();
        l2.store(&mut ram, 0xC000_0000 | STACK, STACK, Width::Word, 0)
            .unwrap();
        l2.flush(0, 0x0FFF_FFE0);
        assert!(!l2.covers(STACK));
        assert_eq!(ram.load(STACK, Width::Word), Ok(0x1234));
    }

    #[test]
    fn a_partial_flush_forgets_only_its_lines() {
        let (mut l2, mut ram) = armed();
        for a in [0x100, STACK] {
            l2.store(&mut ram, 0x8000_0000 | a, a, Width::Word, 1)
                .unwrap();
        }
        l2.flush(STACK, STACK + 0x1F);
        for a in [0x100, STACK] {
            l2.store(&mut ram, 0xC000_0000 | a, a, Width::Word, 2)
                .unwrap();
        }
        assert!(l2.covers(STACK));
        assert_eq!(
            l2.load(&ram, 0x8000_0000 | 0x100, 0x100, Width::Word),
            Ok(1)
        );
        assert_eq!(
            l2.load(&ram, 0x8000_0000 | STACK, STACK, Width::Word),
            Ok(2)
        );
    }
}
