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
//! reference board's 2 and leaves every other mode register at its reset 0.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

/// Status word the firmware polls for a lock/ready edge (`0x8000a3e0` reads
/// `[0x7E00_1080 + 0x1C]`). Bit 31 is "ready"; we always report it set. The
/// sub-controllers are 0x80 apart, so `…9C`, `…11C`, … are all status slots.
const STATUS_SLOT: u32 = 0x1C;
const STATUS_STRIDE: u32 = 0x80;
const STATUS_READY: u32 = 1 << 31;

/// Mode-register access port: the `+0x1C` slot of the sub-controller at `+0x80`.
const MR_PORT: u32 = STATUS_STRIDE + STATUS_SLOT;

/// Command-word layout of [`MR_PORT`].
const MR_ADDR: u32 = 0x0000_00FF;
const MR_WDATA_SHIFT: u32 = 8;
const MR_RDATA: u32 = 0x00FF_0000;
const MR_RDATA_SHIFT: u32 = 16;
const MR_CHANNEL: u32 = 1 << 24;
const MR_DEVICE: u32 = 1 << 25;
/// Set for a mode-register *write*; clear for a read.
const MR_WRITE: u32 = 1 << 28;
/// Transfer failed. Never set here: the modelled DRAM always answers.
const MR_ERROR: u32 = 1 << 30;
/// Transfer complete — the same bit the plain ready polls look at.
const MR_DONE: u32 = STATUS_READY;

/// LPDDR4 MR4 (refresh rate / temperature). Value the reference board reports
/// right after the ARM handover (`vc4-boot.log`: `sdram refresh 1562->3124 (2)`
/// — start4 scales the refresh interval by `1 << (3 - code)`, so 2 means the
/// die is cool enough to refresh at half the nominal rate).
const MR4_REFRESH_RATE: u32 = 4;
const MR4_RESET: u8 = 2;

/// Refresh-interval word: `[+0x04] >> 16` is the interval start4 rescales from
/// the MR4 code.
const REFRESH_WORD: u32 = 0x04;

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
        if off >= STATUS_STRIDE && off % STATUS_STRIDE == STATUS_SLOT {
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
            let interval = value >> 16;
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
        sdc.write(MR_PORT, Width::Word, MR_WRITE | (0x5A << MR_WDATA_SHIFT) | 13)
            .unwrap();
        sdc.write(MR_PORT, Width::Word, 13).unwrap();
        assert_eq!((port(&mut sdc) & MR_RDATA) >> MR_RDATA_SHIFT, 0x5A);
        // ... and only for the channel/device it was written to.
        sdc.write(MR_PORT, Width::Word, MR_CHANNEL | 13).unwrap();
        assert_eq!((port(&mut sdc) & MR_RDATA) >> MR_RDATA_SHIFT, 0);
    }

    #[test]
    fn unwritten_registers_read_as_zero_and_ready() {
        let mut sdc = Sdc::new();
        sdc.write(MR_PORT, Width::Word, 8).unwrap();
        let got = port(&mut sdc);
        assert_eq!(got & MR_RDATA, 0);
        assert_eq!(got & MR_DONE, MR_DONE);
    }

    #[test]
    fn plain_status_slots_still_read_ready() {
        let mut sdc = Sdc::new();
        assert_eq!(
            sdc.read(STATUS_STRIDE * 2 + STATUS_SLOT, Width::Word).unwrap() & STATUS_READY,
            STATUS_READY
        );
        // Timing words are plain storage.
        sdc.write(0x04, Width::Word, 0x061a_0474).unwrap();
        assert_eq!(sdc.read(0x04, Width::Word).unwrap(), 0x061a_0474);
    }
}
