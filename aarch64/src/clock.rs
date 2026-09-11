//! The wall clock the run loop stops on, from the ARM generic timer.
//!
//! `RunLimits::max_wall` is the condition a long boot actually ends on, and
//! `src/time.rs` leaves the clock to the frontend: with no source installed a
//! `no_std` build reads zero forever, which silently turns `max_wall` into a
//! no-op. So this is not optional plumbing — without it a wedged boot runs
//! until QEMU's own timeout, with no report.
//!
//! `CNTPCT_EL0` is the physical counter and `CNTFRQ_EL0` its frequency in Hz
//! (Arm ARM D17.8). QEMU's `raspi4b` reports 54 MHz, the BCM2711's crystal
//! rate, but the divisor is read rather than assumed — the same image is meant
//! to run on real silicon under KVM, where `CNTFRQ_EL0` is whatever the
//! firmware programmed.
//!
//! Note what this measures: *host* wall time, not the modelled time the
//! firmware sees. That is the same split the hosted build has (`Instant` vs
//! the modelled SysTimer), and it is the one `max_wall` wants — it is a budget
//! on how long the emulator may take, not on how long the firmware thinks it
//! took.

/// `CNTFRQ_EL0`, in Hz.
fn cntfrq() -> u64 {
    let v: u64;
    // SAFETY: a system-register read with no side effects. Readable at EL2.
    unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) v, options(nomem, nostack)) };
    v
}

/// `CNTPCT_EL0`, the physical counter.
fn cntpct() -> u64 {
    let v: u64;
    // `isb` first: the counter read is allowed to be speculated ahead of
    // earlier instructions otherwise, which turns a short interval into a
    // negative one (Arm ARM D11.2.2 recommends this for exactly this use).
    // SAFETY: a system-register read with no side effects.
    unsafe { core::arch::asm!("isb", "mrs {}, cntpct_el0", out(reg) v, options(nomem, nostack)) };
    v
}

/// Microseconds since the counter started, for `rpi_virt_fw::time::set_source`.
///
/// `u128` for the multiply: at 54 MHz a `u64` product overflows after about
/// four days of uptime, which is longer than any run but not a limit worth
/// having when the wider multiply costs nothing at this call rate.
pub fn now_us() -> u64 {
    let freq = cntfrq();
    if freq == 0 {
        // A machine that does not report a frequency cannot be converted to
        // microseconds. Reading zero is what `src/time.rs` already does with no
        // source installed: `max_wall` stops bounding the run, `max_steps`
        // still does. Better than a division fault.
        return 0;
    }
    ((cntpct() as u128 * 1_000_000) / freq as u128) as u64
}

/// The counter frequency, for the banner — this is the one number that says
/// whether the clock above means anything.
pub fn frequency_hz() -> u64 {
    cntfrq()
}
