//! How the firmware blobs reach a machine with no filesystem.
//!
//! The hosted frontend opens `firmware/pieeprom.bin` and `firmware/sd.img`.
//! Bare-metal there is nothing to open, and `raspi4b` is stingy about what it
//! will take: `-pflash` is refused outright, a second `-drive if=sd` is
//! refused, and every virtio transport is absent. What it does take is
//! `-initrd`, which QEMU copies into RAM and reports in the device tree as
//! `/chosen/linux,initrd-start` and `linux,initrd-end`. That is one blob, and
//! we need at least two, so the one blob is a container.
//!
//! ```text
//!  0  u8[4]  "RVFB"
//!  4  u32    version, 1
//!  8  u32    entry count
//! 12  u32    reserved, 0
//! 16  entry[count]:
//!       +0   u8[16]  name, NUL-padded (e.g. "pieeprom.bin")
//!      +16   u32     offset from the start of the bundle
//!      +20   u32     length
//!     ...    the blob bodies, in entry order
//! ```
//!
//! Little-endian, because the reader is. Built by
//! `scripts/make-blob-bundle.sh`.
//!
//! Blobs are read in place: [`Blob::bytes`] borrows straight out of the initrd
//! where QEMU put it, and nothing here copies. That matters for the 256 MiB SD
//! image, which is bigger than the slack in the heap window — see `heap.rs`.
//!
//! # The seam this is standing in for
//!
//! The SD image belongs behind QEMU's own SD controller (`-drive if=sd`),
//! fetched a sector at a time through `block::BlockDevice`, which is what that
//! trait was added for and what makes the EEPROM's self-update persist to the
//! file. Until there is an SDHCI driver in this image, `-initrd` carries the
//! image instead and [`BundleBlocks`] serves sectors out of it — the same
//! trait, a different backend, so the swap is local to this file.

use core::str;

const MAGIC: &[u8; 4] = b"RVFB";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 16;
const ENTRY_LEN: usize = 24;
const NAME_LEN: usize = 16;

/// One blob inside a bundle, borrowed in place.
pub struct Blob<'a> {
    pub name: &'a str,
    bytes: &'a [u8],
}

impl<'a> Blob<'a> {
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }
}

/// A parsed `-initrd` bundle.
pub struct Bundle<'a> {
    raw: &'a [u8],
    count: usize,
}

/// Why a bundle could not be used. Deliberately an enum rather than a string:
/// this is reported by a fault path that must not allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleError {
    /// Shorter than the fixed header.
    TooShort,
    /// The first four bytes are not `RVFB` — most likely `-initrd` was pointed
    /// at a raw blob rather than at a bundle.
    BadMagic,
    /// A version this image does not know how to read.
    BadVersion(u32),
    /// An entry's offset/length runs off the end of the blob.
    Truncated,
}

impl BundleError {
    pub fn as_str(&self) -> &'static str {
        match self {
            BundleError::TooShort => "shorter than the RVFB header",
            BundleError::BadMagic => "no RVFB magic (is -initrd a bundle?)",
            BundleError::BadVersion(_) => "unsupported RVFB version",
            BundleError::Truncated => "an entry runs past the end of the blob",
        }
    }
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

impl<'a> Bundle<'a> {
    pub fn parse(raw: &'a [u8]) -> Result<Bundle<'a>, BundleError> {
        if raw.len() < HEADER_LEN {
            return Err(BundleError::TooShort);
        }
        if &raw[..4] != MAGIC {
            return Err(BundleError::BadMagic);
        }
        let version = u32_at(raw, 4);
        if version != VERSION {
            return Err(BundleError::BadVersion(version));
        }
        let count = u32_at(raw, 8) as usize;
        let table_end = HEADER_LEN
            .checked_add(count.checked_mul(ENTRY_LEN).ok_or(BundleError::Truncated)?)
            .ok_or(BundleError::Truncated)?;
        if table_end > raw.len() {
            return Err(BundleError::Truncated);
        }
        let bundle = Bundle { raw, count };
        // Validate every extent up front, so `get`/`iter` can hand out slices
        // without each caller having to consider a malformed table.
        for i in 0..count {
            bundle.entry(i).ok_or(BundleError::Truncated)?;
        }
        Ok(bundle)
    }

    fn entry(&self, i: usize) -> Option<Blob<'a>> {
        let base = HEADER_LEN + i * ENTRY_LEN;
        let raw_name = self.raw.get(base..base + NAME_LEN)?;
        let name_len = raw_name.iter().position(|&c| c == 0).unwrap_or(NAME_LEN);
        let name = str::from_utf8(&raw_name[..name_len]).ok()?;
        let off = u32_at(self.raw, base + NAME_LEN) as usize;
        let len = u32_at(self.raw, base + NAME_LEN + 4) as usize;
        let bytes = self.raw.get(off..off.checked_add(len)?)?;
        Some(Blob { name, bytes })
    }

    /// Blobs in table order.
    pub fn iter(&self) -> impl Iterator<Item = Blob<'a>> + '_ {
        (0..self.count).filter_map(|i| self.entry(i))
    }

    /// Look a blob up by name, as the hosted frontend would look up a path.
    pub fn get(&self, name: &str) -> Option<Blob<'a>> {
        self.iter().find(|b| b.name == name)
    }
}

/// A [`BlockDevice`](rpi_virt_fw::block::BlockDevice) over a blob that is
/// already in RAM.
///
/// Not `MemoryBlocks`: that owns a `Vec`, so attaching the 256 MiB SD image
/// through it would copy the image into the heap next to the model's 1 GiB of
/// RAM. The bytes are already in machine memory where QEMU's loader put them,
/// and the model only ever reads them, so borrowing is both cheaper and closer
/// to what the eventual SDHCI backend will do.
pub struct BundleBlocks {
    image: &'static [u8],
}

impl BundleBlocks {
    pub fn new(image: &'static [u8]) -> BundleBlocks {
        BundleBlocks { image }
    }
}

impl rpi_virt_fw::block::BlockDevice for BundleBlocks {
    fn block_count(&self) -> u64 {
        (self.image.len() / rpi_virt_fw::block::BLOCK_LEN) as u64
    }

    fn read_block(&self, lba: u64, out: &mut [u8; rpi_virt_fw::block::BLOCK_LEN]) -> bool {
        out.fill(0);
        let len = rpi_virt_fw::block::BLOCK_LEN;
        // Computed in u64 and narrowed only once it is known to be inside the
        // image, so a wild LBA cannot wrap into a valid offset.
        let Some(src) = lba
            .checked_mul(len as u64)
            .and_then(|s| usize::try_from(s).ok())
            .and_then(|s| self.image.get(s..s + len))
        else {
            return false;
        };
        out.copy_from_slice(src);
        true
    }

    // No `write_block`: the default refuses, which is what a blob handed to us
    // read-only in RAM is. The SD model does not write, and the EEPROM's
    // self-update goes to `spi0`'s own copy — bare-metal persistence is the
    // block-backed EEPROM partition, not this.
}
