//! Hardware RNG at `0x7E10_4000` (`rng@7e104000`, `brcm,bcm2711-rng200`,
//! `reg = <0x7e104000 0x28>` on the Pi 4).
//!
//! The block is an RNG200, and the model implements its register map. Every
//! client on this silicon speaks it:
//!
//! | Offset | Register | Who touches it |
//! | --- | --- | --- |
//! | `+0x00` | `RNG_CTRL` — `[12:0]` RBGEN, `[14:13]` sample-rate divider | all three |
//! | `+0x04` | `RNG_SOFT_RESET` — bit 0 holds the generator in reset | bootloader, start4 |
//! | `+0x08` | `RBG_SOFT_RESET` — bit 0 holds the bit generator in reset | bootloader, start4 |
//! | `+0x0C` | `RNG_TOTAL_BIT_COUNT` — bits generated since reset (RO) | Linux |
//! | `+0x10` | `RNG_TOTAL_BIT_COUNT_THRESHOLD` — warm-up bits to discard | start4 |
//! | `+0x14` | bit 18 set on this block (RO, see [`PROBE`]) | start4 |
//! | `+0x18` | `RNG_INT_STATUS` — write-one-to-clear | start4 |
//! | `+0x1C` | `RNG_INT_ENABLE` | start4 |
//! | `+0x20` | `RNG_FIFO_DATA` — pops one word | start4, Linux |
//! | `+0x24` | `RNG_FIFO_COUNT` — `[7:0]` words ready, `[15:8]` threshold | start4, Linux |
//!
//! **Linux** (`raspberrypi/linux` `drivers/char/hw_random/iproc-rng200.c`,
//! rpi-6.12.y at `aa731bab`): `bcm2711_rng200_init` leaves the block alone if
//! `RNG_CTRL & 0x1FFF` is already set, else writes threshold `0x40000`,
//! `FIFO_COUNT = 2 << 8` and `RNG_CTRL = (3 << 13) | 0x1FFF` = `0x7FFF`.
//! `bcm2711_rng200_read` spins until `TOTAL_BIT_COUNT > 16`, then until
//! `FIFO_COUNT & 0xFF` is non-zero, then reads that many words from
//! `FIFO_DATA`. With `0x0C` reading 0 it spun forever in `hwrng_fillfn`.
//!
//! **Bootloader** (`pieeprom.bin`, `0x8000378E`): pulses `RBG_SOFT_RESET` and
//! `RNG_SOFT_RESET` (1 then 0) and writes `RNG_CTRL = 0x7FFF`. It never reads
//! the FIFO.
//!
//! **start4** carries two RNG drivers, and all of its RNG accesses are in
//! `0x3ED64A4E..0x3ED64E28`. The probe at `0x3ED64B0A` reads `+0x14` and
//! returns the driver table at `0x3EDFC0BC` (rng200 map) if bit 18 is set, or
//! the one at `0x3EDFC0D4` (the legacy BCM2835 map: `STATUS` at `+0x04`,
//! `DATA` at `+0x08`, driver id "RNGM") if it is clear. start4db's decompile
//! makes the same choice (`FUN_0ee13458`). The rng200 driver:
//!
//! ```text
//! open  0x3ED64CDC  if (!(CTRL & 0x1FFF)) {       ; already running: skip
//!                     restart();                 ; 0x3ED64E16: pulse both resets
//!                     [+0x10] = 0x40000; [+0x24] = 0x1000;
//!                     [+0x1C] = 0x80000022; [+0x00] = 0x7FFF; }
//! read  0x3ED64DD0  if (!([+0x24] & 0xFF)) {      ; FIFO empty: block
//!                     [+0x24] = 0x100;           ; interrupt at one word
//!                     [+0x18] |= 4; [+0x1C] = 0x80000026;
//!                     wait(gp+879560); }
//!                   return [+0x20];
//! irq   0x3ED64BE8  s = [+0x18]                   ; interrupt source 125
//!                   if (s & 0x80000022) { restart(); [+0x18] |= 0x80000022; }
//!                   else if (s & 4) { [+0x1C] = 0x80000022; [+0x24] = 0x1000;
//!                                     [+0x18] |= 4; release(gp+879560); }
//! ```
//!
//! So `INT_STATUS` bit 2 is "FIFO holds `FIFO_COUNT[15:8]` words", and
//! `0x80000022` covers the failure bits (Linux names bit 31
//! `MASTER_FAIL_LOCKOUT` and bit 5 `NIST_FAIL`), which the model never raises.
//!
//! Older start4 builds (1.20210303, #73) drive the same block differently:
//! open leaves `INT_ENABLE = 0x80000026`, so the FIFO interrupt is armed with
//! a threshold of two words, and the handler (`0x3ED565C6` in that build)
//! only acks bit 2 — `[+0x18] |= 4` — without masking it or moving the
//! threshold. The FIFO is still full after the ack, so if bit 2 tracked that
//! as a level the source would fire again at once and nothing else would run.
//! That build booted on hardware, so both status bits are events: each
//! latches when its condition becomes true, and a write-one clear sticks until
//! the condition has gone false and true again. `TOTAL_BITS` could not be a
//! level anyway — the counter only grows.
//!
//! The model used to answer `+0x14` with 0, which steered start4 onto the
//! legacy driver, and then implemented that map instead. The legacy
//! driver's `STATUS`/`DATA` offsets are the rng200's soft-reset registers.
//!
//! A warmed-up generator always has words waiting, so while it runs (enabled,
//! both resets released) the FIFO reports a steady [`FIFO_WORDS`] and the
//! warm-up counter is already past its threshold. Neither driver ever blocks,
//! but `INT_STATUS`/`INT_ENABLE` are modelled so the blocking path works too.
//! Output words come from a fixed-seed xorshift and the bit counter advances
//! only with words read: boot transcripts are golden files, and the ARM
//! console must be byte-identical across runs.

use crate::bus::{BusResult, MmioDevice, Width};

// `PROBE` is not in the kernel's register list: start4's probe (`0x3ED64B0A`)
// picks its rng200 driver iff bit 18 reads set, so on this block it does.
pub use crate::spec::rng::PROBE;
use crate::spec::rng::{
    CTRL, CTRL_RBGEN_MASK as CTRL_RBGEN, FIFO_COUNT, FIFO_COUNT_THRESHOLD_MASK,
    FIFO_COUNT_THRESHOLD_SHIFT, FIFO_DATA, INT_ENABLE, INT_STATUS,
    INT_STATUS_FIFO_FULL_MASK as INT_FIFO_FULL, INT_STATUS_TOTAL_BITS_MASK as INT_TOTAL_BITS,
    PROBE_RESET as PROBE_RNG200, RBG_SOFT_RESET, RNG_SOFT_RESET, TOTAL_BIT_COUNT,
    TOTAL_BIT_COUNT_THRESHOLD,
};
use crate::spec::Coverage;

/// Every register in `specs/rng.toml` is modelled.
pub const COVERAGE: Coverage = Coverage {
    block: "rng",
    decoded: &[
        CTRL,
        RNG_SOFT_RESET,
        RBG_SOFT_RESET,
        TOTAL_BIT_COUNT,
        TOTAL_BIT_COUNT_THRESHOLD,
        PROBE,
        INT_STATUS,
        INT_ENABLE,
        FIFO_DATA,
        FIFO_COUNT,
    ],
};

/// The interrupt start4 registers for this block (handler `0x3ED64BE8`).
pub const IRQ_SRC: u32 = 125;
/// Words the FIFO reports while the generator runs. Real hardware refills
/// continuously; a warmed-up block is never empty for long.
const FIFO_WORDS: u32 = 16;

pub struct Rng {
    ctrl: u32,
    rng_reset: bool,
    rbg_reset: bool,
    bit_threshold: u32,
    /// `FIFO_COUNT[15:8]`, the FIFO-full interrupt level. 0 (reset) = never.
    fifo_threshold: u32,
    /// Latched `INT_STATUS` bits. [`Rng::update`] sets a bit when its
    /// condition becomes true; a clear sticks until it does so again.
    int_status: u32,
    int_enable: u32,
    /// The warm-up and FIFO-full conditions as [`Rng::update`] last saw them,
    /// to latch each on its rising edge only.
    bits_met: bool,
    fifo_met: bool,
    /// Words popped since the generator last left reset; drives the bit count.
    popped: u32,
    /// xorshift32 state. Fixed seed: boot transcripts are golden files.
    seed: u32,
    /// Cached interrupt level. The machine asks for this once per retired
    /// instruction, so it must be a field read.
    asserted: bool,
}

impl Default for Rng {
    fn default() -> Rng {
        Rng {
            ctrl: 0,
            rng_reset: false,
            rbg_reset: false,
            bit_threshold: 0,
            fifo_threshold: 0,
            int_status: 0,
            int_enable: 0,
            bits_met: false,
            fifo_met: false,
            popped: 0,
            seed: 0x1AA2_BB31,
            asserted: false,
        }
    }
}

impl Rng {
    pub fn new() -> Rng {
        Rng::default()
    }

    /// Enabled and out of both soft resets.
    fn running(&self) -> bool {
        self.ctrl & CTRL_RBGEN != 0 && !self.rng_reset && !self.rbg_reset
    }

    /// Words waiting, as `FIFO_COUNT[7:0]` reports them.
    fn available(&self) -> u32 {
        if self.running() {
            FIFO_WORDS
        } else {
            0
        }
    }

    /// `RNG_TOTAL_BIT_COUNT`: the warm-up bits it discarded, the bits sitting
    /// in the FIFO, and the bits already read out.
    fn total_bits(&self) -> u32 {
        if !self.running() {
            return 0;
        }
        self.bit_threshold
            .saturating_add(FIFO_WORDS * 32)
            .saturating_add(self.popped.saturating_mul(32))
    }

    fn next_word(&mut self) -> u32 {
        // xorshift32.
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed
    }

    /// True while the block is asserting its interrupt line: an enabled
    /// `INT_STATUS` bit is set.
    pub fn irq_asserted(&self) -> bool {
        self.asserted
    }

    fn update(&mut self) {
        if !self.running() {
            self.popped = 0;
        }
        // Events, not levels (see the module notes): latch on the way up only.
        let bits_met = self.bit_threshold != 0 && self.total_bits() >= self.bit_threshold;
        if bits_met && !self.bits_met {
            self.int_status |= INT_TOTAL_BITS;
        }
        self.bits_met = bits_met;
        let fifo_met = self.fifo_threshold != 0 && self.available() >= self.fifo_threshold;
        if fifo_met && !self.fifo_met {
            self.int_status |= INT_FIFO_FULL;
        }
        self.fifo_met = fifo_met;
        self.asserted = self.int_status & self.int_enable != 0;
    }
}

impl MmioDevice for Rng {
    fn name(&self) -> &'static str {
        "rng"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset & !3 {
            CTRL => self.ctrl,
            RNG_SOFT_RESET => self.rng_reset as u32,
            RBG_SOFT_RESET => self.rbg_reset as u32,
            TOTAL_BIT_COUNT => self.total_bits(),
            TOTAL_BIT_COUNT_THRESHOLD => self.bit_threshold,
            PROBE => PROBE_RNG200,
            INT_STATUS => self.int_status,
            INT_ENABLE => self.int_enable,
            FIFO_DATA => {
                if self.running() {
                    self.popped = self.popped.saturating_add(1);
                    let word = self.next_word();
                    self.update();
                    word
                } else {
                    0
                }
            }
            FIFO_COUNT => (self.fifo_threshold << FIFO_COUNT_THRESHOLD_SHIFT) | self.available(),
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        match offset & !3 {
            CTRL => self.ctrl = value,
            RNG_SOFT_RESET => self.rng_reset = value & 1 != 0,
            RBG_SOFT_RESET => self.rbg_reset = value & 1 != 0,
            TOTAL_BIT_COUNT_THRESHOLD => self.bit_threshold = value,
            INT_STATUS => self.int_status &= !value,
            INT_ENABLE => self.int_enable = value,
            FIFO_COUNT => {
                self.fifo_threshold =
                    (value & FIFO_COUNT_THRESHOLD_MASK) >> FIFO_COUNT_THRESHOLD_SHIFT
            }
            _ => {}
        }
        self.update();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rd(rng: &mut Rng, off: u32) -> u32 {
        rng.read(off, Width::Word).unwrap()
    }

    fn wr(rng: &mut Rng, off: u32, value: u32) {
        rng.write(off, Width::Word, value).unwrap();
    }

    /// The bootloader's `0x8000378E`: pulse both resets, then enable.
    fn bootloader_init(rng: &mut Rng) {
        wr(rng, RBG_SOFT_RESET, 1);
        wr(rng, RNG_SOFT_RESET, 1);
        wr(rng, RNG_SOFT_RESET, 0);
        wr(rng, RBG_SOFT_RESET, 0);
        wr(rng, CTRL, 0x7FFF);
    }

    #[test]
    fn start4_probe_picks_the_rng200_driver() {
        let mut rng = Rng::new();
        // `0x3ED64B14`: `btest r2, 18` on `[0x7E104014]`.
        assert_ne!(rd(&mut rng, PROBE) & (1 << 18), 0);
    }

    #[test]
    fn idle_block_is_empty() {
        let mut rng = Rng::new();
        assert_eq!(rd(&mut rng, TOTAL_BIT_COUNT), 0);
        assert_eq!(rd(&mut rng, FIFO_COUNT) & 0xFF, 0);
        assert_eq!(rd(&mut rng, FIFO_DATA), 0);
        assert!(!rng.irq_asserted());
    }

    #[test]
    fn soft_reset_holds_the_generator() {
        let mut rng = Rng::new();
        bootloader_init(&mut rng);
        wr(&mut rng, RNG_SOFT_RESET, 1);
        assert_eq!(rd(&mut rng, RNG_SOFT_RESET), 1);
        assert_eq!(rd(&mut rng, FIFO_COUNT) & 0xFF, 0);
        assert_eq!(rd(&mut rng, TOTAL_BIT_COUNT), 0);
        wr(&mut rng, RNG_SOFT_RESET, 0);
        assert_eq!(rd(&mut rng, FIFO_COUNT) & 0xFF, FIFO_WORDS);
    }

    /// start4's open (`0x3ED64CDC`) then read (`0x3ED64DD0`), after the
    /// bootloader has already enabled the block.
    #[test]
    fn start4_reads_without_blocking() {
        let mut rng = Rng::new();
        bootloader_init(&mut rng);
        // Open: already running, so it leaves the block alone.
        assert_ne!(rd(&mut rng, CTRL) & 0x1FFF, 0);
        // Read: words ready, so straight to FIFO_DATA.
        assert_ne!(rd(&mut rng, FIFO_COUNT) & 0xFF, 0);
        let a = rd(&mut rng, FIFO_DATA);
        let b = rd(&mut rng, FIFO_DATA);
        assert_ne!(a, b);
        assert!(!rng.irq_asserted());
    }

    /// `bcm2711_rng200_init` + `bcm2711_rng200_read` on a block the firmware
    /// left running.
    #[test]
    fn linux_rng200_read_gets_words() {
        let mut rng = Rng::new();
        bootloader_init(&mut rng);
        wr(&mut rng, TOTAL_BIT_COUNT_THRESHOLD, 0x40000);
        // init: RBGEN already set, returns without touching anything.
        assert_ne!(rd(&mut rng, CTRL) & 0x1FFF, 0);
        // read: warm-up elapsed, FIFO not empty.
        assert!(rd(&mut rng, TOTAL_BIT_COUNT) > 16);
        let n = rd(&mut rng, FIFO_COUNT) & 0xFF;
        assert_eq!(n, FIFO_WORDS);
        let words: Vec<u32> = (0..n).map(|_| rd(&mut rng, FIFO_DATA)).collect();
        assert!(words.windows(2).all(|w| w[0] != w[1]));
        // The counter moves with the words read, and stays past warm-up.
        assert_eq!(
            rd(&mut rng, TOTAL_BIT_COUNT),
            0x40000 + FIFO_WORDS * 32 + n * 32
        );

        // Same inputs, same words: runs must be reproducible.
        let mut again = Rng::new();
        bootloader_init(&mut again);
        let words2: Vec<u32> = (0..n).map(|_| rd(&mut again, FIFO_DATA)).collect();
        assert_eq!(words, words2);
    }

    /// Linux's own init on a cold block.
    #[test]
    fn linux_init_from_cold() {
        let mut rng = Rng::new();
        wr(&mut rng, TOTAL_BIT_COUNT_THRESHOLD, 0x40000);
        wr(&mut rng, FIFO_COUNT, 2 << 8);
        wr(&mut rng, CTRL, (3 << 13) | 0x1FFF);
        assert!(rd(&mut rng, TOTAL_BIT_COUNT) > 16);
        assert_eq!(rd(&mut rng, FIFO_COUNT), (2 << 8) | FIFO_WORDS);
    }

    /// start4's blocking path and its interrupt handler (`0x3ED64BE8`). The
    /// driver only blocks on an empty FIFO, so hold the generator in reset
    /// while it arms the interrupt.
    #[test]
    fn fifo_interrupt_follows_start4s_handler() {
        let mut rng = Rng::new();
        bootloader_init(&mut rng);
        wr(&mut rng, INT_ENABLE, 0x8000_0022);
        wr(&mut rng, FIFO_COUNT, 0x1000);
        assert!(!rng.irq_asserted());
        // Read, blocking path: interrupt at one word.
        wr(&mut rng, RNG_SOFT_RESET, 1);
        assert_eq!(rd(&mut rng, FIFO_COUNT) & 0xFF, 0);
        wr(&mut rng, FIFO_COUNT, 0x100);
        let s = rd(&mut rng, INT_STATUS);
        wr(&mut rng, INT_STATUS, s | INT_FIFO_FULL);
        wr(&mut rng, INT_ENABLE, 0x8000_0026);
        assert!(!rng.irq_asserted());
        // A word arrives.
        wr(&mut rng, RNG_SOFT_RESET, 0);
        assert!(rng.irq_asserted());
        // Handler: no failure bits, FIFO full.
        let s = rd(&mut rng, INT_STATUS);
        assert_eq!(s & 0x8000_0022, 0);
        assert_ne!(s & INT_FIFO_FULL, 0);
        wr(&mut rng, INT_ENABLE, 0x8000_0022);
        wr(&mut rng, FIFO_COUNT, 0x1000);
        let s = rd(&mut rng, INT_STATUS);
        wr(&mut rng, INT_STATUS, s | INT_FIFO_FULL);
        assert!(!rng.irq_asserted());
    }

    /// start4 1.20210303 (#73): open arms the FIFO interrupt at two words and
    /// the handler only acks it. The FIFO stays full, and the ack still sticks.
    #[test]
    fn an_acked_fifo_interrupt_stays_acked_while_the_fifo_stays_full() {
        let mut rng = Rng::new();
        bootloader_init(&mut rng);
        wr(&mut rng, TOTAL_BIT_COUNT_THRESHOLD, 0x40000);
        wr(&mut rng, FIFO_COUNT, 0x200);
        wr(&mut rng, INT_ENABLE, 0x8000_0026);
        assert!(rng.irq_asserted());
        // Handler `0x0ED565C6`: no failure bits, so `[+0x18] |= 4`.
        let s = rd(&mut rng, INT_STATUS);
        assert_eq!(s & 0x8000_0022, 0);
        assert_ne!(s & INT_FIFO_FULL, 0);
        wr(&mut rng, INT_STATUS, s | INT_FIFO_FULL);
        assert!(!rng.irq_asserted());
        assert_eq!(rd(&mut rng, INT_STATUS) & INT_FIFO_FULL, 0);
        // The reader drains nothing the model can see, and nothing re-fires.
        rd(&mut rng, FIFO_DATA);
        assert!(!rng.irq_asserted());
        // The warm-up bit is an event too: acked once, it stays acked.
        wr(&mut rng, INT_STATUS, INT_TOTAL_BITS);
        rd(&mut rng, FIFO_DATA);
        assert_eq!(rd(&mut rng, INT_STATUS) & INT_TOTAL_BITS, 0);
    }
}
