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
//! `+0x2_0000`, PHY register arrays at `+0x3_8000` (4096 words) and `+0x3_4000`
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
//! * **`+0x3_2100..+0x3_2120`** — calibration-request/result block, five
//!   parameter words then two trailing checksum words. The firmware seeds
//!   `[+0x00] = 1` (valid), `[+0x04] = <command>`, `[+0x08] = <sub-param>`,
//!   `[+0x0C] = <seed>` (a checksum for the `0x101` query, else a plain tag),
//!   `[+0x10] = 0`, `[+0x14] = sum([+0x00..+0x14])`, then pulses the trigger.
//!   The calibration engine writes back:
//!   - `[+0x0C]` = the PHY signature `0x0223_0000` for command `0x101`
//!     ("report signature", checked by `0x800068ce`), otherwise `0`
//!     ("completed, no error", checked by `0x800065f6` / `0x800066a6`)
//!   - `[+0x10]` = `<rank> << 8` for command `0x101` (`0x800068ce` checks
//!     `([+0x10] >> 8) & 0xFF` against the rank it is verifying, 0 then 1),
//!     otherwise `0`. The firmware seeds identical parameters for every rank,
//!     so the rank is inferred from the count of signature-report calibrations.
//!   - `[+0x14] = sum([+0x00..+0x14])`, `[+0x18] = sum([+0x00..+0x18])` so the
//!     firmware's "sum the leading words, compare the trailing word" checks
//!     balance.
//! * Everything else is sticky (read-after-write), default 0. The PHY preset
//!   arrays the firmware copies in from `memsysNN.bin` and sum-checks land here
//!   and read straight back, so those checks pass unchanged.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

// The calibration request / result block is five parameter words from
// `PHY_RES_VALID` then the two checksum words. `PHY_SIGNATURE` is the header
// word of every `memsys00.bin` .. `memsys08.bin`: a fixed PHY-block signature,
// not a per-preset hash.
use crate::spec::sdramc::{
    CTRL_CMD, CTRL_STATUS, CTRL_STATUS_DONE_MASK as CTRL_DONE, PHY_A, PHY_B, PHY_CAL_BUSY,
    PHY_CAL_TRIGGER, PHY_RES_CMD, PHY_RES_PARAM, PHY_RES_SIGNATURE,
    PHY_RES_SIGNATURE_RESET as PHY_SIGNATURE, PHY_RES_STATUS, PHY_RES_SUM5, PHY_RES_SUM6,
    PHY_RES_VALID as PHY_RES_BASE,
};
use crate::spec::Coverage;

/// Every register in `specs/sdramc.toml` is modelled, the PHY arrays and the
/// parameter words as storage.
pub const COVERAGE: Coverage = Coverage {
    block: "sdramc",
    decoded: &[
        CTRL_STATUS,
        CTRL_CMD,
        PHY_CAL_TRIGGER,
        PHY_CAL_BUSY,
        PHY_RES_BASE,
        PHY_RES_CMD,
        PHY_RES_PARAM,
        PHY_RES_SIGNATURE,
        PHY_RES_STATUS,
        PHY_RES_SUM5,
        PHY_RES_SUM6,
        PHY_A,
        PHY_B,
    ],
};

/// Command word (`PHY_RES_CMD`) that asks the PHY to report its signature
/// rather than run a training step.
const PHY_CMD_REPORT_SIGNATURE: u32 = 0x101;

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
    /// Count of `0x101` ("report signature") calibrations run so far. The
    /// firmware verifies each rank with byte-identical PHY parameters, so this
    /// count stands in for "which rank is being trained" in the result word.
    sig_cal_count: u32,
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
            sig_cal_count: 0,
            log: Vec::new(),
            log_limit: 8192,
        }
    }

    fn record(&mut self, a: SdramcAccess) {
        if self.log.len() < self.log_limit {
            self.log.push(a);
        }
    }

    fn word(&self, off: u32) -> u32 {
        self.storage.get(&off).copied().unwrap_or(0)
    }

    /// Rising edge on `PHY_CAL_TRIGGER`: run one "calibration" and fill in the
    /// result block the firmware reads back (`0x800065f6` / `0x800068ce` /
    /// `0x800066a6`).
    fn run_phy_cal(&mut self) {
        let (signature, status) = if self.word(PHY_RES_CMD) == PHY_CMD_REPORT_SIGNATURE {
            let rank = self.sig_cal_count;
            self.sig_cal_count += 1;
            (PHY_SIGNATURE, rank << 8)
        } else {
            (0, 0)
        };
        self.storage.insert(PHY_RES_SIGNATURE, signature);
        self.storage.insert(PHY_RES_STATUS, status);

        let sum5 = (0..5)
            .map(|i| self.word(PHY_RES_BASE + i * 4))
            .fold(0u32, u32::wrapping_add);
        self.storage.insert(PHY_RES_SUM5, sum5);
        self.storage.insert(PHY_RES_SUM6, sum5.wrapping_add(sum5));
    }
}

impl MmioDevice for Sdramc {
    fn name(&self) -> &'static str {
        "sdramc"
    }

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32> {
        let value = match offset & !3 {
            CTRL_STATUS => {
                let base = self.word(CTRL_STATUS);
                if self.cmd_done {
                    base | CTRL_DONE
                } else {
                    base & !CTRL_DONE
                }
            }
            PHY_CAL_BUSY => {
                // Busy tracks the trigger: 1 while a calibration is "running",
                // 0 once the firmware clears the trigger.
                u32::from(self.word(PHY_CAL_TRIGGER) != 0)
            }
            off => self.word(off),
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
                let was = self.word(PHY_CAL_TRIGGER);
                self.storage.insert(PHY_CAL_TRIGGER, value);
                if was == 0 && value != 0 {
                    self.run_phy_cal();
                }
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
