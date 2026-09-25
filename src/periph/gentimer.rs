//! The Cortex-A72's generic timer — the system counter and the core's four
//! timers — driven by *modelled* time.
//!
//! It is not a memory-mapped device: all of it is system registers
//! (`CNTPCT_EL0`, `CNTP_CTL_EL0`, …), which the core answers from here on
//! `MRS`/`MSR` ([`Reg::decode`]). It lives with the peripherals because it is
//! pure state that drives GIC lines, the same whichever core runs it.
//!
//! **Why modelled time:** a counter following the host clock makes every guest
//! timestamp depend on how fast the host ran, and two runs of the same boot then
//! agree only up to the first timestamp Linux prints. Deriving it from ARM
//! cycles makes consoles byte-identical across runs, which is what the
//! regression tests compare.
//!
//! So the counter is a function of ARM cycles and nothing else, running at the
//! rate [`crate::periph::ArmLocal::counter_hz`] reports — 54 MHz once the
//! armstub has programmed the prescaler, matching the `arch_timer: cp15
//! timer(s) running at 54.00MHz (phys)` a Raspberry Pi 4B d03115 prints — and
//! standing still before that.
//!
//! The timers follow ARM ARM D11.2. The PPIs come from the dtb's `timer` node:
//! secure physical INTID 29, non-secure physical 30, virtual 27, hypervisor 26.
//! Linux binds the non-secure physical one; the others are modelled because they
//! cost nothing extra.
//!
//! Not modelled: the `CNTKCTL_EL1` / `CNTHCTL_EL2` access traps (so every EL may
//! read the counter), and the event stream.

use crate::periph::gic;

/// The nominal ARM clock: one instruction per cycle at the Pi 4's default
/// `arm_freq` of 1500 MHz. Only its constancy matters — it fixes how much
/// modelled time an instruction is worth, and so what the counter reads.
pub const ARM_HZ: u64 = 1_500_000_000;

const CTL_ENABLE: u64 = 1 << 0;
const CTL_IMASK: u64 = 1 << 1;
const CTL_ISTATUS: u64 = 1 << 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Phys,
    Virt,
    Hyp,
    SecPhys,
}

impl Which {
    pub const ALL: [Which; 4] = [Which::Phys, Which::Virt, Which::Hyp, Which::SecPhys];

    /// The GIC PPI this timer's output is wired to (module docs).
    pub fn intid(self) -> u32 {
        match self {
            Which::Phys => gic::ID_NS_PHYS_TIMER,
            Which::Virt => gic::ID_VIRT_TIMER,
            Which::Hyp => gic::ID_HYP_TIMER,
            Which::SecPhys => gic::ID_SEC_PHYS_TIMER,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg {
    Pct,
    Vct,
    Voff,
    Tval(Which),
    Ctl(Which),
    Cval(Which),
}

impl Reg {
    pub fn decode(op0: u32, op1: u32, crn: u32, crm: u32, op2: u32) -> Option<Reg> {
        if op0 != 3 || crn != 14 {
            return None;
        }
        let timer = |w| match op2 {
            0 => Some(Reg::Tval(w)),
            1 => Some(Reg::Ctl(w)),
            2 => Some(Reg::Cval(w)),
            _ => None,
        };
        match (op1, crm, op2) {
            (3, 0, 1) => Some(Reg::Pct),
            (3, 0, 2) => Some(Reg::Vct),
            (4, 0, 3) => Some(Reg::Voff),
            (3, 2, _) => timer(Which::Phys),
            (3, 3, _) => timer(Which::Virt),
            (4, 2, _) => timer(Which::Hyp),
            (7, 2, _) => timer(Which::SecPhys),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Timer {
    ctl: u64,
    cval: u64,
}

#[derive(Debug, Clone, Default)]
pub struct GenericTimer {
    hz: u64,
    /// The counter read `base_count` at cycle `base_cycles`; it has advanced
    /// linearly at `hz` since. Rebased whenever the rate changes, so the
    /// count never jumps.
    base_cycles: u64,
    base_count: u64,
    voff: u64,
    timers: [Timer; 4],
}

impl GenericTimer {
    pub fn new() -> GenericTimer {
        GenericTimer::default()
    }

    pub fn hz(&self) -> u64 {
        self.hz
    }

    pub fn set_hz(&mut self, cycles: u64, hz: u64) {
        if hz != self.hz {
            self.base_count = self.count(cycles);
            self.base_cycles = cycles;
            self.hz = hz;
        }
    }

    /// `CNTPCT` at ARM cycle `cycles` (not before the last rate change).
    pub fn count(&self, cycles: u64) -> u64 {
        let elapsed = u128::from(cycles.saturating_sub(self.base_cycles));
        let ticks = elapsed * u128::from(self.hz) / u128::from(ARM_HZ);
        self.base_count.wrapping_add(ticks as u64)
    }

    fn offset(&self, w: Which) -> u64 {
        if w == Which::Virt {
            self.voff
        } else {
            0
        }
    }

    fn timer(&self, w: Which) -> Timer {
        self.timers[w as usize]
    }

    fn met(&self, w: Which, cycles: u64) -> bool {
        let t = self.timer(w);
        t.ctl & CTL_ENABLE != 0 && self.count(cycles).wrapping_sub(self.offset(w)) >= t.cval
    }

    pub fn line(&self, w: Which, cycles: u64) -> bool {
        self.met(w, cycles) && self.timer(w).ctl & CTL_IMASK == 0
    }

    pub fn read(&self, reg: Reg, cycles: u64) -> u64 {
        let now = self.count(cycles);
        match reg {
            Reg::Pct => now,
            Reg::Vct => now.wrapping_sub(self.voff),
            Reg::Voff => self.voff,
            Reg::Ctl(w) => self.timer(w).ctl | if self.met(w, cycles) { CTL_ISTATUS } else { 0 },
            Reg::Cval(w) => self.timer(w).cval,
            Reg::Tval(w) => u64::from(
                self.timer(w)
                    .cval
                    .wrapping_sub(now.wrapping_sub(self.offset(w))) as u32,
            ),
        }
    }

    /// An `MSR`. Writes to the read-only counters are ignored (the hardware
    /// would make them UNDEFINED; nothing does them).
    pub fn write(&mut self, reg: Reg, cycles: u64, value: u64) {
        let now = self.count(cycles);
        match reg {
            Reg::Pct | Reg::Vct => {}
            Reg::Voff => self.voff = value,
            Reg::Ctl(w) => self.timers[w as usize].ctl = value & (CTL_ENABLE | CTL_IMASK),
            Reg::Cval(w) => self.timers[w as usize].cval = value,
            Reg::Tval(w) => {
                let base = now.wrapping_sub(self.offset(w));
                self.timers[w as usize].cval = base.wrapping_add(value as u32 as i32 as i64 as u64);
            }
        }
    }

    /// The earliest cycle at or after `cycles` at which an output that is
    /// low now goes high — the next thing that can interrupt the core —
    /// or `None` if nothing is armed to.
    pub fn next_event(&self, cycles: u64) -> Option<u64> {
        if self.hz == 0 {
            return None;
        }
        Which::ALL
            .iter()
            .filter(|&&w| {
                let t = self.timer(w);
                t.ctl & (CTL_ENABLE | CTL_IMASK) == CTL_ENABLE && !self.met(w, cycles)
            })
            .filter_map(|&w| {
                let target = self.timer(w).cval.checked_add(self.offset(w))?;
                let ticks = u128::from(target.checked_sub(self.base_count)?);
                let dc = (ticks * u128::from(ARM_HZ)).div_ceil(u128::from(self.hz));
                u64::try_from(dc).ok()?.checked_add(self.base_cycles)
            })
            .map(|c| c.max(cycles))
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ: u64 = 54_000_000;

    fn running() -> GenericTimer {
        let mut t = GenericTimer::new();
        t.set_hz(0, HZ);
        t
    }

    #[test]
    fn the_counter_is_a_function_of_cycles_at_54_mhz() {
        let t = running();
        assert_eq!(t.count(0), 0);
        assert_eq!(t.count(ARM_HZ), HZ, "one modelled second");
        assert_eq!(t.read(Reg::Pct, ARM_HZ / 1000), HZ / 1000);
    }

    #[test]
    fn a_stopped_counter_stands_still_and_a_rate_change_does_not_jump() {
        let mut t = GenericTimer::new();
        assert_eq!(t.count(1_000_000), 0);
        t.set_hz(1_000_000, HZ);
        assert_eq!(t.count(1_000_000), 0);
        assert_eq!(t.count(1_000_000 + ARM_HZ), HZ);
        t.set_hz(1_000_000 + ARM_HZ, 2 * HZ);
        assert_eq!(t.count(1_000_000 + 2 * ARM_HZ), 3 * HZ);
    }

    #[test]
    fn encodings_follow_the_arm_arm() {
        // mrs x0, cntpct_el0 = S3_3_C14_C0_1; CNTP_CTL_EL0 = S3_3_C14_C2_1.
        assert_eq!(Reg::decode(3, 3, 14, 0, 1), Some(Reg::Pct));
        assert_eq!(Reg::decode(3, 3, 14, 2, 1), Some(Reg::Ctl(Which::Phys)));
        assert_eq!(Reg::decode(3, 3, 14, 3, 0), Some(Reg::Tval(Which::Virt)));
        assert_eq!(Reg::decode(3, 4, 14, 2, 2), Some(Reg::Cval(Which::Hyp)));
        assert_eq!(Reg::decode(3, 4, 14, 0, 3), Some(Reg::Voff));
        assert_eq!(Reg::decode(3, 3, 14, 0, 0), None);
        assert_eq!(Reg::decode(3, 0, 14, 1, 0), None);
        assert_eq!(Reg::decode(3, 4, 14, 1, 0), None);
    }

    #[test]
    fn tval_arms_a_compare_and_the_line_follows_ctl() {
        let mut t = running();
        let c0 = 1_000_000;
        t.write(Reg::Tval(Which::Phys), c0, 540); // 10 µs
        t.write(Reg::Ctl(Which::Phys), c0, CTL_ENABLE);
        let cval = t.count(c0) + 540;
        assert_eq!(t.read(Reg::Cval(Which::Phys), c0), cval);
        assert_eq!(t.read(Reg::Tval(Which::Phys), c0), 540);
        assert!(!t.line(Which::Phys, c0));

        let due = t.next_event(c0).unwrap();
        assert!(t.count(due) >= cval && t.count(due - 1) < cval);
        assert!(t.line(Which::Phys, due));
        assert_eq!(t.read(Reg::Ctl(Which::Phys), due), CTL_ENABLE | CTL_ISTATUS);
        let later = due + 15_000;
        let over = (t.count(later) - cval) as i32;
        assert!(over > 0);
        assert_eq!(t.read(Reg::Tval(Which::Phys), later) as u32 as i32, -over);

        // IMASK drops the line but not ISTATUS; no further event is due.
        t.write(Reg::Ctl(Which::Phys), due, CTL_ENABLE | CTL_IMASK);
        assert!(!t.line(Which::Phys, due));
        assert_eq!(
            t.read(Reg::Ctl(Which::Phys), due) & CTL_ISTATUS,
            CTL_ISTATUS
        );
        assert_eq!(t.next_event(due), None);
    }

    #[test]
    fn the_virtual_timer_counts_from_cntvoff() {
        let mut t = running();
        t.write(Reg::Voff, 0, 1000);
        let c = ARM_HZ; // counter = 54e6
        assert_eq!(t.read(Reg::Vct, c), HZ - 1000);
        t.write(Reg::Cval(Which::Virt), c, HZ);
        t.write(Reg::Ctl(Which::Virt), c, CTL_ENABLE);
        assert!(!t.line(Which::Virt, c), "virtual count is 1000 short");
        let due = t.next_event(c).unwrap();
        assert_eq!(t.count(due), HZ + 1000);
        assert!(t.line(Which::Virt, due));
    }

    #[test]
    fn the_earliest_of_several_armed_timers_is_the_next_event() {
        let mut t = running();
        t.write(Reg::Cval(Which::Phys), 0, 5400);
        t.write(Reg::Ctl(Which::Phys), 0, CTL_ENABLE);
        t.write(Reg::Cval(Which::Hyp), 0, 540);
        t.write(Reg::Ctl(Which::Hyp), 0, CTL_ENABLE);
        t.write(Reg::Cval(Which::Virt), 0, 54);
        t.write(Reg::Ctl(Which::Virt), 0, CTL_ENABLE | CTL_IMASK);
        assert_eq!(t.count(t.next_event(0).unwrap()), 540);
    }
}
