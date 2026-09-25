//! Instruction-length decoding for the VideoCore IV scalar VPU.
//!
//! VPU instructions are 16, 32, 48 or 80 bits long, and the length is fully
//! determined by the top bits of the first 16-bit parcel, read little-endian.
//!
//! Encoding: `isa/vpu.toml`, "Instruction length".

/// Length of a VPU instruction, in bytes, given its first 16-bit parcel.
#[inline]
pub const fn insn_len_bytes(first_parcel: u16) -> u8 {
    match first_parcel {
        0x0000..=0x7FFF => 2,
        0x8000..=0xDFFF => 4,
        0xE000..=0xF7FF => 6,
        0xF800..=0xFFFF => 10,
    }
}

/// Number of 16-bit parcels in a VPU instruction, given its first parcel.
#[inline]
pub const fn insn_len_parcels(first_parcel: u16) -> usize {
    (insn_len_bytes(first_parcel) / 2) as usize
}

/// Instruction class implied purely by the first parcel. Useful for tracing and
/// for routing to the right decoder table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsnClass {
    Scalar16,
    Scalar32,
    Scalar48,
    Vector48,
    Vector80,
}

#[inline]
pub const fn insn_class(first_parcel: u16) -> InsnClass {
    match first_parcel {
        0x0000..=0x7FFF => InsnClass::Scalar16,
        0x8000..=0xDFFF => InsnClass::Scalar32,
        0xE000..=0xEFFF => InsnClass::Scalar48,
        0xF000..=0xF7FF => InsnClass::Vector48,
        0xF800..=0xFFFF => InsnClass::Vector80,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries() {
        // 16-bit: anything with the top bit clear.
        assert_eq!(insn_len_bytes(0x0000), 2); // bkpt
        assert_eq!(insn_len_bytes(0x0001), 2); // nop
        assert_eq!(insn_len_bytes(0x7FFF), 2);

        // 32-bit: 0b100x .. 0b1101
        assert_eq!(insn_len_bytes(0x8000), 4);
        assert_eq!(insn_len_bytes(0x9000), 4); // b<cond> (32-bit form)
        assert_eq!(insn_len_bytes(0xBFFF), 4);
        assert_eq!(insn_len_bytes(0xC000), 4); // triadic alu
        assert_eq!(insn_len_bytes(0xDFFF), 4);

        // 48-bit scalar: 0b1110
        assert_eq!(insn_len_bytes(0xE000), 6); // j <abs32>
        assert_eq!(insn_len_bytes(0xEFFF), 6);

        // 48-bit vector: 0b11110
        assert_eq!(insn_len_bytes(0xF000), 6);
        assert_eq!(insn_len_bytes(0xF7FF), 6);

        // 80-bit vector: 0b11111
        assert_eq!(insn_len_bytes(0xF800), 10);
        assert_eq!(insn_len_bytes(0xFFFF), 10);
    }

    #[test]
    fn parcels_match_bytes() {
        for p in [
            0x0000u16, 0x4321, 0x8000, 0xC0DE, 0xE000, 0xF123, 0xFC00, 0xFFFF,
        ] {
            assert_eq!(insn_len_parcels(p) * 2, insn_len_bytes(p) as usize);
        }
    }

    #[test]
    fn classes() {
        assert_eq!(insn_class(0x0001), InsnClass::Scalar16);
        assert_eq!(insn_class(0xA000), InsnClass::Scalar32);
        assert_eq!(insn_class(0xE000), InsnClass::Scalar48);
        assert_eq!(insn_class(0xF000), InsnClass::Vector48);
        assert_eq!(insn_class(0xF800), InsnClass::Vector80);
    }
}
