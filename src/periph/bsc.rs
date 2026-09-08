//! BCM2711 Broadcom Serial Controller (BSC / I²C master) + the board PMIC.
//!
//! `start4.elf` reads the Raspberry Pi 4 PMIC (slave `0x1B`) over the BSC
//! instance at `0x7E20_5E00`. With that block unmodelled the `S` register never
//! reports `DONE`, so the firmware's poll loop spins until an 8 s timeout
//! (`PMIC: timeout reading reg 00 (error -1)`) — and because the PMIC also
//! fronts the "external" GPIO pins (`LEDS_PWR_OK` …), `gpioman` never finishes
//! and the boot wedges retrying it.
//!
//! We model the register interface and run every transfer to completion
//! synchronously against [`Pmic`], a tiny register-file device: a write sets
//! the register pointer (and then streams data), a read returns bytes from the
//! pointer onward. Values default to `0`; the few the firmware actually checks
//! can be seeded in [`Pmic::new`].
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

use std::collections::{BTreeMap, VecDeque};

use crate::bus::{BusResult, MmioDevice, Width};

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

/// The 7-bit I²C addresses the Pi 4 board PMIC answers to. The DA9090 exposes
/// two register pages on adjacent addresses (`0x1B` and `0x1E`); the firmware
/// talks to both during rail bring-up.
pub const PMIC_ADDRS: [u8; 2] = [0x1B, 0x1E];

/// A minimal register-file I²C peripheral: byte-addressable registers with an
/// auto-incrementing pointer, matching how the firmware drives the PMIC (write
/// the register offset, then read/write data).
pub struct Pmic {
    regs: BTreeMap<u8, u8>,
    ptr: u8,
}

impl Default for Pmic {
    fn default() -> Self {
        Pmic::new()
    }
}

impl Pmic {
    pub fn new() -> Pmic {
        // Seed the handful of registers the firmware inspects. These are
        // best-effort — refine against a datasheet if the PMIC probe starts
        // rejecting the part. Everything unset reads back 0.
        let regs = BTreeMap::new();
        Pmic { regs, ptr: 0 }
    }

    fn write_byte(&mut self, b: u8) {
        self.regs.insert(self.ptr, b);
        self.ptr = self.ptr.wrapping_add(1);
    }

    fn read_byte(&mut self) -> u8 {
        let v = self.regs.get(&self.ptr).copied().unwrap_or(0);
        self.ptr = self.ptr.wrapping_add(1);
        v
    }
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
    /// The model runs each transfer to completion inside `start()`, but the
    /// driver expects to observe the hardware sequence: `S.TA` (transfer
    /// active) rises, then falls with `S.DONE`. So the first `S` read after
    /// `ST` reports `TA` alone (no `DONE`), and every read after that reports
    /// `DONE` alone — never both together, or the driver latches the stale
    /// `TA` bit and fails the transfer.
    ta_shots: u32,
    tx: VecDeque<u8>,
    rx: VecDeque<u8>,
    /// The device on the bus, if its address is selected.
    pmic: Pmic,
}

impl Bsc {
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
            ta_shots: 0,
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            pmic: Pmic::new(),
        }
    }

    /// Live status word: latched flags plus FIFO state.
    fn status(&self) -> u32 {
        let mut s = self.latched & (S_DONE | S_ERR | S_CLKT);
        s |= S_TXD | S_TXE; // model FIFO drains instantly
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
        // them because DONE lands immediately.
        let _ = (S_TXW, S_RXR);
        s
    }

    /// Run the transfer the `ST` bit just kicked off.
    fn start(&mut self) {
        let device = PMIC_ADDRS.contains(&(self.addr as u8 & 0x7F));
        let len = self.dlen as usize;

        if self.c & C_READ != 0 {
            self.rx.clear();
            if device {
                for _ in 0..len {
                    let b = self.pmic.read_byte();
                    self.rx.push_back(b);
                }
            }
        } else {
            // Write: the bytes are already in `tx` (FIFO writes). The first is
            // the register offset, the rest are data.
            if device {
                if let Some(off) = self.tx.pop_front() {
                    self.pmic.ptr = off;
                    while let Some(b) = self.tx.pop_front() {
                        self.pmic.write_byte(b);
                    }
                }
            }
            self.tx.clear();
        }

        // The transfer ran to completion: every byte has moved to/from the
        // FIFO, so `DLEN` (which the driver reads back as "bytes still
        // outstanding" — it retries while `DLEN != expected - received`) is now
        // zero.
        self.dlen = 0;

        self.latched |= S_DONE;
        self.ta_shots = 1;
        if !device {
            self.latched |= S_ERR; // nobody ACKed the address
        }
        self.c &= !C_ST;
    }
}

impl MmioDevice for Bsc {
    fn name(&self) -> &'static str {
        self.name
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset & !3 {
            C => self.c,
            S => {
                let mut s = self.status();
                if self.ta_shots > 0 {
                    self.ta_shots -= 1;
                    s |= S_TA;
                }
                s
            }
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
                if self.tx.len() < 16 {
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
