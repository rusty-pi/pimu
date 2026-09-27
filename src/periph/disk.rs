//! A block device behind a modelled medium: the USB stick and the SD card.
//!
//! The image is read on demand and the blocks written since are kept in
//! memory, so a run costs the blocks it touches rather than the image's size,
//! and the image file is never modified: every run is a first boot. Reading a
//! multi-gigabyte image whole into memory would otherwise be most of a run's
//! footprint.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::log::Log;

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
    /// Where its transfers go (the `io` channel), and the name this disk goes
    /// by there.
    io: Option<(Log, &'static str)>,
}

enum Backing {
    Mem(Vec<u8>),
    File {
        file: File,
        len: u64,
    },
    Dir {
        meta: Vec<u8>,
        files: Vec<Mapped>,
    },
    /// An image served over HTTP, read in [`CHUNK`] pieces through `Range`
    /// requests and kept for the rest of the run: an image is gigabytes, and a
    /// boot reads a fraction of one.
    Remote {
        url: String,
        len: u64,
        chunks: RefCell<HashMap<u64, Vec<u8>>>,
    },
    /// An `.img.xz`, read a block at a time through its own index: neither
    /// unpacked on the host nor fetched whole from a server
    /// ([`crate::xz`]).
    Xz {
        source: Compressed,
        index: crate::xz::Index,
        blocks: RefCell<HashMap<u64, Vec<u8>>>,
    },
}

/// Where a compressed image's bytes come from, which is all the difference
/// between a host file and a URL once the index has been read.
enum Compressed {
    File { file: File, path: String },
    Url(String),
}

impl Compressed {
    fn read(&self, at: u64, len: usize) -> Result<Vec<u8>> {
        match self {
            Compressed::File { file, path } => {
                let mut buf = vec![0u8; len];
                file.read_exact_at(&mut buf, at)
                    .with_context(|| format!("reading {path} at byte {at}"))?;
                Ok(buf)
            }
            Compressed::Url(url) => crate::remote::fetch_range(url, at, len),
        }
    }
}

impl std::fmt::Display for Compressed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Compressed::File { path, .. } => write!(f, "{path}"),
            Compressed::Url(url) => write!(f, "{url}"),
        }
    }
}

/// What one `Range` request reads. Large enough that a boot costs tens of
/// requests rather than thousands, small enough not to pull a whole image in.
const CHUNK: u64 = 1 << 20;

struct Mapped {
    lba: u64,
    blocks: u64,
    body: Body,
    len: u64,
}

/// A card file's bytes. A remote one is fetched whole the first time a block of
/// it is read — a range request per block would be hundreds of round trips for
/// a `start4.elf` — and the cache makes the next run's fetch a read.
enum Body {
    File(File),
    /// A card file the command line made up, so there is nothing to open.
    Mem(Vec<u8>),
    Remote {
        url: String,
        body: OnceCell<Vec<u8>>,
    },
}

impl Mapped {
    /// `out.len()` bytes of the file from `at`, fetching it whole the first time
    /// a remote one is read.
    fn read(&self, out: &mut [u8], at: u64) -> std::io::Result<()> {
        let (url, body) = match &self.body {
            Body::File(file) => return file.read_exact_at(out, at),
            Body::Mem(bytes) => return copy_from(out, bytes, at, "a card file"),
            Body::Remote { url, body } => (url, body),
        };
        if body.get().is_none() {
            let fetched = crate::remote::fetch(url, self.len).map_err(std::io::Error::other)?;
            let _ = body.set(fetched);
        }
        copy_from(out, body.get().expect("just fetched"), at, url)
    }
}

fn copy_from(out: &mut [u8], body: &[u8], at: u64, what: &str) -> std::io::Result<()> {
    let at = at as usize;
    let src = body
        .get(at..at + out.len())
        .ok_or_else(|| std::io::Error::other(format!("{what}: short at byte {at}")))?;
    out.copy_from_slice(src);
    Ok(())
}

impl Disk {
    pub fn from_vec(image: Vec<u8>) -> Disk {
        let blocks = (image.len() / BLOCK_SIZE) as u64;
        Disk {
            backing: Backing::Mem(image),
            blocks,
            written: HashMap::new(),
            io: None,
        }
    }

    /// Log this disk's transfers on the `io` channel as `name`.
    pub fn with_log(mut self, log: Log, name: &'static str) -> Disk {
        log.map_files(name, &|lba| {
            let mut b = [0u8; BLOCK_SIZE];
            self.peek_block(lba, &mut b).then_some(b)
        });
        self.io = Some((log, name));
        self
    }

    fn log(&self, op: &'static str, lba: u64, count: u64) {
        if let Some((log, name)) = &self.io {
            log.blocks(name, op, lba, count);
        }
    }

    pub fn from_card(card: crate::fat::Card) -> std::io::Result<Disk> {
        let mut files = Vec::with_capacity(card.extents.len());
        for extent in card.extents {
            let body = match extent.source {
                crate::fat::Source::Path(path) => Body::File(File::open(&path)?),
                crate::fat::Source::Url(url) => Body::Remote {
                    url,
                    body: OnceCell::new(),
                },
                crate::fat::Source::Bytes(bytes) => Body::Mem(bytes),
            };
            files.push(Mapped {
                lba: extent.lba,
                blocks: extent.blocks,
                body,
                len: extent.len,
            });
        }
        Ok(Disk {
            backing: Backing::Dir {
                meta: card.meta,
                files,
            },
            blocks: card.blocks,
            written: HashMap::new(),
            io: None,
        })
    }

    /// An image at `url`, `len` bytes of it, read as the guest asks for blocks.
    /// An `.xz` stream of more than one block is read through its index, so
    /// what arrives is the decoded image.
    pub fn remote(url: String, len: u64, min_bytes: u64) -> Result<Disk> {
        let source = Compressed::Url(url.clone());
        if let Some(disk) = Self::compressed(source, len, min_bytes)? {
            return Ok(disk);
        }
        Ok(Disk {
            backing: Backing::Remote {
                url,
                len,
                chunks: RefCell::new(HashMap::new()),
            },
            blocks: len.max(min_bytes) / BLOCK_SIZE as u64,
            written: HashMap::new(),
            io: None,
        })
    }

    /// The disk an xz stream of more than one block makes, or `None` when
    /// what is there is not one — the caller reads it as it is.
    fn compressed(source: Compressed, len: u64, min_bytes: u64) -> Result<Option<Disk>> {
        if len < crate::xz::MAGIC.len() as u64
            || source.read(0, crate::xz::MAGIC.len())? != crate::xz::MAGIC
        {
            return Ok(None);
        }
        let index = crate::xz::index(len, &|at, n| source.read(at, n))
            .with_context(|| format!("reading the xz index of {source}"))?;
        if !index.seekable() {
            bail!(
                "{source} is one xz block, so there is no reading a part of it: \
                 `xz -d` it and give the image"
            );
        }
        let out_len = index.out_len;
        Ok(Some(Disk {
            backing: Backing::Xz {
                source,
                index,
                blocks: RefCell::new(HashMap::new()),
            },
            blocks: out_len.max(min_bytes) / BLOCK_SIZE as u64,
            written: HashMap::new(),
            io: None,
        }))
    }

    /// At least `min_bytes` of capacity, reading as zeros past what backs it —
    /// a card or stick bigger than the image or the files written to it.
    pub fn with_capacity(mut self, min_bytes: u64) -> Disk {
        self.blocks = self.blocks.max(min_bytes / BLOCK_SIZE as u64);
        self
    }

    /// A host image, `.xz` or not.
    pub fn open(path: &Path, min_bytes: u64) -> Result<Disk> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let len = file.metadata()?.len();
        let source = Compressed::File {
            file,
            path: path.display().to_string(),
        };
        if let Some(disk) = Self::compressed(source, len, min_bytes)? {
            return Ok(disk);
        }
        let file = File::open(path)?;
        Ok(Disk {
            backing: Backing::File { file, len },
            blocks: len.max(min_bytes) / BLOCK_SIZE as u64,
            written: HashMap::new(),
            io: None,
        })
    }

    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    #[cfg(test)]
    pub(crate) fn set_blocks(&mut self, blocks: u64) {
        self.blocks = blocks;
    }

    pub fn written_blocks(&self) -> usize {
        self.written.len()
    }

    /// Block `lba` into `out`; `false` and zeros past the end of the disk.
    pub fn read_block(&self, lba: u64, out: &mut [u8; BLOCK_SIZE]) -> bool {
        let ok = self.peek_block(lba, out);
        if ok {
            self.log("read", lba, 1);
        }
        ok
    }

    /// [`Self::read_block`] without counting as a guest read.
    pub fn peek_block(&self, lba: u64, out: &mut [u8; BLOCK_SIZE]) -> bool {
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
                let n = BLOCK_SIZE.min((*len - at) as usize);
                if let Err(e) = file.read_exact_at(&mut out[..n], at) {
                    panic!("reading the disk image at byte {at}: {e}");
                }
            }
            Backing::File { .. } => {}
            Backing::Remote { url, len, chunks } => {
                if at < *len {
                    let n = BLOCK_SIZE.min((*len - at) as usize);
                    let first = at / CHUNK;
                    let mut chunks = chunks.borrow_mut();
                    let chunk = match chunks.get(&first) {
                        Some(chunk) => chunk,
                        None => {
                            let start = first * CHUNK;
                            let want = CHUNK.min(*len - start) as usize;
                            match crate::remote::fetch_range(url, start, want) {
                                Ok(bytes) => chunks.entry(first).or_insert(bytes),
                                Err(e) => panic!("reading {url} at byte {start}: {e:#}"),
                            }
                        }
                    };
                    let from = (at - first * CHUNK) as usize;
                    // A block never straddles two chunks: both are powers of two.
                    out[..n].copy_from_slice(&chunk[from..from + n]);
                }
            }
            Backing::Xz {
                source,
                index,
                blocks,
            } => {
                if let Some(block) = index.block_at(at) {
                    let n = BLOCK_SIZE.min((index.out_len - at) as usize);
                    let mut blocks = blocks.borrow_mut();
                    let bytes = match blocks.get(&block.out_at) {
                        Some(bytes) => bytes,
                        None => {
                            let decoded = source
                                .read(block.at, block.len as usize)
                                .and_then(|raw| {
                                    crate::xz::decode(&index.header, &raw, block.out_len as usize)
                                })
                                .unwrap_or_else(|e| {
                                    panic!("reading {source} at decoded byte {at}: {e:#}")
                                });
                            blocks.entry(block.out_at).or_insert(decoded)
                        }
                    };
                    let from = (at - block.out_at) as usize;
                    // Every block but the last decodes to a whole number of
                    // disk blocks, so a read never straddles two of them.
                    out[..n].copy_from_slice(&bytes[from..from + n]);
                }
            }
            Backing::Dir { meta, files } => {
                if let Some(src) = meta.get(at as usize..at as usize + BLOCK_SIZE) {
                    out.copy_from_slice(src);
                } else if let Ok(i) = files.binary_search_by(|f| {
                    if lba < f.lba {
                        std::cmp::Ordering::Greater
                    } else if lba >= f.lba + f.blocks {
                        std::cmp::Ordering::Less
                    } else {
                        std::cmp::Ordering::Equal
                    }
                }) {
                    let file = &files[i];
                    let at = (lba - file.lba) * BLOCK_SIZE as u64;
                    if at < file.len {
                        let n = BLOCK_SIZE.min((file.len - at) as usize);
                        if let Err(e) = file.read(&mut out[..n], at) {
                            panic!("reading a card file at byte {at}: {e}");
                        }
                    }
                }
            }
        }
        true
    }

    pub fn read(&self, lba: u64, count: u64) -> Option<Vec<u8>> {
        if lba.checked_add(count)? > self.blocks {
            return None;
        }
        let mut out = vec![0u8; count as usize * BLOCK_SIZE];
        for (i, block) in out.as_chunks_mut::<BLOCK_SIZE>().0.iter_mut().enumerate() {
            self.peek_block(lba + i as u64, block);
        }
        self.log("read", lba, count);
        Some(out)
    }

    pub fn write(&mut self, lba: u64, data: &[u8]) -> bool {
        let count = (data.len() / BLOCK_SIZE) as u64;
        if lba.saturating_add(count) > self.blocks {
            return false;
        }
        for (i, block) in data.as_chunks::<BLOCK_SIZE>().0.iter().enumerate() {
            self.written.insert(lba + i as u64, Box::new(*block));
        }
        self.log("write", lba, count);
        true
    }

    pub fn zero(&mut self, first: u64, last: u64) {
        let last = last.min(self.blocks.saturating_sub(1));
        for lba in first..=last {
            self.written.insert(lba, Box::new([0; BLOCK_SIZE]));
        }
        if first <= last {
            self.log("erase", first, last - first + 1);
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

    /// A compressed image reads as the image it decodes to, block for block,
    /// and what the guest writes to it still overlays it. Skipped without
    /// `xz`, which not every builder has.
    #[test]
    fn an_xz_image_reads_as_the_image_inside_it() {
        let mut raw = vec![0u8; 3 << 20];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = (i / BLOCK_SIZE) as u8;
        }
        let dir = std::env::temp_dir().join(format!("pimu-disk-xz-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("image");
        std::fs::write(&path, &raw).unwrap();
        let made = std::process::Command::new("xz")
            .args(["-0", "--block-size=1MiB", "-T2"])
            .arg(&path)
            .status();
        match made {
            Ok(s) if s.success() => {}
            Ok(s) => panic!("xz failed: {s}"),
            Err(_) => {
                eprintln!("xz not installed; skipping");
                std::fs::remove_dir_all(&dir).unwrap();
                return;
            }
        }

        let mut disk = Disk::open(&dir.join("image.xz"), 0).unwrap();
        assert_eq!(disk.blocks(), (3 << 20) / BLOCK_SIZE as u64);
        let mut b = [0u8; BLOCK_SIZE];
        // The first block of each xz block, and one either side of a boundary.
        for lba in [0, 1, 2047, 2048, 4095, 5000] {
            assert!(disk.read_block(lba, &mut b), "block {lba}");
            assert_eq!(
                b[..],
                raw[lba as usize * BLOCK_SIZE..][..BLOCK_SIZE],
                "block {lba}"
            );
        }
        assert!(!disk.read_block((3 << 20) / BLOCK_SIZE as u64, &mut b));
        assert!(disk.write(7, &[0x55; BLOCK_SIZE]));
        assert_eq!(disk.read(7, 1).unwrap()[0], 0x55);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
