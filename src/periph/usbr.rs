//! The USB reset block at `0x7E80_8000`, as far as USB power goes through it.
//!
//! Registers and fields: `specs/usbr.toml` ([`crate::spec::usbr`]).
//!
//! The block names itself: every reserved word of its window answers
//! `0x55534252`, `"USBR"` big-endian, on a Raspberry Pi 4B d03115 — the way
//! [`super::corectl`] answers `INTE` and [`super::mcsync`] answers `MULT`. It
//! is not the BCM2835's HDMI `HD` block, which sits at the same address on
//! that chip: the BCM2711's HDMI `hd` range is `0x7EF2_0000`, per the board's
//! own device tree, and answers a different tag.
//!
//! start4's `SET_POWER_STATE` for USB asks this block for power and spins on
//! `STATUS.ACK` — with no timeout — before it touches the DWC2 controller
//! ([`super::dwc2`]). A `STATUS` that never acknowledges therefore parks the
//! mailbox thread for good and leaves every property request behind it
//! unanswered, which is what UEFI's `DwUsbHostDxe` runs into: it asks for USB
//! power early.
//!
//! So the model derives `STATUS` from `CTRL.POWER` and seeds `CTRL` with the
//! measured idle value. `CTRL` bits 0 and 1 stay plain storage: start4 clears
//! them after setting `POWER` and still waits only for `ACK`.
//!
//! Above `+0x20` the window is write-only: a read answers the tag, as it does
//! on the board, and a write lands in plain storage. start4's HDMI driver owns
//! two of those words -- `HDMI_SM_DIV` and `HDMI_CTL` -- and reaches neither
//! without a mode set, which no boot performs (#142).

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::usbr::{
    CTRL, CTRL_POWER_MASK, CTRL_RESET, HDMI_CTL, HDMI_CTL_RESET, HDMI_SM_DIV, STATUS,
    STATUS_ACK_MASK,
};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "usbr",
    decoded: &[CTRL, STATUS, HDMI_SM_DIV, HDMI_CTL],
};

/// The first offset whose read answers the block tag instead of a register
/// (module docs).
const TAGGED_FROM: u32 = 0x24;

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
        if off >= TAGGED_FROM {
            return Ok(HDMI_CTL_RESET);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The HDMI driver's two words read back the block tag, as on the board,
    /// and keep the write the driver made invisible.
    #[test]
    fn the_write_only_words_read_back_the_block_tag() {
        let mut usbr = Usbr::new();
        assert_eq!(&HDMI_CTL_RESET.to_be_bytes(), b"USBR");
        for off in [HDMI_SM_DIV, HDMI_CTL, 0x24, 0xFC] {
            assert_eq!(usbr.read(off, Width::Word).unwrap(), HDMI_CTL_RESET);
            usbr.write(off, Width::Word, 0x1234_5678).unwrap();
            assert_eq!(usbr.read(off, Width::Word).unwrap(), HDMI_CTL_RESET);
        }
    }

    /// Everything below the tag still reads what was written.
    #[test]
    fn the_registers_below_the_tag_are_untouched() {
        let mut usbr = Usbr::new();
        assert_eq!(usbr.read(CTRL, Width::Word).unwrap(), CTRL_RESET);
        assert_eq!(usbr.read(STATUS, Width::Word).unwrap(), 0);
        usbr.write(CTRL, Width::Word, CTRL_RESET | CTRL_POWER_MASK)
            .unwrap();
        assert_eq!(usbr.read(STATUS, Width::Word).unwrap(), STATUS_ACK_MASK);
    }
}
