//! BCM2711 EMMC2 — the SD Host Controller (SDHCI v3) the main bootloader uses
//! once it picks "Boot mode: SD". Register block at `0x7E34_0000`.
//!
//! **Stub only, on purpose.** This models just the clock / reset /
//! present-state plumbing so SD init gets to `SD HOST: ... 390625 HZ`. It does
//! *not* dispatch commands or back a card — the driver then sends CMD0 and
//! spins forever waiting for command-complete. A full SD stack (SDHCI command
//! engine + an SD card state machine + a FAT image with a real `start4.elf`)
//! is deferred; see the `bootloader-stage-peripherals` note. The alternative
//! path to `start4.elf` is to load it into DRAM directly and jump, skipping
//! the boot-media read entirely.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

// SDHCI register offsets (byte), each a 32-bit word.
const CLOCK_CONTROL: u32 = 0x2C; // TIMEOUT/SRST in the high half
const INT_STATUS: u32 = 0x30;
const PRESENT_STATE: u32 = 0x24;
const CAPABILITIES_0: u32 = 0x40;
const CAPABILITIES_1: u32 = 0x44;
const CONTROLLER_VERSION: u32 = 0xFC;

/// CLOCK_CONTROL (low 16 bits of `0x2C`): internal-clock enable / stable.
const CLK_INTLEN: u32 = 1 << 0;
const CLK_STABLE: u32 = 1 << 1;
/// Software-reset bits (`0x2C` bits 24..26) — self-clearing in the model.
const SRST_MASK: u32 = 0x0700_0000;

/// PRESENT_STATE: a stable, inserted, idle card.
const PRESENT_STATE_IDLE: u32 = (1 << 16) | (1 << 17) | (1 << 20) | (1 << 24);

#[derive(Default)]
pub struct Emmc2 {
    /// Sticky storage for offsets without special behaviour.
    reg: BTreeMap<u32, u32>,
}

impl Emmc2 {
    pub fn new() -> Emmc2 {
        Emmc2::default()
    }

    fn get(&self, off: u32) -> u32 {
        self.reg.get(&off).copied().unwrap_or(0)
    }
}

impl MmioDevice for Emmc2 {
    fn name(&self) -> &'static str {
        "emmc2"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        let v = match off {
            CLOCK_CONTROL => {
                let clk = self.get(CLOCK_CONTROL) & 0xFFFF;
                // Internal clock reports stable as soon as it is enabled; the
                // software-reset bits (high half) always read back done.
                if clk & CLK_INTLEN != 0 {
                    clk | CLK_STABLE
                } else {
                    clk
                }
            }
            PRESENT_STATE => PRESENT_STATE_IDLE,
            // v3 host, base clock 100 MHz, 3.3 V, high-speed, SDMA.
            CAPABILITIES_0 => (100 << 8) | (1 << 21) | (1 << 22) | (1 << 24) | (1 << 25),
            CAPABILITIES_1 => 0,
            CONTROLLER_VERSION => 0x0002,
            _ => self.get(off),
        };
        Ok(v)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        match off {
            CLOCK_CONTROL => {
                self.reg.insert(CLOCK_CONTROL, value & !SRST_MASK);
            }
            INT_STATUS => {
                let cur = self.get(INT_STATUS);
                self.reg.insert(INT_STATUS, cur & !value); // write-1-to-clear
            }
            PRESENT_STATE | CAPABILITIES_0 | CAPABILITIES_1 | CONTROLLER_VERSION => {}
            _ => {
                self.reg.insert(off, value);
            }
        }
        Ok(())
    }
}
