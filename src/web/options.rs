//! What a page asks of the machine, as the JSON it hands over: the browser's
//! counterpart of `pimu boot`'s command line.

use anyhow::{bail, Result};
use serde::Deserialize;

use crate::soc::{Board, Stepping};

/// The biggest Raspberry Pi 4. A board takes only as much of the tab's memory
/// as its guest has written to, so a size is no promise that it fits.
const MAX_MEMORY: u64 = 8 << 30;

#[derive(Deserialize, Default, Debug)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    /// `b0` or `c0`; `c0` when left out.
    pub stepping: Option<String>,
    /// The board's memory, 1024, 2048, 4096 or 8192; 1024 when left out.
    pub memory_mb: Option<u32>,
    /// The OTP revision code in hex, in place of the one the stepping and
    /// `memory_mb` make.
    pub board_rev: Option<String>,
    pub boot_order: Option<String>,
    /// `KEY=VALUE` lines for the EEPROM's `bootconf.txt`.
    pub bootconf: Vec<String>,
    pub skip_signed_boot: bool,
    pub tryboot: bool,
    /// Lines added to the card's `config.txt`.
    pub config_txt: Vec<String>,
    /// The card's `cmdline.txt`.
    pub cmdline: Option<String>,
    /// Fuses programmed before the boot: `[[row, value], ...]`.
    pub otp_rows: Vec<(u32, u32)>,
    /// `--log` channels, comma separated, on top of `io`, which is always on:
    /// the page draws the board's traffic from it.
    pub log: String,
    pub display: bool,
    pub otg_dock: bool,
    pub usb_mb: Option<u64>,
    /// The Ethernet cable goes into the built-in network peer.
    pub network: bool,
    /// Nothing in the microSD slot: a board that boots over the network, or
    /// from USB, has no card.
    pub no_sd_card: bool,
    pub jitter: Option<u64>,
    pub faults: Option<u64>,
    pub check_coherency: bool,
    pub check_alignment: bool,
    pub skip_unimpl: bool,
    /// Run the ARM cores on worker threads, if the page started any.
    pub parallel: bool,
}

impl Options {
    pub fn parse(json: &str) -> Result<Options> {
        if json.trim().is_empty() {
            return Ok(Options::default());
        }
        Ok(serde_json::from_str(json)?)
    }

    pub fn board(&self) -> Result<Board> {
        let stepping = match self.stepping.as_deref() {
            None | Some("c0") => Stepping::C0,
            Some("b0") => Stepping::B0,
            Some(other) => bail!("stepping {other:?}: expected b0 or c0"),
        };
        let mut board = Board::for_stepping(stepping);
        match (&self.board_rev, self.memory_mb) {
            (Some(rev), _) => board.revision = Board::parse_revision(rev)?,
            (None, mb) => {
                let size = match mb.unwrap_or(1024) {
                    1024 => 2,
                    2048 => 3,
                    4096 => 4,
                    8192 => 5,
                    other => bail!("{other} MB of memory: a Pi 4 has 1024, 2048, 4096 or 8192"),
                };
                board.revision = (board.revision & !(7 << 20)) | (size << 20);
            }
        }
        if board.memory_bytes() > MAX_MEMORY {
            bail!(
                "board revision {:06x} has {} GiB of memory, and a Pi 4 has up to {}",
                board.revision,
                board.memory_bytes() >> 30,
                MAX_MEMORY >> 30
            );
        }
        Ok(board)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(json: &str) -> Result<Board> {
        Options::parse(json)?.board()
    }

    #[test]
    fn the_default_board_is_a_1_gib_c0() {
        let board = board("").unwrap();
        assert_eq!(board.stepping, Stepping::C0);
        assert_eq!(board.memory_bytes(), 1 << 30);
        assert_eq!(board.mismatch(), None);
    }

    #[test]
    fn memory_and_stepping_pick_a_board_that_shipped() {
        let b0 = board(r#"{"stepping":"b0","memoryMb":2048}"#).unwrap();
        assert_eq!(b0.stepping, Stepping::B0);
        assert_eq!(b0.memory_bytes(), 2 << 30);
        assert_eq!(b0.mismatch(), None);
    }

    #[test]
    fn every_pi_4_size_is_taken() {
        assert_eq!(
            board(r#"{"memoryMb":8192}"#).unwrap().memory_bytes(),
            8 << 30
        );
        let d03115 = board(r#"{"boardRev":"d03115"}"#).unwrap();
        assert_eq!(d03115.memory_bytes(), 8 << 30);
        assert_eq!(d03115.mismatch(), None);
    }

    #[test]
    fn a_board_that_never_was_is_refused() {
        assert!(board(r#"{"boardRev":"e03115"}"#).is_err());
        assert!(board(r#"{"memoryMb":3072}"#).is_err());
        assert!(board(r#"{"stepping":"a1"}"#).is_err());
    }

    #[test]
    fn the_cores_take_turns_unless_the_page_asks_for_threads() {
        assert!(!Options::parse("").unwrap().parallel);
        assert!(Options::parse(r#"{"parallel":true}"#).unwrap().parallel);
    }

    #[test]
    fn an_unknown_option_is_refused() {
        assert!(Options::parse(r#"{"bootorder":"0x1"}"#).is_err());
    }
}
