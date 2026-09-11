//! The VideoCore IV Vector Register File.
//!
//! The VRF is a 64x64 array of bytes. A vector register is a 16-element window
//! into it, named by a slot descriptor (element width + horizontal/vertical +
//! column band) and a 6-bit coordinate — see [`crate::vpu::insn::VecSlot`].
//!
//! Only *horizontal* windows are modelled: a row of 16 consecutive elements
//! starting at byte column `x0` of one row. That is everything the encodings
//! this model executes name (`src/vpu/insn.rs`, `VecInsn::executable`); a
//! vertical slot faults there rather than reaching this file.
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

/// Byte offset of lane `lane` of a horizontal register at `(row, x0)`.
#[inline]
fn offset(row: u8, x0: u8, lane: u32, lane_bytes: u32) -> usize {
    let col = (x0 as u32 + lane * lane_bytes) as usize % DIM;
    (row as usize % DIM) * DIM + col
}

impl Vrf {
    /// Read one lane, zero-extended to a `u32`. Elements are little-endian, the
    /// same way the memory they are loaded from is.
    pub fn read(&self, row: u8, x0: u8, lane: u32, lane_bytes: u32) -> u32 {
        let off = offset(row, x0, lane, lane_bytes);
        let mut v = 0u32;
        for i in 0..lane_bytes as usize {
            v |= (self.bytes[(off + i) % (DIM * DIM)] as u32) << (8 * i);
        }
        v
    }

    /// Write one lane, truncated to `lane_bytes`.
    pub fn write(&mut self, row: u8, x0: u8, lane: u32, lane_bytes: u32, value: u32) {
        let off = offset(row, x0, lane, lane_bytes);
        for i in 0..lane_bytes as usize {
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

    #[test]
    fn lanes_are_little_endian_and_contiguous() {
        let mut vrf = Vrf::default();
        vrf.write(3, 0, 1, 4, 0x1122_3344);
        assert_eq!(vrf.byte(3, 4), 0x44);
        assert_eq!(vrf.byte(3, 7), 0x11);
        assert_eq!(vrf.read(3, 0, 1, 4), 0x1122_3344);
        // A 16-bit window over the same bytes sees the two halves.
        assert_eq!(vrf.read(3, 0, 2, 2), 0x3344);
        assert_eq!(vrf.read(3, 0, 3, 2), 0x1122);
    }

    #[test]
    fn column_bands_do_not_overlap() {
        let mut vrf = Vrf::default();
        // H(5,16) lane 0 is column 16; H(5,0) lane 15 is column 15.
        vrf.write(5, 16, 0, 1, 0xAB);
        assert_eq!(vrf.read(5, 0, 15, 1), 0);
        assert_eq!(vrf.byte(5, 16), 0xAB);
    }
}
