//! The Raspberry Pi 4B board PMICs, as I²C slaves on the BSC bus at
//! `0x7E20_5E00`.
//!
//! This is the *device* side of the bus; [`Bsc`](super::bsc::Bsc) is the
//! master and knows nothing about what is attached to it beyond the 7-bit
//! address.
//!
//! # What start4 does
//!
//! `pmic_init` (`0x3ED4DAA8`, `vcfw/drivers/chip/vciv/2708/power.c`) picks a
//! transport, then calls `pmic_add` (`0x3ED4D85C`) once per PMIC the board
//! flags word (`gp+0x3BAF0`) says is fitted. On a 4B rev `d03115` exactly two
//! flags are set — bit 12 and bit 11 — and the two descriptors that get added
//! are, verbatim from `start4.elf`:
//!
//! ```text
//!   0x3EDE962C  type 0x83  addr 0x1B  max 1_400_000 µV  min 800_000 µV
//!   0x3EDE9658  type 0x82  addr 0x1E  max 1_900_000 µV  min 300_000 µV
//! ```
//!
//! Each descriptor carries a rail table (`+0x0C`) of 12-byte entries indexed by
//! the mailbox voltage id (1 = core, 2 = sdram_c, 3 = sdram_p, 4 = sdram_i);
//! byte 0 of an entry is the register that holds that rail's setpoint. From the
//! images at `0x3EE135AC` and `0x3EE1360C`:
//!
//! ```text
//!   0x1B  rail 1 -> reg 0x0A   rails 2,3,4 -> reg 0x09   rail 5 -> 0x13   rail 6 -> 0x12
//!   0x1E  rail 1 -> reg 0x25
//! ```
//!
//! `pmic_init` then does `mask[0] &= ~mask[1]`, so with both fitted the split
//! is: **`0x1E` owns rail 1 (core), `0x1B` owns rails 2..6**.
//!
//! Raw register value to microvolts is the descriptor's `+0x10` callback:
//!
//! ```text
//!   0x1B  (0x3EC8C710)  rail 1     -> raw * 5_000 + 800_000
//!                       rails 2..4 -> raw * 5_000 + 900_000
//!                       rails 5,6  -> raw * 20_000 + 10_000
//!   0x1E  (0x3EC8C9F6)  rail 1     -> raw * 10_000
//! ```
//!
//! The `+0x18` callback runs after a setpoint write and polls a "voltage
//! settled" bit until it reads back set:
//!
//! ```text
//!   0x1B  (0x3EC8C746)  reg 0x00 bit 4
//!   0x1E  (0x3EC8C9FC)  reg 0x02 bit 3
//! ```
//!
//! Finally each part's own init sweeps a register range once, purely to log it
//! at debug level (`"PMIC: reg %02x: %02x"`), and gives up on a part whose
//! reads time out: `0x1B` sweeps `0x00..=0x14` (`0x3EC8C3EC`), `0x1E` sweeps
//! `0x01..=0x4B` with two gaps (`0x3EC8C776`). Nothing is checked against a
//! part-id register — **the board revision, not a probe, selects the driver**
//! — so those sweeps only have to answer without erroring.
//!
//! # What is modelled and where the values come from
//!
//! Rail setpoints are seeded so that the firmware's own decode above yields the
//! voltages a real 4B reports, and the boot is then free to drive them: it
//! immediately rewrites `0x1E` reg `0x25` across `0x54..0x6E` (0.84 V to
//! 1.10 V) as it moves the ARM clock, and reads the value straight back.
//!
//! Everything else reads back 0. That is deliberate. Outside the setpoint
//! registers and the two "settled" bits, the firmware only *logs* what it
//! reads — each part's init sweeps a register range once at debug verbosity and
//! throws the values away — so inventing contents would be fiction with no
//! observable effect. In particular there is no part-id register to get right:
//! the driver is chosen by board revision, never by a probe.

use crate::diag_eprintln;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// 7-bit address of the PMIC that owns the SoC core rail (descriptor
/// `0x3EDE9658`, type `0x82`).
pub const ADDR_CORE: u8 = 0x1E;
/// 7-bit address of the PMIC that owns the SDRAM and I/O rails (descriptor
/// `0x3EDE962C`, type `0x83`).
pub const ADDR_RAILS: u8 = 0x1B;

/// `0x1B` reg `0x00` bit 4 — "voltage change complete", polled by `0x3EC8C746`.
const RAILS_SETTLED: u8 = 1 << 4;
/// `0x1E` reg `0x02` bit 3 — the same thing, polled by `0x3EC8C9FC`.
const CORE_SETTLED: u8 = 1 << 3;

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
        }
    }

    /// Current value of a register, for tests and for the machine-level probes.
    pub fn reg(&self, r: u8) -> u8 {
        self.regs.get(&r).copied().unwrap_or(0)
    }
}

/// The board's PMICs. Both descriptors live behind one device because they sit
/// on the same bus; [`Pmic::responds_to`] picks between them by address.
#[derive(Debug, Clone)]
pub struct Pmic {
    parts: Vec<PmicRegs>,
    /// Index into `parts` of the transfer in progress.
    active: Option<usize>,
}

impl Default for Pmic {
    fn default() -> Self {
        Pmic::pi4b()
    }
}

impl Pmic {
    /// The pair fitted to a Raspberry Pi 4B rev `d03115`.
    pub fn pi4b() -> Pmic {
        // SDRAM: `vcgencmd measure_volts sdram_c|sdram_i|sdram_p` on a real
        // d03115 all report 1.1000 V. Inverting the `0x1B` decode for rails
        // 2..4 gives (1_100_000 - 900_000) / 5_000 = 40 in reg 0x09, which all
        // three rails share.
        let rails = PmicRegs::new(ADDR_RAILS, (0x00, RAILS_SETTLED), &[(0x09, 40)]);

        // Core: no ground truth for the *power-on* setpoint. `vcgencmd
        // measure_volts core` on the real board reports 0.9260 V, but that is a
        // live DVFS operating point (and not even a multiple of the 10 mV step,
        // so it is not a plain readback of reg 0x25), long after start4 has
        // taken the rail over. 85 is picked because the firmware's own decode
        // maps it to 850_000 µV — inside the descriptor's 0.3 V..1.9 V range
        // and at the bottom of the 0.84 V..1.10 V band this boot itself writes.
        // The value survives only until the first DVFS step.
        let core = PmicRegs::new(ADDR_CORE, (0x02, CORE_SETTLED), &[(0x25, 85)]);

        // Rails 5 and 6 (`0x1B` regs 0x13 and 0x12, 20 mV steps off a 10 mV
        // base) and rail 1's register on `0x1B` (reg 0x0A, which `pmic_init`
        // hands to `0x1E` instead) are left at 0. Nothing in the boot compares
        // them against anything; the only reads are the init sweep's logging.
        Pmic {
            parts: vec![core, rails],
            active: None,
        }
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
            diag_eprintln!("[pmic {:02x}] W {:02x} = {:02x}", part.addr, part.ptr, b);
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
            diag_eprintln!("[pmic {:02x}] R {:02x} -> {:02x}", part.addr, part.ptr, v);
        }
        part.ptr = part.ptr.wrapping_add(1);
        v
    }
}

/// `RVF_DBG_PMIC=1` logs every register access, in the style of the other
/// `RVF_DBG_*` probes. Off by default.
fn dbg() -> bool {
    crate::diag::flag("RVF_DBG_PMIC")
}
