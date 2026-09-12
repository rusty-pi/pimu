//! The BCM2711 "ARM local" block at `0xFF80_0000` — as much of it as anything
//! on the ARM side actually touches, which is very little.
//!
//! On BCM2836/7 this block *was* the interrupt controller: per-core timer and
//! mailbox interrupts, routed through `irq-bcm2836`. BCM2711 put a GIC-400
//! next to it ([`super::gic`]) and Linux on a Pi 4 uses that instead. The
//! device tree this firmware hands to Linux (`recon … --dump-fdt`) still
//! carries the node, but inertly:
//!
//! ```text
//!   interrupt-controller@40000000 {          // -> 0xFF80_0000 via soc ranges
//!       compatible = "brcm,bcm2836-l1-intc";
//!       reg = <0x40000000 0x100>;
//!       phandle = <0xc6>;
//!   };                                       // no `interrupt-controller;`
//! ```
//!
//! `of_irq_init` only initialises nodes that have the `interrupt-controller`
//! property, so `irq-bcm2836` never probes and nothing maps this region. The
//! other things that used it on earlier Pis are elsewhere on this one:
//!
//! * secondary cores are released through the spin table in RAM — every
//!   `cpu@n` has `enable-method = "spin-table"`, `cpu-release-addr = <0 0xd8>`
//!   … `<0 0xf0>` — not through the local block's mailboxes (the `/cpus`
//!   `enable-method = "brcm,bcm2836-smp"` is only read by 32-bit ARM Linux);
//! * the generic timer interrupt is GIC PPI 30 (`/proc/interrupts` on the
//!   reference board: `GICv2 30 Level arch_timer`), not the local block's
//!   per-core timer IRQ.
//!
//! What does write here is the armstub start4 places at ARM address 0 (file
//! offset `0x1E40EC` in `start4.elf`, the upstream `armstub8` shape), in EL3
//! before it drops to Linux:
//!
//! ```text
//!   ldr  x0, =0xff800000     // literal at 0x1E4194
//!   str  wzr, [x0]           // ARM_CONTROL = 0
//!   mov  w1, #0x80000000
//!   str  w1, [x0, #8]        // core timer prescaler = 0x8000_0000
//!   ldr  x0, =54000000       // literal at 0x1E419C
//!   msr  cntfrq_el0, x0
//! ```
//!
//! Those two registers are modelled as plain storage. With exactly those
//! values the reference board's counter runs at the crystal rate — its `dmesg`
//! says `arch_timer: cp15 timer(s) running at 54.00MHz (phys)`, matching the
//! `CNTFRQ` the stub programs — which [`ArmLocal::counter_hz`] reports for the
//! integration to clock the generic timer by. Any other configuration is not
//! modelled rather than guessed at.
//!
//! Every other offset faults instead of reading zero: the claim here is that
//! nothing else is touched, and if Linux or the stub disagrees, that should
//! be loud. Reset values were not measured (nothing the model does may read
//! the board); the stub writes both registers before anything could read them.

use crate::bus::{BusError, BusResult, MmioDevice, Width};

use crate::spec::armlocal::{ARM_CONTROL, CORE_TIMER_PRESCALER};
/// The dtb's `reg = <0x40000000 0x100>`, through `soc`'s `ranges`.
pub use crate::spec::armlocal::{BASE, SIZE};
use crate::spec::Coverage;

/// Both registers the spec lists; every other offset faults.
pub const COVERAGE: Coverage = Coverage {
    block: "armlocal",
    decoded: &[ARM_CONTROL, CORE_TIMER_PRESCALER],
};

/// What the armstub writes, and with which the counter runs at the crystal.
const STUB_CONTROL: u32 = 0;
const STUB_PRESCALER: u32 = 0x8000_0000;
/// The Pi 4's 54 MHz crystal — the armstub's `CNTFRQ` literal.
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
        // LOCAL_TIMER_CONTROL and a core's IRQ source on BCM2836: unused here.
        assert!(l.read(0x34, Width::Word).is_err());
        assert!(l.write(0x60, Width::Word, 1).is_err());
        assert!(l.read(ARM_CONTROL, Width::Byte).is_err());
    }
}
