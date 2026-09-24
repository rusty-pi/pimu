//! A directory of files as an SD card: MBR, one FAT32 boot partition, and the
//! files in it.
//!
//! A checkout of `raspberrypi/firmware` is a boot partition's contents and not
//! an image, so `boot` builds the card the firmware expects around it rather
//! than asking for one (#144). Only the metadata is built here — the boot
//! sector, both FATs and the directory clusters — and every file's clusters
//! are mapped onto the host file instead, so a 150 MB checkout costs the
//! blocks the firmware reads rather than its size, the way an image file does
//! (`periph::disk`, #54).
//!
//! The directories are allocated before the files for that reason: everything
//! the model has to answer out of memory is then one run of blocks at the
//! front of the card, and everything behind it is a file on the host.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::periph::disk::BLOCK_SIZE;

/// Where the partition starts, as every Pi card has it: 1 MiB in.
const PART_START: u32 = 2048;
/// FAT32 with 4 KiB clusters. Small enough that a volume of a few hundred
/// megabytes still has the 65525 clusters that make it FAT32 and not FAT16 to
/// a driver that tells the two apart by count, as the specification says to.
const SECTORS_PER_CLUSTER: u32 = 8;
/// The boot sector, its backup at 6, the two FSInfo sectors and room to align
/// the first FAT: what `mformat` reserves.
const RESERVED_SECTORS: u32 = 32;
const FATS: u32 = 2;
/// Below this a driver reads the volume as FAT16 whatever the boot sector
/// says, so the partition is padded out to it.
const MIN_CLUSTERS: u32 = 65525 + 16;
/// 1980-01-01, the FAT epoch: every entry is stamped with it, so the same
/// directory always builds the same card.
const EPOCH_DATE: u16 = (1 << 5) | 1;
const VOLUME_LABEL: &[u8; 11] = b"PIMU       ";
const DIR_ENTRY: usize = 32;

/// One file's data, as blocks of the card and bytes of a host file.
pub struct Extent {
    /// The first block of the file's data, from the start of the card.
    pub lba: u64,
    /// How many blocks its clusters cover, the tail of the last one included.
    pub blocks: u64,
    pub path: PathBuf,
    pub len: u64,
}

/// A card built out of a directory: the metadata blocks, in order from the
/// MBR, and where each file's data sits behind them.
pub struct Card {
    /// Blocks `0..`: the MBR, the boot sector, both FATs and every directory.
    pub meta: Vec<u8>,
    /// The files, by ascending `lba`, none of them overlapping.
    pub extents: Vec<Extent>,
    /// The card's size.
    pub blocks: u64,
}

/// What a directory has to hold to be taken for a boot partition rather than a
/// working directory that `boot` happens to have been run in.
pub fn is_boot_partition(dir: &Path) -> bool {
    ["start4.elf", "start.elf", "config.txt"]
        .iter()
        .any(|name| dir.join(name).is_file())
}

/// Build the card whose boot partition holds `dir`.
pub fn card_from_dir(dir: &Path) -> Result<Card> {
    Builder::default().build(read_dir(dir)?)
}

/// A directory of the volume, as it is read off the host.
struct Entry {
    name: String,
    kind: Kind,
}

enum Kind {
    File { path: PathBuf, len: u64 },
    Dir(Vec<Entry>),
}

/// The host directory, by name: the order the entries are written in is the
/// order the firmware finds them, so it may not depend on the host's readdir
/// order or on its collation (`scripts/make-sd.sh` pins the same thing).
/// Dot files are left out, so a `.git` in a firmware checkout is not a
/// directory of the card.
fn read_dir(dir: &Path) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let meta =
            std::fs::metadata(&path).with_context(|| format!("reading {}", path.display()))?;
        let kind = if meta.is_dir() {
            Kind::Dir(read_dir(&path)?)
        } else if meta.is_file() {
            Kind::File {
                path,
                len: meta.len(),
            }
        } else {
            continue;
        };
        entries.push(Entry { name, kind });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

/// A directory of the volume once its entries are named: how much room they
/// take, and which of them are directories themselves.
struct Planned {
    items: Vec<Item>,
    /// Where the directory's own bytes go, filled in by the allocation pass.
    first: u32,
    clusters: u32,
    /// The directory this one is in, for its `..` entry; `None` for the root.
    parent: Option<usize>,
}

struct Item {
    long: Option<String>,
    short: ShortName,
    what: What,
}

enum What {
    /// The index of the planned directory this entry names.
    Dir(usize),
    File {
        path: PathBuf,
        len: u64,
    },
}

#[derive(Default)]
struct Builder {
    /// Every directory of the volume, the root first.
    dirs: Vec<Planned>,
}

impl Builder {
    fn cluster_bytes() -> u32 {
        SECTORS_PER_CLUSTER * BLOCK_SIZE as u32
    }

    /// Name every entry and work out how much room each directory needs. The
    /// clusters are not known yet: they are handed out afterwards, directories
    /// first.
    fn plan(&mut self, entries: Vec<Entry>, parent: Option<usize>) -> usize {
        let me = self.dirs.len();
        self.dirs.push(Planned {
            items: Vec::new(),
            first: 0,
            clusters: 0,
            parent,
        });

        // The root carries its volume label where a subdirectory carries `.`
        // and `..`.
        let mut bytes = if parent.is_some() { 2 } else { 1 } * DIR_ENTRY;
        let mut taken: Vec<[u8; 11]> = Vec::new();
        let mut items = Vec::new();
        for entry in entries {
            let short = short_name(&entry.name, &mut taken);
            let long = short.long.then(|| entry.name.clone());
            bytes += DIR_ENTRY + long.as_deref().map_or(0, |n| fragments(n) * DIR_ENTRY);
            let what = match entry.kind {
                Kind::Dir(children) => What::Dir(self.plan(children, Some(me))),
                Kind::File { path, len } => What::File { path, len },
            };
            items.push(Item { long, short, what });
        }
        let dir = &mut self.dirs[me];
        dir.items = items;
        dir.clusters = (bytes as u32).div_ceil(Self::cluster_bytes()).max(1);
        me
    }

    fn build(mut self, root: Vec<Entry>) -> Result<Card> {
        self.plan(root, None);

        // Cluster 2 is the root directory, which is what the boot sector says
        // and what a driver that does not read it assumes.
        let mut fat: Vec<u32> = vec![0x0fff_fff8, 0x0fff_ffff];
        let chain = |fat: &mut Vec<u32>, count: u32| -> u32 {
            let first = fat.len() as u32;
            for i in 1..count {
                fat.push(first + i);
            }
            fat.push(0x0fff_ffff);
            first
        };
        for i in 0..self.dirs.len() {
            self.dirs[i].first = chain(&mut fat, self.dirs[i].clusters);
        }
        let mut extents = Vec::new();
        let mut files: Vec<(usize, usize, u32)> = Vec::new();
        for (d, dir) in self.dirs.iter().enumerate() {
            for (i, item) in dir.items.iter().enumerate() {
                let What::File { path, len } = &item.what else {
                    continue;
                };
                // A FAT32 entry carries the size in 32 bits, so nothing that
                // big can go on a card at all.
                if *len > u32::MAX as u64 {
                    bail!(
                        "{}: {len} bytes is more than a FAT32 volume can hold",
                        path.display()
                    );
                }
                // An empty file has no clusters, and its entry says cluster 0.
                if *len == 0 {
                    files.push((d, i, 0));
                    continue;
                }
                let clusters = len.div_ceil(Self::cluster_bytes() as u64) as u32;
                let first = chain(&mut fat, clusters);
                files.push((d, i, first));
                extents.push(Extent {
                    lba: first as u64,
                    blocks: (clusters * SECTORS_PER_CLUSTER) as u64,
                    path: path.clone(),
                    len: *len,
                });
            }
        }

        let clusters = (fat.len() as u32 - 2).max(MIN_CLUSTERS);
        let fat_sectors = (clusters + 2).div_ceil(BLOCK_SIZE as u32 / 4);
        let data_start = PART_START + RESERVED_SECTORS + FATS * fat_sectors;
        let part_sectors = data_start - PART_START + clusters * SECTORS_PER_CLUSTER;
        let lba_of = |cluster: u32| (data_start + (cluster - 2) * SECTORS_PER_CLUSTER) as u64;
        for extent in &mut extents {
            extent.lba = lba_of(extent.lba as u32);
        }

        // The metadata reaches to the end of the last directory, which the
        // allocation above put in front of every file.
        let dirs_end = self
            .dirs
            .iter()
            .map(|d| lba_of(d.first) + (d.clusters * SECTORS_PER_CLUSTER) as u64)
            .max()
            .unwrap_or(data_start as u64);
        let mut meta = vec![0u8; dirs_end as usize * BLOCK_SIZE];
        mbr(&mut meta[..BLOCK_SIZE], part_sectors);
        let part = &mut meta[PART_START as usize * BLOCK_SIZE..];
        boot_sector(part, part_sectors, fat_sectors);
        // The backup the boot sector points at, and an FSInfo sector beside
        // each, which a driver reads before it believes either.
        let (first, rest) = part.split_at_mut(6 * BLOCK_SIZE);
        rest[..BLOCK_SIZE].copy_from_slice(&first[..BLOCK_SIZE]);
        fs_info(&mut part[BLOCK_SIZE..2 * BLOCK_SIZE]);
        fs_info(&mut part[7 * BLOCK_SIZE..8 * BLOCK_SIZE]);

        let fat_at = (PART_START + RESERVED_SECTORS) as usize * BLOCK_SIZE;
        for copy in 0..FATS as usize {
            let at = fat_at + copy * fat_sectors as usize * BLOCK_SIZE;
            for (i, entry) in fat.iter().enumerate() {
                meta[at + i * 4..at + i * 4 + 4].copy_from_slice(&entry.to_le_bytes());
            }
        }

        let mut clusters_of: Vec<u32> = vec![0; self.dirs.len()];
        for (i, dir) in self.dirs.iter().enumerate() {
            clusters_of[i] = dir.first;
        }
        let file_cluster = |d: usize, i: usize| {
            files
                .iter()
                .find(|(fd, fi, _)| *fd == d && *fi == i)
                .map(|(_, _, c)| *c)
                .unwrap_or(0)
        };
        for (d, dir) in self.dirs.iter().enumerate() {
            let bytes = self.entries_of(d, &clusters_of, &file_cluster);
            let at = lba_of(dir.first) as usize * BLOCK_SIZE;
            meta[at..at + bytes.len()].copy_from_slice(&bytes);
        }

        let blocks = (PART_START + part_sectors) as u64;
        Ok(Card {
            meta,
            extents,
            // A card is a whole number of 512 KiB units to its CSD, and the
            // firmware reads its size from there.
            blocks: blocks.next_multiple_of(1024),
        })
    }

    /// One directory's entries, as they go on the volume.
    fn entries_of(
        &self,
        d: usize,
        clusters_of: &[u32],
        file_cluster: &impl Fn(usize, usize) -> u32,
    ) -> Vec<u8> {
        let dir = &self.dirs[d];
        let mut out = Vec::new();
        match dir.parent {
            None => out.extend_from_slice(&volume_label()),
            Some(up) => {
                out.extend_from_slice(&dot_entry(b".          ", dir.first));
                // The root's `..` is 0, not 2, as the specification has it.
                let up = if up == 0 { 0 } else { clusters_of[up] };
                out.extend_from_slice(&dot_entry(b"..         ", up));
            }
        }
        for (i, item) in dir.items.iter().enumerate() {
            if let Some(long) = &item.long {
                out.extend_from_slice(&long_entries(long, &item.short.short));
            }
            let (cluster, size, attr) = match &item.what {
                What::Dir(child) => (clusters_of[*child], 0, 0x10),
                What::File { len, .. } => (file_cluster(d, i), *len as u32, 0x20),
            };
            out.extend_from_slice(&short_entry(&item.short, cluster, size, attr));
        }
        out
    }
}

/// One partition, FAT32 LBA (type `0x0c`) and bootable, as `sfdisk` writes it
/// for a Pi card.
fn mbr(sector: &mut [u8], part_sectors: u32) {
    sector[0x1b8..0x1bc].copy_from_slice(&0x5250_494du32.to_le_bytes());
    let p = &mut sector[0x1be..0x1ce];
    p[0] = 0x80;
    // The CHS fields say "past what CHS can address", which is what every tool
    // writes for a partition this far into a card.
    p[1..4].copy_from_slice(&[0xfe, 0xff, 0xff]);
    p[4] = 0x0c;
    p[5..8].copy_from_slice(&[0xfe, 0xff, 0xff]);
    p[8..12].copy_from_slice(&PART_START.to_le_bytes());
    p[12..16].copy_from_slice(&part_sectors.to_le_bytes());
    sector[510..512].copy_from_slice(&[0x55, 0xaa]);
}

fn boot_sector(part: &mut [u8], part_sectors: u32, fat_sectors: u32) {
    let s = &mut part[..BLOCK_SIZE];
    s[0..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    s[3..11].copy_from_slice(b"pimu    ");
    s[11..13].copy_from_slice(&(BLOCK_SIZE as u16).to_le_bytes());
    s[13] = SECTORS_PER_CLUSTER as u8;
    s[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    s[16] = FATS as u8;
    s[21] = 0xf8;
    s[24..26].copy_from_slice(&32u16.to_le_bytes());
    s[26..28].copy_from_slice(&64u16.to_le_bytes());
    s[28..32].copy_from_slice(&PART_START.to_le_bytes());
    s[32..36].copy_from_slice(&part_sectors.to_le_bytes());
    s[36..40].copy_from_slice(&fat_sectors.to_le_bytes());
    s[44..48].copy_from_slice(&2u32.to_le_bytes());
    s[48..50].copy_from_slice(&1u16.to_le_bytes());
    s[50..52].copy_from_slice(&6u16.to_le_bytes());
    s[64] = 0x80;
    s[66] = 0x29;
    s[67..71].copy_from_slice(&0x5250_494du32.to_le_bytes());
    s[71..82].copy_from_slice(VOLUME_LABEL);
    s[82..90].copy_from_slice(b"FAT32   ");
    s[510..512].copy_from_slice(&[0x55, 0xaa]);
}

/// Neither the free count nor the next free cluster is known to be right here,
/// so both say "unknown", which a driver has to accept.
fn fs_info(sector: &mut [u8]) {
    sector[0..4].copy_from_slice(b"RRaA");
    sector[484..488].copy_from_slice(b"rrAa");
    sector[488..492].copy_from_slice(&u32::MAX.to_le_bytes());
    sector[492..496].copy_from_slice(&u32::MAX.to_le_bytes());
    sector[510..512].copy_from_slice(&[0x55, 0xaa]);
}

fn volume_label() -> [u8; DIR_ENTRY] {
    let mut e = [0u8; DIR_ENTRY];
    e[..11].copy_from_slice(VOLUME_LABEL);
    e[11] = 0x08;
    stamp(&mut e);
    e
}

fn dot_entry(name: &[u8; 11], cluster: u32) -> [u8; DIR_ENTRY] {
    let mut e = [0u8; DIR_ENTRY];
    e[..11].copy_from_slice(name);
    e[11] = 0x10;
    stamp(&mut e);
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e
}

/// The FAT epoch in an entry's three time fields.
fn stamp(entry: &mut [u8; DIR_ENTRY]) {
    entry[16..18].copy_from_slice(&EPOCH_DATE.to_le_bytes());
    entry[18..20].copy_from_slice(&EPOCH_DATE.to_le_bytes());
    entry[24..26].copy_from_slice(&EPOCH_DATE.to_le_bytes());
}

/// A name as the directory carries it: the 8.3 entry, whether the long name
/// has to be spelled out beside it, and the case flags that spare a name like
/// `start4.elf` the long entries.
struct ShortName {
    short: [u8; 11],
    /// `0x08` for a lowercase base, `0x10` for a lowercase extension.
    case: u8,
    long: bool,
}

fn short_entry(name: &ShortName, cluster: u32, size: u32, attr: u8) -> [u8; DIR_ENTRY] {
    let mut e = [0u8; DIR_ENTRY];
    e[..11].copy_from_slice(&name.short);
    e[11] = attr;
    e[12] = name.case;
    stamp(&mut e);
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

/// How many long entries a name takes: thirteen UTF-16 code units each, its
/// terminator included.
fn fragments(name: &str) -> usize {
    (name.encode_utf16().count() + 1).div_ceil(13)
}

/// The VFAT entries for a name that does not fit 8.3, as they go on the
/// volume: ahead of the short entry, last fragment first.
fn long_entries(name: &str, short: &[u8; 11]) -> Vec<u8> {
    let checksum = short.iter().fold(0u8, |sum, &c| {
        (sum >> 1).wrapping_add(sum << 7).wrapping_add(c)
    });
    let mut units: Vec<u16> = name.encode_utf16().collect();
    units.push(0);
    // The last fragment is padded out with 0xffff behind the terminator.
    while !units.len().is_multiple_of(13) {
        units.push(0xffff);
    }
    let count = units.len() / 13;
    let mut out = Vec::with_capacity(count * DIR_ENTRY);
    for (n, chunk) in units.chunks(13).enumerate().rev() {
        let mut e = [0u8; DIR_ENTRY];
        e[0] = (n as u8 + 1) | if n + 1 == count { 0x40 } else { 0 };
        e[11] = 0x0f;
        e[13] = checksum;
        for (i, unit) in chunk.iter().enumerate() {
            let at = match i {
                0..=4 => 1 + i * 2,
                5..=10 => 14 + (i - 5) * 2,
                _ => 28 + (i - 11) * 2,
            };
            e[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&e);
    }
    out
}

/// The 8.3 name for `name`, unique among `taken`, which it is added to.
fn short_name(name: &str, taken: &mut Vec<[u8; 11]>) -> ShortName {
    let (base, ext) = match name.rfind('.') {
        Some(at) if at > 0 => (&name[..at], &name[at + 1..]),
        _ => (name, ""),
    };
    let (base_8, base_lossy) = eight_three(base, 8);
    let (ext_3, ext_lossy) = eight_three(ext, 3);
    let lower = |s: &str| s.chars().any(|c| c.is_ascii_lowercase());
    let upper = |s: &str| s.chars().any(|c| c.is_ascii_uppercase());
    // A name fits 8.3 only if nothing was dropped or replaced and its case is
    // all one way, since an entry has one flag for the whole base and one for
    // the whole extension. Otherwise the long entries carry it, and the short
    // name is there to be unique rather than to be read.
    let fits =
        !base_lossy && !ext_lossy && !(lower(base) && upper(base)) && !(lower(ext) && upper(ext));
    let mut case = 0;
    if lower(base) {
        case |= 0x08;
    }
    if lower(ext) {
        case |= 0x10;
    }

    let mut short = pad(&base_8, &ext_3);
    if !fits || taken.contains(&short) {
        case = 0;
        for n in 1..=999_999u32 {
            let tail = format!("~{n}");
            let mut base = base_8.clone();
            base.truncate(8 - tail.len());
            base.extend_from_slice(tail.as_bytes());
            short = pad(&base, &ext_3);
            if !taken.contains(&short) {
                break;
            }
        }
    }
    taken.push(short);
    ShortName {
        short,
        case,
        long: !fits,
    }
}

/// `s` as up to `room` bytes a short name may hold, and whether anything was
/// dropped or replaced on the way.
fn eight_three(s: &str, room: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut lossy = false;
    for c in s.chars() {
        if out.len() == room {
            lossy = true;
            break;
        }
        match c {
            'A'..='Z'
            | '0'..='9'
            | '$'
            | '%'
            | '\''
            | '-'
            | '_'
            | '@'
            | '~'
            | '`'
            | '!'
            | '('
            | ')'
            | '{'
            | '}'
            | '^'
            | '#'
            | '&' => out.push(c as u8),
            'a'..='z' => out.push(c.to_ascii_uppercase() as u8),
            _ => {
                out.push(b'_');
                lossy = true;
            }
        }
    }
    (out, lossy)
}

fn pad(base: &[u8], ext: &[u8]) -> [u8; 11] {
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base);
    out[8..8 + ext.len()].copy_from_slice(ext);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> ShortName {
        short_name(s, &mut Vec::new())
    }

    #[test]
    fn a_plain_lowercase_name_needs_no_long_entries() {
        let n = name("start4.elf");
        assert_eq!(&n.short, b"START4  ELF");
        assert_eq!(n.case, 0x08 | 0x10);
        assert!(!n.long);
    }

    #[test]
    fn a_name_too_long_for_8_3_gets_a_tilde_and_its_long_entries() {
        let n = name("bcm2711-rpi-4-b.dtb");
        assert_eq!(&n.short, b"BCM271~1DTB");
        assert!(n.long);
        // Thirteen code units a fragment, the terminator included.
        assert_eq!(fragments("bcm2711-rpi-4-b.dtb"), 2);
        assert_eq!(long_entries("bcm2711-rpi-4-b.dtb", &n.short).len(), 64);
    }

    #[test]
    fn short_names_that_collide_are_numbered() {
        let mut taken = Vec::new();
        let first = short_name("vc4-kms-v3d.dtbo", &mut taken);
        let second = short_name("vc4-kms-v3d-pi4.dtbo", &mut taken);
        assert_eq!(&first.short, b"VC4-KM~1DTB");
        assert_eq!(&second.short, b"VC4-KM~2DTB");
    }

    #[test]
    fn the_card_is_fat32_with_every_file_behind_its_metadata() {
        let dir = tempdir("fat-card");
        std::fs::write(dir.join("start4.elf"), vec![7u8; 5000]).unwrap();
        std::fs::create_dir(dir.join("overlays")).unwrap();
        std::fs::write(dir.join("overlays/disable-bt.dtbo"), b"x").unwrap();
        let card = card_from_dir(&dir).unwrap();

        let boot = &card.meta[PART_START as usize * BLOCK_SIZE..];
        assert_eq!(&boot[82..90], b"FAT32   ");
        assert_eq!(&boot[510..512], &[0x55, 0xaa]);
        // Enough clusters that nothing reads the volume as FAT16.
        let fat_sectors = u32::from_le_bytes(boot[36..40].try_into().unwrap());
        let data = PART_START + RESERVED_SECTORS + FATS * fat_sectors;
        let total = u32::from_le_bytes(boot[32..36].try_into().unwrap());
        let clusters = (total - (data - PART_START)) / SECTORS_PER_CLUSTER;
        assert!(clusters >= MIN_CLUSTERS, "{clusters} clusters");

        assert_eq!(card.extents.len(), 2);
        let meta_blocks = card.meta.len() as u64 / BLOCK_SIZE as u64;
        for extent in &card.extents {
            assert!(extent.lba >= meta_blocks);
            assert!(extent.lba + extent.blocks <= card.blocks);
        }
        assert_eq!(card.extents[0].len, 5000);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pimu-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
