/* Link map for the arm64 `-kernel` image (issue #32 stage 2).
 *
 * QEMU's `raspi4b` puts RAM at physical 0, so link address == load address ==
 * physical address (the MMU is off and nothing here is position-independent).
 *
 * Load address 0x20_0000, not the 0x8_0000 of a Linux kernel, for two reasons:
 *
 *  1. It is what QEMU actually does. `load_aarch64_image()` in hw/arm/boot.c
 *     takes the load offset from the header's `text_offset` once `image_size`
 *     is non-zero, and then, because it writes its own boot stub at the bottom
 *     of RAM, adds 2 MiB if that offset is below BOOTLOADER_MAX_SIZE. An image
 *     asking for offset 0 therefore lands at 0x20_0000, not at 0x8_0000 —
 *     which is exactly how the first cut of this frontend failed: linked for
 *     0x8_0000, loaded at 0x20_0000, running fine on PC-relative code until
 *     the first absolute pointer (a core::fmt vtable) branched into unmapped
 *     RAM. Asking for 0x20_0000 explicitly makes the placement match the link.
 *  2. We want 0x8_0000 free. It is where the VideoCore firmware puts the ARM
 *     kernel, and stage 4 of #32 ERETs into that kernel from EL2. A resident
 *     sitting at 0x8_0000 would be overwritten by its own guest.
 *
 * The entry stub checks the placement at runtime and says so on the UART if a
 * bootloader ever ignores `text_offset` (the arm64 spec permits it to, as long
 * as the base is 2 MiB aligned) — a silent mismatch is the confusing failure.
 */

ENTRY(_start)

SECTIONS
{
    . = 0x200000;
    _start_addr = .;

    /* .text.boot must come first: the very first bytes of the image are the
     * 64-byte arm64 Image header, and the header's magic is only recognised at
     * file offset 56. */
    .text : ALIGN(4) {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    /* Its own output section, with the alignment VBAR_EL2 requires stated
     * here rather than left to an inner .balign inside .text: VBAR_EL2 ignores
     * bits 10:0 (Arm ARM D17.2.152), so a table that is merely "somewhere in
     * .text" would be silently entered at the wrong address. KEEP because
     * nothing in Rust references the table's contents — only the entry stub's
     * adrp does. */
    .text.vectors : ALIGN(2048) {
        KEEP(*(.text.vectors))
    }

    .rodata : ALIGN(8) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(8) {
        *(.data .data.*)
    }

    /* NOLOAD: .bss occupies no file bytes but must be counted in `image_size`
     * so the bootloader reserves it. The entry stub zeroes it. */
    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    /* The boot stack lives above .bss rather than inside it, so the bss-clear
     * loop cannot walk over the stack it is running on. Still inside the
     * reserved image, so the bootloader will not place anything there. */
    . = ALIGN(16);
    __stack_bottom = .;
    . += 0x10000;             /* 64 KiB is plenty for the scaffolding stage */
    __stack_top = .;

    /* A separate stack for the exception handler. The SP that was live when a
     * fault was taken is one of the things that can *be* the fault (SP
     * alignment, or a stack that ran off its allocation), so the handler must
     * not borrow it. Kept out of .bss so a fault during the .bss clear — which
     * now runs with VBAR_EL2 already installed — still has somewhere to stand.
     */
    . = ALIGN(16);
    __exc_stack_bottom = .;
    . += 0x4000;              /* 16 KiB: the handler only formats and exits */
    __exc_stack_top = .;

    _end = .;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.note .note.*)
    }
}

/* Total memory the bootloader must reserve, header included: the `image_size`
 * field of the arm64 Image header (Documentation/arch/arm64/booting.rst). */
_image_size = _end - _start_addr;

/* VBAR_EL2 ignores the low 11 bits, so a misaligned table is not a link error
 * but a jump into the middle of somebody else's code on the first fault. Make
 * it a link error. */
ASSERT((__exception_vectors & 2047) == 0,
       "the EL2 vector table must be 2 KiB aligned (VBAR_EL2, Arm ARM D17.2.152)")
