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

use alloc::boxed::Box;
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

/// A contiguous run of sectors of another device, addressed from zero.
///
/// This is how the EEPROM gets a home. It is its own chip on real hardware —
/// SPI NOR, nothing to do with the SD card — but `raspi4b` offers no second
/// medium to model it with, so bare-metal it becomes a partition of the one
/// drive QEMU does accept. `Window` is the adapter that makes a partition look
/// like a whole device, so `Spi0` can be backed by one without knowing it is
/// not alone on the disk.
///
/// Nothing about the trait changes: a `Window` is a `BlockDevice` over a
/// `BlockDevice`, so it stacks (a window of a window is fine) and it works over
/// a `Box<dyn BlockDevice>` as readily as over a [`MemoryBlocks`].
///
/// **The bound is the point.** LBA 0 of the window is `first_lba` of the inner
/// device and LBA `blocks` does not exist. An access past the end fails here
/// rather than reaching the inner device, so a runaway EEPROM write cannot
/// reach into the FAT partition the firmware is about to boot from — the exact
/// failure a raw offset would make silent and unrecoverable.
pub struct Window<D> {
    inner: D,
    first_lba: u64,
    blocks: u64,
}

impl<D: BlockDevice> Window<D> {
    /// `blocks` sectors of `inner` starting at `first_lba`.
    ///
    /// `None` if that run does not fit inside `inner` — a window the backing
    /// device cannot satisfy is a mistake in the caller's layout, and it is
    /// worth catching where the layout is described rather than on whichever
    /// access first runs off the end.
    pub fn new(inner: D, first_lba: u64, blocks: u64) -> Option<Window<D>> {
        let end = first_lba.checked_add(blocks)?;
        if end > inner.block_count() {
            return None;
        }
        Some(Window {
            inner,
            first_lba,
            blocks,
        })
    }

    /// The window described by MBR partition `index` (1-based) of `inner`.
    ///
    /// The partition table is read through the same trait, so this works on
    /// whatever the medium happens to be. Only the geometry is taken: the type
    /// byte is not checked, because what a partition holds is the caller's
    /// business — the EEPROM one is type `0xDA` ("non-FS data"), which no
    /// filesystem prober would recognise anyway.
    pub fn mbr_partition(inner: D, index: usize) -> Option<Window<D>> {
        let (first_lba, blocks) = mbr_partition(&inner, index)?;
        Window::new(inner, first_lba, blocks)
    }

    /// Where the window sits in the backing device, for a caller that has to
    /// report the layout it resolved.
    pub fn first_lba(&self) -> u64 {
        self.first_lba
    }

    pub fn into_inner(self) -> D {
        self.inner
    }

    /// The inner LBA for `lba`, or `None` if `lba` is outside the window.
    fn map(&self, lba: u64) -> Option<u64> {
        if lba >= self.blocks {
            return None;
        }
        // Cannot overflow: `new` proved `first_lba + blocks` fits in a u64.
        Some(self.first_lba + lba)
    }
}

impl<D: BlockDevice> BlockDevice for Window<D> {
    fn block_count(&self) -> u64 {
        self.blocks
    }

    fn read_block(&self, lba: u64, out: &mut [u8; BLOCK_LEN]) -> bool {
        match self.map(lba) {
            Some(inner) => self.inner.read_block(inner, out),
            None => {
                // Same contract as a read past the end of the medium: defined
                // bytes, and the failure reported.
                out.fill(0);
                false
            }
        }
    }

    fn write_block(&mut self, lba: u64, data: &[u8; BLOCK_LEN]) -> bool {
        match self.map(lba) {
            Some(inner) => self.inner.write_block(inner, data),
            None => false,
        }
    }
}

/// `(first_lba, blocks)` of MBR partition `index` (1-based, so 1..=4), or
/// `None` if the medium has no MBR signature or that entry is empty.
///
/// The classic table: 64 bytes at offset 446 of LBA 0, four 16-byte entries,
/// `0x55 0xAA` at 510. Within an entry the LBA fields are little-endian at +8
/// (first sector) and +12 (sector count); the CHS fields are ignored, as they
/// have been by everything for thirty years.
pub fn mbr_partition(dev: &dyn BlockDevice, index: usize) -> Option<(u64, u64)> {
    if !(1..=4).contains(&index) {
        return None;
    }
    let mut mbr = [0u8; BLOCK_LEN];
    if !dev.read_block(0, &mut mbr) {
        return None;
    }
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        return None;
    }
    let e = &mbr[446 + (index - 1) * 16..][..16];
    let le32 = |o: usize| u32::from_le_bytes([e[o], e[o + 1], e[o + 2], e[o + 3]]) as u64;
    let (first_lba, blocks) = (le32(8), le32(12));
    if blocks == 0 {
        return None;
    }
    Some((first_lba, blocks))
}

/// Every sector of `dev`, concatenated — a whole medium as a flat image.
///
/// The byte-addressed models want this: `Spi0` serves a NOR image with 4 KiB
/// sector erase and byte-granular page program, which is nothing like a block
/// device, so it reads its backing store once and works in RAM from there.
pub fn read_all(dev: &dyn BlockDevice) -> Vec<u8> {
    let count = usize::try_from(dev.block_count()).unwrap_or(usize::MAX);
    let mut image = Vec::with_capacity(count.saturating_mul(BLOCK_LEN));
    let mut sector = [0u8; BLOCK_LEN];
    for lba in 0..dev.block_count() {
        // A failed read still leaves zeros in `sector`, which is the right
        // filler: it keeps the image the length the medium claimed.
        dev.read_block(lba, &mut sector);
        image.extend_from_slice(&sector);
    }
    image
}

impl<D: BlockDevice + ?Sized> BlockDevice for Box<D> {
    fn block_count(&self) -> u64 {
        (**self).block_count()
    }

    fn read_block(&self, lba: u64, out: &mut [u8; BLOCK_LEN]) -> bool {
        (**self).read_block(lba, out)
    }

    fn write_block(&mut self, lba: u64, data: &[u8; BLOCK_LEN]) -> bool {
        (**self).write_block(lba, data)
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

/// A disk image on the filesystem, read and written in place.
///
/// The hosted counterpart of what QEMU does for the bare-metal frontend with
/// `-drive if=sd,file=sd.img,format=raw`: the sectors stay in the file, and a
/// write is durable. `--sd <img>` still slurps the whole image into a
/// [`MemoryBlocks`] — the boot reads it constantly and never writes it — but
/// the EEPROM partition wants the opposite, so it gets this.
#[cfg(feature = "std")]
pub struct FileBlocks {
    file: std::fs::File,
    blocks: u64,
}

#[cfg(feature = "std")]
impl FileBlocks {
    /// Open `path` read/write. The capacity is the file's length in whole
    /// sectors, fixed at open time — the file is never grown, for the same
    /// reason [`MemoryBlocks`] is not.
    pub fn open(path: &std::path::Path) -> std::io::Result<FileBlocks> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let blocks = file.metadata()?.len() / BLOCK_LEN as u64;
        Ok(FileBlocks { file, blocks })
    }
}

#[cfg(feature = "std")]
impl BlockDevice for FileBlocks {
    fn block_count(&self) -> u64 {
        self.blocks
    }

    fn read_block(&self, lba: u64, out: &mut [u8; BLOCK_LEN]) -> bool {
        use std::os::unix::fs::FileExt;
        out.fill(0);
        if lba >= self.blocks {
            return false;
        }
        self.file.read_exact_at(out, lba * BLOCK_LEN as u64).is_ok()
    }

    fn write_block(&mut self, lba: u64, data: &[u8; BLOCK_LEN]) -> bool {
        use std::os::unix::fs::FileExt;
        if lba >= self.blocks {
            return false;
        }
        self.file.write_all_at(data, lba * BLOCK_LEN as u64).is_ok()
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

    /// A 4-sector image with an MBR in LBA 0 and two partitions, shaped like
    /// what `scripts/make-sd.sh` writes: a big one first, the EEPROM last.
    fn partitioned() -> Vec<u8> {
        let mut img = vec![0u8; 8 * BLOCK_LEN];
        let mut entry = |n: usize, ty: u8, first: u32, count: u32| {
            let e = 446 + n * 16;
            img[e + 4] = ty;
            img[e + 8..e + 12].copy_from_slice(&first.to_le_bytes());
            img[e + 12..e + 16].copy_from_slice(&count.to_le_bytes());
        };
        entry(0, 0x0c, 1, 4);
        entry(1, 0xda, 5, 2);
        img[510] = 0x55;
        img[511] = 0xAA;
        for (lba, chunk) in img.chunks_mut(BLOCK_LEN).enumerate().skip(1) {
            chunk.fill(lba as u8);
        }
        img
    }

    #[test]
    fn a_window_addresses_its_slice_from_zero() {
        let win = Window::new(MemoryBlocks::new(partitioned()), 5, 2).unwrap();
        assert_eq!(win.block_count(), 2);
        assert_eq!(win.first_lba(), 5);
        let mut out = [0u8; BLOCK_LEN];
        assert!(win.read_block(0, &mut out));
        assert_eq!(out, [5u8; BLOCK_LEN]);
        assert!(win.read_block(1, &mut out));
        assert_eq!(out, [6u8; BLOCK_LEN]);
    }

    /// The bound is what stops an EEPROM write from reaching the neighbouring
    /// partition. Both directions: nothing outside the window is readable, and
    /// nothing outside it is writable.
    #[test]
    fn a_window_cannot_be_read_or_written_past_its_end() {
        let mut win = Window::new(MemoryBlocks::new(partitioned()), 5, 2).unwrap();
        let mut out = [0xAA; BLOCK_LEN];
        assert!(!win.read_block(2, &mut out));
        assert_eq!(out, [0u8; BLOCK_LEN]);
        assert!(!win.read_block(u64::MAX, &mut out));

        assert!(!win.write_block(2, &[0xFF; BLOCK_LEN]));
        assert!(!win.write_block(u64::MAX, &[0xFF; BLOCK_LEN]));
        // LBA 7 of the image — one past the window — is untouched, i.e. the
        // rejected write did not land at the inner LBA by accident.
        let img = win.into_inner();
        assert_eq!(&img.image()[7 * BLOCK_LEN..], &[7u8; BLOCK_LEN]);
    }

    #[test]
    fn a_window_write_lands_inside_the_window_only() {
        let mut win = Window::new(MemoryBlocks::new(partitioned()), 5, 2).unwrap();
        assert!(win.write_block(1, &[0x5A; BLOCK_LEN]));
        let img = win.into_inner();
        // Sector 6 rewritten; its neighbours 5 and 7 left alone.
        assert_eq!(
            &img.image()[5 * BLOCK_LEN..6 * BLOCK_LEN],
            &[5u8; BLOCK_LEN]
        );
        assert_eq!(
            &img.image()[6 * BLOCK_LEN..7 * BLOCK_LEN],
            &[0x5A; BLOCK_LEN]
        );
        assert_eq!(&img.image()[7 * BLOCK_LEN..], &[7u8; BLOCK_LEN]);
    }

    /// A window the backing device cannot satisfy is rejected where it is
    /// described, not on the access that first runs off the end.
    #[test]
    fn a_window_must_fit_inside_its_backing_device() {
        assert!(Window::new(MemoryBlocks::new(image()), 2, 2).is_some());
        assert!(Window::new(MemoryBlocks::new(image()), 2, 3).is_none());
        assert!(Window::new(MemoryBlocks::new(image()), 4, 1).is_none());
        // Zero sectors fits, and is empty rather than unbounded.
        let win = Window::new(MemoryBlocks::new(image()), 4, 0).unwrap();
        assert_eq!(win.block_count(), 0);
        assert!(!win.read_block(0, &mut [0u8; BLOCK_LEN]));
        // `first_lba + blocks` must not wrap into a valid range.
        assert!(Window::new(MemoryBlocks::new(image()), u64::MAX, 2).is_none());
    }

    #[test]
    fn a_window_can_be_taken_from_the_partition_table() {
        let img = partitioned();
        assert_eq!(
            mbr_partition(&MemoryBlocks::new(img.clone()), 1),
            Some((1, 4))
        );
        assert_eq!(
            mbr_partition(&MemoryBlocks::new(img.clone()), 2),
            Some((5, 2))
        );
        // Empty entries and out-of-range indices are not partitions.
        assert_eq!(mbr_partition(&MemoryBlocks::new(img.clone()), 3), None);
        assert_eq!(mbr_partition(&MemoryBlocks::new(img.clone()), 0), None);
        assert_eq!(mbr_partition(&MemoryBlocks::new(img.clone()), 5), None);
        // No signature, no table.
        let mut unlabelled = img.clone();
        unlabelled[510] = 0;
        assert_eq!(mbr_partition(&MemoryBlocks::new(unlabelled), 2), None);

        let win = Window::mbr_partition(MemoryBlocks::new(img), 2).unwrap();
        assert_eq!((win.first_lba(), win.block_count()), (5, 2));
    }

    /// Composing over a `Box<dyn BlockDevice>` is what `Spi0`'s backing store
    /// does, so the blanket impl has to hold.
    #[test]
    fn a_window_composes_over_a_boxed_device() {
        let inner: Box<dyn BlockDevice> = Box::new(MemoryBlocks::new(partitioned()));
        let win = Window::mbr_partition(inner, 2).unwrap();
        assert_eq!(read_all(&win).len(), 2 * BLOCK_LEN);
        assert_eq!(&read_all(&win)[..BLOCK_LEN], &[5u8; BLOCK_LEN]);
    }

    #[test]
    fn read_all_returns_every_sector_in_order() {
        let dev = MemoryBlocks::new(image());
        let flat = read_all(&dev);
        assert_eq!(flat.len(), 4 * BLOCK_LEN);
        assert_eq!(flat, image());
        assert!(read_all(&MemoryBlocks::new(Vec::new())).is_empty());
    }
}
