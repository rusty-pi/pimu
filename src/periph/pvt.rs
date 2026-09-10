//! BCM2711 per-channel PVT monitors at `0x7D5D_8000` — the process / voltage /
//! temperature sensors start4's DVFS code samples alongside the AVS monitor.
//!
//! Eighteen channels, `0x40` bytes apart, at `0x7D5D_8000 + ch*0x40`. The block
//! sits inside the `0x7D5D_0000` window, so without this device it falls
//! through to [`ClkMon`](crate::periph::clkmon)'s plain storage and every
//! register reads 0 — including the magic below.
//!
//! ## The conversation start4 has with it
//!
//! `FUN_0ec300fa(ch, _, &hi, &lo)` is the reader, and it is gated on a magic:
//!
//! ```c
//! if (*(int *)(ch * 0x40 + 0x7d5d8010) == 0x7fff50cf) {
//!     uVar1 = *(uint *)(ch * 0x40 + 0x7d5d801c);
//!     *param_3 = (ushort)(uVar1 >> 0x10);
//!     *param_4 = (ushort)uVar1;
//!     if (*param_3 < 10) { *param_3 = 0; ... }
//!     if (uVar2 < 10) { *param_4 = 0; }
//! }
//! else { *param_3 = 0; *param_4 = 0; }
//! ```
//!
//! With `+0x10` reading 0 the magic never matches, the "absent" branch runs and
//! both halves come back zero. The firmware handles that cleanly, so nothing
//! was blocked — but it is the `SCALER_DISPID` trap from #13 exactly: a magic
//! register reading 0 means *this chip is absent*, not *unimplemented*, and the
//! model was quietly asserting that a Pi 4 has no PVT blocks. It has eighteen.
//!
//! The other registers the firmware touches, all per channel:
//!
//! * `+0x14` and `+0x18` are read/write threshold pairs, two 16-bit fields
//!   each. `FUN_0ec30276` reads both and splits them; `FUN_0ec3030e` writes one
//!   or the other as `(lo & 0xffff) | (hi << 16)`. Plain storage is right, but
//!   the *initial* value matters because `FUN_0ec30276` can read them before
//!   anything has written them — so they are seeded from hardware below.
//! * `+0x1C` is the read-only measurement pair, the one `FUN_0ec300fa` takes.
//! * `+0x00` reads back the channel's own index.
//!
//! ## Ground truth
//!
//! Read off `rpi-dev` — a Pi 4 running Linux, freshly booted and idle — through
//! `/dev/mem` at `0xFD5D_8000` (the `0x7C…`/`0xFC…` alias of this window).
//! Every one of the eighteen channels carries the magic:
//!
//! ```text
//!        +0x00  +0x10       +0x14       +0x18       +0x1C
//! ch 0       0  0x7fff50cf  0x03640340  0x06480624  0x04270799
//! ch 1       1  0x7fff50cf  0x03640340  0x06480624  0x042807b2
//! ch 2       2  0x7fff50cf  0           0           0x04250783
//! ch 3       3  0x7fff50cf  0           0           0x042107a7
//! ch 4       4  0x7fff50cf  0x035d0340  0x06480624  0x043707bc
//! ch 5..17   n  0x7fff50cf  0x035d0340  0x06480624  0x04xx07xx
//! ```
//!
//! Two things worth recording about that dump:
//!
//! * Channels 2 and 3 have the magic and a live `+0x1C`, but their `+0x14` and
//!   `+0x18` thresholds are zero. That is not a truncated read — it is
//!   consistent across the whole block and reproduced across two boots.
//! * `+0x14` differs between channels 0..1 (`0x0364…`) and 4..17 (`0x035d…`).
//!   Both are reproduced verbatim rather than averaged into one constant.
//!
//! This is a Linux-booted board, so these are post-start4 values, not
//! power-on-reset values. For `+0x10` and `+0x00` that distinction does not
//! matter (a magic and an index are hardwired). For `+0x14` / `+0x18` it might:
//! start4 may itself have programmed them during the boot we are modelling. As
//! seeds for registers the firmware overwrites anyway, they are the best
//! available answer, and far better than zero.
//!
//! ## What the `+0x1C` readings are used for, and why a constant is right
//!
//! `FUN_0ec303e8` walks the channels twice. The first pass stores each half as
//! a baseline in `gp+0xd47dc` / `gp+0xd4824`; the second re-reads and feeds
//! `FUN_0ec72aae(baseline, sample, temp, volt, coeff)`, the same adaptive
//! corrector the AVS rail monitors go through. It returns the baseline
//! unchanged whenever the two samples differ by at most 1, so a *stable*
//! reading means "no adaptive correction" — the right answer for a model that
//! does not simulate silicon speed. Both halves must also be non-zero, and at
//! least 10, or the correction is skipped entirely; the real values (`0x0406`
//! to `0x0437` high, `0x077c` to `0x07c4` low) clear that floor comfortably.
//!
//! So the per-channel `+0x1C` values are reproduced exactly as measured and
//! held constant, deliberately. Nothing here models drift.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

pub const BASE: u32 = 0x7D5D_8000;
/// Eighteen channels of `0x40` bytes. start4 initialises exactly this many
/// (`k = 0..17`), and channel 17 is the last one carrying the magic on
/// hardware.
pub const CHANNELS: u32 = 18;
pub const STRIDE: u32 = 0x40;
pub const SIZE: u32 = CHANNELS * STRIDE;

/// `+0x00` — reads back the channel index.
const REG_INDEX: u32 = 0x00;
/// `+0x10` — the magic `FUN_0ec300fa` requires before it will read `+0x1C`.
const REG_MAGIC: u32 = 0x10;
/// `+0x14`, `+0x18` — read/write threshold pairs.
const REG_THRESHOLD_A: u32 = 0x14;
const REG_THRESHOLD_B: u32 = 0x18;
/// `+0x1C` — the read-only measurement pair, `hi = [31:16]`, `lo = [15:0]`.
const REG_READING: u32 = 0x1C;

/// The value `FUN_0ec300fa` compares `+0x10` against. Present on all eighteen
/// channels of the reference board.
const MAGIC: u32 = 0x7FFF_50CF;

/// `+0x14` and `+0x18` as measured, per channel. Channels 2 and 3 really do
/// read zero for both.
const THRESHOLDS: [(u32, u32); CHANNELS as usize] = [
    (0x0364_0340, 0x0648_0624), // ch 0
    (0x0364_0340, 0x0648_0624), // ch 1
    (0x0000_0000, 0x0000_0000), // ch 2
    (0x0000_0000, 0x0000_0000), // ch 3
    (0x035d_0340, 0x0648_0624), // ch 4
    (0x035d_0340, 0x0648_0624), // ch 5
    (0x035d_0340, 0x0648_0624), // ch 6
    (0x035d_0340, 0x0648_0624), // ch 7
    (0x035d_0340, 0x0648_0624), // ch 8
    (0x035d_0340, 0x0648_0624), // ch 9
    (0x035d_0340, 0x0648_0624), // ch 10
    (0x035d_0340, 0x0648_0624), // ch 11
    (0x035d_0340, 0x0648_0624), // ch 12
    (0x035d_0340, 0x0648_0624), // ch 13
    (0x035d_0340, 0x0648_0624), // ch 14
    (0x035d_0340, 0x0648_0624), // ch 15
    (0x035d_0340, 0x0648_0624), // ch 16
    (0x035d_0340, 0x0648_0624), // ch 17
];

/// `+0x1C` as measured, per channel.
const READINGS: [u32; CHANNELS as usize] = [
    0x0427_0799, // ch 0
    0x0428_07b2, // ch 1
    0x0425_0783, // ch 2
    0x0421_07a7, // ch 3
    0x0437_07bc, // ch 4
    0x0410_078f, // ch 5
    0x041c_077c, // ch 6
    0x040c_0789, // ch 7
    0x0429_0790, // ch 8
    0x0413_0795, // ch 9
    0x0419_079d, // ch 10
    0x0406_07b9, // ch 11
    0x041f_07a5, // ch 12
    0x0413_07a2, // ch 13
    0x0425_07a2, // ch 14
    0x0423_07a8, // ch 15
    0x0428_07b6, // ch 16
    0x0434_07c4, // ch 17
];

#[derive(Default)]
pub struct Pvt {
    /// Writes to the threshold pairs, and to anything else in the block.
    storage: BTreeMap<u32, u32>,
}

impl Pvt {
    pub fn new() -> Pvt {
        Pvt::default()
    }
}

impl MmioDevice for Pvt {
    fn name(&self) -> &'static str {
        "pvt-monitor"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        let ch = off / STRIDE;
        let reg = off % STRIDE;
        if ch >= CHANNELS {
            return Ok(self.storage.get(&off).copied().unwrap_or(0));
        }
        match reg {
            REG_INDEX => Ok(ch),
            REG_MAGIC => Ok(MAGIC),
            REG_READING => Ok(READINGS[ch as usize]),
            REG_THRESHOLD_A | REG_THRESHOLD_B => {
                // Writable, but seeded from hardware so a read that precedes
                // any write still sees a real value.
                let (a, b) = THRESHOLDS[ch as usize];
                let seed = if reg == REG_THRESHOLD_A { a } else { b };
                Ok(self.storage.get(&off).copied().unwrap_or(seed))
            }
            _ => Ok(self.storage.get(&off).copied().unwrap_or(0)),
        }
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        let ch = off / STRIDE;
        let reg = off % STRIDE;
        // The magic, the index and the reading are read-only on hardware;
        // swallow writes rather than letting them mask the real values.
        if ch < CHANNELS && matches!(reg, REG_INDEX | REG_MAGIC | REG_READING) {
            return Ok(());
        }
        self.storage.insert(off, value);
        Ok(())
    }
}
