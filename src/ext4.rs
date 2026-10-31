//! A tree of files as an ext4 volume, built the way [`crate::fat`] builds a FAT
//! one: only the metadata is made here, and every file's blocks are mapped onto
//! its source, so a root filesystem of a few hundred megabytes costs the
//! blocks the guest reads.
//!
//! The volume is the smallest ext4 Linux mounts: 4 KiB blocks, 256-byte
//! inodes, extents, no journal, no checksums, no backup superblocks. With
//! `flex_bg` every group's bitmaps and inode table sit at the front, followed
//! by the directories and the extent leaves, so everything built in memory is
//! one run of blocks and the files that follow it are contiguous.

use anyhow::{bail, Result};
use sha1::{Digest, Sha1};

use crate::fat::{Entry, Extent, Kind, Source};
use crate::layout::Volume;

const BLOCK: usize = 4096;
const SECTORS_PER_BLOCK: u64 = (BLOCK / crate::periph::disk::BLOCK_SIZE) as u64;
const BLOCKS_PER_GROUP: u64 = 8 * BLOCK as u64;
const INODE_SIZE: usize = 256;
const INODES_PER_BLOCK: u64 = (BLOCK / INODE_SIZE) as u64;
const FIRST_INO: u32 = 11;
const ROOT_INO: u32 = 2;
const MAX_EXTENT: u64 = 32768;
const INDEX_EXTENTS: usize = 4;
const LEAF_EXTENTS: usize = (BLOCK - 12) / 12;
const LOST_FOUND_BLOCKS: u64 = 4;
/// Every timestamp, so the same tree always builds the same volume.
const EPOCH: u32 = 1_704_067_200;
/// Free space left in a volume that has no size of its own: a tenth of what
/// is in it, and 8 MiB at the least.
const MIN_SLACK_BLOCKS: u64 = 2048;

const COMPAT_SPARSE_SUPER2: u32 = 0x200;
const INCOMPAT_FILETYPE: u32 = 0x2;
const INCOMPAT_EXTENTS: u32 = 0x40;
const INCOMPAT_FLEX_BG: u32 = 0x200;
const RO_SPARSE_SUPER: u32 = 0x1;
const RO_LARGE_FILE: u32 = 0x2;
const RO_DIR_NLINK: u32 = 0x20;
const RO_EXTRA_ISIZE: u32 = 0x40;

const S_IFREG: u16 = 0o100000;
const S_IFDIR: u16 = 0o040000;
const EXTENTS_FL: u32 = 0x80000;

enum What {
    Dir {
        children: Vec<usize>,
        bytes: Vec<u8>,
    },
    File {
        source: Source,
        len: u64,
    },
}

struct Node {
    name: String,
    parent: usize,
    ino: u32,
    mode: u16,
    what: What,
    blocks: u64,
    first: u64,
    leaf: Option<u64>,
}

impl Node {
    fn is_dir(&self) -> bool {
        matches!(self.what, What::Dir { .. })
    }

    fn extents(&self) -> usize {
        if self.is_dir() {
            1
        } else {
            self.blocks.div_ceil(MAX_EXTENT) as usize
        }
    }
}

/// An ext4 volume of the entries, at least `min_sectors` long.
pub fn volume_from_entries(entries: Vec<Entry>, min_sectors: u64) -> Result<Volume> {
    let mut nodes = vec![Node {
        name: String::new(),
        parent: 0,
        ino: ROOT_INO,
        mode: 0o755,
        what: What::Dir {
            children: Vec::new(),
            bytes: Vec::new(),
        },
        blocks: 0,
        first: 0,
        leaf: None,
    }];
    let lost_found = Entry {
        name: "lost+found".into(),
        kind: Kind::Dir(Vec::new()),
    };
    if entries.iter().any(|e| e.name == lost_found.name) {
        bail!("`lost+found` is made by the volume itself, not taken from the files");
    }
    let mut next_ino = FIRST_INO + 1;
    flatten(vec![lost_found], 0, &mut nodes, &mut next_ino, true);
    flatten(entries, 0, &mut nodes, &mut next_ino, false);

    for i in 0..nodes.len() {
        if nodes[i].is_dir() {
            let bytes = render_dir(&nodes, i)?;
            let min = if nodes[i].ino == FIRST_INO {
                LOST_FOUND_BLOCKS * BLOCK as u64
            } else {
                0
            };
            let bytes = pad_dir(bytes, min as usize);
            nodes[i].blocks = (bytes.len() / BLOCK) as u64;
            if let What::Dir { bytes: slot, .. } = &mut nodes[i].what {
                *slot = bytes;
            }
        }
    }
    for node in &mut nodes {
        if let What::File { len, .. } = &node.what {
            node.blocks = len.div_ceil(BLOCK as u64);
        }
        if node.extents() > INDEX_EXTENTS {
            if node.extents() > LEAF_EXTENTS {
                bail!("{}: too big for one extent leaf", node.name);
            }
            node.leaf = Some(0);
        }
    }

    let used_inodes = (FIRST_INO as u64 - 1) + (nodes.len() as u64 - 1);
    let want_inodes = used_inodes + used_inodes / 4 + 256;
    let data_blocks: u64 = nodes.iter().filter(|n| !n.is_dir()).map(|n| n.blocks).sum();
    let tree_blocks: u64 = nodes
        .iter()
        .map(|n| (if n.is_dir() { n.blocks } else { 0 }) + n.leaf.map_or(0, |_| 1))
        .sum();
    let min_blocks = min_sectors.div_ceil(SECTORS_PER_BLOCK);

    let mut groups = want_inodes.div_ceil(BLOCK as u64 * 8).max(1);
    let (ipg, total) = loop {
        let ipg = want_inodes
            .div_ceil(groups)
            .next_multiple_of(INODES_PER_BLOCK)
            .max(INODES_PER_BLOCK);
        let fixed = 1 + gdt_blocks(groups) + 2 * groups + groups * ipg / INODES_PER_BLOCK;
        let used = fixed + tree_blocks + data_blocks;
        let mut total = (used + (used / 10).max(MIN_SLACK_BLOCKS)).max(min_blocks);
        let tail = total % BLOCKS_PER_GROUP;
        if tail != 0 && tail < 256 {
            total += 256 - tail;
        }
        let needed = total.div_ceil(BLOCKS_PER_GROUP);
        if needed <= groups {
            break (ipg, total);
        }
        groups = needed;
    };
    if total > u32::MAX as u64 {
        bail!("{total} blocks is more than an ext4 volume without 64bit can address");
    }

    let gdt = gdt_blocks(groups);
    let block_bitmaps = 1 + gdt;
    let inode_bitmaps = block_bitmaps + groups;
    let inode_tables = inode_bitmaps + groups;
    let fixed = inode_tables + groups * ipg / INODES_PER_BLOCK;

    let mut next = fixed;
    for node in nodes.iter_mut().filter(|n| n.is_dir()) {
        node.first = next;
        next += node.blocks;
    }
    for node in nodes.iter_mut().filter(|n| n.leaf.is_some()) {
        node.leaf = Some(next);
        next += 1;
    }
    let mut extents = Vec::new();
    for node in nodes.iter_mut().filter(|n| !n.is_dir() && n.blocks > 0) {
        node.first = next;
        next += node.blocks;
        let What::File { source, len } = &node.what else {
            unreachable!()
        };
        extents.push(Extent {
            lba: node.first * SECTORS_PER_BLOCK,
            blocks: node.blocks * SECTORS_PER_BLOCK,
            source: source.clone(),
            len: *len,
        });
    }
    let used_blocks = next;

    let mut meta = vec![0u8; (fixed + tree_blocks) as usize * BLOCK];
    let put = |_: &mut Vec<u8>, block: u64| -> usize { block as usize * BLOCK };

    let manifest = manifest(&nodes);
    let uuid = {
        let mut u: [u8; 16] = manifest[..16].try_into().unwrap();
        u[6] = u[6] & 0x0f | 0x50;
        u[8] = u[8] & 0x3f | 0x80;
        u
    };

    for node in &nodes {
        let at = put(&mut meta, inode_tables) + (node.ino as usize - 1) * INODE_SIZE;
        write_inode(&mut meta[at..at + INODE_SIZE], node, &nodes);
        if let What::Dir { bytes, .. } = &node.what {
            let at = put(&mut meta, node.first);
            meta[at..at + bytes.len()].copy_from_slice(bytes);
        }
        if let Some(leaf) = node.leaf {
            let at = put(&mut meta, leaf);
            write_extents(&mut meta[at..at + BLOCK], node, LEAF_EXTENTS, 0);
        }
    }
    let dirs_in = |group: u64| {
        nodes
            .iter()
            .filter(|n| n.is_dir() && (n.ino as u64 - 1) / ipg == group)
            .count() as u64
    };
    let mut free_blocks_total = 0u64;
    let mut free_inodes_total = 0u64;
    for g in 0..groups {
        let first = g * BLOCKS_PER_GROUP;
        let last = ((g + 1) * BLOCKS_PER_GROUP).min(total);
        let used_here = used_blocks.clamp(first, last) - first;
        let free_blocks = (last - first) - used_here;
        let ino_first = g * ipg;
        let used_inodes_here = used_inodes.clamp(ino_first, ino_first + ipg) - ino_first;
        let free_inodes = ipg - used_inodes_here;
        free_blocks_total += free_blocks;
        free_inodes_total += free_inodes;

        let bitmap = block_bitmaps as usize * BLOCK + g as usize * BLOCK;
        set_bits(&mut meta[bitmap..bitmap + BLOCK], 0, used_here);
        set_bits(
            &mut meta[bitmap..bitmap + BLOCK],
            last - first,
            BLOCKS_PER_GROUP - (last - first),
        );
        let bitmap = inode_bitmaps as usize * BLOCK + g as usize * BLOCK;
        set_bits(&mut meta[bitmap..bitmap + BLOCK], 0, used_inodes_here);
        set_bits(
            &mut meta[bitmap..bitmap + BLOCK],
            ipg,
            BLOCKS_PER_GROUP - ipg,
        );

        let d = BLOCK + g as usize * 32;
        meta[d..d + 4].copy_from_slice(&((block_bitmaps + g) as u32).to_le_bytes());
        meta[d + 4..d + 8].copy_from_slice(&((inode_bitmaps + g) as u32).to_le_bytes());
        let table = inode_tables + g * ipg / INODES_PER_BLOCK;
        meta[d + 8..d + 12].copy_from_slice(&(table as u32).to_le_bytes());
        meta[d + 12..d + 14].copy_from_slice(&(free_blocks as u16).to_le_bytes());
        meta[d + 14..d + 16].copy_from_slice(&(free_inodes as u16).to_le_bytes());
        meta[d + 16..d + 18].copy_from_slice(&(dirs_in(g) as u16).to_le_bytes());
    }

    superblock(
        &mut meta[1024..2048],
        Counts {
            inodes: groups * ipg,
            blocks: total,
            free_blocks: free_blocks_total,
            free_inodes: free_inodes_total,
            ipg,
        },
        &uuid,
        &manifest,
    );

    Ok(Volume {
        meta,
        extents,
        sectors: total * SECTORS_PER_BLOCK,
    })
}

fn gdt_blocks(groups: u64) -> u64 {
    (groups * 32).div_ceil(BLOCK as u64)
}

fn flatten(
    entries: Vec<Entry>,
    parent: usize,
    nodes: &mut Vec<Node>,
    next_ino: &mut u32,
    lost_found: bool,
) {
    for entry in entries {
        let me = nodes.len();
        let ino = if lost_found {
            FIRST_INO
        } else {
            let ino = *next_ino;
            *next_ino += 1;
            ino
        };
        let (what, mode, children) = match entry.kind {
            Kind::Dir(children) => (
                What::Dir {
                    children: Vec::new(),
                    bytes: Vec::new(),
                },
                if lost_found { 0o700 } else { 0o755 },
                Some(children),
            ),
            Kind::File { source, len } => {
                let mode = host_mode(&source);
                (What::File { source, len }, mode, None)
            }
        };
        nodes.push(Node {
            name: entry.name,
            parent,
            ino,
            mode,
            what,
            blocks: 0,
            first: 0,
            leaf: None,
        });
        if let What::Dir { children, .. } = &mut nodes[parent].what {
            children.push(me);
        }
        if let Some(children) = children {
            flatten(children, me, nodes, next_ino, false);
        }
    }
}

#[cfg(unix)]
fn host_mode(source: &Source) -> u16 {
    use std::os::unix::fs::PermissionsExt;
    match source {
        Source::Path(path) => std::fs::metadata(path)
            .map(|m| (m.permissions().mode() & 0o777) as u16)
            .ok()
            .filter(|mode| *mode != 0)
            .unwrap_or(0o644),
        _ => 0o644,
    }
}

#[cfg(not(unix))]
fn host_mode(_: &Source) -> u16 {
    0o644
}

/// The directory's blocks: `.` and `..`, then the children, each block ending
/// in an entry that runs to the end of it.
fn render_dir(nodes: &[Node], dir: usize) -> Result<Vec<u8>> {
    let What::Dir { children, .. } = &nodes[dir].what else {
        unreachable!()
    };
    let mut list: Vec<(u32, u8, &str)> = vec![
        (nodes[dir].ino, 2, "."),
        (nodes[nodes[dir].parent].ino, 2, ".."),
    ];
    for &c in children {
        let file_type = if nodes[c].is_dir() { 2 } else { 1 };
        list.push((nodes[c].ino, file_type, &nodes[c].name));
    }
    let mut out = Vec::new();
    let mut block = vec![0u8; BLOCK];
    let (mut at, mut last) = (0usize, 0usize);
    for (ino, file_type, name) in list {
        if name.len() > 255 {
            bail!("{name}: a name over 255 bytes");
        }
        let need = (8 + name.len()).next_multiple_of(4);
        if at + need > BLOCK {
            let rest = (BLOCK - last) as u16;
            block[last + 4..last + 6].copy_from_slice(&rest.to_le_bytes());
            out.extend_from_slice(&block);
            block = vec![0u8; BLOCK];
            at = 0;
        }
        block[at..at + 4].copy_from_slice(&ino.to_le_bytes());
        block[at + 4..at + 6].copy_from_slice(&(need as u16).to_le_bytes());
        block[at + 6] = name.len() as u8;
        block[at + 7] = file_type;
        block[at + 8..at + 8 + name.len()].copy_from_slice(name.as_bytes());
        last = at;
        at += need;
    }
    let rest = (BLOCK - last) as u16;
    block[last + 4..last + 6].copy_from_slice(&rest.to_le_bytes());
    out.extend_from_slice(&block);
    Ok(out)
}

/// More blocks of no entries, for a directory that has room reserved.
fn pad_dir(mut bytes: Vec<u8>, min: usize) -> Vec<u8> {
    while bytes.len() < min {
        let mut block = vec![0u8; BLOCK];
        block[4..6].copy_from_slice(&(BLOCK as u16).to_le_bytes());
        bytes.extend_from_slice(&block);
    }
    bytes
}

/// The extent header and as many extents as `max` holds: those of a file in
/// the inode (`max` 4) or in its leaf block.
fn write_extents(out: &mut [u8], node: &Node, max: usize, depth: u16) {
    let count = node.extents();
    out[0..2].copy_from_slice(&0xf30au16.to_le_bytes());
    out[2..4].copy_from_slice(&(count as u16).to_le_bytes());
    out[4..6].copy_from_slice(&(max as u16).to_le_bytes());
    out[6..8].copy_from_slice(&depth.to_le_bytes());
    if node.blocks == 0 {
        out[2..4].copy_from_slice(&0u16.to_le_bytes());
        return;
    }
    let mut logical = 0u64;
    for i in 0..count {
        let len = (node.blocks - logical).min(MAX_EXTENT);
        let at = 12 + i * 12;
        let start = node.first + logical;
        out[at..at + 4].copy_from_slice(&(logical as u32).to_le_bytes());
        out[at + 4..at + 6].copy_from_slice(&(len as u16).to_le_bytes());
        out[at + 6..at + 8].copy_from_slice(&((start >> 32) as u16).to_le_bytes());
        out[at + 8..at + 12].copy_from_slice(&(start as u32).to_le_bytes());
        logical += len;
    }
}

fn write_inode(out: &mut [u8], node: &Node, nodes: &[Node]) {
    let (kind, size, links) = match &node.what {
        What::Dir { children, .. } => {
            let subdirs = children.iter().filter(|&&c| nodes[c].is_dir()).count();
            (S_IFDIR, node.blocks * BLOCK as u64, 2 + subdirs as u16)
        }
        What::File { len, .. } => (S_IFREG, *len, 1),
    };
    out[0..2].copy_from_slice(&(kind | node.mode).to_le_bytes());
    out[4..8].copy_from_slice(&(size as u32).to_le_bytes());
    for at in [8, 12, 16] {
        out[at..at + 4].copy_from_slice(&EPOCH.to_le_bytes());
    }
    out[26..28].copy_from_slice(&links.to_le_bytes());
    let sectors = (node.blocks + node.leaf.map_or(0, |_| 1)) * SECTORS_PER_BLOCK;
    out[28..32].copy_from_slice(&(sectors as u32).to_le_bytes());
    out[32..36].copy_from_slice(&EXTENTS_FL.to_le_bytes());
    match node.leaf {
        None => write_extents(&mut out[40..100], node, INDEX_EXTENTS, 0),
        Some(leaf) => {
            let i_block = &mut out[40..100];
            i_block[0..2].copy_from_slice(&0xf30au16.to_le_bytes());
            i_block[2..4].copy_from_slice(&1u16.to_le_bytes());
            i_block[4..6].copy_from_slice(&(INDEX_EXTENTS as u16).to_le_bytes());
            i_block[6..8].copy_from_slice(&1u16.to_le_bytes());
            i_block[16..20].copy_from_slice(&(leaf as u32).to_le_bytes());
            i_block[20..22].copy_from_slice(&((leaf >> 32) as u16).to_le_bytes());
        }
    }
    out[108..112].copy_from_slice(&((size >> 32) as u32).to_le_bytes());
    out[128..130].copy_from_slice(&32u16.to_le_bytes());
    out[144..148].copy_from_slice(&EPOCH.to_le_bytes());
}

/// Bits `from..from + count` of a bitmap, set.
fn set_bits(bitmap: &mut [u8], from: u64, count: u64) {
    for bit in from..from + count {
        bitmap[bit as usize / 8] |= 1 << (bit % 8);
    }
}

struct Counts {
    inodes: u64,
    blocks: u64,
    free_blocks: u64,
    free_inodes: u64,
    ipg: u64,
}

fn superblock(sb: &mut [u8], c: Counts, uuid: &[u8; 16], seed: &[u8]) {
    let le32 = |sb: &mut [u8], at: usize, v: u32| sb[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let le16 = |sb: &mut [u8], at: usize, v: u16| sb[at..at + 2].copy_from_slice(&v.to_le_bytes());
    le32(sb, 0, c.inodes as u32);
    le32(sb, 4, c.blocks as u32);
    le32(sb, 12, c.free_blocks as u32);
    le32(sb, 16, c.free_inodes as u32);
    le32(sb, 20, 0);
    le32(sb, 24, 2);
    le32(sb, 28, 2);
    le32(sb, 32, BLOCKS_PER_GROUP as u32);
    le32(sb, 36, BLOCKS_PER_GROUP as u32);
    le32(sb, 40, c.ipg as u32);
    le32(sb, 48, EPOCH);
    le16(sb, 54, 0xffff);
    le16(sb, 56, 0xef53);
    le16(sb, 58, 1);
    le16(sb, 60, 1);
    le32(sb, 64, EPOCH);
    le32(sb, 76, 1);
    le32(sb, 84, FIRST_INO);
    le16(sb, 88, INODE_SIZE as u16);
    le32(sb, 92, COMPAT_SPARSE_SUPER2);
    le32(
        sb,
        96,
        INCOMPAT_FILETYPE | INCOMPAT_EXTENTS | INCOMPAT_FLEX_BG,
    );
    le32(
        sb,
        100,
        RO_SPARSE_SUPER | RO_LARGE_FILE | RO_DIR_NLINK | RO_EXTRA_ISIZE,
    );
    sb[104..120].copy_from_slice(uuid);
    for (i, word) in seed[4..20].chunks(4).enumerate() {
        sb[236 + i * 4..240 + i * 4].copy_from_slice(word);
    }
    sb[248] = 1;
    le32(sb, 264, EPOCH);
    le16(sb, 348, 32);
    le16(sb, 350, 32);
    le32(sb, 352, 1);
}

/// What the tree is, as one digest: the volume's UUID and hash seed come from
/// it, so two volumes of different trees differ and one tree always builds
/// the same.
fn manifest(nodes: &[Node]) -> Vec<u8> {
    let mut sha = Sha1::new();
    for node in nodes {
        sha.update(node.ino.to_le_bytes());
        sha.update(node.name.as_bytes());
        sha.update(node.blocks.to_le_bytes());
        if let What::File { len, .. } = &node.what {
            sha.update(len.to_le_bytes());
        }
    }
    let mut out = sha.finalize().to_vec();
    let again = Sha1::digest(&out);
    out.extend_from_slice(&again);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, bytes: &[u8]) -> Entry {
        Entry {
            name: name.into(),
            kind: Kind::File {
                source: Source::Bytes(bytes.to_vec()),
                len: bytes.len() as u64,
            },
        }
    }

    #[test]
    fn a_small_tree_has_its_superblock_and_files_mapped_after_the_metadata() {
        let tree = vec![
            file("a.txt", b"hello"),
            Entry {
                name: "etc".into(),
                kind: Kind::Dir(vec![file("hostname", b"pimu\n")]),
            },
        ];
        let v = volume_from_entries(tree, 0).unwrap();
        assert_eq!(&v.meta[1024 + 56..1024 + 58], &0xef53u16.to_le_bytes());
        assert_eq!(v.extents.len(), 2);
        let meta_sectors = (v.meta.len() / 512) as u64;
        assert!(v.extents.iter().all(|e| e.lba >= meta_sectors));
        assert!(v.extents[0].lba < v.extents[1].lba);
        assert!(v.sectors >= v.extents[1].lba + v.extents[1].blocks);
    }

    #[test]
    fn the_same_tree_builds_the_same_volume() {
        let build = || volume_from_entries(vec![file("a", b"x")], 0).unwrap().meta;
        assert_eq!(build(), build());
    }

    #[test]
    fn a_volume_is_as_long_as_asked() {
        let v = volume_from_entries(Vec::new(), 100 * 1024 * 2).unwrap();
        assert!(v.sectors >= 100 * 1024 * 2);
        assert_eq!(v.sectors % SECTORS_PER_BLOCK, 0);
    }

    #[test]
    fn lost_and_found_is_the_volumes_own() {
        let tree = vec![Entry {
            name: "lost+found".into(),
            kind: Kind::Dir(Vec::new()),
        }];
        assert!(volume_from_entries(tree, 0).is_err());
    }
}
