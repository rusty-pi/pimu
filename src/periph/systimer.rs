//! BCM2711 system timer: a 64-bit free-running microsecond counter with four
//! compare channels. `start4.elf`'s ThreadX port arms one of them (`C0`) as its
//! periodic tick and enables the matching VPU interrupt source; the run loop
//! delivers that interrupt on `sleep` so the RTOS scheduler actually advances
//! timed waits (see [`crate::machine::Machine::timer_wake`]).

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

/// Fallback re-arm interval (µs) for a compare channel written with a value that
/// is already in the past — keeps a periodic tick going even if the firmware
/// never rewrites the compare register itself.
const DEFAULT_INTERVAL_US: u64 = 10_000;

pub struct SysTimer {
    micros: u64,
    frac_cycles: u64,
    cycles_per_us: u64,
    cs: u32,
    cmp: [u32; 4],
    /// Absolute µs deadline of each armed channel (`None` = not armed).
    deadline: [Option<u64>; 4],
    /// Re-arm interval per channel, so a match reloads the compare and the tick
    /// stays periodic.
    interval: [u64; 4],
    /// Count of CLO/CHI reads. The run loop uses this to tell a firmware
    /// `usleep` (polls the counter, is time-bounded) apart from a hung
    /// peripheral poll (never terminates) — the former deserves patience.
    pub clo_reads: u64,
}

impl SysTimer {
    pub fn new() -> SysTimer {
        SysTimer {
            micros: 0,
            frac_cycles: 0,
            cycles_per_us: VPU_HZ_DEFAULT / 1_000_000,
            cs: 0,
            cmp: [0; 4],
            deadline: [None; 4],
            interval: [DEFAULT_INTERVAL_US; 4],
            clo_reads: 0,
        }
    }

    pub fn now_us(&self) -> u64 {
        self.micros
    }

    /// Set any compare channels whose deadline the counter has now reached, and
    /// reload them for the next period.
    fn service_matches(&mut self) {
        for c in 0..4 {
            let Some(mut d) = self.deadline[c] else { continue };
            if self.micros < d {
                continue;
            }
            self.cs |= 1 << c;
            let step = self.interval[c].max(1);
            while d <= self.micros {
                d += step;
            }
            self.deadline[c] = Some(d);
        }
    }

    /// Jump the counter forward to the earliest armed compare deadline (if any),
    /// fire it, and return the channel index. Used to model `sleep` = wait for
    /// the next timer interrupt without spinning through millions of no-op µs.
    pub fn wake_to_next_match(&mut self) -> Option<u8> {
        let (ch, d) = (0..4)
            .filter_map(|c| self.deadline[c].map(|d| (c, d)))
            .min_by_key(|&(_, d)| d)?;
        if self.micros < d {
            self.micros = d;
        }
        self.service_matches();
        Some(ch as u8)
    }

    /// True if any compare channel is armed (the firmware has a tick running).
    pub fn any_armed(&self) -> bool {
        self.deadline.iter().any(Option::is_some)
    }

    /// Advance the counter by `us` microseconds unconditionally, then fire any
    /// armed compare once (`service_matches` collapses a multi-interval jump to
    /// a single match). Unlike [`Self::skip_ahead`] this does *not* stop at the
    /// next deadline — used to fast-forward a firmware `usleep()` that would
    /// otherwise run in real time because the periodic tick keeps the
    /// spin-detector from recognising it.
    pub fn jump(&mut self, us: u64) {
        self.micros = self.micros.saturating_add(us.max(1));
        self.service_matches();
    }

    /// Jump the microsecond counter forward by `us`, servicing any compare
    /// matches crossed. The run loop calls this when it catches the firmware
    /// busy-waiting on the counter (`while now - start < N`) so a multi-ms
    /// `usleep` doesn't spin through millions of no-op model instructions —
    /// but never past the next armed compare, so a tick can't be skipped.
    pub fn skip_ahead(&mut self, us: u64) {
        let cap = self
            .deadline
            .iter()
            .flatten()
            .copied()
            .filter(|&d| d > self.micros)
            .min();
        let target = self.micros.saturating_add(us.max(1));
        self.micros = match cap {
            Some(d) => target.min(d),
            None => target,
        };
        self.service_matches();
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
        self.service_matches();
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset {
            CS => self.cs,
            CLO => {
                self.clo_reads += 1;
                self.micros as u32
            }
            CHI => {
                self.clo_reads += 1;
                (self.micros >> 32) as u32
            }
            C0 => self.cmp[0],
            C1 => self.cmp[1],
            C2 => self.cmp[2],
            C3 => self.cmp[3],
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let arm = |st: &mut SysTimer, c: usize| {
            st.cmp[c] = value;
            // `value` is an absolute CLO compare. Derive the period from how far
            // ahead of "now" it is; if it's already in the past, fire promptly
            // and keep whatever interval we had.
            let delta = value.wrapping_sub(st.micros as u32) as u64;
            if delta == 0 || delta > 0x8000_0000 {
                st.deadline[c] = Some(st.micros);
            } else {
                st.interval[c] = delta;
                st.deadline[c] = Some(st.micros + delta);
            }
        };
        match offset {
            CS => self.cs &= !value, // write-1-to-clear match bits
            C0 => arm(self, 0),
            C1 => arm(self, 1),
            C2 => arm(self, 2),
            C3 => arm(self, 3),
            _ => {}
        }
        Ok(())
    }
}
