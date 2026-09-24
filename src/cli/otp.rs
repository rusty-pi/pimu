//! `boot --otp json:<file>` / `--otp binary:<file>`: the fuse array in a file,
//! so what the firmware programs outlives the run (#93).
//!
//! The file holds the whole array. `boot` reads it before the first boot when
//! it exists, and writes it back after the run when the firmware programmed a
//! row. A missing file starts from the model's own fuses
//! (`src/periph/configotp.rs`) and is created, which is also the way to get
//! that array out, edit it, and boot a board fused differently.
//!
//! * `json:` — an object of row number to value, fused rows only, one a line
//!   (`"36": "0x11111111"`). A value is a hex string or a number.
//! * `binary:` — row n at byte 4n, little-endian, 0 for a blank row. Rows
//!   0-67, the ones start4 reads (`0x3ED3FA60` refuses any above); a longer
//!   file is fine.
//!
//! A file made from a real board's fuses carries that board's secrets (the OTP
//! rule in `CLAUDE.md`): keep it out of the repository.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::config::{parse_json, Value};
use crate::parse_u32;

/// Rows the binary format always carries.
const BINARY_ROWS: u32 = 68;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Binary,
}

/// `--otp <format>:<file>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtpFile {
    pub format: Format,
    pub path: PathBuf,
}

impl std::str::FromStr for OtpFile {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<OtpFile> {
        let (format, path) = match s.split_once(':') {
            Some(("json", path)) => (Format::Json, path),
            Some(("binary", path)) => (Format::Binary, path),
            _ => bail!("--otp {s}: expected json:<file> or binary:<file>"),
        };
        if path.is_empty() {
            bail!("--otp {s}: no file after the format");
        }
        Ok(OtpFile {
            format,
            path: PathBuf::from(path),
        })
    }
}

impl OtpFile {
    /// The fuses in the file, or `None` when there is no file yet.
    pub fn load(&self) -> Result<Option<BTreeMap<u32, u32>>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", self.path.display()));
            }
        };
        let fuses = match self.format {
            Format::Json => std::str::from_utf8(&bytes)
                .context("not UTF-8")
                .and_then(from_json),
            Format::Binary => from_binary(&bytes),
        };
        fuses
            .map(Some)
            .with_context(|| format!("--otp {}", self.path.display()))
    }

    /// Write `fuses` out: to a file next to it first, then renamed over it, so
    /// a run that dies half-way leaves the old one.
    pub fn save(&self, fuses: &BTreeMap<u32, u32>) -> Result<()> {
        let bytes = match self.format {
            Format::Json => to_json(fuses).into_bytes(),
            Format::Binary => to_binary(fuses),
        };
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("writing {}", self.path.display()))
    }
}

fn from_json(text: &str) -> Result<BTreeMap<u32, u32>> {
    let Value::Obj(rows) = parse_json(text)? else {
        bail!("expected a JSON object of row -> value");
    };
    let mut fuses = BTreeMap::new();
    for (key, value) in rows {
        let row = parse_u32(&key).with_context(|| format!("row '{key}'"))?;
        let word = match &value {
            Value::Str(s) | Value::Num(s) => {
                parse_u32(s).with_context(|| format!("row {row}: '{s}'"))?
            }
            other => bail!("row {row}: expected a number or a hex string, got {other:?}"),
        };
        if fuses.insert(row, word).is_some() {
            bail!("row {row} is there twice");
        }
    }
    // A row of zeroes is a blank one, whichever way the file put it.
    fuses.retain(|_, word| *word != 0);
    Ok(fuses)
}

fn to_json(fuses: &BTreeMap<u32, u32>) -> String {
    let rows: Vec<String> = fuses
        .iter()
        .filter(|(_, &word)| word != 0)
        .map(|(row, word)| format!("  \"{row}\": \"{word:#010x}\""))
        .collect();
    if rows.is_empty() {
        return "{}\n".to_string();
    }
    format!("{{\n{}\n}}\n", rows.join(",\n"))
}

fn from_binary(bytes: &[u8]) -> Result<BTreeMap<u32, u32>> {
    let (words, rest) = bytes.as_chunks::<4>();
    if !rest.is_empty() {
        bail!("{} bytes: expected whole 32-bit rows", bytes.len());
    }
    Ok(words
        .iter()
        .enumerate()
        .map(|(row, word)| (row as u32, u32::from_le_bytes(*word)))
        .filter(|&(_, word)| word != 0)
        .collect())
}

fn to_binary(fuses: &BTreeMap<u32, u32>) -> Vec<u8> {
    let rows = fuses
        .keys()
        .next_back()
        .map_or(0, |&row| row + 1)
        .max(BINARY_ROWS);
    let mut out = vec![0u8; rows as usize * 4];
    for (&row, &word) in fuses {
        out[row as usize * 4..][..4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_fuses() -> BTreeMap<u32, u32> {
        pimu::periph::ConfigOtp::new().fuses().clone()
    }

    #[test]
    fn the_format_is_a_prefix() {
        let otp: OtpFile = "json:a/otp.json".parse().unwrap();
        assert_eq!(otp.format, Format::Json);
        assert_eq!(otp.path, PathBuf::from("a/otp.json"));
        let otp: OtpFile = "binary:otp.bin".parse().unwrap();
        assert_eq!(otp.format, Format::Binary);
        for bad in ["otp.json", "hex:otp.txt", "json:"] {
            assert!(bad.parse::<OtpFile>().is_err(), "{bad}");
        }
    }

    #[test]
    fn both_formats_round_trip_the_model_fuses() {
        let fuses = model_fuses();
        assert_eq!(from_json(&to_json(&fuses)).unwrap(), fuses);
        let binary = to_binary(&fuses);
        assert_eq!(binary.len(), 68 * 4);
        assert_eq!(from_binary(&binary).unwrap(), fuses);
    }

    #[test]
    fn json_is_one_row_a_line_and_takes_numbers_too() {
        let fuses = BTreeMap::from([(16, 1), (36, 0x1111_1111)]);
        assert_eq!(
            to_json(&fuses),
            "{\n  \"16\": \"0x00000001\",\n  \"36\": \"0x11111111\"\n}\n"
        );
        assert_eq!(
            from_json(r#"{"16": 1, "0x24": "0x11111111", "37": "0"}"#).unwrap(),
            fuses
        );
        assert_eq!(to_json(&BTreeMap::new()), "{}\n");
    }

    #[test]
    fn binary_puts_row_n_at_byte_4n() {
        let binary = to_binary(&BTreeMap::from([(1, 0x0403_0201), (70, 5)]));
        assert_eq!(binary.len(), 71 * 4);
        assert_eq!(&binary[4..8], &[1, 2, 3, 4]);
        assert_eq!(binary[70 * 4], 5);
    }

    #[test]
    fn malformed_files_are_errors() {
        assert!(from_binary(&[0; 5]).is_err());
        assert!(from_json("[1, 2]").is_err());
        assert!(from_json(r#"{"36": true}"#).is_err());
        assert!(from_json(r#"{"x": "1"}"#).is_err());
        assert!(from_json(r#"{"36": "1", "0x24": "2"}"#).is_err());
    }

    #[test]
    fn a_missing_file_loads_as_none_and_save_creates_it() {
        let path = std::env::temp_dir().join(format!("pimu-otp-test-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let otp = OtpFile {
            format: Format::Json,
            path: path.clone(),
        };
        assert_eq!(otp.load().unwrap(), None);
        let fuses = model_fuses();
        otp.save(&fuses).unwrap();
        assert_eq!(otp.load().unwrap(), Some(fuses));
        std::fs::remove_file(&path).unwrap();
    }
}
