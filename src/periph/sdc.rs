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
//!
//! `+0x9C` is not only a ready bit, though: it is the **LPDDR4 mode-register
//! access port**. start4's SDRAM driver (the `.drivers` entry at `0x3EDFDF68`)
//! drives it from two ops:
//!
//! * read (`0x3ED6BA90`): write `addr | dev<<24 | chan<<25`, poll bit 31 for
//!   "complete", check bit 30 for "error" (it prints `SD MR %08x R timeout …`
//!   when that is set), then take the returned byte from bits 23:16. Its debug
//!   line, `RD: MR addr: %d device: %d channel: %d`, is what names bit 24 the
//!   device (rank, i.e. chip select) and bit 25 the channel.
//! * write (`0x3ED6C084`): the same word plus the data in bits 15:8 and bit 28
//!   set to mark it a write.
//!
//! The one mode register the boot actually needs is **MR4**, the LPDDR4
//! temperature-controlled-refresh register: once the ARM has been started,
//! `0x3ED6BBA0` polls MR4 once a second and rescales the refresh interval in
//! `[0x7E00_1004] >> 16` by `1 << (3 - code)` — code 3 is the nominal 1x
//! interval, a lower code means the die is cool enough to refresh less often.
//! The reference board reports code 2 just after the handover and the firmware
//! doubles the interval, which is what `examples-on-real-hardware/vc4-boot.log`
//! logs as `sdram: sdram refresh 1562->3124 (2)`. With the port returning 0 the
//! firmware instead saw an out-of-range code and logged
//! `Unexpected sdram refresh code (0)`, so the model seeds MR4 with the
//! reference board's 2.
//!
//! The other one that matters is **MR8**, the density, together with which
//! ranks answer at all. The bootloader identifies the part from them and looks
//! up a memsys config record (MCB) for it; every bootloader carries its own MCB
//! table, so the part has to be one that all of them know. The 2023-05-11
//! bootcode's identify step (`0x800056a4`) reads MR5, MR6 and MR8 on both
//! devices and both channels, takes the density per die from MR8 `OP[5:2]`,
//! and counts two ranks only when device 1's MR8 reads the same as device 0's.
//! Its MCB key is then (size, dual-rank, byte-mode) (`0x8000544c`), and its
//! table has a 16 Gbit record only for a **single** rank — a dual-rank 16 Gbit
//! part dies with `MCB 4 16 1 not found` — while the 2026 tables carry both.
//! A 2 GB Pi 4 boots every release, so its part is one rank of 16 Gb x16 dies:
//! that is what the model is, which is also what `boot --eeprom` backs by
//! default. The second chip select has nothing on it, so a transfer to device
//! 1 reaches no die: every mode register reads 0 there and a write is lost.
//! The reference board is an 8 GB Pi 4B (`total-size: 64Gbit` and `rank 2` in
//! `examples-on-real-hardware/sd-card-boot-perfect.log`, 32 Gb per die).
//!
//! MR5 (the manufacturer) stays at its reset 0, which the bootloader prints as
//! `'Unknown'`: it is only printed, never part of the MCB key — Samsung (1),
//! Hynix (6) and Micron (0xFF) all gave the same key on the 2023 bootcode.
//! Every other mode register reads its reset 0.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

// The command-word layout of the mode-register port is `STATUS`'s fields.
use crate::spec::sdc::{
    REFRESH as REFRESH_WORD, REFRESH_INTERVAL_SHIFT, STATUS, STATUS_ADDR_MASK as MR_ADDR,
    STATUS_CHANNEL_MASK as MR_CHANNEL, STATUS_COUNT, STATUS_DEVICE_MASK as MR_DEVICE,
    STATUS_DONE_MASK as STATUS_READY, STATUS_ERROR_MASK as MR_ERROR, STATUS_RDATA_MASK as MR_RDATA,
    STATUS_RDATA_SHIFT as MR_RDATA_SHIFT, STATUS_STRIDE, STATUS_WDATA_SHIFT as MR_WDATA_SHIFT,
    STATUS_WRITE_MASK as MR_WRITE, TIMING, TIMING0,
};
use crate::spec::Coverage;

/// The timing words are storage; the status slots and the mode-register port
/// are modelled.
pub const COVERAGE: Coverage = Coverage {
    block: "sdc",
    decoded: &[TIMING0, REFRESH_WORD, TIMING, STATUS],
};

/// Mode-register access port: the first sub-controller's status slot.
const MR_PORT: u32 = STATUS;
/// Transfer complete — the same bit the plain ready polls look at.
const MR_DONE: u32 = STATUS_READY;

/// Is `off` one of the sub-controller status slots (`+0x9C`, `+0x11C`, …)?
fn status_slot(off: u32) -> bool {
    off.checked_sub(STATUS)
        .is_some_and(|rel| rel % STATUS_STRIDE == 0 && rel / STATUS_STRIDE < STATUS_COUNT)
}

/// LPDDR4 MR4 (refresh rate / temperature). Value the reference board reports
/// right after the ARM handover (`vc4-boot.log`: `sdram refresh 1562->3124 (2)`
/// — start4 scales the refresh interval by `1 << (3 - code)`, so 2 means the
/// die is cool enough to refresh at half the nominal rate).
const MR4_REFRESH_RATE: u32 = 4;
const MR4_RESET: u8 = 2;

/// LPDDR4 MR8 (basic configuration 4): density per die in `OP[5:2]`, I/O
/// width in `OP[7:6]`. `0b0100` is 16 Gb and `0b0110` 32 Gb; width 0 is x16.
const MR8_BASIC_CONFIG: u32 = 8;
const MR8_16GB_X16: u8 = 0b0100 << 2;
const MR8_32GB_X16: u8 = 0b0110 << 2;

/// What the board is fitted with: the density of one die, and whether the
/// second chip select answers like the first. The firmware multiplies the two
/// into the size it logs (`total-size: NNGbit`) and keys its MCB record on it,
/// so this is what decides how much memory the board appears to have: 16 Gb
/// single-rank is a 2 GB Pi 4, 32 Gb single-rank a 4 GB one and 32 Gb
/// dual-rank the 8 GB reference board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dram {
    pub die_gbit: u32,
    pub dual_rank: bool,
}

impl Dram {
    /// The parts a board with `ram_bytes` behind the bus would be fitted
    /// with. Anything under 4 GiB keeps the 2 GB part, which every bootloader
    /// release has an MCB record for.
    pub fn for_ram(ram_bytes: usize) -> Dram {
        match ram_bytes {
            n if n >= 8 << 30 => Dram { die_gbit: 32, dual_rank: true },
            n if n >= 4 << 30 => Dram { die_gbit: 32, dual_rank: false },
            _ => Dram { die_gbit: 16, dual_rank: false },
        }
    }

    fn mr8(&self) -> u8 {
        match self.die_gbit {
            32 => MR8_32GB_X16,
            _ => MR8_16GB_X16,
        }
    }
}

impl Default for Dram {
    fn default() -> Dram {
        Dram { die_gbit: 16, dual_rank: false }
    }
}

/// A mode register is addressed by device (rank), channel and register number.
type MrKey = (bool, bool, u8);

pub struct Sdc {
    storage: BTreeMap<u32, u32>,
    /// The fitted ranks' mode registers, as the firmware's reads and writes
    /// see them. Device 1 has any only on a dual-rank part: see the module
    /// docs.
    mode_regs: BTreeMap<MrKey, u8>,
    /// What the board is fitted with.
    dram: Dram,
    /// Every distinct refresh interval the firmware has programmed, in order.
    /// The boot is expected to leave two entries here: the bootloader's value
    /// and the one start4 rescales to once the ARM is running.
    refresh_history: Vec<u32>,
    /// How many mode-register reads the firmware has issued.
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

    /// A controller in front of the parts `dram` describes.
    pub fn with_dram(dram: Dram) -> Sdc {
        let mut mode_regs = BTreeMap::new();
        let ranks: &[bool] = if dram.dual_rank { &[false, true] } else { &[false] };
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

    /// What the board is fitted with.
    pub fn dram(&self) -> Dram {
        self.dram
    }

    /// Every distinct DRAM refresh interval the firmware has programmed.
    pub fn refresh_history(&self) -> &[u32] {
        &self.refresh_history
    }

    /// How many mode-register reads the firmware has issued.
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
        // Report the ready bit for every sub-controller status slot (`+0x9C`,
        // `+0x11C`, …); the plain timing-table words at `+0x00..+0x30` read back
        // whatever was written. `+0x9C` additionally carries the result of the
        // last mode-register transfer, which the write path already latched.
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
        // Read MR4 on channel 1 of the fitted rank, the way `0x3ED6BA90`
        // builds it.
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
        // ... and only for the channel it was written to.
        assert_eq!(read_mr(&mut sdc, MR_CHANNEL | 13), 0);
    }

    #[test]
    fn a_bigger_board_is_fitted_with_bigger_parts() {
        assert_eq!(Dram::for_ram(2 << 30), Dram { die_gbit: 16, dual_rank: false });
        assert_eq!(Dram::for_ram(4 << 30), Dram { die_gbit: 32, dual_rank: false });
        assert_eq!(Dram::for_ram(8 << 30), Dram { die_gbit: 32, dual_rank: true });
    }

    #[test]
    fn the_8_gb_board_answers_on_both_ranks_with_32_gb_dies() {
        let mut sdc = Sdc::with_dram(Dram::for_ram(8 << 30));
        for key in [8, 8 | MR_CHANNEL, 8 | MR_DEVICE, 8 | MR_DEVICE | MR_CHANNEL] {
            assert_eq!(read_mr(&mut sdc, key), u32::from(MR8_32GB_X16));
        }
    }

    #[test]
    fn mr8_describes_one_rank_of_16_gb_dies() {
        let mut sdc = Sdc::new();
        // 16 Gb x16 on both channels of device 0: 16 Gbit, a 2 GB board.
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
        // Timing words are plain storage.
        sdc.write(REFRESH_WORD, Width::Word, 0x061a_0474).unwrap();
        assert_eq!(sdc.read(REFRESH_WORD, Width::Word).unwrap(), 0x061a_0474);
    }
}
