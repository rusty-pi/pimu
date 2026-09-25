//! Hardware RNG (RNG200) at `0x7E10_4000`.
//!
//! Registers and fields: `specs/rng.toml`.
//!
//! A warmed-up generator always has words waiting, so while it runs (enabled,
//! both resets released) the FIFO reports a steady [`FIFO_WORDS`] and the
//! warm-up counter is already past its threshold. No driver ever blocks, but
//! `INT_STATUS` / `INT_ENABLE` are modelled so the blocking path works too.
//!
//! `INT_STATUS` bits are latched events, not levels: start4 1.20210303 acks
//! the FIFO bit without masking it or moving the threshold, and the FIFO is
//! still full afterwards — as a level the source would re-fire at once and
//! nothing else would run. The warm-up counter only grows, so that bit could
//! not be a level either.
//!
//! Output words come from a fixed-seed xorshift and the bit counter advances
//! only with words read: boot transcripts are golden files, and the ARM
//! console must be byte-identical across runs.
use crate::bus::{BusResult, MmioDevice, Width};

// Re-exported: start4 picks its RNG200 driver from `PROBE` bit 18.
pub use crate::spec::rng::PROBE;
use crate::spec::rng::{
    CTRL, CTRL_RBGEN_MASK as CTRL_RBGEN, FIFO_COUNT, FIFO_COUNT_THRESHOLD_MASK,
    FIFO_COUNT_THRESHOLD_SHIFT, FIFO_DATA, INT_ENABLE, INT_STATUS,
    INT_STATUS_FIFO_FULL_MASK as INT_FIFO_FULL, INT_STATUS_TOTAL_BITS_MASK as INT_TOTAL_BITS,
    PROBE_RESET as PROBE_RNG200, RBG_SOFT_RESET, RNG_SOFT_RESET, TOTAL_BIT_COUNT,
    TOTAL_BIT_COUNT_THRESHOLD,
};
use crate::spec::Coverage;

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

pub const IRQ_SRC: u32 = crate::spec::rng::IRQ_VPU;
/// Words the FIFO reports while the generator runs. Real hardware refills
/// continuously, so a warmed-up block is never empty for long.
const FIFO_WORDS: u32 = 16;

pub struct Rng {
    ctrl: u32,
    rng_reset: bool,
    rbg_reset: bool,
    bit_threshold: u32,
    fifo_threshold: u32,
    /// Latched `INT_STATUS` bits: a bit is set when its condition becomes
    /// true, and a clear sticks until it does so again.
    int_status: u32,
    int_enable: u32,
    bits_met: bool,
    fifo_met: bool,
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

    fn running(&self) -> bool {
        self.ctrl & CTRL_RBGEN != 0 && !self.rng_reset && !self.rbg_reset
    }

    fn available(&self) -> u32 {
        if self.running() {
            FIFO_WORDS
        } else {
            0
        }
    }

    fn total_bits(&self) -> u32 {
        if !self.running() {
            return 0;
        }
        self.bit_threshold
            .saturating_add(FIFO_WORDS * 32)
            .saturating_add(self.popped.saturating_mul(32))
    }

    fn next_word(&mut self) -> u32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed
    }

    pub fn irq_asserted(&self) -> bool {
        self.asserted
    }

    fn update(&mut self) {
        if !self.running() {
            self.popped = 0;
        }
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

    /// The `pieeprom.bin` bootloader's init: pulse both resets, then enable.
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

    /// start4's open then read, after the bootloader has already enabled the
    /// block.
    #[test]
    fn start4_reads_without_blocking() {
        let mut rng = Rng::new();
        bootloader_init(&mut rng);
        assert_ne!(rd(&mut rng, CTRL) & 0x1FFF, 0);
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
        assert_ne!(rd(&mut rng, CTRL) & 0x1FFF, 0);
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

    /// start4's blocking path and its interrupt handler. The driver only
    /// blocks on an empty FIFO, so hold the generator in reset while it arms
    /// the interrupt.
    #[test]
    fn fifo_interrupt_follows_start4s_handler() {
        let mut rng = Rng::new();
        bootloader_init(&mut rng);
        wr(&mut rng, INT_ENABLE, 0x8000_0022);
        wr(&mut rng, FIFO_COUNT, 0x1000);
        assert!(!rng.irq_asserted());
        wr(&mut rng, RNG_SOFT_RESET, 1);
        assert_eq!(rd(&mut rng, FIFO_COUNT) & 0xFF, 0);
        wr(&mut rng, FIFO_COUNT, 0x100);
        let s = rd(&mut rng, INT_STATUS);
        wr(&mut rng, INT_STATUS, s | INT_FIFO_FULL);
        wr(&mut rng, INT_ENABLE, 0x8000_0026);
        assert!(!rng.irq_asserted());
        wr(&mut rng, RNG_SOFT_RESET, 0);
        assert!(rng.irq_asserted());
        let s = rd(&mut rng, INT_STATUS);
        assert_eq!(s & 0x8000_0022, 0);
        assert_ne!(s & INT_FIFO_FULL, 0);
        wr(&mut rng, INT_ENABLE, 0x8000_0022);
        wr(&mut rng, FIFO_COUNT, 0x1000);
        let s = rd(&mut rng, INT_STATUS);
        wr(&mut rng, INT_STATUS, s | INT_FIFO_FULL);
        assert!(!rng.irq_asserted());
    }

    /// start4 1.20210303: open arms the FIFO interrupt at two words and
    /// the handler only acks it. The FIFO stays full, and the ack still sticks.
    #[test]
    fn an_acked_fifo_interrupt_stays_acked_while_the_fifo_stays_full() {
        let mut rng = Rng::new();
        bootloader_init(&mut rng);
        wr(&mut rng, TOTAL_BIT_COUNT_THRESHOLD, 0x40000);
        wr(&mut rng, FIFO_COUNT, 0x200);
        wr(&mut rng, INT_ENABLE, 0x8000_0026);
        assert!(rng.irq_asserted());
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
