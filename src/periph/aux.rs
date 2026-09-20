//! AUX peripheral: mini-UART (`UART1`) + SPI master register stubs.
//!
//! The mini-UART at offset `0x40` is Linux's `ttyS0`, and on a card whose
//! `config.txt` leaves Bluetooth enabled it is *the* console: the base device
//! tree keeps `serial0 = &uart1`, the firmware hands GPIO 14/15 over to ALT5
//! at the ARM handover, and every kernel line comes out here instead of the
//! PL011 (#124).
//!
//! Transmit is instant, as in the PL011 model: bytes written to `MU_IO` are
//! appended to an output buffer the harness drains, and the line status never
//! reports the transmit FIFO full or busy.
//!
//! Receive mirrors [`super::uart_pl011::Pl011`]: host input goes onto the
//! *line* ([`Aux::feed`]) and [`Aux::pump`] moves it into the eight-entry
//! receive FIFO at the rate `MU_BAUD` sets, against the modelled clock, so a
//! scripted input sequence lands at the same guest instant on every run. Bytes
//! wait on the line while the FIFO is full or the receiver is off rather than
//! being dropped as an overrun.
//!
//! `MU_LCR` bit 7 is the 16550's DLAB: while it is set, `MU_IO` and `MU_IER`
//! are the low and high bytes of the baud-rate counter instead of the data
//! and interrupt-enable registers, which is how Linux's `8250` driver
//! programs the rate. Without that, its divisor write would push a byte into
//! the console and leave a stray interrupt enabled.
//!
//! The interrupt is the 16550's, not the datasheet's: `MU_IER` bit 0 enables
//! the receive interrupt and bit 1 the transmit one, which is the order
//! Linux's `8250` driver — the one behind `brcm,bcm2835-aux-uart` — writes
//! them in, and `MU_IIR` reports the highest-priority of the two with the
//! inverted pending bit.

use std::collections::VecDeque;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};

use crate::spec::aux::{
    ENABLES as AUX_ENABLES, ENABLES_UART_MASK as ENABLES_UART, IRQ as AUX_IRQ, MU_BAUD, MU_CNTL,
    MU_CNTL_RX_ENABLE_MASK as CNTL_RX_ENABLE, MU_IER, MU_IER_RX_INT_MASK as IER_RX,
    MU_IER_TX_INT_MASK as IER_TX, MU_IIR, MU_IIR_FIFO_ENABLES_MASK as IIR_FIFO_ENABLES,
    MU_IIR_ID_SHIFT, MU_IIR_PENDING_MASK as IIR_PENDING, MU_IO, MU_LCR,
    MU_LCR_DLAB_MASK as LCR_DLAB, MU_LSR, MU_LSR_DATA_READY_MASK as LSR_DATA_READY,
    MU_LSR_TX_EMPTY_MASK as LSR_TX_EMPTY, MU_LSR_TX_IDLE_MASK as LSR_TX_IDLE, MU_MCR, MU_MSR,
    MU_SCRATCH, MU_STAT, MU_STAT_RX_FIFO_LEVEL_SHIFT, MU_STAT_RX_IDLE_MASK as STAT_RX_IDLE,
    MU_STAT_SPACE_AVAILABLE_MASK as STAT_SPACE_AVAILABLE,
    MU_STAT_SYMBOL_AVAILABLE_MASK as STAT_SYMBOL_AVAILABLE, MU_STAT_TX_DONE_MASK as STAT_TX_DONE,
    MU_STAT_TX_FIFO_EMPTY_MASK as STAT_TX_FIFO_EMPTY, MU_STAT_TX_IDLE_MASK as STAT_TX_IDLE,
};
use crate::spec::Coverage;

/// Every register in `specs/aux.toml` is modelled; the SPI masters are not in
/// it.
pub const COVERAGE: Coverage = Coverage {
    block: "aux",
    decoded: &[
        AUX_IRQ,
        AUX_ENABLES,
        MU_IO,
        MU_IER,
        MU_IIR,
        MU_LCR,
        MU_MCR,
        MU_LSR,
        MU_MSR,
        MU_SCRATCH,
        MU_CNTL,
        MU_STAT,
        MU_BAUD,
    ],
};

/// The mini-UART's receive FIFO is eight bytes deep (the transmit one too).
const FIFO_DEPTH: usize = 8;

/// The clock the baud-rate counter divides: the VPU core clock, 500 MHz. Linux
/// reports the eighth of it the counter starts from —
/// `ttyS0 at MMIO 0xfe215040 (irq = 37, base_baud = 62500000) is a 16550`.
const SYSTEM_CLOCK_HZ: u64 = 500_000_000;

/// `MU_IIR.ID` for "a byte is waiting in the receive FIFO".
const IIR_ID_RX: u32 = 0b10;
/// `MU_IIR.ID` for "room in the transmit holding register".
const IIR_ID_TX: u32 = 0b01;

#[derive(Default)]
pub struct Aux {
    pub out: Vec<u8>,
    /// Where [`Channel::Uart`] goes.
    pub log: Log,
    enables: u32,
    ier: u32,
    lcr: u32,
    mcr: u32,
    scratch: u32,
    cntl: u32,
    baud: u32,
    /// Receive FIFO.
    rx: VecDeque<u8>,
    /// Host input not yet received: the serial line.
    line: VecDeque<u8>,
    /// Modelled time (µs) the next character on the line finishes arriving.
    next_rx_us: u64,
    /// Nothing was on the line at the last [`Aux::pump`].
    line_idle: bool,
}

impl Aux {
    pub fn new() -> Aux {
        Aux {
            // The mini-UART comes out of reset with both halves enabled; the
            // block as a whole does not (`AUX_ENABLES` is 0).
            cntl: CNTL_RX_ENABLE | crate::spec::aux::MU_CNTL_TX_ENABLE_MASK,
            line_idle: true,
            ..Aux::default()
        }
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// Put host bytes on the receive line, behind anything still in flight.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.line.extend(bytes);
    }

    /// Bytes fed but not yet read by the guest.
    pub fn rx_backlog(&self) -> usize {
        self.line.len() + self.rx.len()
    }

    /// The interrupt output, which `AUX_IRQ` bit 0 mirrors.
    pub fn irq_line(&self) -> bool {
        self.iir_id().is_some()
    }

    /// Advance the receiver to modelled time `now_us`: characters whose last
    /// stop bit has gone by enter the FIFO.
    pub fn pump(&mut self, now_us: u64) {
        if self.line.is_empty() {
            self.line_idle = true;
            return;
        }
        let char_us = self.char_us();
        let receiving = self.enables & ENABLES_UART != 0 && self.cntl & CNTL_RX_ENABLE != 0;
        // Input fed onto an idle line starts its first character now.
        if self.line_idle {
            self.line_idle = false;
            self.next_rx_us = now_us;
        }
        while !self.line.is_empty() {
            if !receiving || self.rx.len() >= FIFO_DEPTH {
                // Held back: the sender waits, and resumes a character time
                // after there is room again.
                self.next_rx_us = now_us;
                break;
            }
            if self.next_rx_us + char_us > now_us {
                break;
            }
            let b = self.line.pop_front().expect("non-empty line");
            self.next_rx_us += char_us;
            self.rx.push_back(b);
        }
        if self.line.is_empty() {
            self.line_idle = true;
        }
    }

    /// One 8N1 character (ten bit periods) at the programmed rate, in µs. The
    /// line runs at `SYSTEM_CLOCK_HZ / (8 * (MU_BAUD + 1))`; before anything
    /// has programmed the counter, 115200 baud.
    fn char_us(&self) -> u64 {
        if self.baud == 0 {
            return 87;
        }
        (10 * 8 * (u64::from(self.baud) + 1) * 1_000_000 / SYSTEM_CLOCK_HZ).max(1)
    }

    /// `MU_LCR.DLAB`: `MU_IO` and `MU_IER` are the baud-rate counter's two
    /// bytes while it is set.
    fn dlab(&self) -> bool {
        self.lcr & LCR_DLAB != 0
    }

    /// Which interrupt `MU_IIR` reports, receive first, or `None` when the
    /// line is idle.
    fn iir_id(&self) -> Option<u32> {
        if self.enables & ENABLES_UART == 0 {
            return None;
        }
        if self.ier & IER_RX != 0 && !self.rx.is_empty() {
            // A byte is waiting.
            Some(IIR_ID_RX)
        } else if self.ier & IER_TX != 0 {
            // Transmit never stalls, so the holding register is always empty.
            Some(IIR_ID_TX)
        } else {
            None
        }
    }
}

impl MmioDevice for Aux {
    fn name(&self) -> &'static str {
        "aux(mini-uart)"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset {
            AUX_IRQ => u32::from(self.irq_line()),
            AUX_ENABLES => self.enables,
            MU_IO if self.dlab() => self.baud & 0xFF,
            MU_IO => self.rx.pop_front().map_or(0, u32::from),
            MU_IER if self.dlab() => self.baud >> 8,
            MU_IER => self.ier,
            MU_IIR => {
                let id = self.iir_id();
                IIR_FIFO_ENABLES
                    | match id {
                        Some(id) => id << MU_IIR_ID_SHIFT,
                        None => IIR_PENDING,
                    }
            }
            MU_LCR => self.lcr,
            MU_MCR => self.mcr,
            MU_LSR => {
                // Always ready to send.
                let mut lsr = LSR_TX_EMPTY | LSR_TX_IDLE;
                if !self.rx.is_empty() {
                    lsr |= LSR_DATA_READY;
                }
                lsr
            }
            MU_MSR => 0,
            MU_SCRATCH => self.scratch,
            MU_CNTL => self.cntl,
            MU_STAT => {
                let mut stat = STAT_SPACE_AVAILABLE
                    | STAT_TX_IDLE
                    | STAT_TX_FIFO_EMPTY
                    | STAT_TX_DONE
                    | (self.rx.len() as u32) << MU_STAT_RX_FIFO_LEVEL_SHIFT;
                if self.rx.is_empty() {
                    // Nothing waiting, and nothing part-way in either.
                    stat |= STAT_RX_IDLE;
                } else {
                    stat |= STAT_SYMBOL_AVAILABLE;
                }
                stat
            }
            MU_BAUD => self.baud,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        // The data register is the console, and it is printed; the rest is
        // set-up worth seeing when the console goes quiet.
        if offset != MU_IO {
            crate::log!(self.log, Channel::Uart, "aux +{offset:#04x} <- {value:#x}");
        }
        let was_irq = self.irq_line();
        match offset {
            AUX_ENABLES => self.enables = value & 0x7,
            MU_IO if self.dlab() => self.baud = (self.baud & 0xFF00) | (value & 0xFF),
            MU_IO => self.out.push(value as u8),
            MU_IER if self.dlab() => self.baud = (self.baud & 0xFF) | (value & 0xFF) << 8,
            MU_IER => self.ier = value,
            MU_LCR => self.lcr = value,
            MU_MCR => self.mcr = value,
            MU_SCRATCH => self.scratch = value,
            MU_CNTL => self.cntl = value,
            MU_BAUD => self.baud = value & 0xFFFF,
            // FIFO clear requests: bit 1 the receive FIFO, bit 2 the transmit
            // one, which never holds anything.
            MU_IIR if value & 0x2 != 0 => self.rx.clear(),
            _ => {}
        }
        if self.irq_line() != was_irq {
            crate::log!(
                self.log,
                Channel::Uart,
                "aux interrupt {}",
                if was_irq { "clear" } else { "raised" }
            );
        }
        Ok(())
    }

    fn irq_pending(&self) -> bool {
        self.irq_line()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mini-UART as Linux's `8250` driver sets it up: block on, 115200
    /// baud off the 500 MHz clock, receive interrupt enabled.
    fn linux_setup() -> Aux {
        let mut a = Aux::new();
        a.write(AUX_ENABLES, Width::Word, ENABLES_UART).unwrap();
        a.write(MU_BAUD, Width::Word, 541).unwrap(); // 500e6/(8*542) = 115313
        a.write(MU_IER, Width::Word, IER_RX).unwrap();
        a
    }

    #[test]
    fn the_divisor_latch_is_the_baud_counter() {
        let mut a = linux_setup();
        // What `serial8250_do_set_termios` writes for 115200 baud: DLAB, then
        // the two divisor bytes, then the line format.
        a.write(MU_LCR, Width::Word, 0x93).unwrap();
        a.write(MU_IO, Width::Word, 0x1d).unwrap();
        a.write(MU_IER, Width::Word, 0x02).unwrap();
        assert_eq!(a.read(MU_IO, Width::Word).unwrap(), 0x1d);
        assert_eq!(a.read(MU_IER, Width::Word).unwrap(), 0x02);
        a.write(MU_LCR, Width::Word, 0x13).unwrap();
        // Nothing went out on the line, and no interrupt was left enabled.
        assert!(a.take_output().is_empty());
        assert_eq!(a.read(MU_BAUD, Width::Word).unwrap(), 0x021d);
        assert_eq!(a.read(MU_IER, Width::Word).unwrap(), IER_RX);
        assert!(!a.irq_line());
    }

    #[test]
    fn transmit_is_captured() {
        let mut a = linux_setup();
        for b in b"hi" {
            a.write(MU_IO, Width::Word, u32::from(*b)).unwrap();
        }
        assert_eq!(a.take_output(), b"hi");
        assert!(a.take_output().is_empty());
    }

    #[test]
    fn a_fed_byte_arrives_a_character_time_later() {
        let mut a = linux_setup();
        a.feed(b"x");
        a.pump(0);
        assert_eq!(a.read(MU_LSR, Width::Word).unwrap() & LSR_DATA_READY, 0);
        assert!(!a.irq_line());
        // 10 bits at ~115200 baud is 86 µs.
        a.pump(86);
        assert_eq!(
            a.read(MU_LSR, Width::Word).unwrap() & LSR_DATA_READY,
            LSR_DATA_READY
        );
        assert!(a.irq_line());
        assert_eq!(
            a.read(MU_IIR, Width::Word).unwrap(),
            IIR_FIFO_ENABLES | (IIR_ID_RX << MU_IIR_ID_SHIFT)
        );
        assert_eq!(a.read(MU_IO, Width::Word).unwrap(), u32::from(b'x'));
        assert!(!a.irq_line());
        assert_eq!(
            a.read(MU_IIR, Width::Word).unwrap() & IIR_PENDING,
            IIR_PENDING
        );
    }

    #[test]
    fn the_line_waits_while_the_fifo_is_full() {
        let mut a = linux_setup();
        a.feed(b"0123456789");
        // The first character starts when the line stops being idle, so the
        // pump that notices it delivers nothing yet.
        a.pump(0);
        a.pump(10_000);
        // Eight in the FIFO, two still on the line.
        assert_eq!(a.rx.len(), FIFO_DEPTH);
        assert_eq!(a.line.len(), 2);
        assert_eq!(a.rx_backlog(), 10);
        for want in b"01234567" {
            assert_eq!(a.read(MU_IO, Width::Word).unwrap(), u32::from(*want));
        }
        a.pump(20_000);
        assert_eq!(a.rx.len(), 2);
        assert_eq!(a.rx_backlog(), 2);
    }

    #[test]
    fn nothing_is_received_while_the_receiver_is_off() {
        let mut a = linux_setup();
        a.write(MU_CNTL, Width::Word, 0).unwrap();
        a.feed(b"x");
        a.pump(0);
        a.pump(10_000);
        assert!(a.rx.is_empty());
        assert_eq!(a.rx_backlog(), 1);
        // And it is still there once the receiver comes back.
        a.write(MU_CNTL, Width::Word, CNTL_RX_ENABLE).unwrap();
        a.pump(20_000);
        assert_eq!(a.read(MU_IO, Width::Word).unwrap(), u32::from(b'x'));
    }

    #[test]
    fn the_transmit_interrupt_is_always_ready() {
        let mut a = linux_setup();
        assert!(!a.irq_line());
        a.write(MU_IER, Width::Word, IER_RX | IER_TX).unwrap();
        assert!(a.irq_line());
        assert_eq!(
            a.read(MU_IIR, Width::Word).unwrap(),
            IIR_FIFO_ENABLES | (IIR_ID_TX << MU_IIR_ID_SHIFT)
        );
    }

    #[test]
    fn the_block_disabled_means_no_interrupt() {
        let mut a = linux_setup();
        a.feed(b"x");
        a.pump(0);
        a.pump(10_000);
        assert!(a.irq_line());
        a.write(AUX_ENABLES, Width::Word, 0).unwrap();
        assert!(!a.irq_line());
        assert_eq!(a.read(AUX_IRQ, Width::Word).unwrap(), 0);
    }

    #[test]
    fn the_fifo_level_is_reported() {
        let mut a = linux_setup();
        a.feed(b"abc");
        a.pump(0);
        a.pump(10_000);
        let stat = a.read(MU_STAT, Width::Word).unwrap();
        assert_eq!(stat & STAT_SYMBOL_AVAILABLE, STAT_SYMBOL_AVAILABLE);
        assert_eq!(stat >> MU_STAT_RX_FIFO_LEVEL_SHIFT & 0xF, 3);
    }
}
