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
//!   - `[+0x0C]` = the PHY signature for command `0x101` ("report signature"),
//!     otherwise `0` ("completed, no error", checked by `0x800065f6` /
//!     `0x800066a6`). The signature is a memsys version tag, not a constant:
//!     the bootloader loads it from the memsys config record (MCB) it selected
//!     and compares the PHY's echo against it (2025-11 fails right here with
//!     `BOOT ERROR: code 8 - 'SDRAM failure'` when the model reports the wrong
//!     one). The model has no MCB, so `report_signature` reconstructs it from
//!     the PHY microcode the firmware wrote — see that function.
//!   - `[+0x10]` = `<rank> << 8` for command `0x101` (`0x800068ce` checks
//!     `([+0x10] >> 8) & 0xFF` against the rank it is verifying, 0 then 1),
//!     otherwise `0`. The firmware seeds identical parameters for every rank,
//!     so the rank is inferred from the count of signature-report calibrations.
//!   - `[+0x14] = sum([+0x00..+0x14])`, `[+0x18] = sum([+0x00..+0x18])` so the
//!     firmware's "sum the leading words, compare the trailing word" checks
//!     balance.
//! * Everything else is sticky (read-after-write), default 0. The PHY preset
//!   arrays the firmware copies in from `memsysNN.bin` and sum-checks land here
//!   and read straight back, so those checks pass unchanged. Narrow accesses
//!   get their lane: the 2022-04-26 bootcode decompresses `mcb.bin` straight
//!   into `PHY_B` and hashes it a byte at a time (#69).

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

// The calibration request / result block is five parameter words from
// `PHY_RES_VALID` then the two checksum words. The signature the PHY reports is
// not fixed across firmware: it is a memsys version tag that each bootloader
// derives from the memsys config record it selected, and then expects the PHY
// to echo. `run_phy_cal` reconstructs it from the PHY microcode the firmware
// wrote (see `report_signature`); `PHY_SIGNATURE` is only the fall-back for a
// memsys whose header the model does not recognise.
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
    /// The memsys signature the model reports, latched from the version marker
    /// the firmware wrote to the PHY_B array (see [`Sdramc::report_signature`]).
    /// It is a property of the loaded memsys, so it is kept across the later
    /// training passes that replace the header with other microcode.
    phy_sig: u32,
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
            phy_sig: PHY_SIGNATURE,
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

    /// The signature the PHY reports for the "report signature" command,
    /// reconstructed from the memsys PHY microcode the firmware has already
    /// written (`self.storage`) rather than hard-coded.
    ///
    /// On real hardware the bootloader loads a memsys config record (MCB) and
    /// then verifies the PHY reports the same version signature it holds; the
    /// compare is `Cmp r7, [r8+12]` at VPU pc `0x800067de`, where `r7` is the
    /// MCB's signature field and `[r8+12]` is `PHY_RES_SIGNATURE`. The MCB lives
    /// in firmware RAM the model never sees, but the record's PHY microcode is
    /// written verbatim into the PHY_B array, and it mirrors the signature in
    /// two fixed slots:
    ///
    /// * `PHY_B + 0x388` (`0x7DC3_8388`) — the version marker command
    ///   `0x1860_02vv` (first written from pc `0x800066ba` / `0x8000675e`). Its
    ///   low halfword `0x02vv` is the signature's **high** halfword.
    /// * `PHY_B + 0x38C` (`0x7DC3_8390 - 4`) — an optional value-load command
    ///   `0xA863_llll` that follows the marker only when the low halfword is
    ///   non-zero. When present, `0xllll` is the signature's **low** halfword;
    ///   when the slot instead holds the next microcode word (a different
    ///   command class, e.g. `0xD402_....`), the low halfword is `0`.
    ///
    /// Evidence (write to `+0x388` / `+0x38C` at the first `0x101` calibration
    /// vs. the firmware's expected `r7`):
    ///
    /// | memsys (vintage)      | `+0x388`     | `+0x38C`     | signature    |
    /// |-----------------------|--------------|--------------|--------------|
    /// | `0x0220` (2023..2025-11) | `0x18600220` | `0xa8630100` | `0x02200100` |
    /// | `0x0222` (2026-04-14)    | `0x18600222` | `0xa8630100` | `0x02220100` |
    /// | `0x0223` (pinned 2026)   | `0x18600223` | `0xd402180c` | `0x02230000` |
    ///
    /// The `0x0223` memsys is the pinned board's, so this reproduces its
    /// `0x02230000` unchanged (also the [`PHY_SIGNATURE`] fall-back). The
    /// value is latched in [`Sdramc::phy_sig`]: a report can also run in a later
    /// training pass, once the header has been replaced by other microcode
    /// (e.g. `0x38388 = 0xa4630fff`), and the firmware still expects the
    /// signature it loaded, so only a live version marker updates the latch.
    fn report_signature(&mut self) -> u32 {
        const SIG_MARKER: u32 = PHY_B + 0x388;
        const SIG_COMPANION: u32 = PHY_B + 0x38C;
        let marker = self.word(SIG_MARKER);
        // The version marker is the PHY command class 0x1860_....; anything else
        // in that slot is a later pass's microcode, so keep the latched value.
        if marker >> 16 == 0x1860 {
            let high = marker & 0xFFFF;
            let companion = self.word(SIG_COMPANION);
            // The low halfword is only present as an 0xA863_.... value-load
            // command right after the marker; otherwise that slot holds the
            // next microcode word and the low halfword is zero.
            let low = if companion >> 16 == 0xA863 {
                companion & 0xFFFF
            } else {
                0
            };
            self.phy_sig = (high << 16) | low;
        }
        self.phy_sig
    }

    /// Rising edge on `PHY_CAL_TRIGGER`: run one "calibration" and fill in the
    /// result block the firmware reads back (`0x800065f6` / `0x800068ce` /
    /// `0x800066a6`).
    fn run_phy_cal(&mut self) {
        let (signature, status) = if self.word(PHY_RES_CMD) == PHY_CMD_REPORT_SIGNATURE {
            let rank = self.sig_cal_count;
            self.sig_cal_count += 1;
            (self.report_signature(), rank << 8)
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
        let word = match offset & !3 {
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
        // Narrow reads get their lane, right-aligned: the 2022-04-26 bootcode
        // hashes mcb.bin a byte at a time straight out of PHY_B (#69).
        let value = match width {
            Width::Word => word,
            Width::Half => (word >> ((offset & 2) * 8)) & 0xFFFF,
            Width::Byte => (word >> ((offset & 3) * 8)) & 0xFF,
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
        // Narrow writes merge into the word.
        let merged = match width {
            Width::Word => value,
            _ => {
                let shift = (offset & 3) * 8;
                let mask = if width == Width::Half { 0xFFFF } else { 0xFF } << shift;
                (self.word(offset & !3) & !mask) | ((value << shift) & mask)
            }
        };
        let value = merged;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Load the PHY microcode header the way the bootloader does, run the
    /// "report signature" calibration, and return what the model reports back
    /// in `PHY_RES_SIGNATURE`.
    fn reported_signature(marker: u32, companion: u32) -> u32 {
        let mut sdramc = Sdramc::new();
        sdramc.write(PHY_B + 0x388, Width::Word, marker).unwrap();
        sdramc.write(PHY_B + 0x38C, Width::Word, companion).unwrap();
        // The firmware seeds the request block, then pulses the trigger.
        sdramc.write(PHY_RES_BASE, Width::Word, 1).unwrap();
        sdramc
            .write(PHY_RES_CMD, Width::Word, PHY_CMD_REPORT_SIGNATURE)
            .unwrap();
        sdramc.write(PHY_CAL_TRIGGER, Width::Word, 1).unwrap();
        sdramc.read(PHY_RES_SIGNATURE, Width::Word).unwrap()
    }

    /// The 2022-04-26 bootcode writes the decompressed `mcb.bin` into `PHY_B`
    /// a word at a time and hashes it a byte at a time (#69).
    #[test]
    fn narrow_accesses_get_their_lane() {
        let mut sdramc = Sdramc::new();
        sdramc.write(PHY_B, Width::Word, 0x0000_061c).unwrap();
        let bytes: Vec<u32> = (0..4)
            .map(|i| sdramc.read(PHY_B + i, Width::Byte).unwrap())
            .collect();
        assert_eq!(bytes, [0x1c, 0x06, 0, 0]);
        assert_eq!(sdramc.read(PHY_B + 2, Width::Half).unwrap(), 0);

        sdramc.write(PHY_B + 3, Width::Byte, 0xAB).unwrap();
        sdramc.write(PHY_B + 2, Width::Byte, 0x1CD).unwrap();
        assert_eq!(sdramc.read(PHY_B, Width::Word).unwrap(), 0xabcd_061c);
        assert_eq!(sdramc.read(PHY_B + 2, Width::Half).unwrap(), 0xabcd);
    }

    #[test]
    fn signature_is_derived_from_the_memsys_header() {
        // The three memsys versions that reach the signature check, each with
        // the version marker and (present-or-not) low-halfword companion the
        // bootloader wrote — see `report_signature`'s evidence table.
        assert_eq!(reported_signature(0x18600220, 0xa8630100), 0x02200100);
        assert_eq!(reported_signature(0x18600222, 0xa8630100), 0x02220100);
        // The pinned board's memsys has no companion (the slot holds the next
        // microcode word), so the low halfword is zero — unchanged from the old
        // hard-coded value.
        assert_eq!(reported_signature(0x18600223, 0xd402180c), 0x02230000);
    }

    #[test]
    fn unrecognised_marker_falls_back_to_the_reset_signature() {
        // A header the model has not been shown keeps the previous behaviour.
        assert_eq!(reported_signature(0, 0), PHY_SIGNATURE);
    }

    #[test]
    fn sum6_is_self_consistent_after_a_report() {
        let mut sdramc = Sdramc::new();
        sdramc
            .write(PHY_B + 0x388, Width::Word, 0x18600220)
            .unwrap();
        sdramc
            .write(PHY_B + 0x38C, Width::Word, 0xa8630100)
            .unwrap();
        sdramc.write(PHY_RES_BASE, Width::Word, 1).unwrap();
        sdramc
            .write(PHY_RES_CMD, Width::Word, PHY_CMD_REPORT_SIGNATURE)
            .unwrap();
        sdramc.write(PHY_CAL_TRIGGER, Width::Word, 1).unwrap();
        // The firmware's "sum the leading words, compare the trailing word"
        // check: SUM6 must be twice SUM5.
        let sum5 = sdramc.read(PHY_RES_SUM5, Width::Word).unwrap();
        let sum6 = sdramc.read(PHY_RES_SUM6, Width::Word).unwrap();
        assert_eq!(sum6, sum5.wrapping_add(sum5));
    }
}
