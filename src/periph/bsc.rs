//! BCM2711 Broadcom Serial Controller (BSC / I²C master).
//!
//! Registers and fields: `specs/bsc.toml`.
//!
//! `start4.elf` reads the Raspberry Pi 4 board PMICs over the BSC instance at
//! `0x7E20_5E00`. With that block unmodelled the `S` register never reports
//! `DONE`, so the firmware's poll loop spins until an 8 s timeout
//! (`PMIC: timeout reading reg 00 (error -1)`) — and because the PMIC also
//! fronts the "external" GPIO pins (`LEDS_PWR_OK` …), `gpioman` never finishes
//! and the boot wedges retrying it.
//!
//! This is the *master* only: it moves bytes to and from whatever slave answers
//! the address and knows nothing about them beyond their 7-bit addresses.
//!
//! **A transfer takes the time it would take on a real bus**: `S.TA` from the
//! `C.ST` write until the last byte has clocked out at `core_clock / CDIV`, then
//! `S.DONE` (plus `S.ERR` on an unACKed address). The firmware polls both
//! without a timeout in places, so neither may depend on how many instructions
//! it happens to retire in between.

use std::collections::VecDeque;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::log::Log;
use crate::periph::fxl6408::Fxl6408;
use crate::periph::pmic::Pmic;
use crate::spec::bsc::{
    A, C, CLKT, C_CLEAR_MASK as C_CLEAR, C_I2CEN_MASK as C_I2CEN, C_READ_MASK as C_READ,
    C_ST_MASK as C_ST, DEL, DIV, DLEN, FIFO, S, S_CLKT_MASK as S_CLKT, S_DONE_MASK as S_DONE,
    S_ERR_MASK as S_ERR, S_RXD_MASK as S_RXD, S_RXF_MASK as S_RXF, S_RXR_MASK as S_RXR,
    S_STATE_MASK as S_STATE, S_TA_MASK as S_TA, S_TXD_MASK as S_TXD, S_TXE_MASK as S_TXE,
    S_TXW_MASK as S_TXW,
};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "bsc",
    decoded: &[C, S, DLEN, A, FIFO, DIV, DEL, CLKT],
};

pub trait I2cSlave {
    fn responds_to(&self, addr: u8) -> bool;
    fn begin(&mut self, addr: u8, read: bool);
    fn write_byte(&mut self, b: u8);
    fn read_byte(&mut self) -> u8;
}

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
    /// A transfer whose bytes have all moved but which is still on the wire:
    /// `(the simulated µs at which it finishes clocking out, is_error)`.
    /// `S.TA` reads back until then and `S.DONE` only after — the master
    /// cannot report a transfer complete before the bits have been sent.
    pending: Option<(u64, bool)>,
    /// Simulated time in microseconds, taken from the system timer so the two
    /// stay in step across the run loop's `sleep` fast-forward.
    now_us: u64,
    tx: VecDeque<u8>,
    rx: VecDeque<u8>,
    /// A write kicked by `ST` whose data has not all arrived: `(acked, bytes
    /// expected)`. start4 sets `C.ST` *before* feeding the FIFO, so real
    /// hardware stalls with `TA` asserted until data arrives; so does this.
    writing: Option<(bool, usize)>,
    /// A read `ST` that arrived while a write was still stalled: it must wait
    /// for the write to happen or it would sample the wrong register.
    deferred_read: Option<usize>,
    /// The byte the shifter is still clocking out, kept so that a read armed
    /// on top of it can hand it back the way the part does.
    on_the_wire: Option<u8>,
    /// A read `ST` written while the transfer before it was still on the wire.
    /// The master takes neither: `TA` and `S.STATE` stay as they are, `DONE`
    /// never lands, and the byte in the shifter reads back out of the FIFO.
    /// Only a `C.CLEAR` or `C <- 0` gets the master out of it.
    wedged: bool,
    /// The PMICs on the bus. `None` (with `expander` also `None`) for an
    /// instance with nothing attached — every address then goes unACKed,
    /// which is what real hardware does with an empty bus.
    slave: Option<Pmic>,
    /// The GPIO expander sharing the PMIC bus.
    expander: Option<Fxl6408>,
    /// A HAT's ID EEPROM on the header bus.
    eeprom: Option<super::hat::HatEeprom>,
    /// Whether this instance's devices are on the pins it is muxed to. Only
    /// GPIO 0/1 reaches the 40-pin header, so a probe made with I²C 0 elsewhere
    /// is a probe of a bus the HAT is not on ([`Self::set_pins`]).
    pins: bool,
    /// Handed to what is on the bus, for the `pmic` and `expander` channels.
    log: Log,
}

impl Bsc {
    /// An instance with the board PMICs and GPIO expander on it (the
    /// `0x7E20_5E00` one).
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
            now_us: 0,
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            writing: None,
            deferred_read: None,
            on_the_wire: None,
            wedged: false,
            slave: Some(Pmic::default()),
            expander: Some(Fxl6408::new()),
            eeprom: None,
            pins: true,
            log: Log::default(),
        }
    }

    /// An instance with nothing attached: every address goes unACKed and the
    /// transfer completes `DONE | ERR`. Leaving it unmapped instead would read
    /// `S` as 0 and spin the firmware's completion poll forever.
    pub fn empty(name: &'static str) -> Bsc {
        Bsc {
            slave: None,
            expander: None,
            eeprom: None,
            ..Bsc::new(name)
        }
    }

    /// Put `pmic` on the bus in place of the PMICs there, for a board that has
    /// other ones fitted.
    pub fn fit_pmics(&mut self, mut pmic: Pmic) {
        pmic.log = self.log.clone();
        self.slave = Some(pmic);
    }

    /// Hand `log` to the devices on the bus, and to PMICs fitted later.
    pub fn set_log(&mut self, log: Log) {
        if let Some(pmic) = &mut self.slave {
            pmic.log = log.clone();
        }
        if let Some(expander) = &mut self.expander {
            expander.log = log.clone();
        }
        self.log = log;
    }

    pub fn slave(&self) -> Option<&Pmic> {
        self.slave.as_ref()
    }

    pub fn expander(&self) -> Option<&Fxl6408> {
        self.expander.as_ref()
    }

    pub fn attach_eeprom(&mut self, eeprom: super::hat::HatEeprom) {
        self.eeprom = Some(eeprom);
    }

    /// Say whether the bus this instance's devices are on is the one its pins
    /// are muxed to. Clear: every address goes unACKed, which is what a
    /// master whose pads are somewhere else finds.
    pub fn set_pins(&mut self, on: bool) {
        self.pins = on;
    }

    fn target(&mut self) -> Option<&mut dyn I2cSlave> {
        if !self.pins {
            return None;
        }
        let addr = (self.addr as u8) & 0x7F;
        if let Some(p) = self.slave.as_mut().filter(|p| p.responds_to(addr)) {
            return Some(p);
        }
        if let Some(e) = self.eeprom.as_mut().filter(|e| e.responds_to(addr)) {
            return Some(e);
        }
        self.expander
            .as_mut()
            .filter(|x| x.responds_to(addr))
            .map(|x| x as &mut dyn I2cSlave)
    }

    fn status(&self) -> u32 {
        let mut s = self.latched & (S_DONE | S_ERR | S_CLKT);
        s |= S_TXD | S_TXE; // the modelled FIFO drains instantly
        if self.busy() {
            s |= S_TA; // transfer still "in flight"
                       // `STATE` reads 0, 4 or 5 when the master will take a fresh `ST`
                       // and something else while a transfer is still clocking out. The
                       // bootloader polls it between the register write and the read.
            s |= S_STATE;
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
        // `TXW` / `RXR` ("needs servicing") stay clear: the driver copes
        // without them because `DONE` lands as soon as the transfer settles.
        let _ = (S_TXW, S_RXR);
        s
    }

    /// BSC core clock: the bus runs at `core_clock / CDIV`, and on a Pi 4 that
    /// is the 500 MHz VPU core clock (`vcgencmd measure_clock core` on a
    /// Raspberry Pi 4B d03115 reports 500 000 992 Hz). start4's divisors agree
    /// — 100 kHz for the PMIC bus, 200 kHz probing, ~926 kHz for HDMI DDC.
    const CORE_HZ: u64 = 500_000_000;
    const DEFAULT_CDIV: u64 = 5000;

    fn byte_us(&self) -> u64 {
        let cdiv = match self.div & 0xFFFF {
            0 => Self::DEFAULT_CDIV,
            d => d as u64,
        };
        (9 * cdiv * 1_000_000 / Self::CORE_HZ).max(1)
    }

    /// Advance simulated time; latch `DONE` (and `ERR`) on a transfer that has
    /// finished clocking out.
    pub fn advance_to(&mut self, now_us: u64) {
        self.now_us = now_us;
        if let Some((deadline, err)) = self.pending {
            if deadline <= now_us {
                self.pending = None;
                self.on_the_wire = None;
                if self.wedged {
                    return;
                }
                self.latched |= S_DONE;
                if err {
                    self.latched |= S_ERR;
                }
            }
        }
    }

    /// Whether a transfer still holds the bus.
    fn busy(&self) -> bool {
        self.pending.is_some() || self.writing.is_some() || self.wedged
    }

    fn addressed(&self) -> bool {
        if !self.pins {
            return false;
        }
        let addr = (self.addr as u8) & 0x7F;
        self.slave.as_ref().is_some_and(|s| s.responds_to(addr))
            || self.eeprom.as_ref().is_some_and(|e| e.responds_to(addr))
            || self.expander.as_ref().is_some_and(|x| x.responds_to(addr))
    }

    /// Hand one byte of a stalled write transfer to the slave; complete the
    /// transfer, and any read waiting behind it, once the last byte lands.
    fn push_to_slave(&mut self, b: u8) {
        let Some((acked, left)) = self.writing else {
            return;
        };
        if acked {
            if let Some(slave) = self.target() {
                slave.write_byte(b);
            }
        }
        self.on_the_wire = Some(b);
        let left = left.saturating_sub(1);
        if left > 0 {
            self.writing = Some((acked, left));
            return;
        }
        self.writing = None;
        // The address and every byte but this one were clocked out while
        // software was feeding the FIFO — one byte of wire time is left.
        self.finish(acked, 0);
        if let Some(len) = self.deferred_read.take() {
            self.run_read(len);
        }
    }

    fn run_read(&mut self, len: usize) {
        let addr = (self.addr as u8) & 0x7F;
        let acked = self.addressed();
        self.rx.clear();
        let bytes: Vec<u8> = match self.target() {
            Some(slave) => {
                slave.begin(addr, true);
                (0..len).map(|_| slave.read_byte()).collect()
            }
            None => Vec::new(),
        };
        self.rx.extend(bytes);
        self.finish(acked, len);
    }

    /// Settle a transfer: the bytes have moved, so `DLEN` — which the driver
    /// reads back as bytes outstanding — is zero, but the bits are still on the
    /// wire, so `TA` stays asserted and `DONE` lands only after they have been
    /// clocked. That delay also keeps `DONE` out of the `C` write that started
    /// the transfer.
    fn finish(&mut self, acked: bool, bytes: usize) {
        self.dlen = 0;
        let wire_us = self.byte_us() * (bytes as u64 + 1);
        let wire_us = crate::jitter::stretch(wire_us, "an I2C transfer");
        self.pending = Some((self.now_us + wire_us, !acked));
    }

    fn start(&mut self) {
        let addr = (self.addr as u8) & 0x7F;
        let read = self.c & C_READ != 0;
        let acked = self.addressed();
        let len = self.dlen as usize;
        self.c &= !C_ST;

        if read {
            if self.writing.is_some() {
                self.deferred_read = Some(len);
                return;
            }
            if self.pending.is_some() {
                self.wedged = true;
                self.rx.clear();
                self.rx.extend(self.on_the_wire);
                return;
            }
            self.run_read(len);
            return;
        }

        if let Some(slave) = self.target() {
            slave.begin(addr, false);
        }
        if len == 0 {
            self.finish(acked, 0);
            return;
        }
        self.writing = Some((acked, len));
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
                if value & C_CLEAR != 0 || value == 0 {
                    self.tx.clear();
                    self.rx.clear();
                    self.writing = None;
                    self.deferred_read = None;
                    self.on_the_wire = None;
                    self.pending = None;
                    self.wedged = false;
                }
                self.c = (value & !C_CLEAR) | (value & C_I2CEN);
                if value & (C_I2CEN | C_ST) == (C_I2CEN | C_ST) {
                    self.start();
                }
            }
            S => {
                self.latched &= !(value & (S_DONE | S_ERR | S_CLKT));
            }
            DLEN => self.dlen = value,
            A => self.addr = value,
            FIFO => {
                if self.writing.is_some() {
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
