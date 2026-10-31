//! A disk described by its partitions, after `systemd-repart`: a GPT, and one
//! partition per entry with a `type`, a `format` and the `files` to put in it.
//! The built-in subset is the types `esp`, `root`, `usr`, `usr-verity` and
//! `usr-verity-sig` and the formats `vfat` and `ext4`.
//!
//! Like [`crate::fat`], only the metadata is built: every file is mapped onto
//! its source, whether a host file or a URL read as the guest asks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::fat::{Card, Entry, Extent, Kind, Source};
use crate::periph::disk::BLOCK_SIZE;

/// A partition's blocks, as a filesystem builder makes them: the metadata held
/// in memory and the files mapped, all counted from the partition's first
/// sector.
pub struct Volume {
    pub meta: Vec<u8>,
    pub extents: Vec<Extent>,
    pub sectors: u64,
}

/// What a medium argument starts with to name a file of partitions, as
/// `boot --sd partitions:disk.yaml` takes it.
pub const PARTITIONS_PREFIX: &str = "partitions:";

const ALIGN: u64 = 2048;
const ENTRIES: u64 = 128;
const ENTRY_BYTES: u64 = 128;
const TABLE_SECTORS: u64 = ENTRIES * ENTRY_BYTES / BLOCK_SIZE as u64;
const FIRST_USABLE: u64 = 2 + TABLE_SECTORS;

/// The partitions of a disk, in the order they lie on it.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartitionsSpec {
    pub partitions: Vec<PartitionSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartitionSpec {
    /// What the partition is for, which is the GUID the GPT names it by (the
    /// ones for the architecture of the board: `root` is `root-arm64`).
    #[serde(rename = "type")]
    pub kind: PartitionType,
    /// The filesystem to make in the partition. Without one it is zeros, and
    /// `size` says how long.
    #[serde(default)]
    pub format: Option<Format>,
    /// The partition's name in the GPT.
    #[serde(default)]
    pub label: Option<String>,
    /// At least this long, in bytes or with a `K`, `M`, `G` or `T` (powers of
    /// 1024). A filesystem is as long as its files need, with some room to
    /// spare, if this is less.
    #[serde(default)]
    pub size: Option<Size>,
    /// File name in the filesystem (a `/` makes a directory) and where it
    /// comes from.
    #[serde(default)]
    pub files: BTreeMap<String, FileSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PartitionType {
    /// EFI System Partition.
    Esp,
    Root,
    Usr,
    UsrVerity,
    UsrVeritySig,
}

impl PartitionType {
    fn guid(self) -> &'static str {
        match self {
            PartitionType::Esp => "c12a7328-f81f-11d2-ba4b-00a0c93ec93b",
            PartitionType::Root => "b921b045-1df0-41c3-af44-4c6f280d3fae",
            PartitionType::Usr => "b0e01050-ee5f-4390-949a-9101b17104e9",
            PartitionType::UsrVerity => "6e11a4e7-fbca-4ded-b9e9-e1a512bb664e",
            PartitionType::UsrVeritySig => "c23ce4ff-44bd-4b00-b2d4-b41b3419e02a",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Format {
    Vfat,
    Ext4,
}

/// A byte count: a number, or text such as `256M`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum Size {
    Bytes(u64),
    Text(String),
}

impl Size {
    pub fn bytes(&self) -> Result<u64> {
        match self {
            Size::Bytes(n) => Ok(*n),
            Size::Text(text) => {
                let text = text.trim();
                let digits = text.trim_end_matches(|c: char| !c.is_ascii_digit());
                let n: u64 = digits
                    .parse()
                    .with_context(|| format!("{text:?} is not a size"))?;
                let unit = match text[digits.len()..].trim().to_ascii_uppercase().as_str() {
                    "" | "B" => 1,
                    "K" | "KB" | "KIB" => 1 << 10,
                    "M" | "MB" | "MIB" => 1 << 20,
                    "G" | "GB" | "GIB" => 1 << 30,
                    "T" | "TB" | "TIB" => 1 << 40,
                    other => bail!("{text:?}: no unit called {other:?} (K, M, G, T)"),
                };
                n.checked_mul(unit)
                    .with_context(|| format!("{text:?} is too big"))
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum FileSpec {
    /// A file, relative to the scenario, or a URL.
    From(String),
    Inline {
        /// The text of the file.
        inline: String,
    },
    Builtin {
        /// A file `pimu` carries: `halt` is the kernel that parks the ARM.
        builtin: Builtin,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Builtin {
    Halt,
}

impl Builtin {
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Builtin::Halt => &crate::firmware::HALT_KERNEL,
        }
    }
}

impl FileSpec {
    /// Where the file is and how long it is. A path is read relative to
    /// `base`; a URL is only asked how long it is.
    pub fn source(&self, base: &Path) -> Result<(Source, u64)> {
        match self {
            FileSpec::From(from) if crate::remote::is_url(from) => {
                let probe = crate::remote::probe(from)?;
                Ok((Source::Url(from.clone()), probe.len))
            }
            FileSpec::From(from) => {
                let path = base.join(from);
                let len = std::fs::metadata(&path)
                    .with_context(|| format!("reading {}", path.display()))?
                    .len();
                Ok((Source::Path(path), len))
            }
            FileSpec::Inline { inline } => Ok((
                Source::Bytes(inline.clone().into_bytes()),
                inline.len() as u64,
            )),
            FileSpec::Builtin { builtin } => {
                let bytes = builtin.bytes();
                Ok((Source::Bytes(bytes.to_vec()), bytes.len() as u64))
            }
        }
    }
}

enum Tree {
    File(Source, u64),
    Dir(BTreeMap<String, Tree>),
}

/// The tree a map of paths to sources describes.
pub fn entries_of_files(files: &BTreeMap<String, FileSpec>, base: &Path) -> Result<Vec<Entry>> {
    let mut root = BTreeMap::new();
    for (name, spec) in files {
        let parts: Vec<&str> = name.split('/').filter(|p| !p.is_empty()).collect();
        let Some((leaf, dirs)) = parts.split_last() else {
            bail!("{name:?} names no file");
        };
        let mut dir = &mut root;
        for part in dirs {
            let Tree::Dir(next) = dir
                .entry(part.to_string())
                .or_insert_with(|| Tree::Dir(BTreeMap::new()))
            else {
                bail!("{name}: {part} is a file, not a directory");
            };
            dir = next;
        }
        let (source, len) = spec.source(base)?;
        if dir
            .insert(leaf.to_string(), Tree::File(source, len))
            .is_some()
        {
            bail!("{name}: listed twice, or both a file and a directory");
        }
    }
    Ok(entries_of_tree(root))
}

fn entries_of_tree(tree: BTreeMap<String, Tree>) -> Vec<Entry> {
    tree.into_iter()
        .map(|(name, node)| Entry {
            name,
            kind: match node {
                Tree::File(source, len) => Kind::File { source, len },
                Tree::Dir(children) => Kind::Dir(entries_of_tree(children)),
            },
        })
        .collect()
}

impl PartitionsSpec {
    /// The partitions in a YAML file; the paths in it are relative to the file.
    pub fn load(path: &Path) -> Result<(PartitionsSpec, PathBuf)> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading partitions {}", path.display()))?;
        let spec = yaml_serde::from_str(&text)
            .with_context(|| format!("parsing partitions {}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        Ok((spec, base))
    }

    /// The disk: a GPT, then the partitions.
    pub fn card(&self, base: &Path) -> Result<Card> {
        if self.partitions.is_empty() {
            bail!("a disk with no partitions");
        }
        if self.partitions.len() as u64 > ENTRIES {
            bail!(
                "{} partitions is more than the table holds",
                self.partitions.len()
            );
        }
        let mut extents = Vec::new();
        let mut placed = Vec::new();
        let mut next = ALIGN;
        for (i, part) in self.partitions.iter().enumerate() {
            let volume = part
                .volume(next, base)
                .with_context(|| format!("partition {} ({:?})", i + 1, part.kind))?;
            let sectors = volume.sectors.next_multiple_of(8);
            if !volume.meta.is_empty() {
                extents.push(bytes_at(next, volume.meta));
            }
            extents.extend(volume.extents.into_iter().map(|mut e| {
                e.lba += next;
                e
            }));
            placed.push((next, sectors));
            next = (next + sectors).next_multiple_of(ALIGN);
        }
        let total = (next + TABLE_SECTORS + 1).next_multiple_of(1024);

        let seed = disk_seed(&self.partitions, &placed);
        let table = entry_table(&self.partitions, &placed, &seed);
        let mut head = vec![0u8; FIRST_USABLE as usize * BLOCK_SIZE];
        protective_mbr(&mut head[..BLOCK_SIZE], total);
        let primary = header(1, total - 1, total - 1 - TABLE_SECTORS, &seed, &table, 2);
        head[BLOCK_SIZE..2 * BLOCK_SIZE][..primary.len()].copy_from_slice(&primary);
        head[2 * BLOCK_SIZE..].copy_from_slice(&table);

        let mut tail = table;
        let backup = header(
            total - 1,
            1,
            total - 1 - TABLE_SECTORS,
            &seed,
            &tail,
            total - 1 - TABLE_SECTORS,
        );
        let mut last = vec![0u8; BLOCK_SIZE];
        last[..backup.len()].copy_from_slice(&backup);
        tail.extend_from_slice(&last);
        extents.push(bytes_at(total - 1 - TABLE_SECTORS, tail));

        Ok(Card {
            meta: head,
            extents,
            blocks: total,
        })
    }
}

impl PartitionSpec {
    fn volume(&self, hidden: u64, base: &Path) -> Result<Volume> {
        let min_sectors = match &self.size {
            Some(size) => size.bytes()?.div_ceil(BLOCK_SIZE as u64),
            None => 0,
        };
        let Some(format) = self.format else {
            if !self.files.is_empty() {
                bail!("`files` need a `format` to be put in");
            }
            if min_sectors == 0 {
                bail!("a partition with no `format` needs a `size`");
            }
            return Ok(Volume {
                meta: Vec::new(),
                extents: Vec::new(),
                sectors: min_sectors,
            });
        };
        let entries = entries_of_files(&self.files, base)?;
        match format {
            Format::Vfat => {
                let hidden = u32::try_from(hidden).context("a FAT32 volume starts below 2 TiB")?;
                crate::fat::volume_from_entries(entries, hidden, min_sectors)
            }
            Format::Ext4 => crate::ext4::volume_from_entries(entries, min_sectors),
        }
    }
}

fn bytes_at(lba: u64, bytes: Vec<u8>) -> Extent {
    Extent {
        lba,
        blocks: (bytes.len() as u64).div_ceil(BLOCK_SIZE as u64),
        len: bytes.len() as u64,
        source: Source::Bytes(bytes),
    }
}

/// What the disk's and the partitions' GUIDs come from, so that the same
/// description always builds the same disk.
fn disk_seed(parts: &[PartitionSpec], placed: &[(u64, u64)]) -> [u8; 20] {
    let mut sha = Sha1::new();
    for (part, (start, sectors)) in parts.iter().zip(placed) {
        sha.update(part.kind.guid());
        sha.update(start.to_le_bytes());
        sha.update(sectors.to_le_bytes());
        sha.update(part.label.as_deref().unwrap_or("").as_bytes());
    }
    sha.finalize().into()
}

/// A GUID out of bytes, with the version and variant bits a random one has.
fn derived_guid(seed: &[u8; 20], index: u8) -> [u8; 16] {
    let mut sha = Sha1::new();
    sha.update(seed);
    sha.update([index]);
    let digest = sha.finalize();
    let mut g: [u8; 16] = digest[..16].try_into().unwrap();
    g[7] = g[7] & 0x0f | 0x40;
    g[8] = g[8] & 0x3f | 0x80;
    g
}

/// A GUID as the GPT stores it: the first three fields little-endian.
fn guid_bytes(text: &str) -> [u8; 16] {
    let hex: Vec<u8> = text
        .bytes()
        .filter(|b| *b != b'-')
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect();
    let mut g = [0u8; 16];
    g[0..4].copy_from_slice(&[hex[3], hex[2], hex[1], hex[0]]);
    g[4..6].copy_from_slice(&[hex[5], hex[4]]);
    g[6..8].copy_from_slice(&[hex[7], hex[6]]);
    g[8..16].copy_from_slice(&hex[8..16]);
    g
}

fn entry_table(parts: &[PartitionSpec], placed: &[(u64, u64)], seed: &[u8; 20]) -> Vec<u8> {
    let mut table = vec![0u8; (ENTRIES * ENTRY_BYTES) as usize];
    for (i, (part, (start, sectors))) in parts.iter().zip(placed).enumerate() {
        let e = &mut table[i * ENTRY_BYTES as usize..(i + 1) * ENTRY_BYTES as usize];
        e[0..16].copy_from_slice(&guid_bytes(part.kind.guid()));
        e[16..32].copy_from_slice(&derived_guid(seed, i as u8 + 1));
        e[32..40].copy_from_slice(&start.to_le_bytes());
        e[40..48].copy_from_slice(&(start + sectors - 1).to_le_bytes());
        let label = part.label.as_deref().unwrap_or("");
        for (j, unit) in label.encode_utf16().take(36).enumerate() {
            e[56 + j * 2..58 + j * 2].copy_from_slice(&unit.to_le_bytes());
        }
    }
    table
}

fn header(
    this: u64,
    other: u64,
    backup_table: u64,
    seed: &[u8; 20],
    table: &[u8],
    table_lba: u64,
) -> Vec<u8> {
    let mut h = vec![0u8; 92];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&this.to_le_bytes());
    h[32..40].copy_from_slice(&other.to_le_bytes());
    h[40..48].copy_from_slice(&FIRST_USABLE.to_le_bytes());
    h[48..56].copy_from_slice(&(backup_table - 1).to_le_bytes());
    h[56..72].copy_from_slice(&derived_guid(seed, 0));
    h[72..80].copy_from_slice(&table_lba.to_le_bytes());
    h[80..84].copy_from_slice(&(ENTRIES as u32).to_le_bytes());
    h[84..88].copy_from_slice(&(ENTRY_BYTES as u32).to_le_bytes());
    h[88..92].copy_from_slice(&crc32(table).to_le_bytes());
    let crc = crc32(&h);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

/// The MBR a GPT disk carries so that a tool that only knows MBR sees one
/// partition spanning the disk.
fn protective_mbr(sector: &mut [u8], total: u64) {
    let p = &mut sector[0x1be..0x1ce];
    p[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
    p[4] = 0xee;
    p[5..8].copy_from_slice(&[0xff, 0xff, 0xff]);
    p[8..12].copy_from_slice(&1u32.to_le_bytes());
    let size = u32::try_from(total - 1).unwrap_or(u32::MAX);
    p[12..16].copy_from_slice(&size.to_le_bytes());
    sector[510..512].copy_from_slice(&[0x55, 0xaa]);
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(text: &str) -> PartitionsSpec {
        yaml_serde::from_str(text).expect("partitions")
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn a_guid_is_stored_with_its_first_three_fields_little_endian() {
        assert_eq!(
            guid_bytes("c12a7328-f81f-11d2-ba4b-00a0c93ec93b"),
            [
                0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e,
                0xc9, 0x3b
            ]
        );
    }

    #[test]
    fn sizes_take_a_unit() {
        let of = |s: &str| Size::Text(s.into()).bytes().unwrap();
        assert_eq!(of("512"), 512);
        assert_eq!(of("4K"), 4096);
        assert_eq!(of("256M"), 256 << 20);
        assert_eq!(of("2G"), 2 << 30);
        assert!(Size::Text("2X".into()).bytes().is_err());
    }

    #[test]
    fn a_partition_with_a_key_repart_has_and_we_do_not_is_refused() {
        let bad = "partitions:\n  - type: esp\n    copy-files: x\n";
        assert!(yaml_serde::from_str::<PartitionsSpec>(bad).is_err());
        let bad = "partitions:\n  - type: swap\n";
        assert!(yaml_serde::from_str::<PartitionsSpec>(bad).is_err());
    }

    #[test]
    fn the_gpt_names_every_partition_and_the_backup_ends_the_disk() {
        let s = spec(
            "partitions:
  - type: esp
    format: vfat
    label: boot
    files:
      config.txt: { inline: \"x\\n\" }
  - type: root
    format: ext4
    files:
      etc/hostname: { inline: \"pimu\\n\" }
",
        );
        let card = s.card(Path::new(".")).unwrap();
        assert_eq!(&card.meta[BLOCK_SIZE..BLOCK_SIZE + 8], b"EFI PART");
        let entries = &card.meta[2 * BLOCK_SIZE..];
        assert_eq!(
            &entries[..16],
            &guid_bytes("c12a7328-f81f-11d2-ba4b-00a0c93ec93b")
        );
        assert_eq!(
            &entries[128..144],
            &guid_bytes("b921b045-1df0-41c3-af44-4c6f280d3fae")
        );
        let first = u64::from_le_bytes(entries[32..40].try_into().unwrap());
        let second = u64::from_le_bytes(entries[160..168].try_into().unwrap());
        assert_eq!(first, 2048);
        assert!(second > first && second % ALIGN == 0);
        let last = card.extents.last().unwrap();
        assert_eq!(last.lba + last.blocks, card.blocks);
        let Source::Bytes(tail) = &last.source else {
            panic!("the backup is made up, not read from a file")
        };
        assert_eq!(&tail[tail.len() - BLOCK_SIZE..][..8], b"EFI PART");
        for pair in card.extents.windows(2) {
            assert!(
                pair[0].lba + pair[0].blocks <= pair[1].lba,
                "extents overlap"
            );
        }
    }

    #[test]
    fn a_partition_without_a_filesystem_needs_a_size() {
        let s = spec("partitions:\n  - type: usr-verity\n");
        assert!(s.card(Path::new(".")).is_err());
        let s = spec("partitions:\n  - type: usr-verity\n    size: 1M\n");
        assert!(s.card(Path::new(".")).is_ok());
    }

    #[test]
    fn files_in_directories_become_a_tree() {
        let s = spec(
            "partitions:
  - type: esp
    format: vfat
    files:
      a/b/c: { inline: x }
      a/d: { inline: y }
",
        );
        let entries = entries_of_files(&s.partitions[0].files, Path::new(".")).unwrap();
        assert_eq!(entries.len(), 1);
        let Kind::Dir(a) = &entries[0].kind else {
            panic!("a is a directory")
        };
        assert_eq!(
            a.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["b", "d"]
        );
        let clash = spec(
            "partitions:
  - type: esp
    format: vfat
    files:
      a: { inline: x }
      a/b: { inline: y }
",
        );
        assert!(entries_of_files(&clash.partitions[0].files, Path::new(".")).is_err());
    }
}
