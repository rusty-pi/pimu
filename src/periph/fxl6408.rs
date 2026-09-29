//! The Raspberry Pi 4B GPIO expander: an FXL6408 on the PMIC's I²C bus
//! (`0x7E20_5E00`), 7-bit address `0x43`.
//!
//! Registers and fields: `specs/fxl6408.toml` ([`crate::spec::fxl6408`]).
//!
//! Its eight pins are the "external" GPIOs 128..135 of the board's dt-blob
//! (`pins_4b`: `BT_ON`, `WL_ON`, `PWR_LED`, `GLOBAL_RESET`, SD VDDIO, camera
//! shutdown, `SD_PWR_ON`) and Linux's `expgpio` lines, which it reaches only
//! through the firmware (`GET_GPIO_CONFIG` / `GET_GPIO_STATE` /
//! `SET_GPIO_STATE`). Without a device at `0x43` every one of those answers
//! `0xffffffff`, the 1.8 V SD I/O regulator (`regulator-sd-io-1v8`, expander
//! pin 4) never probes, and Linux's SD controller defers forever.
//!
//! # Which part
//!
//! start4 carries two expander drivers and probes both on this bus, FXL6408
//! first: `0x43` (`gpio_expander_FXL6408.c`) reads register `DEVICE_ID` and
//! accepts any value, then `0x10` (`gpio_expander_gpak.c`, a GreenPAK) insists
//! on `0x12` in register `0xFD`. A part that answers neither is re-probed on
//! every expander access, including from inside the mailbox handler.
//!
//! Which part is fitted was settled on a Raspberry Pi 4B d03115: `start4db.elf`
//! asserts in `FXL6408_readreg` when a read at `0x43` fails, and that board
//! booted with `start_debug=1` logged no assert, so the FXL6408 is the part on
//! that revision. Other revisions may carry the GreenPAK instead.
//!
//! Nothing outside the board drives these pins, so a pin's level is whatever
//! the part puts on it: the output state where it drives, else its pull, else
//! 0. A Raspberry Pi 4B d03115 answers status 0 for all eight pins.

use crate::log::{Channel, Log};
use crate::spec::{fxl6408 as regs, Coverage};

pub const ADDR: u8 = regs::BASE as u8;

const DEVICE_ID: u8 = regs::DEVICE_ID as u8;
const IO_DIR: u8 = regs::IO_DIR as u8;
const OUTPUT: u8 = regs::OUTPUT as u8;
const OUTPUT_HIGH_Z: u8 = regs::OUTPUT_HIGH_Z as u8;
const INPUT_DEFAULT: u8 = regs::INPUT_DEFAULT as u8;
const PULL_ENABLE: u8 = regs::PULL_ENABLE as u8;
const PULL_UP: u8 = regs::PULL_UP as u8;
const INPUT_STATUS: u8 = regs::INPUT_STATUS as u8;
const INT_MASK: u8 = regs::INT_MASK as u8;
const INT_STATUS: u8 = regs::INT_STATUS as u8;

/// Pin 6, the card's power switch (GPIO 134).
const SD_PWR_ON: u8 = 1 << 6;

/// `DEVICE_ID` as it reads: the Fairchild manufacturer field in bits 7..5, the
/// rest 0 — nothing checks it.
const ID_VALUE: u8 = regs::DEVICE_ID_RESET as u8;
const SW_RESET: u8 = regs::DEVICE_ID_SW_RESET_MASK as u8;

pub const COVERAGE: Coverage = Coverage {
    block: "fxl6408",
    decoded: &[
        regs::DEVICE_ID,
        regs::IO_DIR,
        regs::OUTPUT,
        regs::OUTPUT_HIGH_Z,
        regs::INPUT_DEFAULT,
        regs::PULL_ENABLE,
        regs::PULL_UP,
        regs::INPUT_STATUS,
        regs::INT_MASK,
        regs::INT_STATUS,
    ],
};

#[derive(Debug, Clone)]
pub struct Fxl6408 {
    io_dir: u8,
    output: u8,
    high_z: u8,
    input_default: u8,
    pull_enable: u8,
    pull_up: u8,
    int_mask: u8,
    ptr: u8,
    pending_ptr: bool,
    pub log: Log,
}

impl Default for Fxl6408 {
    fn default() -> Self {
        Fxl6408::new()
    }
}

impl Fxl6408 {
    pub fn new() -> Fxl6408 {
        Fxl6408 {
            io_dir: regs::IO_DIR_RESET as u8,
            output: regs::OUTPUT_RESET as u8,
            high_z: regs::OUTPUT_HIGH_Z_RESET as u8,
            input_default: regs::INPUT_DEFAULT_RESET as u8,
            pull_enable: regs::PULL_ENABLE_RESET as u8,
            pull_up: regs::PULL_UP_RESET as u8,
            int_mask: regs::INT_MASK_RESET as u8,
            ptr: 0,
            pending_ptr: false,
            log: Log::default(),
        }
    }

    fn reset(&mut self) {
        *self = Fxl6408 {
            ptr: self.ptr,
            log: std::mem::take(&mut self.log),
            ..Fxl6408::new()
        };
    }

    /// Level on each pin: driven where the part drives it, else pulled, else 0.
    pub fn levels(&self) -> u8 {
        let driven = self.io_dir & !self.high_z;
        let pulled = !driven & self.pull_enable;
        (driven & self.output) | (pulled & self.pull_up)
    }

    /// Whether the card has power: the board pulls `SD_PWR_ON` up, so it is
    /// off only while the part drives the pin low.
    pub fn sd_powered(&self) -> bool {
        let driven_low = self.io_dir & !self.high_z & !self.output;
        driven_low & SD_PWR_ON == 0
    }

    pub fn reg(&self, r: u8) -> u8 {
        match r {
            DEVICE_ID => ID_VALUE,
            IO_DIR => self.io_dir,
            OUTPUT => self.output,
            OUTPUT_HIGH_Z => self.high_z,
            INPUT_DEFAULT => self.input_default,
            PULL_ENABLE => self.pull_enable,
            PULL_UP => self.pull_up,
            INPUT_STATUS => self.levels(),
            INT_MASK => self.int_mask,
            INT_STATUS => 0,
            _ => 0,
        }
    }

    fn write_reg(&mut self, r: u8, v: u8) {
        match r {
            DEVICE_ID if v & SW_RESET != 0 => self.reset(),
            IO_DIR => self.io_dir = v,
            OUTPUT => self.output = v,
            OUTPUT_HIGH_Z => self.high_z = v,
            INPUT_DEFAULT => self.input_default = v,
            PULL_ENABLE => self.pull_enable = v,
            PULL_UP => self.pull_up = v,
            INT_MASK => self.int_mask = v,
            _ => {}
        }
    }
}

impl crate::periph::bsc::I2cSlave for Fxl6408 {
    fn responds_to(&self, addr: u8) -> bool {
        addr == ADDR
    }

    fn begin(&mut self, _addr: u8, read: bool) {
        self.pending_ptr = !read;
    }

    fn write_byte(&mut self, b: u8) {
        if self.pending_ptr {
            self.ptr = b;
            self.pending_ptr = false;
            return;
        }
        crate::log!(
            self.log,
            Channel::Expander,
            "W {:02x} = {:02x}",
            self.ptr,
            b
        );
        self.write_reg(self.ptr, b);
        self.ptr = self.ptr.wrapping_add(1);
    }

    fn read_byte(&mut self) -> u8 {
        let v = self.reg(self.ptr);
        crate::log!(
            self.log,
            Channel::Expander,
            "R {:02x} -> {:02x}",
            self.ptr,
            v
        );
        self.ptr = self.ptr.wrapping_add(1);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::periph::bsc::I2cSlave;

    fn write(x: &mut Fxl6408, reg: u8, v: u8) {
        x.begin(ADDR, false);
        x.write_byte(reg);
        x.write_byte(v);
    }

    fn read(x: &mut Fxl6408, reg: u8) -> u8 {
        x.begin(ADDR, false);
        x.write_byte(reg);
        x.begin(ADDR, true);
        x.read_byte()
    }

    #[test]
    fn probe_reads_fairchild_id() {
        let mut x = Fxl6408::new();
        assert_eq!(read(&mut x, DEVICE_ID) >> 5, 0b101);
    }

    #[test]
    fn driven_pins_read_back_their_output() {
        let mut x = Fxl6408::new();
        write(&mut x, IO_DIR, 0b0101_0000);
        write(&mut x, OUTPUT_HIGH_Z, 0x00);
        write(&mut x, OUTPUT, 0b0001_0000);
        write(&mut x, PULL_ENABLE, 0x00);
        assert_eq!(read(&mut x, INPUT_STATUS), 0b0001_0000);
    }

    #[test]
    fn undriven_pins_follow_their_pull() {
        let mut x = Fxl6408::new();
        assert_eq!(read(&mut x, INPUT_STATUS), 0);
        write(&mut x, PULL_UP, 0b1000_0001);
        assert_eq!(read(&mut x, INPUT_STATUS), 0b1000_0001);
        write(&mut x, IO_DIR, 0xFF);
        write(&mut x, OUTPUT, 0x00);
        assert_eq!(read(&mut x, INPUT_STATUS), 0b1000_0001);
    }

    #[test]
    fn card_is_powered_unless_the_pin_is_driven_low() {
        let mut x = Fxl6408::new();
        assert!(x.sd_powered());
        write(&mut x, IO_DIR, SD_PWR_ON);
        write(&mut x, OUTPUT_HIGH_Z, 0x00);
        write(&mut x, OUTPUT, 0);
        assert!(!x.sd_powered());
        write(&mut x, OUTPUT, SD_PWR_ON);
        assert!(x.sd_powered());
        write(&mut x, OUTPUT, 0);
        write(&mut x, DEVICE_ID, SW_RESET);
        assert!(x.sd_powered());
    }

    #[test]
    fn software_reset_restores_defaults() {
        let mut x = Fxl6408::new();
        write(&mut x, IO_DIR, 0xFF);
        write(&mut x, OUTPUT, 0xAA);
        write(&mut x, DEVICE_ID, SW_RESET);
        assert_eq!(read(&mut x, IO_DIR), 0x00);
        assert_eq!(read(&mut x, OUTPUT), 0x00);
        assert_eq!(read(&mut x, OUTPUT_HIGH_Z), 0xFF);
    }
}
