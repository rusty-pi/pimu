//! Every `start4.elf` address the model knows, in one place.
//!
//! The bench exists to run **two different firmware builds** through the same
//! model and diff what they produce (#5, rpi-mkosi#37). Any address baked into
//! the model is therefore a liability: on a build where it moved, the model
//! keeps running and quietly does the wrong thing. Collecting them here makes a
//! firmware bump a diff in one file instead of archaeology across the run loop
//! (#25).
//!
//! Read at `4a2c9f5` from the `start4.elf` in `firmware/`, the build
//! `scripts/fetch-firmware.sh` pins.
//!
//! # Two kinds, and the difference matters
//!
//! **`*_GP_OFFSET` — data, and derivable.** These are offsets into `.sdata`
//! from the `gp` register, which the firmware itself establishes. Resolve them
//! against the live `gp` and they stay correct when `.sdata` moves. There is no
//! excuse for pinning one of these, and doing so is what version-locked the
//! core-1 spawn gate until `16852e3`.
//!
//! **`*_PC` — code, and not derivable.** Addresses the run loop matched against
//! the program counter, to correct for something the model did differently from
//! the hardware. None are left: the last went with #25, once the model took and
//! left exceptions the way the hardware does. Don't add one back; model the
//! hardware behaviour the firmware relies on instead.
//!
//! A third group — the addresses the `RVF_DBG_*` and `RVF_TRAP` diagnostics
//! watch — is deliberately *not* here. Those are reconnaissance aids: on a
//! firmware where they have moved they simply print nothing, which is a
//! non-event. See `docs/diagnostics.md`.

/// `[gp+3672]`: the ThreadX-SMP dispatch-module global, holding the per-core
/// scheduler object once `_tx_thread_smp` init registers it.
///
/// Core 1's first instructions after the trampoline do `b *([[gp+3672]] + 24)`,
/// so spawning it before this is populated branches through a null vtable. The
/// spawn is gated on it being non-zero.
pub const SMP_DISPATCH_GP_OFFSET: u32 = 3672;

#[cfg(test)]
mod tests {
    use super::*;

    /// The absolutes these offsets resolved to in the pinned build. Not a
    /// contract — a firmware bump is expected to change them — but it pins the
    /// arithmetic, so a typo in an offset is caught rather than silently
    /// shifting the model onto the wrong word.
    #[test]
    fn the_gp_offsets_resolve_to_the_addresses_they_were_read_from() {
        const GP: u32 = 0x3EE0_2D20; // start of .sdata in the pinned build
        assert_eq!(GP + SMP_DISPATCH_GP_OFFSET, 0x3EE0_3B78);
    }
}
