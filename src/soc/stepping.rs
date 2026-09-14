//! BCM2711 silicon revisions ("steppings") the model can be (#77).
//!
//! The Pi 4 family shipped two production steppings: **B0**, on the first
//! Pi 4 Model B boards (rev 1.1, 1.2 and early 1.4), and **C0**, on the Pi 400,
//! every CM4 and the later 4B (late rev 1.4 and all of rev 1.5). The model is a
//! C0 unless told otherwise; every board it has been checked against (rpi-1 to
//! rpi-4 and rpi-dev, all rev 1.5) is one.
//!
//! What the stepping changes, as far as the firmware can tell:
//!
//! * the VPU's `version` value ([`Stepping::vpu_version`]);
//! * the mask ROM's layout, which bootcode up to 2020-06-15 calls into
//!   ([`crate::firmware::bootrom`]);
//! * whether DMA channel 15 is a 40-bit channel
//!   ([`Stepping::dma_channel_15_is_40_bit`]);
//! * which boards carried it ([`crate::soc::Board`]).
//!
//! Linux learns it from the device tree the firmware hands over: `/emmc2bus`
//! `dma-ranges` spans the first 1 GB on B0, whose EMMC2 bus could reach no
//! further, and almost all 4 GB on C0.

use anyhow::{bail, Result};

/// A BCM2711 stepping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stepping {
    B0,
    #[default]
    C0,
}

impl Stepping {
    /// What `version rd` returns on this stepping, on core 0 (core 1 has bit
    /// 16 set as well).
    ///
    /// C0's is `0x0400_0162`. B0's being `0x0400_0161` is inferred rather than
    /// read off a B0 part. Bootcode that calls into the mask ROM keys the
    /// pointers by `version` (`version r2; eor r1, r2; bl r1`), and from
    /// 2019-10-16 on only `0x161` decodes to code addresses; `0x160` gives odd
    /// ones. 2019-07-15, from before the Pi 4 launched, decodes `0x160` too,
    /// which fits that being the engineering stepping, A0: the low bits count
    /// the stepping letter.
    pub const fn vpu_version(self) -> u32 {
        match self {
            Stepping::B0 => 0x0400_0161,
            Stepping::C0 => 0x0400_0162,
        }
    }

    /// Whether DMA channel 15, the one at `0x7EE0_5000`, is a 40-bit ("dma40")
    /// channel that takes DMA4-layout control blocks. start4 only drives it as
    /// one when its chip-feature switch says C0 (`version - 0x0400_0160`
    /// selects which feature words it sets); on B0 it builds legacy control
    /// blocks for the same copies, so there the channel must be a legacy one.
    pub const fn dma_channel_15_is_40_bit(self) -> bool {
        matches!(self, Stepping::C0)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Stepping::B0 => "B0",
            Stepping::C0 => "C0",
        }
    }

    /// `b0` or `c0`, in either case.
    pub fn parse(s: &str) -> Result<Stepping> {
        match s.to_ascii_lowercase().as_str() {
            "b0" => Ok(Stepping::B0),
            "c0" => Ok(Stepping::C0),
            _ => bail!("unknown BCM2711 stepping '{s}' (expected b0 or c0)"),
        }
    }
}

impl std::fmt::Display for Stepping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_either_case_and_rejects_the_rest() {
        assert_eq!(Stepping::parse("b0").unwrap(), Stepping::B0);
        assert_eq!(Stepping::parse("C0").unwrap(), Stepping::C0);
        assert!(Stepping::parse("a0").is_err());
        assert!(Stepping::parse("").is_err());
    }

    #[test]
    fn the_default_is_c0_and_the_low_bits_count_the_letter() {
        assert_eq!(Stepping::default(), Stepping::C0);
        assert_eq!(Stepping::B0.vpu_version() & 3, 1);
        assert_eq!(Stepping::C0.vpu_version() & 3, 2);
        assert_eq!(
            Stepping::B0.vpu_version() & !3,
            Stepping::C0.vpu_version() & !3
        );
    }
}
