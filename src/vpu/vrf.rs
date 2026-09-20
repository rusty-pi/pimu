//! The VideoCore IV Vector Register File.
//!
//! The file is 64 rows of 64 bytes, and a row is **sixteen lanes of four
//! bytes** — not sixty-four bytes in a line. Element `e` of a register whose
//! elements are `w` bytes wide lives at byte `(e & 15) * 4 + (e >> 4) * w` of
//! its row: the lane is the element's low four bits, and the rest of the index
//! picks the sub-field inside that lane. So a row holds 16 32-bit elements, or
//! 32 16-bit ones, or 64 bytes — with consecutive elements four bytes apart,
//! interleaved, rather than side by side.
//!
//! A *horizontal* register is sixteen elements of one row from element `e0`; a
//! *vertical* one is the same element of sixteen consecutive rows, from the
//! 16-aligned band its coordinate names — see [`crate::vpu::insn::VecSlot`].
//!
//! All of that is measured, not inferred: `v8ld H(0,0),(r1)` from a page of
//! ascending bytes lands them four bytes apart, `v16ld HX(10,32)` two bytes
//! into each lane, and `v8ld V(0,17)` puts one byte at column 5 of each of
//! sixteen rows — run through the firmware's `EXECUTE_CODE` mailbox tag on a
//! Raspberry Pi 4B d03115.
//!
//! Alongside the bytes the unit keeps per-lane flags. Only the zero flag is
//! modelled, and only `v<w>bitplanes … SETF` writes it — the one producer the
//! executed encodings have.

/// Bytes per VRF row, and rows in the file.
pub const DIM: usize = 64;

/// Lanes in a vector register. Fixed by the architecture.
pub const LANES: u32 = 16;

pub struct Vrf {
    /// Boxed, not inline: [`Vpu`](crate::vpu::Vpu) is stepped a billion times a
    /// boot and the hot fields around this one have to stay in a few cache
    /// lines. Putting the 4 KiB of file in the middle of the struct instead cost
    /// 75% more wall clock on a whole firmware boot, measured, for a register
    /// file that a handful of instructions touch.
    bytes: Box<[u8; DIM * DIM]>,
    /// Per-lane zero flag, one bit per lane (bit 0 = lane 0).
    pub lane_z: u16,
}

impl Default for Vrf {
    fn default() -> Vrf {
        Vrf {
            bytes: Box::new([0; DIM * DIM]),
            lane_z: 0,
        }
    }
}

/// Byte offset of element `e`, `w` bytes wide, in row `row`.
#[inline]
fn offset(row: u8, e: u32, w: u32) -> usize {
    let lane = (e & 15) as usize;
    let sub = (e >> 4) as usize * w as usize;
    (row as usize % DIM) * DIM + (lane * 4 + sub) % DIM
}

impl Vrf {
    /// Read one element, zero-extended to a `u32`. Elements are little-endian,
    /// the same way the memory they are loaded from is.
    pub fn read(&self, row: u8, e: u32, w: u32) -> u32 {
        let off = offset(row, e, w);
        let mut v = 0u32;
        for i in 0..w as usize {
            v |= (self.bytes[(off + i) % (DIM * DIM)] as u32) << (8 * i);
        }
        v
    }

    /// Write one element, truncated to `w` bytes.
    pub fn write(&mut self, row: u8, e: u32, w: u32, value: u32) {
        let off = offset(row, e, w);
        for i in 0..w as usize {
            self.bytes[(off + i) % (DIM * DIM)] = (value >> (8 * i)) as u8;
        }
    }

    /// One byte of the file, for tests and diagnostics.
    pub fn byte(&self, row: usize, col: usize) -> u8 {
        self.bytes[(row % DIM) * DIM + (col % DIM)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Consecutive elements are a lane apart, not an element apart: element `e`
    /// sits at byte `(e & 15) * 4 + (e >> 4) * w`. Measured with
    /// `v8ld H(0,0),(r1)` over a page of ascending bytes, which lands them at
    /// columns 0, 4, 8 … of the row.
    #[test]
    fn elements_are_interleaved_across_the_lanes() {
        let mut vrf = Vrf::default();
        for e in 0..16 {
            vrf.write(3, e, 1, 0x40 + e);
        }
        for e in 0..16 {
            assert_eq!(vrf.byte(3, e as usize * 4), 0x40 + e as u8, "element {e}");
            assert_eq!(vrf.read(3, e, 1), 0x40 + e);
        }
        // The second band of 8-bit elements is one byte further into each lane.
        vrf.write(3, 16, 1, 0xAB);
        assert_eq!(vrf.byte(3, 1), 0xAB);
    }

    #[test]
    fn a_wider_element_fills_more_of_its_lane() {
        let mut vrf = Vrf::default();
        vrf.write(5, 1, 4, 0x1122_3344);
        assert_eq!(vrf.byte(5, 4), 0x44);
        assert_eq!(vrf.byte(5, 7), 0x11);
        assert_eq!(vrf.read(5, 1, 4), 0x1122_3344);
        // The same bytes seen as 16-bit elements: lane 1's two halves are
        // elements 1 and 17.
        assert_eq!(vrf.read(5, 1, 2), 0x3344);
        assert_eq!(vrf.read(5, 17, 2), 0x1122);
        // And as bytes, lane 1 holds elements 1, 17, 33 and 49.
        assert_eq!(vrf.read(5, 1, 1), 0x44);
        assert_eq!(vrf.read(5, 49, 1), 0x11);
    }

    /// `v16ld HX(10,32)` on hardware writes two bytes into each lane at
    /// offset 2 — the second band of 16-bit elements.
    #[test]
    fn the_second_16_bit_band_starts_two_bytes_into_the_lane() {
        let mut vrf = Vrf::default();
        vrf.write(10, 16, 2, 0x0201);
        assert_eq!(vrf.byte(10, 2), 0x01);
        assert_eq!(vrf.byte(10, 3), 0x02);
        assert_eq!(vrf.byte(10, 0), 0);
    }
}
