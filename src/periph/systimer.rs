//! BCM2711 system timer: a 64-bit free-running microsecond counter.

use crate::bus::{BusResult, MmioDevice, Width};

const CS: u32 = 0x00;
const CLO: u32 = 0x04;
const CHI: u32 = 0x08;
const C0: u32 = 0x0C;
const C1: u32 = 0x10;
const C2: u32 = 0x14;
const C3: u32 = 0x18;

/// Nominal VPU clock in Hz. Used only to convert executed cycles into
/// microseconds for the timer. The real early-boot VPU clock is the crystal
/// (54 MHz on Pi 4) before PLLs come up; this is deliberately a round default
/// and can be revisited once firmware clock programming is modelled.
pub const VPU_HZ_DEFAULT: u64 = 54_000_000;

pub struct SysTimer {
    micros: u64,
    frac_cycles: u64,
    cycles_per_us: u64,
    cs: u32,
    cmp: [u32; 4],
}

impl SysTimer {
    pub fn new() -> SysTimer {
        SysTimer {
            micros: 0,
            frac_cycles: 0,
            cycles_per_us: VPU_HZ_DEFAULT / 1_000_000,
            cs: 0,
            cmp: [0; 4],
        }
    }

    pub fn now_us(&self) -> u64 {
        self.micros
    }
}

impl Default for SysTimer {
    fn default() -> Self {
        SysTimer::new()
    }
}

impl MmioDevice for SysTimer {
    fn name(&self) -> &'static str {
        "systimer"
    }

    fn tick(&mut self, cycles: u64) {
        let total = self.frac_cycles + cycles;
        self.micros += total / self.cycles_per_us;
        self.frac_cycles = total % self.cycles_per_us;
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset {
            CS => self.cs,
            CLO => self.micros as u32,
            CHI => (self.micros >> 32) as u32,
            C0 => self.cmp[0],
            C1 => self.cmp[1],
            C2 => self.cmp[2],
            C3 => self.cmp[3],
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        match offset {
            CS => self.cs &= !value, // write-1-to-clear match bits
            C0 => self.cmp[0] = value,
            C1 => self.cmp[1] = value,
            C2 => self.cmp[2] = value,
            C3 => self.cmp[3] = value,
            _ => {}
        }
        Ok(())
    }
}
