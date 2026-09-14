//! A block device behind a modelled medium: the USB stick and the SD card.
//!
//! The image is read on demand and the blocks written since are kept in
//! memory, so a run costs the blocks it touches rather than the image's size,
//! and the image file is never modified: every run is a first boot. A
//! multi-gigabyte disk image read whole into memory used to be most of a run's
//! footprint (#54).

use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

pub const BLOCK_SIZE: usize = 512;

/// A disk image read on demand, the blocks written since kept in memory, and a
/// capacity that may be larger than the image.
///
/// A Pi boots an image written to the start of a bigger stick, and its first
/// boot uses the rest: the rpi-mkosi initrd's `systemd-repart` creates the
/// encrypted root partition there. So the disk can be bigger than its image
/// (`boot --usb-mb`), reading as zeros past it.
pub struct Disk {
    backing: Backing,
    blocks: u64,
    written: HashMap<u64, Box<[u8; BLOCK_SIZE]>>,
}

enum Backing {
    Mem(Vec<u8>),
    File { file: File, len: u64 },
}

impl Disk {
    /// A disk exactly the size of `image`.
    pub fn from_vec(image: Vec<u8>) -> Disk {
        let blocks = (image.len() / BLOCK_SIZE) as u64;
        Disk {
            backing: Backing::Mem(image),
            blocks,
            written: HashMap::new(),
        }
    }

    /// The image at `path`, on a disk of at least `min_bytes`.
    pub fn open(path: &Path, min_bytes: u64) -> std::io::Result<Disk> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Disk {
            backing: Backing::File { file, len },
            blocks: len.max(min_bytes) / BLOCK_SIZE as u64,
            written: HashMap::new(),
        })
    }

    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    /// Resize, for a test that wants a disk bigger than its image.
    #[cfg(test)]
    pub(crate) fn set_blocks(&mut self, blocks: u64) {
        self.blocks = blocks;
    }

    /// How many blocks the host has written.
    pub fn written_blocks(&self) -> usize {
        self.written.len()
    }

    /// Block `lba` into `out`; `false` (and zeros) past the end of the disk.
    pub fn read_block(&self, lba: u64, out: &mut [u8; BLOCK_SIZE]) -> bool {
        out.fill(0);
        if lba >= self.blocks {
            return false;
        }
        if let Some(w) = self.written.get(&lba) {
            out.copy_from_slice(&w[..]);
            return true;
        }
        let at = lba * BLOCK_SIZE as u64;
        match &self.backing {
            Backing::Mem(image) => {
                if let Some(src) = image.get(at as usize..at as usize + BLOCK_SIZE) {
                    out.copy_from_slice(src);
                }
            }
            Backing::File { file, len } if at < *len => {
                // The image's last block may be short; the rest is zeros.
                let n = BLOCK_SIZE.min((*len - at) as usize);
                if let Err(e) = file.read_exact_at(&mut out[..n], at) {
                    panic!("reading the disk image at byte {at}: {e}");
                }
            }
            Backing::File { .. } => {}
        }
        true
    }

    /// `count` blocks from `lba`, or `None` past the end of the disk.
    pub fn read(&self, lba: u64, count: u64) -> Option<Vec<u8>> {
        if lba.checked_add(count)? > self.blocks {
            return None;
        }
        let mut out = vec![0u8; count as usize * BLOCK_SIZE];
        for (i, block) in out.as_chunks_mut::<BLOCK_SIZE>().0.iter_mut().enumerate() {
            self.read_block(lba + i as u64, block);
        }
        Some(out)
    }

    /// Whole blocks of `data` at `lba`; `false` past the end of the disk.
    pub fn write(&mut self, lba: u64, data: &[u8]) -> bool {
        let count = (data.len() / BLOCK_SIZE) as u64;
        if lba.saturating_add(count) > self.blocks {
            return false;
        }
        for (i, block) in data.as_chunks::<BLOCK_SIZE>().0.iter().enumerate() {
            self.written.insert(lba + i as u64, Box::new(*block));
        }
        true
    }

    /// Zero blocks `first..=last`, clipped to the disk.
    pub fn zero(&mut self, first: u64, last: u64) {
        let last = last.min(self.blocks.saturating_sub(1));
        for lba in first..=last {
            self.written.insert(lba, Box::new([0; BLOCK_SIZE]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_erases_overlay_the_image() {
        let mut image = vec![0u8; 4 * BLOCK_SIZE];
        image[BLOCK_SIZE] = 0xAA;
        let mut d = Disk::from_vec(image);
        let mut b = [0u8; BLOCK_SIZE];
        assert!(d.read_block(1, &mut b) && b[0] == 0xAA);
        assert!(d.write(2, &[0x55; BLOCK_SIZE]));
        assert_eq!(d.read(2, 1).unwrap()[0], 0x55);
        d.zero(1, 99);
        assert!(d.read_block(1, &mut b) && b[0] == 0);
        assert!(!d.read_block(4, &mut b));
        assert_eq!(d.written_blocks(), 3);
    }
}
