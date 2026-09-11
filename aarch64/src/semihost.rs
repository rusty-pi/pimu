//! ARM semihosting: how this image reports a result to the host.
//!
//! A bare-metal image has no `cargo test`, no `boot-check.sh` and no golden
//! transcript — #32 calls that the epic's real open risk. Semihosting closes it
//! without adding a channel: the guest executes `HLT #0xF000` with an operation
//! number in `x0` and a pointer to the parameter block in `x1`, and the
//! hypervisor-or-emulator underneath services it (ARM "Semihosting for AArch32
//! and AArch64", issue D, §5.1 for the AArch64 trap and §6.6 for `SYS_EXIT`).
//! With `SYS_EXIT_EXTENDED` QEMU turns the guest's verdict into its own process
//! exit status, so `scripts/qemu-kernel.sh` can assert on a status instead of
//! grepping stdout for a banner line — which does not scale past a banner.
//!
//! QEMU only honours this when started with `-semihosting-config enable=on`;
//! without it `HLT #0xF000` is an unallocated instruction and traps to our own
//! vector table. See [`exiting`] for why that does not become a loop.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

/// `SYS_EXIT_EXTENDED` (semihosting spec §6.7). The plain `SYS_EXIT` (0x18)
/// carries the exit code in the reason word on AArch64 and cannot express one,
/// so the extended call is the only way to return a status.
const SYS_EXIT_EXTENDED: u64 = 0x20;
/// `ADP_Stopped_ApplicationExit`: "the application exited normally", the one
/// reason code whose second parameter word is an exit status (spec §6.6).
const ADP_STOPPED_APPLICATION_EXIT: u64 = 0x2_0026;

/// The image ran and every self-check passed.
pub const EXIT_OK: u32 = 0;
/// A self-check inside the image failed — wrong exception level, wrong load
/// address. The image reached its own code and disagreed with the machine.
pub const EXIT_SELF_CHECK: u32 = 1;
// 2 is deliberately unused: `scripts/qemu-kernel.sh` reserves it for "this QEMU
// has no raspi4b machine", which CI turns into a warning rather than a failure.
// An image that ever exited 2 would make a real failure look like a toolchain
// gap.
/// An exception was taken — see `vectors.rs`. Distinct from [`EXIT_SELF_CHECK`]
/// because a crash and a failed assertion want different first questions, and
/// distinct from a timeout because a hang and a crash used to look identical
/// from CI.
pub const EXIT_FAULT: u32 = 3;
/// A Rust `panic!` reached the panic handler.
pub const EXIT_PANIC: u32 = 4;

/// Set for as long as an `exit` is in flight. If semihosting is not enabled,
/// the `HLT` traps to the synchronous vector, which would call `exit` again:
/// the flag lets the handler notice it is the reason it was entered and park
/// instead of recursing forever.
static EXITING: AtomicBool = AtomicBool::new(false);

/// True once [`exit`] has issued its `HLT` — i.e. the trap we are handling may
/// be the semihosting call itself failing.
pub fn exiting() -> bool {
    EXITING.load(Ordering::Relaxed)
}

/// Stop the machine and hand `code` to the host as its process exit status.
///
/// Never returns: either the host tears the machine down, or semihosting is
/// disabled and we park (the caller has nothing useful left to do either way).
pub fn exit(code: u32) -> ! {
    // The parameter block is two 64-bit words: reason, then exit status. It may
    // live on the stack — the host reads it synchronously during the trap.
    let block: [u64; 2] = [ADP_STOPPED_APPLICATION_EXIT, code as u64];
    EXITING.store(true, Ordering::Relaxed);
    // SAFETY: `HLT #0xF000` is the architected AArch64 semihosting trap. If the
    // host does not implement it the instruction is unallocated and traps to
    // VBAR_EL2, which is a path we handle rather than a path that corrupts us.
    unsafe {
        asm!(
            "hlt #0xf000",
            inout("x0") SYS_EXIT_EXTENDED => _,
            in("x1") block.as_ptr(),
            options(nostack),
        );
    }
    park()
}

/// Halt without reporting anything. The last resort when semihosting is not
/// available; `scripts/qemu-kernel.sh`'s timeout is what ends the run then.
pub fn park() -> ! {
    loop {
        // SAFETY: `wfe` is unprivileged and has no memory effects.
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}
