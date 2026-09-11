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

use alloc::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

/// `SCALER_DISPID` — a read-only identification register. start4 gates its
/// whole display bring-up on it: `0x3EC945CC` reads `[0x7E40_0008]`, compares
/// it against `0x64647276` and returns -1 immediately if it does not match,
/// which skips `hdmi_init` and therefore the HDMI provider registration at
/// `0x3ECEC626` (issue #13). Returning 0 made the model look like a chip with
/// no HVS at all.
///
/// The value is read off a running Pi 4, not guessed:
/// `/sys/kernel/debug/dri/0/hvs_regs` reports `SCALER_DISPID = 0x64647276`.
const DISPID: u32 = 0x08;
const DISPID_VALUE: u32 = 0x6464_7276;

/// First "requested frame" slot; four channels at stride 4.
const REQUESTED_BASE: u32 = 0x20;
/// First "current frame" slot, mirrored back from `REQUESTED_BASE`.
const CURRENT_BASE: u32 = 0x30;
const CHANNELS: u32 = 4;

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
        if (CURRENT_BASE..CURRENT_BASE + 4 * CHANNELS).contains(&off) {
            // Scanout instantly caught up to the requested frame.
            let requested = REQUESTED_BASE + (off - CURRENT_BASE);
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
