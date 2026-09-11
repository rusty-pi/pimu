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
//! * `+0x220 + ch*4` are the per-channel ring-oscillator monitors
//!   `FUN_0ec3007a` polls for bit 16, taking `[14:0]`. The DVFS sweeps run
//!   ch 0..0x17; the sensor API `FUN_0ec5f2c0` accepts ch 0..0x23, so the block
//!   is **36 channels wide**, not 24. `FUN_0ec303e8` skips every channel that
//!   reads back 0 and returns 0 if they all do — which makes the whole DVFS
//!   voltage calculation (`FUN_0ec31bb2`) bail out with 0.
//! * `+0xD00 + ch*4` / `+0xE00 + ch*4` are the per-channel lower and upper
//!   bounds `FUN_0ec300a4` reads back and `FUN_0ec303e8` compares each monitor
//!   count against. `FUN_0ec302c2` writes them at the end of the
//!   characterisation, so they are plain storage — see below.
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
//! ## The `+0x220` monitors, 36 channels wide
//!
//! These were long carried as "one constant count, valid, for 24 channels".
//! The width was wrong: `FUN_0ec5f2c0`, the sensor API behind
//! `vcgencmd`-style rail reads, range-checks its channel against 0x23 and
//! hands it straight to `FUN_0ec3007a`, and the boot does sweep all 36. Read
//! one word at a time off `rpi-dev`, channels 0x18..0x1F report real, small
//! counts and 0x20..0x23 report `0x00010000` — settled, with a count of
//! exactly 0.
//!
//! "Settled with a count of 0" is not the same as "never settles", and the
//! model used to answer both the same way. Every one of those twelve reads
//! burned `FUN_0ec3007a`'s ten retries with a `msleep(1000)` apiece before
//! giving up, and the sensor API then reported 0 V for rails hardware has a
//! value for.
//!
//! The counts' magnitude is otherwise inert: start4 reads each channel twice
//! and passes both samples to `FUN_0ec72aae`, which returns its baseline
//! unchanged whenever the two differ by at most 1. A per-channel constant
//! therefore means "no adaptive correction", which is the right answer for a
//! model that does not simulate silicon speed.
//!
//! ## Channel 3 has to follow the core rail
//!
//! Channel 3 is a voltage sensor on the SoC core rail, and the rail is driven
//! over I2C by the `0x1E` PMIC. Answering it with a fixed count — however well
//! measured — describes a rail that does not respond to being programmed, and
//! start4 checks for exactly that:
//!
//! ```text
//! FUN_0ed7223a(label, rail, target)   set the rail, return FUN_0ec302bc()
//! FUN_0ec302bc()                      = FUN_0ecc6488(3), i.e. this channel
//! FUN_0ec303e8                        two targets ~56 mV apart; if the two
//!                                     readings differ by less than 100
//!                                     tenths of a mV, give up and return 0
//! ```
//!
//! A zero from `FUN_0ec303e8` propagates: `FUN_0ec31bb2` returns 0,
//! `FUN_0ec309ec` returns 0, and the whole voltage/PVT characterisation
//! (`FUN_0ec30828`) is skipped. That is what the model used to do — the
//! calibration aborted on its very first probe, roughly 27 million
//! instructions were spent reaching that verdict, and `0x7D5D_183C`, start4's
//! own "characterisation done" flag, was left at 0 where hardware holds 1.
//!
//! So [`crate::machine::Machine`] mirrors the PMIC setpoint into this block
//! before every read of it, and channel 3 reports the count start4's own
//! decode turns back into that voltage. The calibration then converges the way
//! it does on silicon, and the proof is that it writes the same numbers:
//! `FUN_0ec30828`'s 44 computed bounds at `+0xD00` / `+0xE00` come out
//! byte-identical to the 44 words read off the reference board, starting
//! `0x683` / `0x6A1` for channel 0.
//!
//! That also settles what `+0xD00` / `+0xE00` are. They looked like per-die
//! calibration the model was blanking — nothing wrote them across a whole
//! boot, yet hardware held non-zero values in exactly the 22 channels the
//! firmware reads and 0 in the two `FUN_0ec30bde` masks off. They are neither:
//! they are `FUN_0ec302c2`'s own output, and the model read 0 only because the
//! code that fills them never ran. Plain storage is right, and seeding them
//! would have papered over the real bug.
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
/// Per-channel ring-oscillator monitors `FUN_0ec3007a` polls, `+0x220 + ch*4`.
const RAIL_BASE: u32 = 0x220;
/// `FUN_0ec5f2c0` range-checks its channel against 0x23 before passing it to
/// `FUN_0ec3007a`, and the boot reads every one of them.
const RAIL_CHANNELS: u32 = 0x24;

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
///
/// Channel 3's entry is the count the reference board reported at its own
/// operating point. In a machine it is overridden by the live core-rail
/// setpoint (see [`Avs::set_core_rail_uv`]); it stands only for an `Avs` that
/// nothing has told about a rail.
const CHANNEL_COUNTS: [u32; RESULT_CHANNELS as usize] = [752, 2, 669, 758, 2, 841];

/// `+0x220 + ch*4` counts, read one word at a time off `rpi-dev`. Channels
/// 0x20..0x23 really do report a count of 0 (the register reads `0x00010000`:
/// settled, nothing to report).
const RAIL_COUNTS: [u32; RAIL_CHANNELS as usize] = [
    0x07FD, 0x07BC, 0x0650, 0x043C, 0x0A83, 0x0A99, 0x084B, 0x05C2, // ch 0x00..0x07
    0x1ED6, 0x1E17, 0x184A, 0x1077, 0x15B1, 0x14BB, 0x10E7, 0x0B23, // ch 0x08..0x0F
    0x1590, 0x149C, 0x115E, 0x0B5D, 0x0CC4, 0x0C45, 0x05AD, 0x1658, // ch 0x10..0x17
    0x02B4, 0x0330, 0x0284, 0x02F1, 0x0258, 0x0303, 0x0223, 0x0224, // ch 0x18..0x1F
    0x0000, 0x0000, 0x0000, 0x0000, // ch 0x20..0x23
];

/// `FUN_0ecc6488`'s scale for channel 3: `(count * 100571) >> 13` tenths of a
/// millivolt. The inverse turns a rail voltage back into the count the sensor
/// would have to report for start4 to read that voltage.
const CH3_SCALE: u64 = 100_571;
const CH3_SHIFT: u32 = 13;
/// Channel 3 is the SoC core rail — see the module docs.
const CORE_CHANNEL: u32 = 3;

#[derive(Default)]
pub struct Avs {
    storage: BTreeMap<u32, u32>,
    /// Core-rail setpoint in microvolts, mirrored from the PMIC by
    /// [`crate::machine::Machine`] before every read of this block. `None`
    /// until the boot programs the rail, when the measured table value stands.
    core_uv: Option<u32>,
}

impl Avs {
    /// Tell the block what the core rail is actually set to. Channel 3 is a
    /// voltage sensor on that rail: its count has to move when the rail moves,
    /// or start4's DVFS calibration concludes the rail does not respond and
    /// abandons the whole scan (see the module docs).
    pub fn set_core_rail_uv(&mut self, uv: u32) {
        self.core_uv = Some(uv);
    }

    /// The count channel 3 must report for start4 to decode `uv`.
    fn core_count(uv: u32) -> u32 {
        // `uv / 100` is the firmware's unit (tenths of a millivolt).
        let tenths = u64::from(uv) / 100;
        ((tenths << CH3_SHIFT) / CH3_SCALE) as u32
    }
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
            let count = match (ch, self.core_uv) {
                (CORE_CHANNEL, Some(uv)) => Avs::core_count(uv),
                _ => CHANNEL_COUNTS[ch as usize],
            };
            return Ok(SETTLED | VALID | count);
        }
        if (RAIL_BASE..RAIL_BASE + RAIL_CHANNELS * 4).contains(&off) {
            let ch = (off - RAIL_BASE) / 4;
            return Ok(SETTLED | RAIL_COUNTS[ch as usize]);
        }
        Ok(self.storage.get(&off).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset & !3, value);
        Ok(())
    }
}
