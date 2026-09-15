//! Busy-wait parking (#53): see the module docs of `arm/mod.rs`, "Busy-wait
//! loops".

use crate::aarch64::{Abort, Cpu, Memory, Step};
use crate::bus::Width;
use crate::machine::Machine;
use crate::periph::gentimer::GenericTimer;

use super::{timer_reg, PERIPH, PERIPH_TO_BUS};

/// Backward jumps to one target that start a watch of the loop behind it.
const HOT: i32 = 32;
/// Where a target that failed a watch starts counting again.
const COOLDOWN: i32 = -4096;
/// The longest loop body parked, in instructions.
const MAX_PASS: usize = 64;
/// Complete passes watched before parking.
const PASSES: usize = 6;
/// A loop with fewer passes to go than this is left to run: the search for
/// its end costs about as much.
const MIN_SKIP: u64 = 32;
/// The furthest one park looks ahead, in passes. A longer wait parks again.
const MAX_SKIP: u64 = 1 << 36;

/// One data read a pass makes: physical address, size, and the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Read {
    pub addr: u64,
    pub size: u32,
    pub value: u64,
}

/// The state a pass may change: the general registers, the stack pointers
/// and the flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Regs {
    pub(super) x: [u64; 31],
    pub(super) sp: [u64; 4],
    pub(super) nzcv: u32,
}

impl Regs {
    pub(super) fn of(c: &Cpu) -> Regs {
        Regs {
            x: c.x,
            sp: c.sp_el,
            nzcv: c.nzcv,
        }
    }

    pub(super) fn put(&self, c: &mut Cpu) {
        c.x = self.x;
        c.sp_el = self.sp;
        c.nzcv = self.nzcv;
    }

    /// Every register moved on by `n` times its step in `delta`; the flags
    /// as they are.
    fn advanced(&self, delta: &Regs, n: u64) -> Regs {
        let mut r = *self;
        let regs = r.x.iter_mut().chain(r.sp.iter_mut());
        for (v, d) in regs.zip(delta.x.iter().chain(&delta.sp)) {
            *v = v.wrapping_add(d.wrapping_mul(n));
        }
        r
    }
}

/// The rest of the state a pass could touch, which a parked loop has to
/// leave as it is.
#[derive(PartialEq)]
pub(super) struct Fixed {
    daif: u32,
    el: u32,
    spsel: bool,
    fpcr: u32,
    fpsr: u32,
    v: [u128; 32],
}

impl Fixed {
    pub(super) fn of(c: &Cpu) -> Fixed {
        Fixed {
            daif: c.daif,
            el: c.el,
            spsel: c.spsel,
            fpcr: c.fpcr,
            fpsr: c.fpsr,
            v: c.v,
        }
    }
}

/// A loop being watched: the passes so far, which have to repeat.
struct Watch {
    head: u64,
    effects: u64,
    fixed: Fixed,
    /// The pass in progress.
    pcs: Vec<u64>,
    reads: Vec<Read>,
    /// The first complete pass.
    pass: Option<(Vec<u64>, Vec<Read>)>,
    /// The state at every arrival at the head, with the cycle the head
    /// instruction runs in.
    heads: Vec<(u64, Regs)>,
}

/// Per core: finds loops worth parking.
#[derive(Default)]
pub(super) struct Detector {
    /// Backward-jump targets and how often each was jumped to.
    hot: [(u64, i32); 16],
    watch: Option<Box<Watch>>,
    /// The reads of the step in flight, while watching.
    pub(super) log: Vec<Read>,
    /// Its stores, which only a SHA-256 recording wants.
    pub(super) stores: Vec<Read>,
    /// Looks for SHA-256 block loops (`sha.rs`), and the head of the one the
    /// core was last fitted to. `sha_stop` says the core has just jumped
    /// back to that head: the run loop has a skip to consider.
    pub(super) sha: super::sha::Finder,
    pub(super) sha_head: Option<u64>,
    pub(super) sha_stop: bool,
    /// Which of the two to look for (`RVF_NO_PARK`, `RVF_NO_SHA_SKIP`).
    pub(super) park_on: bool,
    pub(super) sha_on: bool,
}

fn slot(target: u64) -> usize {
    ((target >> 2) ^ (target >> 7)) as usize & 15
}

impl Detector {
    pub(super) fn watching(&self) -> bool {
        self.watch.is_some() || self.sha_stop || self.sha.recording()
    }

    /// The core took an exception or an interrupt, or waited: what was being
    /// watched was not one loop.
    pub(super) fn interrupted(&mut self) {
        self.watch = None;
        self.sha.abandon();
        self.sha_stop = false;
        self.log.clear();
        self.stores.clear();
    }

    /// The core retired the instruction at `pc` in `cycle`, and stored
    /// something if `wrote`. True when a watch has seen enough passes for
    /// [`Self::park`].
    #[inline]
    pub(super) fn retired(&mut self, cpu: &Cpu, cycle: u64, pc: u64, wrote: bool) -> bool {
        // A skip that did not happen: what the step logged goes nowhere.
        if self.sha_stop {
            self.sha_stop = false;
            self.log.clear();
            self.stores.clear();
        }
        if self.watch.is_some() {
            self.stores.clear();
            return self.watched(cpu, cycle, pc, wrote);
        }
        if self.sha.recording() {
            self.sha.record(cpu, pc, &mut self.log, &mut self.stores);
            return false;
        }
        if cpu.pc <= pc {
            self.backward(cpu, cycle);
        }
        false
    }

    fn backward(&mut self, cpu: &Cpu, cycle: u64) {
        let t = cpu.pc;
        if self.sha_head == Some(t) {
            self.sha_stop = true;
            return;
        }
        if self.sha_on && self.sha.backward(cpu, cycle) {
            return;
        }
        if !self.park_on {
            return;
        }
        let e = &mut self.hot[slot(t)];
        if e.0 != t {
            *e = (t, 0);
        }
        e.1 += 1;
        if e.1 < HOT {
            return;
        }
        e.1 = 0;
        self.watch = Some(Box::new(Watch {
            head: t,
            effects: cpu.effects,
            fixed: Fixed::of(cpu),
            pcs: Vec::new(),
            reads: Vec::new(),
            pass: None,
            heads: vec![(cycle + 1, Regs::of(cpu))],
        }));
    }

    fn watched(&mut self, cpu: &Cpu, cycle: u64, pc: u64, wrote: bool) -> bool {
        let Some(w) = self.watch.as_mut() else {
            return false;
        };
        w.pcs.push(pc);
        w.reads.append(&mut self.log);
        if wrote || cpu.effects != w.effects || w.pcs.len() > MAX_PASS {
            return self.fail();
        }
        if cpu.pc != w.head {
            return false;
        }
        let pcs = std::mem::take(&mut w.pcs);
        let reads = std::mem::take(&mut w.reads);
        let repeats = match &w.pass {
            None => true,
            Some((p, r)) => *p == pcs && *r == reads,
        };
        if !repeats || Fixed::of(cpu) != w.fixed {
            return self.fail();
        }
        w.pass.get_or_insert((pcs, reads));
        w.heads.push((cycle + 1, Regs::of(cpu)));
        w.heads.len() > PASSES
    }

    fn fail(&mut self) -> bool {
        if let Some(w) = self.watch.take() {
            self.hot[slot(w.head)] = (w.head, COOLDOWN);
        }
        false
    }

    /// Park the core the watch has been following, which is at the loop's
    /// head: `None` if the loop does not qualify, or ends too soon to be
    /// worth it. `cpu` is left as it was.
    pub(super) fn park(
        &mut self,
        cpu: &mut Cpu,
        m: &Machine,
        timer: &GenericTimer,
    ) -> Option<Box<Park>> {
        let w = self.watch.take()?;
        let head = w.head;
        let now = Regs::of(cpu);
        let park = Park::fit(*w, cpu, m, timer);
        now.put(cpu);
        cpu.pc = head;
        if park.is_none() {
            self.hot[slot(head)] = (head, COOLDOWN);
        }
        park
    }
}

/// A parked core's loop, and what it needs to rebuild the core's state at
/// any cycle before [`Self::until`].
pub(super) struct Park {
    /// One pass, from the head, and its reads: the loop's inputs.
    pcs: Vec<u64>,
    reads: Vec<Read>,
    /// The state at the head of pass 0, which runs from cycle `at`.
    base: Regs,
    at: u64,
    len: u64,
    /// What a register adds every pass; 0 for one that does not change, or
    /// that changes with the counter.
    delta: Regs,
    /// The cycle the pass that leaves the loop starts, or the end of what
    /// [`MAX_SKIP`] let the search look at.
    pub(super) until: u64,
}

impl Park {
    fn fit(w: Watch, cpu: &mut Cpu, m: &Machine, timer: &GenericTimer) -> Option<Box<Park>> {
        let (pcs, reads) = w.pass?;
        // Every input has to be something the run loop can watch without
        // reading it.
        if reads
            .iter()
            .any(|r| peek(m, r.addr, r.size) != Some(r.value))
        {
            return None;
        }
        let heads = &w.heads;
        let step = |f: &dyn Fn(&Regs) -> u64| {
            let d = f(&heads[1].1).wrapping_sub(f(&heads[0].1));
            let steady = heads
                .windows(2)
                .all(|h| f(&h[1].1).wrapping_sub(f(&h[0].1)) == d);
            if steady {
                d
            } else {
                0
            }
        };
        let mut delta = Regs {
            x: [0; 31],
            sp: [0; 4],
            nzcv: 0,
        };
        for i in 0..31 {
            delta.x[i] = step(&|r| r.x[i]);
        }
        for i in 0..4 {
            delta.sp[i] = step(&|r| r.sp[i]);
        }
        // A register that moves every pass has to be counting down to zero,
        // the way a timeout does: an end the search below can find. One
        // counting up is compared against a limit, often for equality, and a
        // halving search steps over the one pass that ends the loop (#53:
        // UEFI's bitmap scan at `0x383288a0`, `cmp w3, #8`).
        let newest = &heads.last()?.1;
        let counts_down = |(&d, &v): (&u64, &u64)| d == 0 || ((d as i64) < 0 && (v as i64) > 0);
        if !delta.x.iter().zip(&newest.x).all(counts_down) || delta.sp.iter().any(|&d| d != 0) {
            return None;
        }
        let len = pcs.len() as u64;
        let mut park = Park {
            pcs,
            reads,
            base: heads[0].1,
            at: heads[0].0,
            len,
            delta,
            until: 0,
        };
        // The rebuild has to reproduce every head watched, from the first
        // and from the second.
        for b in 0..2 {
            park.base = heads[b].1;
            park.at = heads[b].0;
            for (k, h) in heads.iter().enumerate().skip(b + 2) {
                let n = (k - b) as u64;
                let rebuilt = park.rebuild(cpu, m, timer, n).is_ok();
                if !rebuilt || cpu.pc != w.head || Regs::of(cpu) != h.1 {
                    return None;
                }
            }
        }
        let (at, last) = *heads.last()?;
        park.base = last;
        park.at = at;
        // The first passes one by one: a loop about to end is not worth
        // parking, and this finds its end whatever decides it.
        park.rebuild(cpu, m, timer, 0).ok()?;
        for j in 0..MIN_SKIP {
            park.pass(cpu, m, timer, j, park.pcs.len()).ok()?;
        }
        // Then the first pass that leaves the loop: the rebuilt state at its
        // head makes a pass that strays from the watched one, or moves a
        // countdown other than the rebuild does. Leaving is for good (the
        // countdown hits zero, the counter passes a deadline), so a search
        // doubling up to it and halving back down finds it.
        let hi = park.room().saturating_add(2).min(MAX_SKIP);
        let mut leaves = |n: u64| {
            let r = park
                .rebuild(cpu, m, timer, n)
                .and_then(|()| park.pass(cpu, m, timer, n, park.pcs.len()));
            r.is_err() || !park.on_course(cpu, n + 1)
        };
        if hi < MIN_SKIP || leaves(MIN_SKIP) {
            return None;
        }
        let (mut lo, mut end) = (MIN_SKIP, hi);
        let mut n = MIN_SKIP;
        while n < hi {
            n = n.saturating_mul(4).min(hi);
            if leaves(n) {
                end = n;
                break;
            }
            lo = n;
        }
        if lo == hi {
            end = hi;
        } else {
            while end - lo > 1 {
                let mid = lo + (end - lo) / 2;
                if leaves(mid) {
                    end = mid;
                } else {
                    lo = mid;
                }
            }
        }
        park.until = park.at + end * park.len;
        Some(Box::new(park))
    }

    /// The loop's instructions, one pass.
    pub(super) fn pcs(&self) -> &[u64] {
        &self.pcs
    }

    /// Has an input changed since the core parked?
    pub(super) fn inputs_changed(&self, m: &Machine) -> bool {
        self.reads
            .iter()
            .any(|r| peek(m, r.addr, r.size) != Some(r.value))
    }

    /// Put the core in the state it would have been in at cycle `t` (at most
    /// [`Self::until`]) had it run the loop, and return how many
    /// instructions that was.
    pub(super) fn resume(&self, cpu: &mut Cpu, m: &Machine, timer: &GenericTimer, t: u64) -> u64 {
        let d = t - self.at;
        let (n, p) = (d / self.len, (d % self.len) as usize);
        let r = self
            .rebuild(cpu, m, timer, n)
            .and_then(|()| self.pass(cpu, m, timer, n, p));
        if let Err(e) = r {
            panic!(
                "parked loop at {:#x} strayed rebuilding pass {n} + {p}: {e}\n{}",
                self.pcs[0],
                self.describe(m)
            );
        }
        d
    }

    /// Do the countdowns in `cpu` hold what moving them on to pass `n`
    /// gives?
    fn on_course(&self, cpu: &Cpu, n: u64) -> bool {
        let want = self.base.advanced(&self.delta, n);
        let regs = cpu.x.iter().zip(&want.x).zip(&self.delta.x);
        regs.filter(|(_, &d)| d != 0).all(|((v, w), _)| v == w)
    }

    /// The state at the head of pass `n`: every register moved on to pass
    /// `n - 2`, and the last two passes run, which puts back what the loop
    /// computes from the counter. An error if one of them strays.
    fn rebuild(
        &self,
        cpu: &mut Cpu,
        m: &Machine,
        timer: &GenericTimer,
        n: u64,
    ) -> Result<(), String> {
        let k = n.min(2);
        self.base.advanced(&self.delta, n - k).put(cpu);
        cpu.pc = self.pcs[0];
        (n - k..n).try_for_each(|j| {
            self.pass(cpu, m, timer, j, self.pcs.len())
                .map_err(|e| format!("pass {j}: {e}"))
        })
    }

    /// Run the first `steps` instructions of pass `n` on `cpu`, off the
    /// machine: the reads get the watched values, the counter reads the
    /// cycle each instruction runs in. An error if the pass strays, and for
    /// a whole pass if it does not end back at the head having made every
    /// read.
    fn pass(
        &self,
        cpu: &mut Cpu,
        m: &Machine,
        timer: &GenericTimer,
        n: u64,
        steps: usize,
    ) -> Result<(), String> {
        let start = self.at + n * self.len;
        let effects = cpu.effects;
        let mut bus = SpecBus {
            m,
            timer,
            cycle: start,
            reads: &self.reads,
            pos: 0,
            bad: false,
        };
        for (i, &pc) in self.pcs[..steps].iter().enumerate() {
            bus.cycle = start + i as u64;
            if cpu.pc != pc {
                return Err(format!("step {i} at {:#x}, not {pc:#x}", cpu.pc));
            }
            let s = cpu.step(&mut bus);
            if bus.bad || !matches!(s, Step::Retired) {
                let bad = if bus.bad {
                    ", an access it may not make"
                } else {
                    ""
                };
                return Err(format!("step {i} at {pc:#x}: {s:?}{bad}"));
            }
        }
        if cpu.effects != effects {
            return Err("changed state beyond the registers".into());
        }
        if steps == self.pcs.len() && (cpu.pc != self.pcs[0] || bus.pos != self.reads.len()) {
            return Err(format!(
                "ended at {:#x} after {} of {} reads",
                cpu.pc,
                bus.pos,
                self.reads.len()
            ));
        }
        Ok(())
    }

    /// The loop, for a report: its instructions (as RAM holds them at their
    /// virtual addresses), its reads, and the registers at pass 0.
    fn describe(&self, m: &Machine) -> String {
        let mut s = format!(
            "  {} instructions a pass from cycle {}, until {}\n",
            self.len, self.at, self.until
        );
        for &pc in &self.pcs {
            let insn = ram(m, pc, 4).map_or("?".into(), |w| format!("{w:08x}"));
            s += &format!("  {pc:#x}: {insn}\n");
        }
        for r in &self.reads {
            s += &format!("  read {:#x}/{} = {:#x}\n", r.addr, r.size, r.value);
        }
        for (i, (&v, &d)) in self.base.x.iter().zip(&self.delta.x).enumerate() {
            let step = if d != 0 {
                format!(" {:+} a pass", d as i64)
            } else {
                String::new()
            };
            s += &format!("  x{i} = {v:#x}{step}\n");
        }
        s += &format!("  nzcv {:#x}", self.base.nzcv);
        s
    }

    /// How many passes the base state can be moved on before a register that
    /// changes every pass reaches or crosses 0, 2^31, 2^32 or 2^63 — where a
    /// countdown ends or a counter wraps, and moving it on stops being what
    /// the loop does.
    fn room(&self) -> u64 {
        let base = self.base.x.iter().chain(&self.base.sp);
        let delta = self.delta.x.iter().chain(&self.delta.sp);
        base.zip(delta)
            .filter(|(_, &d)| d != 0)
            .map(|(&v, &d)| {
                let (up, step) = if (d as i64) > 0 {
                    (true, d)
                } else {
                    (false, d.wrapping_neg())
                };
                [0, 1 << 31, 1 << 32, 1 << 63]
                    .into_iter()
                    .map(|b: u64| {
                        let dist = if up {
                            b.wrapping_sub(v)
                        } else {
                            v.wrapping_sub(b)
                        };
                        match dist {
                            0 => u64::MAX,
                            _ => (dist - 1) / step,
                        }
                    })
                    .min()
                    .unwrap_or(u64::MAX)
            })
            .min()
            .unwrap_or(u64::MAX)
    }
}

/// A RAM read of at most 8 bytes, the way `ArmBus` routes one.
pub(super) fn ram(m: &Machine, addr: u64, size: u32) -> Option<u64> {
    if size > 8 || addr.checked_add(u64::from(size))? > m.ram.len() as u64 {
        return None;
    }
    let base = m.ram.base() + addr as u32;
    let load = |a: u32, s: u32| {
        let w = match s {
            1 => Width::Byte,
            2 => Width::Half,
            _ => Width::Word,
        };
        m.ram.load(a, w).ok().map(u64::from)
    };
    match size {
        8 => Some(load(base, 4)? | load(base + 4, 4)? << 32),
        _ => load(base, size),
    }
}

/// What a read would return, if the machine can tell without the read's
/// side effects.
fn peek(m: &Machine, addr: u64, size: u32) -> Option<u64> {
    if let Some(v) = ram(m, addr, size) {
        return Some(v);
    }
    if size == 4 && PERIPH.contains(&addr) {
        return m.peek((addr - PERIPH_TO_BUS) as u32).map(u64::from);
    }
    None
}

/// The machine as a rebuilt pass sees it: the watched inputs, RAM for table
/// walks, the counter at a given cycle, and no stores.
struct SpecBus<'a> {
    m: &'a Machine,
    timer: &'a GenericTimer,
    cycle: u64,
    reads: &'a [Read],
    /// The next read the pass makes.
    pos: usize,
    /// The pass did something a parked loop may not.
    bad: bool,
}

impl Memory for SpecBus<'_> {
    fn fetch(&mut self, addr: u64) -> Result<u32, Abort> {
        ram(self.m, addr, 4)
            .map(|v| v as u32)
            .ok_or(Abort { addr, write: false })
    }

    fn read(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
        if let Some(r) = self.reads.get(self.pos) {
            if r.addr == addr && r.size == size {
                self.pos += 1;
                return Ok(r.value);
            }
        }
        // A table walk after a TLB flush reads RAM the pass itself does not.
        ram(self.m, addr, size).ok_or_else(|| {
            self.bad = true;
            Abort { addr, write: false }
        })
    }

    fn write(&mut self, addr: u64, _size: u32, _value: u64) -> Result<(), Abort> {
        self.bad = true;
        Err(Abort { addr, write: true })
    }

    fn sysreg_read(&mut self, key: u32) -> Option<u64> {
        timer_reg(key).map(|r| self.timer.read(r, self.cycle))
    }

    fn sysreg_write(&mut self, _key: u32, _value: u64) -> bool {
        self.bad = true;
        false
    }
}
