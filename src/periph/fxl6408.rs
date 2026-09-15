//! The Raspberry Pi 4B GPIO expander: an FXL6408 on the PMIC's I²C bus
//! (`0x7E20_5E00`), 7-bit address `0x43`.
//!
//! Its eight pins are the "external" GPIOs 128..135 of the board's dt-blob
//! (`pins_4b`: BT_ON, WL_ON, PWR_LED, GLOBAL_RESET, SD VDDIO, camera shutdown,
//! SD_PWR_ON) and Linux's `expgpio` lines, which it reaches only through the
//! firmware (`GET_GPIO_CONFIG` / `GET_GPIO_STATE` / `SET_GPIO_STATE`). With no
//! device at `0x43` every one of those answers `0xffffffff`, the 1.8 V SD I/O
//! regulator (`regulator-sd-io-1v8`, expander pin 4) never probes, and Linux's
//! SD controller defers forever. rpi-dev (d03115, rev 1.5) answers status 0 for
//! all eight pins.
//!
//! # Which part
//!
//! start4 carries two expander drivers and probes both on this bus, FXL6408
//! first: `0x43` (`gpio_expander_FXL6408.c`) reads register `0x01` and accepts
//! any value, then `0x10` (`gpio_expander_gpak.c`, a GreenPAK) insists on
//! `0x12` in register `0xFD`. Neither answered in the model, so the firmware
//! re-probed both on every expander access, including from inside the
//! mailbox handler.
//!
//! The part is only visible from the VPU side, and the release firmware's log
//! does not name it. The debug build does, indirectly: `start4db.elf` asserts
//! (`FXL6408_readreg`, `0x0EC335A0`) when a register read at `0x43` fails, and
//! a Pi 4B rev 1.5 (d03115) booted with `start_debug=1` logged no assert at
//! all (`vclog -a` empty), so its `0x43` read succeeded — the FXL6408 is the
//! part on that revision. Other revisions may carry the GreenPAK instead.
//!
//! # What start4 does with it
//!
//! From `start4db.elf`'s decompile (its asserts name the functions): probe
//! (`0x0ED18984`) reads `0x01` and writes `0x01 = 1` (software reset); set
//! level (`gpio_expander_FXL6408_set_level_internal`) writes the output
//! register `0x05` from a shadow; get level
//! (`gpio_expander_FXL6408_get_level_internal`) reads the input status `0x0F`
//! for pins it does not drive itself. Pin configuration goes through the
//! direction, high-Z and pull registers below.
//!
//! # Register map
//!
//! Names and the manufacturer id from mainline Linux
//! `drivers/gpio/gpio-fxl6408.c`; reset values from the onsemi FXL6408
//! datasheet:
//!
//! ```text
//!   0x01  device id / control   MF = 0b101 in bits 7..5; bit 0 = software reset
//!   0x03  I/O direction         1 = output                 reset 0x00
//!   0x05  output state          1 = drive high             reset 0x00
//!   0x07  output high-Z         1 = high-Z                 reset 0xFF
//!   0x09  input default state                              reset 0x00
//!   0x0B  pull enable           1 = pull resistor on       reset 0xFF
//!   0x0D  pull-down / pull-up   1 = pull-up                reset 0x00
//!   0x0F  input status          pin levels (read-only)
//!   0x11  interrupt mask                                   reset 0x00
//!   0x13  interrupt status      (read-only)
//! ```
//!
//! Nothing outside the board drives these pins in the model, so a pin's level
//! is whatever the part itself puts on it: the output state where it drives
//! the pin, otherwise its pull resistor, otherwise 0.

use crate::log::{Channel, Log};
use crate::spec::{fxl6408 as regs, Coverage};

/// 7-bit I²C address.
pub const ADDR: u8 = regs::BASE as u8;

// Register numbers, as the byte the register pointer holds.
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

/// Register `0x01` as it reads: the Fairchild manufacturer field
/// (`FXL6408_MF_FAIRCHILD`) in bits 7..5, the rest 0 — nothing checks it.
const ID_VALUE: u8 = regs::DEVICE_ID_RESET as u8;
const SW_RESET: u8 = regs::DEVICE_ID_SW_RESET_MASK as u8;

/// Every register in `specs/fxl6408.toml` is modelled.
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
    /// Set between the start of a write transfer and its first data byte: that
    /// byte is the register offset.
    pending_ptr: bool,
    /// Where [`Channel::Expander`] goes: every register access.
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

    /// Current value of a register, for tests and probes.
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
            // No pin ever changes on its own, so no input leaves its default.
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
        // Reset: all inputs, pulls on, pull-down.
        assert_eq!(read(&mut x, INPUT_STATUS), 0);
        write(&mut x, PULL_UP, 0b1000_0001);
        assert_eq!(read(&mut x, INPUT_STATUS), 0b1000_0001);
        // A high-Z output is not driving: still the pull.
        write(&mut x, IO_DIR, 0xFF);
        write(&mut x, OUTPUT, 0x00);
        assert_eq!(read(&mut x, INPUT_STATUS), 0b1000_0001);
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
