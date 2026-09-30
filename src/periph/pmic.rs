//! The Raspberry Pi 4-family board PMICs, as I²C slaves on the BSC bus.
//!
//! This is the *device* side; [`Bsc`](super::bsc::Bsc) is the master. Registers
//! and reset values: `specs/pmic_core.toml`, `specs/pmic_rails.toml` and
//! `specs/pmic_1d.toml`.
//!
//! start4 derives which parts are fitted from the board revision — nothing
//! probes the bus — so [`Pmic::for_board`] does the same: `0x1B` + `0x1E` on a
//! 4B rev 1.5, `0x1D` + `0x1E` on a 4B rev 1.4, a Pi 400 and a CM4, `0x1D`
//! alone on a 4B rev 1.1 or 1.2. With two parts fitted the second one takes the
//! rails it owns, so **`0x1E` owns rail 1 (core) wherever it is fitted**.
//!
//! The `0x1D` part's only identity check: `pmic_get_voltage` stops with fatal
//! error `0x46` unless reg `0x0F == reg 0x14 ^ 0xAD`, where `0x14` is what the
//! probe read — and on boards with `0x1E` the probe writes `0xA5` there first,
//! so the reset values have to satisfy it both ways (the `const` assertions
//! below hold them to it). There is no board with that part to measure, so its
//! values are inferred; its status register powers on healthy, since with the
//! power-ok bit clear start4 reports under-voltage and throttles the ARM.
//!
//! Rail setpoints are seeded so the firmware's own decode yields the voltages a
//! real 4B reports. Everything else reads 0 until written: outside the setpoint
//! registers, the status bits and that check, the firmware only logs what it
//! reads.

use std::collections::BTreeMap;

use crate::log::{Channel, Log};
use crate::soc::board::{TYPE_CM4, TYPE_PI400, TYPE_PI4B};
use crate::soc::Board;
use crate::spec::{pmic_1d, pmic_core, pmic_rails, Coverage};

/// The PMIC that owns the SoC core rail (start4 descriptor type `0x82`).
pub const ADDR_CORE: u8 = pmic_core::BASE as u8;
/// The 4B rev 1.5 PMIC that owns the SDRAM and I/O rails (type `0x83`).
pub const ADDR_RAILS: u8 = pmic_rails::BASE as u8;
/// The PMIC every other board has (type `0x81`).
pub const ADDR_1D: u8 = pmic_1d::BASE as u8;
/// The core-rail setpoint register on [`ADDR_CORE`], 10 mV a step.
pub const CORE_SETPOINT: u8 = pmic_core::SETPOINT_CORE as u8;

/// The "voltage change complete" bit each part polls, as `(register, bit)`.
const RAILS_SETTLED: (u8, u8) = (
    pmic_rails::STATUS as u8,
    pmic_rails::STATUS_SETTLED_MASK as u8,
);
const CORE_SETTLED: (u8, u8) = (
    pmic_core::STATUS as u8,
    pmic_core::STATUS_SETTLED_MASK as u8,
);
const SETTLED_1D: (u8, u8) = (pmic_1d::STATUS as u8, pmic_1d::STATUS_SETTLED_MASK as u8);

// The `0x1D` part's power-on values have to pass `pmic_get_voltage`'s check.
const _: () = assert!(pmic_1d::ID_RESET == pmic_1d::SETPOINT_CORE_RESET ^ 0xAD);
// Its power-on status has to read settled and as good input power.
const _: () = assert!(pmic_1d::STATUS_RESET & pmic_1d::STATUS_SETTLED_MASK != 0);
const _: () = assert!(
    pmic_1d::STATUS_RESET & (pmic_1d::STATUS_POWER_OK_MASK | pmic_1d::STATUS_LATCHED_MASK)
        == pmic_1d::STATUS_POWER_OK_MASK
);

/// Register-file storage; the status registers carry the settled latch.
pub const COVERAGE_RAILS: Coverage = Coverage {
    block: "pmic_rails",
    decoded: &[
        pmic_rails::STATUS,
        pmic_rails::SETPOINT_SDRAM,
        pmic_rails::SETPOINT_CORE,
        pmic_rails::SETPOINT_RAIL6,
        pmic_rails::SETPOINT_RAIL5,
    ],
};
pub const COVERAGE_CORE: Coverage = Coverage {
    block: "pmic_core",
    decoded: &[pmic_core::STATUS, pmic_core::SETPOINT_CORE],
};
pub const COVERAGE_1D: Coverage = Coverage {
    block: "pmic_1d",
    decoded: &[
        pmic_1d::ID,
        pmic_1d::SETPOINT_SDRAM,
        pmic_1d::SETPOINT_CORE,
        pmic_1d::STATUS,
        pmic_1d::SETPOINT_RAIL6,
        pmic_1d::SETPOINT_RAIL5,
    ],
};

/// One PMIC: a byte-addressable register file with the auto-incrementing
/// pointer the firmware's transport assumes.
#[derive(Debug, Clone)]
pub struct PmicRegs {
    addr: u8,
    regs: BTreeMap<u8, u8>,
    ptr: u8,
    /// The next data byte is the register offset, not content.
    pending_ptr: bool,
    /// The bit the firmware polls for "the setpoint has taken effect".
    settled: (u8, u8),
    /// A register a write only clears bits of, as `(register, mask)`.
    w1c: Option<(u8, u8)>,
    /// On the part owning the core rail: setpoint register and µV per step.
    core: Option<(u8, u32)>,
}

impl PmicRegs {
    fn new(addr: u8, settled: (u8, u8), seed: &[(u8, u8)]) -> PmicRegs {
        let mut regs: BTreeMap<u8, u8> = seed.iter().copied().collect();
        *regs.entry(settled.0).or_insert(0) |= settled.1;
        PmicRegs {
            addr,
            regs,
            ptr: 0,
            pending_ptr: false,
            settled,
            w1c: None,
            core: None,
        }
    }

    /// This part drives the core rail from `setpoint`.
    fn owning_core(self, setpoint: u8, uv_per_step: u32) -> PmicRegs {
        PmicRegs {
            core: Some((setpoint, uv_per_step)),
            ..self
        }
    }

    /// Writes to `reg` clear the bits of `mask` they write as 1.
    fn clearing_on_write(self, reg: u8, mask: u8) -> PmicRegs {
        PmicRegs {
            w1c: Some((reg, mask)),
            ..self
        }
    }

    /// The `0x1B` part: SDRAM and I/O rails. The seeded setpoint is what
    /// `vcgencmd measure_volts sdram_c|sdram_i|sdram_p` reports on a
    /// Raspberry Pi 4B d03115 (1.1000 V), inverted through the part's decode;
    /// the rails nothing compares are left at 0.
    fn rails() -> PmicRegs {
        PmicRegs::new(
            ADDR_RAILS,
            RAILS_SETTLED,
            &[
                (pmic_rails::REG_05 as u8, pmic_rails::REG_05_RESET as u8),
                (
                    pmic_rails::SETPOINT_SDRAM as u8,
                    pmic_rails::SETPOINT_SDRAM_RESET as u8,
                ),
            ],
        )
    }

    /// The `0x1E` part: the core rail. There is no ground truth for the
    /// *power-on* setpoint — a running board reports a live DVFS operating
    /// point — so the seed is the bottom of the band this boot writes, and it
    /// survives only until the first DVFS step.
    fn core() -> PmicRegs {
        PmicRegs::new(
            ADDR_CORE,
            CORE_SETTLED,
            &[(CORE_SETPOINT, pmic_core::SETPOINT_CORE_RESET as u8)],
        )
        .owning_core(CORE_SETPOINT, 10_000)
    }

    /// The `0x1D` part, which drives the core rail when it is alone.
    fn part_1d(alone: bool) -> PmicRegs {
        let part = PmicRegs::new(
            ADDR_1D,
            SETTLED_1D,
            &[
                (
                    pmic_1d::SETPOINT_CORE as u8,
                    pmic_1d::SETPOINT_CORE_RESET as u8,
                ),
                (pmic_1d::ID as u8, pmic_1d::ID_RESET as u8),
                (
                    pmic_1d::SETPOINT_SDRAM as u8,
                    pmic_1d::SETPOINT_SDRAM_RESET as u8,
                ),
                (pmic_1d::STATUS as u8, pmic_1d::STATUS_RESET as u8),
            ],
        )
        .clearing_on_write(pmic_1d::STATUS as u8, pmic_1d::STATUS_LATCHED_MASK as u8);
        if alone {
            part.owning_core(pmic_1d::SETPOINT_CORE as u8, 6_250)
        } else {
            part
        }
    }

    /// A register's current value, for tests and machine-level probes.
    pub fn reg(&self, r: u8) -> u8 {
        self.regs.get(&r).copied().unwrap_or(0)
    }
}

/// The board's PMICs: one device, since they share a bus.
#[derive(Debug, Clone)]
pub struct Pmic {
    parts: Vec<PmicRegs>,
    /// The transfer in progress.
    active: Option<usize>,
    /// Where [`Channel::Pmic`] goes: every register access.
    pub log: Log,
}

impl Default for Pmic {
    fn default() -> Self {
        Pmic::for_board(Board::default())
    }
}

impl Pmic {
    /// The PMICs `board` has fitted; unlisted board types get rev 1.5's pair.
    pub fn for_board(board: Board) -> Pmic {
        let parts = match (board.board_type(), board.pcb_revision()) {
            (TYPE_PI4B, ..=2) => vec![PmicRegs::part_1d(true)],
            (TYPE_PI4B, 5..) => vec![PmicRegs::core(), PmicRegs::rails()],
            (TYPE_PI4B | TYPE_PI400 | TYPE_CM4, _) => {
                vec![PmicRegs::core(), PmicRegs::part_1d(false)]
            }
            _ => vec![PmicRegs::core(), PmicRegs::rails()],
        };
        Pmic {
            parts,
            active: None,
            log: Log::default(),
        }
    }

    /// The core rail's voltage; the AVS monitor's channel 3 follows it.
    pub fn core_rail_uv(&self) -> Option<u32> {
        self.parts
            .iter()
            .find_map(|p| p.core.map(|(reg, step)| u32::from(p.reg(reg)) * step))
    }

    /// Does one of the parts answer this 7-bit address?
    pub fn responds_to(&self, addr: u8) -> bool {
        self.parts.iter().any(|p| p.addr == addr)
    }

    fn select(&mut self, addr: u8) -> Option<&mut PmicRegs> {
        self.active = self.parts.iter().position(|p| p.addr == addr);
        self.active.map(|i| &mut self.parts[i])
    }

    /// One part, by address. For tests.
    pub fn part(&self, addr: u8) -> Option<&PmicRegs> {
        self.parts.iter().find(|p| p.addr == addr)
    }

    /// A transfer begins. The register pointer is *not* reset: a read burst
    /// continues where the preceding one-byte write left it.
    pub fn begin(&mut self, addr: u8, read: bool) {
        if let Some(part) = self.select(addr) {
            // The first byte of a write selects the register.
            part.pending_ptr = !read;
        }
    }

    pub fn write_byte(&mut self, b: u8) {
        let Some(i) = self.active else { return };
        let part = &mut self.parts[i];
        if part.pending_ptr {
            part.ptr = b;
            part.pending_ptr = false;
            return;
        }
        crate::log!(
            self.log,
            Channel::Pmic,
            "{:02x} W {:02x} = {:02x}",
            part.addr,
            part.ptr,
            b
        );
        match part.w1c {
            Some((reg, mask)) if reg == part.ptr => {
                *part.regs.entry(reg).or_insert(0) &= !(b & mask);
            }
            _ => {
                part.regs.insert(part.ptr, b);
            }
        }
        // Nothing to ramp here, so "settled" latches at once; the firmware
        // polls it until set.
        let (reg, bit) = part.settled;
        *part.regs.entry(reg).or_insert(0) |= bit;
        part.ptr = part.ptr.wrapping_add(1);
    }

    pub fn read_byte(&mut self) -> u8 {
        let Some(i) = self.active else { return 0 };
        let part = &mut self.parts[i];
        let v = part.regs.get(&part.ptr).copied().unwrap_or(0);
        crate::log!(
            self.log,
            Channel::Pmic,
            "{:02x} R {:02x} -> {:02x}",
            part.addr,
            part.ptr,
            v
        );
        part.ptr = part.ptr.wrapping_add(1);
        v
    }
}

impl crate::periph::bsc::I2cSlave for Pmic {
    fn responds_to(&self, addr: u8) -> bool {
        Pmic::responds_to(self, addr)
    }

    fn begin(&mut self, addr: u8, read: bool) {
        Pmic::begin(self, addr, read)
    }

    fn write_byte(&mut self, b: u8) {
        Pmic::write_byte(self, b)
    }

    fn read_byte(&mut self) -> u8 {
        Pmic::read_byte(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soc::Stepping;

    fn fitted(revision: u32) -> Pmic {
        Pmic::for_board(Board {
            stepping: Stepping::C0,
            revision,
        })
    }

    fn addrs(revision: u32) -> Vec<u8> {
        let mut addrs: Vec<u8> = fitted(revision).parts.iter().map(|p| p.addr).collect();
        addrs.sort();
        addrs
    }

    #[test]
    fn each_board_gets_the_pmics_start4_drives_on_it() {
        for rev in [0x00A0_3111, 0x00C0_3111, 0x00C0_3112] {
            assert_eq!(addrs(rev), [0x1D], "{rev:06x}");
        }
        for rev in [
            0x00B0_3114,
            0x00D0_3114,
            0x00C0_3130,
            0x00A0_3140,
            0x00D0_3140,
        ] {
            assert_eq!(addrs(rev), [0x1D, 0x1E], "{rev:06x}");
        }
        for rev in [0x00B0_3115, 0x00D0_3115] {
            assert_eq!(addrs(rev), [0x1B, 0x1E], "{rev:06x}");
        }
    }

    /// `0x1E` takes the core rail wherever it is fitted.
    #[test]
    fn the_core_rail_goes_to_0x1e_when_it_is_fitted() {
        assert_eq!(fitted(0x00D0_3115).core_rail_uv(), Some(850_000));
        assert_eq!(fitted(0x00D0_3114).core_rail_uv(), Some(850_000));
        assert_eq!(fitted(0x00C0_3112).core_rail_uv(), Some(0xA5 * 6_250));
    }

    /// start4 takes the `0x1D` part's input power as good only while
    /// `STATUS & 0x60 == 0x20`, and writes `0x40` back when bit 6 is set.
    #[test]
    fn the_0x1d_status_reads_as_good_power_and_clears_its_latch_on_write() {
        let status = pmic_1d::STATUS as u8;
        for rev in [0x00B0_3112, 0x00B0_3114] {
            let mut pmic = fitted(rev);
            assert_eq!(pmic.part(ADDR_1D).unwrap().reg(status), 0x30, "{rev:06x}");

            let part = pmic.parts.iter_mut().find(|p| p.addr == ADDR_1D);
            part.unwrap().regs.insert(status, 0x70);
            pmic.begin(ADDR_1D, false);
            pmic.write_byte(status);
            pmic.write_byte(0x40);
            assert_eq!(pmic.part(ADDR_1D).unwrap().reg(status), 0x30, "{rev:06x}");

            pmic.begin(ADDR_1D, false);
            pmic.write_byte(status);
            pmic.write_byte(0x00);
            assert_eq!(pmic.part(ADDR_1D).unwrap().reg(status), 0x30, "{rev:06x}");
        }
    }
}
