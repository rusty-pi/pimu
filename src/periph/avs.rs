//! BCM2711 AVS monitor at `0x7D5D_2000` — the on-die temperature sensor and the
//! ring-oscillator / voltage monitors that start4's DVFS code reads.
//!
//! The block is a real device tree node on the Pi 4:
//!
//! ```text
//! avs-monitor@7d5d2000 {
//!     compatible = "brcm,bcm2711-avs-monitor", "syscon", "simple-mfd";
//!     reg = <0x7d5d2000 0xf00>;
//!     thermal { compatible = "brcm,bcm2711-thermal"; };
//! };
//! ```
//!
//! Linux's `bcm2711_thermal` only touches `+0x200`: bit 10 = valid,
//! bits `[9:0]` = count, and `temp_mC = 410040 - 487 * count`. start4 uses the
//! same formula — `FUN_0ecc6488` multiplies by `-498739` and shifts right by 10
//! (`-498739 / 1024 = -487.05`) before adding `410040` — which is how we know
//! `+0x200` is the temperature channel here too.
//!
//! ## What start4 reads
//!
//! * `+0x03C` is a per-channel mask, written `~(1 << ch) & 0x7F` to take a
//!   reading from channel `ch` and `0` again afterwards (`FUN_0ed6040e`).
//!   Note the polarity: the *selected* channel's bit is the one left **clear**,
//!   and the all-zero value the firmware restores afterwards therefore means
//!   "no channel masked off", not "nothing selected". Real hardware agrees —
//!   see the mask discussion below. `+0x040` / `+0x044` are further enable
//!   masks, written all-ones for the duration of a measurement.
//! * `+0x200 + ch*4` (ch 0..5) is that channel's result. `FUN_0ed603e2` spins
//!   until **both** bit 10 (valid) and bit 16 (settled) are set, then takes
//!   `[9:0]`; `FUN_0ed6040e` averages up to 150 such samples.
//! * `+0x220 + ch*4` (ch 0..0x17) are the per-rail monitors `FUN_0ec3007a`
//!   polls for bit 16, taking `[14:0]`. `FUN_0ec303e8` skips every channel that
//!   reads back 0 and returns 0 if they all do — which makes the whole DVFS
//!   voltage calculation (`FUN_0ec31bb2`) bail out with 0.
//! * `+0xD00 + ch*4` / `+0xE00 + ch*4` are plain read/write storage
//!   (`FUN_0ec302c2` writes them, `FUN_0ec300a4` reads them back).
//!
//! ## Where the counts come from
//!
//! The channel results were originally *inverted* from what the firmware
//! reports, because `/dev/mem` was believed to be locked on the reference
//! board. It is not (`# CONFIG_STRICT_DEVMEM is not set`), so `+0x200 + ch*4`
//! is now read directly off `rpi-dev` — a Pi 4 running Linux, freshly booted
//! and idle:
//!
//! ```text
//! 0x7d5d2200  0x000106e8   ch0  valid|settled  count 744
//! 0x7d5d2204  0x00010402   ch1  valid|settled  count   2
//! 0x7d5d2208  0x0001069d   ch2  valid|settled  count 669
//! 0x7d5d220c  0x000106f6   ch3  valid|settled  count 758
//! 0x7d5d2210  0x00010402   ch4  valid|settled  count   2
//! 0x7d5d2214  0x00010749   ch5  valid|settled  count 841
//! ```
//!
//! That is the whole `DAT_0edfbe9c` register table (`0x7C000000 + 0x015D22xx`)
//! that `FUN_0ed603e2` indexes, so all six channels are accounted for. Pushing
//! each count back through start4's own conversion — `FUN_0ecc6488`, which is
//! `(count * DAT_0edfbe84[ch]) >> (ch == 0 ? 10 : 13)` plus `410040` for
//! channel 0 — gives readings that line up with what the firmware reports
//! through `vcgencmd` on the same board:
//!
//! * ch0, scale `-498739`: `410040 - 487 * 744 = 47712` = 47.7 degC, against
//!   `measure_temp` = 45.7 degC on a board that had just booted.
//! * ch3, scale `100571`: `(758 * 100571) >> 13 = 9305` tenths of a mV =
//!   930.5 mV, against `measure_volts core` = 0.9260 V. One count is 12.3 mV,
//!   so that is the sensor's own granularity, not a modelling error.
//! * ch2, same scale: 821.2 mV. ch5, scale `175488`: 1801.6 mV — the 1.8 V
//!   rail. Neither has a `vcgencmd` name to check against, but both land on a
//!   plausible rail voltage, which the old shared count did not.
//! * ch1 and ch4 genuinely read ~0 on real silicon (count 2, i.e. 2.4 mV).
//!   These are unpopulated monitors, and the previous model was actively wrong
//!   to report a healthy 754 for them.
//!
//! Prior to this the model answered every channel with one of two counts (752
//! for ch0, 754 for the rest), which was the remaining half of point 2 in
//! issue #1.
//!
//! Channel 0 keeps 752 rather than the 744 read above: 752 is the count that
//! matches the 43.816 degC recorded from `thermal_zone0` on a warmed-up board,
//! and it is what the rest of this file's arithmetic is documented against.
//! Both are valid idle readings of the same sensor a few degrees apart; there
//! is nothing to choose between them, so the documented one stays.
//!
//! The `+0x220` rail monitors have no ground truth at all, but their magnitude
//! cancels: start4 reads each rail twice and passes both to `FUN_0ec72aae`,
//! which returns its baseline unchanged whenever the two samples differ by at
//! most 1. A stable count therefore means "no adaptive correction", which is
//! the right answer for a model that does not simulate silicon speed. What
//! matters is only that they are non-zero and valid.
//!
//! ## The masks at `+0x03C`, `+0x040` and `+0x044`
//!
//! All three read back `0` on the idle reference board, at the same moment the
//! six channels above were reporting live, valid, plausible counts. That single
//! observation settles both of the questions the firmware alone left open:
//!
//! * `+0x03C` is an active-high **disable** mask, not a select. If a clear bit
//!   meant "not selected", an all-zero mask would gate every channel off, and
//!   the counts above could not have been read. Written `~(1 << ch) & 0x7F` it
//!   masks off everything *except* `ch`; restored to `0` it masks off nothing.
//!   The model honours it: a channel whose bit is set reports neither valid nor
//!   settled, which is what `FUN_0ed603e2` treats as "no sample" (it returns 0,
//!   and `FUN_0ed6040e` skips zeros when averaging).
//! * `+0x040` / `+0x044` cannot be read-enables, because the channels report
//!   perfectly good samples while both are zero. Whatever they do enable is not
//!   something the firmware's reads depend on, so they stay plain storage. That
//!   is a deliberate default backed by the measurement, not an omission.
//!
//! Because `FUN_0ed6040e` only ever reads the one channel it just unmasked,
//! honouring `+0x03C` does not change any measurement start4 takes. It matters
//! for the paths that read a channel *without* going through `FUN_0ed6040e`,
//! which previously got a reading from a channel that was masked off.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

pub const BASE: u32 = 0x7D5D_2000;
/// `reg = <0x7d5d2000 0xf00>` in the Pi 4 device tree.
pub const SIZE: u32 = 0x0000_0F00;

/// Channel results `FUN_0ed603e2` polls, `+0x200 + ch*4` for ch 0..5.
const RESULT_BASE: u32 = 0x200;
const RESULT_CHANNELS: u32 = 6;
/// Per-rail monitors `FUN_0ec3007a` polls, `+0x220 + ch*4` for ch 0..0x17.
const RAIL_BASE: u32 = 0x220;
const RAIL_CHANNELS: u32 = 0x18;

/// Channel disable mask, `~(1 << ch) & 0x7F` while a channel is being read and
/// `0` at rest. See the module docs: bits set here mask a channel *off*.
const DISABLE_MASK: u32 = 0x03C;
/// Only the low 7 bits of `+0x03C` are channel bits.
const DISABLE_MASK_BITS: u32 = 0x7F;

/// Bit 10: the reading is valid (Linux calls this `BCM2711_TS_VALID_MASK`).
const VALID: u32 = 1 << 10;
/// Bit 16: the reading has settled. Linux ignores it; start4 will not accept a
/// sample without it, which is why a zero-returning stub stalled the boot.
const SETTLED: u32 = 1 << 16;

/// Per-channel counts, read straight off `rpi-dev` at `0x7D5D2200 + ch*4` — see
/// the module docs for the raw dump and the conversion each one satisfies.
///
/// Channel 0 carries the 752 that matches the recorded 43.816 degC rather than
/// the 744 in that dump; the two are the same sensor a few degrees apart.
const CHANNEL_COUNTS: [u32; RESULT_CHANNELS as usize] = [752, 2, 669, 758, 2, 841];

/// Per-rail monitor count. Only "non-zero and stable" is load-bearing.
const RAIL_COUNT: u32 = 754;

#[derive(Default)]
pub struct Avs {
    storage: BTreeMap<u32, u32>,
}

impl Avs {
    pub fn new() -> Avs {
        Avs::default()
    }

    /// Is channel `ch` masked off by the current `+0x03C` value?
    ///
    /// Active high, low 7 bits only. At rest the register is 0, so nothing is
    /// masked — which is the state the reference board's channels were read in.
    fn channel_masked(&self, ch: u32) -> bool {
        let mask = self.storage.get(&DISABLE_MASK).copied().unwrap_or(0);
        mask & DISABLE_MASK_BITS & (1 << ch) != 0
    }
}

impl MmioDevice for Avs {
    fn name(&self) -> &'static str {
        "avs-monitor"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        if (RESULT_BASE..RESULT_BASE + RESULT_CHANNELS * 4).contains(&off) {
            let ch = (off - RESULT_BASE) / 4;
            if self.channel_masked(ch) {
                // Masked off: no valid, no settled. `FUN_0ed603e2` reads this
                // as "no sample" and returns 0, which `FUN_0ed6040e` skips.
                return Ok(0);
            }
            return Ok(SETTLED | VALID | CHANNEL_COUNTS[ch as usize]);
        }
        if (RAIL_BASE..RAIL_BASE + RAIL_CHANNELS * 4).contains(&off) {
            return Ok(SETTLED | RAIL_COUNT);
        }
        Ok(self.storage.get(&off).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset & !3, value);
        Ok(())
    }
}
