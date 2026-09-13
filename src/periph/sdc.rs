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
//! * read (`0x3ED6BA90`): write `addr | chan<<24 | dev<<25`, poll bit 31 for
//!   "complete", check bit 30 for "error" (it prints `SD MR %08x R timeout …`
//!   when that is set), then take the returned byte from bits 23:16.
//! * write (`0x3ED6C084`): the same word plus the data in bits 15:8 and bit 28
//!   set to mark it a write.
//!
//! The one mode register the boot actually needs is **MR4**, the LPDDR4
//! temperature-controlled-refresh register: once the ARM has been started,
//! `0x3ED6BBA0` polls MR4 on every channel/device once a second and rescales
//! the refresh interval in `[0x7E00_1004] >> 16` by `1 << (3 - code)` — code 3
//! is the nominal 1x interval, a lower code means the die is cool enough to
//! refresh less often. The reference board reports code 2 just after the
//! handover and the firmware doubles the interval, which is what
//! `examples-on-real-hardware/vc4-boot.log` logs as
//! `sdram: sdram refresh 1562->3124 (2)`. With the port returning 0 the
//! firmware instead saw an out-of-range code and logged
//! `Unexpected sdram refresh code (0)`, so the model seeds MR4 with the
//! reference board's 2.
//!
//! The other one that matters is **MR8**, the density. The bootloader sizes
//! the DRAM from it (`Initialising SDRAM rank 2 total-size: <n>Gbit`), and
//! start4 answers `GET_BOARD_REVISION` with a memory-size field to match, not
//! the OTP's. The reference board is an 8 GB Pi 4B (`total-size: 64Gbit` in
//! `examples-on-real-hardware/sd-card-boot-perfect.log`, 32 Gb per die); the
//! model is a 2 GB one, which is what `recon --eeprom` backs by default: MR8
//! says 8 Gb per die, x16, so two ranks make 16 Gbit. At its reset 0 (4 Gb)
//! the board came out as 1 GB. Every other mode register reads its reset 0.

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
/// width in `OP[7:6]`. `0b0010` is 8 Gb, and width 0 is x16.
const MR8_BASIC_CONFIG: u32 = 8;
const MR8_8GB_X16: u8 = 0b0010 << 2;

/// A mode register is addressed by channel, device (rank) and register number.
type MrKey = (bool, bool, u8);

pub struct Sdc {
    storage: BTreeMap<u32, u32>,
    /// The DRAM's mode registers, as the firmware's reads and writes see them.
    mode_regs: BTreeMap<MrKey, u8>,
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
        let mut mode_regs = BTreeMap::new();
        for chan in [false, true] {
            for dev in [false, true] {
                mode_regs.insert((chan, dev, MR4_REFRESH_RATE as u8), MR4_RESET);
                mode_regs.insert((chan, dev, MR8_BASIC_CONFIG as u8), MR8_8GB_X16);
            }
        }
        Sdc {
            storage: BTreeMap::new(),
            mode_regs,
            refresh_history: Vec::new(),
            mr_reads: 0,
        }
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
            cmd & MR_CHANNEL != 0,
            cmd & MR_DEVICE != 0,
            (cmd & MR_ADDR) as u8,
        );
        if cmd & MR_WRITE != 0 {
            let data = (cmd >> MR_WDATA_SHIFT) as u8;
            self.mode_regs.insert(key, data);
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

    #[test]
    fn mr4_reports_the_reference_boards_refresh_code() {
        let mut sdc = Sdc::new();
        // Read MR4 on channel 1 / device 1, the way `0x3ED6BA90` builds it.
        sdc.write(MR_PORT, Width::Word, 4 | MR_CHANNEL | MR_DEVICE)
            .unwrap();
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
        sdc.write(MR_PORT, Width::Word, 13).unwrap();
        assert_eq!((port(&mut sdc) & MR_RDATA) >> MR_RDATA_SHIFT, 0x5A);
        // ... and only for the channel/device it was written to.
        sdc.write(MR_PORT, Width::Word, MR_CHANNEL | 13).unwrap();
        assert_eq!((port(&mut sdc) & MR_RDATA) >> MR_RDATA_SHIFT, 0);
    }

    #[test]
    fn mr8_describes_a_2_gb_board() {
        let mut sdc = Sdc::new();
        sdc.write(MR_PORT, Width::Word, 8 | MR_DEVICE).unwrap();
        assert_eq!((port(&mut sdc) & MR_RDATA) >> MR_RDATA_SHIFT, 0x08);
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
