//! Every `start4.elf` address the model knows, in one place — and there are
//! none left.
//!
//! The bench exists to run **two different firmware builds** through the same
//! model and diff what they produce (#5, rpi-mkosi#37). Any address baked into
//! the model is therefore a liability: on a build where it moved, the model
//! keeps running and quietly does the wrong thing. Collecting them here makes a
//! firmware bump a diff in one file instead of archaeology across the run loop
//! (#25).
//!
//! # Two kinds, and the difference matters
//!
//! **`*_GP_OFFSET` — data, and derivable.** These are offsets into `.sdata`
//! from the `gp` register, which the firmware itself establishes. Resolving
//! them against the live `gp` keeps them right while `.sdata` stays laid out
//! the same, but a build that reorders `.sdata` still moves them. The last one,
//! the ThreadX-SMP dispatch global that gated core 1's start, went with #72:
//! start4 wakes core 1 itself through `IC1_WAKEUP`, and the model now honours
//! that register instead ([`crate::periph::corectl`]).
//!
//! **`*_PC` — code, and not derivable.** Addresses the run loop matched against
//! the program counter, to correct for something the model did differently from
//! the hardware. None are left: the last went with #25, once the model took and
//! left exceptions the way the hardware does.
//!
//! Don't add either kind back; model the hardware behaviour the firmware relies
//! on instead.
//!
//! A third group — the addresses the `RVF_DBG_*` and `RVF_TRAP` diagnostics
//! watch — is deliberately *not* here. Those are reconnaissance aids: on a
//! firmware where they have moved they simply print nothing, which is a
//! non-event. See `docs/diagnostics.md`.
