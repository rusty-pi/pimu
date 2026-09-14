//! The Raspberry Pi 4-family board PMICs, as I²C slaves on the BSC bus at
//! `0x7E20_5E00`.
//!
//! This is the *device* side of the bus; [`Bsc`](super::bsc::Bsc) is the
//! master and knows nothing about what is attached to it beyond the 7-bit
//! address.
//!
//! Which PMICs are fitted depends on the board ([`Pmic::for_board`], #78): a 4B
//! rev 1.5 has parts at `0x1B` and `0x1E`, a 4B rev 1.4, a Pi 400 and a CM4
//! at `0x1D` and `0x1E`, and a 4B rev 1.1 or 1.2 one part at `0x1D`.
//!
//! # What start4 does
//!
//! `pmic_init` (`0x3ED4DAA8`, `vcfw/drivers/chip/vciv/2708/power.c`) picks a
//! transport, then runs one probe per PMIC the board flags word
//! (`gp+0x3BAF0`) says is fitted, in flag order 9, 10, 12, 11; each probe
//! calls `pmic_add` (`0x3ED4D85C`) with its descriptor. Verbatim from
//! `start4.elf`:
//!
//! ```text
//!   flag  probe       descriptor  type  addr  range
//!    9    0x3ED3ABEC  0x3EDFB060  0x80  0x1C  0.6 V .. 1.39375 V
//!   10    0x3EDD20F2  0x3EE00570  0x81  0x1D    0 V .. 1.39375 V
//!   11    0x3EC8C776  0x3EDE9658  0x82  0x1E  0.3 V .. 1.9 V
//!   12    0x3EC8C3EC  0x3EDE962C  0x83  0x1B  0.8 V .. 1.4 V
//! ```
//!
//! start4 derives the flags word from the revision code; nothing probes the
//! bus for it. Read out of a boot for each board (`--dump 0x3EE3E810:4`):
//!
//! ```text
//!   4B rev 1.1, 1.2   a03111 c03111 c03112   0x05DE04D7   flag 10
//!   4B rev 1.4        b03114 d03114          0x055E0CF7   flags 10, 11
//!   Pi 400            c03130                 0x0D5E0CF7   flags 10, 11
//!   CM4               a03140 d03140          0x05184CF7   flags 10, 11
//!   4B rev 1.5        b03115 d03115          0x055E18F7   flags 11, 12
//! ```
//!
//! Each descriptor carries a rail table (`+0x0C`) of 12-byte entries indexed by
//! the mailbox voltage id (1 = core, 2 = sdram_c, 3 = sdram_p, 4 = sdram_i);
//! byte 0 of an entry is the register that holds that rail's setpoint. From the
//! images at `0x3EE135AC`, `0x3EE1360C` and `0x3EE312D0`:
//!
//! ```text
//!   0x1B  rail 1 -> reg 0x0A   rails 2,3,4 -> reg 0x09   rail 5 -> 0x13   rail 6 -> 0x12
//!   0x1E  rail 1 -> reg 0x25
//!   0x1D  rail 1 -> reg 0x14   rails 2,3,4 -> reg 0x13   rail 5 -> 0x1D   rail 6 -> 0x1C
//! ```
//!
//! With two parts fitted `pmic_init` does `mask[0] &= ~mask[1]`: the part
//! added first gives up the rails the second one has. So **`0x1E` owns rail 1
//! (core) wherever it is fitted**, and `0x1B` or `0x1D` owns rails 2..6. A
//! `0x1D` on its own owns them all.
//!
//! Raw register value to microvolts is the descriptor's `+0x10` callback:
//!
//! ```text
//!   0x1B  (0x3EC8C710)  rail 1     -> raw * 5_000 + 800_000
//!                       rails 2..4 -> raw * 5_000 + 900_000
//!                       rails 5,6  -> raw * 20_000 + 10_000
//!   0x1E  (0x3EC8C9F6)  rail 1     -> raw * 10_000
//!   0x1D  (0x3EDD259A)  rails 1..4 -> raw * 6_250
//!                       rails 5,6  -> raw * 10_000
//! ```
//!
//! The `+0x18` callback runs after a setpoint write and polls a "voltage
//! settled" bit until it reads back set:
//!
//! ```text
//!   0x1B  (0x3EC8C746)  reg 0x00 bit 4
//!   0x1E  (0x3EC8C9FC)  reg 0x02 bit 3
//!   0x1D  (0x3EDD25AE)  reg 0x1A bit 4
//! ```
//!
//! Each probe also sweeps a register range once, purely to log it at debug
//! level (`"PMIC: reg %02x: %02x"`), and hangs on a part whose reads time out
//! (`PMIC: timeout reading reg 00`): `0x1B` sweeps `0x00..=0x14`, `0x1E`
//! `0x01..=0x4B` with two gaps, `0x1D` `0x00..=0x1B` without `0x0C..=0x0F`.
//!
//! # The `0x1D` part's check
//!
//! `pmic_get_voltage` (`0x3ED4D960`) compares two bytes the probes leave
//! behind, and stops with fatal error `0x46` unless
//! `gp+0xD361B ^ 0xC9 == gp+0xD3640 ^ 0x33`. It only starts checking after a
//! countdown seeded from the system timer, `(STC & 0xF) + 4` calls. The `0x1B`
//! probe stores one register there and the same register `^ 0xFA`, which
//! always passes. The `0x1D` probe stores reg `0x14`, read four times in a row,
//! and reg `0x0F ^ 0x57`, so it passes only if **`0x0F == 0x14 ^ 0xAD`**: the
//! one thing on these buses that works as a part-id check. On boards with the
//! `0x1E` part the probe writes `0xA5` to reg `0x14` just before.
//!
//! # What is modelled and where the values come from
//!
//! Rail setpoints are seeded so that the firmware's own decode above yields the
//! voltages a real 4B reports, and the boot is then free to drive them: on rev
//! 1.5 it immediately rewrites `0x1E` reg `0x25` across `0x54..0x6E` (0.84 V to
//! 1.10 V) as it moves the ARM clock, and reads the value straight back.
//!
//! There is no board with the `0x1D` part to measure. Reg `0x0F` is `0x08`,
//! which the check needs where the probe writes `0xA5` to reg `0x14`; reg
//! `0x14` powers on at `0xA5` (1.031 V) so that the check passes where it
//! does not. Its SDRAM rails get 1.1 V, as on rev 1.5.
//!
//! Everything else reads back 0 until written. That is deliberate. Outside
//! the setpoint registers, the settled bits and the `0x1D` part's check, the
//! firmware only *logs* what it reads, so inventing contents would be fiction
//! with no observable effect.

use std::collections::BTreeMap;

use crate::soc::board::{TYPE_CM4, TYPE_PI400, TYPE_PI4B};
use crate::soc::Board;
use crate::spec::{pmic_1d, pmic_core, pmic_rails, Coverage};

/// 7-bit address of the PMIC that owns the SoC core rail (descriptor
/// `0x3EDE9658`, type `0x82`).
pub const ADDR_CORE: u8 = pmic_core::BASE as u8;
/// 7-bit address of the 4B rev 1.5 PMIC that owns the SDRAM and I/O rails
/// (descriptor `0x3EDE962C`, type `0x83`).
pub const ADDR_RAILS: u8 = pmic_rails::BASE as u8;
/// 7-bit address of the PMIC every other board has (descriptor `0x3EE00570`,
/// type `0x81`).
pub const ADDR_1D: u8 = pmic_1d::BASE as u8;
/// The core-rail setpoint register on [`ADDR_CORE`], 10 mV a step.
pub const CORE_SETPOINT: u8 = pmic_core::SETPOINT_CORE as u8;

/// The "voltage change complete" bit each part's settle callback polls
/// (`0x3EC8C746` / `0x3EC8C9FC` / `0x3EDD25AE`), as `(register, bit)`.
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

/// Every register the specs list is register-file storage; the status
/// registers carry the settled latch.
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
/// pointer the firmware's transport assumes (a one-byte write selects the
/// register; a following read burst walks upward from it).
#[derive(Debug, Clone)]
pub struct PmicRegs {
    addr: u8,
    regs: BTreeMap<u8, u8>,
    ptr: u8,
    /// Set between the start of a write transfer and its first data byte: that
    /// byte is the register offset, not register content.
    pending_ptr: bool,
    /// Which bit of which register the firmware polls for "the setpoint I just
    /// wrote has taken effect".
    settled: (u8, u8),
    /// On the part that owns the SoC core rail: its setpoint register and the
    /// microvolts per step the descriptor's decode callback applies.
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
            core: None,
        }
    }

    /// This part drives the core rail from `setpoint`, `uv_per_step` a step.
    fn owning_core(self, setpoint: u8, uv_per_step: u32) -> PmicRegs {
        PmicRegs {
            core: Some((setpoint, uv_per_step)),
            ..self
        }
    }

    /// The `0x1B` part: SDRAM and I/O rails.
    fn rails() -> PmicRegs {
        // SDRAM: `vcgencmd measure_volts sdram_c|sdram_i|sdram_p` on a real
        // d03115 all report 1.1000 V. Inverting the `0x1B` decode for rails
        // 2..4 gives (1_100_000 - 900_000) / 5_000 = 40 in reg 0x09, which all
        // three rails share.
        //
        // Rails 5 and 6 (regs 0x13 and 0x12, 20 mV steps off a 10 mV base) and
        // rail 1's register (reg 0x0A, which `pmic_init` hands to `0x1E`
        // instead) are left at 0. Nothing in the boot compares them against
        // anything; the only reads are the init sweep's logging.
        PmicRegs::new(
            ADDR_RAILS,
            RAILS_SETTLED,
            &[(
                pmic_rails::SETPOINT_SDRAM as u8,
                pmic_rails::SETPOINT_SDRAM_RESET as u8,
            )],
        )
    }

    /// The `0x1E` part: the core rail.
    fn core() -> PmicRegs {
        // No ground truth for the *power-on* setpoint. `vcgencmd measure_volts
        // core` on the real board reports 0.9260 V, but that is a live DVFS
        // operating point (and not even a multiple of the 10 mV step, so it is
        // not a plain readback of reg 0x25), long after start4 has taken the
        // rail over. 85 is picked because the firmware's own decode maps it to
        // 850_000 µV — inside the descriptor's 0.3 V..1.9 V range and at the
        // bottom of the 0.84 V..1.10 V band this boot itself writes. The value
        // survives only until the first DVFS step.
        PmicRegs::new(
            ADDR_CORE,
            CORE_SETTLED,
            &[(CORE_SETPOINT, pmic_core::SETPOINT_CORE_RESET as u8)],
        )
        .owning_core(CORE_SETPOINT, 10_000)
    }

    /// The `0x1D` part, which also drives the core rail when it is alone.
    /// Its power-on values are the module docs' inferences;
    /// `specs/pmic_1d.toml` has their sources.
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
            ],
        );
        if alone {
            part.owning_core(pmic_1d::SETPOINT_CORE as u8, 6_250)
        } else {
            part
        }
    }

    /// Current value of a register, for tests and for the machine-level probes.
    pub fn reg(&self, r: u8) -> u8 {
        self.regs.get(&r).copied().unwrap_or(0)
    }
}

/// The board's PMICs. They live behind one device because they sit on the
/// same bus; [`Pmic::responds_to`] picks between them by address.
#[derive(Debug, Clone)]
pub struct Pmic {
    parts: Vec<PmicRegs>,
    /// Index into `parts` of the transfer in progress.
    active: Option<usize>,
}

impl Default for Pmic {
    fn default() -> Self {
        Pmic::for_board(Board::default())
    }
}

impl Pmic {
    /// The PMICs `board` has fitted: the ones behind the flags start4 sets for
    /// it (see the module docs). Board types not listed there get rev 1.5's
    /// pair.
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
        }
    }

    /// The SoC core rail's voltage, as the firmware's decode of the owning
    /// part's setpoint gives it. The AVS monitor's core channel follows it.
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

    /// Read-only view of one part, by address. For tests.
    pub fn part(&self, addr: u8) -> Option<&PmicRegs> {
        self.parts.iter().find(|p| p.addr == addr)
    }

    /// A transfer to `addr` begins. The register pointer is *not* reset: a read
    /// burst continues from wherever the preceding one-byte write left it,
    /// which is how the firmware addresses a register.
    pub fn begin(&mut self, addr: u8, read: bool) {
        if let Some(part) = self.select(addr) {
            // The first byte of a write selects the register.
            part.pending_ptr = !read;
        }
    }

    /// Master to slave.
    pub fn write_byte(&mut self, b: u8) {
        let Some(i) = self.active else { return };
        let part = &mut self.parts[i];
        if part.pending_ptr {
            part.ptr = b;
            part.pending_ptr = false;
            return;
        }
        if dbg() {
            eprintln!("[pmic {:02x}] W {:02x} = {:02x}", part.addr, part.ptr, b);
        }
        part.regs.insert(part.ptr, b);
        // Writing a rail setpoint starts a voltage ramp on real silicon; the
        // firmware then polls the part's "settled" bit until it sets. There is
        // nothing to ramp here, so latch it set immediately — the alternative
        // is a poll loop that never terminates.
        let (reg, bit) = part.settled;
        *part.regs.entry(reg).or_insert(0) |= bit;
        part.ptr = part.ptr.wrapping_add(1);
    }

    /// Slave to master.
    pub fn read_byte(&mut self) -> u8 {
        let Some(i) = self.active else { return 0 };
        let part = &mut self.parts[i];
        let v = part.regs.get(&part.ptr).copied().unwrap_or(0);
        if dbg() {
            eprintln!("[pmic {:02x}] R {:02x} -> {:02x}", part.addr, part.ptr, v);
        }
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

/// `RVF_DBG_PMIC=1` logs every register access, in the style of the other
/// `RVF_DBG_*` probes. Off by default.
fn dbg() -> bool {
    std::env::var_os("RVF_DBG_PMIC").is_some()
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

    /// The parts behind the flags start4 sets for each board.
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

    /// `0x1E` takes the core rail wherever it is fitted; a `0x1D` on its own
    /// keeps it.
    #[test]
    fn the_core_rail_goes_to_0x1e_when_it_is_fitted() {
        assert_eq!(fitted(0x00D0_3115).core_rail_uv(), Some(850_000));
        assert_eq!(fitted(0x00D0_3114).core_rail_uv(), Some(850_000));
        assert_eq!(fitted(0x00C0_3112).core_rail_uv(), Some(0xA5 * 6_250));
    }
}
