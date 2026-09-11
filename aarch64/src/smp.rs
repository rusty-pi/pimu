//! Getting cores 1-3 out of the memory the model is about to claim.
//!
//! This exists because of #32 stage 4's central requirement: the model's RAM
//! has to *be* physical memory, which means physical 0 .. 1 GiB belongs to the
//! model and gets zeroed and then written for the whole boot. Three of
//! `raspi4b`'s four cores are executing code in there.
//!
//! # Where they are
//!
//! Not in this image. Under `-kernel`, `hw/arm/raspi.c` installs
//! `write_smpboot64()` as the board's `write_secondary_boot` hook, and
//! `do_cpu_reset()` starts every CPU but the first at `smp_loader_start`. That
//! stub is eleven instructions at physical 0x300, read back from the image on
//! QEMU 10.2.1 as:
//!
//! ```text
//!   0x300  d2801b05   mov  x5, #0xd8
//!   0x304  d53800a6   mrs  x6, mpidr_el1
//!   0x308  924004c6   and  x6, x6, #0x3
//!   0x30c  d503205f   spin: wfe
//!          f86678a4   ldr  x4, [x5, x6, lsl #3]
//!          b4ffffc4   cbz  x4, spin
//!          ...        mov  x0..x3, #0
//!          d61f0080   br   x4
//! ```
//!
//! so they poll an eight-byte release slot per CPU at 0xd8 and branch to
//! whatever address appears there. The table read back as all zeroes, which is
//! the "nobody has released us" state, and 0x300 and 0xd8 are both inside the
//! model's gigabyte.
//!
//! Left alone, the model's first write to low RAM turns that stub into
//! whatever the firmware happened to store, and three cores start executing
//! model RAM as instructions — with `VBAR_EL2` at its reset value of 0, so
//! even the exceptions vector back into it. They would not merely spin: a
//! stray store from one of them lands in the model's RAM, and the retired
//! instruction count stops matching the hosted run for reasons nothing in the
//! transcript would explain.
//!
//! # What we do about it
//!
//! Release them into [`__secondary_park`] — a `wfi` loop inside the *relocated*
//! image, above the model's gigabyte — and then wait for each to say it got
//! there. The waiting is the point: it turns "they are probably out of the way"
//! into evidence, one byte per CPU, before a single byte of model RAM is
//! written.
//!
//! # Porting
//!
//! [`release`] refuses to proceed when the stub's first instruction is not the
//! one above, because then it has no idea where the secondaries are. A machine
//! that powers its secondaries down over PSCI, or one that starts them at the
//! image entry instead, needs its own answer here; failing loudly is the only
//! honest thing to do with a machine whose cores are somewhere unknown while
//! we overwrite a gigabyte.
//!
//! # Stage 4 will have to undo this
//!
//! Linux brings up the Pi's secondaries through the same spin table
//! (`enable-method = "spin-table"`, `cpu-release-addr = <0xd8>`), and by then
//! 0xd8 is a location *inside the model's RAM* that the firmware also writes.
//! So the guest's releases will have to be mediated by the resident rather
//! than seen directly by a parked core polling model memory — the park loop
//! here deliberately polls nothing.

use crate::uart::Uart;
use core::fmt::Write;

/// First instruction of QEMU's `write_smpboot64()` stub, `mov x5, #0xd8`.
/// Recognising it is how [`release`] knows where the secondaries are.
const SMPBOOT_FIRST_INSN: u32 = 0xd280_1b05;
/// Where `hw/arm/raspi.c` writes that stub (`SMPBOOT_ADDR`).
const SMPBOOT_ADDR: usize = 0x300;
/// The release table the stub polls, indexed by `MPIDR_EL1 & 3`, eight bytes
/// per CPU (`ldr x4, [x5, x6, lsl #3]`).
const SPINTABLE_ADDR: usize = 0xd8;
/// `raspi4b` is a quad-core BCM2711.
const CPUS: usize = 4;

extern "C" {
    /// The `wfi` park in the relocated image. Only its address is used.
    fn __secondary_park() -> !;
    /// One byte per CPU, set by [`__secondary_park`] on arrival (boot.rs).
    static __secondaries_parked: [u8; CPUS];
}

/// Move cores 1-3 out of the model's gigabyte, and report whether they got
/// there.
///
/// Must be called before anything writes model RAM, and after the image has
/// been relocated (the park address it hands out is in the relocated copy).
///
/// # Safety
/// Writes the machine's spin-table release slots at [`SPINTABLE_ADDR`], which
/// is only meaningful on a machine whose secondaries are in the stub this
/// checks for.
pub unsafe fn release(con: &mut Uart) -> bool {
    let stub = core::ptr::read_volatile(SMPBOOT_ADDR as *const u32);
    if stub != SMPBOOT_FIRST_INSN {
        let _ = writeln!(
            con,
            "FAILED: no spin-table stub at {SMPBOOT_ADDR:#x} (read {stub:#010x}, \
             expected {SMPBOOT_FIRST_INSN:#010x}); this machine parks its secondary \
             CPUs somewhere aarch64/src/smp.rs does not know about, and the model \
             is about to overwrite the first gigabyte"
        );
        return false;
    }

    let park = __secondary_park as *const () as usize as u64;
    for cpu in 1..CPUS {
        core::ptr::write_volatile((SPINTABLE_ADDR + cpu * 8) as *mut u64, park);
    }
    // The stub's poll sits behind a `wfe`, so the release is not seen until an
    // event arrives. `dsb` first, or the `sev` can outrun the stores.
    core::arch::asm!("dsb sy", "sev", options(nostack, preserves_flags));

    // Bounded: a core that is not going to arrive must not hang the boot.
    // Generous because this is once per run and the alternative to waiting is
    // a corrupted model — measured arrival is immediate.
    for _ in 0..100_000_000u64 {
        if parked_count() == CPUS - 1 {
            let _ = writeln!(
                con,
                "  secondaries {}/{} parked at {:#010x} (QEMU's spin table at {:#x})",
                CPUS - 1,
                CPUS - 1,
                park,
                SPINTABLE_ADDR,
            );
            return true;
        }
        // A second `sev` costs nothing and covers the event register having
        // been set (and so consumed by a `wfe` that returned) before the
        // release was stored.
        core::arch::asm!("sev", options(nomem, nostack, preserves_flags));
    }

    let _ = writeln!(
        con,
        "FAILED: only {} of {} secondary CPUs reached the park at {:#010x}; \
         the rest are still executing inside the model's RAM",
        parked_count(),
        CPUS - 1,
        park,
    );
    false
}

/// How many secondaries have marked their byte.
///
/// `read_volatile`, because the writers are other CPUs: a plain read is a read
/// the optimiser may hoist out of the wait loop below, which would spin on a
/// value taken before the `sev`.
fn parked_count() -> usize {
    // SAFETY: a `.bss` array in this image; each byte has exactly one writer,
    // the CPU it belongs to, so a torn read is not possible.
    let arrived =
        |cpu: usize| unsafe { core::ptr::read_volatile(&raw const __secondaries_parked[cpu]) } != 0;
    (1..CPUS).filter(|&cpu| arrived(cpu)).count()
}
