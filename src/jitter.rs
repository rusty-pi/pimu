//! The nondeterminism a board has and the model does not.
//!
//! Every interval here is one number, and silicon is not so even: firmware that
//! only meets the even version can carry an ordering assumption for years and
//! still boot every time. `--jitter <seed>` stretches each interval by a factor
//! from a seeded generator, so a run stays reproducible, and nothing is ever
//! shortened — anything that breaks under it is the firmware believing something
//! finishes by a certain time. The yardstick is the stock firmware: a seed stock
//! survives and ours does not is ours to fix; a seed neither survives means the
//! bounds here are wrong.

use std::cell::RefCell;

use crate::log::{Channel, Log};

/// How much an interval can be stretched, as hundredths, drawn uniformly.
const MIN_PERCENT: u64 = 100;
const MAX_PERCENT: u64 = 400;
const TAIL_IN: u64 = 16;
const TAIL_PERCENT: u64 = 800;
/// A device that answers its own commands can also be *slow* — a flash
/// program, a relocated block, a unit not ready yet. One draw in this many
/// takes up to a thousand times as long, the range firmware timeouts are
/// written for.
const SLOW_IN: u64 = 64;
const SLOW_MAX_PERCENT: u64 = 100_000;

struct State {
    seed: u64,
    stretched: usize,
    added_us: u64,
    /// How often a fault has been injected, and one in how many chances takes
    /// one. Zero, the default, is no faults: a stretch only costs time, while a
    /// fault asks the firmware to recover, and a failing seed should say which.
    faults: usize,
    fault_in: u64,
    log: Log,
}

thread_local! {
    static JITTER: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Start jittering this thread's run; seed 0 is as good as any other, standing
/// in for the generator's one dead state.
pub fn arm(seed: u64, log: Log) {
    JITTER.with(|j| {
        *j.borrow_mut() = Some(State {
            seed: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
            stretched: 0,
            added_us: 0,
            faults: 0,
            fault_in: 0,
            log,
        });
    });
}

pub fn disarm() {
    JITTER.with(|j| *j.borrow_mut() = None);
}

pub fn is_on() -> bool {
    JITTER.with(|j| j.borrow().is_some())
}

/// Inject a fault about one time in `one_in`, on top of the stretching. Only
/// the kinds a board really produces belong here — a lost frame, a bad CRC, a
/// stalled endpoint — since anything else says nothing about the firmware.
pub fn set_faults(one_in: u64) {
    JITTER.with(|j| {
        if let Some(state) = j.borrow_mut().as_mut() {
            state.fault_in = one_in;
        }
    });
}

pub fn fault(what: &'static str) -> bool {
    JITTER.with(|j| {
        let mut held = j.borrow_mut();
        let Some(state) = held.as_mut() else {
            return false;
        };
        if state.fault_in == 0 {
            return false;
        }
        if !draw(&mut state.seed).is_multiple_of(state.fault_in) {
            return false;
        }
        state.faults += 1;
        crate::log!(state.log, Channel::Jitter, "fault: {what}");
        true
    })
}

pub fn report() -> (usize, u64, usize) {
    JITTER.with(|j| {
        j.borrow()
            .as_ref()
            .map_or((0, 0, 0), |s| (s.stretched, s.added_us, s.faults))
    })
}

/// `us` as the hardware might have taken it; `what` names it in the log.
pub fn stretch(us: u64, what: &'static str) -> u64 {
    scale(us, what, false)
}

/// The same for an interval a device answers in its own time, where the tail
/// runs to a thousand times the even figure.
pub fn stretch_slow(us: u64, what: &'static str) -> u64 {
    scale(us, what, true)
}

fn scale(us: u64, what: &'static str, slow: bool) -> u64 {
    if us == 0 {
        return us;
    }
    JITTER.with(|j| {
        let mut held = j.borrow_mut();
        let Some(state) = held.as_mut() else {
            return us;
        };
        let percent = match draw(&mut state.seed) {
            r if slow && r.is_multiple_of(SLOW_IN) => {
                MIN_PERCENT + draw(&mut state.seed) % (SLOW_MAX_PERCENT - MIN_PERCENT + 1)
            }
            r if r.is_multiple_of(TAIL_IN) => TAIL_PERCENT,
            r => MIN_PERCENT + r % (MAX_PERCENT - MIN_PERCENT + 1),
        };
        let out = us.saturating_mul(percent) / 100;
        state.stretched += 1;
        state.added_us += out - us;
        crate::log!(
            state.log,
            Channel::Jitter,
            "{what}: {us} us -> {out} us ({percent} %)"
        );
        out
    })
}

fn draw(seed: &mut u64) -> u64 {
    let mut x = *seed;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *seed = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet() -> Log {
        Log::default()
    }

    #[test]
    fn off_by_default_and_exact() {
        disarm();
        assert!(!is_on());
        assert_eq!(stretch(21, "test"), 21);
        assert_eq!(report(), (0, 0, 0));
        assert!(!fault("test"));
    }

    #[test]
    fn a_slow_device_has_a_long_tail() {
        arm(11, quiet());
        let mut longest = 0;
        for _ in 0..5000 {
            longest = longest.max(stretch_slow(100, "test"));
        }
        assert!(longest > 8 * 100, "no tail: {longest}");
        arm(11, quiet());
        let even = (0..5000).map(|_| stretch(100, "test")).max().unwrap_or(0);
        assert!(
            even <= 8 * 100,
            "an even interval must stay inside the tail: {even}"
        );
    }

    #[test]
    fn a_stretch_only_ever_adds() {
        arm(7, quiet());
        for _ in 0..1000 {
            let out = stretch(100, "test");
            assert!((100..=800).contains(&out), "{out}");
        }
        let (count, added, _) = report();
        assert_eq!(count, 1000);
        assert!(added > 0);
    }

    #[test]
    fn the_same_seed_draws_the_same_run() {
        let run = || {
            arm(42, quiet());
            (0..20).map(|_| stretch(1000, "test")).collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
        arm(43, quiet());
        let other = (0..20).map(|_| stretch(1000, "test")).collect::<Vec<_>>();
        assert_ne!(run(), other);
        arm(0, quiet());
        let zero = (0..20).map(|_| stretch(1000, "test")).collect::<Vec<_>>();
        assert!(zero.iter().any(|&us| us != 1000));
    }

    #[test]
    fn faults_only_happen_when_asked_for() {
        arm(3, quiet());
        assert!((0..500).all(|_| !fault("test")), "none without a rate");
        set_faults(4);
        let hits = (0..2000).filter(|_| fault("test")).count();
        assert!(
            (300..=700).contains(&hits),
            "one in four, give or take: {hits}"
        );
        assert_eq!(report().2, hits);
    }

    #[test]
    fn zero_stays_zero() {
        arm(1, quiet());
        assert_eq!(stretch(0, "test"), 0);
    }
}
