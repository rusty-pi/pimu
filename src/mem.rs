//! Plain read/write RAM region.

use crate::bus::{BusError, BusResult, Width};
use alloc::vec::Vec;

/// A contiguous block of little-endian RAM mapped at `base`.
pub struct Ram {
    base: u32,
    data: Vec<u8>,
}

impl Ram {
    pub fn new(base: u32, size: usize) -> Ram {
        Ram {
            base,
            data: vec![0; size],
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
        Ok(())
    }
}
