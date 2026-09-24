//! The USB reset block at `0x7E80_8000`, as far as USB power goes through it
//! (#49).
//!
//! The block names itself: every reserved word of its window answers
//! `0x55534252`, `"USBR"` big-endian, measured on a Raspberry Pi 4B d03115, the
//! way `corectl` answers `INTE` and `mcsync` answers `MULT`. This module used
//! to be called `hd`, on the strength of the BCM2835 having its HDMI `HD` block
//! at this address. The BCM2711 does not: its HDMI `hd` range is `0x7EF20000`,
//! which the board's own device tree gives and which answers a different tag
//! (#139). Registers `+0x00` to `+0x20` read back; `+0x24` upwards answers the
//! tag, which means not readable rather than not implemented -- `GPSET` answers
//! the GPIO tag and still drives pins -- so a write-only register may live up
//! there. start4's HDMI code writes `+0x2C` (#142).
//!
//! ## What start4 does with it
//!
//! `SET_POWER_STATE` for USB (device 3) is handled at `0x3ED89520`. On BCM2711
//! it asks this block for power and waits for the acknowledge before it touches
//! the DWC2 controller ([`super::dwc2`]):
//!
//! ```c
//! [0x7E808008] |= 4;                     // CTRL.POWER
//! while (([0x7E808020] & 3) != 3) { }    // STATUS.ACK, no timeout
//! [0x7E980088] |= 0x30000;
//! // ...then the DWC2 core reset and FIFO flushes
//! ```
//!
//! Left on the catch-all stub, `+0x20` read 0 because nothing writes it, and
//! the handler spun there for good. Every property request after it went
//! unanswered. In the rpi-mkosi image, UEFI's `DwUsbHostDxe` asks for USB
//! power soon after the banner, so UEFI then spent a second on each later
//! request and looked hung. `0x3EC607B0` and `0x3ECACB0C` make the same
//! request on other paths.
//!
//! ## Ground truth
//!
//! Read on a Raspberry Pi 4B d03115 (our pinned start4, Linux idle) through
//! `/dev/mem`, before and after asking the firmware with `vcmailbox`:
//!
//! ```text
//!                                  +0x08 CTRL   +0x20 STATUS
//!   no USB power request yet       0x3          0x0
//!   SET_POWER_STATE(USB, on)       0x7          0x3    (reply: on)
//!   SET_POWER_STATE(USB, off)      0x7          0x3    (reply: off)
//! ```
//!
//! The acknowledge follows the request, and the off path never comes back here.
//! The model derives `STATUS` from `CTRL.POWER` and seeds `CTRL` with the idle
//! value. Bits 0 and 1 of `CTRL` stay plain storage: `0x3ECACB0C` clears them
//! after setting `POWER` and still only waits for `ACK` bit 0.
//!
//! ## What the block is
//!
//! Not settled. BCM2835 has its HDMI "HD" block at this address, and start4's
//! HDMI code writes `+0x2C` and `+0x38` here, but BCM2711's Linux binding puts
//! its "hd" range at `0x7EF2_0000`. The rest of the window is plain storage.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::usbr::{CTRL, CTRL_POWER_MASK, CTRL_RESET, STATUS, STATUS_ACK_MASK};
use crate::spec::Coverage;

/// The power request and its acknowledge; the rest of the window is storage.
pub const COVERAGE: Coverage = Coverage {
    block: "usbr",
    decoded: &[CTRL, STATUS],
};

pub struct Usbr {
    storage: BTreeMap<u32, u32>,
}

impl Default for Usbr {
    fn default() -> Usbr {
        Usbr {
            storage: BTreeMap::from([(CTRL, CTRL_RESET)]),
        }
    }
}

impl Usbr {
    pub fn new() -> Usbr {
        Usbr::default()
    }
}

impl MmioDevice for Usbr {
    fn name(&self) -> &'static str {
        "hd"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        if off == STATUS {
            let ctrl = self.storage.get(&CTRL).copied().unwrap_or(0);
            return Ok(if ctrl & CTRL_POWER_MASK != 0 {
                STATUS_ACK_MASK
            } else {
                0
            });
        }
        Ok(self.storage.get(&off).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        if off != STATUS {
            self.storage.insert(off, value);
        }
        Ok(())
    }
}
