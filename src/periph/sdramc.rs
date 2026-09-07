//! BCM2711 LPDDR4 memory controller + PHY (`0x7DC0_0000`, below the legacy
//! peripheral window).
//!
//! The EEPROM bootloader's `init_sdram_*` path (VC4 code in the `memsysNN.bin`
//! SDRAM-training firmware, decompressed from `pieeprom.bin`) drives this block
//! to bring LPDDR4 up. The model does not simulate DRAM electrically — our
//! "DRAM" is just flat, always-correct RAM — so every training / calibration
//! step is reported as immediately successful.
//!
//! Offsets are relative to `SDRAMC_BASE` (`0x7DC0_0000`); the controller sits at
//! `+0x2_0000` and the PHY / DDR-PLL blocks from `+0x2_0240` upward and at
//! `+0x3_0000` / `+0x3_4000` / `+0x3_8000`.
//!
//! Register conventions observed via `--trace-mmio`:
//!
//! * **`+0x2_0010` STATUS / `+0x2_0014` CMD** — the controller command port. A
//!   command word (`0x5501_0000`) written to CMD retires immediately; STATUS
//!   bit 0 then reads set (`0x80003898` polls `btest [+0x10], #0`).
//! * **`+0x3_2010` trigger / `+0x3_2014` busy** — a PHY calibration handshake.
//!   The firmware writes 1 to the trigger, waits for busy to read a *stable
//!   non-zero* value 10 times running (`0x8000685e`), then writes 0 to the
//!   trigger and waits for busy to return to 0. We mirror the trigger: busy
//!   reads 1 while trigger != 0, else 0.
//! * Everything else is sticky (read-after-write), default 0.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

/// Controller command/status (offset from `SDRAMC_BASE`).
const CTRL_STATUS: u32 = 0x2_0010;
const CTRL_CMD: u32 = 0x2_0014;
/// `STATUS` bit 0 = "last command complete".
const CTRL_DONE: u32 = 1 << 0;

/// PHY calibration handshake.
const PHY_CAL_TRIGGER: u32 = 0x3_2010;
const PHY_CAL_BUSY: u32 = 0x3_2014;

#[derive(Debug, Clone)]
pub struct SdramcAccess {
    pub offset: u32,
    pub width: Width,
    pub value: u32,
    pub write: bool,
}

pub struct Sdramc {
    storage: BTreeMap<u32, u32>,
    /// Whether the most recent controller `CMD` has "completed" (always true in
    /// the model; a fresh write re-arms the rising edge a poll expects).
    cmd_done: bool,
    pub log: Vec<SdramcAccess>,
    pub log_limit: usize,
}

impl Default for Sdramc {
    fn default() -> Self {
        Sdramc::new()
    }
}

impl Sdramc {
    pub fn new() -> Sdramc {
        Sdramc {
            storage: BTreeMap::new(),
            cmd_done: true,
            log: Vec::new(),
            log_limit: 8192,
        }
    }

    fn record(&mut self, a: SdramcAccess) {
        if self.log.len() < self.log_limit {
            self.log.push(a);
        }
    }
}

impl MmioDevice for Sdramc {
    fn name(&self) -> &'static str {
        "sdramc"
    }

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32> {
        let value = match offset & !3 {
            CTRL_STATUS => {
                let base = self.storage.get(&CTRL_STATUS).copied().unwrap_or(0);
                if self.cmd_done {
                    base | CTRL_DONE
                } else {
                    base & !CTRL_DONE
                }
            }
            PHY_CAL_BUSY => {
                // Busy tracks the trigger: 1 while a calibration is "running",
                // 0 once the firmware clears the trigger.
                u32::from(self.storage.get(&PHY_CAL_TRIGGER).copied().unwrap_or(0) != 0)
            }
            off => self.storage.get(&off).copied().unwrap_or(0),
        };
        self.record(SdramcAccess {
            offset,
            width,
            value,
            write: false,
        });
        Ok(value)
    }

    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()> {
        match offset & !3 {
            CTRL_CMD => {
                self.storage.insert(CTRL_CMD, value);
                self.cmd_done = true;
            }
            off => {
                self.storage.insert(off, value);
            }
        }
        self.record(SdramcAccess {
            offset,
            width,
            value,
            write: true,
        });
        Ok(())
    }
}
