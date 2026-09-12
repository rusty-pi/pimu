//! VPU core-control block at `0x7E00_2000`. This region carries both the
//! per-core boot handshake and the VPU interrupt controller. Register map:
//! `specs/corectl.toml` ([`crate::spec::corectl`]).
//!
//! `start4.elf`'s entry trampoline runs on both VPU cores; they diverge on
//! `version` bit 16. Core 0 writes its vector base to offset `0x30` (core 1's
//! copy would use `0x38`) early on, then continues the main boot.
//!
//! Interrupt controller: `enable_irq_source(src, prio)` (start4 `0x3ED72374`)
//! stores a 4-bit priority/enable field per source into the words at
//! `0x10..0x20` (core 0) / `0x810..0x820` (core 1): `word = (src >> 3) & 3`,
//! `field = (src & 7) * 4`. A nonzero field means "enabled, dispatch through
//! vector-table slot `prio`". start4 enables source 64 (systimer, [`SYS_IRQ_SRC`])
//! at priority 1 and arms a system-timer compare as its ThreadX tick.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::spec::corectl::{
    INSTANCE_STRIDE as CORE_STRIDE, IRQ_PENDING, IRQ_PENDING_BITS, IRQ_PENDING_BITS_COUNT,
    IRQ_PENDING_BITS_STRIDE, IRQ_PENDING_SOURCE_MASK, IRQ_PENDING_SOURCE_SHIFT,
    IRQ_PENDING_VALID_MASK, IRQ_PRIO, IRQ_PRIO_COUNT, IRQ_PRIO_STRIDE, VBASE,
};
use crate::spec::Coverage;

/// Everything else in the bank is plain read-back storage.
pub const COVERAGE: Coverage = Coverage {
    block: "corectl",
    decoded: &[IRQ_PENDING, IRQ_PRIO, VBASE, IRQ_PENDING_BITS],
};

// Register notes beyond what `specs/corectl.toml` records:
//
// * `IRQ_PRIO` — start4 numbers its sources from 64, folded back into these
//   four words by `(src >> 3) & 3`. Its first two words double as the model's
//   core-1 release trigger, see `write`.
// * `VBASE` — core 1's copy is one `CORE_STRIDE` higher like every other
//   register in this block. `RVF_DBG_IRQEN` and the peripheral stub both show
//   core 1 writing `0x7E002830`, not `+0x38`; with the old `0x38` guess
//   `vbase[1]` was never populated, so core 1 could not be vectored at all.
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

/// Run-state field words. A write of a code address here releases core 1 (small
/// values are the interrupt-controller priority words, not a release vector).
const RUNSTATE_LO: u32 = IRQ_PRIO;
const RUNSTATE_HI: u32 = IRQ_PRIO + IRQ_PRIO_STRIDE;

/// The interrupt source start4 wires to the BCM system timer (compare channel
/// `src - SYS_IRQ_SRC`). Enabled via `enable_irq_source(64, 1)`.
pub const SYS_IRQ_SRC: u32 = 64;

#[derive(Default)]
pub struct CoreCtl {
    /// Source raised by a peripheral and not yet read by the dispatcher.
    pending_src: Option<u32>,
    storage: BTreeMap<u32, u32>,
    pending_core1_release: bool,
    core1_started: bool,
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

    /// Non-destructive view of [`Self::raise_source`]'s pending value, for
    /// diagnostics (the register itself is read-to-clear).
    pub fn peek_pending(&self) -> Option<u32> {
        self.pending_src
    }

    /// Next `(core, source)` the firmware raised in software by setting a bit in
    /// [`IRQ_PENDING_BITS`]. Real hardware asserts the line as soon as the bit
    /// goes up; the model vectors it on the next step.
    pub fn take_sw_raised(&mut self) -> Option<(u32, u32)> {
        self.sw_raised.pop_front()
    }

    pub fn take_core1_release(&mut self) -> bool {
        std::mem::take(&mut self.pending_core1_release)
    }

    /// The 4-bit priority/enable field for interrupt source `src` (as numbered by
    /// start4, i.e. 64.. for the first word). 0 = disabled.
    pub fn irq_priority(&self, src: u32) -> u8 {
        let word = IRQ_PRIO + ((src >> 3) % IRQ_PRIO_COUNT) * IRQ_PRIO_STRIDE;
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
        self.storage.insert(offset, value);
        if off == VBASE {
            if let Some(vbase) = self.vbase.get_mut(core as usize) {
                *vbase = value;
            }
        }
        // A code-address write to the run-state words releases core 1. The
        // interrupt-controller priority words live at the same offsets but only
        // ever hold small bitfields, so a small value is an IRQ-enable, not a
        // release vector.
        if matches!(offset, RUNSTATE_LO | RUNSTATE_HI) && value >= 0x1000 && !self.core1_started {
            self.core1_started = true;
            self.pending_core1_release = true;
        }
        Ok(())
    }
}
