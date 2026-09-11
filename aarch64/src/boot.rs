//! The arm64 Image header and the entry stub.
//!
//! `-kernel` on aarch64 does not require a Linux kernel — it requires the arm64
//! boot protocol (`Documentation/arch/arm64/booting.rst`): a 64-byte header
//! whose magic `ARM\x64` (`0x644d5241`) sits at offset 56. U-Boot, EDK2 and Xen
//! all ship as "kernel Images" this way, and so does this image.
//!
//! Header layout, byte offsets from the start of the image:
//!
//! ```text
//!  0  u32 code0        executable code (we branch to the entry stub)
//!  4  u32 code1        executable code
//!  8  u64 text_offset  load offset from the start of RAM
//! 16  u64 image_size   effective image size, .bss included
//! 24  u64 flags        bit 0 endianness, 1-2 page size, 3 physical placement
//! 32  u64 res2
//! 40  u64 res3
//! 48  u64 res4
//! 56  u32 magic        0x644d5241
//! 60  u32 res5         offset to a PE header; 0 = not an EFI image
//! ```
//!
//! `text_offset` is the offset from the base of RAM at which the image wants to
//! be placed, and `image_size` is how much memory it needs there. Both matter:
//!
//!  * `image_size = 0` means "this is an old image, honour `text_offset`
//!    literally"; a non-zero value is what makes a bootloader reserve .bss too.
//!    Too small a value and the bootloader is free to drop the device tree or
//!    the initrd on top of our own .bss.
//!  * QEMU only consults `text_offset` when `image_size` is non-zero, and then
//!    adds 2 MiB if the offset would collide with the boot stub it writes at
//!    the bottom of RAM (`load_aarch64_image()`, hw/arm/boot.c). We therefore
//!    ask for 0x20_0000 outright — see link.ld for why that address and not the
//!    kernel's 0x8_0000.
//!
//! `_start_addr` is the link address; `raspi4b` has RAM at physical 0, so the
//! link address *is* the offset from the base of RAM.
//!
//! On entry the boot protocol guarantees `x0` = physical address of the device
//! tree and `x1..x3` = 0. QEMU enters at EL2 on `raspi4b` (verified in #32),
//! which later stages need for stage-2 trapping, so the stub deliberately does
//! nothing to drop privilege.

use core::arch::global_asm;

global_asm!(
    r#"
.section .text.boot, "ax"
.globl _start
_start:
    b       .Lentry                 // code0: the arm64 header's "branch to entry"
    .long   0                       // code1
    .quad   _start_addr             // text_offset: RAM base is 0 on raspi4b, so this
                                    //              is the link address from link.ld
    .quad   _image_size             // image_size, from link.ld
    .quad   0                       // flags: little-endian, page size unspecified,
                                    //        2MB-aligned placement near start of RAM
    .quad   0                       // res2
    .quad   0                       // res3
    .quad   0                       // res4
    .long   0x644d5241              // magic: "ARM\x64"
    .long   0                       // res5: no PE header, this is not an EFI image

.Lentry:
    // Only the boot CPU runs the image. The arm64 protocol says secondaries are
    // still parked by the bootloader, but `raspi4b` also models a spin table and
    // a stray core running the init below would race the .bss clear.
    mrs     x1, mpidr_el1
    and     x1, x1, #0xff
    cbnz    x1, .Lpark

    // Stack first: everything after this may call.
    adrp    x1, __stack_top
    add     x1, x1, :lo12:__stack_top
    mov     sp, x1

    // Zero .bss. Both bounds are 16-byte aligned by link.ld, so the 16-byte
    // store pair never overruns __bss_end.
    adrp    x1, __bss_start
    add     x1, x1, :lo12:__bss_start
    adrp    x2, __bss_end
    add     x2, x2, :lo12:__bss_end
1:  cmp     x1, x2
    b.hs    2f
    stp     xzr, xzr, [x1], #16
    b       1b
2:

    // Hand rvf_main the evidence for the placement check: where we are actually
    // running (`adr` is PC-relative, so it survives a wrong load address) and
    // where we were linked to run (a literal the linker filled in). Everything
    // up to here is PC-relative and works either way; the first absolute
    // pointer — a core::fmt vtable, say — would branch into unmapped RAM.
    // x0 is still the device tree pointer the bootloader passed.
    adr     x1, _start
    ldr     x2, .Llink_addr
    bl      rvf_main

    // rvf_main is `-> !`; if it ever returns, park rather than run off the end.
.Lpark:
    wfe
    b       .Lpark

    .balign 8
.Llink_addr:
    .quad   _start
"#
);
