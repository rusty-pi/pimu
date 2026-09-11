//! PL011 (`UART0`) transmit, enough to be a console.
//!
//! Register layout and bit names follow `src/periph/uart_pl011.rs`, which is the
//! model this frontend will eventually be driving; keeping the two in the same
//! vocabulary means a mismatch shows up as a diff rather than as a puzzle.
//!
//! Address: the BCM2711 runs in "low peripheral" mode with the peripheral
//! window at `0xFE00_0000`, so `UART0` is at `0xFE20_1000`. QEMU's `raspi4b`
//! wires this PL011 to `serial_hd(0)` (`hw/arm/bcm2835_peripherals.c`), i.e. to
//! `-serial stdio`; the mini-UART is serial 1.

use core::fmt;
use core::ptr::{read_volatile, write_volatile};

/// BCM2711 low-peripheral-mode base, ARM physical view.
pub const PERIPH_BASE: usize = 0xFE00_0000;
/// PL011 `UART0`.
pub const UART0_BASE: usize = PERIPH_BASE + 0x0020_1000;

// Register offsets (PL011 TRM, and src/periph/uart_pl011.rs).
const DR: usize = 0x00;
const FR: usize = 0x18;
const IBRD: usize = 0x24;
const FBRD: usize = 0x28;
const LCRH: usize = 0x2C;
const CR: usize = 0x30;
const IMSC: usize = 0x38;
const ICR: usize = 0x44;

// FR bits.
const FR_TXFF: u32 = 1 << 5; // transmit FIFO full

// CR bits.
const CR_UARTEN: u32 = 1 << 0;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;

// LCRH bits.
const LCRH_FEN: u32 = 1 << 4; // FIFOs enabled
const LCRH_WLEN_8: u32 = 0b11 << 5;

/// Transmit-only PL011 console.
pub struct Uart {
    base: usize,
}

impl Uart {
    /// # Safety
    /// `base` must be the MMIO base of a PL011 that nothing else is driving.
    pub const unsafe fn new(base: usize) -> Uart {
        Uart { base }
    }

    unsafe fn reg_write(&self, off: usize, value: u32) {
        write_volatile((self.base + off) as *mut u32, value);
    }

    unsafe fn reg_read(&self, off: usize) -> u32 {
        read_volatile((self.base + off) as *const u32)
    }

    /// Bring the UART up at 115200 8N1.
    ///
    /// QEMU's PL011 accepts `DR` writes whatever the enable bits say, so this is
    /// not what makes the output appear under emulation — it is here so the same
    /// image is correct on real silicon, where UART0 comes out of reset
    /// disabled. The divisors assume the Pi 4's default 48 MHz UART clock:
    /// 48e6 / (16 * 115200) = 26.0417, so IBRD = 26 and FBRD = round(0.0417*64) = 3.
    pub fn init(&self) {
        unsafe {
            self.reg_write(CR, 0); // disable while reprogramming
            self.reg_write(ICR, 0x7FF); // clear all pending interrupts
            self.reg_write(IBRD, 26);
            self.reg_write(FBRD, 3);
            self.reg_write(LCRH, LCRH_WLEN_8 | LCRH_FEN);
            self.reg_write(IMSC, 0); // polled output only, no interrupts
            self.reg_write(CR, CR_UARTEN | CR_TXE | CR_RXE);
        }
    }

    pub fn putc(&self, c: u8) {
        unsafe {
            while self.reg_read(FR) & FR_TXFF != 0 {
                core::hint::spin_loop();
            }
            self.reg_write(DR, c as u32);
        }
    }

    pub fn puts(&self, s: &str) {
        for b in s.bytes() {
            // The console on the other end is a terminal, not a tty in raw mode.
            if b == b'\n' {
                self.putc(b'\r');
            }
            self.putc(b);
        }
    }

    /// Write the low `digits` nibbles of `v` as `0x`-prefixed hex.
    ///
    /// This exists so the fault handler can report without `core::fmt`. A
    /// handler must not be able to fault, and `core::fmt` dispatches through
    /// vtable pointers — absolute addresses baked in at link time, which is
    /// precisely what a mis-loaded image gets wrong (the `0x200000` incident;
    /// see link.ld). `write!` is fine for the banner, which only runs once the
    /// placement check has passed; it is not fine for the handler that catches
    /// the case where the check was not enough.
    pub fn puthex(&self, v: u64, digits: u32) {
        self.puts("0x");
        let mut i = digits;
        while i > 0 {
            i -= 1;
            let nibble = ((v >> (i * 4)) & 0xf) as u8;
            self.putc(match nibble {
                0..=9 => b'0' + nibble,
                _ => b'a' + nibble - 10,
            });
        }
    }

    /// `0x` + 16 hex digits, for a 64-bit register.
    pub fn puthex64(&self, v: u64) {
        self.puthex(v, 16);
    }

    /// `0x` + 8 hex digits, for a 32-bit register such as `ESR_EL2`.
    pub fn puthex32(&self, v: u32) {
        self.puthex(v as u64, 8);
    }

    /// Unsigned decimal, `core::fmt`-free. Only used for small values (register
    /// numbers, vector indices), so the 20-digit buffer is more than enough.
    pub fn putdec(&self, v: u64) {
        let mut buf = [0u8; 20];
        let mut n = 0;
        let mut x = v;
        loop {
            buf[n] = b'0' + (x % 10) as u8;
            n += 1;
            x /= 10;
            if x == 0 {
                break;
            }
        }
        while n > 0 {
            n -= 1;
            self.putc(buf[n]);
        }
    }
}

impl fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.puts(s);
        Ok(())
    }
}

/// The console every part of this image writes to.
///
/// Taken by value rather than kept in a `static mut`: the scaffolding is
/// single-threaded with interrupts masked, so there is no state worth sharing
/// and no lock worth paying for.
///
/// # Safety
/// Caller must be on the boot CPU with no other PL011 user.
pub unsafe fn console() -> Uart {
    Uart::new(UART0_BASE)
}
