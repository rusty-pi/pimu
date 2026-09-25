//! BCM2711 AVS monitor at `0x7D5D_2000`: the on-die temperature sensor and the
//! ring-oscillator / rail voltage monitors start4's DVFS code reads.
//!
//! Registers and fields: `specs/avs.toml`. `RESULT` and `RAIL` counts are
//! measured on a Raspberry Pi 4B d03115; pushed back through start4's own
//! conversion they land on what the firmware reports.
//!
//! Two things a reader cannot infer from the code:
//!
//! * `DISABLE_MASK` is an active-high *disable* mask, not a select — it reads 0
//!   on hardware while all six channels sample fine. `REG_040` / `REG_044` and
//!   the `LOWER` / `UPPER` bound tables are plain storage for the same reason:
//!   they read 0 while the channels work, and the bounds are start4's own
//!   output, not per-die calibration data.
//! * Channel 3 is a voltage sensor on the SoC core rail, and start4's DVFS
//!   calibration probes for that: it sets two targets ~56 mV apart and gives up
//!   if the readings barely differ, skipping the whole PVT characterisation. So
//!   [`crate::machine::Machine`] mirrors the PMIC setpoint into this block
//!   before every read, and channel 3 reports the count start4's decode turns
//!   back into that voltage. The calibration then writes the same 44 bounds a
//!   real board holds.
//!
//! `RAIL` is 36 channels wide, not 24, and channels `0x20..0x23` settle with a
//! count of exactly 0 — answering "not settled" there costs ten retries with a
//! `msleep(1000)` apiece and reports 0 V for rails that have a value.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

// start4 will not accept a `RESULT` sample without `SETTLED`, which Linux
// ignores: a stub returning 0 here stalls the boot.
use crate::spec::avs::{
    DISABLE_MASK, DISABLE_MASK_CHANNELS_MASK as DISABLE_MASK_BITS, LOWER, RAIL,
    RAIL_COUNT as RAIL_CHANNELS, RAIL_SETTLED_MASK, RAIL_STRIDE, REG_040, REG_044, REG_06C,
    REG_074, REG_078, RESULT, RESULT_COUNT as RESULT_CHANNELS, RESULT_SETTLED_MASK as SETTLED,
    RESULT_STRIDE, RESULT_VALID_MASK as VALID, UPPER,
};
use crate::spec::Coverage;

/// The channel results, the rail monitors and the disable mask are modelled;
/// the other masks and the bound tables are plain storage.
pub const COVERAGE: Coverage = Coverage {
    block: "avs",
    decoded: &[
        DISABLE_MASK,
        REG_040,
        REG_044,
        REG_06C,
        REG_074,
        REG_078,
        RESULT,
        RAIL,
        LOWER,
        UPPER,
    ],
};

/// `RESULT` counts, measured on a Raspberry Pi 4B d03115. Channel 0's 752
/// matches the 43.816 degC recorded from `thermal_zone0` on a warmed-up board;
/// channels 1 and 4 are unpopulated monitors and really read ~0. Channel 3 is
/// overridden by the live core-rail setpoint ([`Avs::set_core_rail_uv`]).
const CHANNEL_COUNTS: [u32; RESULT_CHANNELS as usize] = [752, 2, 669, 758, 2, 841];

/// `RAIL` counts, measured on a Raspberry Pi 4B d03115.
const RAIL_COUNTS: [u32; RAIL_CHANNELS as usize] = [
    0x07FD, 0x07BC, 0x0650, 0x043C, 0x0A83, 0x0A99, 0x084B, 0x05C2, // ch 0x00..0x07
    0x1ED6, 0x1E17, 0x184A, 0x1077, 0x15B1, 0x14BB, 0x10E7, 0x0B23, // ch 0x08..0x0F
    0x1590, 0x149C, 0x115E, 0x0B5D, 0x0CC4, 0x0C45, 0x05AD, 0x1658, // ch 0x10..0x17
    0x02B4, 0x0330, 0x0284, 0x02F1, 0x0258, 0x0303, 0x0223, 0x0224, // ch 0x18..0x1F
    0x0000, 0x0000, 0x0000, 0x0000, // ch 0x20..0x23
];

/// start4's scale for channel 3: `(count * 100571) >> 13` tenths of a
/// millivolt.
const CH3_SCALE: u64 = 100_571;
const CH3_SHIFT: u32 = 13;
/// Channel 3 is the SoC core rail.
const CORE_CHANNEL: u32 = 3;

#[derive(Default)]
pub struct Avs {
    storage: BTreeMap<u32, u32>,
    /// Core-rail setpoint in microvolts, mirrored from the PMIC. `None` until
    /// the boot programs the rail, when the measured table value stands.
    core_uv: Option<u32>,
}

impl Avs {
    /// Tell the block what the core rail is set to: channel 3's count has to
    /// move with the rail or start4 abandons its DVFS scan (module docs).
    pub fn set_core_rail_uv(&mut self, uv: u32) {
        self.core_uv = Some(uv);
    }

    /// The count channel 3 must report for start4 to decode `uv`.
    fn core_count(uv: u32) -> u32 {
        // Tenths of a millivolt, the firmware's unit.
        let tenths = u64::from(uv) / 100;
        ((tenths << CH3_SHIFT) / CH3_SCALE) as u32
    }
}

impl Avs {
    pub fn new() -> Avs {
        Avs::default()
    }

    /// Is channel `ch` masked off? Active high, low 7 bits only.
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
        if (RESULT..RESULT + RESULT_CHANNELS * RESULT_STRIDE).contains(&off) {
            let ch = (off - RESULT) / RESULT_STRIDE;
            if self.channel_masked(ch) {
                // Masked off: start4 reads "no valid, no settled" as no
                // sample.
                return Ok(0);
            }
            let count = match (ch, self.core_uv) {
                (CORE_CHANNEL, Some(uv)) => Avs::core_count(uv),
                _ => CHANNEL_COUNTS[ch as usize],
            };
            return Ok(SETTLED | VALID | count);
        }
        if (RAIL..RAIL + RAIL_CHANNELS * RAIL_STRIDE).contains(&off) {
            let ch = (off - RAIL) / RAIL_STRIDE;
            return Ok(RAIL_SETTLED_MASK | RAIL_COUNTS[ch as usize]);
        }
        Ok(self.storage.get(&off).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset & !3, value);
        Ok(())
    }
}
