//! Parsing `pieeprom.bin` — the Raspberry Pi 4 bootloader SPI-flash image.
//!
//! Layout (from `raspberrypi/rpi-eeprom`'s `rpi-eeprom-config`): a chain of
//! sections, each `>u32 magic, >u32 length` then `length` body bytes, padded to
//! 8. `magic & 0xFFFF_F00F == 0x55AA_F00F`. Section kinds seen:
//!
//! | magic         | meaning                                    |
//! |---------------|--------------------------------------------|
//! | `0x55AA_F00F` | bootcode (the VPU second-stage bootloader)  |
//! | `0x55AA_F11F` | modifiable file (filename in first 248 B)   |
//! | `0x55AA_F33F` | packed resource (e.g. SDRAM init firmware)  |
//! | `0x55AA_F44F` | packed resource                            |
//! | `0x55AA_FEEF` | padding                                    |
//!
//! The bootcode body begins with a 0x200-byte header/signature area; the VPU
//! entry point is body-offset `0x200`. The BCM2711 boot ROM stages the bootcode
//! in L2-as-SRAM at `0x8000_0000` (per `librerpi/lk-overlay`'s `bootcode.ld`,
//! `ORIGIN = 0x8000_0000`).

use anyhow::{bail, Context, Result};

const MAGIC_MASK: u32 = 0xFFFF_F00F;
const MAGIC_BASE: u32 = 0x55AA_F00F;
pub const MAGIC_BOOTCODE: u32 = 0x55AA_F00F;
pub const MAGIC_FILE: u32 = 0x55AA_F11F;
pub const MAGIC_PAD: u32 = 0x55AA_FEEF;

/// Where the boot ROM stages the bootcode, and the offset of its entry point.
pub const BOOTCODE_LOAD_ADDR: u32 = 0x8000_0000;
pub const BOOTCODE_ENTRY_OFFSET: u32 = 0x200;

#[derive(Debug, Clone)]
pub struct Section {
    pub magic: u32,
    /// Offset of the section header within the image.
    pub header_offset: usize,
    /// Body bytes (excludes the 8-byte header).
    pub body: Vec<u8>,
    /// Filename for `MAGIC_FILE` sections.
    pub filename: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EepromImage {
    pub sections: Vec<Section>,
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl EepromImage {
    pub fn parse(bytes: &[u8]) -> Result<EepromImage> {
        let mut sections = Vec::new();
        let mut off = 0usize;

        while off + 8 <= bytes.len() {
            let magic = be32(bytes, off);
            if magic == 0 || magic == 0xFFFF_FFFF {
                break; // EOF marker
            }
            if magic & MAGIC_MASK != MAGIC_BASE {
                bail!("corrupt EEPROM: bad section magic {magic:#010x} at offset {off:#x}");
            }
            let length = be32(bytes, off + 4) as usize;
            let body_start = off + 8;
            let body_end = body_start
                .checked_add(length)
                .filter(|&e| e <= bytes.len())
                .with_context(|| format!("section at {off:#x} length {length:#x} runs past EOF"))?;

            let filename = if magic == MAGIC_FILE {
                let raw = &bytes[body_start..body_start.saturating_add(248).min(body_end)];
                let name: String = raw
                    .iter()
                    .take_while(|&&c| c != 0)
                    .map(|&c| c as char)
                    .collect();
                Some(name)
            } else {
                None
            };

            sections.push(Section {
                magic,
                header_offset: off,
                body: bytes[body_start..body_end].to_vec(),
                filename,
            });

            off = (body_end + 7) & !7;
        }

        if sections.is_empty() {
            bail!("no sections found in EEPROM image");
        }
        Ok(EepromImage { sections })
    }

    pub fn bootcode(&self) -> Option<&Section> {
        self.sections.iter().find(|s| s.magic == MAGIC_BOOTCODE)
    }

    pub fn file(&self, name: &str) -> Option<&Section> {
        self.sections
            .iter()
            .find(|s| s.magic == MAGIC_FILE && s.filename.as_deref() == Some(name))
    }

    /// The `bootconf.txt` config, if present.
    pub fn config_text(&self) -> Option<String> {
        self.file("bootconf.txt")
            .map(|s| String::from_utf8_lossy(&s.body).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_garbage() {
        assert!(EepromImage::parse(&[0xAA; 64]).is_err());
    }

    #[test]
    fn parses_a_minimal_image() {
        // one bootcode section, body = 4 bytes, then EOF.
        let mut img = Vec::new();
        img.extend_from_slice(&MAGIC_BOOTCODE.to_be_bytes());
        img.extend_from_slice(&4u32.to_be_bytes());
        img.extend_from_slice(&[1, 2, 3, 4]);
        img.extend_from_slice(&[0, 0, 0, 0]); // pad to 8 + EOF
        let e = EepromImage::parse(&img).unwrap();
        assert_eq!(e.bootcode().unwrap().body, vec![1, 2, 3, 4]);
    }
}
