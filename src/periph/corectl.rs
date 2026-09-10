//! VPU core-control block at `0x7E00_2000`. This region carries both the
//! per-core boot handshake and the VPU interrupt controller.
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

/// Run-state field words. A write of a code address here releases core 1 (small
/// values are the interrupt-controller priority words, not a release vector).
const RUNSTATE_LO: u32 = 0x10;
const RUNSTATE_HI: u32 = 0x14;
/// Core 0 / core 1 exception-vector-base registers.
const VBASE_CORE0: u32 = 0x30;
const VBASE_CORE1: u32 = 0x38;

/// Interrupt-priority words for sources 0..31 (core 0). start4 numbers its
/// sources from 64, folded back into these four words by `(src >> 3) & 3`.
const IRQ_PRIO_BASE: u32 = 0x10;

/// Per-core interrupt **pending** bitmask, one bit per source: `+0x40` holds
/// sources 64..95, `+0x44` sources 96..127 (core 1's pair lives at `+0x840` /
/// `+0x844`, i.e. `CORE_STRIDE` higher). start4 drives them with three helpers,
/// all of which pick the base from a core-index argument:
///
/// * `0x3ED01896(src, core)` — `[base] |= 1 << bit`, i.e. *raise* the source in
///   software. This is how the firmware posts an interrupt to a core, including
///   the inter-core reschedule IPI (source 78 for core 0, 79 for core 1, both
///   of which have a direct vector entry at `0x3EC3F8F4`).
/// * `0x3ED01792(src, core)` — `[base] &= ~(1 << bit)`, the acknowledge every
///   ISR performs on entry.
/// * `0x3ED01980(src, core)` — read one bit back.
const IRQ_PENDING_BITS: u32 = 0x40;
/// Distance between core 0's register block and core 1's (`[blk+12]` is set to
/// `0x7E002000 + core * 0x800` by the per-core init at `0x3EC3E938`).
const CORE_STRIDE: u32 = 0x800;

/// The interrupt source start4 wires to the BCM system timer (compare channel
/// `src - SYS_IRQ_SRC`). Enabled via `enable_irq_source(64, 1)`.
pub const SYS_IRQ_SRC: u32 = 64;

/// Offset the generic per-source ISR dispatcher (`0x3EC3E9BC`) reads to learn
/// which interrupt is pending: it does `r2 = [r29+12]` (= `0x7E00_2000`),
/// `r0 = [r2+4]`, `btest r0, 8` (bit 8 = "something pending"), then
/// `or r0, 64` / 7-bit mask to get the source number in 64..127, and finally
/// indexes the handler table at `gp+58004` with it.
const IRQ_PENDING: u32 = 0x04;

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
}

impl CoreCtl {
    pub fn new() -> CoreCtl {
        CoreCtl::default()
    }

    /// Present `src` (64..127) at [`IRQ_PENDING`] for the dispatcher to pick up.
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
        let word = IRQ_PRIO_BASE + ((src >> 3) & 3) * 4;
        let field = (src & 7) * 4;
        ((self.storage.get(&word).copied().unwrap_or(0) >> field) & 0xF) as u8
    }
}

/// Decode a pending-bitmask offset into `(core, word)`; `word` 0 covers sources
/// 64..95 and word 1 sources 96..127.
fn pending_word(offset: u32) -> Option<(u32, u32)> {
    let (core, off) = if offset >= CORE_STRIDE {
        (1, offset - CORE_STRIDE)
    } else {
        (0, offset)
    };
    match off {
        IRQ_PENDING_BITS => Some((core, 0)),
        _ if off == IRQ_PENDING_BITS + 4 => Some((core, 1)),
        _ => None,
    }
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
                return Ok(0x100 | (src & 0x3F));
            }
        }
        Ok(self.storage.get(&offset).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        // `RVF_DBG_IRQEN=1`: decode writes to the interrupt-priority words back
        // into the `enable_irq_source(src, prio)` calls that produced them, for
        // core 0 (`0x10..0x20`) and core 1 (`0x810..0x820`). Which sources core 1
        // enables is how we find the inter-core doorbell's interrupt number.
        if std::env::var_os("RVF_DBG_IRQEN").is_some()
            && matches!(offset, 0x10..=0x1F | 0x810..=0x81F)
        {
            let core = u32::from(offset >= 0x800);
            let word = (offset - if core == 1 { 0x810 } else { 0x10 }) / 4;
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
                    self.sw_raised.push_back((core, SYS_IRQ_SRC + word * 32 + bit));
                }
            }
        }
        self.storage.insert(offset, value);
        match offset {
            VBASE_CORE0 => self.vbase[0] = value,
            VBASE_CORE1 => self.vbase[1] = value,
            _ => {}
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
