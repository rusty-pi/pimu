//! BCM2711 Broadcom Serial Controller (BSC / I²C master).
//!
//! `start4.elf` reads the Raspberry Pi 4 board PMICs over the BSC instance at
//! `0x7E20_5E00`. With that block unmodelled the `S` register never reports
//! `DONE`, so the firmware's poll loop spins until an 8 s timeout
//! (`PMIC: timeout reading reg 00 (error -1)`) — and because the PMIC also
//! fronts the "external" GPIO pins (`LEDS_PWR_OK` …), `gpioman` never finishes
//! and the boot wedges retrying it.
//!
//! This is the *master* only: it moves bytes to and from whatever slave is
//! attached (see [`crate::periph::pmic`]) and knows nothing about it beyond its
//! 7-bit address.
//!
//! Register map (offsets from the instance base):
//! ```text
//!   0x00 C     control:  I2CEN(15)  ST(7)  CLEAR(4..5)  READ(0)
//!   0x04 S     status:   CLKT(9) ERR(8) RXF(7) TXE(6) RXD(5) TXD(4) RXR(3) TXW(2) DONE(1) TA(0)
//!   0x08 DLEN  transfer length in bytes
//!   0x0C A     slave address (7-bit)
//!   0x10 FIFO  data FIFO (16 bytes each way)
//!   0x14 DIV / 0x18 DEL / 0x1C CLKT   timing — stored, otherwise ignored
//! ```

use std::collections::VecDeque;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::periph::pmic::Pmic;

const C: u32 = 0x00;
const S: u32 = 0x04;
const DLEN: u32 = 0x08;
const A: u32 = 0x0C;
const FIFO: u32 = 0x10;
const DIV: u32 = 0x14;
const DEL: u32 = 0x18;
const CLKT: u32 = 0x1C;

// C register bits.
const C_READ: u32 = 1 << 0;
const C_CLEAR: u32 = 0b11 << 4;
const C_ST: u32 = 1 << 7;
const C_I2CEN: u32 = 1 << 15;

// S register bits.
const S_TA: u32 = 1 << 0;
const S_DONE: u32 = 1 << 1;
const S_TXW: u32 = 1 << 2;
const S_RXR: u32 = 1 << 3;
const S_TXD: u32 = 1 << 4;
const S_RXD: u32 = 1 << 5;
const S_TXE: u32 = 1 << 6;
const S_RXF: u32 = 1 << 7;
const S_ERR: u32 = 1 << 8;
const S_CLKT: u32 = 1 << 9;

pub struct Bsc {
    name: &'static str,
    c: u32,
    dlen: u32,
    addr: u32,
    div: u32,
    del: u32,
    clkt: u32,
    /// Latched status bits (`DONE` / `ERR` / `CLKT`) — sticky until written 1.
    latched: u32,
    /// A transfer kicked by `ST` but not yet "finished". The data movement
    /// happens immediately in `start()`, but `S.DONE` is held off for a few
    /// `tick()`s: `S.TA` (transfer active) reads back meanwhile. Completing
    /// synchronously inside the `C`-register write re-enters the driver's
    /// async request queue (the completion callback runs nested inside submit)
    /// and deadlocks its per-bus "processing" flag. `(ticks_left, is_error)`.
    pending: Option<(u32, bool)>,
    tx: VecDeque<u8>,
    rx: VecDeque<u8>,
    /// A write transfer kicked by `ST` whose data has not all arrived yet.
    /// `(acked, bytes still expected)`.
    ///
    /// start4's BSC transport (`0x3ECF0ED0`) sets `C.ST` for the write phase
    /// *before* it feeds the FIFO — it waits for `S.TA` in between — so the
    /// register-select byte of every `read(reg)` shows up after the transfer
    /// has nominally started. Real hardware simply stalls the transfer with
    /// `TA` asserted until the FIFO has data; so do we.
    writing: Option<(bool, usize)>,
    /// A read `ST` that arrived while a write was still stalled for data. The
    /// same transport, when `cfg[8] & 2` is clear, programs the read phase
    /// immediately after the write phase and only then pushes the register
    /// byte. The read has to wait for the write to actually happen or it would
    /// sample the wrong register. Holds the read length.
    deferred_read: Option<usize>,
    /// The slave on the bus. `None` for an instance with nothing attached —
    /// every address then goes unACKed, which is what real hardware does with
    /// an empty bus.
    slave: Option<Pmic>,
}

impl Bsc {
    /// An instance with the board PMICs on it (the `0x7E20_5E00` one).
    pub fn new(name: &'static str) -> Bsc {
        Bsc {
            name,
            c: 0,
            dlen: 0,
            addr: 0,
            div: 0,
            del: 0,
            clkt: 0,
            latched: 0,
            pending: None,
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            writing: None,
            deferred_read: None,
            slave: Some(Pmic::pi4b()),
        }
    }

    /// An instance with nothing attached. start4 probes `0x7E20_5000` for a
    /// HAT/display EEPROM at address `0x52`; with no board plugged in the
    /// address is not acknowledged and the transfer completes `DONE | ERR`.
    /// Leaving the instance unmapped instead means `S` reads back 0 and the
    /// firmware's completion poll spins forever.
    pub fn empty(name: &'static str) -> Bsc {
        Bsc {
            slave: None,
            ..Bsc::new(name)
        }
    }

    /// Read-only view of the attached slave, for tests and probes.
    pub fn slave(&self) -> Option<&Pmic> {
        self.slave.as_ref()
    }

    /// Live status word: latched flags plus FIFO state.
    fn status(&self) -> u32 {
        let mut s = self.latched & (S_DONE | S_ERR | S_CLKT);
        s |= S_TXD | S_TXE; // model FIFO drains instantly
        if self.pending.is_some() || self.writing.is_some() {
            s |= S_TA; // transfer still "in flight"
        }
        if !self.tx.is_empty() {
            s &= !S_TXE;
        }
        if !self.rx.is_empty() {
            s |= S_RXD;
        }
        if self.rx.len() >= 16 {
            s |= S_RXF;
        }
        // TXW/RXR ("needs servicing") — keep clear; the driver copes without
        // them because DONE lands as soon as the transfer settles.
        let _ = (S_TXW, S_RXR);
        s
    }

    /// Number of `tick()`s between `ST` and `S.DONE`. Long enough that the
    /// driver's submit call (and its request-queue bookkeeping) unwinds before
    /// the completion is observed; short enough to be invisible to timing.
    const COMPLETE_DELAY: u32 = 96;

    /// Advance a transfer in flight; latch `DONE` (and `ERR`) when it settles.
    fn advance(&mut self) {
        if let Some((left, err)) = self.pending {
            if left <= 1 {
                self.pending = None;
                self.latched |= S_DONE;
                if err {
                    self.latched |= S_ERR;
                }
            } else {
                self.pending = Some((left - 1, err));
            }
        }
    }

    /// Does the attached slave answer the address currently in `A`?
    fn addressed(&self) -> bool {
        let addr = (self.addr as u8) & 0x7F;
        self.slave.as_ref().is_some_and(|s| s.responds_to(addr))
    }

    /// Hand one byte of a stalled write transfer to the slave; complete the
    /// transfer, and any read waiting behind it, once the last byte lands.
    fn push_to_slave(&mut self, b: u8) {
        let Some((acked, left)) = self.writing else {
            return;
        };
        if acked {
            if let Some(slave) = self.slave.as_mut() {
                slave.write_byte(b);
            }
        }
        let left = left.saturating_sub(1);
        if left > 0 {
            self.writing = Some((acked, left));
            return;
        }
        self.writing = None;
        self.finish(acked);
        if let Some(len) = self.deferred_read.take() {
            self.run_read(len);
        }
    }

    /// Move `len` bytes from the slave into the RX FIFO.
    fn run_read(&mut self, len: usize) {
        let addr = (self.addr as u8) & 0x7F;
        let acked = self.addressed();
        self.rx.clear();
        if let Some(slave) = self.slave.as_mut().filter(|_| acked) {
            slave.begin(addr, true);
            for _ in 0..len {
                let b = slave.read_byte();
                self.rx.push_back(b);
            }
        }
        self.finish(acked);
    }

    /// Settle a transfer: every byte has moved, so `DLEN` (which the driver
    /// reads back as "bytes still outstanding" — it retries while
    /// `DLEN != expected - received`) is now zero, and `DONE` (plus `ERR` if
    /// the address went unACKed) lands a few ticks later — see `pending`.
    fn finish(&mut self, acked: bool) {
        self.dlen = 0;
        self.pending = Some((Self::COMPLETE_DELAY, !acked));
    }

    /// Run the transfer the `ST` bit just kicked off.
    fn start(&mut self) {
        let addr = (self.addr as u8) & 0x7F;
        let read = self.c & C_READ != 0;
        let acked = self.addressed();
        let len = self.dlen as usize;
        self.c &= !C_ST;

        if read {
            if self.writing.is_some() {
                // The write phase is still waiting for its register byte; this
                // read runs when that lands.
                self.deferred_read = Some(len);
                return;
            }
            self.run_read(len);
            return;
        }

        if let Some(slave) = self.slave.as_mut().filter(|_| acked) {
            slave.begin(addr, false);
        }
        if len == 0 {
            self.finish(acked);
            return;
        }
        self.writing = Some((acked, len));
        // Anything already in the FIFO goes out now, in order; the rest streams
        // in through `write()`.
        let queued: Vec<u8> = self.tx.drain(..).collect();
        for b in queued {
            self.push_to_slave(b);
        }
    }
}

impl MmioDevice for Bsc {
    fn name(&self) -> &'static str {
        self.name
    }

    fn tick(&mut self, _cycles: u64) {
        self.advance();
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset & !3 {
            C => self.c,
            S => self.status(),
            DLEN => self.dlen,
            A => self.addr,
            FIFO => self.rx.pop_front().unwrap_or(0) as u32,
            DIV => self.div,
            DEL => self.del,
            CLKT => self.clkt,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        match offset & !3 {
            C => {
                if value & C_CLEAR != 0 {
                    self.tx.clear();
                    self.rx.clear();
                    // A FIFO clear abandons anything still stalled for data.
                    self.writing = None;
                    self.deferred_read = None;
                }
                self.c = (value & !C_CLEAR) | (value & C_I2CEN);
                if value & (C_I2CEN | C_ST) == (C_I2CEN | C_ST) {
                    self.start();
                }
            }
            S => {
                // Write 1 to clear DONE / ERR / CLKT.
                self.latched &= !(value & (S_DONE | S_ERR | S_CLKT));
            }
            DLEN => self.dlen = value,
            A => self.addr = value,
            FIFO => {
                if self.writing.is_some() {
                    // A write transfer is already running: the byte goes
                    // straight out on the wire.
                    self.push_to_slave(value as u8);
                } else if self.tx.len() < 16 {
                    self.tx.push_back(value as u8);
                }
            }
            DIV => self.div = value,
            DEL => self.del = value,
            CLKT => self.clkt = value,
            _ => {}
        }
        Ok(())
    }
}
