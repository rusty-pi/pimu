//! The model's RAM: a bump allocator over a fixed window of physical memory.
//!
//! Stage 3 of #32 puts the VPU model inside the image, and the model allocates
//! — `Machine::new` is a `vec![0; ram_bytes]`, the console is a `Vec`, the
//! EEPROM image is a `Vec`. So the image needs a `#[global_allocator]`, and
//! where it takes that memory from is a design decision rather than a detail:
//!
//!  * #32's zero-copy property depends on the model's RAM *being* machine
//!    physical memory. QEMU's devices DMA straight into the machine's RAM with
//!    no SMMU in the way, so once stage 4 lets Linux program a DMA engine, a
//!    buffer the firmware handed out has to be an address QEMU's devices can
//!    actually write. A heap carved out of physical RAM keeps that available;
//!    anything indirected does not.
//!  * The window has to miss everything else in the machine, and on `raspi4b`
//!    there is no MMU on and no memory map handed to us — RAM is simply 2 GiB
//!    at physical 0 and every claim on it is by convention.
//!
//! # The window: 0x2000_0000 .. 0x8000_0000 (512 MiB .. 2 GiB)
//!
//! Bottom-up, what is already spoken for on `raspi4b`:
//!
//! | range | who |
//! | --- | --- |
//! | `0x0` .. `~0x1000` | QEMU's own boot stub, written at `loader_start` by `arm_setup_direct_kernel_boot()` |
//! | `0x8_0000` | where the VideoCore firmware puts the ARM kernel; stage 4 `ERET`s into it, so it must stay free |
//! | `0x20_0000` .. `_end` | this image, its stacks included (see `link.ld`) |
//! | `0x800_0000` .. | the `-initrd` blob: QEMU places it at `loader_start + MIN(ram_size / 2, 128 MiB)`, i.e. 128 MiB in on this 2 GiB machine, and puts the device tree after it |
//!
//! The blob bundle carries the 256 MiB SD image, so the initrd runs to about
//! 384 MiB and the device tree sits just above that. 512 MiB is the next round
//! number clear of it with room for a bundle half again as large, and it is
//! checked rather than assumed: [`check_clear_of`] takes the initrd extent the
//! device tree reports and fails the run if it reaches into the window.
//!
//! 2 GiB is the top because `raspi4b` has exactly that much and will not take
//! `-m` (`Invalid RAM size, should be 2 GiB`). That leaves 1.5 GiB, against a
//! model that asks for 1 GiB — see `model.rs` for why 1 GiB is not a guess but
//! the whole of the address space `Machine::fold_ram_addr` can reach.
//!
//! # Why a bump allocator, and why that is not as reckless as it sounds
//!
//! Allocation here is not general-purpose: one enormous long-lived block (the
//! model's RAM) plus a modest amount of `Vec`/`String` churn, on one core,
//! with interrupts off. A bump pointer with LIFO rollback — freeing the most
//! recent block un-bumps, growing it in place extends — serves that exactly,
//! and handles the pattern that would otherwise dominate: a `Vec` doubling as
//! the console fills never copies, because it is the top block.
//!
//! What it does not do is recycle a block that is no longer on top; that
//! leaks. With 1.5 GiB of window against a 1 GiB model the slack is 512 MiB,
//! and [`stats`] reports the high-water mark at the end of every run so the
//! margin is a number in the transcript and not a hope. Exhaustion is a clean
//! failure: `alloc` returns null, which `alloc::alloc::handle_alloc_error`
//! turns into the image's panic handler and exit status 4.
//!
//! # Zeroing
//!
//! `alloc_zeroed` is the hot one — `vec![0; 1 GiB]` goes through it — and
//! memory this allocator has never handed out before does not need zeroing:
//! QEMU's guest RAM is an anonymous mapping, so it comes up zero, and nothing
//! but the loader has touched this window. Only the part of an allocation
//! below the high-water mark can be dirty, and that is the only part zeroed.
//! [`check_window_is_zero`] samples the window at startup so the assumption is
//! tested rather than trusted: a machine that came up with dirty RAM would
//! otherwise show as a firmware that mysteriously misbehaves.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

/// First byte of the heap window. See the module comment for the derivation.
pub const HEAP_BASE: usize = 0x2000_0000;
/// One past the last byte: the top of `raspi4b`'s 2 GiB of RAM.
pub const HEAP_END: usize = 0x8000_0000;

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

/// Fail the run if the `-initrd` blob the bootloader placed for us reaches
/// into the heap window.
///
/// QEMU's placement is a function of RAM size and blob size, so a bigger
/// bundle moves the collision closer without any warning. The overlap would
/// present as the firmware reading corrupted blob bytes some time later, which
/// is a long way from the cause.
pub fn check_clear_of(start: u64, end: u64) -> bool {
    end as usize <= HEAP_BASE || start as usize >= HEAP_END
}

/// Sample the window for non-zero bytes, testing the assumption `alloc_zeroed`
/// rests on.
///
/// One `u64` every 64 KiB: 24k loads over the 1.5 GiB window, which is free
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
