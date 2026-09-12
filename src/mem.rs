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

    pub fn load(&self, addr: u32, width: Width) -> BusResult<u32> {
        let n = width.bytes() as usize;
        let off = self.offset(addr, n).map_err(|_| BusError::Unmapped {
            addr,
            width,
            write: false,
        })?;
        let mut buf = [0u8; 4];
        buf[..n].copy_from_slice(&self.data[off..off + n]);
        Ok(u32::from_le_bytes(buf))
    }

    pub fn store(&mut self, addr: u32, width: Width, value: u32) -> BusResult<()> {
        let n = width.bytes() as usize;
        let off = self.offset(addr, n).map_err(|_| BusError::Unmapped {
            addr,
            width,
            write: true,
        })?;
        self.data[off..off + n].copy_from_slice(&value.to_le_bytes()[..n]);
        self.wrote(off, n);
        Ok(())
    }
}
