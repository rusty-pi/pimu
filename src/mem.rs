//! Plain read/write RAM region.
//!
//! Where the bytes are is a [`Store`]. A 64-bit host takes the whole RAM as one
//! zeroed allocation ([`Flat`]), whose pages the host only maps as they are
//! first used, so an 8 GiB board costs what its guest has touched. A 32-bit
//! host has no room for that, and a WebAssembly allocator zeroes everything it
//! hands out, so the browser build keeps RAM in 1 MiB [`Chunks`] that are only
//! allocated when something other than zeros is first written into them.

use std::alloc::Layout;
use std::borrow::Cow;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicPtr, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::bus::{BusError, BusResult, Width};

const PAGE_SHIFT: u32 = 12;
const CHUNK_SHIFT: u32 = 20;
const CHUNK: usize = 1 << CHUNK_SHIFT;
const CHUNK_MASK: u64 = CHUNK as u64 - 1;
const PAGES: usize = CHUNK >> PAGE_SHIFT;

#[cfg(target_pointer_width = "64")]
pub type DefaultStore = Flat;
#[cfg(not(target_pointer_width = "64"))]
pub type DefaultStore = Chunks;
pub type DefaultMap = <DefaultStore as Store>::Map;

/// Where a RAM's bytes are: the allocation, which the RAM owns.
pub trait Store: Sized {
    type Map: Map;
    fn new(len: u64) -> Self;
    fn map(&self) -> Self::Map;
}

/// Where a RAM's bytes are, as an access on any thread finds them. Every
/// offset handed in is inside the RAM.
pub trait Map: Copy + Send + Sync {
    /// Every byte exists from the start: [`Map::byte`] is never null and
    /// [`Map::room`] never ends.
    const FLAT: bool;
    /// How many bytes from `off` on are contiguous with it.
    fn room(self, off: u64) -> usize;
    /// The byte at `off`, or null where nothing but zeros has been written.
    fn byte(self, off: u64) -> *mut u8;
    /// The byte at `off`, allocated if it was not.
    fn byte_or_allocate(self, off: u64) -> *mut u8;
    /// The write generation of the page holding `off`, or null where nothing
    /// but zeros has been written.
    fn gen(self, off: u64) -> *mut u64;
}

#[cold]
fn refused() -> ! {
    panic!("the host gave no more memory for the board's RAM")
}

fn zeroed<T>(n: usize) -> *mut T {
    let layout = Layout::array::<T>(n).unwrap_or_else(|_| refused());
    if layout.size() == 0 {
        return NonNull::dangling().as_ptr();
    }
    let p = unsafe { std::alloc::alloc_zeroed(layout) }.cast::<T>();
    if p.is_null() {
        refused();
    }
    p
}

/// # Safety
/// `p` came from [`zeroed`] with the same `n`, and nothing uses it any more.
unsafe fn free<T>(p: *mut T, n: usize) {
    if n != 0 {
        unsafe { std::alloc::dealloc(p.cast(), Layout::array::<T>(n).unwrap()) }
    }
}

/// The whole RAM as one zeroed allocation, plus a write generation per page.
pub struct Flat {
    bytes: *mut u8,
    gens: *mut u64,
    len: usize,
    pages: usize,
}

unsafe impl Send for Flat {}
unsafe impl Sync for Flat {}

#[derive(Clone, Copy)]
pub struct FlatMap {
    bytes: *mut u8,
    gens: *mut u64,
}

unsafe impl Send for FlatMap {}
unsafe impl Sync for FlatMap {}

impl Store for Flat {
    type Map = FlatMap;

    fn new(len: u64) -> Flat {
        let len = usize::try_from(len).unwrap_or_else(|_| refused());
        let pages = len.div_ceil(1 << PAGE_SHIFT);
        Flat {
            bytes: zeroed(len),
            gens: zeroed(pages),
            len,
            pages,
        }
    }

    fn map(&self) -> FlatMap {
        FlatMap {
            bytes: self.bytes,
            gens: self.gens,
        }
    }
}

impl Drop for Flat {
    fn drop(&mut self) {
        unsafe {
            free(self.bytes, self.len);
            free(self.gens, self.pages);
        }
    }
}

impl Map for FlatMap {
    const FLAT: bool = true;

    #[inline]
    fn room(self, _: u64) -> usize {
        usize::MAX
    }

    #[inline]
    fn byte(self, off: u64) -> *mut u8 {
        unsafe { self.bytes.add(off as usize) }
    }

    #[inline]
    fn byte_or_allocate(self, off: u64) -> *mut u8 {
        self.byte(off)
    }

    #[inline]
    fn gen(self, off: u64) -> *mut u64 {
        unsafe { self.gens.add((off >> PAGE_SHIFT) as usize) }
    }
}

struct Chunk {
    bytes: [u8; CHUNK],
    /// A page of a chunk not yet allocated is at generation 0 and a new chunk
    /// starts its pages at 1, so allocating one counts as a write.
    gens: [u64; PAGES],
}

impl Chunk {
    fn allocate() -> *mut Chunk {
        let chunk = zeroed::<Chunk>(1);
        unsafe { (*chunk).gens = [1; PAGES] };
        chunk
    }
}

/// RAM in 1 MiB chunks, each allocated when something other than zeros is
/// first written into it.
pub struct Chunks {
    slots: Box<[AtomicPtr<Chunk>]>,
}

#[derive(Clone, Copy)]
pub struct ChunkMap {
    slots: *const AtomicPtr<Chunk>,
}

unsafe impl Send for ChunkMap {}
unsafe impl Sync for ChunkMap {}

impl Store for Chunks {
    type Map = ChunkMap;

    fn new(len: u64) -> Chunks {
        let chunks = usize::try_from(len.div_ceil(CHUNK as u64)).unwrap_or_else(|_| refused());
        Chunks {
            slots: (0..chunks).map(|_| AtomicPtr::default()).collect(),
        }
    }

    fn map(&self) -> ChunkMap {
        ChunkMap {
            slots: self.slots.as_ptr(),
        }
    }
}

impl Drop for Chunks {
    fn drop(&mut self) {
        for slot in self.slots.iter_mut() {
            let chunk = *slot.get_mut();
            if !chunk.is_null() {
                unsafe { free(chunk, 1) };
            }
        }
    }
}

impl ChunkMap {
    #[inline]
    fn slot(self, off: u64) -> *const AtomicPtr<Chunk> {
        unsafe { self.slots.add((off >> CHUNK_SHIFT) as usize) }
    }

    #[inline]
    fn chunk(self, off: u64) -> *mut Chunk {
        unsafe { (*self.slot(off)).load(Ordering::Acquire) }
    }

    /// Another thread allocating the same chunk at the same time is fine: one
    /// of the two is kept.
    #[cold]
    fn install(self, off: u64) -> *mut Chunk {
        let chunk = Chunk::allocate();
        match unsafe { &*self.slot(off) }.compare_exchange(
            ptr::null_mut(),
            chunk,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => chunk,
            Err(theirs) => {
                unsafe { free(chunk, 1) };
                theirs
            }
        }
    }

    #[inline]
    fn at(chunk: *mut Chunk, off: u64) -> *mut u8 {
        unsafe {
            ptr::addr_of_mut!((*chunk).bytes)
                .cast::<u8>()
                .add((off & CHUNK_MASK) as usize)
        }
    }
}

impl Map for ChunkMap {
    const FLAT: bool = false;

    #[inline]
    fn room(self, off: u64) -> usize {
        CHUNK - (off & CHUNK_MASK) as usize
    }

    #[inline]
    fn byte(self, off: u64) -> *mut u8 {
        let chunk = self.chunk(off);
        if chunk.is_null() {
            return ptr::null_mut();
        }
        Self::at(chunk, off)
    }

    #[inline]
    fn byte_or_allocate(self, off: u64) -> *mut u8 {
        let mut chunk = self.chunk(off);
        if chunk.is_null() {
            chunk = self.install(off);
        }
        Self::at(chunk, off)
    }

    #[inline]
    fn gen(self, off: u64) -> *mut u64 {
        let chunk = self.chunk(off);
        if chunk.is_null() {
            return ptr::null_mut();
        }
        unsafe {
            ptr::addr_of_mut!((*chunk).gens)
                .cast::<u64>()
                .add(((off & CHUNK_MASK) >> PAGE_SHIFT) as usize)
        }
    }
}

/// Bump the write generation of every page in `off..off + len`.
fn bump<M: Map>(map: M, off: u64, len: usize) {
    let last = (off + len as u64 - 1) >> PAGE_SHIFT;
    for page in off >> PAGE_SHIFT..=last {
        unsafe { *map.gen(page << PAGE_SHIFT) += 1 };
    }
}

/// What [`Ram`] holds, and what [`Ram::take_contents`] hands over to the RAM
/// of the next boot.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Contents<S: Store = DefaultStore> {
    store: S,
    len: u64,
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
pub struct SharedRam<M: Map = DefaultMap> {
    map: M,
    len: u64,
}

impl<M: Map> SharedRam<M> {
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
        if self.map.room(off) < size as usize {
            return (0..u64::from(size))
                .try_fold(0, |v, i| Some(v | self.load(off + i, 1)? << (8 * i)));
        }
        let p = self.map.byte(off);
        if !M::FLAT && p.is_null() {
            return Some(0);
        }
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
        if self.map.room(off) < size as usize {
            for i in 0..u64::from(size) {
                self.store(off + i, 1, value >> (8 * i))?;
            }
            return Some(());
        }
        let mut p = self.map.byte(off);
        if !M::FLAT && p.is_null() {
            if value & (u64::MAX >> (64 - 8 * size)) == 0 {
                return Some(());
            }
            p = self.map.byte_or_allocate(off);
        }
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
        let mut p = self.map.byte(off);
        if !M::FLAT && p.is_null() {
            if old != 0 {
                return Some(false);
            }
            if new == 0 {
                return Some(true);
            }
            p = self.map.byte_or_allocate(off);
        }
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

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(bound = "")]
pub struct Ram<S: Store = DefaultStore> {
    base: u32,
    /// Every writer goes through [`Ram::store`], [`Ram::store_at`] or
    /// [`Ram::write_slice`], so none can get past the write generations that
    /// let the VPU cache decoded instructions.
    contents: crate::snap::Keep<Contents<S>>,
    /// Cache-coherency tracking (`--check-coherency`); off unless asked for.
    pub coherency: crate::snap::Keep<crate::coherency::Coherency>,
}

impl Ram {
    pub fn new(base: u32, size: u64) -> Ram {
        Ram::new_in(base, size)
    }
}

impl<S: Store> Ram<S> {
    pub fn new_in(base: u32, size: u64) -> Ram<S> {
        Ram {
            base,
            contents: crate::snap::Keep(Contents {
                store: S::new(size),
                len: size,
            }),
            coherency: crate::snap::Keep(crate::coherency::Coherency::off()),
        }
    }

    /// What the DRAM holds, to carry across a reset: the chips keep their
    /// contents through a watchdog reset, and the bootloader that runs next
    /// overwrites only what it uses (checked on a Raspberry Pi 4B d03115,
    /// where 1 MiB written in the VPU's memory and 1 MiB in the ARM's came
    /// back intact after the bootloader had started the firmware again).
    pub fn take_contents(&mut self) -> Contents<S> {
        std::mem::replace(
            &mut self.contents,
            Contents {
                store: S::new(0),
                len: 0,
            },
        )
    }

    /// Put back what [`Ram::take_contents`] took, into a RAM of the same size.
    pub fn restore_contents(&mut self, contents: Contents<S>) {
        assert_eq!(
            contents.len, self.contents.len,
            "the DRAM keeps its size across a reset"
        );
        self.contents = crate::snap::Keep(contents);
    }

    #[inline]
    fn map(&self) -> S::Map {
        self.contents.store.map()
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
        let gen = self.map().gen(off);
        if !S::Map::FLAT && gen.is_null() {
            return Some(0);
        }
        Some(unsafe { *gen })
    }

    pub fn base(&self) -> u32 {
        self.base
    }

    /// The same bytes, for cores on other threads. See [`SharedRam`] for what
    /// the caller promises.
    pub fn share(&mut self) -> SharedRam<S::Map> {
        SharedRam {
            map: self.map(),
            len: self.contents.len,
        }
    }

    pub fn len(&self) -> u64 {
        self.contents.len
    }

    pub fn is_empty(&self) -> bool {
        self.contents.len == 0
    }

    /// The whole RAM from offset 0, 1 MiB at a time.
    pub fn regions(&self) -> impl Iterator<Item = (u64, Region<'_>)> {
        let (map, len) = (self.map(), self.contents.len);
        (0..len.div_ceil(CHUNK as u64)).map(move |i| {
            let off = i << CHUNK_SHIFT;
            let n = (len - off).min(CHUNK as u64) as usize;
            let p = map.byte(off);
            let region = if !S::Map::FLAT && p.is_null() {
                Region::Zeros(n)
            } else {
                Region::Bytes(unsafe { std::slice::from_raw_parts(p, n) })
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
        let map = self.map();
        let mut done = 0;
        while done < bytes.len() {
            let at = off + done as u64;
            let n = map.room(at).min(bytes.len() - done);
            let piece = &bytes[done..done + n];
            let mut p = map.byte(at);
            if !S::Map::FLAT && p.is_null() && piece.iter().any(|&b| b != 0) {
                p = map.byte_or_allocate(at);
            }
            if S::Map::FLAT || !p.is_null() {
                unsafe { ptr::copy_nonoverlapping(piece.as_ptr(), p, n) };
                bump(map, at, n);
            }
            done += n;
        }
        if !bytes.is_empty() {
            self.note_write(off, bytes.len());
        }
        Ok(())
    }

    /// `len` bytes from `addr`: in place when they are contiguous and have
    /// been written to, else a copy.
    pub fn read_slice(&self, addr: u32, len: usize) -> BusResult<Cow<'_, [u8]>> {
        let off = self.offset(addr, len)?;
        self.note_read(off, len);
        let map = self.map();
        if map.room(off) >= len {
            let p = map.byte(off);
            if !S::Map::FLAT && p.is_null() {
                return Ok(Cow::Owned(vec![0; len]));
            }
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
        let map = self.map();
        if map.room(off) < N {
            return Some(self.bytes_across(off));
        }
        let p = map.byte(off);
        if !S::Map::FLAT && p.is_null() {
            return Some([0; N]);
        }
        Some(unsafe { ptr::read_unaligned(p.cast::<[u8; N]>()) })
    }

    #[cold]
    fn bytes_across<const N: usize>(&self, off: u64) -> [u8; N] {
        std::array::from_fn(|i| self.bytes::<1>(off + i as u64).map_or(0, |b| b[0]))
    }

    /// Write `N` bytes `off` bytes into RAM, which [`Self::fits`] has checked.
    #[inline]
    fn put<const N: usize>(&mut self, off: u64, bytes: [u8; N]) {
        let map = self.map();
        if map.room(off) < N {
            return self.put_across(off, &bytes);
        }
        let mut p = map.byte(off);
        if !S::Map::FLAT && p.is_null() {
            if bytes == [0; N] {
                return;
            }
            p = map.byte_or_allocate(off);
        }
        unsafe { ptr::write_unaligned(p.cast::<[u8; N]>(), bytes) };
        let first = map.gen(off);
        let last = map.gen(off + N as u64 - 1);
        unsafe {
            *first += 1;
            if last != first {
                *last += 1;
            }
        }
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

    /// The 4 KiB page at `off` (page-aligned, inside RAM) as host memory, for
    /// fetching instructions straight out of it; `None` where nothing but
    /// zeros has been written. The pointer holds until the contents are
    /// taken or the RAM dropped.
    #[inline(always)]
    pub fn page_bytes(&self, off: u64) -> Option<*mut u8> {
        let p = self.map().byte(off);
        (!p.is_null()).then_some(p)
    }

    /// [`Self::page_bytes`] as instruction words.
    #[inline(always)]
    pub fn page_ptr(&self, off: u64) -> Option<*const u32> {
        self.page_bytes(off).map(|p| p.cast::<u32>().cast_const())
    }

    /// The write generation of the page holding `off` (inside RAM), where a
    /// cache of what the page holds can watch it; null where nothing but
    /// zeros has been written.
    #[inline(always)]
    pub fn gen_ptr(&self, off: u64) -> *const u64 {
        self.map().gen(off).cast_const()
    }

    /// A little-endian value of 1, 2, 4 or 8 bytes `off` bytes into RAM, for
    /// a core's data access; `None` past the end.
    #[inline(always)]
    pub fn read_le(&self, off: u64, size: u32) -> Option<u64> {
        self.note_read(off, size as usize);
        match size {
            1 => self.bytes::<1>(off).map(|b| u64::from(b[0])),
            2 => self
                .bytes::<2>(off)
                .map(|b| u64::from(u16::from_le_bytes(b))),
            4 => self
                .bytes::<4>(off)
                .map(|b| u64::from(u32::from_le_bytes(b))),
            _ => self.bytes::<8>(off).map(u64::from_le_bytes),
        }
    }

    /// [`Self::read_le`]'s store; `false` past the end.
    #[inline(always)]
    pub fn write_le(&mut self, off: u64, size: u32, value: u64) -> bool {
        let n = size as usize;
        if !self.fits(off, n) {
            return false;
        }
        match size {
            1 => self.put(off, [value as u8]),
            2 => self.put(off, (value as u16).to_le_bytes()),
            4 => self.put(off, (value as u32).to_le_bytes()),
            _ => self.put(off, value.to_le_bytes()),
        }
        self.note_write(off, n);
        true
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
    #[inline(always)]
    fn note_write(&mut self, off: u64, len: usize) {
        if self.coherency.is_on() {
            self.note_write_tracked(off, len);
        }
    }

    #[inline(never)]
    fn note_write_tracked(&mut self, off: u64, len: usize) {
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

    fn resident(ram: &Ram<Chunks>) -> usize {
        ram.regions()
            .filter(|(_, r)| matches!(r, Region::Bytes(_)))
            .count()
            * CHUNK
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

    fn across_two_chunks<S: Store>() {
        let mut ram = Ram::<S>::new_in(0x100, 4 << 20);
        let edge = 0x100 + (1 << 20) - 2;
        ram.store(edge, Width::Word, 0x1122_3344).unwrap();
        assert_eq!(ram.load(edge, Width::Word), Ok(0x1122_3344));
        assert_eq!(ram.load(edge + 2, Width::Half), Ok(0x1122));
        ram.write_slice(edge - 1, b"abcdef").unwrap();
        assert_eq!(&*ram.read_slice(edge - 1, 6).unwrap(), b"abcdef");
        let shared = ram.share();
        assert_eq!(shared.load(u64::from(edge - 0x100), 4), Some(0x6564_6362));
        shared
            .store((2 << 20) - 4, 8, 0x0102_0304_0506_0708)
            .unwrap();
        assert_eq!(shared.load((2 << 20) - 4, 8), Some(0x0102_0304_0506_0708));
        assert_eq!(ram.load_at((2 << 20) - 4, Width::Word), Ok(0x0506_0708));
        assert!(ram.load_at((4 << 20) - 2, Width::Word).is_err());
    }

    #[test]
    fn an_access_across_two_chunks_reaches_both() {
        across_two_chunks::<Flat>();
        across_two_chunks::<Chunks>();
    }

    fn page_generations<S: Store>() {
        let mut ram = Ram::<S>::new_in(0, 2 << 20);
        let before = ram.page_gen(0x1000).unwrap();
        ram.store(0x1000, Width::Word, 1).unwrap();
        let after = ram.page_gen(0x1000).unwrap();
        assert_ne!(after, before);
        let neighbour = ram.page_gen(0x2000);
        ram.store(0x1000, Width::Word, 0).unwrap();
        assert_ne!(ram.page_gen(0x1000), Some(after));
        assert_eq!(ram.page_gen(0x2000), neighbour);
        assert_eq!(ram.page_gen(2 << 20), None);
    }

    #[test]
    fn a_store_moves_its_page_generation_and_only_its_own() {
        page_generations::<Flat>();
        page_generations::<Chunks>();
    }

    fn exclusive<S: Store>() {
        let mut ram = Ram::<S>::new_in(0, 1 << 20);
        let shared = ram.share();
        assert_eq!(shared.compare_exchange(0x40, 8, 1, 2), Some(false));
        assert_eq!(shared.compare_exchange(0x40, 8, 0, 0), Some(true));
        assert_eq!(shared.compare_exchange(0x40, 8, 0, 5), Some(true));
        assert_eq!(shared.load(0x40, 8), Some(5));
        assert_eq!(shared.compare_exchange(0x40, 8, 0, 6), Some(false));
        assert_eq!(shared.compare_exchange(0x44, 8, 0, 6), None, "misaligned");
    }

    #[test]
    fn an_exclusive_store_compares_against_what_is_there() {
        exclusive::<Flat>();
        exclusive::<Chunks>();
    }

    #[test]
    fn chunks_take_host_memory_only_for_what_is_written() {
        let mut ram = Ram::<Chunks>::new_in(0, 8 << 30);
        assert_eq!(resident(&ram), 0);
        assert_eq!(ram.load_at(0x1_8000_0000, Width::Word), Ok(0));
        ram.store_at(0x1_8000_0000, Width::Word, 0).unwrap();
        ram.write_slice(0x4000_0000, &[0; 3 << 20]).unwrap();
        ram.share().store(0x10, 8, 0).unwrap();
        assert_eq!(resident(&ram), 0, "zeros over zeros change nothing");
        ram.store_at(0x1_8000_0000, Width::Word, 0xDEAD_BEEF)
            .unwrap();
        assert_eq!(ram.load_at(0x1_8000_0000, Width::Word), Ok(0xDEAD_BEEF));
        assert_eq!(resident(&ram), 1 << 20);
        assert!(ram.load_at((8 << 30) - 2, Width::Word).is_err());
    }

    #[test]
    fn a_chunk_written_for_the_first_time_moves_its_generations() {
        let mut ram = Ram::<Chunks>::new_in(0, 2 << 20);
        assert_eq!(ram.page_gen(0x1000), Some(0));
        ram.store(0x1000, Width::Word, 0).unwrap();
        assert_eq!(ram.page_gen(0x1000), Some(0), "zeros change nothing");
        ram.share().store(0x10_1000, 4, 7).unwrap();
        assert_ne!(
            ram.page_gen(0x10_1000),
            Some(0),
            "nor through another thread"
        );
    }

    #[test]
    fn regions_walk_the_whole_ram() {
        let mut ram = Ram::<Chunks>::new_in(0, (2 << 20) + 0x1000);
        ram.store_at(0x20_0010, Width::Byte, 9).unwrap();
        let regions: Vec<_> = ram.regions().collect();
        assert_eq!(regions.len(), 3);
        assert!(matches!(regions[0], (0, Region::Zeros(n)) if n == 1 << 20));
        assert!(
            matches!(&regions[2], (0x20_0000, Region::Bytes(b)) if b.len() == 0x1000 && b[0x10] == 9)
        );
        let flat = Ram::<Flat>::new_in(0, (2 << 20) + 0x1000);
        let lens: Vec<_> = flat.regions().map(|(_, r)| r.len()).collect();
        assert_eq!(lens, [1 << 20, 1 << 20, 0x1000]);
    }
}
