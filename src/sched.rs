//! When the model next has timed work.
//!
//! A peripheral whose state changes with the clock rather than with an access —
//! a compare that is about to fire, a frame that is about to end — has to be
//! reached before the counter passes it. Two questions depend on that: how long
//! [`Emulator::fast_steps`](crate::emulator::Emulator::fast_steps) may leave the
//! core alone, and where a `sleep` jumps the counter to.
//!
//! This module is the single list of those sources, so a new timed peripheral is
//! one entry here instead of a clause in each expression that asks the question.
//! Nothing here holds state: every source derives its deadline from the device
//! that owns it, so a schedule cannot go stale behind the write that armed it.

use crate::machine::Machine;

/// A source of timed work, named so the caller can service the one that is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timed {
    /// A system-timer compare channel.
    SysTimer,
    /// An HVS end-of-frame flag that holds source
    /// [`hvs::IRQ_SRC`](crate::periph::hvs::IRQ_SRC).
    Hvs,
}

/// When a source next has work, or `None` while it has nothing armed.
type Deadline = fn(&Machine) -> Option<u64>;

/// Every source of timed work. Order decides ties: the first entry wins, which
/// keeps a compare ahead of a frame that ends in the same microsecond.
const SOURCES: &[(Timed, Deadline)] = &[
    (Timed::SysTimer, |m| m.systimer.next_deadline()),
    (Timed::Hvs, |m| m.hvs.deadline()),
];

/// The earliest modelled time at which some device has work, and which device.
pub fn next_due(m: &Machine) -> Option<(u64, Timed)> {
    SOURCES
        .iter()
        .filter_map(|&(which, deadline)| deadline(m).map(|us| (us, which)))
        .min_by_key(|&(us, _)| us)
}
