//! PL011 UART (`UART0`): the firmware's debug console and Linux's `ttyAMA0`.
//!
//! Registers and fields: `specs/uart0.toml`.
//!
//! Transmit is instant: bytes written to `DR` are appended to an output buffer
//! that the harness drains, and the flag register never reports the transmit
//! FIFO full or busy, so polling code never stalls. The transmit interrupt is
//! never raised either — Linux's PIO path writes until `TXFF`, finds room for
//! everything, and never needs to wait for the FIFO to drain.
//!
//! Host input goes onto the *line* ([`Pl011::feed`]) and [`Pl011::pump`] clocks
//! it into the 32-entry FIFO at the programmed baud rate, against modelled time,
//! so a scripted sequence lands at the same guest instant every run. Bytes wait
//! on the line while the FIFO is full rather than being dropped as an overrun.
//! `RXIS` follows the `IFLS` trigger level and `RTIS` the receive timeout, which
//! is what `amba-pl011` enables.

use std::collections::VecDeque;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::uart0::{
    CR, CR_RESET, CR_RXE_MASK as CR_RXE, CR_UARTEN_MASK as CR_UARTEN, DR, FBRD, FR,
    FR_BUSY_MASK as FR_BUSY, FR_RXFE_MASK as FR_RXFE, FR_RXFF_MASK as FR_RXFF,
    FR_TXFE_MASK as FR_TXFE, FR_TXFF_MASK as FR_TXFF, IBRD, ICR, IFLS, IFLS_RESET,
    IFLS_RXIFLSEL_MASK, IFLS_RXIFLSEL_SHIFT, IMSC, LCRH, LCRH_FEN_MASK as LCRH_FEN, MIS, RIS,
    RIS_RT_MASK as INT_RT, RIS_RX_MASK as INT_RX,
};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "uart0",
    decoded: &[DR, FR, IBRD, FBRD, LCRH, CR, IFLS, IMSC, RIS, MIS, ICR],
};

const FIFO_DEPTH: usize = 32;

/// `UARTCLK`: the firmware's `init_uart_clock` default on a Pi 4, and the rate
/// `clk-bcm2835` reports for the UART clock the kernel divides.
const UARTCLK_HZ: u64 = 48_000_000;

pub struct Pl011 {
    pub out: Vec<u8>,
    ibrd: u32,
    fbrd: u32,
    lcrh: u32,
    cr: u32,
    ifls: u32,
    imsc: u32,
    ris: u32,
    rx: VecDeque<u8>,
    line: VecDeque<u8>,
    next_rx_us: u64,
    line_idle: bool,
    /// Modelled time the last character entered the FIFO, for `RTIS`.
    last_rx_us: u64,
    clock_live: bool,
    stalled: bool,
    /// Whether a character written to `DR` still counts as on the wire. One
    /// read of `FR` retires it, which is what a `BUSY` drain loop does.
    transmitting: bool,
    /// `PIMU_UART_STRICT_DISABLE=1`: wedge the transmitter when `UARTEN` is
    /// taken away with a character still on the wire, as the TRM warns. Off by
    /// default, because the stock `start4.elf` clears `UARTCR` that way during
    /// its baud-rate change and boots a board regardless -- so the silicon rule
    /// has a condition this does not capture yet.
    strict_disable: bool,
}

impl Default for Pl011 {
    fn default() -> Self {
        Self::new()
    }
}

impl Pl011 {
    pub fn new() -> Pl011 {
        Pl011 {
            out: Vec::new(),
            ibrd: 0,
            fbrd: 0,
            lcrh: 0,
            cr: CR_RESET,     // TXE|RXE, UARTEN clear: a PL011 comes up off
            ifls: IFLS_RESET, // both triggers at half full
            imsc: 0,
            ris: 0,
            rx: VecDeque::new(),
            line: VecDeque::new(),
            next_rx_us: 0,
            line_idle: true,
            last_rx_us: 0,
            clock_live: true,
            stalled: false,
            transmitting: false,
            strict_disable: std::env::var("PIMU_UART_STRICT_DISABLE").is_ok_and(|v| v != "0"),
        }
    }

    /// Couple the port to its clock generator. A 4B rev 1.5 wedges its
    /// transmitter for the rest of the boot if the port is enabled while
    /// `UARTCLK` is stopped, or if the generator is stopped or retuned while
    /// the port is still enabled: `FR` then reads `TXFF` set, `TXFE` clear and
    /// `BUSY` stuck, and re-initialising the port does not recover it.
    pub fn clock_state(&mut self, live: bool, disturbed: bool) {
        self.clock_live = live;
        if self.cr & CR_UARTEN != 0 && (disturbed || !live) {
            self.stalled = true;
        }
    }

    pub fn transmit_stalled(&self) -> bool {
        self.stalled
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// Put host bytes on the receive line, behind anything still in flight.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.line.extend(bytes);
    }

    pub fn rx_backlog(&self) -> usize {
        self.line.len() + self.rx.len()
    }

    pub fn irq_line(&self) -> bool {
        self.ris & self.imsc != 0
    }

    /// Advance the receiver to modelled time `now_us`: characters whose last
    /// stop bit has gone by enter the FIFO, and a FIFO left alone for 32 bit
    /// periods raises the receive timeout.
    pub fn pump(&mut self, now_us: u64) {
        if self.line.is_empty() && self.rx.is_empty() {
            return;
        }
        let char_us = self.char_us();
        let receiving = self.cr & (CR_UARTEN | CR_RXE) == CR_UARTEN | CR_RXE;
        // Input fed onto an idle line starts its first character now.
        if self.line_idle && !self.line.is_empty() {
            self.line_idle = false;
            self.next_rx_us = now_us;
        }
        while let Some(&b) = self.line.front() {
            if !receiving || self.rx.len() >= self.depth() {
                // Held back: the sender waits, and resumes a character
                // time after there is room again.
                self.next_rx_us = now_us;
                break;
            }
            if self.next_rx_us + char_us > now_us {
                break;
            }
            self.line.pop_front();
            self.next_rx_us += char_us;
            self.rx.push_back(b);
            self.last_rx_us = self.next_rx_us;
            if self.rx.len() >= self.rx_trigger() {
                self.ris |= INT_RX;
            }
        }
        if self.line.is_empty() {
            self.line_idle = true;
        }
        if !self.rx.is_empty() && now_us >= self.last_rx_us + char_us * 16 / 5 {
            self.ris |= INT_RT;
        }
    }

    /// One 8N1 character (ten bit periods) at the programmed baud rate, in µs:
    /// the divisor is `IBRD + FBRD/64` of `UARTCLK/16`. Before anything has
    /// programmed it, 115200 baud.
    fn char_us(&self) -> u64 {
        let div64 = u64::from(self.ibrd) * 64 + u64::from(self.fbrd);
        if div64 == 0 {
            return 87;
        }
        (10 * 16 * div64 * 1_000_000 / 64 / UARTCLK_HZ).max(1)
    }

    fn depth(&self) -> usize {
        if self.lcrh & LCRH_FEN != 0 {
            FIFO_DEPTH
        } else {
            1
        }
    }

    fn rx_trigger(&self) -> usize {
        if self.lcrh & LCRH_FEN == 0 {
            return 1;
        }
        match (self.ifls & IFLS_RXIFLSEL_MASK) >> IFLS_RXIFLSEL_SHIFT {
            0 => 4,
            1 => 8,
            2 => 16,
            3 => 24,
            _ => 28,
        }
    }
}

impl MmioDevice for Pl011 {
    fn name(&self) -> &'static str {
        "uart0(pl011)"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset {
            DR => {
                let b = self.rx.pop_front().map_or(0, u32::from);
                if self.rx.len() < self.rx_trigger() {
                    self.ris &= !INT_RX;
                }
                if self.rx.is_empty() {
                    self.ris &= !INT_RT;
                }
                b
            }
            FR => {
                if self.stalled {
                    return Ok(FR_TXFF | FR_BUSY);
                }
                // Transmit never fills. A character just written counts as on
                // the wire for one read, so code that drains `BUSY` before it
                // clears `UARTCR` sees it go by and code that does not stalls.
                if self.transmitting {
                    self.transmitting = false;
                    return Ok(FR_BUSY);
                }
                let mut fr = FR_TXFE;
                if self.rx.is_empty() {
                    fr |= FR_RXFE;
                }
                if self.rx.len() >= self.depth() {
                    fr |= FR_RXFF;
                }
                fr
            }
            IBRD => self.ibrd,
            FBRD => self.fbrd,
            LCRH => self.lcrh,
            CR => self.cr,
            IFLS => self.ifls,
            IMSC => self.imsc,
            RIS => self.ris,
            MIS => self.ris & self.imsc,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        match offset {
            DR => {
                if !self.stalled {
                    self.out.push(value as u8);
                    self.transmitting = true;
                }
            }
            IBRD => self.ibrd = value & 0xFFFF,
            FBRD => self.fbrd = value & 0x3F,
            LCRH => {
                // Toggling FEN flushes the FIFO, as the TRM warns.
                if (self.lcrh ^ value) & LCRH_FEN != 0 {
                    self.rx.clear();
                    self.ris &= !(INT_RX | INT_RT);
                }
                self.lcrh = value;
            }
            CR => {
                // The TRM's warning, and what a 4B rev 1.5 does: taking
                // `UARTEN` away while a character is still on the wire wedges
                // the transmitter. The stock clock-change callback drains
                // `BUSY` first for exactly this reason.
                if self.strict_disable
                    && value & CR_UARTEN == 0
                    && self.cr & CR_UARTEN != 0
                    && self.transmitting
                {
                    self.stalled = true;
                }
                if value & CR_UARTEN != 0 && !self.clock_live {
                    self.stalled = true;
                }
                self.cr = value;
            }
            IFLS => self.ifls = value & 0x3F,
            IMSC => self.imsc = value & 0x7FF,
            ICR => self.ris &= !value,
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux_setup() -> Pl011 {
        let mut u = Pl011::new();
        u.write(IBRD, Width::Word, 26).unwrap();
        u.write(FBRD, Width::Word, 3).unwrap();
        u.write(LCRH, Width::Word, 0x70).unwrap();
        u.write(IMSC, Width::Word, INT_RX | INT_RT).unwrap();
        u.write(CR, Width::Word, CR_RESET | CR_UARTEN).unwrap();
        u
    }

    #[test]
    fn bytes_arrive_at_the_baud_rate() {
        let mut u = linux_setup();
        assert_eq!(u.char_us(), 86);
        u.feed(b"ab");
        u.pump(1_000);
        u.pump(1_050);
        assert_ne!(u.read(FR, Width::Word).unwrap() & FR_RXFE, 0);
        u.pump(1_086);
        assert_eq!(u.rx.len(), 1, "one character time has passed");
        u.pump(5_000);
        assert_eq!(u.rx.len(), 2);
        assert_eq!(u.read(DR, Width::Word).unwrap(), u32::from(b'a'));
        assert_eq!(u.read(DR, Width::Word).unwrap(), u32::from(b'b'));
        assert_ne!(u.read(FR, Width::Word).unwrap() & FR_RXFE, 0);
    }

    #[test]
    fn a_short_burst_interrupts_on_the_timeout_and_reading_clears_it() {
        let mut u = linux_setup();
        u.feed(b"ls\r");
        u.pump(10_000);
        u.pump(10_000 + 86 * 3);
        assert_eq!(u.rx.len(), 3);
        assert!(!u.irq_line());
        u.pump(10_000 + 86 * 7);
        assert!(u.irq_line());
        assert_eq!(u.read(MIS, Width::Word).unwrap(), INT_RT);
        while u.read(FR, Width::Word).unwrap() & FR_RXFE == 0 {
            u.read(DR, Width::Word).unwrap();
        }
        assert!(!u.irq_line());
    }

    #[test]
    fn a_full_fifo_holds_the_rest_on_the_line() {
        let mut u = linux_setup();
        u.feed(&[b'x'; 40]);
        u.pump(1_000_000);
        u.pump(2_000_000);
        assert_eq!(u.rx.len(), FIFO_DEPTH);
        assert_eq!(u.rx_backlog(), 40);
        assert_ne!(u.read(RIS, Width::Word).unwrap() & INT_RX, 0);
        assert_ne!(u.read(FR, Width::Word).unwrap() & FR_RXFF, 0);
        for _ in 0..FIFO_DEPTH {
            u.read(DR, Width::Word).unwrap();
        }
        u.pump(2_000_000 + 86 * 3);
        assert_eq!(u.rx.len(), 3, "the sender resumes a character at a time");
        u.pump(3_000_000);
        assert_eq!(u.rx.len(), 8);
    }

    #[test]
    fn nothing_is_received_with_the_receiver_off() {
        let mut u = linux_setup();
        u.write(CR, Width::Word, CR_UARTEN).unwrap();
        u.feed(b"z");
        u.pump(1_000);
        assert!(u.rx.is_empty());
        u.write(CR, Width::Word, CR_UARTEN | CR_RXE).unwrap();
        u.pump(2_000);
        assert_eq!(u.read(DR, Width::Word).unwrap(), u32::from(b'z'));
    }
}
