//! The ID EEPROM on a HAT, the board that sits on the 40-pin header.
//!
//! An AT24C32-class part at 7-bit address `0x50` on I²C0 (`bsc0`, GPIO 0 and 1
//! on ALT0), which is the bus the firmware gives to the header while it looks
//! for one. Only while: the part answers when those two pins carry the master
//! and goes unACKed otherwise, since the firmware runs the same master on GPIO
//! 44/45 for the camera and display, where no HAT is
//! ([`crate::machine::Machine::route_gpio_pins`]). Addresses `0x51`..`0x53`
//! are what a stacked HAT would answer; only the first is modelled.
//!
//! The usual two-byte-addressed serial EEPROM: a write transfer carries a
//! big-endian address and leaves the pointer there, a read streams from the
//! pointer and wraps at the end of the part. Nothing is written to — a HAT
//! EEPROM is write-protected and the firmware only reads it — and the ID EEPROM
//! format itself is not parsed, only served.

use super::bsc::I2cSlave;

pub const HAT_ADDR: u8 = 0x50;

pub struct HatEeprom {
    bytes: Vec<u8>,
    ptr: usize,
    taken: u8,
    next: usize,
}

impl HatEeprom {
    pub fn new(mut bytes: Vec<u8>) -> HatEeprom {
        if bytes.is_empty() {
            bytes.push(0xFF);
        }
        HatEeprom {
            bytes,
            ptr: 0,
            taken: 0,
            next: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl I2cSlave for HatEeprom {
    fn responds_to(&self, addr: u8) -> bool {
        addr == HAT_ADDR
    }

    fn begin(&mut self, _addr: u8, read: bool) {
        if !read {
            self.taken = 0;
            self.next = 0;
        }
    }

    fn write_byte(&mut self, b: u8) {
        // Only the two address bytes are taken; a HAT EEPROM is not written
        // to by the firmware, so anything after them is dropped on the floor.
        if self.taken < 2 {
            self.next = self.next << 8 | usize::from(b);
            self.taken += 1;
            if self.taken == 2 {
                self.ptr = self.next;
            }
        }
    }

    fn read_byte(&mut self) -> u8 {
        let b = self.bytes[self.ptr % self.bytes.len()];
        self.ptr = self.ptr.wrapping_add(1);
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_moves_the_pointer_and_reads_stream_from_it() {
        let mut e = HatEeprom::new((0..=255u8).collect());
        e.begin(HAT_ADDR, false);
        e.write_byte(0x00);
        e.write_byte(0x10);
        e.begin(HAT_ADDR, true);
        assert_eq!([e.read_byte(), e.read_byte()], [0x10, 0x11]);

        e.begin(HAT_ADDR, true);
        assert_eq!(e.read_byte(), 0x12);
    }

    #[test]
    fn reads_wrap_at_the_end_of_the_part() {
        let mut e = HatEeprom::new(vec![1, 2, 3]);
        e.begin(HAT_ADDR, false);
        e.write_byte(0);
        e.write_byte(2);
        e.begin(HAT_ADDR, true);
        assert_eq!([e.read_byte(), e.read_byte()], [3, 1]);
    }

    #[test]
    fn nothing_else_on_the_bus_is_answered() {
        let e = HatEeprom::new(vec![0]);
        assert!(e.responds_to(0x50));
        assert!(!e.responds_to(0x51));
        assert!(!e.responds_to(0x1d));
    }
}
