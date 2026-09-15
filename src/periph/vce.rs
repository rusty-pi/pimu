//! VCE — the VideoCore vector/codec engine, at `0x7F10_0000`..`0x7F14_1000`.
//!
//! start4's driver for it is `vcfw/drivers/chip/vciv/2708/vce.c` (the assert
//! strings in `start4db.elf` name the file and every function in it, which is
//! where the register map below comes from). The whole block was unmapped, so
//! every status read returned 0 from the peripheral stub and the boot stalled
//! at the very last step before `arm_loader` with
//!
//! ```text
//! VCE taking >1s to run. Status=0x0 PCs=(0x0,0x0,0x0,0x0) bad_addr=0x0
//! Last VCE launch code: 0x0 for 0 data: 0x0 for 0
//! ```
//!
//! ## What the boot asks the VCE to do
//!
//! Exactly one client runs on it in this boot: the **codec licence check**.
//! `FUN_0EC7911C` (`codec_enabled`) is called for each fourcc — `MPG2`
//! (`0x4D504732`) and `WVC1` (`0x57564331`) are the two that are licence-gated;
//! `H264`, `MJPG`, `AGIF`, … return "enabled" without asking. For a gated one it
//! calls `FUN_0EC817D0`, which drives the VCE through its driver table
//! (`FUN_0ED9CD30` returns it):
//!
//! ```text
//! [+0x0c] open                     -> handle
//! [+0x14] obtain_semaphore(handle, 1)
//! [+0x3c] loaddata(handle, blob, 0x130)      ; -> VCE data memory
//! [+0x40] loadprogram(handle, blob, 0x100)   ; -> VCE program memory
//! [+0x30] setreg(3, 0)
//! [+0x30] setreg(1, board_serial)
//! [+0x30] setreg(2, fourcc ^ (key_present ? 0x137AFEDA : 0xF00BAD34))
//! [+0x30] setreg(6, licence_key)             ; OTP row 45 (MPG2) / 46 (WVC1)
//! [+0x20] launch(handle, launch_struct, 0xC0000000)
//! [+0x34] getreg(2)                          ; -> non-zero means "key matches"
//! [+0x18] release_semaphore
//! [+0x10] close
//! ```
//!
//! So it is a genuine computation — a 256-byte obfuscated hash of the board
//! serial, the fourcc and the licence key — not a self-test or a handshake.
//! The VCE's own ISA is undocumented and not emulated here; see "What is not
//! modelled" below for what that costs.
//!
//! ## Register map
//!
//! Every address below is read straight out of the driver:
//!
//! | Address | What |
//! | --- | --- |
//! | `0x7F10_0000` | data memory — `vce_loaddata` memcpys the caller's blob here, `vce_launch_complete` copies results back out of it |
//! | `0x7F11_0000` | program memory — `vce_loadprogram`'s destination; the driver asserts the program is under `0x4000` bytes |
//! | `0x7F12_0000` | register file, register *n* at `+n*4` (`vce_getreg`/`vce_setreg`: `(n * 4 + 0x120000) | 0x7F000000`) |
//! | `0x7F14_0000` | control block (below) |
//!
//! Control block, from `vce_run_start`, `vce_run_complete`, `vce_clear_interrupt`
//! and the interrupt handler:
//!
//! | Offset | Register |
//! | --- | --- |
//! | `+0x00` | `STATUS`. Bits `[20:16]` are the **endcode** the program halted on; bit 31 is the interrupt-pending flag; bit 24 is something the driver asserts is clear (see below) |
//! | `+0x08` | `PC0` — written with the start pc by `vce_run_start`, read back as the first of the four pcs in the timeout message |
//! | `+0x0C`, `+0x10`, `+0x14` | `PC1..PC3`; `+0x14` is `pc_ex0` in the "unexpected endcode" message |
//! | `+0x20` | `RUN` — `vce_run_start` writes 1 to launch; `vce_obtain_semaphore` and `vce_release_semaphore` write 0 |
//! | `+0x24` | `INTCLR`, write-1-to-clear. `vce_clear_interrupt` writes `0x8000_0000` and then asserts `STATUS & 0x8000_0000 == 0`; `vce_run_start` writes `0xFF`; the ISR writes `0x20` for endcode 5; `vce_run_complete` writes 1 |
//! | `+0x28` | `ENDCODE_ENABLE`. `vce_run_start` writes `(1 << endcode) \| 0x20` whenever the requested endcode is non-zero |
//! | `+0x30` | `BAD_ADDR` — non-zero is "bad address in VCE ld/st" (`vce_run_complete`'s own assert text) |
//!
//! ## How completion is signalled
//!
//! `vce_run` (`FUN_0ED9D4CC`) waits on a ThreadX event flags group at
//! `gp+0x103C00` with a 100-tick timeout, and prints the two lines above when
//! the wait times out. The other half is **interrupt source 68** (`0x44`):
//! `vce_obtain_semaphore` enables it and `vce_release_semaphore` disables it,
//! both via the interrupt driver's `[+0x18]` op, and the VCE driver's init
//! registers `0x3ED9D1EA` as its handler (confirmed at runtime —
//! `--log irqtbl` prints `src 68 handler=0x3ed9d1ea`). That handler is:
//!
//! ```text
//! vce_clear_interrupt()                  ; [0x7F140024] = 0x80000000
//! endcode = ([0x7F140000] >> 16) & 0x1F
//! if endcode == 5:                       ; the always-enabled 0x20 mask bit
//!     ...kick the VCE clock in CM, [0x7F140024] = 0x20, [0x7F140028] = 0x20
//! else:
//!     ...optional client callback...
//!     _tx_event_flags_set(gp+0x103C00, 1, TX_OR)
//! ```
//!
//! So a completed run has to raise source 68 with a **non-5** endcode in
//! `STATUS`, and the handler wakes `vce_run`. The model asserts the line while
//! `STATUS` bit 31 is set; the handler's own `vce_clear_interrupt` drops it.
//!
//! ## What is modelled and what is not
//!
//! Modelled: data memory, program memory, the register file, the launch /
//! completion / interrupt protocol, and the endcode handshake. `vce_run_start`
//! only writes `ENDCODE_ENABLE` when the requested endcode is non-zero, so the
//! endcode a launch expects is recoverable: it is the one bit set in the mask
//! other than bit 5 (the clock-stall condition the ISR handles itself and which
//! `vce_run_start` always ORs in), or 0 when the mask was not written for this
//! launch. The licence check launches with flags `0xC0000000`, whose endcode
//! field is 0, and `vce_run_complete` then requires `STATUS[20:16] == 0`.
//!
//! **Not modelled: the VCE compute core.** Its instruction set is undocumented
//! and executing 256 bytes of obfuscated hash microcode is a project of its own.
//! A completed launch therefore leaves the register file zeroed rather than
//! holding whatever the program would have computed, which makes `getreg(2)` —
//! the licence-check result — read 0, i.e. "this key does not match this fourcc
//! on this board".
//!
//! That is the right answer for the board this model reproduces, and it is
//! measured, not assumed. On the reference Pi 4 (`rpi-dev`):
//!
//! ```text
//! $ vcgencmd otp_dump | sed -n '45p;46p'
//! 45:00000000
//! 46:00000000
//! $ vcgencmd codec_enabled MPG2   ->  MPG2=disabled
//! $ vcgencmd codec_enabled WVC1   ->  WVC1=disabled
//! ```
//!
//! Both licence rows are blank and both licence-gated codecs report disabled, so
//! a real VCE running the real hash on this board also returns "no match". If a
//! board with programmed licence rows is ever modelled, this is the one place
//! that would have to change — and it could only change by emulating the engine.
//!
//! Also left at a default: `PC1`..`PC3` and `BAD_ADDR` read 0 (nothing executes,
//! so there are no per-core pcs and no load/store fault), `RUN` reads back 0
//! (the engine is always already halted by the time anything could read it), and
//! `STATUS` bit 24 stays clear — `vce_obtain_semaphore` and
//! `vce_release_semaphore` in `start4db.elf` both assert it is clear on an idle
//! engine, which is all the evidence there is about it.

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

/// The control block. `PC[1..3]` read 0: nothing executes.
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
/// Where the control block's offsets start: the device is handed offsets
/// from [`vce::BASE`] for both windows.
pub const CTRL_OFF: u32 = vce_ctrl::BASE - vce::BASE;

/// Each memory, 64 KiB.
const MEM_WINDOW: u32 = vce::DATA_COUNT * vce::DATA_STRIDE;
/// 1024 registers.
const REGS_SIZE: u32 = vce::REG_COUNT * vce::REG_STRIDE;
/// `STATUS.ENDCODE`, shifted down.
const ENDCODE_BITS: u32 = STATUS_ENDCODE_MASK >> STATUS_ENDCODE_SHIFT;

/// `vce_run_start` always ORs bit 5 into `ENDCODE_ENABLE`. The interrupt
/// handler shows why: endcode 5 is a clock-stall condition it services and
/// re-arms itself, never a result a launch waits for.
const ENDCODE_STALL: u32 = 5;

/// The interrupt source `vce_obtain_semaphore` enables (`0x44`). start4's
/// handler for it is `0x3ED9D1EA`.
pub const IRQ_SRC: u32 = 68;

pub struct Vce {
    data: Vec<u8>,
    prog: Vec<u8>,
    regs: Vec<u32>,
    /// `PC0` — the start pc `vce_run_start` writes.
    pc0: u32,
    /// `ENDCODE_ENABLE` as last written.
    endcode_enable: u32,
    /// Whether `ENDCODE_ENABLE` has been written since the last launch.
    /// `vce_run_start` writes it only for a non-zero endcode, so "not written"
    /// is how a launch says "expect endcode 0".
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

    /// True while the block drives interrupt source [`IRQ_SRC`]. Level, not
    /// edge: start4's handler clears it through `INTCLR`.
    pub fn irq_asserted(&self) -> bool {
        self.int_pending
    }

    /// A launch: `RUN` was written with 1.
    ///
    /// The compute core is not emulated, so the run "finishes" in the same
    /// instant. It halts on the endcode the launch armed, raises the completion
    /// interrupt, and leaves the register file zeroed — see the module docs for
    /// why zero is the honest result for the one client that uses this block.
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
            // Nothing executes: no per-core pcs, no load/store fault, and the
            // engine is never caught mid-run.
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
                // Write-1-to-clear over the status flags. Bit 31 is the
                // interrupt-pending flag `vce_clear_interrupt` acks; the low
                // bits are per-endcode, so `0xFF` (from `vce_run_start`) or
                // `0x20` (from the ISR's endcode-5 path) retires the recorded
                // endcode. `vce_run_complete` writes 1 to retire endcode 0.
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
