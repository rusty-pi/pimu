//! BCM2711 LPDDR4 memory controller + PHY (`0x7DC0_0000`, below the legacy
//! peripheral window).
//!
//! The EEPROM bootloader's `init_sdram_*` path (VC4 in the bootcode, driven by
//! the `memsysNN.bin` PHY-preset tables decompressed from `pieeprom.bin`) drives
//! this block to bring LPDDR4 up. The model does not simulate DRAM electrically
//! — our "DRAM" is flat, always-correct RAM — so every training / calibration
//! step is reported as immediately successful with the values the firmware's
//! self-consistency checks expect.
//!
//! Offsets are relative to `SDRAMC_BASE` (`0x7DC0_0000`); the controller sits at
//! `+0x2_0000`, PHY register arrays at `+0x2_8000` (4096 words) and `+0x3_4000`
//! (1024 words), and the small calibration-result block at `+0x3_2100`.
//!
//! Register conventions (from `--trace-mmio` + tracing the memsys verifier):
//!
//! * **`+0x2_0010` STATUS / `+0x2_0014` CMD** — controller command port. A
//!   command word (`0x5501_0000`) written to CMD retires at once; STATUS bit 0
//!   then reads set (`0x80003898` polls `btest [+0x10], #0`).
//! * **`+0x3_2010` trigger / `+0x3_2014` busy** — PHY calibration handshake.
//!   The firmware writes 1 to the trigger, waits for busy to read a stable
//!   non-zero value 10× (`0x8000685e`), writes 0 back, waits for busy = 0.
//! * **`+0x3_2100..+0x3_2118`** — calibration-result block, 6 data words +
//!   trailing sums. The firmware seeds `[+0x00]=1 [+0x04]=<lane mask>` (`0x101`
//!   or `0x203`) `[+0x08]=0`, writes their sum to `[+0x0C]`, runs the
//!   calibration, then verifies (`0x800068ce` / `0x800066a6`):
//!     - a running sum of the leading words == a trailing sum word
//!       (`sum([+0x00..+0x10])` == `[+0x14]`, or `sum([+0x00..+0x14])` == `[+0x18]`)
//!     - `[+0x0C]` == the memsys blob's magic word `0x0223_0000`
//!     - `([+0x10] >> 8) & 0xFF` == an expected byte (0 here)
//!   So a completed calibration leaves `[+0x0C]` = the PHY signature, `[+0x10]`
//!   = 0, and `[+0x14]` / `[+0x18]` = the running sums.
//! * Everything else is sticky (read-after-write), default 0. The PHY preset
//!   arrays the firmware copies in from `memsysNN.bin` and sum-checks land here
//!   and read straight back, so those checks pass unchanged.

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

/// PHY calibration-result block.
const PHY_RES_BASE: u32 = 0x3_2100;
const PHY_RES_SIGNATURE: u32 = PHY_RES_BASE + 0x0C;
/// The two trailing running-sum words a completed calibration fills in.
const PHY_RES_SUM5: u32 = PHY_RES_BASE + 0x14; // sum of words [0..5)
const PHY_RES_SUM6: u32 = PHY_RES_BASE + 0x18; // sum of words [0..6)

/// Value a completed calibration leaves in `[+0x3_210C]`. All of `memsys00.bin`
/// .. `memsys08.bin` carry this as their header word and the verifier compares
/// the two — it is a fixed PHY-block signature, not a per-preset hash.
const PHY_SIGNATURE: u32 = 0x0223_0000;

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
    /// Set once a PHY calibration has been triggered (`PHY_CAL_TRIGGER` written
    /// non-zero) since the result block was last seeded; makes the signature
    /// register report a completed calibration.
    cal_ran: bool,
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
            cal_ran: false,
            log: Vec::new(),
            log_limit: 8192,
        }
    }

    fn record(&mut self, a: SdramcAccess) {
        if self.log.len() < self.log_limit {
            self.log.push(a);
        }
    }

    /// The value a read of one calibration-result word yields, after applying
    /// the "calibration has run" fixups. `+0x0C` becomes the PHY signature;
    /// `+0x14` / `+0x18` become the running sum of every result word before
    /// them, so the firmware's "sum the leading words, compare the trailing
    /// word" checks balance (`0x800066a6`, `0x800068ce`).
    fn result_word(&self, off: u32) -> u32 {
        let stored = |o: u32| self.storage.get(&o).copied().unwrap_or(0);
        // The 5 data words, with `+0x0C` replaced by the signature post-cal.
        let data = |i: u32| {
            let o = PHY_RES_BASE + i * 4;
            if o == PHY_RES_SIGNATURE && self.cal_ran {
                PHY_SIGNATURE
            } else {
                stored(o)
            }
        };
        let sum = |n: u32| (0..n).map(data).fold(0u32, u32::wrapping_add);
        match off {
            PHY_RES_SIGNATURE if self.cal_ran => PHY_SIGNATURE,
            PHY_RES_SUM5 if self.cal_ran => sum(5),
            PHY_RES_SUM6 if self.cal_ran => sum(5).wrapping_add(sum(5)),
            _ => stored(off),
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
            off @ (PHY_RES_SIGNATURE | PHY_RES_SUM5 | PHY_RES_SUM6) => self.result_word(off),
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
            PHY_CAL_TRIGGER => {
                self.storage.insert(PHY_CAL_TRIGGER, value);
                if value != 0 {
                    self.cal_ran = true;
                }
            }
            PHY_RES_SIGNATURE => {
                // The firmware seeds this with its own checksum before kicking
                // the calibration; that write invalidates the "cal ran" fixup
                // until the next trigger.
                self.storage.insert(PHY_RES_SIGNATURE, value);
                self.cal_ran = false;
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
