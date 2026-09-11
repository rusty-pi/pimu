//! The EL2 exception vector table, and the handler that reports a fault.
//!
//! Why this exists before it is strictly needed: with `VBAR_EL2` left at 0 a
//! fault vectors into QEMU's own boot stub at physical `0x200`, which is not
//! code, so the machine spins re-taking the same exception and prints nothing
//! at all. That is not hypothetical — it is exactly how the `0x200000`
//! load-address bug in stage 2 presented, visible only as
//! `Taking exception 1 [Undefined Instruction] ... ELR 0x810f0` under QEMU's
//! `-d int`, and it cost real time to find. Stage 4 of #32 needs the table
//! anyway: stage-2 translation faults are delivered through it.
//!
//! # Shape of the table
//!
//! `VBAR_EL2` must be 2 KiB aligned (Arm ARM D17.2.152) and addresses 16 slots
//! of 0x80 bytes, in four groups of four (Arm ARM D1.1.4 "Exception vectors"):
//!
//! ```text
//!  0x000  Current EL with SP_EL0     Synchronous / IRQ / FIQ / SError
//!  0x200  Current EL with SP_ELx     Synchronous / IRQ / FIQ / SError
//!  0x400  Lower EL, AArch64          Synchronous / IRQ / FIQ / SError
//!  0x600  Lower EL, AArch32          Synchronous / IRQ / FIQ / SError
//! ```
//!
//! Stage 3 takes everything at the current EL: those are our own bugs. Stage 4
//! will start caring about the `0x400` group, where a guest's stage-2 aborts
//! arrive. Each slot is therefore a stub that records *which* slot fired and
//! branches to one common path, rather than sixteen separate handlers — new
//! behaviour per group is a branch in `rvf_exception`, not a rewrite.
//!
//! # A handler must not be able to fault
//!
//! Three rules follow, and they are why the assembly below looks the way it
//! does:
//!
//!  1. **Its own stack.** The faulting `SP` may be the reason we are here (an
//!     SP alignment fault, or a stack that walked off its allocation), so the
//!     stub switches to `__exc_stack_top` before calling anything.
//!  2. **No memory it has to find with a register it has not saved.** The
//!     general registers are captured into a fixed `.bss` frame, and the two
//!     registers needed to reach that frame are parked in `TPIDR_EL2` and
//!     `TPIDRRO_EL0` first. Nothing is pushed anywhere until `SP` is ours.
//!  3. **No `core::fmt`.** The reporting path is `uart::puts`/`puthex` only —
//!     see the comment on `Uart::puthex`.

use crate::semihost;
use crate::uart;
use core::arch::global_asm;

/// Offsets into the saved frame, in 64-bit words. `x0..x30`, then the `SP` that
/// was live when the exception was taken.
const FRAME_SP: usize = 31;

global_asm!(
    r#"
.section .text.vectors, "ax"

// One slot per entry. Keep it to three instructions: the slot is 0x80 bytes,
// but anything longer here is logic that belongs in `rvf_exception` where it
// can be read. TPIDR_EL2 is a scratch system register with no architectural
// meaning to us — nothing in this image uses thread pointers — so it is a safe
// place to park x0 before we have anywhere in memory to put it.
.macro VECTOR_SLOT index
    .balign 0x80
    msr     tpidr_el2, x0
    mov     x0, #\index
    b       __exc_common
.endm

.balign 2048
.globl __exception_vectors
__exception_vectors:
    VECTOR_SLOT 0           // Current EL, SP_EL0:  Synchronous
    VECTOR_SLOT 1           // Current EL, SP_EL0:  IRQ
    VECTOR_SLOT 2           // Current EL, SP_EL0:  FIQ
    VECTOR_SLOT 3           // Current EL, SP_EL0:  SError
    VECTOR_SLOT 4           // Current EL, SP_ELx:  Synchronous
    VECTOR_SLOT 5           // Current EL, SP_ELx:  IRQ
    VECTOR_SLOT 6           // Current EL, SP_ELx:  FIQ
    VECTOR_SLOT 7           // Current EL, SP_ELx:  SError
    VECTOR_SLOT 8           // Lower EL, AArch64:   Synchronous
    VECTOR_SLOT 9           // Lower EL, AArch64:   IRQ
    VECTOR_SLOT 10          // Lower EL, AArch64:   FIQ
    VECTOR_SLOT 11          // Lower EL, AArch64:   SError
    VECTOR_SLOT 12          // Lower EL, AArch32:   Synchronous
    VECTOR_SLOT 13          // Lower EL, AArch32:   IRQ
    VECTOR_SLOT 14          // Lower EL, AArch32:   FIQ
    VECTOR_SLOT 15          // Lower EL, AArch32:   SError
    .balign 0x80            // so the table really is 16 * 0x80 bytes long

// On entry: x0 = slot index, the caller's x0 is in TPIDR_EL2, x1..x30 and SP
// are exactly as the faulting code left them.
__exc_common:
    // x1 is the only other register we may clobber before the frame is
    // reachable, so park it too. TPIDRRO_EL0 is writable at EL2 and, like
    // TPIDR_EL2, means nothing to this image.
    msr     tpidrro_el0, x1

    adrp    x1, __exc_frame
    add     x1, x1, :lo12:__exc_frame
    stp     x2, x3, [x1, #16]
    stp     x4, x5, [x1, #32]
    stp     x6, x7, [x1, #48]
    stp     x8, x9, [x1, #64]
    stp     x10, x11, [x1, #80]
    stp     x12, x13, [x1, #96]
    stp     x14, x15, [x1, #112]
    stp     x16, x17, [x1, #128]
    stp     x18, x19, [x1, #144]
    stp     x20, x21, [x1, #160]
    stp     x22, x23, [x1, #176]
    stp     x24, x25, [x1, #192]
    stp     x26, x27, [x1, #208]
    stp     x28, x29, [x1, #224]
    // x30 (LR) and the SP that was live at the fault. The SP is worth having:
    // "SP is nowhere near __stack_top" is the whole diagnosis of a runaway.
    mov     x2, sp
    stp     x30, x2, [x1, #240]
    mrs     x2, tpidr_el2
    str     x2, [x1, #0]            // the caller's x0
    mrs     x2, tpidrro_el0
    str     x2, [x1, #8]            // the caller's x1

    // Only now is it safe to have a stack: whatever SP was, it is recorded.
    mov     x2, x0                  // slot index, before x0 becomes an argument
    adrp    x3, __exc_stack_top
    add     x3, x3, :lo12:__exc_stack_top
    mov     sp, x3

    mov     x0, x2                  // arg0 = slot index
                                    // arg1 = x1, already the frame pointer
    b       rvf_exception           // diverging: never comes back

// The frame lives in .bss at a fixed address so the stub can reach it with a
// PC-relative adrp/add and no stack. 32 * 8 bytes: x0..x30 plus SP.
.section .bss.exc_frame, "aw", @nobits
.balign 16
__exc_frame:
    .space 256
"#
);

/// Read a 64-bit system register by name.
macro_rules! read_sysreg {
    ($name:literal) => {{
        let v: u64;
        // SAFETY: a system-register read with no side effects. Every register
        // used here is readable at EL2.
        unsafe { core::arch::asm!(concat!("mrs {}, ", $name), out(reg) v, options(nomem, nostack)) };
        v
    }};
}

/// Which of the sixteen slots fired.
fn slot_name(index: u64) -> &'static str {
    match index {
        0 => "Current EL, SP_EL0: Synchronous",
        1 => "Current EL, SP_EL0: IRQ",
        2 => "Current EL, SP_EL0: FIQ",
        3 => "Current EL, SP_EL0: SError",
        4 => "Current EL, SP_ELx: Synchronous",
        5 => "Current EL, SP_ELx: IRQ",
        6 => "Current EL, SP_ELx: FIQ",
        7 => "Current EL, SP_ELx: SError",
        8 => "Lower EL, AArch64: Synchronous",
        9 => "Lower EL, AArch64: IRQ",
        10 => "Lower EL, AArch64: FIQ",
        11 => "Lower EL, AArch64: SError",
        12 => "Lower EL, AArch32: Synchronous",
        13 => "Lower EL, AArch32: IRQ",
        14 => "Lower EL, AArch32: FIQ",
        15 => "Lower EL, AArch32: SError",
        _ => "impossible slot index",
    }
}

/// `ESR_ELx.EC`, bits 31:26 (Arm ARM D17.2.37). Only the classes that can
/// plausibly reach this image are named; the rest print as a bare number, which
/// is enough to look up.
fn ec_name(ec: u32) -> &'static str {
    match ec {
        0x00 => "unknown reason (undefined instruction, or semihosting HLT with no host)",
        0x01 => "trapped WFI/WFE",
        0x07 => "trapped SIMD/FP access",
        0x0e => "illegal execution state",
        0x15 => "SVC from AArch64",
        0x16 => "HVC from AArch64",
        0x17 => "SMC from AArch64",
        0x18 => "trapped MSR/MRS/system instruction",
        0x19 => "trapped SVE access",
        0x20 => "instruction abort from a lower EL (stage 2, once stage 4 lands)",
        0x21 => "instruction abort without a change of EL",
        0x22 => "PC alignment fault",
        0x24 => "data abort from a lower EL (stage 2, once stage 4 lands)",
        0x25 => "data abort without a change of EL",
        0x26 => "SP alignment fault",
        0x2c => "trapped FP exception",
        0x2f => "SError",
        0x30 | 0x31 => "breakpoint",
        0x32 | 0x33 => "software step",
        0x34 | 0x35 => "watchpoint",
        0x3c => "BRK instruction",
        _ => "unnamed EC, see Arm ARM D17.2.37",
    }
}

/// True if `ec` is one of the abort classes whose ISS carries a DFSC/IFSC and a
/// meaningful `FAR_EL2`.
fn is_abort(ec: u32) -> bool {
    matches!(ec, 0x20 | 0x21 | 0x24 | 0x25)
}

/// `ISS.DFSC`/`IFSC`, bits 5:0 of an abort's ISS (Arm ARM D17.2.37). The
/// stage-2 classes are the ones stage 4 will live in: a translation fault at
/// level 1-3 is the normal, expected result of unmapping a peripheral window,
/// so this decode is what tells "we trapped the guest on purpose" apart from
/// "we broke something".
fn fault_status_name(fsc: u32) -> &'static str {
    match fsc {
        0x00..=0x03 => "address size fault",
        0x04..=0x07 => "translation fault",
        0x08..=0x0b => "access flag fault",
        0x0c..=0x0f => "permission fault",
        0x10 => "synchronous external abort",
        0x11 => "synchronous tag check fault",
        0x14..=0x17 => "synchronous external abort on a translation table walk",
        0x18 => "synchronous parity/ECC error",
        0x1c..=0x1f => "synchronous parity/ECC error on a translation table walk",
        0x21 => "alignment fault",
        0x30 => "TLB conflict abort",
        _ => "unnamed fault status, see Arm ARM D17.2.37",
    }
}

/// Common handler for every slot. Reports and stops; it never returns, because
/// nothing in this image knows how to make a fault survivable and "returned and
/// then behaved strangely" is a worse bug report than "stopped here".
///
/// # Safety
/// Called only from the vector stub above, with `frame` pointing at
/// `__exc_frame`.
#[no_mangle]
pub unsafe extern "C" fn rvf_exception(index: u64, frame: *const u64) -> ! {
    // A fresh handle rather than a shared one: the console's owner may well be
    // the code that just faulted, mid-write.
    let con = uart::console();

    // If `semihost::exit` has already issued its HLT, this trap *is* that HLT
    // being unallocated because the host has no semihosting. Reporting it would
    // call `exit` again and loop forever, printing. Say so once and park; the
    // script's timeout ends the run.
    if semihost::exiting() {
        con.puts("\nrpi-virt-fw: semihosting is not enabled on this host\n");
        con.puts("  add -semihosting-config enable=on,target=native to the QEMU command line\n");
        semihost::park();
    }

    let esr = read_sysreg!("esr_el2");
    let elr = read_sysreg!("elr_el2");
    let far = read_sysreg!("far_el2");
    let spsr = read_sysreg!("spsr_el2");
    let ec = ((esr >> 26) & 0x3f) as u32;
    let iss = (esr & 0x01ff_ffff) as u32;

    con.puts("\nrpi-virt-fw: EXCEPTION: ");
    con.puts(slot_name(index));
    con.puts("\n  ESR_EL2     ");
    con.puthex32(esr as u32);
    con.puts("   EC ");
    con.puthex(ec as u64, 2);
    con.puts(" = ");
    con.puts(ec_name(ec));
    con.puts("\n  ELR_EL2     ");
    con.puthex64(elr);
    con.puts("   (the faulting instruction)\n  FAR_EL2     ");
    con.puthex64(far);
    if is_abort(ec) {
        con.puts("   (the faulting address)");
    } else {
        con.puts("   (not meaningful for this EC)");
    }
    con.puts("\n  SPSR_EL2    ");
    con.puthex64(spsr);
    con.puts("\n  ISS         ");
    con.puthex(iss as u64, 7);
    if is_abort(ec) {
        con.puts("      ");
        con.puts(fault_status_name(iss & 0x3f));
        // ISS bit 6, WnR: was it a write? Only defined for a data abort.
        if matches!(ec, 0x24 | 0x25) {
            con.puts(if iss & (1 << 6) != 0 {
                ", write"
            } else {
                ", read"
            });
        }
    }
    con.puts("\n");

    // The general registers last: they are the bulkiest part and the least
    // often the answer, but "x2 is 0" is what a null callback dispatch looks
    // like, and the firmware model is full of those.
    const LAST_GPR: usize = 30; // x30 is the LR; there is no x31
    for r in 0..=LAST_GPR {
        if r % 2 == 0 {
            con.puts("  ");
        }
        con.puts("x");
        con.putdec(r as u64);
        // Pad the register name to a fixed width without core::fmt.
        con.puts(if r < 10 { "  " } else { " " });
        con.puthex64(frame.add(r).read_volatile());
        // Two per line, and x30 ends its line alone because 31 is odd.
        con.puts(if r % 2 == 0 && r != LAST_GPR {
            "   "
        } else {
            "\n"
        });
    }
    con.puts("  SP          ");
    con.puthex64(frame.add(FRAME_SP).read_volatile());
    con.puts("   (at the fault, not the handler's)\n");

    semihost::exit(semihost::EXIT_FAULT)
}
