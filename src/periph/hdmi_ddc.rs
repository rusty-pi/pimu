//! HDMI DDC I²C masters (`0x7EF0_4500`, `0x7EF0_9500`).
//!
//! Each BCM2711 HDMI controller has its own I²C master for the DDC lines of
//! its connector — the bus a monitor's EDID EEPROM (slave `0x50`) sits on.
//! This is *not* the BSC of [`crate::periph::bsc`]: the Pi 4 device tree calls
//! it `brcm,bcm2711-hdmi-i2c` (Linux `drivers/i2c/busses/i2c-brcmstb.c`), a
//! different block with a different register layout. Ground truth from
//! a Raspberry Pi 4B d03115:
//!
//! ```text
//! /proc/device-tree/soc/i2c@7ef04500/compatible      brcm,bcm2711-hdmi-i2c
//! /proc/device-tree/soc/i2c@7ef04500/reg             0x7ef04500 0x100
//!                                                    0x7ef00b00 0x300
//! /proc/device-tree/soc/i2c@7ef04500/clock-frequency 97500
//! ```
//!
//! The node's second `reg` window is the "auto-i2c" block: sequencers that
//! write a list of values into this master and report when the transfer they
//! start has finished. start4 1.20190925 to 1.20200601 run one such list at
//! boot and wait for it with no timeout; later builds leave the block alone.
//! What they use of it is modelled here (`specs/hdmi_auto_i2c.toml`), and the
//! machine maps that window onto this device at [`AUTO_WINDOW`].
//!
//! Register map, offsets from the instance base — the `bsc_regs` struct of the
//! Linux driver, and exactly what start4's own driver drives (`0x3ECE69E2`
//! read, `0x3ECE6DEC` write, `0x3ECE6B00`/`0x3ECE6EC8` accessors):
//!
//! ```text
//!   0x00        CHIP_ADDRESS  slave address, already shifted: addr<<1 | read
//!   0x04..0x24  DATA_IN[8]    bytes to send, packed little-endian per word
//!   0x24        CNT           byte count (CNT1 in bits 0..5)
//!   0x28        CTL           DTF(0..1) SCL_SEL(4..5) INT_EN(6) DIV_CLK(7)
//!   0x2C        IIC_ENABLE    ENABLE(0) INTRP(1) NOACK(2) NOSTOP(4)
//!                             NOSTART(5) RESTART(6)
//!   0x30..0x50  DATA_OUT[8]   bytes received, packed the same way
//!   0x50        CTLHI         WAIT_DIS(0) IGNORE_ACK(1) DATAREG_SIZE(6)
//!   0x54        SCL_PARAM     timing — stored, otherwise ignored
//! ```
//!
//! A transfer starts when software writes `IIC_ENABLE` with `ENABLE` set, and
//! the two status bits are what the firmware waits on (`0x3ECE6D5C`, verbatim):
//!
//! ```c
//!   for (i = 20; i; i--) { if (read(0x2c) & 2) break; usleep(5000); }
//!   if (!i)                 { log("%s HDMI%d timed out"); return -1; }
//!   if (read(0x2c) & 4)     { log("%s HDMI%d no ACK");    return -2; }
//!   return 0;
//! ```
//!
//! So `INTRP` reads back as "the transfer has finished" (software writes it as
//! an enable), and `NOACK` as "the slave never acknowledged its address". Both
//! have to be *computed*, not RAM-backed: a register that reads back whatever
//! was last written to it reports every transfer instantly complete and
//! acknowledged, which is how the model used to conclude that a bus with
//! nothing on it had answered with 128 zero bytes. start4 then failed the EDID
//! checksum, and because no error was reported it never bumped its per-block
//! attempt counter and re-read EDID forever.
//!
//! Like [`crate::periph::bsc`], completion is timed in simulated microseconds
//! off the system timer rather than in retired instructions: `INTRP` lands
//! once the bytes would have been clocked out at the bus rate, so it can never
//! appear inside the register write that started the transfer.
//!
//! The reference board has no monitor on either connector — a
//! Raspberry Pi 4B d03115 reports both `card1-HDMI-A-{1,2}/status` as
//! `disconnected` — so by default no slave answers and every transfer completes
//! `INTRP | NOACK`, which is what makes start4 log
//! `HDMI%d:EDID error reading EDID block 0 attempt 0` and give up.
//! [`HdmiDdc::with_edid`] attaches an EDID EEPROM instead, for the day the HDMI
//! mode-set path is worth exercising.

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::hdmi_auto_i2c::{
    CLEAR as AUTO_CLEAR, CTL2 as AUTO_CTL2, DONE as AUTO_DONE, LIST2 as AUTO_LIST2,
    LIST2_COUNT as AUTO_LIST2_COUNT, LIST2_STRIDE as AUTO_LIST2_STRIDE, SIZE as AUTO_SIZE,
    START as AUTO_START,
};
use crate::spec::hdmi_ddc::{
    CHIP_ADDRESS, CNT, CNT_CNT1_MASK, CTL, CTLHI, CTLHI_IGNORE_ACK_MASK as CTLHI_IGNORE_ACK,
    CTL_DTF_SHIFT, DATA_IN, DATA_IN_COUNT, DATA_IN_STRIDE, DATA_OUT, DATA_OUT_COUNT,
    DATA_OUT_STRIDE, IIC_ENABLE, IIC_ENABLE_ENABLE_MASK as EN_ENABLE,
    IIC_ENABLE_INTRP_MASK as EN_INTRP, IIC_ENABLE_NOACK_MASK as EN_NOACK, SCL_PARAM,
};
use crate::spec::Coverage;

/// Every register in `specs/hdmi_ddc.toml` is modelled, on both connectors.
pub const COVERAGE: Coverage = Coverage {
    block: "hdmi_ddc",
    decoded: &[
        CHIP_ADDRESS,
        DATA_IN,
        CNT,
        CTL,
        IIC_ENABLE,
        DATA_OUT,
        CTLHI,
        SCL_PARAM,
    ],
};

/// What start4 uses of the auto-i2c window is modelled; the rest is stored.
pub const COVERAGE_AUTO: Coverage = Coverage {
    block: "hdmi_auto_i2c",
    decoded: &[AUTO_CTL2, AUTO_LIST2, AUTO_START, AUTO_CLEAR, AUTO_DONE],
};

/// The machine adds this to an access in the auto-i2c window, so that one
/// device answers both of its device-tree node's `reg` windows.
pub const AUTO_WINDOW: u32 = 0x1000;
const AUTO_WORDS: usize = (AUTO_SIZE / 4) as usize;
/// The one channel whose list is known to sit at `LIST2`.
const AUTO_CHANNEL: u32 = 2;
/// A list command that writes its value to the master's register at
/// `4 * (command & 0xFF)`.
const AUTO_CMD_WRITE: u32 = 0x100;

/// Eight data registers each way, four bytes apiece.
const DATA_REGS: usize = DATA_IN_COUNT as usize;
const DATA_IN_LAST: u32 = DATA_IN + (DATA_IN_COUNT - 1) * DATA_IN_STRIDE;
const DATA_OUT_LAST: u32 = DATA_OUT + (DATA_OUT_COUNT - 1) * DATA_OUT_STRIDE;
/// Longest transfer the block can do in one go.
pub const MAX_BYTES: usize = DATA_REGS * 4;

/// `CTL.DTF` bit 0: a read.
const CTL_DTF_READ: u32 = 1 << CTL_DTF_SHIFT;

/// 7-bit address of a monitor's EDID EEPROM on the DDC bus.
pub const EDID_ADDR: u32 = 0x50;

pub struct HdmiDdc {
    name: &'static str,
    chip_address: u32,
    cnt: u32,
    ctl: u32,
    ctlhi: u32,
    scl_param: u32,
    /// `IIC_ENABLE` as written, status bits masked out — they are computed.
    iic_enable: u32,
    data_in: [u32; DATA_REGS],
    data_out: [u32; DATA_REGS],
    /// A transfer still on the wire: `(simulated µs at which it finishes,
    /// whether the slave acknowledged)`.
    pending: Option<(u64, bool)>,
    /// Latched status of the last completed transfer.
    done: bool,
    noack: bool,
    now_us: u64,
    /// The EDID EEPROM on the bus, if a monitor is plugged in, and the byte
    /// offset its address pointer currently sits at.
    edid: Option<Vec<u8>>,
    edid_ptr: usize,
    /// The auto-i2c window, as written.
    auto: [u32; AUTO_WORDS],
    /// Channels whose list has run and whose transfer is still on the wire.
    auto_busy: u32,
    /// `DONE`.
    auto_done: u32,
}

impl HdmiDdc {
    /// A connector with nothing plugged into it.
    pub fn new(name: &'static str) -> HdmiDdc {
        HdmiDdc {
            name,
            chip_address: 0,
            cnt: 0,
            ctl: 0,
            ctlhi: 0,
            scl_param: 0,
            iic_enable: 0,
            data_in: [0; DATA_REGS],
            data_out: [0; DATA_REGS],
            pending: None,
            done: false,
            noack: false,
            now_us: 0,
            edid: None,
            edid_ptr: 0,
            auto: [0; AUTO_WORDS],
            auto_busy: 0,
            auto_done: 0,
        }
    }

    /// A connector with a monitor on it, answering at [`EDID_ADDR`] with
    /// `edid`. Nothing on the boot path builds one of these yet; it is what
    /// the HDMI mode-set path would need.
    pub fn with_edid(mut self, edid: Vec<u8>) -> HdmiDdc {
        self.edid = Some(edid);
        self
    }

    /// Bus rate, from the device tree's `clock-frequency`. The `CTL` clock
    /// selectors (`SCL_SEL` / `DIV_CLK`) pick between a handful of rates on
    /// real silicon; the only thing that rides on the exact figure here is how
    /// long a transfer takes, and start4 programs the bus the device tree
    /// describes, so take that rate and store the selectors.
    const BUS_HZ: u64 = 97_500;

    /// How long `bytes` data bytes plus the address byte take on the wire, in
    /// microseconds — nine bits each, counting the ACK slot.
    fn wire_us(&self, bytes: usize) -> u64 {
        (9 * (bytes as u64 + 1) * 1_000_000 / Self::BUS_HZ).max(1)
    }

    /// Advance simulated time; latch the status of a transfer that has
    /// finished clocking out.
    pub fn advance_to(&mut self, now_us: u64) {
        self.now_us = now_us;
        if let Some((deadline, acked)) = self.pending {
            if deadline <= now_us {
                self.pending = None;
                self.done = true;
                // `CTLHI.IGNORE_ACK` makes the master carry on regardless, so
                // an unACKed address is not reported.
                self.noack = !acked && self.ctlhi & CTLHI_IGNORE_ACK == 0;
            }
        }
    }

    /// Live `IIC_ENABLE`: what software wrote, plus the two status bits.
    fn enable_status(&self) -> u32 {
        let mut v = self.iic_enable & !(EN_INTRP | EN_NOACK);
        if self.done {
            v |= EN_INTRP;
        }
        if self.noack {
            v |= EN_NOACK;
        }
        v
    }

    /// Run the transfer the `ENABLE` write just kicked off.
    fn start(&mut self) {
        let read = self.ctl & CTL_DTF_READ != 0;
        let addr = (self.chip_address >> 1) & 0x7F;
        let count = ((self.cnt & CNT_CNT1_MASK) as usize).min(MAX_BYTES);
        let acked = self.edid.is_some() && addr == EDID_ADDR;

        if acked {
            if read {
                self.fill_data_out(count);
            } else {
                self.take_data_in(count);
            }
        } else if read {
            self.data_out = [0; DATA_REGS];
        }

        self.done = false;
        self.noack = false;
        self.pending = Some((self.now_us + self.wire_us(count), acked));
    }

    /// A write transfer to the EEPROM: its payload is the byte offset that the
    /// read after it starts from.
    fn take_data_in(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        self.edid_ptr = (self.data_in[0] & 0xFF) as usize;
    }

    /// Move `count` bytes out of the EEPROM into `DATA_OUT`, little-endian
    /// within each word, and advance its address pointer — which is what makes
    /// the driver's `NOSTART` continuation chunks pick up where the last one
    /// stopped. The EEPROM wraps at the end of its address space, the way an
    /// EDID ROM does.
    fn fill_data_out(&mut self, count: usize) {
        self.data_out = [0; DATA_REGS];
        let Some(edid) = self.edid.as_ref() else {
            return;
        };
        if edid.is_empty() {
            return;
        }
        for i in 0..count {
            let b = edid[(self.edid_ptr + i) % edid.len()] as u32;
            self.data_out[i / 4] |= b << (8 * (i % 4));
        }
        self.edid_ptr = (self.edid_ptr + count) % edid.len();
    }

    fn auto_word(&self, off: u32) -> u32 {
        self.auto.get((off / 4) as usize).copied().unwrap_or(0)
    }

    fn auto_read(&mut self, off: u32) -> u32 {
        match off {
            AUTO_DONE => {
                self.auto_settle();
                self.auto_done
            }
            _ => self.auto_word(off),
        }
    }

    fn auto_write(&mut self, off: u32, value: u32) {
        if let Some(word) = self.auto.get_mut((off / 4) as usize) {
            *word = value;
        }
        match off {
            AUTO_START => self.auto_start(value),
            AUTO_CLEAR => self.auto_done &= !value,
            _ => {}
        }
    }

    /// Run the list of each channel set in a `START` write. Only channel 2's
    /// list location is known; any other channel reports done at once.
    fn auto_start(&mut self, channels: u32) {
        for ch in 0..32 {
            let bit = 1 << ch;
            if channels & bit == 0 {
                continue;
            }
            if ch == AUTO_CHANNEL && self.auto_run_list() {
                self.auto_busy |= bit;
            } else {
                self.auto_done |= bit;
            }
        }
    }

    /// Write channel 2's list into the master, pair by pair. True if it
    /// started a transfer, which the channel then waits for.
    fn auto_run_list(&mut self) -> bool {
        let mut started = false;
        for i in (0..AUTO_LIST2_COUNT - 1).step_by(2) {
            let cmd = self.auto_word(AUTO_LIST2 + i * AUTO_LIST2_STRIDE);
            if cmd & AUTO_CMD_WRITE == 0 {
                break;
            }
            let value = self.auto_word(AUTO_LIST2 + (i + 1) * AUTO_LIST2_STRIDE);
            let reg = (cmd & 0xFF) * 4;
            let _ = self.write(reg, Width::Word, value);
            if reg == IIC_ENABLE {
                started = value & EN_ENABLE != 0;
            }
        }
        started
    }

    /// Move the channels whose transfer has finished from busy to `DONE`.
    fn auto_settle(&mut self) {
        if self.auto_busy != 0 && self.pending.is_none() {
            self.auto_done |= self.auto_busy;
            self.auto_busy = 0;
        }
    }
}

impl MmioDevice for HdmiDdc {
    fn name(&self) -> &'static str {
        self.name
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        if let Some(auto) = off.checked_sub(AUTO_WINDOW) {
            return Ok(self.auto_read(auto));
        }
        Ok(match off {
            CHIP_ADDRESS => self.chip_address,
            DATA_IN..=DATA_IN_LAST => self.data_in[((off - DATA_IN) / DATA_IN_STRIDE) as usize],
            CNT => self.cnt,
            CTL => self.ctl,
            IIC_ENABLE => self.enable_status(),
            DATA_OUT..=DATA_OUT_LAST => {
                self.data_out[((off - DATA_OUT) / DATA_OUT_STRIDE) as usize]
            }
            CTLHI => self.ctlhi,
            SCL_PARAM => self.scl_param,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        if let Some(auto) = off.checked_sub(AUTO_WINDOW) {
            self.auto_write(auto, value);
            return Ok(());
        }
        match off {
            CHIP_ADDRESS => self.chip_address = value,
            DATA_IN..=DATA_IN_LAST => {
                self.data_in[((off - DATA_IN) / DATA_IN_STRIDE) as usize] = value
            }
            CNT => self.cnt = value,
            CTL => self.ctl = value,
            IIC_ENABLE => {
                self.iic_enable = value & !(EN_INTRP | EN_NOACK);
                if value & EN_ENABLE != 0 {
                    self.start();
                } else {
                    // Disabling the master abandons the transfer and drops its
                    // status; start4 does this after every one.
                    self.pending = None;
                    self.done = false;
                    self.noack = false;
                }
            }
            CTLHI => self.ctlhi = value,
            SCL_PARAM => self.scl_param = value,
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto(d: &mut HdmiDdc, off: u32, value: u32) {
        d.write(AUTO_WINDOW + off, Width::Word, value).unwrap();
    }

    /// The one list start4 1.20190925 runs, on a connector with nothing on
    /// it: the master runs the transfer the list sets up, and channel 2's
    /// `DONE` bit comes up once that has finished (#76).
    #[test]
    fn an_auto_i2c_list_runs_through_the_master() {
        let mut d = HdmiDdc::new("hdmi-ddc0");
        let list = [
            0x10B, 0, 0x100, 0x60, 0x10A, 0xD0, 0x109, 2, 0x114, 0x40, 0x101, 0, 0x10B, 1,
        ];
        auto(&mut d, AUTO_CLEAR, 1 << 2);
        for (i, v) in list.into_iter().enumerate() {
            auto(&mut d, AUTO_LIST2 + i as u32 * AUTO_LIST2_STRIDE, v);
        }
        auto(&mut d, AUTO_CTL2, 0x180C_0005);
        auto(&mut d, AUTO_START, 1 << 2);
        assert_eq!(d.read(CHIP_ADDRESS, Width::Word), Ok(0x60));
        assert_eq!(d.read(CNT, Width::Word), Ok(2));
        assert_eq!(d.read(AUTO_WINDOW + AUTO_DONE, Width::Word), Ok(0));

        d.advance_to(1_000);
        assert_eq!(d.read(AUTO_WINDOW + AUTO_DONE, Width::Word), Ok(1 << 2));
        assert_eq!(
            d.read(IIC_ENABLE, Width::Word),
            Ok(EN_ENABLE | EN_INTRP | EN_NOACK)
        );
        auto(&mut d, AUTO_CLEAR, 1 << 2);
        assert_eq!(d.read(AUTO_WINDOW + AUTO_DONE, Width::Word), Ok(0));
    }
}
