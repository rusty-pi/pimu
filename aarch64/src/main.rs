//! Bare-metal aarch64 frontend for the firmware model — stage 2 of issue #32.
//!
//! This is scaffolding, not the emulator: it proves that a Rust image of ours
//! is loadable by `qemu-system-aarch64 -machine raspi4b -kernel`, that it lands
//! at the BCM2711 address the firmware assumes, and that it runs at EL2 (which
//! stage 4 needs in order to install stage-2 translation and trap the guest's
//! MMIO). Stage 3 drops the VPU interpreter in behind this.
//!
//! `raspi4b` and not `virt`: `raspi4b` puts RAM at physical 0 and loads at
//! 0x80000, which is the map the VideoCore firmware produces addresses for
//! (`memory@0`, `vc_mem.mem_base=0x3ec00000`). `virt` starts RAM at
//! 0x4000_0000 with `flash@0` at the bottom, so every firmware-produced address
//! would be wrong.
//!
//! Run it with `scripts/qemu-kernel.sh`; see `docs/aarch64-frontend.md`.

#![no_std]
#![no_main]

mod boot;
mod uart;

use core::fmt::Write;

extern "C" {
    // Addresses from link.ld. Only their addresses are meaningful — reading the
    // `u8` would read whatever the image happens to have at that offset.
    static __bss_start: u8;
    static __bss_end: u8;
    static __stack_top: u8;
    static _end: u8;
    // Absolute (non-allocated) symbol: its "address" *is* the image size.
    static _image_size: u8;
}

fn sym(s: &u8) -> usize {
    s as *const u8 as usize
}

/// Current exception level, from bits 3:2 of `CurrentEL`.
fn current_el() -> u64 {
    let el: u64;
    // SAFETY: a system-register read with no side effects.
    unsafe { core::arch::asm!("mrs {}, CurrentEL", out(reg) el, options(nomem, nostack)) };
    (el >> 2) & 0b11
}

fn mpidr() -> u64 {
    let v: u64;
    // SAFETY: a system-register read with no side effects.
    unsafe { core::arch::asm!("mrs {}, mpidr_el1", out(reg) v, options(nomem, nostack)) };
    v
}

/// Entry point called by the stub in `boot.rs`.
///
/// `dtb` is `x0` as the arm64 boot protocol left it: the physical address of
/// the device tree. `run_base` and `link_base` are the stub's placement
/// evidence — see `boot.rs`.
#[no_mangle]
pub extern "C" fn rvf_main(dtb: u64, run_base: u64, link_base: u64) -> ! {
    // SAFETY: boot CPU, nothing else is touching UART0.
    let mut con = unsafe { uart::console() };
    con.init();

    // Before anything that formats: core::fmt dispatches through vtable
    // pointers, which are absolute addresses baked in at link time. If we were
    // not loaded where we were linked, the first `writeln!` branches into
    // unmapped RAM and the machine dies with an "Undefined Instruction" at an
    // address that does not appear in the binary. Say so instead. `puts` is
    // safe here: rustc materialises string literals with adrp/add, which is
    // PC-relative.
    if run_base != link_base {
        con.puts("\nrpi-virt-fw: FATAL: image loaded at the wrong address.\n");
        con.puts("  the bootloader ignored the header's text_offset; see aarch64/link.ld\n");
        park();
    }

    let _ = writeln!(con, "\nrpi-virt-fw: aarch64 frontend (issue #32 stage 2)");
    // The exception level is the load-bearing claim of this stage: stage 4
    // installs stage-2 translation, which only exists at EL2. Printing it means
    // the binary itself attests to it, rather than a host-side QEMU trace.
    let _ = writeln!(con, "  CurrentEL   EL{}", current_el());
    let _ = writeln!(con, "  MPIDR_EL1   {:#018x}", mpidr());
    let _ = writeln!(con, "  DTB (x0)    {:#018x}", dtb);
    // Should read 0x80000 .. and a size matching the header's image_size; if it
    // does not, the bootloader placed us somewhere we did not link for.
    let _ = writeln!(
        con,
        "  image       {:#010x} .. {:#010x} ({:#x} bytes)",
        _start as *const () as usize,
        // SAFETY: linker-provided symbols, addresses only.
        unsafe { sym(&_end) },
        unsafe { sym(&_image_size) },
    );
    let _ = writeln!(
        con,
        "  bss         {:#010x} .. {:#010x}   stack top {:#010x}",
        // SAFETY: as above.
        unsafe { sym(&__bss_start) },
        unsafe { sym(&__bss_end) },
        unsafe { sym(&__stack_top) },
    );
    let _ = writeln!(con, "  UART0       {:#010x} (PL011)", uart::UART0_BASE);
    let _ = writeln!(con, "scaffolding only: no VPU model in this image yet");

    park()
}

extern "C" {
    /// The image's first byte, i.e. the arm64 Image header. Declared as a
    /// function because that is what the entry stub defines it as.
    fn _start() -> !;
}

fn park() -> ! {
    loop {
        // SAFETY: `wfe` is unprivileged and has no memory effects.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // A fresh handle rather than a shared one: panicking is exactly the moment
    // when the console's owner may be mid-write or gone.
    // SAFETY: we are terminal; no other user can matter any more.
    let mut con = unsafe { uart::console() };
    let _ = writeln!(con, "\nrpi-virt-fw: PANIC: {info}");
    park()
}
