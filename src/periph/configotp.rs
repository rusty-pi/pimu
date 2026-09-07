//! The always-on config / OTP engine at `0x7E20_F000`.
//!
//! The EEPROM bootloader's `getconfig(key)` path reads board identity through
//! this block:
//!
//! ```text
//!   write key            -> +0x1C
//!   write 0, 0           -> +0x0C, +0x08     (transaction params)
//!   set  +0x08 |= 1                          (trigger)
//!   poll +0x10 bit 0                         (done)
//!   read value           <- +0x18
//! ```
//!
//! The same block also takes some clock-mux pokes at `+0x04` (values 3/0/2)
//! which we just absorb. Anything we don't recognise keeps the old "always
//! ready" status bits so unrelated pollers still make progress.
//!
//! We do not model real OTP fuses — [`ConfigOtp::table`] is a small map from
//! key to value, seeded with a plausible Raspberry Pi 4 Model B identity. The
//! values only need to be self-consistent across firmware versions for the
//! `rpi-machine-id` regression to be meaningful.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

const REG_CLKMUX: u32 = 0x04;
const REG_PARAM_A: u32 = 0x08;
const REG_STATUS: u32 = 0x10;
const REG_DATA: u32 = 0x18;
const REG_KEY: u32 = 0x1C;

/// `+0x08` bit 0 kicks off a transaction; `+0x10` **bit 1** reports completion
/// (the poll at `0x8000760e` is `btest [+0x10], #1`).
const GO: u32 = 1 << 0;
const DONE: u32 = 1 << 1;

/// Status bits unrelated firmware paths poll for on this block.
const READY: u32 = (1 << 17) | (1 << 18) | (1 << 7);

pub struct ConfigOtp {
    storage: BTreeMap<u32, u32>,
    /// Key latched via `+0x1C`, resolved on the next triggered transaction.
    key: u32,
    /// Result presented at `+0x18`.
    data: u32,
    done: bool,
    /// key -> config value.
    table: BTreeMap<u32, u32>,
}

impl Default for ConfigOtp {
    fn default() -> Self {
        ConfigOtp::new()
    }
}

impl ConfigOtp {
    pub fn new() -> ConfigOtp {
        let mut table = BTreeMap::new();
        // Raspberry Pi 4 Model B, 8 GB, rev 1.5 — matches the board revision
        // code seen on real hardware (`examples-on-real-hardware/early-boot.log`:
        // "board: boardrev d03115 otp d03115").
        table.insert(30, 0x00D0_3115); // board revision
        table.insert(28, 0x1AA2_BB31); // board serial (arbitrary but fixed)
        ConfigOtp {
            storage: BTreeMap::new(),
            key: 0,
            data: 0,
            done: false,
            table,
        }
    }

    /// Override / extend the config table (e.g. from a scenario spec).
    pub fn set(&mut self, key: u32, value: u32) {
        self.table.insert(key, value);
    }

    fn resolve(&mut self) {
        self.data = self.table.get(&self.key).copied().unwrap_or(0);
        self.done = true;
    }
}

impl MmioDevice for ConfigOtp {
    fn name(&self) -> &'static str {
        "config-otp"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset & !3 {
            REG_STATUS => {
                if self.done {
                    DONE
                } else {
                    0
                }
            }
            REG_DATA => self.data,
            REG_KEY => self.key,
            off => self.storage.get(&off).copied().unwrap_or(READY),
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        match offset & !3 {
            REG_KEY => {
                self.key = value;
                self.done = false;
            }
            REG_PARAM_A => {
                self.storage.insert(REG_PARAM_A, value);
                if value & GO != 0 {
                    self.resolve();
                }
            }
            REG_STATUS => {
                // write-1-to-clear the done latch
                if value & DONE != 0 {
                    self.done = false;
                }
            }
            REG_CLKMUX => {
                self.storage.insert(REG_CLKMUX, value);
            }
            off => {
                self.storage.insert(off, value);
            }
        }
        Ok(())
    }
}
