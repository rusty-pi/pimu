//! Plain read/write RAM region.

use crate::bus::{BusError, BusResult, Width};
use alloc::vec::Vec;

/// A contiguous block of little-endian RAM mapped at `base`.
///
/// # Why the bytes are a raw pointer and not a slice
///
/// There are two frontends and they want different things from the backing
/// store:
///
///  * Hosted, RAM is an allocation and its address is nobody's business. The
///    `recon` reboot loop builds a fresh `Machine` per reset and the
///    regression harness builds one per scenario, so the block has to be
///    *freed* when the `Ram` is dropped — which is what `owner` is for.
///  * Bare-metal (#32 stage 4), the model's RAM has to *be* the physical
///    memory the firmware believes in. Linux's devices are QEMU's, they DMA
///    with no SMMU in the way, and stage-2 translation does not apply to a
///    device — so an address the firmware hands a peripheral is only writable
///    if the backing store sits at exactly that physical address. On a Pi that
///    address range starts at **physical zero**.
///
/// A Rust reference may not be null, and `&[u8]` at address 0 is not a
/// formality to wave through: built with `slice::from_raw_parts_mut(0, ..)`,
/// LLVM propagated `nonnull` backwards and deleted the whole model from the
/// bare-metal image — 276 KiB of `.text` down to 40 KiB, and a boot that
/// printed nothing. So `Ram` holds a pointer, hands out no references into the
/// region, and [`Ram::read_into`] copies out rather than borrowing.
///
/// The other single representation considered was `Box::leak`ing the hosted
/// allocation to make both cases `&'static mut [u8]`. That fails the first
/// bullet — every `Machine` becomes a permanent 1-2 GiB leak — as well as the
/// second.
pub struct Ram {
    base: u32,
    ptr: *mut u8,
    len: usize,
    /// Owns what `ptr` points at, when the RAM was allocated here rather than
    /// lent to us. Never touched again after construction — `ptr` aliases it,
    /// so anything that could reallocate or move it would dangle — which is
    /// also why it is dead to the compiler: its whole job is to be dropped.
    #[allow(dead_code)]
    owner: Option<Vec<u8>>,
}

// SAFETY: `Ram` behaves as the exclusive owner of `base .. base + len`. The
// pointer is either into its own `owner` allocation or into a region the
// frontend has promised nothing else touches, so moving one between threads is
// no different from moving the `Vec` it replaced.
unsafe impl Send for Ram {}

impl Ram {
    pub fn new(base: u32, size: usize) -> Ram {
        let mut owner = vec![0u8; size];
        let ptr = owner.as_mut_ptr();
        Ram {
            base,
            ptr,
            len: size,
            owner: Some(owner),
        }
    }

    /// RAM backed by memory the caller owns, rather than by an allocation.
    ///
    /// The region is zeroed, so that a model built this way comes up exactly
    /// as one built by [`Ram::new`] — which instructions the firmware retires
    /// must not depend on which constructor a frontend used, and a physical
    /// window has the bootloader's leavings all over it.
    ///
    /// # Safety
    /// `ptr .. ptr + len` must be readable and writable memory that stays
    /// valid, and that nothing else reads or writes, for as long as this `Ram`
    /// lives. For the bare-metal frontend that means: clear of the image, its
    /// stacks and its heap, with the bootloader's blobs copied out of it and
    /// no other CPU still executing in it.
    pub unsafe fn over_region(base: u32, ptr: *mut u8, len: usize) -> Ram {
        core::ptr::write_bytes(ptr, 0, len);
        Ram {
            base,
            ptr,
            len,
            owner: None,
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

    /// Bulk-load bytes at an absolute address (used by firmware/ELF loaders).
    pub fn write_slice(&mut self, addr: u32, bytes: &[u8]) -> BusResult<()> {
        let off = self.offset(addr, bytes.len())?;
        // SAFETY: `offset` has bounds-checked `off .. off + bytes.len()`, and
        // `bytes` is a separate object, so the two cannot overlap.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr.add(off), bytes.len()) };
        Ok(())
    }

    /// Copy `out.len()` bytes from `addr` into `out`.
    ///
    /// Copying rather than returning a `&[u8]` is what keeps a region based at
    /// physical zero expressible — see the type comment.
    pub fn read_into(&self, addr: u32, out: &mut [u8]) -> BusResult<()> {
        let off = self.offset(addr, out.len())?;
        // SAFETY: bounds-checked by `offset`; `out` is a separate object.
        unsafe { core::ptr::copy_nonoverlapping(self.ptr.add(off), out.as_mut_ptr(), out.len()) };
        Ok(())
    }

    /// Does `addr .. addr + len` hold anything but zeroes? `false` for a range
    /// that is out of bounds.
    ///
    /// A read-only question about a span too large to copy out (the hosted
    /// hand-off report walks the whole gigabyte a page at a time).
    pub fn any_nonzero(&self, addr: u32, len: usize) -> bool {
        let Ok(off) = self.offset(addr, len) else {
            return false;
        };
        (0..len).any(|i| {
            // SAFETY: bounds-checked by `offset`.
            unsafe { core::ptr::read(self.ptr.add(off + i)) != 0 }
        })
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
        // SAFETY: bounds-checked by `offset`, and `n <= 4 == buf.len()`.
        unsafe { core::ptr::copy_nonoverlapping(self.ptr.add(off), buf.as_mut_ptr(), n) };
        Ok(u32::from_le_bytes(buf))
    }

    pub fn store(&mut self, addr: u32, width: Width, value: u32) -> BusResult<()> {
        let n = width.bytes() as usize;
        let off = self.offset(addr, n).map_err(|_| BusError::Unmapped {
            addr,
            width,
            write: true,
        })?;
        let bytes = value.to_le_bytes();
        // SAFETY: bounds-checked by `offset`, and `n <= 4 == bytes.len()`.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr.add(off), n) };
        Ok(())
    }
}
