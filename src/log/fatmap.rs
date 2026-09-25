//! Which file a disk block belongs to, for the I/O log.
//!
//! The model only sees blocks move; the names come from the bench's own
//! reading of the medium — the partition table, then each FAT partition's
//! directory tree — so the firmware stays a black box. Partitions that are not
//! FAT are still named, just without files inside them.

pub type ReadBlock<'a> = dyn Fn(u64) -> Option<[u8; 512]> + 'a;

/// Named block ranges `[first, last]`, sorted and not overlapping.
#[derive(Debug, Default, Clone)]
pub struct FileMap {
    extents: Vec<(u64, u64, String)>,
}

impl FileMap {
    pub fn build(read: &ReadBlock) -> FileMap {
        let mut map = FileMap::default();
        let (parts, table_end) = partitions(read);
        if !parts.is_empty() {
            map.extents
                .push((0, table_end, "(partition table)".to_string()));
        }
        for (n, first, last) in parts {
            let part = format!("p{n}");
            let before = map.extents.len();
            fat_files(read, first, &part, &mut map.extents);
            if map.extents.len() == before {
                map.extents.push((first, last, part));
            }
        }
        map.extents.sort_by_key(|e| e.0);
        map
    }

    /// The names everything `[first, first + count)` touches, in block order.
    pub fn names(&self, first: u64, count: u64) -> Vec<&str> {
        let last = first + count.max(1) - 1;
        let start = self.extents.partition_point(|e| e.1 < first);
        let mut out: Vec<&str> = Vec::new();
        for (a, _, name) in &self.extents[start..] {
            if *a > last {
                break;
            }
            if out.last() != Some(&name.as_str()) {
                out.push(name);
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.extents.is_empty()
    }
}

fn le16(b: &[u8], at: usize) -> u64 {
    u64::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn le32(b: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]))
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn partitions(read: &ReadBlock) -> (Vec<(usize, u64, u64)>, u64) {
    let Some(mbr) = read(0) else {
        return (Vec::new(), 0);
    };
    if mbr[510..512] != [0x55, 0xAA] {
        return (Vec::new(), 0);
    }
    let entry = |i: usize| &mbr[446 + 16 * i..462 + 16 * i];
    if (0..4).any(|i| entry(i)[4] == 0xEE) {
        return gpt_partitions(read);
    }
    let parts = (0..4)
        .filter_map(|i| {
            let e = entry(i);
            let (start, size) = (le32(e, 8), le32(e, 12));
            // Not `then_some`: it would evaluate `start + size - 1` for an
            // empty slot, where that underflows.
            if e[4] != 0 && size != 0 {
                Some((i + 1, start, start + size - 1))
            } else {
                None
            }
        })
        .collect();
    (parts, 0)
}

fn gpt_partitions(read: &ReadBlock) -> (Vec<(usize, u64, u64)>, u64) {
    let Some(hdr) = read(1) else {
        return (Vec::new(), 0);
    };
    if &hdr[..8] != b"EFI PART" {
        return (Vec::new(), 0);
    }
    let (table, count, size) = (le64(&hdr, 72), le32(&hdr, 80), le32(&hdr, 84) as usize);
    if !(128..=512).contains(&size) || count > 1024 {
        return (Vec::new(), 0);
    }
    let table_end = table + (count * size as u64).div_ceil(512).max(1) - 1;
    let mut out = Vec::new();
    for i in 0..count as usize {
        let at = i * size;
        let Some(block) = read(table + (at / 512) as u64) else {
            break;
        };
        let e = &block[at % 512..at % 512 + size.min(512 - at % 512)];
        if e.len() < 48 || e[..16].iter().all(|&b| b == 0) {
            continue;
        }
        out.push((i + 1, le64(e, 32), le64(e, 40)));
    }
    (out, table_end)
}

struct Fat {
    first: u64,
    spc: u64,
    fat: u64,
    root: Option<(u64, u64)>,
    root_cluster: u64,
    data: u64,
    clusters: u64,
    fat32: bool,
}

impl Fat {
    fn probe(read: &ReadBlock, first: u64) -> Option<Fat> {
        let b = read(first)?;
        if b[510..512] != [0x55, 0xAA] || le16(&b, 11) != 512 {
            return None;
        }
        let spc = u64::from(b[13]);
        let reserved = le16(&b, 14);
        let nfats = u64::from(b[16]);
        let root_entries = le16(&b, 17);
        let total = match le16(&b, 19) {
            0 => le32(&b, 32),
            n => n,
        };
        let fat32 = le16(&b, 22) == 0;
        let fat_len = if fat32 { le32(&b, 36) } else { le16(&b, 22) };
        if spc == 0 || !spc.is_power_of_two() || nfats == 0 || fat_len == 0 || reserved == 0 {
            return None;
        }
        let root_len = (root_entries * 32).div_ceil(512);
        let data_rel = reserved + nfats * fat_len + root_len;
        let clusters = total.checked_sub(data_rel)? / spc;
        // FAT12 volumes are too small to boot a Pi from.
        if clusters < 4085 {
            return None;
        }
        Some(Fat {
            first,
            spc,
            fat: first + reserved,
            root: (!fat32).then_some((first + reserved + nfats * fat_len, root_len)),
            root_cluster: le32(&b, 44),
            data: first + data_rel,
            clusters,
            fat32,
        })
    }

    fn next(&self, read: &ReadBlock, cluster: u64) -> Option<u64> {
        let width = if self.fat32 { 4 } else { 2 };
        let at = cluster * width;
        let b = read(self.fat + at / 512)?;
        let off = (at % 512) as usize;
        let next = if self.fat32 {
            le32(&b, off) & 0x0FFF_FFFF
        } else {
            le16(&b, off)
        };
        (2..self.clusters + 2).contains(&next).then_some(next)
    }

    fn chain(&self, read: &ReadBlock, first: u64) -> Vec<(u64, u64)> {
        let mut out: Vec<(u64, u64)> = Vec::new();
        let mut c = first;
        for _ in 0..self.clusters {
            if !(2..self.clusters + 2).contains(&c) {
                break;
            }
            let a = self.data + (c - 2) * self.spc;
            let b = a + self.spc - 1;
            match out.last_mut() {
                Some(last) if last.1 + 1 == a => last.1 = b,
                _ => out.push((a, b)),
            }
            match self.next(read, c) {
                Some(n) => c = n,
                None => break,
            }
        }
        out
    }

    fn blocks_of(&self, read: &ReadBlock, extents: &[(u64, u64)]) -> Vec<[u8; 512]> {
        extents
            .iter()
            .flat_map(|&(a, b)| a..=b)
            .map_while(read)
            .collect()
    }
}

/// Every file and directory of the FAT file system at `first`, plus its own
/// metadata, as named extents.
fn fat_files(read: &ReadBlock, first: u64, part: &str, out: &mut Vec<(u64, u64, String)>) {
    let Some(fat) = Fat::probe(read, first) else {
        return;
    };
    out.push((fat.first, fat.fat - 1, format!("{part}:(boot sector)")));
    let fats_end = fat.root.map_or(fat.data, |(a, _)| a) - 1;
    out.push((fat.fat, fats_end, format!("{part}:(FAT)")));
    let root = match fat.root {
        Some((a, len)) => vec![(a, a + len - 1)],
        None => fat.chain(read, fat.root_cluster),
    };
    let mut stack = vec![(String::from("/"), root, 0usize)];
    let mut seen = 0usize;
    while let Some((dir, extents, depth)) = stack.pop() {
        for &(a, b) in &extents {
            out.push((a, b, format!("{part}:{dir}")));
        }
        let mut lfn: Vec<(u8, [u16; 13])> = Vec::new();
        for block in fat.blocks_of(read, &extents) {
            for e in block.as_chunks::<32>().0 {
                seen += 1;
                if e[0] == 0 || seen > 100_000 {
                    break;
                }
                if e[0] == 0xE5 {
                    lfn.clear();
                    continue;
                }
                if e[11] == 0x0F {
                    let mut chars = [0u16; 13];
                    for (i, at) in (1..11)
                        .step_by(2)
                        .chain((14..26).step_by(2))
                        .chain((28..32).step_by(2))
                        .enumerate()
                    {
                        chars[i] = u16::from_le_bytes([e[at], e[at + 1]]);
                    }
                    lfn.push((e[0] & 0x1F, chars));
                    continue;
                }
                let long = std::mem::take(&mut lfn);
                if e[11] & 0x08 != 0 {
                    continue; // the volume label
                }
                let name = if long.is_empty() {
                    short_name(e)
                } else {
                    let mut parts = long;
                    parts.sort_by_key(|p| p.0);
                    let units: Vec<u16> = parts
                        .iter()
                        .flat_map(|p| p.1)
                        .take_while(|&u| u != 0 && u != 0xFFFF)
                        .collect();
                    String::from_utf16_lossy(&units)
                };
                if name == "." || name == ".." {
                    continue;
                }
                let cluster = (le16(e, 20) << 16) | le16(e, 26);
                let path = format!("{dir}{name}");
                if e[11] & 0x10 != 0 {
                    if depth < 16 && cluster >= 2 {
                        stack.push((format!("{path}/"), fat.chain(read, cluster), depth + 1));
                    }
                } else if cluster >= 2 && le32(e, 28) > 0 {
                    for (a, b) in fat.chain(read, cluster) {
                        out.push((a, b, format!("{part}:{path}")));
                    }
                }
            }
        }
    }
}

fn short_name(e: &[u8]) -> String {
    let trim = |b: &[u8]| String::from_utf8_lossy(b).trim_end().to_string();
    let (base, ext) = (trim(&e[..8]), trim(&e[8..11]));
    let (base, ext) = (base.to_lowercase(), ext.to_lowercase());
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A FAT16 volume with a long-named file spanning two clusters.
    fn image() -> Vec<[u8; 512]> {
        let part = 8u64;
        let (reserved, fat_len, root_entries, spc) = (1u64, 20u64, 32u64, 4u64);
        let clusters = 4200u64;
        let total = reserved + fat_len + 2 + clusters * spc;
        let mut img = vec![[0u8; 512]; (part + total) as usize];
        let mbr = &mut img[0];
        mbr[446 + 4] = 0x06;
        mbr[446 + 8..446 + 12].copy_from_slice(&(part as u32).to_le_bytes());
        mbr[446 + 12..446 + 16].copy_from_slice(&(total as u32).to_le_bytes());
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        let bs = &mut img[part as usize];
        bs[11..13].copy_from_slice(&512u16.to_le_bytes());
        bs[13] = spc as u8;
        bs[14..16].copy_from_slice(&(reserved as u16).to_le_bytes());
        bs[16] = 1;
        bs[17..19].copy_from_slice(&(root_entries as u16).to_le_bytes());
        bs[32..36].copy_from_slice(&(total as u32).to_le_bytes());
        bs[22..24].copy_from_slice(&(fat_len as u16).to_le_bytes());
        bs[510] = 0x55;
        bs[511] = 0xAA;
        let fat = (part + reserved) as usize;
        let mut set = |c: usize, v: u16| {
            img[fat + c * 2 / 512][c * 2 % 512..c * 2 % 512 + 2].copy_from_slice(&v.to_le_bytes())
        };
        set(2, 0xFFFF);
        set(3, 5);
        set(5, 0xFFFF);
        let root = (part + reserved + fat_len) as usize;
        let data = root as u64 + 2;
        let entry = |name: &[u8; 11], attr: u8, cluster: u16, size: u32| {
            let mut e = [0u8; 32];
            e[..11].copy_from_slice(name);
            e[11] = attr;
            e[26..28].copy_from_slice(&cluster.to_le_bytes());
            e[28..32].copy_from_slice(&size.to_le_bytes());
            e
        };
        img[root][..32].copy_from_slice(&entry(b"BOOT       ", 0x10, 2, 0));
        let dir = data as usize; // cluster 2
        let mut lfn = [0xFFu8; 32];
        lfn[0] = 0x41;
        lfn[11] = 0x0F;
        let units: Vec<u16> = "start4.elf".encode_utf16().chain([0]).collect();
        let slots: Vec<usize> = (1..11)
            .step_by(2)
            .chain((14..26).step_by(2))
            .chain((28..32).step_by(2))
            .collect();
        for (u, at) in units.iter().zip(slots) {
            lfn[at..at + 2].copy_from_slice(&u.to_le_bytes());
        }
        img[dir][..32].copy_from_slice(&lfn);
        img[dir][32..64].copy_from_slice(&entry(b"START4  ELF", 0x20, 3, 3000));
        img
    }

    #[test]
    fn files_are_named_by_the_blocks_they_occupy() {
        let img = image();
        let read = |lba: u64| img.get(lba as usize).copied();
        let map = FileMap::build(&read);
        let data = 8 + 1 + 20 + 2;
        assert_eq!(map.names(data + 4, 4), ["p1:/boot/start4.elf"]);
        assert_eq!(map.names(data + 12, 1), ["p1:/boot/start4.elf"]);
        assert!(map.names(data + 8, 4).is_empty());
        assert_eq!(map.names(data, 1), ["p1:/boot/"]);
        assert_eq!(map.names(9, 1), ["p1:(FAT)"]);
        assert_eq!(map.names(8, 23), ["p1:(boot sector)", "p1:(FAT)", "p1:/"]);
    }

    #[test]
    fn a_partition_without_fat_is_named_whole() {
        let mut img = vec![[0u8; 512]; 64];
        img[0][446 + 4] = 0x83;
        img[0][446 + 8..446 + 12].copy_from_slice(&16u32.to_le_bytes());
        img[0][446 + 12..446 + 16].copy_from_slice(&32u32.to_le_bytes());
        img[0][510] = 0x55;
        img[0][511] = 0xAA;
        let read = |lba: u64| img.get(lba as usize).copied();
        let map = FileMap::build(&read);
        assert_eq!(map.names(20, 4), ["p1"]);
        assert!(map.names(2, 1).is_empty());
    }
}
