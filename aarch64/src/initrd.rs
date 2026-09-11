//! Finding the `-initrd` blob the bootloader left in RAM.
//!
//! The arm64 boot protocol hands the image one pointer in `x0` and everything
//! else is discovered from there. What that pointer *is* depends on the
//! machine, and `raspi4b` is the awkward case:
//!
//!  * Normally it is a device tree, and QEMU records the blob it loaded as
//!    `/chosen/linux,initrd-start` and `linux,initrd-end`
//!    (`arm_setup_direct_kernel_boot()`, hw/arm/boot.c). That is how a Linux
//!    kernel finds its initramfs.
//!  * But QEMU only builds a device tree when the machine supplies one or
//!    `-dtb` names one, and `raspi4b` does neither. It falls back to
//!    `set_kernel_args()`, which writes an **ATAG list** at `0x100` — the
//!    twenty-year-old ARM boot convention — and passes that address instead.
//!    Verified on QEMU 10.2.1: `x0 = 0x100`, holding `ATAG_CORE` (`0x54410001`),
//!    `ATAG_MEM` and `ATAG_INITRD2` (`0x54420005`) with the blob's address and
//!    length, and no `d00dfeed` anywhere.
//!
//! So both are read: the device tree if `x0` points at one, the ATAG list
//! otherwise. Which also means the image is not tied to `raspi4b` — `virt`,
//! `-dtb`, and a real Pi under U-Boot all take the first path.
//!
//! The device-tree parsing is `rpi_virt_fw::fdt`, the crate's own reader,
//! which is already `no_std` + `alloc`: it needs the heap, and the heap here is
//! a fixed window live from the first instruction, so there is no bootstrap
//! ordering problem and no reason for a second FDT parser. The ATAG list has no
//! parser in the crate and does not deserve one — it is a walk over
//! `(u32 words, u32 tag, ...)` records, below.

use rpi_virt_fw::fdt::Fdt;

/// End of the ATAG list.
const ATAG_NONE: u32 = 0x0000_0000;
/// First tag; its presence is what identifies the list.
const ATAG_CORE: u32 = 0x5441_0001;
/// Initial ramdisk in physical memory: `start`, then `size`.
const ATAG_INITRD2: u32 = 0x5442_0005;

/// `/chosen`'s initrd extent, as physical addresses.
pub struct Initrd {
    pub start: u64,
    pub end: u64,
}

impl Initrd {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start) as usize
    }

    /// The blob itself.
    ///
    /// # Safety
    /// The caller must have established that the extent is real RAM that
    /// nothing else will write — in particular that the heap window does not
    /// overlap it (`heap::check_clear_of`). `'static` is the right lifetime:
    /// the bytes are in physical memory for as long as the machine runs, and
    /// the models keep borrowing them for the whole boot.
    pub unsafe fn bytes(&self) -> &'static [u8] {
        core::slice::from_raw_parts(self.start as *const u8, self.len())
    }
}

/// Read whatever the bootloader left at `x0` and return what it says about the
/// initrd.
///
/// `Err` carries a fixed reason string — this runs before anything worth
/// reporting has happened, so it is a message and not an error type.
///
/// # Safety
/// `arg` must be the pointer the bootloader passed in `x0`.
pub unsafe fn locate(arg: u64) -> Result<Initrd, &'static str> {
    if arg == 0 {
        return Err("no boot-argument pointer in x0");
    }
    match from_fdt(arg) {
        Ok(i) => Ok(i),
        // Only fall through to ATAGs when there was no device tree at all; a
        // device tree that parsed and simply had no initrd in it is an answer,
        // not a reason to go looking somewhere else.
        Err(NoFdt) => from_atags(arg),
        Err(Fdt2(why)) => Err(why),
    }
}

/// Distinguishes "that was not a device tree" from "it was, and here is what
/// went wrong" — see [`locate`].
enum FdtMiss {
    NoFdt,
    Fdt2(&'static str),
}
use FdtMiss::{Fdt2, NoFdt};

unsafe fn from_fdt(dtb: u64) -> Result<Initrd, FdtMiss> {
    // The header's `totalsize` is at offset 4, so read that much and no more:
    // a device tree of unknown length cannot be handed to a slice up front.
    let head = core::slice::from_raw_parts(dtb as *const u8, 8);
    if head[..4] != [0xd0, 0x0d, 0xfe, 0xed] {
        return Err(NoFdt);
    }
    let total = u32::from_be_bytes([head[4], head[5], head[6], head[7]]) as usize;
    let blob = core::slice::from_raw_parts(dtb as *const u8, total);
    let fdt = Fdt::parse(blob).map_err(|_| Fdt2("the device tree did not parse"))?;
    let props = fdt
        .properties_of("/chosen")
        .ok_or(Fdt2("the device tree has no /chosen node"))?;

    // QEMU writes these as one cell or two depending on whether the address
    // fits in 32 bits, so accept either width rather than assuming this
    // machine's RAM stays below 4 GiB.
    let cell = |name: &str| -> Option<u64> {
        let p = props.iter().find(|p| p.name == name)?;
        match p.value.len() {
            4 => Some(u32::from_be_bytes(p.value[..4].try_into().ok()?) as u64),
            8 => Some(u64::from_be_bytes(p.value[..8].try_into().ok()?)),
            _ => None,
        }
    };
    let start =
        cell("linux,initrd-start").ok_or(Fdt2("no /chosen/linux,initrd-start (pass -initrd)"))?;
    let end = cell("linux,initrd-end").ok_or(Fdt2("no /chosen/linux,initrd-end"))?;
    if end <= start {
        return Err(Fdt2("/chosen says the initrd is empty"));
    }
    Ok(Initrd { start, end })
}

/// Walk the ATAG list for `ATAG_INITRD2`.
///
/// Records are `u32 size_in_words`, `u32 tag`, then `size - 2` payload words,
/// terminated by `ATAG_NONE`. The list is little-endian here (the CPU is), and
/// `size` is in *words*, which is the part that is easy to get wrong.
unsafe fn from_atags(list: u64) -> Result<Initrd, &'static str> {
    let mut p = list as *const u32;
    if core::ptr::read_volatile(p.add(1)) != ATAG_CORE {
        return Err("x0 is neither a device tree nor an ATAG list");
    }
    // A malformed list must not become an unbounded walk over RAM: QEMU writes
    // a handful of tags into one page, so anything beyond that is corruption.
    for _ in 0..256 {
        let size = core::ptr::read_volatile(p) as usize;
        let tag = core::ptr::read_volatile(p.add(1));
        if tag == ATAG_NONE || size < 2 {
            break;
        }
        if tag == ATAG_INITRD2 && size >= 4 {
            let start = core::ptr::read_volatile(p.add(2)) as u64;
            let len = core::ptr::read_volatile(p.add(3)) as u64;
            if len == 0 {
                return Err("ATAG_INITRD2 says the initrd is empty");
            }
            return Ok(Initrd {
                start,
                end: start + len,
            });
        }
        p = p.add(size);
    }
    Err("no ATAG_INITRD2 in the boot arguments (pass -initrd)")
}
