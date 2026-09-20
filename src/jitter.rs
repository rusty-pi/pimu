//! The nondeterminism a board has and the model does not.
//!
//! Every interval in here is modelled as one number — a block takes 21 µs on
//! the wire, a SuperSpeed link trains in 100 ms, an I²C byte takes as long as
//! the divider says. Silicon is not so even: a card stalls on its own
//! housekeeping, a hub reports a port late, a drive answers the fourth
//! command slower than the first three. Firmware that only ever meets the
//! even version can carry an ordering assumption for years and still boot
//! every time here.
//!
//! With `--jitter <seed>` each of those intervals is stretched by a factor
//! drawn from a seeded generator, so a run is reproducible but no longer
//! even. Nothing is shortened: a stretch can only make the firmware wait
//! longer than it expected, never less, so anything that breaks under it is
//! the firmware believing something finishes by a certain time.
//!
//! The yardstick is the stock firmware, not the model: run stock and ours on
//! the same case with the same seed. A seed both survive says nothing; a seed
//! stock survives and ours does not is ours to fix; a seed neither survives
//! means the stretch went past what the hardware would do, and the bounds
//! here are wrong.
//!
//! The state is thread-local because a seed belongs to one run and the test
//! binary runs several at once.

use std::cell::RefCell;

use crate::log::{Channel, Log};

/// How much an interval can be stretched, as hundredths. A draw is uniform
/// over this range, so most intervals land within a factor of four.
const MIN_PERCENT: u64 = 100;
const MAX_PERCENT: u64 = 400;
/// One draw in this many is a long tail instead: the card that disappears for
/// a moment, the device that turns up two polls later than it should.
const TAIL_IN: u64 = 16;
const TAIL_PERCENT: u64 = 800;
/// A device that answers its own commands — a card, a drive — is not only
/// uneven, it can be *slow*: a flash program, a block the controller has to
/// relocate, a unit that is not ready yet. One draw in this many of those
/// takes up to a thousand times as long, which is the range firmware
/// timeouts are written for and the model has never made them meet.
const SLOW_IN: u64 = 64;
const SLOW_MAX_PERCENT: u64 = 100_000;

struct State {
    seed: u64,
    /// How many intervals have been stretched, and by how much in total.
    stretched: usize,
    added_us: u64,
    /// How often a fault has been injected, and one in how many chances
    /// takes one. Zero is no faults at all, which is the default: a stretch
    /// can only cost time, while a fault asks the firmware to recover, and
    /// the two are worth telling apart when a seed fails.
    faults: usize,
    fault_in: u64,
    log: Log,
}

thread_local! {
    static JITTER: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Start jittering this thread's run. Seed 0 is as good as any other: the
/// generator is only unable to leave the one state, so that seed stands in
/// for the golden ratio's, and every other seed is its own run.
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

/// Stop jittering this thread's run, which the tests lean on to start from
/// a known state whatever order they run in.
pub fn disarm() {
    JITTER.with(|j| *j.borrow_mut() = None);
}

pub fn is_on() -> bool {
    JITTER.with(|j| j.borrow().is_some())
}

/// Inject a fault about one time in `one_in`, on top of the stretching.
/// Only the kinds a board really produces belong here — a frame that never
/// arrives, a block that fails its CRC, an endpoint that stalls — because a
/// failure under anything else says nothing about the firmware.
pub fn set_faults(one_in: u64) {
    JITTER.with(|j| {
        if let Some(state) = j.borrow_mut().as_mut() {
            state.fault_in = one_in;
        }
    });
}

/// Whether this is one of the times something goes wrong. False whenever
/// jitter is off or no fault rate was asked for.
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

/// How many intervals have been stretched, by how much altogether, and how
/// many faults were injected.
pub fn report() -> (usize, u64, usize) {
    JITTER.with(|j| {
        j.borrow()
            .as_ref()
            .map_or((0, 0, 0), |s| (s.stretched, s.added_us, s.faults))
    })
}

/// `us` as the hardware might have taken it: the same when jitter is off.
/// `what` names the interval on the `jitter` log channel.
pub fn stretch(us: u64, what: &'static str) -> u64 {
    scale(us, what, false)
}

/// The same for an interval a device answers in its own time — a card block,
/// a drive's completion — where the tail runs to a thousand times the even
/// figure.
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

/// xorshift64*, which is enough for picking delays and keeps a run
/// reproducible from its seed alone.
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
        // Seed 0 is a run like any other, not a dead generator.
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
