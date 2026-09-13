//! Plain read/write RAM region.

use crate::bus::{BusError, BusResult, Width};

/// Pages are 4 KiB for the purpose of [`Ram::page_gen`].
const PAGE_SHIFT: u32 = 12;

/// A contiguous block of little-endian RAM mapped at `base`.
pub struct Ram {
    base: u32,
    data: Vec<u8>,
    /// A write generation per 4 KiB page, bumped by every store into it.
    ///
    /// This is what lets the VPU cache decoded instructions (#43): a cached
    /// instruction is still good while its page's generation is the one it
    /// was decoded under. `data` is private and every writer — CPU stores on
    /// either side, DMA, the loaders — goes through [`Ram::store`] or
    /// [`Ram::write_slice`], so no write can get past it. 64 bits, so a page
    /// can never wrap back to a generation a stale entry still holds.
    gens: Vec<u64>,
}

impl Ram {
    pub fn new(base: u32, size: usize) -> Ram {
        Ram {
            base,
            data: vec![0; size],
            gens: vec![0; size.div_ceil(1 << PAGE_SHIFT)],
        }
    }

    /// The write generation of the page holding `addr`, or `None` outside RAM.
    #[inline]
    pub fn page_gen(&self, addr: u32) -> Option<u64> {
        let rel = addr.checked_sub(self.base)? as usize;
        self.gens.get(rel >> PAGE_SHIFT).copied()
    }

    /// Bump the generation of every page `len > 0` bytes at `off` touch.
    #[inline]
    fn wrote(&mut self, off: usize, len: usize) {
        let first = off >> PAGE_SHIFT;
        let last = (off + len - 1) >> PAGE_SHIFT;
        for g in &mut self.gens[first..=last] {
            *g += 1;
        }
    }

    pub fn base(&self) -> u32 {
        self.base
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Every byte, from `base` on — for dumping.
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    #[inline]
    pub fn contains(&self, addr: u32) -> bool {
        let end = self.base as u64 + self.data.len() as u64;
        (addr as u64) >= self.base as u64 && (addr as u64) < end
    }

    /// Bulk-load bytes at an absolute address (used by firmware/ELF loaders).
    pub fn write_slice(&mut self, addr: u32, bytes: &[u8]) -> BusResult<()> {
        let off = self.offset(addr, bytes.len())?;
        self.data[off..off + bytes.len()].copy_from_slice(bytes);
        if !bytes.is_empty() {
            self.wrote(off, bytes.len());
        }
        Ok(())
    }

    pub fn read_slice(&self, addr: u32, len: usize) -> BusResult<&[u8]> {
        let off = self.offset(addr, len)?;
        Ok(&self.data[off..off + len])
    }

    #[inline]
    fn offset(&self, addr: u32, len: usize) -> BusResult<usize> {
        let rel = (addr as u64).checked_sub(self.base as u64);
        match rel {
            Some(r)
                if (r as usize)
                    .checked_add(len)
                    .is_some_and(|e| e <= self.data.len()) =>
            {
                Ok(r as usize)
            }
            _ => Err(BusError::Unmapped {
                addr,
                width: Width::Byte,
                write: false,
            }),
        }
    }

    /// The `N` bytes at `addr`, if they are all in RAM.
    #[inline]
    fn bytes<const N: usize>(&self, addr: u32) -> Option<[u8; N]> {
        let rel = addr.checked_sub(self.base)? as usize;
        self.data.get(rel..rel.checked_add(N)?)?.try_into().ok()
    }

    #[inline]
    fn bytes_mut<const N: usize>(&mut self, addr: u32) -> Option<(usize, &mut [u8; N])> {
        let rel = addr.checked_sub(self.base)? as usize;
        let b = self
            .data
            .get_mut(rel..rel.checked_add(N)?)?
            .try_into()
            .ok()?;
        Some((rel, b))
    }

    // One arm per width, so that each access is a plain load or store
    // rather than a copy of a run-time length.
    #[inline]
    pub fn load(&self, addr: u32, width: Width) -> BusResult<u32> {
        let v = match width {
            Width::Byte => self.bytes::<1>(addr).map(|b| u32::from(b[0])),
            Width::Half => self
                .bytes::<2>(addr)
                .map(|b| u32::from(u16::from_le_bytes(b))),
            Width::Word => self.bytes::<4>(addr).map(u32::from_le_bytes),
        };
        v.ok_or(BusError::Unmapped {
            addr,
            width,
            write: false,
        })
    }

    #[inline]
    pub fn store(&mut self, addr: u32, width: Width, value: u32) -> BusResult<()> {
        let unmapped = BusError::Unmapped {
            addr,
            width,
            write: true,
        };
        let (off, n) = match width {
            Width::Byte => {
                let (off, b) = self.bytes_mut::<1>(addr).ok_or(unmapped)?;
                *b = [value as u8];
                (off, 1)
            }
            Width::Half => {
                let (off, b) = self.bytes_mut::<2>(addr).ok_or(unmapped)?;
                *b = (value as u16).to_le_bytes();
                (off, 2)
            }
            Width::Word => {
                let (off, b) = self.bytes_mut::<4>(addr).ok_or(unmapped)?;
                *b = value.to_le_bytes();
                (off, 4)
            }
        };
        // At most two pages, and nearly always one.
        let first = off >> PAGE_SHIFT;
        let last = (off + n - 1) >> PAGE_SHIFT;
        self.gens[first] += 1;
        if last != first {
            self.gens[last] += 1;
        }
        Ok(())
    }
}
