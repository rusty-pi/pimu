//! Parsing `pieeprom.bin` — the Raspberry Pi 4 bootloader SPI-flash image.
//!
//! Layout (from `raspberrypi/rpi-eeprom`'s `rpi-eeprom-config` and what
//! `start4.elf`'s `bootloader_eeprom_find_files` at `~0x3EC65490` walks): a
//! chain of sections, each `>u32 magic, >u32 length` then `length` body bytes,
//! the next header 8-byte aligned. `magic & 0xFFFF_F00F == 0x55AA_F00F`.
//! Section kinds seen in the pinned images:
//!
//! | magic         | meaning                                             |
//! |---------------|-----------------------------------------------------|
//! | `0x55AA_F00F` | bootcode (the VPU second-stage bootloader)           |
//! | `0x55AA_F11F` | modifiable file — filename in the first 12 B of body |
//! | `0x55AA_F33F` | packed resource (e.g. SDRAM init firmware)           |
//! | `0x55AA_F44F` | packed resource (per-part memsys / PHY tables)       |
//! | `0x55AA_FEEF` | padding to the next `0x100` boundary (body all `FF`) |
//!
//! The real bootcode / packed-resource layout is not otherwise interpreted —
//! the firmware reads those bytes itself over SPI0 (see `periph::spi0`). What
//! the model uses this parser for is (a) staging the bootcode for the
//! direct-execution / `disasm --eeprom` path and (b) model-side introspection
//! of the modifiable files, in particular `bootconf.txt` (boot order, etc.)
//! that later boot stages consult.
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
pub const MAGIC_PACKED_A: u32 = 0x55AA_F33F;
pub const MAGIC_PACKED_B: u32 = 0x55AA_F44F;
pub const MAGIC_PAD: u32 = 0x55AA_FEEF;

/// Where the boot ROM stages the bootcode, and the offset of its entry point.
pub const BOOTCODE_LOAD_ADDR: u32 = 0x8000_0000;
pub const BOOTCODE_ENTRY_OFFSET: u32 = 0x200;

/// A `MAGIC_FILE` filename lives in the first bytes of the section body. The
/// bootloader stores it in a fixed-size field; 12 covers every name in the
/// pinned images (`bootconf.txt`, `pubkey.bin`, `cacert.der`, …), but read a
/// little more and stop at the NUL so a longer name still round-trips.
const FILENAME_FIELD: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    Bootcode,
    /// Modifiable file (`bootconf.txt` etc.).
    File,
    /// Packed resource blob — SDRAM init / memsys tables.
    PackedResource,
    /// `0xFF` padding to the next `0x100` boundary.
    Pad,
    /// Magic in the `0x55AA_F00F` family but not one we name.
    Unknown,
}

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

impl Section {
    pub fn kind(&self) -> SectionKind {
        match self.magic {
            MAGIC_BOOTCODE => SectionKind::Bootcode,
            MAGIC_FILE => SectionKind::File,
            MAGIC_PACKED_A | MAGIC_PACKED_B => SectionKind::PackedResource,
            MAGIC_PAD => SectionKind::Pad,
            _ => SectionKind::Unknown,
        }
    }
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
                let raw =
                    &bytes[body_start..body_start.saturating_add(FILENAME_FIELD).min(body_end)];
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

    /// Every modifiable file, in image order.
    pub fn files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.sections.iter().filter_map(|s| {
            if s.magic != MAGIC_FILE {
                return None;
            }
            Some((
                s.filename.as_deref().unwrap_or(""),
                s.body.get(FILENAME_FIELD..).unwrap_or(&[]),
            ))
        })
    }

    pub fn file(&self, name: &str) -> Option<&Section> {
        self.sections
            .iter()
            .find(|s| s.magic == MAGIC_FILE && s.filename.as_deref() == Some(name))
    }

    /// Raw `bootconf.txt` bytes (the modifiable file body, past the name field).
    pub fn config_text(&self) -> Option<String> {
        self.files()
            .find(|(n, _)| *n == "bootconf.txt")
            .map(|(_, body)| {
                String::from_utf8_lossy(body)
                    .trim_end_matches(['\0', '\u{ff}'])
                    .to_string()
            })
    }

    /// Parsed `bootconf.txt`.
    pub fn bootconf(&self) -> Option<BootConf> {
        self.config_text().map(|t| BootConf::parse(&t))
    }

    /// A one-line-per-section summary for `boot --eeprom` startup output.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for s in &self.sections {
            let end = s.header_offset + 8 + s.body.len();
            let tag = match s.kind() {
                SectionKind::Bootcode => "bootcode".to_string(),
                SectionKind::File => format!("file {}", s.filename.as_deref().unwrap_or("?")),
                SectionKind::PackedResource => format!("packed {:#06x}", s.magic & 0xFFFF),
                SectionKind::Pad => "pad".to_string(),
                SectionKind::Unknown => format!("? {:#010x}", s.magic),
            };
            out.push_str(&format!(
                "  {:#08x}..{:#08x}  {}\n",
                s.header_offset, end, tag
            ));
        }
        out
    }
}

/// A `BOOT_ORDER` device code (one hex nibble). Names per the Raspberry Pi
/// bootloader documentation.
/// Replace the body of the modifiable file `name` in `flash` in place, the
/// way `rpi-eeprom-config`'s `ImageSection.update` does: the length field
/// becomes the data plus the 16-byte name field, the data follows the name,
/// and the rest of the slot up to the next non-padding section becomes `0xFF`
/// with a `MAGIC_PAD` section header, so the section walk still finds
/// everything after it. The file keeps its place; nothing moves.
pub fn replace_file(flash: &mut [u8], name: &str, data: &[u8]) -> Result<()> {
    let img = EepromImage::parse(flash)?;
    let i = img
        .sections
        .iter()
        .position(|s| s.magic == MAGIC_FILE && s.filename.as_deref() == Some(name))
        .with_context(|| format!("no {name} in the EEPROM image"))?;
    let hdr = img.sections[i].header_offset;
    let next = img.sections[i + 1..]
        .iter()
        .find(|s| s.magic != MAGIC_PAD)
        .map(|s| s.header_offset)
        .with_context(|| {
            format!("{name} is the last section; only slots with a successor are replaced")
        })?;
    let at = hdr + 8 + FILENAME_FIELD;
    if at + data.len() > next {
        bail!(
            "{name}: {} bytes do not fit the {}-byte slot",
            data.len(),
            next - at
        );
    }
    flash[hdr + 4..hdr + 8].copy_from_slice(&((data.len() + FILENAME_FIELD) as u32).to_be_bytes());
    flash[at..at + data.len()].copy_from_slice(data);
    let mut p = at + data.len();
    while !p.is_multiple_of(8) {
        flash[p] = 0xFF;
        p += 1;
    }
    if next - p >= 8 {
        flash[p..p + 4].copy_from_slice(&MAGIC_PAD.to_be_bytes());
        flash[p + 4..p + 8].copy_from_slice(&((next - p - 8) as u32).to_be_bytes());
        p += 8;
    }
    flash[p..next].fill(0xFF);
    Ok(())
}

pub fn boot_device_name(code: u8) -> &'static str {
    match code {
        0x0 => "NONE",
        0x1 => "SD CARD",
        0x2 => "NETWORK",
        0x3 => "RPIBOOT",
        0x4 => "USB-MSD",
        0x5 => "USB 2.0 BCM",
        0x6 => "NVME",
        0x7 => "HTTP",
        0xe => "STOP",
        0xf => "RESTART",
        _ => "?",
    }
}

/// Parsed `bootconf.txt`: an ordered list of `(group, key, value)` and the
/// helpers later boot stages need.
#[derive(Debug, Clone, Default)]
pub struct BootConf {
    /// `(group, KEY, VALUE)` in file order. `group` is `""` for the implicit
    /// leading section and the `[all]` section.
    pub entries: Vec<(String, String, String)>,
}

impl BootConf {
    pub fn parse(text: &str) -> BootConf {
        let mut entries = Vec::new();
        let mut group = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                group = if inner.eq_ignore_ascii_case("all") {
                    String::new()
                } else {
                    inner.to_string()
                };
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                entries.push((group.clone(), k.trim().to_string(), v.trim().to_string()));
            }
        }
        BootConf { entries }
    }

    /// Resolve `key` against the `[all]` section plus any conditional group
    /// whose name is in `active` (e.g. `["pi4", "0x1aa2bb31"]`). Later entries
    /// win, matching `rpi-eeprom-config` precedence.
    pub fn get_for(&self, key: &str, active: &[&str]) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|(g, k, _)| {
                k.eq_ignore_ascii_case(key)
                    && (g.is_empty() || active.iter().any(|a| a.eq_ignore_ascii_case(g)))
            })
            .map(|(_, _, v)| v.as_str())
    }

    /// `get_for` with no conditional groups — `[all]` only.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.get_for(key, &[])
    }

    /// `BOOT_ORDER` decoded into device codes, first-tried first. `0xf41` →
    /// `[1, 4, 0xf]` (SD, USB-MSD, RESTART).
    pub fn boot_order(&self) -> Option<Vec<u8>> {
        let raw = self.get("BOOT_ORDER")?;
        let raw = raw.trim().trim_start_matches("0x").trim_start_matches("0X");
        let val = u32::from_str_radix(raw, 16).ok()?;
        let width = (8 - val.leading_zeros() / 4).max(1); // significant nibbles
        Some((0..width).map(|i| ((val >> (i * 4)) & 0xf) as u8).collect())
    }

    /// `BOOT_ORDER` as `"SD CARD -> USB-MSD -> RESTART"`.
    pub fn boot_order_names(&self) -> Option<String> {
        Some(
            self.boot_order()?
                .iter()
                .map(|&c| boot_device_name(c))
                .collect::<Vec<_>>()
                .join(" -> "),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file section: header, 16-byte name field, body, 8-byte aligned.
    fn file_section(img: &mut Vec<u8>, name: &str, body: &[u8]) {
        img.extend_from_slice(&MAGIC_FILE.to_be_bytes());
        img.extend_from_slice(&((FILENAME_FIELD + body.len()) as u32).to_be_bytes());
        let mut field = [0u8; FILENAME_FIELD];
        field[..name.len()].copy_from_slice(name.as_bytes());
        img.extend_from_slice(&field);
        img.extend_from_slice(body);
        while !img.len().is_multiple_of(8) {
            img.push(0xFF);
        }
    }

    #[test]
    fn replace_file_shrinks_the_slot_and_pads_up_to_the_next_section() {
        let mut img = Vec::new();
        file_section(&mut img, "pubkey.bin", &[0; 512]);
        let next = img.len();
        file_section(&mut img, "bootconf.txt", b"BOOT_ORDER=0xf41\n");
        img.extend_from_slice(&[0xFF; 64]);

        let key: Vec<u8> = (0..264).map(|i| i as u8).collect();
        replace_file(&mut img, "pubkey.bin", &key).unwrap();
        let parsed = EepromImage::parse(&img).unwrap();
        let names: Vec<_> = parsed.files().map(|(n, _)| n.to_string()).collect();
        assert_eq!(
            names,
            ["pubkey.bin", "bootconf.txt"],
            "the walk still reaches bootconf"
        );
        assert_eq!(parsed.files().next().unwrap().1, key.as_slice());
        let pad = &parsed.sections[1];
        assert_eq!(pad.magic, MAGIC_PAD);
        assert!(pad.body.iter().all(|&b| b == 0xFF));
        assert_eq!(parsed.sections[2].header_offset, next);

        assert!(
            replace_file(&mut img, "pubkey.bin", &[0; 600]).is_err(),
            "too big"
        );
        assert!(replace_file(&mut img, "nope", &key).is_err());
    }

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

    #[test]
    fn bootconf_groups_and_boot_order() {
        let text = "\
[all]
BOOT_UART=0
BOOT_ORDER=0xf41

[0xdeadbeef]
BOOT_UART=1
";
        let c = BootConf::parse(text);
        assert_eq!(c.get("BOOT_UART"), Some("0"));
        assert_eq!(c.get_for("BOOT_UART", &["0xdeadbeef"]), Some("1"));
        assert_eq!(c.boot_order(), Some(vec![1, 4, 0xf]));
        assert_eq!(
            c.boot_order_names().as_deref(),
            Some("SD CARD -> USB-MSD -> RESTART")
        );
    }

    /// The pinned `firmware/pieeprom.bin`, when present, must walk end to end
    /// and expose a sane `bootconf.txt`.
    #[test]
    fn parses_the_pinned_image() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/firmware/pieeprom.bin");
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("skipping: {path} not fetched");
            return;
        };
        // `parse` returning Ok means the walk reached the EOF marker without
        // bailing on a bad magic or an out-of-range length.
        let img = EepromImage::parse(&bytes).expect("parse pinned pieeprom.bin");
        assert!(img.bootcode().is_some(), "has a bootcode section");
        assert!(img.file("bootconf.txt").is_some(), "has bootconf.txt");
        let conf = img.bootconf().expect("bootconf parses");
        assert!(
            conf.get("BOOT_ORDER").is_some() || conf.get("NET_INSTALL_AT_POWER_ON").is_some(),
            "bootconf has recognisable keys: {:?}",
            conf.entries
        );
    }
}
