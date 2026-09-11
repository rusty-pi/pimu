/* Link map for the arm64 `-kernel` image (#32 stages 2-4).
 *
 * QEMU's `raspi4b` puts RAM at physical 0 with the MMU off, so a link address
 * is a physical address. This image is loaded at one and runs at another:
 *
 *   _load_addr  0x0020_0000   where QEMU puts the file (the header's text_offset)
 *   _start_addr 0x4000_0000   where it is linked, and where the entry stub
 *                             copies itself to before running any absolute code
 *
 * # Why the image has to move
 *
 * Stage 4 needs the model's RAM to *be* physical memory. Linux's devices are
 * QEMU's, they DMA with no SMMU in the way, and stage-2 translation does not
 * apply to a device — so an address the firmware hands a peripheral only works
 * if the model's backing store sits at exactly that physical address. The
 * model can reach 1 GiB (`Machine::fold_ram_addr` masks with 0x3FFF_FFFF), so
 * physical 0 .. 0x4000_0000 belongs to the model, and everything of ours has
 * to be above it. See aarch64/src/heap.rs for the whole map.
 *
 * # Why it cannot simply be *loaded* at 0x4000_0000
 *
 * `load_aarch64_image()` in hw/arm/boot.c honours the header's `text_offset`
 * once `image_size` is non-zero, so asking to be loaded at 1 GiB does work on
 * its own. What breaks is `-initrd`, which is how the firmware blobs get in
 * (aarch64/src/bundle.rs): QEMU places the initrd at
 * `MAX(loader_start + MIN(ram_size/2, 128 MiB), end-of-kernel-image)` and then
 * refuses it if it does not fit under `ram_size`. On `raspi4b` that limit is
 * 960 MiB, not the machine's 2 GiB — `raspi_machine_init()` passes
 * `machine->ram_size - vcram_size` to `setup_boot()`, and the ARM's share is
 * 1 GiB less the 64 MiB VideoCore split. Measured on QEMU 10.2.1 by patching
 * `text_offset` in the built image and booting it with a 1 MiB `-initrd`:
 *
 *     0x3800_0000  ok
 *     0x3BF0_0000  "could not load initrd"          (blob crosses 0x3C00_0000)
 *     0x3BFB_0000  "not enough space after kernel"  (start >= 0x3C00_0000)
 *     0x4000_0000  "not enough space after kernel"
 *
 * So the image must be *loaded* low, below anything the initrd needs, and get
 * itself out of the model's way afterwards. 0x20_0000 for the load address
 * because that is where QEMU puts an image asking for offset 0 anyway — it
 * writes its own boot stub at the bottom of RAM and adds 2 MiB if the
 * requested offset is below BOOTLOADER_MAX_SIZE — so nothing is gained by
 * asking for anything lower, and 0x8_0000 is spoken for: it is where the
 * VideoCore firmware puts the ARM kernel that stage 4 will ERET into.
 *
 * The entry stub does the copy, and `rvf_main` still compares where it is
 * running against where it was linked, so a copy that did not happen reads as
 * a named failure rather than as the first absolute pointer — a core::fmt
 * vtable, say — branching into unmapped RAM.
 */

ENTRY(_start)

/* The header's `text_offset`: where QEMU loads the file. Not where it runs. */
_load_addr = 0x200000;

SECTIONS
{
    . = 0x40000000;
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

/* Bytes that exist in the file, i.e. everything up to the NOLOAD .bss — the
 * part the entry stub has to copy from the load address to the link address.
 * `__bss_start` is 16-byte aligned above, so the stub's `ldp`/`stp` pair
 * cannot run past it. */
_load_size = __bss_start - _start_addr;

/* VBAR_EL2 ignores the low 11 bits, so a misaligned table is not a link error
 * but a jump into the middle of somebody else's code on the first fault. Make
 * it a link error. */
ASSERT((__exception_vectors & 2047) == 0,
       "the EL2 vector table must be 2 KiB aligned (VBAR_EL2, Arm ARM D17.2.152)")

/* The image, its .bss and both its stacks have to fit in the 2 MiB reserved
 * for them below the heap, or they grow into the allocator's window. The
 * constant is `heap::HEAP_BASE`; keep the two in step. */
ASSERT(_end <= 0x40200000,
       "the image has outgrown the 2 MiB below heap::HEAP_BASE (0x40200000)")

/* The copy at the load address must not reach the link address, or the stub
 * would be overwriting its own source. 0x20_0000 + 2 MiB against 0x4000_0000
 * is not close, but it is the invariant that makes a forward copy legal. */
ASSERT(_load_addr + _image_size <= _start_addr,
       "the loaded image overlaps the address it is copied to")
