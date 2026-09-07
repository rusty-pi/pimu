//! BCM2711 legacy SDRAM-controller register interface (`0x7E00_1000`).
//!
//! After the LPDDR4 PHY is trained through the `0x7DC0_0000` block, the EEPROM
//! bootloader programs a table of DRAM timing words here (`+0x00..+0x30`,
//! e.g. `0x061a0474`, `0x11013110`, ... — packed tRAS/tRC/tRCD/tRFC/… fields;
//! `0x80006380` prints one back as `SD_SB %08x`) and then waits, in a block of
//! sub-controllers based at `+0x80`, for a lock/ready bit to come up:
//! `0x8000a3e0` polls `[+0x9C] & 0x8000_0000` ten times with 1 ms sleeps and
//! reports "block device timeout" if it never sets.
//!
//! We do not model the DRAM clock tree — timing words read straight back, and
//! every `+0x9C`-style status word reports its top bit (ready) set.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

/// Status word the firmware polls for a lock/ready edge (`0x8000a3e0` reads
/// `[0x7E00_1080 + 0x1C]`). Bit 31 is "ready"; we always report it set. The
/// sub-controllers are 0x80 apart, so `…9C`, `…11C`, … are all status slots.
const STATUS_SLOT: u32 = 0x1C;
const STATUS_STRIDE: u32 = 0x80;
const STATUS_READY: u32 = 1 << 31;

#[derive(Default)]
pub struct Sdc {
    storage: BTreeMap<u32, u32>,
}

impl Sdc {
    pub fn new() -> Sdc {
        Sdc::default()
    }
}

impl MmioDevice for Sdc {
    fn name(&self) -> &'static str {
        "sdc"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        let stored = self.storage.get(&off).copied().unwrap_or(0);
        // Report the ready bit for every sub-controller status slot (`+0x9C`,
        // `+0x11C`, …); the plain timing-table words at `+0x00..+0x30` read back
        // whatever was written.
        if off >= STATUS_STRIDE && off % STATUS_STRIDE == STATUS_SLOT {
            return Ok(stored | STATUS_READY);
        }
        Ok(stored)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset & !3, value);
        Ok(())
    }
}
