//! The VPU's L2 while the bootcode runs out of it.
//!
//! The boot ROM stages the bootcode into the L2 at `0x8000_0000` before there
//! is any SDRAM behind it, and it runs there until it flushes the L2 through
//! the bootbox (`specs/bootbox.toml`). Until that flush a write through the
//! uncached alias (`0xC000_0000`) must go to SDRAM and leave the line the L2
//! holds alone — old bootcode relies on it, clearing or loading over ranges
//! that cover its own stack and config. So this sits in front of the machine's
//! single backing store for the VPU's own accesses: lines the ROM staged or a
//! cached alias wrote are *held*, uncached accesses to a held line go to the
//! SDRAM behind it, and a flush writes the held lines back and forgets them.
//! DMA engines address the backing store directly, so they see the L2's copy.

use crate::bus::{BusResult, Width};
use crate::mem::Ram;

/// How much of physical memory the bootcode's L2 can hold: more than 128 KiB,
/// since 2023-05-11 bootcode decompresses to `0x8002_0800`.
pub const WINDOW: u32 = 0x4_0000;

const LINE_SHIFT: u32 = 5;
const LINE: u32 = 1 << LINE_SHIFT;
const LINES: usize = (WINDOW >> LINE_SHIFT) as usize;

#[derive(Default)]
pub struct CacheAsRam {
    active: bool,
    held: Vec<bool>,
    diverged: Vec<bool>,
    behind: Option<Ram>,
}

impl CacheAsRam {
    /// The ROM puts `len` bytes at `phys` into the L2; `len` 0 for the ROM's
    /// own stores.
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

    #[inline]
    pub fn covers(&self, phys: u32) -> bool {
        self.active && phys < WINDOW
    }

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
            let base = phys & !(LINE - 1);
            behind.write_slice(base, ram.read_slice(base, LINE as usize)?)?;
            self.diverged[l] = true;
        }
        behind.store(phys, width, value)
    }

    /// Clean and invalidate `start..=end`: the held lines are forgotten.
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
