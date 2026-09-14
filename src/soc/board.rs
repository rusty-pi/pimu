//! The board around the SoC: which Pi 4 family board it is, as the revision
//! code in OTP row 30 names it, and which BCM2711 stepping it carries (#77).
//!
//! The firmware decodes the revision code early and picks the board's PMICs,
//! GPIO expander and pin map by it, so it has to fit the stepping. A B0 never
//! shipped on a rev 1.5 board, and the pinned firmware running as a B0 on one
//! derails inside start4.
//!
//! The code's fields (Raspberry Pi's `revision-codes` documentation): bit 23
//! marks the new-style code, bits 22:20 the memory size, 19:16 the maker,
//! 15:12 the processor (3 = BCM2711), 11:4 the board type (`0x11` = 4B) and
//! 3:0 the PCB revision.

use anyhow::{Context, Result};

use super::Stepping;

/// Pi 4 Model B, 8 GB, rev 1.5, a C0 board: what every board the model has been
/// checked against reports, and the model's default.
pub const PI4B_8GB_REV_1_5: u32 = 0x00D0_3115;

/// Pi 4 Model B, 4 GB, rev 1.2, a B0 board, and the one the public B0 UART
/// logs come from (raspberrypi/rpi-eeprom#251, #466).
pub const PI4B_4GB_REV_1_2: u32 = 0x00C0_3112;

/// Board type `0x11` in bits 11:4 of the revision code.
const TYPE_PI4B: u32 = 0x11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Board {
    pub stepping: Stepping,
    /// The new-style revision code OTP row 30 holds.
    pub revision: u32,
}

impl Default for Board {
    fn default() -> Board {
        Board::for_stepping(Stepping::default())
    }
}

impl Board {
    /// The board modelled for `stepping` when none is named: C0 on the rev 1.5
    /// 4B, B0 on the rev 1.2 one.
    pub const fn for_stepping(stepping: Stepping) -> Board {
        let revision = match stepping {
            Stepping::B0 => PI4B_4GB_REV_1_2,
            Stepping::C0 => PI4B_8GB_REV_1_5,
        };
        Board { stepping, revision }
    }

    /// The PCB revision: 2 for rev 1.2, 5 for rev 1.5.
    pub const fn pcb_revision(self) -> u32 {
        self.revision & 0xF
    }

    /// Why this stepping on this board is not a combination that shipped, if it
    /// is not one. Only the 4B is checked: rev 1.1 and 1.2 predate C0 silicon,
    /// rev 1.4 came with either, and every rev 1.5 measured is a C0.
    pub fn mismatch(self) -> Option<String> {
        if (self.revision >> 4) & 0xFF != TYPE_PI4B {
            return None;
        }
        match (self.stepping, self.pcb_revision()) {
            (Stepping::C0, rev @ (1 | 2)) => Some(format!(
                "a Pi 4B rev 1.{rev} ({:06x}) predates C0 silicon",
                self.revision
            )),
            (Stepping::B0, 5) => Some(format!(
                "a Pi 4B rev 1.5 ({:06x}) is a C0 board",
                self.revision
            )),
            _ => None,
        }
    }

    /// A revision code as `--board-rev` takes it: hex, `0x` optional.
    pub fn parse_revision(s: &str) -> Result<u32> {
        u32::from_str_radix(s.trim_start_matches("0x"), 16)
            .with_context(|| format!("expected a hex revision code like d03115, got '{s}'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_stepping_gets_a_board_it_shipped_on() {
        assert_eq!(Board::default(), Board::for_stepping(Stepping::C0));
        assert_eq!(Board::default().revision, 0x00D0_3115);
        assert_eq!(Board::for_stepping(Stepping::B0).revision, 0x00C0_3112);
        for s in [Stepping::B0, Stepping::C0] {
            assert_eq!(Board::for_stepping(s).mismatch(), None, "{s}");
        }
    }

    #[test]
    fn combinations_that_never_shipped_are_named() {
        let board = |stepping, revision| Board { stepping, revision };
        assert!(board(Stepping::B0, 0x00D0_3115).mismatch().is_some());
        assert!(board(Stepping::C0, 0x00C0_3112).mismatch().is_some());
        // Rev 1.4 came with either.
        assert_eq!(board(Stepping::B0, 0x00D0_3114).mismatch(), None);
        assert_eq!(board(Stepping::C0, 0x00D0_3114).mismatch(), None);
        // Not a 4B (a CM4, type 0x14): not checked.
        assert_eq!(board(Stepping::B0, 0x00B0_3141).mismatch(), None);
    }

    #[test]
    fn revision_codes_parse_as_hex() {
        assert_eq!(Board::parse_revision("c03112").unwrap(), 0x00C0_3112);
        assert_eq!(Board::parse_revision("0xd03115").unwrap(), 0x00D0_3115);
        assert!(Board::parse_revision("rev1.2").is_err());
    }
}
