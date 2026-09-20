//! The VideoCore IV Vector Register File.
//!
//! The VRF is a 64x64 array of bytes. A vector register is a 16-element window
//! into it, named by a slot — see [`crate::vpu::insn::VecSlot`]. A *horizontal*
//! window is 16 consecutive elements along one row, starting at byte column
//! `x`; a *vertical* one is the same 16 elements read down a column, one per
//! row, from the 16-aligned band `y` names. Element width comes from the
//! operation, not from the slot.
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

/// Byte offset of lane `lane` of the window at `(y, x)`.
#[inline]
fn offset(y: u8, x: u8, vertical: bool, lane: u32, lane_bytes: u32) -> usize {
    let (row, col) = if vertical {
        ((y as u32 + lane) as usize % DIM, x as usize % DIM)
    } else {
        (
            y as usize % DIM,
            (x as u32 + lane * lane_bytes) as usize % DIM,
        )
    };
    row * DIM + col
}

impl Vrf {
    /// Read one lane, zero-extended to a `u32`. Elements are little-endian, the
    /// same way the memory they are loaded from is.
    pub fn read(&self, y: u8, x: u8, vertical: bool, lane: u32, lane_bytes: u32) -> u32 {
        let off = offset(y, x, vertical, lane, lane_bytes);
        let mut v = 0u32;
        for i in 0..lane_bytes as usize {
            v |= (self.bytes[(off + i) % (DIM * DIM)] as u32) << (8 * i);
        }
        v
    }

    /// Write one lane, truncated to `lane_bytes`.
    pub fn write(&mut self, y: u8, x: u8, vertical: bool, lane: u32, lane_bytes: u32, value: u32) {
        let off = offset(y, x, vertical, lane, lane_bytes);
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
        vrf.write(3, 0, false, 1, 4, 0x1122_3344);
        assert_eq!(vrf.byte(3, 4), 0x44);
        assert_eq!(vrf.byte(3, 7), 0x11);
        assert_eq!(vrf.read(3, 0, false, 1, 4), 0x1122_3344);
        // A 16-bit window over the same bytes sees the two halves.
        assert_eq!(vrf.read(3, 0, false, 2, 2), 0x3344);
        assert_eq!(vrf.read(3, 0, false, 3, 2), 0x1122);
    }

    #[test]
    fn a_vertical_window_walks_down_a_column() {
        let mut vrf = Vrf::default();
        // V(16,8) lane 3 is row 19, at column 8, two bytes wide.
        vrf.write(16, 8, true, 3, 2, 0xBEEF);
        assert_eq!(vrf.byte(19, 8), 0xEF);
        assert_eq!(vrf.byte(19, 9), 0xBE);
        assert_eq!(vrf.read(16, 8, true, 3, 2), 0xBEEF);
        // Neighbouring lanes are neighbouring rows, not neighbouring columns.
        assert_eq!(vrf.read(16, 8, true, 2, 2), 0);
        assert_eq!(vrf.read(19, 8, false, 0, 2), 0xBEEF);
    }

    #[test]
    fn column_bands_do_not_overlap() {
        let mut vrf = Vrf::default();
        // H(5,16) lane 0 is column 16; H(5,0) lane 15 is column 15.
        vrf.write(5, 16, false, 0, 1, 0xAB);
        assert_eq!(vrf.read(5, 0, false, 15, 1), 0);
        assert_eq!(vrf.byte(5, 16), 0xAB);
    }
}
