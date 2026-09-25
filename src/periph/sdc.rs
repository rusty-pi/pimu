//! BCM2711 legacy SDRAM-controller register interface (`0x7E00_1000`).
//!
//! Registers and fields: `specs/sdc.toml`.
//!
//! The DRAM clock tree is not modelled: the timing words the EEPROM
//! bootloader programs read straight back, and every sub-controller status
//! word reports ready. What is modelled is the LPDDR4 mode-register port in
//! status slot 0, because two mode registers decide how the boot goes.
//!
//! **MR4**, the temperature-controlled-refresh register: once the ARM is
//! running, start4 polls it about once a second and rescales the refresh
//! interval by `1 << (3 - code)` — code 3 is the nominal 1x interval, a lower
//! code means the die is cool enough to refresh less often. An out-of-range
//! code makes it log `Unexpected sdram refresh code (0)`, so the model seeds
//! MR4 with the 2 a Raspberry Pi 4B d03115 reports just after the handover,
//! behind its `sdram: sdram refresh 1562->3124 (2)`.
//!
//! **MR8**, the density, and which ranks answer at all: the bootloader
//! identifies the part from them and looks up its own memsys config record, so
//! the modelled part has to be one every bootloader knows. The 2023-05-11
//! table has a 16 Gbit record only for a **single** rank — dual-rank dies with
//! `MCB 4 16 1 not found` — so the model is one rank of 16 Gb x16 dies, a 2 GB
//! Pi 4, which every release boots. Device 1 then reaches no die: its mode
//! registers read 0 and writes are lost. An 8 GB Raspberry Pi 4B d03115 instead
//! reports `total-size: 64Gbit` and `rank 2`, 32 Gb per die.
//!
//! MR5, the manufacturer, stays at its reset 0 (printed as `'Unknown'`); it is
//! never part of the MCB key. Every other mode register reads its reset 0.
use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::sdc::{
    REFRESH as REFRESH_WORD, REFRESH_INTERVAL_SHIFT, STATUS, STATUS_ADDR_MASK as MR_ADDR,
    STATUS_CHANNEL_MASK as MR_CHANNEL, STATUS_COUNT, STATUS_DEVICE_MASK as MR_DEVICE,
    STATUS_DONE_MASK as STATUS_READY, STATUS_ERROR_MASK as MR_ERROR, STATUS_RDATA_MASK as MR_RDATA,
    STATUS_RDATA_SHIFT as MR_RDATA_SHIFT, STATUS_STRIDE, STATUS_WDATA_SHIFT as MR_WDATA_SHIFT,
    STATUS_WRITE_MASK as MR_WRITE, TIMING, TIMING0,
};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "sdc",
    decoded: &[TIMING0, REFRESH_WORD, TIMING, STATUS],
};

const MR_PORT: u32 = STATUS;
const MR_DONE: u32 = STATUS_READY;

fn status_slot(off: u32) -> bool {
    off.checked_sub(STATUS)
        .is_some_and(|rel| rel % STATUS_STRIDE == 0 && rel / STATUS_STRIDE < STATUS_COUNT)
}

/// LPDDR4 MR4 (refresh rate / temperature); its reset value here is the code a
/// Raspberry Pi 4B d03115 reports right after the ARM handover.
const MR4_REFRESH_RATE: u32 = 4;
const MR4_RESET: u8 = 2;

/// LPDDR4 MR8 (basic configuration 4): density per die in `OP[5:2]`, I/O
/// width in `OP[7:6]`. `0b0100` is 16 Gb and `0b0110` 32 Gb; width 0 is x16.
const MR8_BASIC_CONFIG: u32 = 8;
const MR8_16GB_X16: u8 = 0b0100 << 2;
const MR8_32GB_X16: u8 = 0b0110 << 2;
const MR8_8GB_X16: u8 = 0b0010 << 2;

/// What the board is fitted with: the density of one die, and whether the
/// second chip select answers like the first. The firmware multiplies the two
/// into the size it logs (`total-size: NNGbit`) and keys its MCB record on it,
/// so this is what decides how much memory the board appears to have: 16 Gb
/// single-rank is a 2 GB Pi 4, 32 Gb single-rank a 4 GB one and 32 Gb
/// dual-rank an 8 GB one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dram {
    pub die_gbit: u32,
    pub dual_rank: bool,
}

impl Dram {
    /// The parts a board with `bytes` of memory is fitted with: a die twice
    /// the size past 2 GB, and a second rank past 4 GB.
    pub fn for_memory(bytes: usize) -> Dram {
        match bytes {
            n if n >= 8 << 30 => Dram {
                die_gbit: 32,
                dual_rank: true,
            },
            n if n >= 4 << 30 => Dram {
                die_gbit: 32,
                dual_rank: false,
            },
            n if n >= 2 << 30 => Dram {
                die_gbit: 16,
                dual_rank: false,
            },
            _ => Dram {
                die_gbit: 8,
                dual_rank: false,
            },
        }
    }

    fn mr8(&self) -> u8 {
        match self.die_gbit {
            32 => MR8_32GB_X16,
            8 => MR8_8GB_X16,
            _ => MR8_16GB_X16,
        }
    }
}

impl Default for Dram {
    fn default() -> Dram {
        Dram {
            die_gbit: 16,
            dual_rank: false,
        }
    }
}

type MrKey = (bool, bool, u8);

pub struct Sdc {
    storage: BTreeMap<u32, u32>,
    /// The fitted ranks' mode registers, as the firmware's reads and writes
    /// see them. Device 1 has any only on a dual-rank part.
    mode_regs: BTreeMap<MrKey, u8>,
    dram: Dram,
    /// Every distinct refresh interval the firmware has programmed, in order.
    /// A boot leaves two entries: the bootloader's value, and the one start4
    /// rescales to once the ARM is running.
    refresh_history: Vec<u32>,
    mr_reads: u64,
}

impl Default for Sdc {
    fn default() -> Self {
        Sdc::new()
    }
}

impl Sdc {
    pub fn new() -> Sdc {
        Sdc::with_dram(Dram::default())
    }

    pub fn with_dram(dram: Dram) -> Sdc {
        let mut mode_regs = BTreeMap::new();
        let ranks: &[bool] = if dram.dual_rank {
            &[false, true]
        } else {
            &[false]
        };
        for &device in ranks {
            for chan in [false, true] {
                mode_regs.insert((device, chan, MR4_REFRESH_RATE as u8), MR4_RESET);
                mode_regs.insert((device, chan, MR8_BASIC_CONFIG as u8), dram.mr8());
            }
        }
        Sdc {
            storage: BTreeMap::new(),
            mode_regs,
            dram,
            refresh_history: Vec::new(),
            mr_reads: 0,
        }
    }

    pub fn dram(&self) -> Dram {
        self.dram
    }

    pub fn refresh_history(&self) -> &[u32] {
        &self.refresh_history
    }

    pub fn mode_register_reads(&self) -> u64 {
        self.mr_reads
    }

    /// Run one mode-register transfer and return the word the firmware will
    /// read back from [`MR_PORT`].
    fn mode_register_access(&mut self, cmd: u32) -> u32 {
        let key: MrKey = (
            cmd & MR_DEVICE != 0,
            cmd & MR_CHANNEL != 0,
            (cmd & MR_ADDR) as u8,
        );
        // An unfitted rank has nothing to drive the data back, and a write
        // to it lands nowhere.
        let fitted = !key.0 || self.dram.dual_rank;
        if cmd & MR_WRITE != 0 {
            if fitted {
                let data = (cmd >> MR_WDATA_SHIFT) as u8;
                self.mode_regs.insert(key, data);
            }
            return (cmd & !MR_ERROR) | MR_DONE;
        }
        self.mr_reads += 1;
        let data = self.mode_regs.get(&key).copied().unwrap_or(0);
        (cmd & !(MR_RDATA | MR_ERROR)) | (u32::from(data) << MR_RDATA_SHIFT) | MR_DONE
    }
}

impl MmioDevice for Sdc {
    fn name(&self) -> &'static str {
        "sdc"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        let stored = self.storage.get(&off).copied().unwrap_or(0);
        // Every sub-controller status slot reports ready; slot 0 additionally
        // carries the result of the last mode-register transfer, which the
        // write path already latched.
        if status_slot(off) {
            return Ok(stored | STATUS_READY);
        }
        Ok(stored)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        let value = if off == MR_PORT {
            self.mode_register_access(value)
        } else {
            value
        };
        if off == REFRESH_WORD {
            let interval = value >> REFRESH_INTERVAL_SHIFT;
            if self.refresh_history.last() != Some(&interval) {
                self.refresh_history.push(interval);
            }
        }
        self.storage.insert(off, value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(sdc: &mut Sdc) -> u32 {
        sdc.read(MR_PORT, Width::Word).unwrap()
    }

    fn read_mr(sdc: &mut Sdc, cmd: u32) -> u32 {
        sdc.write(MR_PORT, Width::Word, cmd).unwrap();
        (port(sdc) & MR_RDATA) >> MR_RDATA_SHIFT
    }

    #[test]
    fn mr4_reports_the_reference_boards_refresh_code() {
        let mut sdc = Sdc::new();
        sdc.write(MR_PORT, Width::Word, 4 | MR_CHANNEL).unwrap();
        let got = port(&mut sdc);
        assert_eq!(got & MR_DONE, MR_DONE, "transfer must report complete");
        assert_eq!(got & MR_ERROR, 0, "transfer must not report an error");
        assert_eq!((got & MR_RDATA) >> MR_RDATA_SHIFT, u32::from(MR4_RESET));
    }

    #[test]
    fn a_mode_register_write_is_read_back() {
        let mut sdc = Sdc::new();
        sdc.write(
            MR_PORT,
            Width::Word,
            MR_WRITE | (0x5A << MR_WDATA_SHIFT) | 13,
        )
        .unwrap();
        assert_eq!(read_mr(&mut sdc, 13), 0x5A);
        assert_eq!(read_mr(&mut sdc, MR_CHANNEL | 13), 0);
    }

    #[test]
    fn a_bigger_board_is_fitted_with_bigger_parts() {
        assert_eq!(
            Dram::for_memory(1 << 30),
            Dram {
                die_gbit: 8,
                dual_rank: false
            }
        );
        assert_eq!(
            Dram::for_memory(2 << 30),
            Dram {
                die_gbit: 16,
                dual_rank: false
            }
        );
        assert_eq!(
            Dram::for_memory(4 << 30),
            Dram {
                die_gbit: 32,
                dual_rank: false
            }
        );
        assert_eq!(
            Dram::for_memory(8 << 30),
            Dram {
                die_gbit: 32,
                dual_rank: true
            }
        );
    }

    #[test]
    fn the_8_gb_board_answers_on_both_ranks_with_32_gb_dies() {
        let mut sdc = Sdc::with_dram(Dram::for_memory(8 << 30));
        for key in [8, 8 | MR_CHANNEL, 8 | MR_DEVICE, 8 | MR_DEVICE | MR_CHANNEL] {
            assert_eq!(read_mr(&mut sdc, key), u32::from(MR8_32GB_X16));
        }
    }

    #[test]
    fn mr8_describes_one_rank_of_16_gb_dies() {
        let mut sdc = Sdc::new();
        assert_eq!(read_mr(&mut sdc, 8), 0x10);
        assert_eq!(read_mr(&mut sdc, 8 | MR_CHANNEL), 0x10);
        // The bootloader counts a second rank only when device 1's MR8 matches.
        assert_eq!(read_mr(&mut sdc, 8 | MR_DEVICE), 0);
        assert_eq!(read_mr(&mut sdc, 8 | MR_DEVICE | MR_CHANNEL), 0);
    }

    #[test]
    fn the_rank_that_is_not_fitted_reads_zero_and_drops_writes() {
        let mut sdc = Sdc::new();
        sdc.write(
            MR_PORT,
            Width::Word,
            MR_DEVICE | MR_WRITE | (0x5A << MR_WDATA_SHIFT) | 13,
        )
        .unwrap();
        assert_eq!(port(&mut sdc) & MR_DONE, MR_DONE);
        assert_eq!(read_mr(&mut sdc, MR_DEVICE | 13), 0);
        assert_eq!(read_mr(&mut sdc, MR_DEVICE | 4), 0);
    }

    #[test]
    fn unwritten_registers_read_as_zero_and_ready() {
        let mut sdc = Sdc::new();
        sdc.write(MR_PORT, Width::Word, 6).unwrap();
        let got = port(&mut sdc);
        assert_eq!(got & MR_RDATA, 0);
        assert_eq!(got & MR_DONE, MR_DONE);
    }

    #[test]
    fn plain_status_slots_still_read_ready() {
        let mut sdc = Sdc::new();
        assert_eq!(
            sdc.read(STATUS + STATUS_STRIDE, Width::Word).unwrap() & STATUS_READY,
            STATUS_READY
        );
        sdc.write(REFRESH_WORD, Width::Word, 0x061a_0474).unwrap();
        assert_eq!(sdc.read(REFRESH_WORD, Width::Word).unwrap(), 0x061a_0474);
    }
}
