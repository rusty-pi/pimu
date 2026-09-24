//! The four ARM↔VideoCore doorbells at `0x7E00_B840`, with the VPU's view of
//! the same four at `0x7E00_B940`.
//!
//! Register map: `specs/bell.toml` ([`crate::spec::bell`]).
//!
//! This is VCHIQ's wake path, and nothing else in the boot rings a bell. Each
//! side writes its messages into shared DRAM and then sets `fired` in one of
//! the receiver's four `remote_event`s; if the receiver has set `armed`, the
//! sender rings a doorbell so the receiver's wait is broken. Linux's
//! `remote_event_signal` writes `BELL2`, which raises the VPU; the firmware
//! writes `BELL0`, which raises `vchiq_doorbell_irq` on the ARM (`GIC_SPI 34`,
//! the `interrupts` of the `brcm,bcm2711-vchiq` node).
//!
//! A bell is one bit of state per index:
//!
//! * a **write** rings it, whatever value it carries (`writel(0, BELL2)`);
//! * a **read** returns [`BELL_RUNG_MASK`] if it was rung, and clears it —
//!   "This interrupt can be cleared by reading the relevant doorbell register"
//!   (BCM2711 ARM Peripherals, Table 118), and how `vchiq_doorbell_irq` acks.
//!
//! Direction goes by index, not by view: bells 0 and 1 are ARMC interrupts 2
//! and 3, bells 2 and 3 raise VPU source 94 — the whole ARM control block's
//! line, which is also the mailbox's, and why one stock ISR (`0xEC58302`)
//! reads both bells and then the mailbox. Which view a *sender* writes is not
//! settled by any source: start4's accessors take the view as an argument
//! (`0x7E00B840 + view * 0x100 + bell * 4`), so this model rings and clears one
//! bell per index whichever view the access came through. Every access either
//! side is known to make agrees with that reading.
//!
//! The block is carved out of [`crate::periph::armctrl`]'s window, and its VPU
//! view out of [`crate::periph::mbox`]'s, so it must be decoded ahead of both.

use crate::bus::{BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};
use crate::spec::bell::{BELL, BELL_COUNT, BELL_RUNG_MASK, BELL_STRIDE};
use crate::spec::Coverage;

/// Every register in `specs/bell.toml` is modelled.
pub const COVERAGE: Coverage = Coverage {
    block: "bell",
    decoded: &[BELL],
};

/// The VPU interrupt source bells 2 and 3 raise, shared with the mailbox.
pub const IRQ_SRC: u32 = crate::spec::bell::IRQ_VPU;
/// The GIC id of doorbell 0's line, which is the one VCHIQ's ARM side binds.
pub const IRQ_GIC: u32 = crate::spec::bell::IRQ_GIC_DOORBELL0;

/// Bells 0 and 1 go to the ARM, 2 and 3 to the VPU.
const TO_VPU: usize = 2;

#[derive(Default)]
pub struct Bell {
    /// Whether each bell has been rung and not yet read.
    rung: [bool; BELL_COUNT as usize],
    /// How many times each has been rung, for the run report: a firmware that
    /// never rings bell 0 is one whose VCHIQ peer is never woken.
    pub rings: [u64; BELL_COUNT as usize],
    /// Where [`Channel::Mbox`] goes — the doorbells are the other half of the
    /// same conversation.
    pub log: Log,
}

impl Bell {
    pub fn new() -> Bell {
        Bell::default()
    }

    /// Whether a bell the ARM takes is waiting. Level-triggered: it stays up
    /// until `vchiq_doorbell_irq` reads the register.
    pub fn arm_irq_asserted(&self) -> bool {
        self.rung[..TO_VPU].iter().any(|&r| r)
    }

    /// Whether a bell this core takes is waiting.
    pub fn vpu_irq_asserted(&self) -> bool {
        self.rung[TO_VPU..].iter().any(|&r| r)
    }

    /// Ring one bell, as a write to it does. The harness uses this to stand in
    /// for the ARM's `remote_event_signal`.
    pub fn ring(&mut self, bell: usize) {
        if bell < self.rung.len() {
            self.rung[bell] = true;
            self.rings[bell] += 1;
        }
    }

    /// Read and clear one bell, as `vchiq_doorbell_irq` does; whether it had
    /// been rung.
    pub fn take(&mut self, bell: usize) -> bool {
        match self.rung.get_mut(bell) {
            Some(rung) => std::mem::replace(rung, false),
            None => false,
        }
    }

    /// Which bell an offset names. Both views are decoded to the same offsets,
    /// so an access through either lands on the same bell.
    fn index(offset: u32) -> Option<usize> {
        let offset = offset & !3;
        (BELL..BELL + BELL_COUNT * BELL_STRIDE)
            .contains(&offset)
            .then(|| ((offset - BELL) / BELL_STRIDE) as usize)
    }
}

impl MmioDevice for Bell {
    fn name(&self) -> &'static str {
        "bell"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let Some(bell) = Bell::index(offset) else {
            return Ok(0);
        };
        let rung = self.take(bell);
        if rung {
            crate::log!(self.log, Channel::Mbox, "doorbell {bell} read and cleared");
        }
        Ok(if rung { BELL_RUNG_MASK } else { 0 })
    }

    fn write(&mut self, offset: u32, _width: Width, _value: u32) -> BusResult<()> {
        if let Some(bell) = Bell::index(offset) {
            self.ring(bell);
            crate::log!(self.log, Channel::Mbox, "doorbell {bell} rung");
        }
        Ok(())
    }
}
