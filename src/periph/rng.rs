//! Hardware RNG at `0x7E10_4000` (`rng@7e104000`, `brcm,bcm2711-rng200`,
//! `reg = <0x7e104000 0x28>` on the Pi 4).
//!
//! Linux drives this block through `iproc-rng200`, but **start4 programs it
//! with the legacy BCM2835 register map**, and that is the contract the model
//! has to honour:
//!
//! | Offset | Register |
//! | --- | --- |
//! | `+0x00` | `RNG_CTRL` — bit 0 enables the generator |
//! | `+0x04` | `RNG_STATUS` — bits `[31:24]` = words waiting in the FIFO |
//! | `+0x08` | `RNG_DATA` — pops one word |
//! | `+0x0C` | `RNG_FF_THRESHOLD` — interrupt when that many words are ready |
//! | `+0x10` | `RNG_INT_MASK` — bit 0 **masks** the interrupt |
//!
//! start4 registers a driver for it whose id word is `0x524E474D`, "RNGM". The
//! driver's init (`0x3ED64B80`) creates a gate already held
//! (`memset(gp+879520, 0, 28); [gp+879532] = 1`), and its read op
//! (`0x3ED64D96`) is:
//!
//! ```text
//! acquire(gp+879524)                ; the driver's own lock
//! r1 = [0x7E104004] >> 24           ; words available
//! if (r1 != 0) skip                 ; data already waiting - no need to block
//! [0x7E10400C] = 1                  ; FF_THRESHOLD: wake me at one word
//! [0x7E104010] = 0                  ; unmask the interrupt
//! acquire(gp+879532)                ; wait for it
//! r6 = [0x7E104008]                 ; RNG_DATA
//! release(gp+879524)
//! ```
//!
//! and the handler for **interrupt source 125** (`0x3ED64BD0`, from the
//! `gp+58004` table — `RVF_DBG_IRQTBL=1`) is the other half:
//!
//! ```text
//! [0x7E104010] = 1                  ; mask it again
//! release(gp+879532)
//! ```
//!
//! With the block unmapped every write vanished into the peripheral stub,
//! `RNG_STATUS` read back 0, and the boot thread waited on that gate forever.
//!
//! A warmed-up generator always has words waiting, so the modelled FIFO reports
//! a steady [`FIFO_WORDS`] and the driver takes its non-blocking path. The
//! interrupt is still modelled — asserted while the generator is enabled, the
//! mask is clear and the FIFO holds at least `RNG_FF_THRESHOLD` words — so the
//! blocking path works too if start4 ever takes it.
//!
//! Output words come from a fixed-seed xorshift: boot transcripts are golden
//! files, so the sequence has to be reproducible.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

pub const BASE: u32 = 0x7E10_4000;
/// `reg = <0x7e104000 0x28>`.
pub const SIZE: u32 = 0x28;

/// The interrupt start4 registers for this block (handler `0x3ED64BD0`).
pub const IRQ_SRC: u32 = 125;

const CTRL: u32 = 0x00;
const STATUS: u32 = 0x04;
const DATA: u32 = 0x08;
const FF_THRESHOLD: u32 = 0x0C;
const INT_MASK: u32 = 0x10;

/// `RNG_CTRL` bit 0. start4 writes `0x7FFF`: enable, plus a warm-up count in
/// the upper bits that the model has no analogue for.
const CTRL_ENABLE: u32 = 1 << 0;
/// `RNG_INT_MASK` bit 0 — set means "do not interrupt me".
const INT_MASKED: u32 = 1 << 0;
/// Words the FIFO reports while the generator runs. Real hardware refills
/// continuously; a warmed-up block is never empty for long.
const FIFO_WORDS: u32 = 16;

pub struct Rng {
    storage: BTreeMap<u32, u32>,
    /// xorshift32 state. Fixed seed: boot transcripts are golden files.
    seed: u32,
    /// Cached interrupt level, recomputed on every register write. The machine
    /// asks for this once per retired instruction, so it must not walk the
    /// register map to answer.
    asserted: bool,
}

impl Default for Rng {
    fn default() -> Rng {
        Rng {
            storage: BTreeMap::new(),
            seed: 0x1AA2_BB31,
            asserted: false,
        }
    }
}

impl Rng {
    pub fn new() -> Rng {
        Rng::default()
    }

    fn reg(&self, off: u32) -> u32 {
        self.storage.get(&off).copied().unwrap_or(0)
    }

    fn enabled(&self) -> bool {
        self.reg(CTRL) & CTRL_ENABLE != 0
    }

    /// Words currently waiting, as `RNG_STATUS[31:24]` reports them.
    fn available(&self) -> u32 {
        if self.enabled() { FIFO_WORDS } else { 0 }
    }

    fn next_word(&mut self) -> u32 {
        // xorshift32.
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed
    }

    /// True while the block is asserting its interrupt line: enabled, unmasked,
    /// and at least `RNG_FF_THRESHOLD` words ready. A threshold of zero is the
    /// reset state and means "never interrupt" — without that, the line would
    /// assert the instant the generator is enabled, long before start4 arms it.
    pub fn irq_asserted(&self) -> bool {
        self.asserted
    }

    fn update_irq(&mut self) {
        let threshold = self.reg(FF_THRESHOLD);
        self.asserted = threshold != 0
            && self.enabled()
            && self.reg(INT_MASK) & INT_MASKED == 0
            && self.available() >= threshold;
    }
}

impl MmioDevice for Rng {
    fn name(&self) -> &'static str {
        "rng"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        match off {
            STATUS => Ok(self.available() << 24),
            DATA => Ok(if self.enabled() { self.next_word() } else { 0 }),
            _ => Ok(self.reg(off)),
        }
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset & !3, value);
        self.update_irq();
        Ok(())
    }
}
