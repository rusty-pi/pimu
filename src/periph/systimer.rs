//! BCM2711 system timer: a 64-bit free-running microsecond counter with four
//! compare channels. `start4.elf`'s ThreadX port arms one of them (`C0`) as its
//! periodic tick and enables the matching VPU interrupt source; the run loop
//! delivers that interrupt whenever a compare comes due — and on `sleep`, where
//! the idle loop parks with interrupts masked — so the RTOS scheduler actually
//! advances timed waits (see [`crate::bus::Bus::timer_tick_slot`]).

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
    /// Per-channel "this compare fired and its interrupt has not been taken
    /// yet" flag, set by [`Self::service_matches`] and cleared by
    /// [`Self::take_pending_channel`].
    ///
    /// Each compare channel is its own VPU interrupt source (`64 + channel`)
    /// with its own vector-table entry, and start4 uses more than one: channel 0
    /// is the ThreadX periodic tick (source 64 -> `0x3EC40B7C`) and channel 2 is
    /// the clock service's timeout timer (source 66 -> `0x3EC3E9BC`), which is
    /// what releases a thread blocked in `msleep`. Collapsing them into one flag
    /// delivered every match as source 64, so the clock-service timeouts never
    /// fired and every blocking `msleep` hung forever.
    ///
    /// Independent of `cs` — the tick ISR acks `CS` itself.
    pending: [bool; 4],
    /// Whether any of [`Self::pending`] is set, kept in step with it so the
    /// per-instruction poll is a bool read rather than a four-way scan.
    pending_any: bool,
    /// `RVF_DBG_CMP=1`: log every compare-register arm.
    dbg_cmp: bool,
    arms: u64,
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
            pending: [false; 4],
            pending_any: false,
            dbg_cmp: std::env::var_os("RVF_DBG_CMP").is_some(),
            arms: 0,
        }
    }

    /// Consume the lowest-numbered channel whose compare has fired, if any.
    /// The caller is expected to vector interrupt source `64 + channel`.
    pub fn take_pending_channel(&mut self) -> Option<u8> {
        let c = self.pending_channel()?;
        self.pending[c as usize] = false;
        self.pending_any = self.pending.iter().any(|&p| p);
        Some(c)
    }

    /// Consume the "a compare fired since last checked" flag.
    pub fn take_tick_pending(&mut self) -> bool {
        self.take_pending_channel().is_some()
    }

    /// Peek the lowest-numbered pending channel without consuming it. The run
    /// loop uses this so a match that becomes due while interrupts are masked /
    /// an ISR is running stays latched until it can actually be delivered (real
    /// hardware holds the compare-match line asserted until it is acked),
    /// instead of being silently dropped.
    pub fn pending_channel(&self) -> Option<u8> {
        // `pending_any` short-circuits the scan. The run loop asks this once
        // per retired instruction — nearly two billion times a boot — and the
        // answer is almost always "nothing".
        if !self.pending_any {
            return None;
        }
        (0..4).find(|&c| self.pending[c]).map(|c| c as u8)
    }

    /// Peek the pending-tick flag without consuming it.
    pub fn tick_pending(&self) -> bool {
        self.pending_channel().is_some()
    }

    pub fn now_us(&self) -> u64 {
        self.micros
    }

    /// Advance the counter by `cycles` VPU cycles, reporting whether the
    /// microsecond count moved.
    ///
    /// At 54 cycles per microsecond, 53 of every 54 calls cannot change
    /// anything a compare could match on, and the run loop makes one per
    /// retired instruction. The caller uses the return value to skip its own
    /// time-derived work on those calls.
    pub fn advance(&mut self, cycles: u64) -> bool {
        let total = self.frac_cycles + cycles;
        if total < self.cycles_per_us {
            self.frac_cycles = total;
            return false;
        }
        self.micros += total / self.cycles_per_us;
        self.frac_cycles = total % self.cycles_per_us;
        self.service_matches();
        true
    }

    /// Set any compare channels whose deadline the counter has now reached.
    ///
    /// BCM system-timer compares are **one-shot**: the channel matches once,
    /// the firmware acks it via `CS` and writes a fresh `Cn`. There is no
    /// auto-reload, and the model does not invent one.
    ///
    /// It used to, behind `RVF_ONESHOT_CMP`, because the tick routing of the
    /// time never reached `0x3EC40B7C` — the only code that re-arms `C0` — so
    /// without a reload the tick stopped after its first match. That has not
    /// been true since the tick started vectoring through its priority stub
    /// (#7, `8d7c27a`): the firmware re-arms its own compares, and faking a
    /// reload only made a channel armed once as a timeout fire forever,
    /// flooding the CPU with spurious `64 + channel` interrupts.
    fn service_matches(&mut self) {
        for c in 0..4 {
            let Some(d) = self.deadline[c] else { continue };
            if self.micros < d {
                continue;
            }
            self.cs |= 1 << c;
            self.pending[c] = true;
            self.pending_any = true;
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
            C0 => self.cmp[0],
            C1 => self.cmp[1],
            C2 => self.cmp[2],
            C3 => self.cmp[3],
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let arm = |st: &mut SysTimer, c: usize| {
            if st.dbg_cmp {
                st.arms += 1;
                if st.arms <= 40 || st.arms.is_multiple_of(2000) {
                    eprintln!(
                        "[cmp] #{} C{c} <- {value:#x} now={} delta={}",
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
            if delta == 0 || delta > 0x8000_0000 {
                // Already in the past. Real hardware would fire once and then
                // not match again until the 32-bit counter wraps (~71 min); the
                // firmware re-arms it every ISR, and if its `now + interval`
                // math lands a hair behind `micros` (ISR latency, or a bad
                // interval from an unmodelled clock RPC) that would re-fire
                // every step — a runaway tick. Re-arm one retained interval
                // ahead instead so it stays periodic and bounded.
                st.deadline[c] = Some(st.micros + st.interval[c].max(1));
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
