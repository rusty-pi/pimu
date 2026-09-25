//! VCE — the VideoCore vector/codec engine, `0x7F10_0000`..`0x7F14_1000`.
//!
//! Registers and fields: `specs/vce.toml` (memories, register file) and
//! `specs/vce_ctrl.toml` (control block). start4's driver is
//! `vcfw/drivers/chip/vciv/2708/vce.c`, named by the asserts in `start4db.elf`.
//!
//! One client runs on it: the codec licence check, which loads a small program
//! and data blob, sets the board serial, the fourcc and the OTP licence key in
//! registers, launches, and reads register 2 back — non-zero means the key
//! matches. A completed run has to raise interrupt source 68 with a **non-5**
//! endcode (5 is a clock stall the handler services itself); the model drives
//! the line while `STATUS.INT` is set.
//!
//! **The compute core is not emulated** — its ISA is undocumented — so a launch
//! leaves the register file zeroed and the licence check reads "no match". That
//! is the right answer for the board this models: on a Raspberry Pi 4B d03115
//! OTP rows 45 and 46 are blank and `vcgencmd codec_enabled` reports `MPG2` and
//! `WVC1` disabled. Modelling a board with programmed licence rows could only
//! be done by emulating the engine.
//!
//! `PC[1..3]` and `BAD_ADDR` read 0, `RUN` reads back 0 (the engine is always
//! already halted) and `STATUS.IDLE_CHECK` stays clear.

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::vce_ctrl::{
    BAD_ADDR, ENDCODE_ENABLE, INTCLR, PC as PC0, RUN, STATUS, STATUS_ENDCODE_MASK,
    STATUS_ENDCODE_SHIFT, STATUS_INT_MASK as STATUS_INT,
};
use crate::spec::{vce, vce_ctrl, Coverage};

/// Data memory, program memory and the register file are all modelled.
pub const COVERAGE: Coverage = Coverage {
    block: "vce",
    decoded: &[vce::DATA, vce::PROG, vce::REG],
};

/// The control block.
pub const COVERAGE_CTRL: Coverage = Coverage {
    block: "vce_ctrl",
    decoded: &[STATUS, PC0, RUN, INTCLR, ENDCODE_ENABLE, BAD_ADDR],
};

/// Data memory (`vce_loaddata` / `vce_launch_complete`).
const DATA_OFF: u32 = vce::DATA;
/// Program memory (`vce_loadprogram`).
const PROG_OFF: u32 = vce::PROG;
/// Register file, register *n* at `+n*4`.
const REGS_OFF: u32 = vce::REG;
/// The control block's offsets: the device is handed offsets from
/// [`vce::BASE`] for both windows.
pub const CTRL_OFF: u32 = vce_ctrl::BASE - vce::BASE;

/// Each memory, 64 KiB.
const MEM_WINDOW: u32 = vce::DATA_COUNT * vce::DATA_STRIDE;
/// 1024 registers.
const REGS_SIZE: u32 = vce::REG_COUNT * vce::REG_STRIDE;
/// `STATUS.ENDCODE`, shifted down.
const ENDCODE_BITS: u32 = STATUS_ENDCODE_MASK >> STATUS_ENDCODE_SHIFT;

/// Endcode 5 is the clock stall the handler services itself; `vce_run_start`
/// always ORs it into `ENDCODE_ENABLE`.
const ENDCODE_STALL: u32 = 5;

/// The interrupt source `vce_obtain_semaphore` enables (68).
pub const IRQ_SRC: u32 = vce_ctrl::IRQ_VPU;

pub struct Vce {
    data: Vec<u8>,
    prog: Vec<u8>,
    regs: Vec<u32>,
    /// `PC0` — the start pc `vce_run_start` writes.
    pc0: u32,
    /// `ENDCODE_ENABLE` as last written.
    endcode_enable: u32,
    /// `ENDCODE_ENABLE` written since the last launch: `vce_run_start` writes
    /// it only for a non-zero endcode, so "not written" means endcode 0.
    endcode_armed: bool,
    /// `STATUS[20:16]`.
    endcode: u32,
    /// `STATUS` bit 31, and the interrupt line.
    int_pending: bool,
}

impl Default for Vce {
    fn default() -> Vce {
        Vce::new()
    }
}

impl Vce {
    pub fn new() -> Vce {
        Vce {
            data: vec![0; MEM_WINDOW as usize],
            prog: vec![0; MEM_WINDOW as usize],
            regs: vec![0; (REGS_SIZE / 4) as usize],
            pc0: 0,
            endcode_enable: 0,
            endcode_armed: false,
            endcode: 0,
            int_pending: false,
        }
    }

    /// True while the block drives [`IRQ_SRC`]. A level, cleared via `INTCLR`.
    pub fn irq_asserted(&self) -> bool {
        self.int_pending
    }

    /// A launch. The core is not emulated, so the run finishes in the same
    /// instant with the register file zeroed (module docs).
    fn launch(&mut self) {
        self.endcode = if self.endcode_armed {
            let wanted = self.endcode_enable & !(1 << ENDCODE_STALL);
            if wanted == 0 {
                0
            } else {
                wanted.trailing_zeros() & ENDCODE_BITS
            }
        } else {
            0
        };
        self.endcode_armed = false;
        for r in self.regs.iter_mut() {
            *r = 0;
        }
        self.int_pending = true;
    }

    fn status(&self) -> u32 {
        let mut s = (self.endcode << STATUS_ENDCODE_SHIFT) & STATUS_ENDCODE_MASK;
        if self.int_pending {
            s |= STATUS_INT;
        }
        s
    }

    fn mem_read(buf: &[u8], off: u32, width: Width) -> u32 {
        let i = off as usize;
        match width {
            Width::Byte => buf[i] as u32,
            Width::Half => u16::from_le_bytes([buf[i], buf[i + 1]]) as u32,
            Width::Word => u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]),
        }
    }

    fn mem_write(buf: &mut [u8], off: u32, width: Width, value: u32) {
        let i = off as usize;
        let b = value.to_le_bytes();
        match width {
            Width::Byte => buf[i] = b[0],
            Width::Half => buf[i..i + 2].copy_from_slice(&b[..2]),
            Width::Word => buf[i..i + 4].copy_from_slice(&b),
        }
    }
}

impl MmioDevice for Vce {
    fn name(&self) -> &'static str {
        "vce"
    }

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32> {
        if offset < PROG_OFF {
            return Ok(Vce::mem_read(&self.data, offset - DATA_OFF, width));
        }
        if offset < REGS_OFF {
            return Ok(Vce::mem_read(&self.prog, offset - PROG_OFF, width));
        }
        if offset < REGS_OFF + REGS_SIZE {
            return Ok(self.regs[((offset - REGS_OFF) / 4) as usize]);
        }
        Ok(match offset - CTRL_OFF {
            STATUS => self.status(),
            PC0 => self.pc0,
            // Nothing executes, and the engine is never caught mid-run.
            RUN | BAD_ADDR => 0,
            ENDCODE_ENABLE => self.endcode_enable,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()> {
        if offset < PROG_OFF {
            Vce::mem_write(&mut self.data, offset - DATA_OFF, width, value);
            return Ok(());
        }
        if offset < REGS_OFF {
            Vce::mem_write(&mut self.prog, offset - PROG_OFF, width, value);
            return Ok(());
        }
        if offset < REGS_OFF + REGS_SIZE {
            self.regs[((offset - REGS_OFF) / 4) as usize] = value;
            return Ok(());
        }
        match offset - CTRL_OFF {
            PC0 => self.pc0 = value,
            ENDCODE_ENABLE => {
                self.endcode_enable = value;
                self.endcode_armed = true;
            }
            INTCLR => {
                // Write-1-to-clear: bit 31 the interrupt, bit n endcode n.
                if value & STATUS_INT != 0 {
                    self.int_pending = false;
                }
                if value & (1u32 << self.endcode) != 0 {
                    self.endcode = 0;
                }
            }
            RUN if value & 1 != 0 => {
                self.launch();
            }
            _ => {}
        }
        Ok(())
    }

    fn irq_pending(&self) -> bool {
        self.int_pending
    }
}
