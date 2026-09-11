//! The image's own memory: the physical map, and a bump allocator over the
//! part of it that is not the model's.
//!
//! # The whole map
//!
//! `raspi4b` has 2 GiB of RAM at physical 0, will not take `-m` (*"Invalid RAM
//! size, should be 2 GiB"*), and hands us no memory map — the MMU is off and
//! every claim on RAM is by convention. The convention, bottom-up:
//!
//! | range | size | who |
//! | --- | --- | --- |
//! | `0x0000_0000` .. `0x4000_0000` | 1 GiB | **the model's RAM**, at the address the firmware believes RAM starts at |
//! | `0x4000_0000` .. `0x4020_0000` | 2 MiB | this image after it relocates itself, `.bss` and both stacks included (`link.ld`) |
//! | `0x4020_0000` .. `0x8000_0000` | 1022 MiB | **the heap** |
//!
//! and transiently, before the model claims the first gigabyte, all of it
//! inside that gigabyte and all of it consumed by the time it does:
//!
//! | range | who |
//! | --- | --- |
//! | `0x0` .. `0x1000` | QEMU's boot stub, the ATAG list at `0x100` (`initrd.rs`), the secondary-CPU spin table at `0xd8` and its stub at `0x300` (`smp.rs`) |
//! | `0x8_0000` | where the VideoCore firmware puts the ARM kernel; stage 4 `ERET`s into it |
//! | `0x20_0000` .. | the image as QEMU loaded it, dead once the entry stub has copied it up |
//! | `0x800_0000` .. | the `-initrd` bundle, copied into the heap by `main.rs` before the model starts |
//!
//! # Why the model gets physical 0, and gets exactly 1 GiB
//!
//! Stage 4 puts Linux at EL1 under stage-2 translation, which can map a guest
//! IPA of 0 onto a block of ours wherever it landed. Device DMA cannot: there
//! is no SMMU here, so a QEMU device Linux programs writes *physical*
//! addresses, and an address the firmware handed out is only writable if the
//! model's backing store is at that physical address. Identity is the only
//! arrangement in which the guest's IPA, the model's address and the machine's
//! physical address are the same number.
//!
//! 1 GiB is not a comfortable round figure but the exact size of the address
//! space the model can reach: `Machine::fold_ram_addr` masks every RAM address
//! with `0x3FFF_FFFF` to collapse the four VC4 cache aliases (`0x0`,
//! `0x4000_0000`, `0x8000_0000`, `0xC000_0000`) onto one backing store.
//!
//! Which leaves the arithmetic: 2 GiB total, less the model's first gigabyte,
//! is 1 GiB for the image and the heap. The image needs 360 KiB today and is
//! given 2 MiB, `ASSERT`ed in `link.ld` against this file's [`HEAP_BASE`]. The
//! heap's largest tenant is the copy of the `-initrd` bundle — 272 MiB of SD
//! image — so 1022 MiB is not tight.
//!
//! # Why the image is not simply loaded here
//!
//! Because `-initrd` would not fit: QEMU places the blob after the kernel
//! image and refuses it above `raspi4b`'s 960 MiB ARM split. So the image is
//! loaded at `0x20_0000` and copies itself to `0x4000_0000` — see `link.ld`,
//! which carries the measurements.
//!
//! # Why a bump allocator, and why that is not as reckless as it sounds
//!
//! Allocation here is not general-purpose: one large long-lived block (the
//! bundle copy) plus a modest amount of `Vec`/`String` churn, on one core,
//! with interrupts off. A bump pointer with LIFO rollback — freeing the most
//! recent block un-bumps, growing it in place extends — serves that exactly,
//! and handles the pattern that would otherwise dominate: a `Vec` doubling as
//! the console fills never copies, because it is the top block.
//!
//! What it does not do is recycle a block that is no longer on top; that
//! leaks. [`stats`] reports the high-water mark at the end of every run so the
//! margin is a number in the transcript and not a hope. Exhaustion is a clean
//! failure: `alloc` returns null, which `alloc::alloc::handle_alloc_error`
//! turns into the image's panic handler and exit status 4.
//!
//! # Zeroing
//!
//! Memory this allocator has never handed out does not need zeroing: QEMU's
//! guest RAM is an anonymous mapping, so it comes up zero, and nothing but the
//! loader has touched this window. Only the part of an allocation below the
//! high-water mark can be dirty, and that is the only part `alloc_zeroed`
//! zeroes. [`check_window_is_zero`] samples the window at startup so the
//! assumption is tested rather than trusted: a machine that came up with dirty
//! RAM would otherwise show as a firmware that mysteriously misbehaves.
//!
//! The model's gigabyte gets no such shortcut — the bootloader's leavings are
//! all over it — so `Ram::over_region` zeroes it outright, which is what makes
//! a bare-metal boot retire the same instructions as a hosted one.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

/// First byte of the model's RAM. The firmware's own `SDRAM_CACHED_BASE`, and
/// the reason this frontend exists in this shape at all.
pub const MODEL_RAM_BASE: usize = 0x0000_0000;
/// How much of it: the whole of what `Machine::fold_ram_addr` can reach.
pub const MODEL_RAM_BYTES: usize = 0x4000_0000;
/// Where the image is linked and relocated to, one past the model's RAM.
/// Mirrors `_start_addr` in `link.ld`.
pub const IMAGE_BASE: usize = MODEL_RAM_BASE + MODEL_RAM_BYTES;
/// First byte of the heap window: 2 MiB above [`IMAGE_BASE`], which `link.ld`
/// `ASSERT`s the image fits inside.
pub const HEAP_BASE: usize = IMAGE_BASE + 0x20_0000;
/// One past the last byte: the top of `raspi4b`'s 2 GiB of RAM.
pub const HEAP_END: usize = 0x8000_0000;

/// The model's RAM, as `Machine::with_ram_region` wants it.
///
/// # Why this is not a `&'static mut [u8]`, and why the pointer goes through
/// an `asm!`
///
/// The window starts at physical zero, and Rust has no sanctioned way to name
/// that memory: a reference may not be null, and `ptr::copy_nonoverlapping`
/// and friends require a non-null pointer too. Address 0 is a fact about the
/// machine — the Pi's SDRAM starts there and the firmware hands out addresses
/// in it — but to the compiler it is a promise that this code is unreachable,
/// and it acts on that.
///
/// It acted on it twice while this was written. `slice::from_raw_parts_mut(0,
/// 1 GiB)` and then `write_bytes(0 as *mut u8, ..)` each let LLVM propagate
/// the non-null precondition backwards until the entire VPU model was dead
/// code: `.text` fell from 276 KiB to 40 KiB and the image printed its banner
/// and exited, having silently skipped the boot it exists to run.
///
/// So the address is made opaque here, once, at the only place the constant
/// becomes a pointer. An empty `asm!` block with an `inout` operand is the
/// identity function the optimiser cannot see through; everything downstream
/// gets a pointer whose value it cannot reason about, which is exactly the
/// truth of the situation. The `Ram` type comment in `src/mem.rs` has the
/// other half of this — why it stores a pointer and copies out rather than
/// handing anyone a slice.
pub fn model_ram() -> (*mut u8, usize) {
    let mut addr: usize = MODEL_RAM_BASE;
    // SAFETY: no instructions, no memory access, no flags — the operand
    // constraint is the whole point. See above.
    unsafe {
        core::arch::asm!("/* {0} is physical RAM */", inout(reg) addr,
                         options(nomem, nostack, preserves_flags));
    }
    (addr as *mut u8, MODEL_RAM_BYTES)
}

/// Next unallocated byte. Starts at [`HEAP_BASE`]; only ever moves within the
/// window.
static CURSOR: AtomicUsize = AtomicUsize::new(HEAP_BASE);
/// Highest value [`CURSOR`] has ever held. Everything at or above it is memory
/// the allocator has never handed out, which is what makes the `alloc_zeroed`
/// shortcut sound.
static HIGH_WATER: AtomicUsize = AtomicUsize::new(HEAP_BASE);
/// Allocation requests that did not fit, so an out-of-memory run says so
/// rather than only showing up as a panic from `handle_alloc_error`.
static FAILURES: AtomicUsize = AtomicUsize::new(0);

/// Bytes currently handed out, the high-water mark, and the number of failed
/// requests — all in bytes from [`HEAP_BASE`].
pub fn stats() -> (usize, usize, usize) {
    (
        CURSOR.load(Ordering::Relaxed) - HEAP_BASE,
        HIGH_WATER.load(Ordering::Relaxed) - HEAP_BASE,
        FAILURES.load(Ordering::Relaxed),
    )
}

/// Total size of the window.
pub fn capacity() -> usize {
    HEAP_END - HEAP_BASE
}

/// Is the `-initrd` blob the bootloader placed for us clear of the image and
/// the heap?
///
/// It is *expected* to be inside the model's gigabyte — QEMU puts it at
/// 128 MiB — which is why `main.rs` copies it into the heap before the model
/// claims that memory. What must not happen is the other direction: a blob big
/// enough to reach `IMAGE_BASE` would be overwriting this image's own code and
/// stacks while it runs. QEMU's placement is a function of RAM size and blob
/// size, so a bigger bundle moves that closer without any warning.
pub fn check_clear_of(start: u64, end: u64) -> bool {
    end as usize <= IMAGE_BASE || start as usize >= HEAP_END
}

/// Sample the window for non-zero bytes, testing the assumption `alloc_zeroed`
/// rests on.
///
/// One `u64` every 64 KiB: 16k loads over the 1022 MiB window, which is free
/// next to the boot it precedes, and enough to catch a machine whose RAM is
/// not the zero-filled anonymous mapping QEMU gives us. It is a sample and not
/// a proof; a proof costs as much as simply zeroing, which is the thing being
/// avoided. Returns the first dirty address.
///
/// Starts at the high-water mark, not at [`HEAP_BASE`], so it can be called
/// after the first few allocations — the claim being tested is only ever about
/// memory the allocator has *not* handed out.
pub fn check_window_is_zero() -> Option<usize> {
    let mut a = (HIGH_WATER.load(Ordering::Relaxed) + 7) & !7;
    while a < HEAP_END {
        // SAFETY: inside the window, which is RAM and which nothing has
        // allocated from yet (this runs before the allocator is first used).
        if unsafe { core::ptr::read_volatile(a as *const u64) } != 0 {
            return Some(a);
        }
        a += 64 * 1024;
    }
    None
}

pub struct BumpHeap;

// SAFETY: the image runs on one core with interrupts masked (the entry stub
// never unmasks, and nothing here enables them), so the load/store pairs below
// cannot interleave. `Sync` is required by `#[global_allocator]` regardless.
unsafe impl Sync for BumpHeap {}

unsafe impl GlobalAlloc for BumpHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let cur = CURSOR.load(Ordering::Relaxed);
        // `align` is a power of two, so the round-up is the usual mask. It
        // cannot overflow: `cur` is below 2 GiB and alignments here are small.
        let start = (cur + layout.align() - 1) & !(layout.align() - 1);
        let Some(next) = start.checked_add(layout.size()) else {
            FAILURES.fetch_add(1, Ordering::Relaxed);
            return core::ptr::null_mut();
        };
        if next > HEAP_END {
            FAILURES.fetch_add(1, Ordering::Relaxed);
            return core::ptr::null_mut();
        }
        CURSOR.store(next, Ordering::Relaxed);
        if next > HIGH_WATER.load(Ordering::Relaxed) {
            HIGH_WATER.store(next, Ordering::Relaxed);
        }
        start as *mut u8
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // Read the mark *before* allocating: everything from here up is memory
        // this allocator has never handed out, and so is still the zeros QEMU's
        // anonymous mapping came up with.
        let virgin_from = HIGH_WATER.load(Ordering::Relaxed);
        let p = self.alloc(layout);
        if p.is_null() {
            return p;
        }
        let start = p as usize;
        // Only the part below the mark can hold a previous tenant's bytes.
        let dirty_end = core::cmp::min(start + layout.size(), virgin_from);
        if dirty_end > start {
            core::ptr::write_bytes(p, 0, dirty_end - start);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // Only the most recent block can be given back. Anything else leaks —
        // see the module comment for why that is affordable here, and `stats`
        // for how the margin is reported.
        let end = ptr as usize + layout.size();
        if end == CURSOR.load(Ordering::Relaxed) {
            CURSOR.store(ptr as usize, Ordering::Relaxed);
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Growing the top block in place is what keeps a `Vec` that doubles as
        // it fills — the modelled console, most of the run — from copying
        // itself each time, and from leaking every previous generation.
        let end = ptr as usize + layout.size();
        if end == CURSOR.load(Ordering::Relaxed) {
            let next = ptr as usize + new_size;
            if next <= HEAP_END {
                CURSOR.store(next, Ordering::Relaxed);
                if next > HIGH_WATER.load(Ordering::Relaxed) {
                    HIGH_WATER.store(next, Ordering::Relaxed);
                }
                return ptr;
            }
            FAILURES.fetch_add(1, Ordering::Relaxed);
            return core::ptr::null_mut();
        }
        // Not on top: the default alloc/copy/free.
        let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
        let new_ptr = self.alloc(new_layout);
        if !new_ptr.is_null() {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, core::cmp::min(layout.size(), new_size));
            self.dealloc(ptr, layout);
        }
        new_ptr
    }
}
