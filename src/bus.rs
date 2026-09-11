//! The memory/MMIO bus abstraction.
//!
//! The VPU core never holds a reference to memory or peripherals. `Vpu::step`
//! takes `&mut dyn Bus`, and [`Machine`](crate::machine::Machine) is the concrete
//! implementation that owns RAM and every peripheral and decodes addresses to
//! them. This keeps the borrow graph a tree: `Emulator` owns `Vpu` and `Machine`
//! as siblings.

use core::fmt;

/// Access width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    Byte,
    Half,
    Word,
}

impl Width {
    pub fn bytes(self) -> u32 {
        match self {
            Width::Byte => 1,
            Width::Half => 2,
            Width::Word => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusError {
    /// Nothing is mapped at this address.
    Unmapped {
        addr: u32,
        width: Width,
        write: bool,
    },
    /// Mapped, but the access is misaligned for the target.
    Misaligned { addr: u32, width: Width },
    /// Mapped region rejected the access (e.g. write to read-only).
    Faulted {
        addr: u32,
        width: Width,
        write: bool,
        reason: &'static str,
    },
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BusError::Unmapped { addr, width, write } => write!(
                f,
                "unmapped {} {:?} @ {:#010x}",
                if *write { "store" } else { "load" },
                width,
                addr
            ),
            BusError::Misaligned { addr, width } => {
                write!(f, "misaligned {:?} access @ {:#010x}", width, addr)
            }
            BusError::Faulted {
                addr,
                width,
                write,
                reason,
            } => write!(
                f,
                "faulted {} {:?} @ {:#010x}: {}",
                if *write { "store" } else { "load" },
                width,
                addr,
                reason
            ),
        }
    }
}

pub type BusResult<T> = Result<T, BusError>;

/// Anything the VPU can read from and write to. Values are zero-extended to
/// `u32` on load and truncated on store.
pub trait Bus {
    fn load(&mut self, addr: u32, width: Width) -> BusResult<u32>;
    fn store(&mut self, addr: u32, width: Width, value: u32) -> BusResult<()>;

    fn load8(&mut self, addr: u32) -> BusResult<u8> {
        self.load(addr, Width::Byte).map(|v| v as u8)
    }
    fn load16(&mut self, addr: u32) -> BusResult<u16> {
        self.load(addr, Width::Half).map(|v| v as u16)
    }
    fn load32(&mut self, addr: u32) -> BusResult<u32> {
        self.load(addr, Width::Word)
    }
    fn store8(&mut self, addr: u32, v: u8) -> BusResult<()> {
        self.store(addr, Width::Byte, v as u32)
    }
    fn store16(&mut self, addr: u32, v: u16) -> BusResult<()> {
        self.store(addr, Width::Half, v as u32)
    }
    fn store32(&mut self, addr: u32, v: u32) -> BusResult<()> {
        self.store(addr, Width::Word, v)
    }

    /// Fetch the instruction bytes at `pc` into `out`, returning its length.
    ///
    /// The default walks the instruction a halfword at a time through
    /// [`Self::load16`]; `Machine` overrides it with a slice copy when the
    /// instruction lies in RAM.
    fn read_insn(&mut self, pc: u32, out: &mut [u8; 10]) -> BusResult<u8> {
        let p0 = self.load16(pc)?;
        let len = crate::vpu::length::insn_len_bytes(p0);
        out[0..2].copy_from_slice(&p0.to_le_bytes());
        let mut i = 2u32;
        while i < len as u32 {
            let h = self.load16(pc.wrapping_add(i))?;
            out[i as usize..i as usize + 2].copy_from_slice(&h.to_le_bytes());
            i += 2;
        }
        Ok(len)
    }

    /// The interrupt vector-table slot for the periodic ThreadX tick source, if
    /// the firmware has enabled it. This does not touch the timer — the run
    /// loop calls it when a compare deadline has been crossed to deliver a
    /// genuine periodic tick (real hardware's preemption point). `None` = the
    /// tick source is not enabled yet.
    fn timer_tick_slot(&mut self) -> Option<u32> {
        None
    }

    /// Consume the "a system-timer compare has fired since last checked" flag
    /// (see [`Self::timer_tick_slot`]). `Op::Sleep` uses this to service a
    /// pending periodic tick that the run loop couldn't deliver because
    /// interrupts were disabled.
    /// Take the next device-raised interrupt source, if any. Distinct from
    /// [`Self::timer_tick_slot`]: that one answers "which vector does the
    /// system-timer compare use", this one is any peripheral asking to be
    /// serviced (currently the DMA channels' completion interrupt).
    fn take_pending_irq(&mut self) -> Option<u32> {
        None
    }

    fn take_tick_pending(&mut self) -> bool {
        false
    }

    /// `sleep` with nothing pending: real VC4 halts the core until an interrupt
    /// arrives, so advance the system timer straight to its next armed compare
    /// instead of letting the idle loop spin. Returns false if nothing is armed
    /// (then there is nothing to wake up for).
    fn sleep_advance(&mut self) -> bool {
        false
    }
}

/// A memory-mapped peripheral. Offsets are relative to the device's base.
///
/// Devices see naturally-aligned 32-bit accesses in almost all cases; the
/// `Machine` splits or rejects odd widths per-region as needed.
pub trait MmioDevice {
    /// Human-readable name, for tracing.
    fn name(&self) -> &'static str;

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32>;
    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()>;

    /// Advance any internal time-based state by `cycles` VPU cycles. Default: no-op.
    fn tick(&mut self, _cycles: u64) {}

    /// True if the device is currently asserting its interrupt line.
    fn irq_pending(&self) -> bool {
        false
    }
}
