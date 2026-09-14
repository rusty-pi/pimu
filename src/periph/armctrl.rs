//! The ARM control block at `0x7E00_B000` as the VPU sees it — the part below
//! the mailboxes (`0x7E00_B880`, [`crate::periph::mbox`]).
//!
//! What matters here is how the firmware lets the ARM cores out of reset.
//! Traced on the pinned firmware (`RVF_TRACE_MMIO=7e00b000-7e101000 boot
//! …`, #40): the whole boot touches this block only a handful of times, and
//! the writes right after `arm_loader: Starting ARM with 948MB` are
//!
//! ```text
//!   0x7E00_B41C <- 0x0000_000A
//!   0x7E00_B008 <- 0x0000_3030
//!   0x7E00_B000 <- 0x0000_1000        (earlier in the boot: 0x0000_0200)
//! ```
//!
//! all from start4's MMIO write helper at `0xFEC0_043A`, with nothing but
//! power-management housekeeping after them. The last one is the release:
//! the model takes a write to `+0x000` with bit 12 set as "start the ARM",
//! and the ARM's reset state (PC 0, EL3) is where the armstub
//! [`crate::armstub`] describes begins. What the other bits of these
//! registers do is not known; they are kept as plain storage.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::armctrl::{CONTROL, CONTROL_RELEASE_MASK as CONTROL_RELEASE, REG_008, REG_41C};
use crate::spec::Coverage;

/// Everything is storage apart from the release bit.
pub const COVERAGE: Coverage = Coverage {
    block: "armctrl",
    decoded: &[CONTROL, REG_008, REG_41C],
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

    /// Has the firmware released the ARM since the last call?
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
        c.write(REG_41C, Width::Word, 0xA).unwrap();
        c.write(REG_008, Width::Word, 0x3030).unwrap();
        c.write(CONTROL, Width::Word, 0x1000).unwrap();
        assert!(c.take_release());
        assert!(!c.take_release());
        assert_eq!(c.read(REG_008, Width::Word).unwrap(), 0x3030);
    }
}
