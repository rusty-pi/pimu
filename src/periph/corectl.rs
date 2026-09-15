//! VPU core-control block at `0x7E00_2000`: one interrupt controller per VPU
//! core, core 0's bank at `+0x000` and core 1's at `+0x800`. Register map:
//! `specs/corectl.toml` ([`crate::spec::corectl`]).
//!
//! `start4.elf`'s entry trampoline runs on both VPU cores; they diverge on
//! `version` bit 16. Each writes its vector base to its own bank's `VBASE`
//! (`0x30` / `0x830`) early on.
//!
//! Core 1 sleeps until a start address is written to its bank's [`WAKEUP`]
//! (`0x834`). start4 does that itself, once, when its power manager first
//! powers domain `0x20000` — in a Linux boot shortly after `Booting Linux`, in
//! a firmware-only boot not at all (#72).
//!
//! Interrupt controller: `enable_irq_source(src, prio)` (start4 `0x3ED72374`)
//! stores a 4-bit priority/enable field per source into the words at
//! `0x10..0x20` (core 0) / `0x810..0x820` (core 1): `word = (src >> 3) & 3`,
//! `field = (src & 7) * 4`. A nonzero field enables the source at that
//! priority; the vector is the interrupt number, `64 + source`, not the field.
//! start4 enables source 64 (systimer, [`SYS_IRQ_SRC`]) at priority 1 and arms a
//! system-timer compare as its ThreadX tick.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::spec::corectl::{
    INSTANCE_STRIDE as CORE_STRIDE, IRQ_PENDING, IRQ_PENDING_BITS, IRQ_PENDING_BITS_COUNT,
    IRQ_PENDING_BITS_STRIDE, IRQ_PENDING_SOURCE_MASK, IRQ_PENDING_SOURCE_SHIFT,
    IRQ_PENDING_VALID_MASK, IRQ_PRIO, IRQ_PRIO_COUNT, IRQ_PRIO_STRIDE, VBASE, WAKEUP,
    WAKEUP_ADDR_MASK, WAKEUP_ADDR_SHIFT,
};
use crate::spec::Coverage;

/// Everything else in the bank is plain read-back storage.
pub const COVERAGE: Coverage = Coverage {
    block: "corectl",
    decoded: &[IRQ_PENDING, IRQ_PRIO, VBASE, WAKEUP, IRQ_PENDING_BITS],
};

// Register notes beyond what `specs/corectl.toml` records:
//
// * `IRQ_PRIO` — start4 numbers its sources from 64, folded back into these
//   four words by `(src >> 3) & 3`.
// * `VBASE` — core 1's copy is one `CORE_STRIDE` higher like every other
//   register in this block. `RVF_DBG_IRQEN` and the peripheral stub both show
//   core 1 writing `0x7E002830`, not `+0x38`; with the old `0x38` guess
//   `vbase[1]` was never populated, so core 1 could not be vectored at all.
// * `WAKEUP` — only core 1's copy starts anything: core 0 is already running
//   whenever firmware can write to this block.
// * `IRQ_PENDING_BITS` — start4 drives the pending bitmask with three helpers,
//   all of which pick the bank from a core-index argument:
//   `0x3ED01896(src, core)` raises the source in software (`|= 1 << bit`),
//   which is how the firmware posts an interrupt to a core, including the
//   inter-core reschedule IPI (source 78 for core 0, 79 for core 1, both with a
//   direct vector entry at `0x3EC3F8F4`); `0x3ED01792` is the acknowledge every
//   ISR performs on entry; `0x3ED01980` reads one bit back.
// * `IRQ_PENDING` — the generic per-source ISR dispatcher (`0x3EC3E9BC`) does
//   `r2 = [r29+12]` (the bank), `r0 = [r2+4]`, `btest r0, 8`, then `or r0, 64`
//   / 7-bit mask to get the source number, and indexes the handler table at
//   `gp+58004` with it.

/// The interrupt source start4 wires to the BCM system timer (compare channel
/// `src - SYS_IRQ_SRC`). Enabled via `enable_irq_source(64, 1)`.
pub const SYS_IRQ_SRC: u32 = 64;

#[derive(Default)]
pub struct CoreCtl {
    /// Source raised by a peripheral and not yet read by the dispatcher.
    pending_src: Option<u32>,
    storage: BTreeMap<u32, u32>,
    /// Start address last written to core 1's [`WAKEUP`], not yet acted on.
    core1_wake: Option<u32>,
    /// Last exception-vector base the firmware wrote for core 0 / core 1.
    pub vbase: [u32; 2],
    /// Sources newly raised in software through [`IRQ_PENDING_BITS`], as
    /// `(core, source)`, waiting to be vectored on that core.
    sw_raised: std::collections::VecDeque<(u32, u32)>,
    /// `RVF_DBG_IRQEN`, read once — this device is written from the step loop.
    dbg_irqen: bool,
}

impl CoreCtl {
    pub fn new() -> CoreCtl {
        CoreCtl {
            dbg_irqen: std::env::var_os("RVF_DBG_IRQEN").is_some(),
            ..CoreCtl::default()
        }
    }

    /// Present `src` (64..127) at [`IRQ_PENDING`] for the dispatcher to pick up.
    /// Call it as the source is vectored, never when it is merely queued: the
    /// register holds one value, and the dispatcher reads it only after its
    /// entry sequence.
    pub fn raise_source(&mut self, src: u32) {
        self.pending_src = Some(src);
    }

    /// Next `(core, source)` the firmware raised in software by setting a bit in
    /// [`IRQ_PENDING_BITS`]. Real hardware asserts the line as soon as the bit
    /// goes up; the model vectors it on the next step.
    pub fn take_sw_raised(&mut self) -> Option<(u32, u32)> {
        self.sw_raised.pop_front()
    }

    /// Where the firmware last told core 1 to start, once: the address written
    /// to core 1's [`WAKEUP`] since the previous call.
    pub fn take_core1_wake(&mut self) -> Option<u32> {
        self.core1_wake.take()
    }

    /// The 4-bit priority/enable field for interrupt source `src` (as numbered by
    /// start4, i.e. 64.. for the first word) in `core`'s bank. 0 = disabled: a
    /// source queued for that core waits until this is non-zero.
    pub fn irq_priority(&self, core: u32, src: u32) -> u8 {
        let word = core * CORE_STRIDE + IRQ_PRIO + ((src >> 3) % IRQ_PRIO_COUNT) * IRQ_PRIO_STRIDE;
        let field = (src & 7) * 4;
        ((self.storage.get(&word).copied().unwrap_or(0) >> field) & 0xF) as u8
    }
}

/// Split a window offset into `(core, offset within that core's bank)`.
fn bank(offset: u32) -> (u32, u32) {
    (offset / CORE_STRIDE, offset % CORE_STRIDE)
}

/// Index of the array element `off` addresses, for an array register at
/// `base` with `count` elements `stride` apart.
fn element(off: u32, base: u32, count: u32, stride: u32) -> Option<u32> {
    let rel = off.checked_sub(base)?;
    (rel % stride == 0 && rel / stride < count).then_some(rel / stride)
}

/// Decode a pending-bitmask offset into `(core, word)`; `word` 0 covers sources
/// 64..95 and word 1 sources 96..127.
fn pending_word(offset: u32) -> Option<(u32, u32)> {
    let (core, off) = bank(offset);
    let word = element(
        off,
        IRQ_PENDING_BITS,
        IRQ_PENDING_BITS_COUNT,
        IRQ_PENDING_BITS_STRIDE,
    )?;
    Some((core, word))
}

impl MmioDevice for CoreCtl {
    fn name(&self) -> &'static str {
        "core-ctl"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        if offset == IRQ_PENDING {
            // Read-to-clear: the dispatcher reads this once per entry, then the
            // handler acks the device itself.
            if let Some(src) = self.pending_src.take() {
                return Ok(IRQ_PENDING_VALID_MASK
                    | ((src << IRQ_PENDING_SOURCE_SHIFT) & IRQ_PENDING_SOURCE_MASK));
            }
        }
        Ok(self.storage.get(&offset).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        // `RVF_DBG_IRQEN=1`: decode writes to the interrupt-priority words back
        // into the `enable_irq_source(src, prio)` calls that produced them, for
        // core 0 (`0x10..0x20`) and core 1 (`0x810..0x820`). Which sources core 1
        // enables is how we find the inter-core doorbell's interrupt number.
        let (core, off) = bank(offset);
        let prio_word = element(off, IRQ_PRIO, IRQ_PRIO_COUNT, IRQ_PRIO_STRIDE);
        if let (true, Some(word)) = (self.dbg_irqen, prio_word) {
            let prev = self.storage.get(&offset).copied().unwrap_or(0);
            for f in 0..8u32 {
                let (a, b) = ((prev >> (f * 4)) & 0xF, (value >> (f * 4)) & 0xF);
                if a != b {
                    let src = word * 8 + f + 64;
                    eprintln!("[irqen] core{core} src={src} prio {a} -> {b}");
                }
            }
        }
        // A 0 -> 1 transition in a pending word is the firmware raising that
        // source on that core; queue it for delivery. Clearing bits is the
        // ISR's acknowledge and needs no action.
        if let Some((core, word)) = pending_word(offset) {
            let prev = self.storage.get(&offset).copied().unwrap_or(0);
            for bit in 0..32 {
                let mask = 1u32 << bit;
                if value & mask != 0 && prev & mask == 0 {
                    self.sw_raised
                        .push_back((core, SYS_IRQ_SRC + word * 32 + bit));
                }
            }
        }
        let value = if off == WAKEUP {
            value & (WAKEUP_ADDR_MASK << WAKEUP_ADDR_SHIFT)
        } else {
            value
        };
        self.storage.insert(offset, value);
        match (core, off) {
            (_, VBASE) => {
                if let Some(vbase) = self.vbase.get_mut(core as usize) {
                    *vbase = value;
                }
            }
            (1, WAKEUP) => self.core1_wake = Some(value),
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_to_core_1s_wakeup_is_its_start_address_once() {
        let mut c = CoreCtl::new();
        c.write(CORE_STRIDE + WAKEUP, Width::Word, 0xFEC0_0201)
            .unwrap();
        assert_eq!(c.take_core1_wake(), Some(0xFEC0_0200));
        assert_eq!(c.take_core1_wake(), None);
        assert_eq!(
            c.read(CORE_STRIDE + WAKEUP, Width::Word).unwrap(),
            0xFEC0_0200
        );
    }

    #[test]
    fn nothing_else_in_the_block_starts_core_1() {
        let mut c = CoreCtl::new();
        // Core 0's own copy, and the priority words the model used to guess
        // from: start4 writes 0x0100_1000 to IRQ_PRIO word 1 in every boot.
        c.write(WAKEUP, Width::Word, 0xFEC0_0200).unwrap();
        c.write(IRQ_PRIO + IRQ_PRIO_STRIDE, Width::Word, 0x0100_1000)
            .unwrap();
        c.write(CORE_STRIDE + VBASE, Width::Word, 0xFEC0_1E00)
            .unwrap();
        assert_eq!(c.take_core1_wake(), None);
    }
}
