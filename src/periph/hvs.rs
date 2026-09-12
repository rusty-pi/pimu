//! BCM2711 HVS (Hardware Video Scaler) register block at `0x7E40_0000`.
//!
//! The main bootloader brings up a diagnostic display and, after queuing a
//! frame, calls a "channel swap" wait (`0x0008adc0`): it polls a pair of
//! per-channel words and spins — 100 × `delay(1000µs)` — until
//!
//! ```text
//!   (*current & 0xFFFF) == (*requested & 0xFFF)
//! ```
//!
//! `requested` lives at `+0x20 + 4*chan`, `current` at `+0x30 + 4*chan`. Real
//! hardware advances `current` to `requested` when the scanout hits the queued
//! frame; we have no display, so without help every swap runs the full timeout
//! (~0.5 s of modelled time each, hundreds of times) and the boot crawls.
//!
//! Model: reads of the `current` slot return whatever was last written to the
//! matching `requested` slot, so the swap always reports complete on the first
//! poll. Everything else is plain sticky storage.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::hvs::{
    CURRENT, CURRENT_COUNT, CURRENT_STRIDE, DISPID, DISPID_RESET as DISPID_VALUE, REQUESTED,
};
use crate::spec::Coverage;

/// `DISPID` answers the measured id — start4 gates its whole display bring-up
/// on it, and 0 read as "no HVS" (issue #13) — and the frame-swap words are
/// modelled; the rest of the block is storage.
pub const COVERAGE: Coverage = Coverage {
    block: "hvs",
    decoded: &[DISPID, REQUESTED, CURRENT],
};

#[derive(Default)]
pub struct Hvs {
    storage: BTreeMap<u32, u32>,
}

impl Hvs {
    pub fn new() -> Hvs {
        Hvs::default()
    }
}

impl MmioDevice for Hvs {
    fn name(&self) -> &'static str {
        "hvs"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        if off == DISPID {
            return Ok(DISPID_VALUE);
        }
        if (CURRENT..CURRENT + CURRENT_COUNT * CURRENT_STRIDE).contains(&off) {
            // Scanout instantly caught up to the requested frame.
            let requested = REQUESTED + (off - CURRENT);
            return Ok(self.storage.get(&requested).copied().unwrap_or(0));
        }
        Ok(self.storage.get(&off).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        if off == DISPID {
            // Read-only.
            return Ok(());
        }
        self.storage.insert(off, value);
        Ok(())
    }
}
