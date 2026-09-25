//! BCM2711 silicon revisions ("steppings") the model can be.
//!
//! The Pi 4 family shipped two production steppings: **B0**, on the first
//! Pi 4 Model B boards (rev 1.1, 1.2 and early 1.4), and **C0**, on the Pi 400,
//! every CM4 and the later 4B (late rev 1.4 and all of rev 1.5). The model is a
//! C0 unless told otherwise; every board it has been checked against (five
//! Raspberry Pi 4B d03115 boards, rev 1.5) is one.
//!
//! What it changes, as far as the firmware can tell: the VPU's `version`, the
//! mask ROM's layout, whether DMA channel 15 is a 40-bit channel, and which
//! boards carried it. Linux learns it from `/emmc2bus` `dma-ranges` in the
//! handed-over tree: the first 1 GB on B0, whose EMMC2 bus reached no further,
//! and almost all 4 GB on C0.

use anyhow::{bail, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stepping {
    B0,
    #[default]
    C0,
}

impl Stepping {
    /// What `version rd` returns on core 0; core 1 sets bit 16 too. C0's
    /// `0x0400_0162` is measured, B0's `0x0400_0161` inferred: bootcode keys its
    /// mask-ROM pointers by it, and only `0x161` decodes to code addresses.
    pub const fn vpu_version(self) -> u32 {
        match self {
            Stepping::B0 => 0x0400_0161,
            Stepping::C0 => 0x0400_0162,
        }
    }

    /// Whether DMA channel 15 takes DMA4-layout control blocks: start4 drives
    /// it that way only on C0, and builds legacy blocks on B0.
    pub const fn dma_channel_15_is_40_bit(self) -> bool {
        matches!(self, Stepping::C0)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Stepping::B0 => "B0",
            Stepping::C0 => "C0",
        }
    }

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
