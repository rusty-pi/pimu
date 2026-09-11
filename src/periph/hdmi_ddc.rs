//! HDMI DDC I²C masters (`0x7EF0_4500`, `0x7EF0_9500`).
//!
//! Each BCM2711 HDMI controller has its own I²C master for the DDC lines of
//! its connector — the bus a monitor's EDID EEPROM (slave `0x50`) sits on.
//! This is *not* the BSC of [`crate::periph::bsc`]: the Pi 4 device tree calls
//! it `brcm,bcm2711-hdmi-i2c` (Linux `drivers/i2c/busses/i2c-brcmstb.c`), a
//! different block with a different register layout. Ground truth from
//! `rpi-dev`:
//!
//! ```text
//! /proc/device-tree/soc/i2c@7ef04500/compatible      brcm,bcm2711-hdmi-i2c
//! /proc/device-tree/soc/i2c@7ef04500/reg             0x7ef04500 0x100
//!                                                    0x7ef00b00 0x300
//! /proc/device-tree/soc/i2c@7ef04500/clock-frequency 97500
//! ```
//!
//! (The second `reg` window is the "auto-i2c" block, which start4 does not
//! use; only the master below is modelled.)
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
//! The reference board has no monitor on either connector — `rpi-dev` reports
//! both `card1-HDMI-A-{1,2}/status` as `disconnected` — so by default no slave
//! answers and every transfer completes `INTRP | NOACK`, which is what makes
//! start4 log `HDMI%d:EDID error reading EDID block 0 attempt 0` and give up.
//! [`HdmiDdc::with_edid`] attaches an EDID EEPROM instead, for the day the
//! HDMI mode-set path is worth exercising.

use crate::bus::{BusResult, MmioDevice, Width};

/// DDC master of HDMI0.
pub const HDMI0_BASE: u32 = 0x7EF0_4500;
/// DDC master of HDMI1.
pub const HDMI1_BASE: u32 = 0x7EF0_9500;
/// Window size from the device tree's `reg`.
pub const SIZE: u32 = 0x100;

const CHIP_ADDRESS: u32 = 0x00;
const DATA_IN: u32 = 0x04;
const CNT: u32 = 0x24;
const CTL: u32 = 0x28;
const IIC_ENABLE: u32 = 0x2C;
const DATA_OUT: u32 = 0x30;
const CTLHI: u32 = 0x50;
const SCL_PARAM: u32 = 0x54;

/// Eight data registers each way, four bytes apiece.
const DATA_REGS: usize = 8;
/// Longest transfer the block can do in one go.
pub const MAX_BYTES: usize = DATA_REGS * 4;

// CTL bits.
const CTL_DTF_READ: u32 = 1 << 0;

// IIC_ENABLE bits.
const EN_ENABLE: u32 = 1 << 0;
const EN_INTRP: u32 = 1 << 1;
const EN_NOACK: u32 = 1 << 2;

// CTLHI bits.
const CTLHI_IGNORE_ACK: u32 = 1 << 1;

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
        let count = (self.cnt as usize & 0x3F).min(MAX_BYTES);
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
}

impl MmioDevice for HdmiDdc {
    fn name(&self) -> &'static str {
        self.name
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        Ok(match off {
            CHIP_ADDRESS => self.chip_address,
            DATA_IN..=0x20 => self.data_in[((off - DATA_IN) / 4) as usize],
            CNT => self.cnt,
            CTL => self.ctl,
            IIC_ENABLE => self.enable_status(),
            DATA_OUT..=0x4C => self.data_out[((off - DATA_OUT) / 4) as usize],
            CTLHI => self.ctlhi,
            SCL_PARAM => self.scl_param,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        match off {
            CHIP_ADDRESS => self.chip_address = value,
            DATA_IN..=0x20 => self.data_in[((off - DATA_IN) / 4) as usize] = value,
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
