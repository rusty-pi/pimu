//! The run loop's wall clock.
//!
//! `RunLimits::max_wall` is the stop condition every long boot actually ends
//! on, so it cannot simply be compiled out of the `no_std` build. But
//! `std::time::Instant` is hosted-only, so the clock is injected instead: the
//! hosted build reads `Instant`, and a bare-metal frontend installs a
//! microsecond source with [`set_source`] (on the Pi that is `CNTPCT_EL0`).
//!
//! With no source installed a `no_std` build's stopwatch reads zero forever,
//! which makes `max_wall` a no-op rather than an immediate timeout — a run
//! that cannot tell the time should not stop, and `max_steps` still bounds it.

use core::time::Duration;

/// A monotonic elapsed-time measurement, started at [`Stopwatch::start`].
#[derive(Debug, Clone, Copy)]
pub struct Stopwatch {
    #[cfg(feature = "std")]
    started: std::time::Instant,
    #[cfg(not(feature = "std"))]
    started_us: u64,
}

impl Stopwatch {
    pub fn start() -> Stopwatch {
        Stopwatch {
            #[cfg(feature = "std")]
            started: std::time::Instant::now(),
            #[cfg(not(feature = "std"))]
            started_us: now_us(),
        }
    }

    pub fn elapsed(&self) -> Duration {
        #[cfg(feature = "std")]
        {
            self.started.elapsed()
        }
        #[cfg(not(feature = "std"))]
        {
            Duration::from_micros(now_us().saturating_sub(self.started_us))
        }
    }
}

#[cfg(not(feature = "std"))]
mod source {
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// A monotonic microsecond counter.
    pub type Source = fn() -> u64;

    /// Null pointer = no source installed.
    static SOURCE: AtomicUsize = AtomicUsize::new(0);

    /// Install the monotonic clock. Idempotent; the last caller wins.
    pub fn set_source(f: Source) {
        SOURCE.store(f as usize, Ordering::Relaxed);
    }

    pub fn now_us() -> u64 {
        let p = SOURCE.load(Ordering::Relaxed);
        if p == 0 {
            return 0;
        }
        // SAFETY: `p` is non-null, so it was stored by `set_source` from a
        // `Source` and nothing else ever writes this slot.
        let f = unsafe { core::mem::transmute::<usize, Source>(p) };
        f()
    }
}

#[cfg(not(feature = "std"))]
pub use source::{now_us, set_source, Source};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_stopwatch_has_barely_run() {
        assert!(Stopwatch::start().elapsed() < Duration::from_secs(1));
    }
}
