//! BCM2711 LPDDR4 memory controller + PHY (`0x7DC0_0000`, below the legacy
//! peripheral window).
//!
//! Registers and fields: `specs/sdramc.toml`.
//!
//! The EEPROM bootloader's `init_sdram_*` path (VC4 code in the bootcode,
//! driven by the `memsysNN.bin` PHY-preset tables decompressed from
//! `pieeprom.bin`) drives this block to bring LPDDR4 up. The model does not
//! simulate DRAM electrically — its "DRAM" is flat, always-correct RAM — so
//! every training and calibration step reports immediate success with the
//! values the firmware's self-consistency checks expect.
//!
//! Everything the spec does not list is sticky storage defaulting to 0: the
//! PHY preset arrays the firmware copies in from `memsysNN.bin` and sum-checks
//! read straight back, so those checks pass unchanged. Narrow accesses get
//! their lane, because the 2022-04-26 bootcode decompresses `mcb.bin` straight
//! into `PHY_B` and hashes it a byte at a time.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

// `PHY_SIGNATURE` is only the fall-back for a memsys whose header
// `report_signature` does not recognise.
use crate::spec::sdramc::{
    CTRL_CMD, CTRL_STATUS, CTRL_STATUS_DONE_MASK as CTRL_DONE, PHY_A, PHY_B, PHY_CAL_BUSY,
    PHY_CAL_TRIGGER, PHY_RES_CMD, PHY_RES_PARAM, PHY_RES_SIGNATURE,
    PHY_RES_SIGNATURE_RESET as PHY_SIGNATURE, PHY_RES_STATUS, PHY_RES_SUM5, PHY_RES_SUM6,
    PHY_RES_VALID as PHY_RES_BASE,
};
use crate::spec::Coverage;

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
    /// Whether the most recent controller `CMD` has "completed": always true
    /// here, but a fresh write re-arms the rising edge a poll expects.
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

    /// The signature the PHY reports, reconstructed from the memsys microcode
    /// the firmware has already written rather than hard-coded.
    ///
    /// The bootloader verifies the PHY reports the same version signature its
    /// memsys config record holds. That record lives in firmware RAM the model
    /// never sees, but its microcode is written verbatim into `PHY_B` and
    /// mirrors the signature in two slots: the version marker `0x1860_02vv` at
    /// `+0x388`, whose `0x02vv` is the signature's **high** halfword, and an
    /// optional `0xA863_llll` value-load at `+0x38C` supplying the low one —
    /// absent when that slot holds the next microcode word instead, and the low
    /// halfword is then 0.
    ///
    /// Latched in [`Sdramc::phy_sig`]: a report can run again in a later pass,
    /// after the header has been overwritten, and the firmware still expects
    /// the signature it loaded, so only a live version marker updates it.
    fn report_signature(&mut self) -> u32 {
        const SIG_MARKER: u32 = PHY_B + 0x388;
        const SIG_COMPANION: u32 = PHY_B + 0x38C;
        let marker = self.word(SIG_MARKER);
        if marker >> 16 == 0x1860 {
            let high = marker & 0xFFFF;
            let companion = self.word(SIG_COMPANION);
            let low = if companion >> 16 == 0xA863 {
                companion & 0xFFFF
            } else {
                0
            };
            self.phy_sig = (high << 16) | low;
        }
        self.phy_sig
    }

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
            PHY_CAL_BUSY => u32::from(self.word(PHY_CAL_TRIGGER) != 0),
            off => self.word(off),
        };
        // Narrow reads get their lane, right-aligned: the 2022-04-26 bootcode
        // hashes `mcb.bin` a byte at a time straight out of `PHY_B`.
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

    fn reported_signature(marker: u32, companion: u32) -> u32 {
        let mut sdramc = Sdramc::new();
        sdramc.write(PHY_B + 0x388, Width::Word, marker).unwrap();
        sdramc.write(PHY_B + 0x38C, Width::Word, companion).unwrap();
        sdramc.write(PHY_RES_BASE, Width::Word, 1).unwrap();
        sdramc
            .write(PHY_RES_CMD, Width::Word, PHY_CMD_REPORT_SIGNATURE)
            .unwrap();
        sdramc.write(PHY_CAL_TRIGGER, Width::Word, 1).unwrap();
        sdramc.read(PHY_RES_SIGNATURE, Width::Word).unwrap()
    }

    /// The 2022-04-26 bootcode writes the decompressed `mcb.bin` into `PHY_B`
    /// a word at a time and hashes it a byte at a time.
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
        assert_eq!(reported_signature(0x18600220, 0xa8630100), 0x02200100);
        assert_eq!(reported_signature(0x18600222, 0xa8630100), 0x02220100);
        assert_eq!(reported_signature(0x18600223, 0xd402180c), 0x02230000);
    }

    #[test]
    fn unrecognised_marker_falls_back_to_the_reset_signature() {
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
        let sum5 = sdramc.read(PHY_RES_SUM5, Width::Word).unwrap();
        let sum6 = sdramc.read(PHY_RES_SUM6, Width::Word).unwrap();
        assert_eq!(sum6, sum5.wrapping_add(sum5));
    }
}
