//! Plain read/write RAM region.
//!
//! The bytes are kept in 1 MiB chunks, and a chunk is only allocated when
//! something other than zeros is first written into it: a board takes as much
//! host memory as its guest has used, the way an untouched `calloc` does on a
//! host that maps pages lazily. A WebAssembly allocator zeroes what it hands
//! out, and a 32-bit address space has no room for an 8 GiB board, so the
//! browser build depends on it.

use std::alloc::Layout;
use std::borrow::Cow;
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::bus::{BusError, BusResult, Width};

const PAGE_SHIFT: u32 = 12;
const CHUNK_SHIFT: u32 = 20;
const CHUNK: usize = 1 << CHUNK_SHIFT;
const CHUNK_MASK: u64 = CHUNK as u64 - 1;
const PAGES: usize = CHUNK >> PAGE_SHIFT;

struct Chunk {
    bytes: [u8; CHUNK],
    /// A write generation per 4 KiB page, bumped by every store into it: what
    /// lets the VPU cache decoded instructions. A page of a chunk not yet
    /// allocated is at generation 0 and a new chunk starts its pages at 1, so
    /// allocating one counts as a write.
    gens: [u64; PAGES],
}

impl Chunk {
    const LAYOUT: Layout = Layout::new::<Chunk>();

    fn allocate() -> *mut Chunk {
        let chunk = unsafe { std::alloc::alloc_zeroed(Self::LAYOUT) }.cast::<Chunk>();
        if chunk.is_null() {
            refused();
        }
        unsafe { (*chunk).gens = [1; PAGES] };
        chunk
    }

    /// # Safety
    /// `chunk` came from [`Chunk::allocate`] and nothing uses it any more.
    unsafe fn free(chunk: *mut Chunk) {
        unsafe { std::alloc::dealloc(chunk.cast(), Self::LAYOUT) }
    }

    #[inline]
    fn byte(chunk: *mut Chunk, at: usize) -> *mut u8 {
        unsafe { ptr::addr_of_mut!((*chunk).bytes).cast::<u8>().add(at) }
    }

    #[inline]
    fn bump(chunk: *mut Chunk, at: usize, len: usize) {
        let gens = unsafe { &mut *ptr::addr_of_mut!((*chunk).gens) };
        for g in &mut gens[at >> PAGE_SHIFT..=(at + len - 1) >> PAGE_SHIFT] {
            *g += 1;
        }
    }
}

#[cold]
fn refused() -> ! {
    panic!(
        "the host gave no more memory for the board's RAM ({} KiB more asked for)",
        CHUNK >> 10
    )
}

/// The chunk holding `off`, allocated on the way if `slots` has none for it
/// yet; another thread allocating the same one at the same time is fine.
#[cold]
fn install(slots: &[AtomicPtr<Chunk>], off: u64) -> *mut Chunk {
    let slot = &slots[(off >> CHUNK_SHIFT) as usize];
    let chunk = Chunk::allocate();
    match slot.compare_exchange(ptr::null_mut(), chunk, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => chunk,
        Err(theirs) => {
            unsafe { Chunk::free(chunk) };
            theirs
        }
    }
}

#[inline]
fn chunk_at(slots: &[AtomicPtr<Chunk>], off: u64) -> *mut Chunk {
    slots[(off >> CHUNK_SHIFT) as usize].load(Ordering::Acquire)
}

#[inline]
fn within_chunk(off: u64, len: usize) -> bool {
    (off & CHUNK_MASK) as usize + len <= CHUNK
}

/// What [`Ram`] holds, chunk by chunk, and what [`Ram::take_contents`] hands
/// over to the RAM of the next boot.
pub struct Contents {
    slots: Box<[AtomicPtr<Chunk>]>,
    len: u64,
}

impl Contents {
    fn new(len: u64) -> Contents {
        let chunks = usize::try_from(len.div_ceil(CHUNK as u64))
            .unwrap_or_else(|_| panic!("{len} bytes of RAM do not fit this host"));
        Contents {
            slots: (0..chunks).map(|_| AtomicPtr::default()).collect(),
            len,
        }
    }
}

impl Drop for Contents {
    fn drop(&mut self) {
        for slot in self.slots.iter_mut() {
            let chunk = *slot.get_mut();
            if !chunk.is_null() {
                unsafe { Chunk::free(chunk) };
            }
        }
    }
}

/// A stretch of RAM, as [`Ram::regions`] walks it.
pub enum Region<'a> {
    Bytes(&'a [u8]),
    /// This many bytes nothing but zeros has ever been written to.
    Zeros(usize),
}

impl Region<'_> {
    pub fn len(&self) -> usize {
        match self {
            Region::Bytes(b) => b.len(),
            Region::Zeros(n) => *n,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// RAM as cores on other threads see it: every access is atomic, so a core
/// reading what another is writing gets one of the two values, never a mix.
///
/// A handle borrows nothing, so the [`Ram`] it came from has to outlive every
/// copy and must not swap its contents meanwhile; the parallel ARM run
/// guarantees both by joining its threads before the machine goes.
#[derive(Clone, Copy)]
pub struct SharedRam {
    slots: *const AtomicPtr<Chunk>,
    chunks: usize,
    len: u64,
}

unsafe impl Send for SharedRam {}
unsafe impl Sync for SharedRam {}

impl SharedRam {
    #[inline]
    fn slots(&self) -> &[AtomicPtr<Chunk>] {
        unsafe { std::slice::from_raw_parts(self.slots, self.chunks) }
    }

    #[inline]
    fn fits(&self, off: u64, size: u32) -> bool {
        off.checked_add(u64::from(size))
            .is_some_and(|end| end <= self.len)
    }

    /// `size` bytes (1, 2, 4 or 8) at `off`, little-endian. Naturally aligned
    /// accesses are single atomic operations; the rest go a byte at a time.
    #[inline]
    pub fn load(&self, off: u64, size: u32) -> Option<u64> {
        if !self.fits(off, size) {
            return None;
        }
        if !within_chunk(off, size as usize) {
            return (0..u64::from(size))
                .try_fold(0, |v, i| Some(v | self.load(off + i, 1)? << (8 * i)));
        }
        let chunk = chunk_at(self.slots(), off);
        if chunk.is_null() {
            return Some(0);
        }
        let p = Chunk::byte(chunk, (off & CHUNK_MASK) as usize);
        Some(unsafe {
            match size {
                1 => u64::from((*(p as *const AtomicU8)).load(Ordering::Acquire)),
                2 if p as usize & 1 == 0 => {
                    u64::from(AtomicU16::from_ptr(p.cast()).load(Ordering::Acquire))
                }
                4 if p as usize & 3 == 0 => {
                    u64::from(AtomicU32::from_ptr(p.cast()).load(Ordering::Acquire))
                }
                8 if p as usize & 7 == 0 => AtomicU64::from_ptr(p.cast()).load(Ordering::Acquire),
                _ => (0..size as usize).fold(0, |v, i| {
                    v | u64::from((*(p.add(i) as *const AtomicU8)).load(Ordering::Relaxed))
                        << (8 * i)
                }),
            }
        })
    }

    /// The store matching [`Self::load`].
    #[inline]
    pub fn store(&self, off: u64, size: u32, value: u64) -> Option<()> {
        if !self.fits(off, size) {
            return None;
        }
        if !within_chunk(off, size as usize) {
            for i in 0..u64::from(size) {
                self.store(off + i, 1, value >> (8 * i))?;
            }
            return Some(());
        }
        let mut chunk = chunk_at(self.slots(), off);
        if chunk.is_null() {
            if value & (u64::MAX >> (64 - 8 * size)) == 0 {
                return Some(());
            }
            chunk = install(self.slots(), off);
        }
        let p = Chunk::byte(chunk, (off & CHUNK_MASK) as usize);
        unsafe {
            match size {
                1 => (*(p as *const AtomicU8)).store(value as u8, Ordering::Release),
                2 if p as usize & 1 == 0 => {
                    AtomicU16::from_ptr(p.cast()).store(value as u16, Ordering::Release)
                }
                4 if p as usize & 3 == 0 => {
                    AtomicU32::from_ptr(p.cast()).store(value as u32, Ordering::Release)
                }
                8 if p as usize & 7 == 0 => {
                    AtomicU64::from_ptr(p.cast()).store(value, Ordering::Release)
                }
                _ => {
                    for i in 0..size as usize {
                        (*(p.add(i) as *const AtomicU8))
                            .store((value >> (8 * i)) as u8, Ordering::Relaxed)
                    }
                }
            }
        }
        Some(())
    }

    /// Store `new` if the location still holds `old`: whether it did. `None`
    /// if the access is out of range or not naturally aligned, which an
    /// exclusive access never is.
    #[inline]
    pub fn compare_exchange(&self, off: u64, size: u32, old: u64, new: u64) -> Option<bool> {
        if !self.fits(off, size) || off & (u64::from(size) - 1) != 0 {
            return None;
        }
        let mut chunk = chunk_at(self.slots(), off);
        if chunk.is_null() {
            if old != 0 {
                return Some(false);
            }
            if new == 0 {
                return Some(true);
            }
            chunk = install(self.slots(), off);
        }
        let p = Chunk::byte(chunk, (off & CHUNK_MASK) as usize);
        let (a, r) = (Ordering::AcqRel, Ordering::Acquire);
        Some(unsafe {
            match size {
                1 => (*(p as *const AtomicU8))
                    .compare_exchange(old as u8, new as u8, a, r)
                    .is_ok(),
                2 => AtomicU16::from_ptr(p.cast())
                    .compare_exchange(old as u16, new as u16, a, r)
                    .is_ok(),
                4 => AtomicU32::from_ptr(p.cast())
                    .compare_exchange(old as u32, new as u32, a, r)
                    .is_ok(),
                8 => AtomicU64::from_ptr(p.cast())
                    .compare_exchange(old, new, a, r)
                    .is_ok(),
                _ => return None,
            }
        })
    }
}

pub struct Ram {
    base: u32,
    /// Every writer goes through [`Ram::store`], [`Ram::store_at`] or
    /// [`Ram::write_slice`], so none can get past the write generations.
    contents: Contents,
    /// Cache-coherency tracking (`--check-coherency`); off unless asked for.
    pub coherency: crate::coherency::Coherency,
}

impl Ram {
    pub fn new(base: u32, size: u64) -> Ram {
        Ram {
            base,
            contents: Contents::new(size),
            coherency: crate::coherency::Coherency::off(),
        }
    }

    /// What the DRAM holds, to carry across a reset: the chips keep their
    /// contents through a watchdog reset, and the bootloader that runs next
    /// overwrites only what it uses (checked on a Raspberry Pi 4B d03115,
    /// where 1 MiB written in the VPU's memory and 1 MiB in the ARM's came
    /// back intact after the bootloader had started the firmware again).
    pub fn take_contents(&mut self) -> Contents {
        std::mem::replace(&mut self.contents, Contents::new(0))
    }

    /// Put back what [`Ram::take_contents`] took, into a RAM of the same size.
    pub fn restore_contents(&mut self, contents: Contents) {
        assert_eq!(
            contents.len, self.contents.len,
            "the DRAM keeps its size across a reset"
        );
        self.contents = contents;
    }

    #[inline]
    fn slots(&self) -> &[AtomicPtr<Chunk>] {
        &self.contents.slots
    }

    #[inline]
    fn fits(&self, off: u64, len: usize) -> bool {
        off.checked_add(len as u64)
            .is_some_and(|end| end <= self.contents.len)
    }

    /// The write generation of the page holding `addr`, or `None` outside RAM.
    #[inline]
    pub fn page_gen(&self, addr: u32) -> Option<u64> {
        let off = u64::from(addr.checked_sub(self.base)?);
        if off >= self.contents.len {
            return None;
        }
        let chunk = chunk_at(self.slots(), off);
        if chunk.is_null() {
            return Some(0);
        }
        Some(unsafe { (*chunk).gens[(off & CHUNK_MASK) as usize >> PAGE_SHIFT] })
    }

    pub fn base(&self) -> u32 {
        self.base
    }

    /// The same bytes, for cores on other threads. See [`SharedRam`] for what
    /// the caller promises.
    pub fn share(&mut self) -> SharedRam {
        SharedRam {
            slots: self.contents.slots.as_ptr(),
            chunks: self.contents.slots.len(),
            len: self.contents.len,
        }
    }

    pub fn len(&self) -> u64 {
        self.contents.len
    }

    pub fn is_empty(&self) -> bool {
        self.contents.len == 0
    }

    /// The whole RAM from offset 0, a chunk at a time.
    pub fn regions(&self) -> impl Iterator<Item = (u64, Region<'_>)> {
        let len = self.contents.len;
        self.slots().iter().enumerate().map(move |(i, slot)| {
            let off = (i as u64) << CHUNK_SHIFT;
            let n = (len - off).min(CHUNK as u64) as usize;
            let chunk = slot.load(Ordering::Acquire);
            let region = if chunk.is_null() {
                Region::Zeros(n)
            } else {
                Region::Bytes(unsafe { std::slice::from_raw_parts(Chunk::byte(chunk, 0), n) })
            };
            (off, region)
        })
    }

    #[inline]
    pub fn contains(&self, addr: u32) -> bool {
        addr >= self.base && u64::from(addr - self.base) < self.contents.len
    }

    pub fn write_slice(&mut self, addr: u32, bytes: &[u8]) -> BusResult<()> {
        let off = self.offset(addr, bytes.len())?;
        let mut done = 0;
        while done < bytes.len() {
            let at = off + done as u64;
            let n = (CHUNK - (at & CHUNK_MASK) as usize).min(bytes.len() - done);
            let piece = &bytes[done..done + n];
            let mut chunk = chunk_at(self.slots(), at);
            if chunk.is_null() && piece.iter().any(|&b| b != 0) {
                chunk = install(self.slots(), at);
            }
            if !chunk.is_null() {
                let at = (at & CHUNK_MASK) as usize;
                unsafe { ptr::copy_nonoverlapping(piece.as_ptr(), Chunk::byte(chunk, at), n) };
                Chunk::bump(chunk, at, n);
            }
            done += n;
        }
        if !bytes.is_empty() {
            self.note_write(off, bytes.len());
        }
        Ok(())
    }

    /// `len` bytes from `addr`: in place when they sit in one chunk the guest
    /// has written to, else a copy.
    pub fn read_slice(&self, addr: u32, len: usize) -> BusResult<Cow<'_, [u8]>> {
        let off = self.offset(addr, len)?;
        self.note_read(off, len);
        if within_chunk(off, len) {
            let chunk = chunk_at(self.slots(), off);
            if chunk.is_null() {
                return Ok(Cow::Owned(vec![0; len]));
            }
            let p = Chunk::byte(chunk, (off & CHUNK_MASK) as usize);
            return Ok(Cow::Borrowed(unsafe { std::slice::from_raw_parts(p, len) }));
        }
        let mut out = vec![0; len];
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.bytes::<1>(off + i as u64).map_or(0, |b| b[0]);
        }
        Ok(Cow::Owned(out))
    }

    #[inline]
    fn offset(&self, addr: u32, len: usize) -> BusResult<u64> {
        match addr.checked_sub(self.base).map(u64::from) {
            Some(off) if self.fits(off, len) => Ok(off),
            _ => Err(BusError::Unmapped {
                addr,
                width: Width::Byte,
                write: false,
            }),
        }
    }

    /// The `N` bytes `off` bytes into RAM.
    #[inline]
    fn bytes<const N: usize>(&self, off: u64) -> Option<[u8; N]> {
        if !self.fits(off, N) {
            return None;
        }
        if !within_chunk(off, N) {
            return Some(self.bytes_across(off));
        }
        let chunk = chunk_at(self.slots(), off);
        if chunk.is_null() {
            return Some([0; N]);
        }
        let p = Chunk::byte(chunk, (off & CHUNK_MASK) as usize);
        Some(unsafe { ptr::read_unaligned(p.cast::<[u8; N]>()) })
    }

    #[cold]
    fn bytes_across<const N: usize>(&self, off: u64) -> [u8; N] {
        std::array::from_fn(|i| self.bytes::<1>(off + i as u64).map_or(0, |b| b[0]))
    }

    /// Write `N` bytes `off` bytes into RAM, which [`Self::fits`] has checked.
    #[inline]
    fn put<const N: usize>(&mut self, off: u64, bytes: [u8; N]) {
        if !within_chunk(off, N) {
            return self.put_across(off, &bytes);
        }
        let mut chunk = chunk_at(self.slots(), off);
        if chunk.is_null() {
            if bytes == [0; N] {
                return;
            }
            chunk = install(self.slots(), off);
        }
        let at = (off & CHUNK_MASK) as usize;
        unsafe { ptr::write_unaligned(Chunk::byte(chunk, at).cast::<[u8; N]>(), bytes) };
        Chunk::bump(chunk, at, N);
    }

    #[cold]
    fn put_across(&mut self, off: u64, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            self.put(off + i as u64, [b]);
        }
    }

    /// One value `off` bytes into RAM, for a bus that addresses memory by
    /// offset rather than through a 32-bit window (the A72); an error carries
    /// the offset's low 32 bits, all [`BusError`] has room for.
    #[inline]
    pub fn load_at(&self, off: u64, width: Width) -> BusResult<u32> {
        self.note_read(off, width.bytes() as usize);
        let v = match width {
            Width::Byte => self.bytes::<1>(off).map(|b| u32::from(b[0])),
            Width::Half => self
                .bytes::<2>(off)
                .map(|b| u32::from(u16::from_le_bytes(b))),
            Width::Word => self.bytes::<4>(off).map(u32::from_le_bytes),
        };
        v.ok_or(BusError::Unmapped {
            addr: off as u32,
            width,
            write: false,
        })
    }

    // One arm per width, so each access is a plain load or store.
    #[inline]
    pub fn store_at(&mut self, off: u64, width: Width, value: u32) -> BusResult<()> {
        let n = width.bytes() as usize;
        if !self.fits(off, n) {
            return Err(BusError::Unmapped {
                addr: off as u32,
                width,
                write: true,
            });
        }
        match width {
            Width::Byte => self.put(off, [value as u8]),
            Width::Half => self.put(off, (value as u16).to_le_bytes()),
            Width::Word => self.put(off, value.to_le_bytes()),
        }
        self.note_write(off, n);
        Ok(())
    }

    #[inline]
    pub fn load(&self, addr: u32, width: Width) -> BusResult<u32> {
        let unmapped = || BusError::Unmapped {
            addr,
            width,
            write: false,
        };
        let off = addr.checked_sub(self.base).ok_or_else(unmapped)?;
        self.load_at(u64::from(off), width).map_err(|_| unmapped())
    }

    #[inline]
    pub fn store(&mut self, addr: u32, width: Width, value: u32) -> BusResult<()> {
        let unmapped = || BusError::Unmapped {
            addr,
            width,
            write: true,
        };
        let off = addr.checked_sub(self.base).ok_or_else(unmapped)?;
        self.store_at(u64::from(off), width, value)
            .map_err(|_| unmapped())
    }

    /// A write that did not come from the VPU leaves the caches out of date.
    #[inline]
    fn note_write(&mut self, off: u64, len: usize) {
        if !self.coherency.is_on() {
            return;
        }
        let at = self.base.wrapping_add(off as u32);
        match self.coherency.writer() {
            crate::coherency::Master::Vpu => {}
            crate::coherency::Master::Vc4Dma(_) => self.coherency.wrote_through_l2(at, len as u32),
            _ => self.coherency.wrote_by_other(at, len as u32),
        }
    }

    /// A read that did not come from the VPU sees memory, not the cache. The
    /// VPU's own reads are the bus's to mark: it knows which alias they used.
    #[inline]
    fn note_read(&self, off: u64, len: usize) {
        if !self.coherency.is_on() || self.coherency.reader().is_coherent() {
            return;
        }
        self.coherency.read_by(
            self.base.wrapping_add(off as u32),
            len as u32,
            self.coherency.reader(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Ram {
        fn resident_bytes(&self) -> usize {
            self.regions()
                .filter(|(_, r)| matches!(r, Region::Bytes(_)))
                .count()
                * CHUNK
        }
    }

    #[test]
    fn contents_carry_across_a_reset() {
        let mut before = Ram::new(0, 8192);
        before.write_slice(0x1010, b"kept").unwrap();
        let mut after = Ram::new(0, 8192);
        after.restore_contents(before.take_contents());
        assert_eq!(&*after.read_slice(0x1010, 4).unwrap(), b"kept");
        assert!(before.is_empty());
    }

    #[test]
    fn only_what_is_written_takes_host_memory() {
        let mut ram = Ram::new(0, 8 << 30);
        assert_eq!(ram.resident_bytes(), 0);
        assert_eq!(ram.load_at(0x1_8000_0000, Width::Word), Ok(0));
        ram.store_at(0x1_8000_0000, Width::Word, 0).unwrap();
        ram.write_slice(0x4000_0000, &[0; 3 << 20]).unwrap();
        assert_eq!(ram.resident_bytes(), 0, "zeros over zeros change nothing");
        ram.store_at(0x1_8000_0000, Width::Word, 0xDEAD_BEEF)
            .unwrap();
        assert_eq!(ram.load_at(0x1_8000_0000, Width::Word), Ok(0xDEAD_BEEF));
        assert_eq!(ram.resident_bytes(), 1 << 20);
        assert!(ram.load_at((8 << 30) - 2, Width::Word).is_err());
    }

    #[test]
    fn an_access_across_two_chunks_reaches_both() {
        let mut ram = Ram::new(0x100, 4 << 20);
        let edge = 0x100 + (1 << 20) - 2;
        ram.store(edge, Width::Word, 0x1122_3344).unwrap();
        assert_eq!(ram.load(edge, Width::Word), Ok(0x1122_3344));
        assert_eq!(ram.load(edge + 2, Width::Half), Ok(0x1122));
        ram.write_slice(edge - 1, b"abcdef").unwrap();
        assert_eq!(&*ram.read_slice(edge - 1, 6).unwrap(), b"abcdef");
        assert_eq!(ram.resident_bytes(), 2 << 20);
        let shared = ram.share();
        assert_eq!(shared.load(u64::from(edge - 0x100), 4), Some(0x6564_6362));
        shared
            .store((2 << 20) - 4, 8, 0x0102_0304_0506_0708)
            .unwrap();
        assert_eq!(shared.load((2 << 20) - 4, 8), Some(0x0102_0304_0506_0708));
        assert_eq!(ram.load_at((2 << 20) - 4, Width::Word), Ok(0x0506_0708));
    }

    #[test]
    fn a_page_written_for_the_first_time_moves_its_generation() {
        let mut ram = Ram::new(0, 2 << 20);
        let before = ram.page_gen(0x1000).unwrap();
        ram.store(0x1000, Width::Word, 0).unwrap();
        assert_eq!(ram.page_gen(0x1000), Some(before), "zeros change nothing");
        ram.store(0x1000, Width::Word, 1).unwrap();
        let after = ram.page_gen(0x1000).unwrap();
        assert_ne!(after, before);
        ram.store(0x1000, Width::Word, 0).unwrap();
        assert_ne!(ram.page_gen(0x1000), Some(after));
        let shared = ram.share();
        shared.store(0x10_1000, 4, 7).unwrap();
        assert_ne!(
            ram.page_gen(0x10_1000),
            Some(0),
            "nor through another thread"
        );
    }

    #[test]
    fn an_exclusive_store_into_untouched_memory_compares_against_zero() {
        let mut ram = Ram::new(0, 1 << 20);
        let shared = ram.share();
        assert_eq!(shared.compare_exchange(0x40, 8, 1, 2), Some(false));
        assert_eq!(shared.compare_exchange(0x40, 8, 0, 0), Some(true));
        assert_eq!(ram.resident_bytes(), 0);
        assert_eq!(shared.compare_exchange(0x40, 8, 0, 5), Some(true));
        assert_eq!(shared.load(0x40, 8), Some(5));
    }

    #[test]
    fn regions_walk_the_whole_ram() {
        let mut ram = Ram::new(0, (2 << 20) + 0x1000);
        ram.store_at(0x20_0010, Width::Byte, 9).unwrap();
        let regions: Vec<_> = ram.regions().collect();
        assert_eq!(regions.len(), 3);
        assert!(matches!(regions[0], (0, Region::Zeros(n)) if n == 1 << 20));
        assert!(
            matches!(&regions[2], (0x20_0000, Region::Bytes(b)) if b.len() == 0x1000 && b[0x10] == 9)
        );
    }
}
