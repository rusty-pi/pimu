//! The ARM control block at `0x7E00_B000` as the VPU sees it — the part below
//! the mailboxes (`0x7E00_B880`, [`crate::periph::mbox`]).
//!
//! Registers and fields: `specs/armctrl.toml` ([`crate::spec::armctrl`]).
//!
//! What matters here is how the firmware lets the ARM cores out of reset. A
//! traced boot touches the block only a handful of times, and the release is a
//! write to `+0x000` with bit 12 set, right after `arm_loader: Starting ARM`;
//! the cores then start from their reset state (PC 0, EL3) in the armstub
//! [`crate::armstub`] describes. What the other bits do is not known, so they
//! are plain storage.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::armctrl::{
    CONTROL, CONTROL_RELEASE_MASK as CONTROL_RELEASE, REG_008, TIMER_PREDIV,
};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "armctrl",
    decoded: &[CONTROL, REG_008, TIMER_PREDIV],
};

#[derive(Default)]
pub struct ArmCtrl {
    regs: BTreeMap<u32, u32>,
    release: bool,
    released: bool,
}

impl ArmCtrl {
    pub fn new() -> ArmCtrl {
        ArmCtrl::default()
    }

    pub fn take_release(&mut self) -> bool {
        std::mem::take(&mut self.release)
    }

    /// Has the firmware ever released the ARM? True whether or not the ARM is
    /// modelled: this is where a firmware boot hands over.
    pub fn released(&self) -> bool {
        self.released
    }
}

impl MmioDevice for ArmCtrl {
    fn name(&self) -> &'static str {
        "armctrl"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(self.regs.get(&(offset & !3)).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let reg = offset & !3;
        if reg == CONTROL && value & CONTROL_RELEASE != 0 {
            self.release = true;
            self.released = true;
        }
        self.regs.insert(reg, value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arm_loader_write_releases_the_arm_once() {
        let mut c = ArmCtrl::new();
        c.write(0, Width::Word, 0x200).unwrap();
        assert!(!c.take_release());
        c.write(TIMER_PREDIV, Width::Word, 0xA).unwrap();
        c.write(REG_008, Width::Word, 0x3030).unwrap();
        c.write(CONTROL, Width::Word, 0x1000).unwrap();
        assert!(c.take_release());
        assert!(!c.take_release());
        assert_eq!(c.read(REG_008, Width::Word).unwrap(), 0x3030);
    }
}
