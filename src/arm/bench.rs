//! A micro-benchmark of the ARM side: all four cores run a loop shaped like
//! the Linux boot's hottest one, either taking turns or on threads of their
//! own, driven the way the page drives them. It takes seconds where a boot
//! takes half an hour, for comparing changes to a core's run loop before the
//! boot confirms them (`web/tests/arm-speed.html`, and `arm::bench::tests`
//! natively).

#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

use super::{ArmSide, CORES};
use crate::bus::Width;
use crate::machine::Machine;

const CODE: u32 = 0x10000;
const DONE: u32 = 0x808;
const ROUNDS_AT: u32 = CODE + 0x18;
const PER_ROUND: u64 = 2 + 16_384 * 12 + 2;

/// From `CODE` on: each core takes a stack page of its own, then runs the
/// loop of `update_event_printk`'s shape (byte loads, a table lookup, flags,
/// a pair store and load) `16384` times a round, and when its rounds are
/// done sets its word at `DONE + 8 * id` and sleeps in `wfi`.
const LOOP: [u32; 29] = [
    0xd538_00a8, // mrs x8, mpidr_el1
    0x9240_1d08, // and x8, x8, #0xff
    0xd2a0_061a, // mov x26, #0x300000
    0xd2a0_081c, // mov x28, #0x400000
    0x8b08_339c, // add x28, x28, x8, lsl #12
    0x9120_039c, // add x28, x28, #0x800
    0xd280_0015, // mov x21, #rounds
    0xd2a0_0413, // outer: mov x19, #0x200000
    0xd288_0014, // mov x20, #0x4000
    0x3840_1e63, // 1: ldrb w3, [x19, #1]!
    0x3863_cb40, // ldrb w0, [x26, w3, sxtw]
    0x1200_0400, // and w0, w0, #3
    0x7101_7c7f, // cmp w3, #0x5f
    0x7a40_1800, // ccmp w0, #0, #0, ne
    0x8b00_00a5, // add x5, x5, x0
    0xd37e_f4a6, // lsl x6, x5, #2
    0xa9bf_1b85, // stp x5, x6, [x28, #-16]!
    0xa8c1_1b85, // ldp x5, x6, [x28], #16
    0x9a86_00a7, // csel x7, x5, x6, eq
    0xf100_0694, // subs x20, x20, #1
    0x54ff_fea1, // b.ne 1b
    0xf100_06b5, // subs x21, x21, #1
    0x54ff_fe21, // b.ne outer
    0xd281_0109, // mov x9, #0x808
    0x8b08_0d29, // add x9, x9, x8, lsl #3
    0xd280_002a, // mov x10, #1
    0xf900_012a, // str x10, [x9]
    0xd503_207f, // 2: wfi
    0x17ff_ffff, // b 2b
];

/// What a run of [`speed`] did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Speed {
    /// Whether the cores ran on threads; `false` where the build or the host
    /// has none for them.
    pub threaded: bool,
    /// Guest instructions all four cores ran, together.
    pub insns: u64,
    pub secs: f64,
}

impl Speed {
    pub fn mips(&self) -> f64 {
        self.insns as f64 / self.secs / 1e6
    }
}

/// Each core runs about `per_core` instructions of [`LOOP`]; on threads when
/// `parallel` and the build has them. `None` if the cores stopped or did not
/// get there within a few minutes.
pub fn speed(parallel: bool, per_core: u64) -> Option<Speed> {
    let rounds = per_core.div_ceil(PER_ROUND).clamp(1, 0xffff) as u32;
    let mut m = Machine::new(8 << 20);
    let base = m.ram.base();
    m.ram
        .store(base, Width::Word, 0x1400_0000 | (CODE >> 2))
        .ok()?;
    for (i, w) in LOOP.iter().enumerate() {
        m.ram
            .store(base + CODE + 4 * i as u32, Width::Word, *w)
            .ok()?;
    }
    m.ram
        .store(base + ROUNDS_AT, Width::Word, LOOP[6] | (rounds << 5))
        .ok()?;
    let done = |m: &Machine| {
        (0..CORES as u32).all(|id| m.ram.load(base + DONE + 8 * id, Width::Word) == Ok(1))
    };
    let mut arm = ArmSide::new();
    arm.set_parallel(parallel);
    let start = Instant::now();
    while !done(&m) && arm.stopped.is_none() {
        if start.elapsed().as_secs() > 300 {
            return None;
        }
        m.tick(54 * 20);
        for _ in 0..64 {
            arm.catch_up(&mut m);
        }
    }
    let secs = start.elapsed().as_secs_f64();
    let threaded = arm.is_parallel();
    arm.pause(&mut m);
    arm.stopped.is_none().then_some(Speed {
        threaded,
        insns: CORES as u64 * u64::from(rounds) * PER_ROUND,
        secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_core_finishes_its_rounds() {
        let s = speed(false, 1).unwrap();
        assert!(!s.threaded);
        assert_eq!(s.insns, CORES as u64 * PER_ROUND);
    }

    #[cfg(pimu_threads)]
    #[test]
    fn every_core_finishes_its_rounds_on_threads() {
        let s = speed(true, 1).unwrap();
        assert!(s.threaded);
    }

    /// `PIMU_SPEED_INSNS` instructions a core (100 M), taking turns and then
    /// on threads, for timing a change to either run loop on a board.
    #[test]
    #[ignore]
    fn speed_both_ways() {
        let n = std::env::var("PIMU_SPEED_INSNS").map_or(100_000_000, |v| v.parse().unwrap());
        for parallel in [false, true] {
            let s = speed(parallel, n).unwrap();
            println!(
                "threaded={} insns={} secs={:.3} mips={:.1}",
                s.threaded,
                s.insns,
                s.secs,
                s.mips()
            );
        }
    }
}
