//! Where the SD and USB models get their sectors from.
//!
//! Hosted, a disk image is a `Vec<u8>`: `--sd sd.img` is read once and the
//! whole thing sits in RAM. Bare-metal ([#32]) it cannot be. The emulator runs
//! as an arm64 `-kernel` with no filesystem under it, the image is attached to
//! QEMU the ordinary way (`-drive if=sd,file=sd.img,format=raw`), and the
//! sectors live behind QEMU's own SD controller — to be fetched a block at a
//! time, over MMIO, when the firmware asks for them.
//!
//! So the peripherals take a [`BlockDevice`] instead of owning bytes. Hosted
//! that is [`MemoryBlocks`], which is exactly the old behaviour; bare-metal it
//! will be a driver for QEMU's SDHCI at ARM `0xFE34_0000`. Two SD controllers
//! in series, then: the eMMC2 we model, serving the VPU firmware, backed by the
//! real one underneath.
//!
//! The read is fallible — `false` rather than a `Result`, because a caller can
//! do nothing with a reason. A `Vec` cannot fail, but a controller can (no card,
//! timeout, CRC), and both call sites already have somewhere sensible to put
//! that: the SD model zero-fills the sector, as it always has for a read past
//! the end of the image, and the USB model fails the SCSI command. An error
//! type here would be `unwrap()`ed on the hosted path and discarded on the
//! other.
//!
//! There is a write path even though no *modelled* device writes today — the
//! SD card is read-only ("Writes are not modelled", `sdcard.rs`) and the
//! mass-storage device answers no WRITE opcode. The blob that does get
//! rewritten is the EEPROM: `pieeprom.upd` self-updates mid-boot, `spi0.rs`
//! keeps the modified bytes, and hosted they survive only if `RVF_DUMP_FLASH`
//! is set. Bare-metal the EEPROM has nowhere else to live — `raspi4b` takes no
//! second drive (`-pflash`, `if=sd,index=1` and every virtio transport are all
//! refused) — so it gets a partition on the SD image, reached by LBA through
//! this same trait, and QEMU persists the writes back to the file. The
//! firmware never notices: it still sees serial NOR through `spi0.rs`, and
//! where the host keeps those bytes is a backend detail.
//!
//! A medium may of course be read-only, so [`BlockDevice::write_block`]
//! defaults to refusing.
//!
//! [#32]: https://github.com/valtzu/rpi-virt-fw/issues/32

use alloc::vec::Vec;

/// Sector size. 512 everywhere in this machine: SDHC addresses in 512-byte
/// units, and the mass-storage model reports 512 in READ CAPACITY.
pub const BLOCK_LEN: usize = 512;

/// A flat array of 512-byte sectors, however they are actually stored.
pub trait BlockDevice {
    /// How many sectors the medium has. Read as the card/disk capacity, so a
    /// partial trailing sector does not count.
    fn block_count(&self) -> u64;

    /// Fetch sector `lba` into `out`.
    ///
    /// Returns `false` if the sector could not be supplied — past the end of
    /// the medium, or a transport failure on a real controller. `out` is fully
    /// written either way: zeros on failure, so a caller that ignores the
    /// result still sees defined bytes rather than the previous sector.
    fn read_block(&self, lba: u64, out: &mut [u8; BLOCK_LEN]) -> bool;

    /// Store `data` in sector `lba`, returning whether it landed.
    ///
    /// The default is "read-only medium", which is what every device modelled
    /// today wants. Persistence is the backend's business: a file-backed or
    /// QEMU-backed medium makes the write durable, a [`MemoryBlocks`] only
    /// makes it visible to later reads in this run.
    fn write_block(&mut self, lba: u64, data: &[u8; BLOCK_LEN]) -> bool {
        let _ = (lba, data);
        false
    }
}

/// A disk image held in RAM — the hosted backend, and what every test uses.
pub struct MemoryBlocks {
    image: Vec<u8>,
}

impl MemoryBlocks {
    pub fn new(image: Vec<u8>) -> MemoryBlocks {
        MemoryBlocks { image }
    }

    /// The image bytes, for the callers that still want the whole thing —
    /// including any writes, which is how a hosted caller would persist them.
    pub fn image(&self) -> &[u8] {
        &self.image
    }
}

impl BlockDevice for MemoryBlocks {
    fn block_count(&self) -> u64 {
        (self.image.len() / BLOCK_LEN) as u64
    }

    fn read_block(&self, lba: u64, out: &mut [u8; BLOCK_LEN]) -> bool {
        out.fill(0);
        // `lba * BLOCK_LEN` is computed in u64 and only narrowed once it is
        // known to be inside the image, so a wild LBA cannot wrap into a valid
        // offset on a 32-bit host.
        let Some(start) = lba.checked_mul(BLOCK_LEN as u64) else {
            return false;
        };
        let Some(src) = start
            .try_into()
            .ok()
            .and_then(|start: usize| self.image.get(start..start + BLOCK_LEN))
        else {
            return false;
        };
        out.copy_from_slice(src);
        true
    }

    fn write_block(&mut self, lba: u64, data: &[u8; BLOCK_LEN]) -> bool {
        let Some(dst) = lba
            .checked_mul(BLOCK_LEN as u64)
            .and_then(|start| usize::try_from(start).ok())
            .and_then(|start| self.image.get_mut(start..start + BLOCK_LEN))
        else {
            // Never grow the image: the medium's capacity is what the model
            // reported to the firmware, and a write past it is an error there
            // as it would be on real hardware.
            return false;
        };
        dst.copy_from_slice(data);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Vec<u8> {
        let mut img = vec![0u8; 4 * BLOCK_LEN];
        for (lba, chunk) in img.chunks_mut(BLOCK_LEN).enumerate() {
            chunk.fill(lba as u8 + 1);
        }
        img
    }

    #[test]
    fn round_trips_every_sector() {
        let dev = MemoryBlocks::new(image());
        assert_eq!(dev.block_count(), 4);
        let mut out = [0u8; BLOCK_LEN];
        for lba in 0..4u64 {
            assert!(dev.read_block(lba, &mut out));
            assert_eq!(out, [lba as u8 + 1; BLOCK_LEN]);
        }
    }

    /// Past the end reads back zeros and reports the failure — what the SD
    /// model has always done, and what the USB model turns into a SCSI error.
    #[test]
    fn reads_past_the_end_fail_and_zero_the_buffer() {
        let dev = MemoryBlocks::new(image());
        let mut out = [0xAA; BLOCK_LEN];
        assert!(!dev.read_block(4, &mut out));
        assert_eq!(out, [0u8; BLOCK_LEN]);
        // A wild LBA must not wrap into a valid offset.
        assert!(!dev.read_block(u64::MAX, &mut out));
        assert_eq!(out, [0u8; BLOCK_LEN]);
    }

    /// A trailing partial sector is not a sector: capacity truncates, and the
    /// bytes in it are unreachable.
    #[test]
    fn a_partial_trailing_sector_is_not_counted() {
        let dev = MemoryBlocks::new(vec![0xFF; BLOCK_LEN + 1]);
        assert_eq!(dev.block_count(), 1);
        let mut out = [0u8; BLOCK_LEN];
        assert!(dev.read_block(0, &mut out));
        assert!(!dev.read_block(1, &mut out));
    }

    #[test]
    fn writes_are_visible_to_later_reads_and_bounded_by_capacity() {
        let mut dev = MemoryBlocks::new(image());
        assert!(dev.write_block(2, &[0x5A; BLOCK_LEN]));
        let mut out = [0u8; BLOCK_LEN];
        assert!(dev.read_block(2, &mut out));
        assert_eq!(out, [0x5A; BLOCK_LEN]);
        assert_eq!(
            &dev.image()[2 * BLOCK_LEN..3 * BLOCK_LEN],
            &[0x5A; BLOCK_LEN]
        );
        // Past the end the image must not grow.
        assert!(!dev.write_block(4, &[0xFF; BLOCK_LEN]));
        assert!(!dev.write_block(u64::MAX, &[0xFF; BLOCK_LEN]));
        assert_eq!(dev.block_count(), 4);
    }

    /// The trait's default is a read-only medium; only backends that opt in
    /// accept writes.
    #[test]
    fn a_medium_is_read_only_unless_it_says_otherwise() {
        struct Rom;
        impl BlockDevice for Rom {
            fn block_count(&self) -> u64 {
                1
            }
            fn read_block(&self, _lba: u64, out: &mut [u8; BLOCK_LEN]) -> bool {
                out.fill(0xEE);
                true
            }
        }
        assert!(!Rom.write_block(0, &[0u8; BLOCK_LEN]));
    }

    #[test]
    fn an_empty_image_has_no_sectors() {
        let dev = MemoryBlocks::new(Vec::new());
        assert_eq!(dev.block_count(), 0);
        assert!(!dev.read_block(0, &mut [0u8; BLOCK_LEN]));
    }
}
