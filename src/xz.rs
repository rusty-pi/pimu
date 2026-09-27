//! Random access into an xz stream: the block index says where every block
//! starts and how much it decodes to, so a compressed disk image is read in
//! place — a `.img.xz` on the host, or one on a server over `Range` requests,
//! is never unpacked and never fetched whole.
//!
//! Only a stream built with more than one block can be read this way, which is
//! what `xz -T<n>` and `xz --block-size` produce, and what the distributions
//! publish: Ubuntu's Raspberry Pi image is 3850 blocks of 1 MiB. A stream of
//! one block has no random access in it at all, and the caller falls back to
//! decoding the whole of it.
//!
//! Decoding is `xz -dc --single-stream` on the stream header and the block:
//! the tool is everywhere, and a decompressor is a large dependency for an
//! option most runs never use.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

/// `.xz` begins with this, whatever it is called.
pub const MAGIC: [u8; 6] = [0xfd, b'7', b'z', b'X', b'Z', 0x00];

const HEADER: usize = 12;
const FOOTER: usize = 12;

/// One block of the stream, as the index describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    /// Where its bytes are in the compressed stream, and how many there are.
    pub at: u64,
    pub len: u64,
    /// Where they land once decoded, and how many they become.
    pub out_at: u64,
    pub out_len: u64,
}

/// What the stream's own index says about it.
#[derive(Clone, Debug)]
pub struct Index {
    /// The stream header, which every block needs in front of it to decode.
    pub header: [u8; HEADER],
    pub blocks: Vec<Block>,
    pub out_len: u64,
}

impl Index {
    /// The block byte `out_at` of the decoded image falls in.
    pub fn block_at(&self, out_at: u64) -> Option<&Block> {
        let i = self
            .blocks
            .partition_point(|b| b.out_at + b.out_len <= out_at);
        self.blocks.get(i).filter(|b| b.out_at <= out_at)
    }

    /// A stream of one block is no better than a plain `.xz`: there is nothing
    /// to seek to, since decoding byte 0 costs the same as decoding all of it.
    pub fn seekable(&self) -> bool {
        self.blocks.len() > 1
    }
}

/// The index of the `len`-byte stream `read` reads: `read(at, n)` is `n` bytes
/// of it from `at`, however the caller comes by them.
pub fn index(len: u64, read: &dyn Fn(u64, usize) -> Result<Vec<u8>>) -> Result<Index> {
    if len < (HEADER + FOOTER) as u64 {
        bail!("{len} bytes is too short to be an xz stream");
    }
    let header: [u8; HEADER] = read(0, HEADER)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("short read of the xz stream header"))?;
    if header[..MAGIC.len()] != MAGIC {
        bail!("not an xz stream: it does not start with the magic");
    }
    let footer = read(len - FOOTER as u64, FOOTER)?;
    if footer[10..] != *b"YZ" {
        bail!("not an xz stream: no footer at the end of it");
    }
    // The backward size counts the index in four-byte units, one less than it
    // is: the field is four bytes of little-endian at offset 4.
    let backward = u32::from_le_bytes([footer[4], footer[5], footer[6], footer[7]]);
    let index_len = (backward as u64 + 1) * 4;
    let index_at = len
        .checked_sub(FOOTER as u64 + index_len)
        .context("the xz index is longer than the stream")?;
    let index = read(index_at, index_len as usize)?;
    let records = records(&index)?;

    // A block's bytes start after the header and every block before it, each
    // padded out to a multiple of four.
    let mut blocks = Vec::with_capacity(records.len());
    let (mut at, mut out_at) = (HEADER as u64, 0u64);
    for (unpadded, out_len) in records {
        blocks.push(Block {
            at,
            len: unpadded,
            out_at,
            out_len,
        });
        at += unpadded.next_multiple_of(4);
        out_at += out_len;
    }
    Ok(Index {
        header,
        blocks,
        out_len: out_at,
    })
}

/// The index's records: an unpadded size and an uncompressed size each.
fn records(index: &[u8]) -> Result<Vec<(u64, u64)>> {
    let mut i = 0;
    if index.first() != Some(&0) {
        bail!("the xz index does not begin with its indicator");
    }
    i += 1;
    let count = varint(index, &mut i)?;
    let mut out = Vec::with_capacity(count.min(1 << 20) as usize);
    for _ in 0..count {
        let unpadded = varint(index, &mut i)?;
        let uncompressed = varint(index, &mut i)?;
        if unpadded == 0 {
            bail!("an xz index record of no bytes");
        }
        out.push((unpadded, uncompressed));
    }
    Ok(out)
}

/// The multibyte integer xz writes: seven bits a byte, low first, the top bit
/// saying that another byte follows.
fn varint(b: &[u8], i: &mut usize) -> Result<u64> {
    let mut n = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*i).context("the xz index ends mid-number")?;
        *i += 1;
        n |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok(n);
        }
    }
    bail!("an xz index number longer than 64 bits")
}

/// `block`'s bytes decoded, as many as the index promised.
///
/// `xz` is given the stream header and the block and nothing else, so it
/// decodes what it was handed and then reports the end of input it never got:
/// the index is what says the output is complete, not the exit status.
pub fn decode(header: &[u8; HEADER], block: &[u8], out_len: usize) -> Result<Vec<u8>> {
    let mut child = Command::new("xz")
        .args(["-dc", "--single-stream"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                anyhow::anyhow!("reading an xz image needs `xz`, which is not installed")
            }
            _ => anyhow::anyhow!("running xz: {e}"),
        })?;
    // The decoded block is a megabyte and a pipe holds far less, so the
    // writing goes on its own thread: feeding the whole block before reading a
    // byte of the output would deadlock as soon as xz filled its stdout.
    let mut stdin = child.stdin.take().expect("piped");
    let mut payload = Vec::with_capacity(HEADER + block.len());
    payload.extend_from_slice(header);
    payload.extend_from_slice(block);
    let writer = std::thread::spawn(move || stdin.write_all(&payload));
    let out = child.wait_with_output().context("running xz")?;
    // A write that failed is the decoder having stopped early, and its own
    // message says why — so what is checked is the length of the output, not
    // the write and not the exit status: xz always reports the end of input it
    // never got.
    let _ = writer
        .join()
        .map_err(|_| anyhow::anyhow!("the thread feeding xz panicked"))?;
    if out.stdout.len() != out_len {
        bail!(
            "xz decoded {} bytes of a block the index says is {out_len}: {}",
            out.stdout.len(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream of `mib` MiB of pseudo-random bytes in 1 MiB blocks, and what
    /// it decodes to. `None` without `xz`, which not every builder has.
    fn stream(name: &str, mib: usize) -> Option<(Vec<u8>, Vec<u8>)> {
        let mut raw = Vec::with_capacity(mib << 20);
        let mut x = 0x1234_5678u32;
        for _ in 0..(mib << 20) {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            raw.push((x >> 16) as u8);
        }
        let dir = std::env::temp_dir().join(format!("pimu-xz-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("image");
        std::fs::write(&path, &raw).unwrap();
        let made = Command::new("xz")
            .args(["-0", "--block-size=1MiB", "-T2", "-k"])
            .arg(&path)
            .status();
        match made {
            Ok(s) if s.success() => {}
            Ok(s) => panic!("xz failed: {s}"),
            Err(_) => {
                eprintln!("xz not installed; skipping");
                let _ = std::fs::remove_dir_all(&dir);
                return None;
            }
        }
        let xz = std::fs::read(dir.join("image.xz")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        Some((raw, xz))
    }

    fn reader(xz: &[u8]) -> impl Fn(u64, usize) -> Result<Vec<u8>> + '_ {
        move |at, len| Ok(xz[at as usize..at as usize + len].to_vec())
    }

    #[test]
    fn the_index_says_where_every_block_starts_and_what_it_decodes_to() {
        let Some((raw, xz)) = stream("index", 3) else {
            return;
        };
        let index = index(xz.len() as u64, &reader(&xz)).unwrap();
        assert!(index.seekable(), "3 MiB in 1 MiB blocks is three blocks");
        assert_eq!(index.out_len, raw.len() as u64);
        assert_eq!(index.blocks.len(), 3);
        for (i, block) in index.blocks.iter().enumerate() {
            assert_eq!(block.out_at, (i as u64) << 20);
            assert_eq!(block.out_len, 1 << 20);
        }
    }

    #[test]
    fn a_block_decodes_on_its_own_behind_the_stream_header() {
        let Some((raw, xz)) = stream("decode", 3) else {
            return;
        };
        let index = index(xz.len() as u64, &reader(&xz)).unwrap();
        let block = *index.block_at(1 << 20 | 0x1234).unwrap();
        assert_eq!(block.out_at, 1 << 20);
        let at = block.at as usize;
        let bytes = decode(
            &index.header,
            &xz[at..at + block.len as usize],
            block.out_len as usize,
        )
        .unwrap();
        assert_eq!(bytes, raw[1 << 20..2 << 20]);
    }

    #[test]
    fn a_byte_past_the_end_is_in_no_block() {
        let Some((_, xz)) = stream("past", 2) else {
            return;
        };
        let index = index(xz.len() as u64, &reader(&xz)).unwrap();
        assert!(index.block_at(index.out_len).is_none());
        assert!(index.block_at(index.out_len - 1).is_some());
    }

    #[test]
    fn anything_else_is_not_a_stream() {
        let not = vec![0u8; 64];
        let e = index(not.len() as u64, &reader(&not)).unwrap_err();
        assert!(e.to_string().contains("magic"), "{e:#}");
        let e = index(4, &reader(&not)).unwrap_err();
        assert!(e.to_string().contains("too short"), "{e:#}");
    }
}
