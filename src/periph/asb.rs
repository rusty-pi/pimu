//! ASB — the AXI async slave bridges at `0x7E00_A000`.
//!
//! Registers and fields: `specs/asb.toml`. Each video block (V3D, ISP, H264)
//! sits behind a pair of bridges that have to be stopped and acknowledge before
//! its power domain is gated, and released and acknowledge before it comes
//! back. That handshake is the whole content of the block.
//!
//! `ACK` follows `REQ_STOP`, immediately: there is no queued AXI traffic to
//! drain, and both drivers only poll. Beware the Ghidra rendering of start4's
//! power-domain switch — it sinks the stores below the loops, so it reads as a
//! poll for `ACK` *before* the request, which no mirroring can satisfy in both
//! directions.
//!
//! Left at defaults deliberately: `EMPTY` reads 1 and `FULL` 0 (nothing issues
//! transactions through these bridges), `BRDG_VERSION` / `CPR_CTRL` are plain
//! storage reading 0, and every bridge comes up running. Linux ORs
//! `PM_PASSWORD` into each write and start4 does not; both are accepted.
//!
//! The bridges gate nothing — there is no V3D / ISP / H264 block behind them —
//! and the BCM2711's second instance (`rpivid_asb`, which Linux uses for V3D)
//! stays unmapped.

use crate::bus::{BusResult, MmioDevice, Width};

// `CTRL[0..6]` are Linux's `ASB_V3D_S_CTRL` .. `ASB_H264_M_CTRL`.
use crate::spec::asb::{
    AXI_BRDG_ID, AXI_BRDG_ID_RESET as BRDG_ID, BRDG_VERSION, CPR_CTRL, CTRL, CTRL_ACK_MASK as ACK,
    CTRL_COUNT, CTRL_EMPTY_MASK as EMPTY, CTRL_REQ_STOP_MASK as REQ_STOP, CTRL_STRIDE,
};
use crate::spec::Coverage;

/// Every register in `specs/asb.toml` is modelled.
pub const COVERAGE: Coverage = Coverage {
    block: "asb",
    decoded: &[BRDG_VERSION, CPR_CTRL, CTRL, AXI_BRDG_ID],
};

/// Number of `*_CTRL` registers: three blocks, slave and master port each.
const BRIDGES: usize = CTRL_COUNT as usize;

#[derive(Default)]
pub struct Asb {
    /// Per-bridge `REQ_STOP`. `ACK` is derived from it, never stored.
    stopped: [bool; BRIDGES],
    /// `BRDG_VERSION` / `CPR_CTRL`, plain storage.
    version: u32,
    cpr_ctrl: u32,
}

impl Asb {
    pub fn new() -> Asb {
        Asb::default()
    }

    fn bridge(offset: u32) -> Option<usize> {
        let rel = offset.checked_sub(CTRL)?;
        (rel % CTRL_STRIDE == 0 && rel / CTRL_STRIDE < CTRL_COUNT)
            .then_some((rel / CTRL_STRIDE) as usize)
    }

    /// A bridge's status word: `ACK` mirrors `REQ_STOP`, the queue is empty.
    fn ctrl(&self, i: usize) -> u32 {
        let mut v = EMPTY;
        if self.stopped[i] {
            v |= REQ_STOP | ACK;
        }
        v
    }
}

impl MmioDevice for Asb {
    fn name(&self) -> &'static str {
        "asb"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        if let Some(i) = Asb::bridge(offset) {
            return Ok(self.ctrl(i));
        }
        Ok(match offset {
            BRDG_VERSION => self.version,
            CPR_CTRL => self.cpr_ctrl,
            AXI_BRDG_ID => BRDG_ID,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        if let Some(i) = Asb::bridge(offset) {
            // `REQ_STOP` is the only writable bit; `PM_PASSWORD` lands in the
            // ignored top bits.
            self.stopped[i] = value & REQ_STOP != 0;
            return Ok(());
        }
        match offset {
            BRDG_VERSION => self.version = value,
            CPR_CTRL => self.cpr_ctrl = value,
            _ => {}
        }
        Ok(())
    }
}
