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
//! Interrupt controller: start4's secure service `0xCEC006A6` stores a 4-bit
//! priority/enable field per source into the words at `0x10..0x30` (core 0) /
//! `0x810..0x830` (core 1): `word = (src >> 3) & 7`, `field = (src & 7) * 4`.
//! A nonzero field enables the source at that priority; the vector is the
//! interrupt number, `64 + source`, not the field.
//! start4 enables source 64 (systimer, [`SYS_IRQ_SRC`]) at priority 1 and arms a
//! system-timer compare as its ThreadX tick.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};
use crate::spec::corectl::{
    INSTANCE_STRIDE as CORE_STRIDE, IRQ_GATE, IRQ_PENDING, IRQ_PENDING_BITS, IRQ_PENDING_BITS_CLR,
    IRQ_PENDING_BITS_COUNT, IRQ_PENDING_BITS_SET, IRQ_PENDING_BITS_SET_RESET as TAG,
    IRQ_PENDING_BITS_STRIDE, IRQ_PENDING_PRIO_MASK, IRQ_PENDING_PRIO_SHIFT,
    IRQ_PENDING_SOURCE_MASK, IRQ_PENDING_SOURCE_SHIFT, IRQ_PRIO, IRQ_PRIO_COUNT, IRQ_PRIO_STRIDE,
    IRQ_PROFILE, VBASE, VBASE_ADDR_MASK, WAKEUP, WAKEUP_ADDR_MASK, WAKEUP_ADDR_SHIFT,
};
use crate::spec::Coverage;

/// Everything else in the bank is plain read-back storage.
pub const COVERAGE: Coverage = Coverage {
    block: "corectl",
    decoded: &[
        IRQ_GATE,
        IRQ_PENDING,
        IRQ_PRIO,
        VBASE,
        WAKEUP,
        IRQ_PENDING_BITS,
        IRQ_PENDING_BITS_SET,
        IRQ_PENDING_BITS_CLR,
        IRQ_PROFILE,
    ],
};

// Register notes beyond what `specs/corectl.toml` records:
//
// * `IRQ_PRIO` — start4 numbers its sources from 64, folded back into these
//   eight words by `(src >> 3) & 7`.
// * `VBASE` — core 1's copy is one `CORE_STRIDE` higher like every other
//   register in this block. `--log irqen` and the peripheral stub both show
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
/// `src - SYS_IRQ_SRC`), from `specs/systimer.toml`. Enabled via
/// `enable_irq_source(64, 1)`.
pub const SYS_IRQ_SRC: u32 = crate::spec::systimer::IRQ_VPU_C0;

#[derive(Default)]
pub struct CoreCtl {
    /// Per core: the source being vectored and the priority it was enabled at,
    /// not yet read by that core's dispatcher through its bank's
    /// [`IRQ_PENDING`]. The priority is latched here because a handler may
    /// rewrite [`IRQ_PRIO`] before it reads the register -- pi4-firmware's stray
    /// handler does exactly that.
    pending_src: [Option<(u32, u32)>; 2],
    storage: BTreeMap<u32, u32>,
    /// Start address last written to core 1's [`WAKEUP`], not yet acted on.
    core1_wake: Option<u32>,
    /// Last exception-vector base the firmware wrote for core 0 / core 1.
    pub vbase: [u32; 2],
    /// A write to core 0's / core 1's [`VBASE`] the core has not picked up yet.
    vbase_written: [Option<u32>; 2],
    /// Sources newly raised in software through [`IRQ_PENDING_BITS`], as
    /// `(core, source)`, waiting to be vectored on that core.
    sw_raised: std::collections::VecDeque<(u32, u32)>,
    /// Where [`Channel::IrqEn`] goes.
    pub log: Log,
}

impl CoreCtl {
    pub fn new() -> CoreCtl {
        CoreCtl::default()
    }

    /// Present `src` (64..127) at `core`'s [`IRQ_PENDING`] for its dispatcher to
    /// pick up. Call it as the source is vectored, never when it is merely
    /// queued: the register holds one value, and the dispatcher reads it only
    /// after its entry sequence.
    pub fn raise_source(&mut self, core: u32, src: u32) {
        let prio = self.irq_priority(core, src) as u32;
        if let Some(slot) = self.pending_src.get_mut(core as usize) {
            *slot = Some((src, prio));
        }
    }

    /// Next `(core, source)` the firmware raised in software by setting a bit in
    /// [`IRQ_PENDING_BITS`]. Real hardware asserts the line as soon as the bit
    /// goes up; the model vectors it on the next step.
    pub fn take_sw_raised(&mut self) -> Option<(u32, u32)> {
        self.sw_raised.pop_front()
    }

    /// The vector base last written for `core` (0 or 1), once: the core reads
    /// the register for every exception, so a later write moves its table.
    pub fn take_vbase(&mut self, core: u32) -> Option<u32> {
        self.vbase_written.get_mut(core as usize)?.take()
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
        let prio = ((self.storage.get(&word).copied().unwrap_or(0) >> field) & 0xF) as u8;
        if prio <= self.irq_gate(core) {
            return 0;
        }
        prio
    }

    /// `core`'s delivery gate: a source enabled at this priority or below waits
    /// until the gate drops. Zero out of reset, and no firmware the model boots
    /// writes it.
    fn irq_gate(&self, core: u32) -> u8 {
        let off = core * CORE_STRIDE + IRQ_GATE;
        (self.storage.get(&off).copied().unwrap_or(0) & 0xF) as u8
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

/// Whether a read of `off` (within one bank) answers the block tag: the hole
/// before [`IRQ_PENDING_BITS`] and everything past the last register.
fn tag_read(off: u32) -> bool {
    off == 0x3C || (IRQ_PENDING_BITS_SET..=0xFF).contains(&off)
}

/// Decode a write-only alias offset into `(which alias, word)`.
fn alias_word(off: u32) -> Option<(u32, u32)> {
    for base in [IRQ_PENDING_BITS_SET, IRQ_PENDING_BITS_CLR] {
        if let Some(word) = element(off, base, IRQ_PENDING_BITS_COUNT, IRQ_PENDING_BITS_STRIDE) {
            return Some((base, word));
        }
    }
    None
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
        let (core, off) = bank(offset);
        if off == IRQ_PENDING {
            // Read-to-clear: the dispatcher reads this once per entry, then the
            // handler acks the device itself. It runs on both cores and reaches
            // its own bank through a per-core pointer, so each bank has its own.
            if let Some((src, prio)) = self
                .pending_src
                .get_mut(core as usize)
                .and_then(Option::take)
            {
                // The interrupt number and the priority it was enabled at, in
                // both half-words: a board answers the same value twice.
                let half = ((prio << IRQ_PENDING_PRIO_SHIFT) & IRQ_PENDING_PRIO_MASK)
                    | ((src << IRQ_PENDING_SOURCE_SHIFT) & IRQ_PENDING_SOURCE_MASK);
                return Ok(half | (half << 16));
            }
        }
        if off == IRQ_PROFILE {
            // Whatever it profiles, a board's VPU reads 0 from it after any
            // write and at handler entry; the value it holds before that is
            // not reproducible and nothing here uses it.
            return Ok(0);
        }
        if off == VBASE || tag_read(off) {
            // `VBASE` does not read back at all, and the words past the last
            // register answer the block's own tag, the way `GPSET` answers
            // `"gpio"`.
            return Ok(if off == VBASE { 0 } else { TAG });
        }
        Ok(self.storage.get(&offset).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        // `--log irqen`: decode writes to the interrupt-priority words back
        // into the `enable_irq_source(src, prio)` calls that produced them, for
        // core 0 (`0x10..0x30`) and core 1 (`0x810..0x830`). Which sources core 1
        // enables is how we find the inter-core doorbell's interrupt number.
        let (core, off) = bank(offset);
        let prio_word = element(off, IRQ_PRIO, IRQ_PRIO_COUNT, IRQ_PRIO_STRIDE);
        if let (true, Some(word)) = (self.log.on(Channel::IrqEn), prio_word) {
            let prev = self.storage.get(&offset).copied().unwrap_or(0);
            for f in 0..8u32 {
                let (a, b) = ((prev >> (f * 4)) & 0xF, (value >> (f * 4)) & 0xF);
                if a != b {
                    let src = word * 8 + f + 64;
                    crate::log!(
                        self.log,
                        Channel::IrqEn,
                        "core{core} src={src} prio {a} -> {b}"
                    );
                }
            }
        }
        // The write-only aliases set or clear bits of the pending word rather
        // than replacing it; fold them in and let the plain path below see the
        // result.
        let (offset, value) = match alias_word(off) {
            Some((base, word)) => {
                let target = core * CORE_STRIDE + IRQ_PENDING_BITS + word * IRQ_PENDING_BITS_STRIDE;
                let prev = self.storage.get(&target).copied().unwrap_or(0);
                let folded = if base == IRQ_PENDING_BITS_SET {
                    prev | value
                } else {
                    prev & !value
                };
                (target, folded)
            }
            None => (offset, value),
        };
        let (core, off) = bank(offset);
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
        let value = match off {
            WAKEUP => value & (WAKEUP_ADDR_MASK << WAKEUP_ADDR_SHIFT),
            // The low nine bits are not stored: a vector table that is not
            // 512-byte aligned is fetched from the address below it, and a
            // firmware that gets this wrong takes no interrupt at all while
            // every register it can read says it should.
            VBASE => value & VBASE_ADDR_MASK,
            _ => value,
        };
        self.storage.insert(offset, value);
        match (core, off) {
            (_, VBASE) => {
                if let Some(vbase) = self.vbase.get_mut(core as usize) {
                    *vbase = value;
                    self.vbase_written[core as usize] = Some(value);
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
    fn every_write_to_vbase_is_picked_up_once() {
        let mut c = CoreCtl::new();
        assert_eq!(c.take_vbase(0), None);
        c.write(VBASE, Width::Word, 0x8000_94B8).unwrap();
        c.write(VBASE, Width::Word, 0).unwrap();
        c.write(VBASE, Width::Word, 0xFEC0_1E00).unwrap();
        assert_eq!(c.take_vbase(0), Some(0xFEC0_1E00));
        assert_eq!(c.take_vbase(0), None);
        assert_eq!(c.take_vbase(1), None);
    }

    #[test]
    fn vbase_keeps_only_the_aligned_address() {
        // `IC0_VADDR_MASK` is 0xFFFFFE00, so a table that is not 512-byte
        // aligned is fetched from the address below it. A firmware that gets
        // this wrong takes no interrupt at all, with every register it can
        // read saying it should: `pi4-firmware` sat at 0xFEC2B7C0 and was dead
        // until it moved to 0xFEC2A000.
        let mut c = CoreCtl::new();
        c.write(VBASE, Width::Word, 0xFEC2_B7C0).unwrap();
        assert_eq!(c.take_vbase(0), Some(0xFEC2_B600));
        c.write(VBASE, Width::Word, 0xFEC2_A000).unwrap();
        assert_eq!(c.take_vbase(0), Some(0xFEC2_A000));
    }

    #[test]
    fn a_vectored_source_reads_back_with_its_priority_in_both_halves() {
        // Raspberry Pi 4B d03115, read from inside a handler: source 71 at
        // priority 1 gives 0x01470147, at priority 7 0x07470747, and source 96
        // at priority 1 gives 0x01600160.
        let mut c = CoreCtl::new();
        c.write(IRQ_PRIO, Width::Word, 1 << 28).unwrap();
        c.raise_source(0, 71);
        assert_eq!(c.read(IRQ_PENDING, Width::Word).unwrap(), 0x0147_0147);
        // Read to clear: the dispatcher sees it once.
        assert_eq!(c.read(IRQ_PENDING, Width::Word).unwrap(), 0);

        c.write(IRQ_PRIO, Width::Word, 7 << 28).unwrap();
        c.raise_source(0, 71);
        assert_eq!(c.read(IRQ_PENDING, Width::Word).unwrap(), 0x0747_0747);

        c.write(IRQ_PRIO + 4 * 4, Width::Word, 1).unwrap();
        c.raise_source(0, 96);
        assert_eq!(c.read(IRQ_PENDING, Width::Word).unwrap(), 0x0160_0160);
    }

    #[test]
    fn the_latched_priority_survives_a_handler_rewriting_irq_prio() {
        // pi4-firmware's stray handler rewrites the priority words before
        // anything reads IRQ_PENDING; hardware latches at delivery.
        let mut c = CoreCtl::new();
        c.write(IRQ_PRIO, Width::Word, 5 << 28).unwrap();
        c.raise_source(0, 71);
        c.write(IRQ_PRIO, Width::Word, 0).unwrap();
        assert_eq!(c.read(IRQ_PENDING, Width::Word).unwrap(), 0x0547_0547);
    }

    #[test]
    fn irq_profile_reads_zero_whatever_is_written() {
        let mut c = CoreCtl::new();
        assert_eq!(c.read(IRQ_PROFILE, Width::Word).unwrap(), 0);
        c.write(IRQ_PROFILE, Width::Word, 0x5A5A).unwrap();
        assert_eq!(c.read(IRQ_PROFILE, Width::Word).unwrap(), 0);
        assert_eq!(c.read(CORE_STRIDE + IRQ_PROFILE, Width::Word).unwrap(), 0);
    }

    #[test]
    fn the_write_only_aliases_set_and_clear_the_pending_word() {
        // Raspberry Pi 4B d03115: `+0x48 <- 0x80` makes `+0x40` read `0x80`,
        // and `+0x50 <- 0x80` puts it back to 0, with the raw source lines
        // untouched either way.
        let mut c = CoreCtl::new();
        c.write(IRQ_PENDING_BITS_SET, Width::Word, 0x80).unwrap();
        assert_eq!(c.read(IRQ_PENDING_BITS, Width::Word).unwrap(), 0x80);
        assert_eq!(c.take_sw_raised(), Some((0, SYS_IRQ_SRC + 7)));
        c.write(IRQ_PENDING_BITS_CLR, Width::Word, 0x80).unwrap();
        assert_eq!(c.read(IRQ_PENDING_BITS, Width::Word).unwrap(), 0);
        assert_eq!(c.take_sw_raised(), None);
    }

    #[test]
    fn an_alias_write_reaches_its_own_cores_bank_and_word() {
        let mut c = CoreCtl::new();
        c.write(
            CORE_STRIDE + IRQ_PENDING_BITS_SET + IRQ_PENDING_BITS_STRIDE,
            Width::Word,
            1,
        )
        .unwrap();
        assert_eq!(
            c.read(
                CORE_STRIDE + IRQ_PENDING_BITS + IRQ_PENDING_BITS_STRIDE,
                Width::Word
            )
            .unwrap(),
            1
        );
        assert_eq!(c.take_sw_raised(), Some((1, SYS_IRQ_SRC + 32)));
    }

    #[test]
    fn the_write_only_offsets_read_back_the_block_tag() {
        // Raspberry Pi 4B d03115: `+0x3C` and every word from `+0x48` to
        // `+0xFF` answer `"INTE"`, in both banks.
        let mut c = CoreCtl::new();
        for off in [0x3C, IRQ_PENDING_BITS_SET, IRQ_PENDING_BITS_CLR, 0x60, 0xFC] {
            assert_eq!(c.read(off, Width::Word).unwrap(), TAG, "{off:#x}");
            assert_eq!(
                c.read(CORE_STRIDE + off, Width::Word).unwrap(),
                TAG,
                "{off:#x}"
            );
        }
    }

    #[test]
    fn the_gate_holds_a_source_until_it_drops() {
        // Raspberry Pi 4B d03115: with `IRQ_GATE` at 0xf a source enabled at
        // priority 1 and forced stayed undelivered, and went in as soon as the
        // gate went back to 0.
        let mut c = CoreCtl::new();
        c.write(IRQ_PRIO, Width::Word, 1).unwrap();
        assert_eq!(c.irq_priority(0, SYS_IRQ_SRC), 1);
        c.write(IRQ_GATE, Width::Word, 0xF).unwrap();
        assert_eq!(c.irq_priority(0, SYS_IRQ_SRC), 0);
        assert_eq!(c.irq_priority(1, SYS_IRQ_SRC), 0);
        c.write(IRQ_GATE, Width::Word, 0).unwrap();
        assert_eq!(c.irq_priority(0, SYS_IRQ_SRC), 1);
    }

    #[test]
    fn vbase_does_not_read_back() {
        // Raspberry Pi 4B d03115: the register reads 0 while the core is
        // demonstrably vectoring through the table last written to it.
        let mut c = CoreCtl::new();
        c.write(VBASE, Width::Word, 0xFEC2_A000).unwrap();
        assert_eq!(c.read(VBASE, Width::Word).unwrap(), 0);
        assert_eq!(c.take_vbase(0), Some(0xFEC2_A000));
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
