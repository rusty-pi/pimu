//! The deadline registry ([`pimu::sched`]): which device the model owes work to
//! next. A source missing from the registry is a device the run loop can leave a
//! whole fast window past its deadline, so each one is checked here on its own and
//! then against the other — including the tie, which a compare has to win, since
//! [`pimu::machine::Machine`] services a frame by moving the counter and a compare
//! by firing it.

use pimu::bus::Bus;
use pimu::sched::{next_due, Timed};
use pimu::soc::bcm2711 as map;
use pimu::spec::hvs as hvs_reg;
use pimu::spec::systimer as sys_reg;
use pimu::Machine;

fn machine() -> Machine {
    Machine::new(1024 * 1024)
}

/// Arm system-timer compare 0 at `us`. The counter starts at 0 in a fresh
/// `Machine`, so the deadline is the value written.
fn arm_compare(m: &mut Machine, us: u32) {
    m.store32(map::SYSTIMER_BASE + sys_reg::C, us).unwrap();
}

/// Start HVS channel 0 and enable the interrupt for each of its frame events, the
/// state `NOTIFY_DISPLAY_DONE` needs (see [`pimu::periph::hvs`]).
fn run_hvs_channel0(m: &mut Machine) {
    m.store32(
        map::HVS_BASE + hvs_reg::DISPCTRLX,
        hvs_reg::DISPCTRLX_ENABLE_MASK,
    )
    .unwrap();
    m.store32(
        map::HVS_BASE + hvs_reg::DISPCTRL,
        hvs_reg::DISPCTRL_ENABLE_MASK
            | hvs_reg::DISPCTRL_DISPEIRQ0_MASK
            | hvs_reg::DISPCTRL_DSPEIVST0_MASK
            | hvs_reg::DISPCTRL_DSPEIEOF0_MASK
            | hvs_reg::DISPCTRL_DSPEIEOLN0_MASK,
    )
    .unwrap();
}

#[test]
fn an_idle_machine_owes_nothing() {
    let m = machine();
    assert_eq!(next_due(&m), None);
}

#[test]
fn an_armed_compare_is_due() {
    let mut m = machine();
    arm_compare(&mut m, 5_000);
    assert_eq!(next_due(&m), Some((5_000, Timed::SysTimer)));
}

#[test]
fn a_running_channel_is_due_at_its_first_frame_event() {
    let mut m = machine();
    run_hvs_channel0(&mut m);
    let (us, which) = next_due(&m).expect("a running channel has a deadline");
    assert_eq!(which, Timed::Hvs);
    assert!(us > 0 && us < pimu::periph::hvs::FRAME_US);
}

#[test]
fn the_earliest_source_wins() {
    let mut m = machine();
    run_hvs_channel0(&mut m);
    let frame = next_due(&m).expect("a running channel has a deadline").0;

    arm_compare(&mut m, frame as u32 + 1);
    assert_eq!(next_due(&m), Some((frame, Timed::Hvs)));

    arm_compare(&mut m, frame as u32 - 1);
    assert_eq!(next_due(&m), Some((frame - 1, Timed::SysTimer)));
}

/// A compare and a frame event in the same microsecond: the compare wins, so the
/// `sleep` path fires it instead of only moving the counter onto it.
#[test]
fn a_compare_wins_a_tie() {
    let mut m = machine();
    run_hvs_channel0(&mut m);
    let frame = next_due(&m).expect("a running channel has a deadline").0;

    arm_compare(&mut m, frame as u32);
    assert_eq!(next_due(&m), Some((frame, Timed::SysTimer)));
}
