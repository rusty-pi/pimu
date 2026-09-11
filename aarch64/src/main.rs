//! Bare-metal aarch64 frontend for the firmware model — issue #32, stages 2
//! and 3.
//!
//! Stage 2 established the environment: an image `qemu-system-aarch64 -machine
//! raspi4b -kernel` accepts, landing at the BCM2711 address the firmware
//! assumes, entered at EL2 (which stage 4 needs in order to install stage-2
//! translation and trap the guest's MMIO), with exception vectors and a way to
//! report a verdict.
//!
//! Stage 3 puts the VPU model in it. The same crate the hosted `recon` drives
//! runs here, over the same models, and boots the same `pieeprom.bin` +
//! `sd.img` — see `model.rs`. The four things `src/` leaves to a frontend are
//! set up below, in this order, because each needs the one before it:
//!
//!  1. [`heap`] — a `#[global_allocator]` over a fixed window of physical RAM.
//!  2. [`clock`] — `CNTPCT_EL0` as `rpi_virt_fw::time`'s microsecond source,
//!     without which `RunLimits::max_wall` reads zero and never trips.
//!  3. The sinks: `diag_eprintln!` and the modelled UART, both to the PL011.
//!  4. A `DiagConfig`, since `from_env` is hosted-only (in `model.rs`).
//!
//! With no `-initrd` the image is still stage 2: banner, self-checks, exit. It
//! is the bundle that turns it into a boot, so CI keeps a cheap check of the
//! environment and a separate, long one of the model.
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

extern crate alloc;

mod boot;
mod bundle;
mod clock;
mod heap;
mod initrd;
mod model;
mod semihost;
mod uart;
mod vectors;

use core::fmt::Write;

/// Everything the model allocates comes from here. See `heap.rs` for the
/// window and why a bump allocator is the right shape for this workload.
#[global_allocator]
static HEAP: heap::BumpHeap = heap::BumpHeap;

/// Seconds the model's run loop may take, baked in by `build.rs` from
/// `RVF_KERNEL_WALL` — the guest cannot ask QEMU how long `timeout` will give
/// it, so the budget travels in the image and is set a little under the host's
/// so a wedged boot reports rather than being killed.
pub fn run_budget_secs() -> u64 {
    env!("RVF_IMAGE_WALL_SECS").parse().unwrap_or(5)
}

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
        semihost::exit(semihost::EXIT_SELF_CHECK);
    }

    let _ = writeln!(con, "\nrpi-virt-fw: aarch64 frontend (issue #32 stage 3)");
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
    let _ = writeln!(
        con,
        "  VBAR_EL2    {:#010x} (16 x 0x80, see aarch64/src/vectors.rs)",
        vbar_el2()
    );
    let _ = writeln!(
        con,
        "  heap        {:#010x} .. {:#010x} ({} MiB, see aarch64/src/heap.rs)",
        heap::HEAP_BASE,
        heap::HEAP_END,
        heap::capacity() / (1024 * 1024),
    );
    let _ = writeln!(
        con,
        "  CNTFRQ_EL0  {} Hz (the max_wall clock)",
        clock::frequency_hz()
    );

    // `fault` is how the vector table is proven to work: without a deliberate
    // fault the handler is code nobody has ever executed. Keep it, so the next
    // change to it can be checked the same way.
    if cfg!(feature = "fault") {
        con.puts("fault feature: branching to an unmapped address on purpose\n");
        // 0x8000_0000_0000_0000 is outside the PA range the BCM2711 can
        // address, so the branch is an abort however the access is decoded.
        // x9 gets a recognisable value first, so the register dump is visibly
        // the faulting context and not the handler's.
        // SAFETY: never returns, and is meant not to.
        unsafe {
            core::arch::asm!(
                "movz x9, #0xbeef",
                "br {}",
                in(reg) 0x8000_0000_0000_0000u64,
                options(nostack),
            )
        };
    }

    // The exception level is this stage's load-bearing claim, so the image
    // checks it rather than the host grepping the banner for it: a host-side
    // grep cannot tell "printed EL1" from "printed nothing".
    if current_el() != 2 {
        con.puts("FAILED: not at EL2; stage-2 translation is unavailable\n");
        semihost::exit(semihost::EXIT_SELF_CHECK);
    }

    semihost::exit(boot_the_model(dtb, &mut con))
}

/// Stage 3 proper: find the blobs, wire the frontend seams, run the VPU model.
///
/// Returns the image exit status. No `-initrd` is not a failure — it is the
/// stage-2 image, which is a cheap and useful thing for CI to keep booting.
fn boot_the_model(dtb: u64, con: &mut uart::Uart) -> u32 {
    // SAFETY: `dtb` is `x0` as the bootloader left it; `locate` validates the
    // FDT magic before believing any of it.
    let found = unsafe { initrd::locate(dtb) };
    let initrd = match found {
        Ok(i) => i,
        Err(why) => {
            let _ = writeln!(con, "no firmware bundle: {why}");
            let _ = writeln!(
                con,
                "stage 2 only: pass -initrd <bundle> (scripts/make-blob-bundle.sh) to boot the model"
            );
            return semihost::EXIT_OK;
        }
    };
    let _ = writeln!(
        con,
        "  initrd      {:#010x} .. {:#010x} ({} KiB)",
        initrd.start,
        initrd.end,
        initrd.len() / 1024
    );

    // Both of these are about the heap window's two assumptions, and both are
    // cheap enough to check on every run because the alternative is a
    // misbehaving firmware a long way from the cause. See `heap.rs`.
    if !heap::check_clear_of(initrd.start, initrd.end) {
        let _ = writeln!(
            con,
            "FAILED: the -initrd blob overlaps the heap window at {:#x}; \
             the bundle has outgrown the gap QEMU leaves below it",
            heap::HEAP_BASE
        );
        return semihost::EXIT_SELF_CHECK;
    }
    if let Some(dirty) = heap::check_window_is_zero() {
        let _ = writeln!(
            con,
            "FAILED: heap window is not zero at {dirty:#x}; alloc_zeroed's \
             shortcut would hand the model dirty RAM"
        );
        return semihost::EXIT_SELF_CHECK;
    }

    // SAFETY: the extent came from the bootloader's own device tree and has
    // just been shown not to overlap anything this image allocates from.
    let raw = unsafe { initrd.bytes() };
    let bundle = match bundle::Bundle::parse(raw) {
        Ok(b) => b,
        Err(e) => {
            let _ = writeln!(con, "FAILED: -initrd: {}", e.as_str());
            return semihost::EXIT_SELF_CHECK;
        }
    };
    for blob in bundle.iter() {
        let _ = writeln!(con, "  blob        {:<16} {} bytes", blob.name, blob.len());
    }

    // The seams `src/` leaves open, installed before anything uses them.
    rpi_virt_fw::time::set_source(clock::now_us);
    rpi_virt_fw::diag::set_sink(diag_sink);
    rpi_virt_fw::diag::set_console_sink(console_sink);

    let outcome = model::run(&bundle, con);
    model::report(&outcome, con);

    let (live, high, failures) = heap::stats();
    let _ = writeln!(
        con,
        "heap        {} MiB live, {} MiB peak of {} MiB{}",
        live / (1024 * 1024),
        high / (1024 * 1024),
        heap::capacity() / (1024 * 1024),
        if failures == 0 {
            ""
        } else {
            " — EXHAUSTED (see aarch64/src/heap.rs)"
        }
    );

    if outcome.complete {
        let _ = writeln!(con, "\nthe VPU model reached the ARM hand-off bare-metal");
        semihost::EXIT_OK
    } else {
        let _ = writeln!(
            con,
            "\nFAILED: the boot stopped short (see the ticks above)"
        );
        semihost::EXIT_SELF_CHECK
    }
}

/// `diag_eprintln!` — the library's own diagnostics.
fn diag_sink(args: core::fmt::Arguments<'_>) {
    // SAFETY: one core, interrupts masked; the PL011 has no other user.
    let mut con = unsafe { uart::console() };
    let _ = con.write_fmt(args);
}

/// The modelled UART, byte for byte.
///
/// Not `puts`: that turns `\n` into `\r\n` for a terminal's benefit, and this
/// stream is the raw material of the golden transcript — the firmware emits
/// its own `\r\n` (visible in any `boot.log.console`), so adding another CR
/// would make the bare-metal transcript differ from the hosted one on every
/// single line.
fn console_sink(bytes: &[u8]) {
    // SAFETY: as above.
    let con = unsafe { uart::console() };
    for &b in bytes {
        con.putc(b);
    }
}

/// The vector table base actually in effect, read back rather than assumed —
/// `msr vbar_el2` from the entry stub is the sort of thing that silently does
/// not happen if the image is ever entered at EL1.
fn vbar_el2() -> u64 {
    let v: u64;
    // SAFETY: a system-register read with no side effects.
    unsafe { core::arch::asm!("mrs {}, vbar_el2", out(reg) v, options(nomem, nostack)) };
    v
}

extern "C" {
    /// The image's first byte, i.e. the arm64 Image header. Declared as a
    /// function because that is what the entry stub defines it as.
    fn _start() -> !;
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // A fresh handle rather than a shared one: panicking is exactly the moment
    // when the console's owner may be mid-write or gone.
    // SAFETY: we are terminal; no other user can matter any more.
    let mut con = unsafe { uart::console() };
    let _ = writeln!(con, "\nrpi-virt-fw: PANIC: {info}");
    // Its own status: a panic is the image disagreeing with itself, which is a
    // different first question from a fault (EXIT_FAULT) or a self-check.
    semihost::exit(semihost::EXIT_PANIC)
}
