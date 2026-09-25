//! The BCM2711 "ARM local" block at `0xFF80_0000` — as much of it as anything
//! on the ARM side actually touches, which is very little.
//!
//! Registers: `specs/armlocal.toml`.
//!
//! On BCM2836/7 this block *was* the interrupt controller. BCM2711 put a
//! GIC-400 next to it ([`super::gic`]) and Linux uses that instead: the device
//! tree still carries the node but without an `interrupt-controller` property,
//! so `irq-bcm2836` never probes. Secondary cores come up through the spin
//! table in RAM, and the generic timer's interrupt is GIC PPI 30 — `GICv2 30
//! Level arch_timer` in `/proc/interrupts` on a Raspberry Pi 4B d03115.
//!
//! The only writer is the armstub start4 places at ARM address 0: `ARM_CONTROL`
//! 0 and `CORE_TIMER_PRESCALER` `0x8000_0000`, next to `cntfrq_el0 = 54000000`.
//! With exactly those values that board's counter runs at the crystal rate
//! (`arch_timer: cp15 timer(s) running at 54.00MHz (phys)`), which is what
//! [`ArmLocal::counter_hz`] reports; any other configuration is deliberately
//! not modelled rather than guessed at.
//!
//! **Every other offset faults rather than reading zero**, so if Linux or the
//! stub touches something unexpected it is loud.

use crate::bus::{BusError, BusResult, MmioDevice, Width};

use crate::spec::armlocal::{ARM_CONTROL, CORE_TIMER_PRESCALER};
pub use crate::spec::armlocal::{BASE, SIZE};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "armlocal",
    decoded: &[ARM_CONTROL, CORE_TIMER_PRESCALER],
};

const STUB_CONTROL: u32 = 0;
const STUB_PRESCALER: u32 = 0x8000_0000;
pub const CRYSTAL_HZ: u64 = 54_000_000;

#[derive(Debug, Default)]
pub struct ArmLocal {
    control: u32,
    prescaler: u32,
}

impl ArmLocal {
    pub fn new() -> ArmLocal {
        ArmLocal::default()
    }

    /// The rate the ARM generic-timer counter (`CNTPCT`) advances at, for
    /// the configuration the armstub sets up; `None` for any other, which is
    /// not modelled.
    pub fn counter_hz(&self) -> Option<u64> {
        (self.control == STUB_CONTROL && self.prescaler == STUB_PRESCALER).then_some(CRYSTAL_HZ)
    }

    fn register(&mut self, offset: u32, width: Width, write: bool) -> BusResult<&mut u32> {
        let fault = |reason| BusError::Faulted {
            addr: BASE + offset,
            width,
            write,
            reason,
        };
        if width != Width::Word {
            return Err(fault("ARM local registers are word-access only"));
        }
        match offset {
            ARM_CONTROL => Ok(&mut self.control),
            CORE_TIMER_PRESCALER => Ok(&mut self.prescaler),
            _ => Err(fault(
                "ARM local register not modelled: Linux on BCM2711 uses the GIC",
            )),
        }
    }
}

impl MmioDevice for ArmLocal {
    fn name(&self) -> &'static str {
        "arm-local"
    }

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32> {
        self.register(offset, width, false).map(|r| *r)
    }

    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()> {
        *self.register(offset, width, true)? = value;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_armstub_writes_land_and_set_the_counter_rate() {
        let mut l = ArmLocal::new();
        l.write(ARM_CONTROL, Width::Word, 0).unwrap();
        assert_eq!(l.counter_hz(), None, "prescaler not written yet");
        l.write(CORE_TIMER_PRESCALER, Width::Word, 0x8000_0000)
            .unwrap();
        assert_eq!(l.read(CORE_TIMER_PRESCALER, Width::Word), Ok(0x8000_0000));
        assert_eq!(l.counter_hz(), Some(54_000_000));
    }

    #[test]
    fn everything_else_faults() {
        let mut l = ArmLocal::new();
        assert!(l.read(0x34, Width::Word).is_err());
        assert!(l.write(0x60, Width::Word, 1).is_err());
        assert!(l.read(ARM_CONTROL, Width::Byte).is_err());
    }
}
