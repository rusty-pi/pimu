//! BCM2711 system timer: a 64-bit free-running microsecond counter with four
//! compare channels. `start4.elf`'s ThreadX port arms one of them (`C0`) as its
//! periodic tick and enables the matching VPU interrupt source; the run loop
//! delivers that interrupt whenever a compare comes due — and on `sleep`, where
//! the idle loop parks with interrupts masked — so the RTOS scheduler actually
//! advances timed waits (see [`crate::bus::Bus::timer_tick_slot`]).
//!
//! Registers and fields: `specs/systimer.toml`.

use crate::bus::{BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};
use crate::spec::systimer::{C, CHI, CLO, CS, CS_M0_MASK, C_COUNT, C_STRIDE};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "systimer",
    decoded: &[CS, CLO, CHI, C],
};

fn compare_channel(offset: u32) -> Option<usize> {
    let rel = offset.checked_sub(C)?;
    (rel % C_STRIDE == 0 && rel / C_STRIDE < C_COUNT).then_some((rel / C_STRIDE) as usize)
}

/// Nominal VPU clock in Hz, used only to convert executed cycles into
/// microseconds. This is the crystal a Pi 4 runs the VPU from before the PLLs
/// come up; the model keeps that rate for the whole run, since it does not
/// follow the firmware's clock programming.
pub const VPU_HZ_DEFAULT: u64 = 54_000_000;

const CYCLES_PER_US: u64 = VPU_HZ_DEFAULT / 1_000_000;

pub struct SysTimer {
    micros: u64,
    frac_cycles: u64,
    cs: u32,
    cmp: [u32; 4],
    deadline: [Option<u64>; 4],
    /// Count of `CLO`/`CHI` reads, which tells a time-bounded firmware
    /// `usleep` apart from a hung peripheral poll.
    pub clo_reads: u64,
    /// Per-channel "fired, interrupt not yet taken" flag. Each channel is its
    /// own VPU source (`64 + channel`) and start4 uses more than one — channel
    /// 0 for the ThreadX tick, channel 2 for the timeout that releases a thread
    /// blocked in `msleep` — so they must stay separate: one shared flag would
    /// deliver every match as source 64. Independent of `cs`, which the ISR acks.
    pending: [bool; 4],
    pending_any: bool,
    fired: bool,
    log: Log,
    arms: u64,
}

impl SysTimer {
    pub fn new() -> SysTimer {
        SysTimer {
            micros: 0,
            frac_cycles: 0,
            cs: 0,
            cmp: [0; 4],
            deadline: [None; 4],
            clo_reads: 0,
            pending: [false; 4],
            pending_any: false,
            fired: false,
            log: Log::default(),
            arms: 0,
        }
    }

    pub fn channel_pending(&self, c: u8) -> bool {
        self.pending[c as usize]
    }

    pub fn take_channel(&mut self, c: u8) -> bool {
        let was = std::mem::take(&mut self.pending[c as usize]);
        self.pending_any = self.pending.iter().any(|&p| p);
        was
    }

    /// Peek the lowest-numbered pending channel without consuming it, so a
    /// match due while interrupts are masked stays latched until it can be
    /// delivered — as hardware holds the line until it is acked.
    pub fn pending_channel(&self) -> Option<u8> {
        // Asked once per retired instruction, nearly two billion times a boot,
        // and almost always "nothing".
        if !self.pending_any {
            return None;
        }
        (0..4).find(|&c| self.pending[c]).map(|c| c as u8)
    }

    pub fn tick_pending(&self) -> bool {
        self.pending_channel().is_some()
    }

    pub fn now_us(&self) -> u64 {
        self.micros
    }

    /// Where [`Channel::Cmp`] goes. The log takes its clock from this
    /// counter: every line is stamped with the model time it went out at.
    pub fn set_log(&mut self, log: Log) {
        log.set_time(self.micros);
        self.log = log;
    }

    pub fn take_fired(&mut self) -> bool {
        std::mem::take(&mut self.fired)
    }

    /// How many cycles of [`Self::advance`] it takes the counter to reach
    /// `us` (0 if it is there already).
    pub fn cycles_until(&self, us: u64) -> u64 {
        if self.micros >= us {
            return 0;
        }
        (us - self.micros)
            .saturating_mul(CYCLES_PER_US)
            .saturating_sub(self.frac_cycles)
    }

    /// Modelled time in cycles of a clock at `hz`, including the fraction of a
    /// microsecond not yet shown. The ARM side is paced by this, so it keeps up
    /// with the fast-forwards as well as with retired cycles.
    pub fn cycles_at(&self, hz: u64) -> u64 {
        let per_us = hz / 1_000_000;
        self.micros * per_us + self.frac_cycles * per_us / CYCLES_PER_US
    }

    /// Advance by `cycles` VPU cycles, reporting whether the microsecond count
    /// moved — 53 of every 54 calls cannot, and the caller skips its own
    /// time-derived work on those.
    #[inline]
    pub fn advance(&mut self, cycles: u64) -> bool {
        let total = self.frac_cycles + cycles;
        if total < CYCLES_PER_US {
            self.frac_cycles = total;
            return false;
        }
        self.advance_us(total);
        true
    }

    #[inline(never)]
    fn advance_us(&mut self, total: u64) {
        self.micros += total / CYCLES_PER_US;
        self.frac_cycles = total % CYCLES_PER_US;
        self.service_matches();
    }

    /// Set any compare channels whose deadline the counter has reached.
    ///
    /// BCM system-timer compares are **one-shot**: the firmware acks via `CS`
    /// and writes a fresh `Cn`. Faking an auto-reload would make a channel
    /// armed once as a timeout fire forever.
    fn service_matches(&mut self) {
        self.log.set_time(self.micros);
        for c in 0..4 {
            let Some(d) = self.deadline[c] else { continue };
            if self.micros < d {
                continue;
            }
            self.cs |= CS_M0_MASK << c;
            self.pending[c] = true;
            self.pending_any = true;
            self.fired = true;
            self.deadline[c] = None;
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

    pub fn next_deadline(&self) -> Option<u64> {
        self.deadline.iter().flatten().copied().min()
    }

    /// Move the counter forward to `us` (never back), firing the compares it
    /// reaches: [`Self::wake_to_next_match`] for a `sleep` something else
    /// ended early.
    pub fn advance_to(&mut self, us: u64) {
        if self.micros < us {
            self.micros = us;
        }
        self.service_matches();
    }

    pub fn any_armed(&self) -> bool {
        self.deadline.iter().any(Option::is_some)
    }

    /// Advance by `us` unconditionally, then fire any armed compare once.
    /// Unlike [`Self::skip_ahead`] this does *not* stop at the next deadline.
    pub fn jump(&mut self, us: u64) {
        self.micros = self.micros.saturating_add(us.max(1));
        self.service_matches();
    }

    /// Jump forward by `us`, servicing the compares crossed but never going
    /// past the next armed one, so a tick cannot be skipped. Used when the run
    /// loop catches the firmware busy-waiting on the counter.
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
        self.advance(cycles);
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
            o => compare_channel(o).map_or(0, |c| self.cmp[c]),
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let arm = |st: &mut SysTimer, c: usize| {
            if st.log.on(Channel::Cmp) {
                st.arms += 1;
                if st.arms <= 40 || st.arms.is_multiple_of(2000) {
                    crate::log!(
                        st.log,
                        Channel::Cmp,
                        "#{} C{c} <- {value:#x} now={} delta={}",
                        st.arms,
                        st.micros as u32,
                        value.wrapping_sub(st.micros as u32)
                    );
                }
            }
            st.cmp[c] = value;
            // `value` is an absolute CLO compare. Derive the period from how far
            // ahead of "now" it is.
            let delta = value.wrapping_sub(st.micros as u32) as u64;
            // A compare matches when the 32-bit counter equals it, so one
            // written in the past matches only after the counter wraps.
            st.deadline[c] = Some(st.micros + delta);
        };
        match offset {
            CS => self.cs &= !value, // write-1-to-clear match bits
            o => {
                if let Some(c) = compare_channel(o) {
                    arm(self, c);
                }
            }
        }
        Ok(())
    }
}
