//! The memory/MMIO bus abstraction.
//!
//! The VPU core never holds a reference to memory or peripherals. `Vpu::step`
//! takes a `Bus` — a type parameter, not a trait object, so the RAM path
//! inlines into the executor — and [`Machine`](crate::machine::Machine) is the
//! implementation that owns RAM and every peripheral. This keeps the borrow
//! graph a tree: `Emulator` owns `Vpu` and `Machine` as siblings.

use std::fmt;

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
    Unmapped {
        addr: u32,
        width: Width,
        write: bool,
    },
    Misaligned {
        addr: u32,
        width: Width,
    },
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

/// Anything the VPU can read and write; values zero-extend on load.
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

    /// The write generation of the RAM page `pc` is in, or `None` when an
    /// instruction there must not be served from a decode cache. A match with
    /// `cached` skips [`Self::read_insn`], so the fetch's bookkeeping is here.
    fn code_gen(&mut self, _pc: u32, _cached: Option<u64>) -> Option<u64> {
        None
    }

    /// The vector slot for the periodic tick source, or `None` until the
    /// firmware enables it; called once a compare deadline has been crossed.
    fn timer_tick_slot(&mut self) -> Option<u32> {
        None
    }

    /// Consume the "a compare has fired since last checked" flag, so `Sleep`
    /// can service a tick the run loop could not deliver with interrupts off.
    fn take_pending_irq(&mut self) -> Option<u32> {
        None
    }

    fn take_tick_pending(&mut self) -> bool {
        false
    }

    /// `sleep` with nothing pending: the core halts until an interrupt, so
    /// advance the timer to its next armed compare. False when none is.
    fn sleep_advance(&mut self) -> bool {
        false
    }
}

/// A memory-mapped peripheral. Offsets are relative to the device's base, and
/// accesses are naturally-aligned 32-bit ones unless the `Machine` splits them.
pub trait MmioDevice {
    fn name(&self) -> &'static str;

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32>;
    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()>;

    fn tick(&mut self, _cycles: u64) {}

    fn irq_pending(&self) -> bool {
        false
    }
}
