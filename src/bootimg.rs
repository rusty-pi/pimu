//! A directory of files as a small FAT16 volume with no partition table: the
//! `boot.img` that HTTP boot downloads and the bootloader runs as a ramdisk.
//!
//! [`crate::fat`] builds a whole card, FAT32 and at least 33 MB, which would be
//! downloaded and hashed byte by byte in the bootloader's own software. This is
//! as small as holds the files, with the cluster size picked to keep the cluster
//! count a FAT16 driver accepts (4085 to 65524), the way `mformat` sizes the image
//! `scripts/make-netboot.sh` makes.

use anyhow::{bail, Result};

use crate::fat::{
    dot_entry, fragments, long_entries, short_entry, short_name, stamp, Entry, Kind, ShortName,
    Source, DIR_ENTRY,
};

const SECTOR: usize = 512;
const ROOT_ENTRIES: usize = 512;
const ROOT_SECTORS: usize = ROOT_ENTRIES * DIR_ENTRY / SECTOR;
const FATS: usize = 2;
/// Fewer clusters than this and a driver reads the volume as FAT12.
const MIN_CLUSTERS: usize = 4085;
/// More than this and it is FAT32.
const MAX_CLUSTERS: usize = 65524;
/// Free clusters left at the end, so the volume is not exactly full.
const SPARE: usize = 8;
const LABEL: &[u8; 11] = b"RPIBOOT    ";

struct Dir {
    items: Vec<Item>,
    /// `None` for the root, whose entries have a region of their own.
    first: Option<usize>,
    clusters: usize,
    parent: Option<usize>,
}

struct Item {
    long: Option<String>,
    short: ShortName,
    what: What,
}

enum What {
    Dir(usize),
    File(Vec<u8>),
}

fn plan(dirs: &mut Vec<Dir>, entries: Vec<Entry>, parent: Option<usize>) -> Result<usize> {
    let me = dirs.len();
    dirs.push(Dir {
        items: Vec::new(),
        first: None,
        clusters: 0,
        parent,
    });
    let mut taken = Vec::new();
    let mut items = Vec::new();
    for entry in entries {
        let short = short_name(&entry.name, &mut taken);
        let long = short.long.then(|| entry.name.clone());
        let what = match entry.kind {
            Kind::Dir(children) => What::Dir(plan(dirs, children, Some(me))?),
            Kind::File {
                source: Source::Bytes(bytes),
                ..
            } => What::File(bytes),
            Kind::File { source, .. } => {
                bail!("{source}: a boot image holds bytes, not a path or a URL")
            }
        };
        items.push(Item { long, short, what });
    }
    dirs[me].items = items;
    Ok(me)
}

/// The bytes a directory's entries take, `.` and `..` included for a subdirectory.
fn dir_bytes(dir: &Dir) -> usize {
    let dots = if dir.parent.is_some() { 2 } else { 0 };
    (dots
        + dir
            .items
            .iter()
            .map(|i| 1 + i.long.as_deref().map_or(0, fragments))
            .sum::<usize>())
        * DIR_ENTRY
}

/// The volume holding `root`, as a disk image.
pub fn fat16_image(root: Vec<Entry>) -> Result<Vec<u8>> {
    let mut dirs = Vec::new();
    plan(&mut dirs, root, None)?;
    if dir_bytes(&dirs[0]) / DIR_ENTRY > ROOT_ENTRIES {
        bail!("the root of a FAT16 volume holds {ROOT_ENTRIES} entries");
    }

    let file_bytes: usize = dirs
        .iter()
        .flat_map(|d| &d.items)
        .map(|i| match &i.what {
            What::File(bytes) => bytes.len(),
            What::Dir(_) => 0,
        })
        .sum();
    let sectors_per_cluster = [1usize, 2, 4, 8, 16, 32, 64]
        .into_iter()
        .find(|spc| needed_clusters(&mut dirs, *spc) + SPARE <= MAX_CLUSTERS)
        .filter(|_| file_bytes < MAX_CLUSTERS * 64 * SECTOR)
        .ok_or_else(|| anyhow::anyhow!("{file_bytes} bytes do not fit a FAT16 volume"))?;
    let cluster_bytes = sectors_per_cluster * SECTOR;
    let used = needed_clusters(&mut dirs, sectors_per_cluster);
    let clusters = (used + SPARE).max(MIN_CLUSTERS);

    let fat_sectors = ((clusters + 2) * 2).div_ceil(SECTOR);
    let root_at = 1 + FATS * fat_sectors;
    let data_at = root_at + ROOT_SECTORS;
    let total = data_at + clusters * sectors_per_cluster;
    let mut image = vec![0u8; total * SECTOR];

    boot_sector(
        &mut image[..SECTOR],
        sectors_per_cluster,
        fat_sectors,
        total,
    );

    // Clusters: directories first, then the files, each in a chain.
    let mut fat = vec![0u16; clusters + 2];
    fat[0] = 0xfff8;
    fat[1] = 0xffff;
    let mut next = 2;
    let mut chain = |fat: &mut Vec<u16>, count: usize| -> usize {
        let first = next;
        for i in 0..count {
            fat[first + i] = if i + 1 == count {
                0xffff
            } else {
                (first + i + 1) as u16
            };
        }
        next += count;
        first
    };
    for dir in dirs.iter_mut().skip(1) {
        dir.first = Some(chain(&mut fat, dir.clusters));
    }
    let mut file_first: Vec<Vec<usize>> = Vec::new();
    for dir in &dirs {
        let mut firsts = Vec::new();
        for item in &dir.items {
            firsts.push(match &item.what {
                What::File(bytes) if !bytes.is_empty() => {
                    chain(&mut fat, bytes.len().div_ceil(cluster_bytes))
                }
                _ => 0,
            });
        }
        file_first.push(firsts);
    }

    for copy in 0..FATS {
        let at = (1 + copy * fat_sectors) * SECTOR;
        for (i, entry) in fat.iter().enumerate() {
            image[at + i * 2..at + i * 2 + 2].copy_from_slice(&entry.to_le_bytes());
        }
    }

    let cluster_at = |cluster: usize| (data_at + (cluster - 2) * sectors_per_cluster) * SECTOR;
    for (d, dir) in dirs.iter().enumerate() {
        let mut out = Vec::new();
        if let Some(first) = dir.first {
            out.extend_from_slice(&dot_entry(b".          ", first as u32));
            let up = dir.parent.and_then(|p| dirs[p].first).unwrap_or(0);
            out.extend_from_slice(&dot_entry(b"..         ", up as u32));
        } else {
            out.extend_from_slice(&label_entry());
        }
        for (i, item) in dir.items.iter().enumerate() {
            if let Some(long) = &item.long {
                out.extend_from_slice(&long_entries(long, &item.short.short));
            }
            let (cluster, size, attr) = match &item.what {
                What::Dir(child) => (dirs[*child].first.unwrap_or(0), 0, 0x10),
                What::File(bytes) => (file_first[d][i], bytes.len(), 0x20),
            };
            out.extend_from_slice(&short_entry(&item.short, cluster as u32, size as u32, attr));
        }
        let at = match dir.first {
            Some(first) => cluster_at(first),
            None => root_at * SECTOR,
        };
        image[at..at + out.len()].copy_from_slice(&out);
    }
    for (d, dir) in dirs.iter().enumerate() {
        for (i, item) in dir.items.iter().enumerate() {
            if let What::File(bytes) = &item.what {
                if !bytes.is_empty() {
                    let at = cluster_at(file_first[d][i]);
                    image[at..at + bytes.len()].copy_from_slice(bytes);
                }
            }
        }
    }
    Ok(image)
}

/// How many clusters the files and the subdirectories take at this cluster
/// size, and the size each subdirectory gets.
fn needed_clusters(dirs: &mut [Dir], sectors_per_cluster: usize) -> usize {
    let cluster_bytes = sectors_per_cluster * SECTOR;
    let mut total = 0;
    for (i, dir) in dirs.iter_mut().enumerate() {
        if i > 0 {
            dir.clusters = dir_bytes(dir).div_ceil(cluster_bytes).max(1);
            total += dir.clusters;
        }
        for item in &dir.items {
            if let What::File(bytes) = &item.what {
                total += bytes.len().div_ceil(cluster_bytes);
            }
        }
    }
    total
}

fn boot_sector(s: &mut [u8], sectors_per_cluster: usize, fat_sectors: usize, total: usize) {
    s[0..3].copy_from_slice(&[0xeb, 0x3c, 0x90]);
    s[3..11].copy_from_slice(b"RPIVIRT ");
    s[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    s[13] = sectors_per_cluster as u8;
    s[14..16].copy_from_slice(&1u16.to_le_bytes());
    s[16] = FATS as u8;
    s[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes());
    if total < 0x1_0000 {
        s[19..21].copy_from_slice(&(total as u16).to_le_bytes());
    } else {
        s[32..36].copy_from_slice(&(total as u32).to_le_bytes());
    }
    s[21] = 0xf8;
    s[22..24].copy_from_slice(&(fat_sectors as u16).to_le_bytes());
    s[24..26].copy_from_slice(&32u16.to_le_bytes());
    s[26..28].copy_from_slice(&64u16.to_le_bytes());
    s[36] = 0x80;
    s[38] = 0x29;
    s[39..43].copy_from_slice(&0x5250_4656u32.to_le_bytes());
    s[43..54].copy_from_slice(LABEL);
    s[54..62].copy_from_slice(b"FAT16   ");
    s[510..512].copy_from_slice(&[0x55, 0xaa]);
}

fn label_entry() -> [u8; DIR_ENTRY] {
    let mut e = [0u8; DIR_ENTRY];
    e[..11].copy_from_slice(LABEL);
    e[11] = 0x08;
    stamp(&mut e);
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, bytes: &[u8]) -> Entry {
        Entry {
            name: name.to_string(),
            kind: Kind::File {
                len: bytes.len() as u64,
                source: Source::Bytes(bytes.to_vec()),
            },
        }
    }

    fn dir(name: &str, children: Vec<Entry>) -> Entry {
        Entry {
            name: name.to_string(),
            kind: Kind::Dir(children),
        }
    }

    /// Reads the volume back the way a driver does: the boot sector, the root
    /// region, then the FAT chains, long names and all.
    struct Volume<'a>(&'a [u8]);

    impl Volume<'_> {
        fn u16_at(&self, at: usize) -> usize {
            u16::from_le_bytes([self.0[at], self.0[at + 1]]) as usize
        }

        fn spc(&self) -> usize {
            self.0[13] as usize
        }

        fn fat_sectors(&self) -> usize {
            self.u16_at(22)
        }

        fn root_at(&self) -> usize {
            (1 + FATS * self.fat_sectors()) * SECTOR
        }

        fn data_at(&self) -> usize {
            self.root_at() + ROOT_SECTORS * SECTOR
        }

        fn total(&self) -> usize {
            match self.u16_at(19) {
                0 => u32::from_le_bytes(self.0[32..36].try_into().unwrap()) as usize,
                n => n,
            }
        }

        fn clusters(&self) -> usize {
            (self.total() - self.data_at() / SECTOR) / self.spc()
        }

        fn chain(&self, first: usize) -> Vec<u8> {
            let mut out = Vec::new();
            let mut cluster = first;
            while (2..0xfff8).contains(&cluster) {
                let at = self.data_at() + (cluster - 2) * self.spc() * SECTOR;
                out.extend_from_slice(&self.0[at..at + self.spc() * SECTOR]);
                cluster = self.u16_at(SECTOR + cluster * 2);
            }
            out
        }

        /// `(name, is_dir, first cluster, size)` of what a directory's bytes list.
        fn list(&self, bytes: &[u8]) -> Vec<(String, bool, usize, usize)> {
            let mut out = Vec::new();
            let mut long: Vec<u16> = Vec::new();
            for e in bytes.chunks(DIR_ENTRY) {
                if e[0] == 0 {
                    break;
                }
                if e[11] == 0x0f {
                    let mut units = Vec::new();
                    for range in [1..11, 14..26, 28..32] {
                        units.extend(e[range].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])));
                    }
                    units.retain(|&u| u != 0 && u != 0xffff);
                    units.append(&mut long);
                    long = units;
                    continue;
                }
                if e[11] == 0x08 {
                    continue;
                }
                let name = if long.is_empty() {
                    let base = String::from_utf8_lossy(&e[..8]).trim_end().to_lowercase();
                    let ext = String::from_utf8_lossy(&e[8..11]).trim_end().to_lowercase();
                    if ext.is_empty() {
                        base
                    } else {
                        format!("{base}.{ext}")
                    }
                } else {
                    String::from_utf16(&std::mem::take(&mut long)).unwrap()
                };
                let size = u32::from_le_bytes(e[28..32].try_into().unwrap()) as usize;
                out.push((
                    name,
                    e[11] & 0x10 != 0,
                    u16::from_le_bytes([e[26], e[27]]) as usize,
                    size,
                ));
            }
            out
        }

        fn read(&self, path: &str) -> Option<Vec<u8>> {
            let mut listing = self.list(&self.0[self.root_at()..self.data_at()]);
            let mut parts = path.split('/').peekable();
            while let Some(part) = parts.next() {
                let (_, is_dir, first, size) = listing.iter().find(|e| e.0 == part)?.clone();
                let bytes = self.chain(first);
                if parts.peek().is_none() {
                    return (!is_dir).then(|| bytes[..size].to_vec());
                }
                listing = self.list(&bytes);
            }
            None
        }
    }

    #[test]
    fn the_files_come_back_by_name_through_the_boot_sector_and_the_fat() {
        let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let image = fat16_image(vec![
            file("bcm2711-rpi-4-b.dtb", b"a device tree"),
            file("config.txt", b"arm_64bit=1\n"),
            file("empty.txt", b""),
            file("kernel8.img", &big),
            dir("overlays", vec![file("disable-bt.dtbo", b"overlay")]),
            file("start4.elf", b"elf"),
        ])
        .unwrap();
        let v = Volume(&image);
        assert_eq!(&image[54..62], b"FAT16   ");
        assert_eq!(image[510..512], [0x55, 0xaa]);
        assert!(
            (MIN_CLUSTERS..=MAX_CLUSTERS).contains(&v.clusters()),
            "{} clusters",
            v.clusters()
        );
        assert_eq!(image.len(), v.total() * SECTOR);
        assert_eq!(v.read("bcm2711-rpi-4-b.dtb").unwrap(), b"a device tree");
        assert_eq!(v.read("config.txt").unwrap(), b"arm_64bit=1\n");
        assert_eq!(v.read("empty.txt").unwrap(), b"");
        assert_eq!(v.read("kernel8.img").unwrap(), big);
        assert_eq!(v.read("overlays/disable-bt.dtbo").unwrap(), b"overlay");
        assert_eq!(v.read("start4.elf").unwrap(), b"elf");
        assert_eq!(v.read("nothing"), None);
    }

    #[test]
    fn the_image_is_about_as_big_as_its_files() {
        let image = fat16_image(vec![file("kernel8.img", &vec![7u8; 10 << 20])]).unwrap();
        assert!(
            image.len() >= 10 << 20 && image.len() < (10 << 20) + (1 << 20),
            "{} bytes",
            image.len()
        );
    }

    #[test]
    fn a_tiny_volume_still_has_enough_clusters_to_be_fat16() {
        let image = fat16_image(vec![file("a.txt", b"a")]).unwrap();
        assert!(Volume(&image).clusters() >= MIN_CLUSTERS);
    }

    #[test]
    fn a_big_volume_gets_bigger_clusters_to_stay_fat16() {
        let image = fat16_image(vec![file("big.bin", &vec![1u8; 70 << 20])]).unwrap();
        let v = Volume(&image);
        assert!(v.spc() > 2, "{} sectors a cluster", v.spc());
        assert!(v.clusters() <= MAX_CLUSTERS);
        assert_eq!(v.read("big.bin").unwrap().len(), 70 << 20);
    }

    #[test]
    fn a_file_that_is_a_path_is_refused() {
        let entry = Entry {
            name: "x".into(),
            kind: Kind::File {
                len: 1,
                source: Source::Path("/nowhere".into()),
            },
        };
        assert!(fat16_image(vec![entry]).is_err());
    }
}
