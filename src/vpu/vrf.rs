//! The VC4 Vector Register File.
//!
//! 64 rows of 64 bytes, and a row is **sixteen lanes of four bytes** — not
//! sixty-four bytes in a line. Element `e` of a register with `w`-byte elements
//! lives at byte `(e & 15) * 4 + (e >> 4) * w` of its row, so consecutive
//! elements are four bytes apart, interleaved, rather than side by side. A
//! *horizontal* register is sixteen elements of one row; a *vertical* one is
//! the same element of sixteen consecutive rows.
//!
//! Measured, not inferred, on a Raspberry Pi 4B d03115
//! (`vpu-probe/probes/layout.s`, `vpu-probe/probes/vert.s`).
//!
//! The unit also keeps per-lane zero, negative and carry flags, written by an
//! ALU op with `SETF` and read by the lane predicates; a *transfer* with `SETF`
//! writes none of them, measured.

/// Bytes per VRF row, and rows in the file.
pub const DIM: usize = 64;

/// Lanes in a vector register. Fixed by the architecture.
pub const LANES: u32 = 16;

/// Bytes in the unit's lookup table.
pub const LUT: usize = 1024;

/// Bytes of that table each lane addresses. Measured: sixteen lanes writing
/// the same index do not overwrite one another, and each reads its own value
/// back — so the 1 KiB is sixteen private 64-byte regions, one per lane.
pub const LUT_LANE: usize = LUT / LANES as usize;

pub struct Vrf {
    /// Boxed, not inline: [`Vpu`](crate::vpu::Vpu) is stepped a billion times a
    /// boot and the hot fields around this one have to stay in a few cache
    /// lines. Inline, the 4 KiB of file costs 75% more wall clock on a whole
    /// firmware boot, measured, for a register file a handful of instructions
    /// touch.
    bytes: Box<[u8; DIM * DIM]>,
    /// Per-lane zero flag, one bit per lane (bit 0 = lane 0).
    pub lane_z: u16,
    /// Per-lane negative flag: the result's sign bit at the operation's width.
    pub lane_n: u16,
    /// Per-lane carry flag. Only the ops that produce one write it — an
    /// addition's carry out, a subtraction's borrow, a saturating op's clamp,
    /// the operand `min`/`max` chose, the last bit out of a shift — and the
    /// rest leave it as they found it.
    pub lane_c: u16,
    /// The vector unit's own 1 KiB lookup table, the one `readlut` and
    /// `writelut` address. Measured with `vpu-probe/probes/lut.s`: a `v8memwrite`
    /// followed by a `v8memread` over the same indices hands back exactly what
    /// was written, and a `v16` pair round-trips halfwords at twice the index.
    /// Each lane addresses its own [`LUT_LANE`] bytes of it — lanes that share
    /// an index keep their own value, so the table is banked, not shared.
    pub lut: Box<[u8; LUT]>,
    /// One accumulator per lane. Wider than an element — four accumulates of
    /// `0xffff` read back as `0x3fffc` — so it is kept as a `u32`; the `SIGN`
    /// bit of the modifier decides how a result is extended into it.
    pub acc: [u32; LANES as usize],
}

impl Default for Vrf {
    fn default() -> Vrf {
        Vrf {
            bytes: Box::new([0; DIM * DIM]),
            lut: Box::new([0; LUT]),
            lane_z: 0,
            lane_n: 0,
            lane_c: 0,
            acc: [0; LANES as usize],
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
    /// Which lanes a predicate lets through.
    pub fn lanes(&self, pred: crate::vpu::insn::VecPred) -> u16 {
        use crate::vpu::insn::VecPred::*;
        match pred {
            All => u16::MAX,
            NoLanes => 0,
            IfZero => self.lane_z,
            IfNonZero => !self.lane_z,
            IfNeg => self.lane_n,
            IfNotNeg => !self.lane_n,
            IfCarry => self.lane_c,
            IfNotCarry => !self.lane_c,
        }
    }

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

    /// Consecutive elements are a lane apart, not an element apart: measured
    /// with `v8ld H(0,0),(r1)` over a page of ascending bytes, which lands them
    /// at columns 0, 4, 8 … of the row.
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
