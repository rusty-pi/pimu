//! Plain read/write RAM region.

use crate::bus::{BusError, BusResult, Width};

/// A contiguous block of little-endian RAM mapped at `base`.
///
/// The backing store is either owned (`Ram::new`, a zeroed `Vec`) or borrowed
/// from whoever really owns the machine's memory (`Ram::over_raw`). The second
/// form exists for the QEMU frontend: there the VideoCore model has to run over
/// the *guest's* RAM — the same bytes the ARM cores and QEMU's DMA-capable
/// devices see — so the region is QEMU's `MemoryRegion` host pointer, not an
/// allocation of ours. Everything above this type only ever sees `load`,
/// `store` and the slice helpers, so the two cases are indistinguishable to it.
pub struct Ram {
    base: u32,
    ptr: *mut u8,
    len: usize,
    /// Keeps an owned allocation alive; `None` when the memory is borrowed.
    _owner: Option<Vec<u8>>,
}

// The raw pointer is either our own `Vec` or a region the caller guarantees
// outlives the `Ram` (see `over_raw`). Neither is tied to a thread.
unsafe impl Send for Ram {}

impl Ram {
    pub fn new(base: u32, size: usize) -> Ram {
        let mut data = vec![0u8; size];
        let ptr = data.as_mut_ptr();
        Ram {
            base,
            ptr,
            len: size,
            _owner: Some(data),
        }
    }

    /// Build a `Ram` over memory the caller owns.
    ///
    /// # Safety
    /// `ptr` must be valid for reads and writes of `len` bytes for as long as
    /// the returned `Ram` (and anything holding it) lives, and nothing else may
    /// hand out Rust references into that range while it does. Concurrent
    /// access from other bus masters (the ARM, device DMA) is the situation
    /// this exists for; every access here is a plain byte copy, never a
    /// long-lived borrow, which is the same contract guest RAM already has.
    pub unsafe fn over_raw(base: u32, ptr: *mut u8, len: usize) -> Ram {
        assert!(!ptr.is_null(), "Ram::over_raw: null backing pointer");
        Ram {
            base,
            ptr,
            len,
            _owner: None,
        }
    }

    pub fn base(&self) -> u32 {
        self.base
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn contains(&self, addr: u32) -> bool {
        let end = self.base as u64 + self.len as u64;
        (addr as u64) >= self.base as u64 && (addr as u64) < end
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: `ptr`/`len` describe memory valid for the life of `self`
        // (owned `Vec`, or the `over_raw` contract).
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `bytes`, and `&mut self` guarantees no other borrow of
        // ours is live.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    pub fn write_slice(&mut self, addr: u32, bytes: &[u8]) -> BusResult<()> {
        let off = self.offset(addr, bytes.len())?;
        self.bytes_mut()[off..off + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    pub fn read_slice(&self, addr: u32, len: usize) -> BusResult<&[u8]> {
        let off = self.offset(addr, len)?;
        Ok(&self.bytes()[off..off + len])
    }

    fn offset(&self, addr: u32, len: usize) -> BusResult<usize> {
        let rel = (addr as u64).checked_sub(self.base as u64);
        match rel {
            Some(r) if (r as usize).checked_add(len).is_some_and(|e| e <= self.len) => {
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
        buf[..n].copy_from_slice(&self.bytes()[off..off + n]);
        Ok(u32::from_le_bytes(buf))
    }

    pub fn store(&mut self, addr: u32, width: Width, value: u32) -> BusResult<()> {
        let n = width.bytes() as usize;
        let off = self.offset(addr, n).map_err(|_| BusError::Unmapped {
            addr,
            width,
            write: true,
        })?;
        self.bytes_mut()[off..off + n].copy_from_slice(&value.to_le_bytes()[..n]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_borrowed_region_reads_and_writes_the_callers_bytes() {
        let mut backing = vec![0u8; 64];
        let ptr = backing.as_mut_ptr();
        {
            let mut ram = unsafe { Ram::over_raw(0x1000, ptr, backing.len()) };
            ram.store(0x1004, Width::Word, 0xdead_beef).unwrap();
            assert_eq!(ram.load(0x1006, Width::Half).unwrap(), 0xdead);
            assert!(ram.load(0x1040, Width::Byte).is_err());
        }
        assert_eq!(&backing[4..8], &0xdead_beefu32.to_le_bytes());
    }
}
