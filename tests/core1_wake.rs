//! VPU core 1 starts where the firmware writes its `IC1_WAKEUP` register, and
//! nowhere else (#72).
//!
//! start4 wakes core 1 itself, once, through core 1's copy of the core-control
//! `WAKEUP` register (`0x7E00_2834`), with its own entry point. Nothing else in
//! the block starts it, core 0's copy included.

use rpi_virt_fw::bus::Bus;
use rpi_virt_fw::emulator::{Emulator, RunEnd, RunLimits};
use rpi_virt_fw::soc::bcm2711::{CORECTL_BASE, UART0_BASE};
use rpi_virt_fw::Machine;

const CODE: u32 = 0x1000;
const CORE1: u32 = 0x4000;
/// Core 1's bank is 0x800 above core 0's; `WAKEUP` is at 0x34 in each.
const IC1_WAKEUP: u32 = CORECTL_BASE + 0x834;
const IC0_WAKEUP: u32 = CORECTL_BASE + 0x034;

/// `b .`
const SPIN: u16 = 0x1F00;

/// `st rd, (rs)`
const fn st(rd: u16, rs: u16) -> u16 {
    0x0900 | (rs << 4) | rd
}

/// `mov rd, #imm32`, the 48-bit form.
fn mov32(rd: u16, v: u32) -> [u16; 3] {
    [0xE800 | rd, v as u16, (v >> 16) as u16]
}

fn load(m: &mut Machine, at: u32, code: &[u16]) {
    for (i, h) in code.iter().enumerate() {
        m.store16(at + 2 * i as u32, *h).unwrap();
    }
}

/// Core 0 writes `CORE1` (with bit 0 set, which the register drops) to
/// `wakeup`, then spins. Core 1's code, at `CORE1`, prints `#` and spins.
fn run(wakeup: u32) -> (Emulator, RunEnd) {
    let mut m = Machine::new(1 << 20);
    let core0: Vec<u16> = [mov32(0, CORE1 | 1), mov32(1, wakeup)]
        .concat()
        .into_iter()
        .chain([st(0, 1), SPIN])
        .collect();
    load(&mut m, CODE, &core0);
    let core1: Vec<u16> = [mov32(6, UART0_BASE), mov32(7, u32::from(b'#'))]
        .concat()
        .into_iter()
        .chain([st(7, 6), SPIN])
        .collect();
    load(&mut m, CORE1, &core1);
    let mut emu = Emulator::new(m, CODE);
    let report = emu.run(&RunLimits {
        max_steps: Some(10_000),
        until: Some("#".into()),
        ..RunLimits::default()
    });
    (emu, report.end)
}

#[test]
fn a_write_to_ic1_wakeup_starts_core_1_there() {
    let (emu, end) = run(IC1_WAKEUP);
    assert_eq!(end, RunEnd::Until);
    let core1 = emu.cpu1.as_ref().expect("core 1 is running");
    assert!((CORE1..CORE1 + 0x20).contains(&core1.pc()));
}

#[test]
fn core_0s_own_wakeup_leaves_core_1_asleep() {
    let (emu, end) = run(IC0_WAKEUP);
    assert_eq!(end, RunEnd::StepLimit);
    assert!(emu.cpu1.is_none());
}
