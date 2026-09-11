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
//! `_load_addr` is the offset the image asks to be *loaded* at; `_start_addr`
//! is the address it is *linked* for, and the two differ by design — the model
//! owns physical 0 .. 1 GiB and `-initrd` will not go above 960 MiB, so the
//! image is loaded low and copies itself high. link.ld has the whole argument
//! with the measurements behind it.
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
    .quad   _load_addr              // text_offset: RAM base is 0 on raspi4b, so this
                                    //              is a physical address. NOT the link
                                    //              address — see link.ld: loading high
                                    //              would push -initrd past the 960 MiB
                                    //              raspi4b gives the ARM.
    .quad   _image_size             // image_size, from link.ld
    .quad   0                       // flags: little-endian, page size unspecified,
                                    //        2MB-aligned placement near start of RAM
    .quad   0                       // res2
    .quad   0                       // res3
    .quad   0                       // res4
    .long   0x644d5241              // magic: "ARM\x64"
    .long   0                       // res5: no PE header, this is not an EFI image

.Lentry:
    // Only the boot CPU runs the image. On `raspi4b` under `-kernel` this is
    // belt and braces: QEMU's `write_smpboot64()` holds cores 1-3 in a spin
    // table at physical 0x300 and they never reach here at all — verified by
    // reading 0x300 from the image, which holds that stub's `mov x5, #0xd8`
    // (0xd2801b05). `smp.rs` is what actually deals with them, because the
    // model is about to claim the RAM they are spinning in.
    //
    // A bootloader that *does* start the secondaries here parks them at the
    // load address, which the model will overwrite. That is a fatal condition
    // rather than a handled one: `smp::release` requires every secondary to
    // check in at the relocated park below, and a core parked down here
    // cannot, so such a machine fails loudly instead of quietly corrupting a
    // boot. See smp.rs for what porting to one would take.
    mrs     x1, mpidr_el1
    and     x1, x1, #0xff
    cbnz    x1, .Lpark

    // Move the image to the address it was linked for, before anything that is
    // not PC-relative runs. QEMU loads us at `_load_addr` (0x20_0000) because
    // that is the only place from which `-initrd` still fits (link.ld), and
    // 0x20_0000 is inside the gigabyte the model takes as its RAM.
    //
    // Only the `_load_size` bytes that exist in the file are copied: .bss and
    // the two stacks are NOLOAD, and the loop below is followed by the .bss
    // clear anyway. Source and destination cannot overlap (link.ld ASSERTs
    // it), so a forward copy is safe, and both ends are 16-byte aligned.
    adr     x1, _start                  // where we are (PC-relative)
    ldr     x2, .Llink_addr             // where we were linked (absolute literal)
    cmp     x1, x2
    b.eq    .Lrelocated                 // already there: nothing to do
    ldr     x3, .Lload_size
    add     x3, x1, x3                  // one past the last source byte
1:  ldp     x4, x5, [x1], #16
    stp     x4, x5, [x2], #16
    cmp     x1, x3
    b.lo    1b

    // We have just written the instructions we are about to execute. The MMU
    // is off so this is Device memory and should not be cached, but an image
    // that only works because of that is an image that breaks the first time
    // somebody turns the MMU on in stage 4. Arm ARM B2.2.5: invalidate to the
    // point of unification, then synchronise, before branching into it.
    dsb     sy
    ic      iallu
    dsb     sy
    isb
    ldr     x1, .Lrelocated_addr        // absolute -> the copy, not this one
    br      x1

.Lrelocated:
    // Vectors before anything else, including the .bss clear below. With
    // VBAR_EL2 at its reset value of 0 a fault vectors into QEMU's boot stub at
    // physical 0x200, which is not a handler, so the machine silently re-takes
    // the same exception forever — how the 0x200000 load-address bug presented.
    // Installing here costs three instructions and covers every instruction
    // after them. See vectors.rs.
    adrp    x1, __exception_vectors
    add     x1, x1, :lo12:__exception_vectors
    msr     vbar_el2, x1
    isb                             // Arm ARM D17.2.152: the write needs a
                                    // context-synchronising event to take
                                    // effect for following instructions

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
    //
    // `wfi`, not `wfe`. This is where `raspi4b`'s other three cores spend the
    // whole run — `smp::release` sends them here through `__secondary_park`
    // below — and under TCG that choice is the difference between three idle
    // vCPU threads and three busy ones. QEMU's `HELPER(wfi)` halts the vCPU
    // until an interrupt; `HELPER(wfe)` is a yield that returns at once, so a
    // `wfe` park loop burns a host core each. Measured: 366% CPU for the
    // process with `wfe`, against ~110% with `wfi` — the difference is stolen
    // from the one core that is actually interpreting the VPU.
.Lpark:
    wfi
    b       .Lpark

    // Where `smp.rs` sends the secondaries: QEMU's spin-table stub loads this
    // address out of the release table at 0xd8 and branches to it with x0-x3
    // zeroed. It is inside the relocated image, so it survives the model
    // claiming physical 0 .. 1 GiB — which is the entire point of sending them
    // here rather than leaving them in the stub at 0x300.
    //
    // Each arrival marks its own byte of `__secondaries_parked` before halting,
    // so the primary can *see* that low RAM is clear rather than assume it. A
    // byte per CPU and not a counter: no two cores write the same location, so
    // there is nothing to make atomic, and exclusives on Device memory (which
    // is what everything is with the MMU off) are CONSTRAINED UNPREDICTABLE.
.globl __secondary_park
__secondary_park:
    mrs     x1, mpidr_el1
    and     x1, x1, #0x3                // the index QEMU's stub used at 0xd8
    adrp    x2, __secondaries_parked
    add     x2, x2, :lo12:__secondaries_parked
    mov     w3, #1
    strb    w3, [x2, x1]
    dsb     sy
    b       .Lpark

    .balign 8
.Llink_addr:
    .quad   _start
.Lload_size:
    .quad   _load_size
.Lrelocated_addr:
    .quad   .Lrelocated

// One byte per CPU, set by __secondary_park above. In .bss, which the entry
// stub clears long before smp.rs releases anybody.
.section .bss.smp, "aw", @nobits
    .balign 8
.globl __secondaries_parked
__secondaries_parked:
    .space  4
"#
);
