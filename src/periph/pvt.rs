//! BCM2711 per-channel PVT monitors at `0x7D5D_8000` — the process / voltage /
//! temperature sensors start4's DVFS code samples alongside the AVS monitor.
//!
//! Registers and fields: `specs/pvt.toml` ([`crate::spec::pvt`]). Eighteen
//! channels, `0x40` bytes apart. The block sits inside the `0x7D5D_0000`
//! window, so it has to be decoded ahead of [`ClkMon`](crate::periph::clkmon),
//! whose plain storage would otherwise answer 0 everywhere — including for the
//! magic at `MAGIC`. A magic register reading 0 does not mean *unimplemented*,
//! it tells the firmware *this chip is absent*: start4 then skips the channel
//! and reports no measurement at all.
//!
//! ## Ground truth
//!
//! Read off a Raspberry Pi 4B d03115 running Linux, freshly booted and idle,
//! through `/dev/mem` at `0xFD5D_8000` (the `0x7C…`/`0xFC…` alias of this
//! window). Every one of the eighteen channels carries the magic:
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
//! Channels 2 and 3 have the magic and a live `READING`, but zero thresholds:
//! start4 leaves them out of its core-voltage characterisation. `THRESHOLD_A`
//! differs between channels 0..1 and 4..17; both values are reproduced verbatim
//! rather than averaged into one constant.
//!
//! These are post-start4 values off a Linux-booted board, not power-on-reset
//! values. For the magic and the index that makes no difference; the thresholds
//! may well be start4's own, but as seeds for registers the firmware overwrites
//! anyway they are far better than zero.
//!
//! ## Why the readings are constants
//!
//! start4 walks the channels twice, keeps the first pass as a baseline and
//! feeds the second through the same adaptive corrector the AVS rail monitors
//! go through. That corrector returns the baseline unchanged whenever the two
//! samples differ by at most 1, so a *stable* reading means "no adaptive
//! correction" — the right answer for a model that does not simulate silicon
//! speed. Both halves must also be at least 10 or the correction is skipped;
//! the measured values clear that floor comfortably. Nothing here models drift.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::pvt::{
    INDEX as REG_INDEX, INSTANCES, INSTANCE_STRIDE as STRIDE, MAGIC as REG_MAGIC,
    MAGIC_RESET as MAGIC, READING as REG_READING, THRESHOLD_A as REG_THRESHOLD_A,
    THRESHOLD_B as REG_THRESHOLD_B,
};
use crate::spec::Coverage;

/// Eighteen channels, one register bank each — the number start4 initialises,
/// and the number carrying the magic on hardware.
pub const CHANNELS: u32 = INSTANCES;

/// Every register in `specs/pvt.toml` is modelled.
pub const COVERAGE: Coverage = Coverage {
    block: "pvt",
    decoded: &[
        REG_INDEX,
        REG_MAGIC,
        REG_THRESHOLD_A,
        REG_THRESHOLD_B,
        REG_READING,
    ],
};

/// `THRESHOLD_A` and `THRESHOLD_B` as measured, per channel. Channels 2 and 3
/// really do read zero for both.
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

/// `READING` as measured, per channel.
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
