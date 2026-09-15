//! SHA-256 block loops (#79): see the module docs of `arm/mod.rs`, "SHA-256
//! loops".

use std::collections::{HashMap, HashSet};

use crate::aarch64::{Abort, Cpu, Memory, Step};
use crate::bus::Width;
use crate::machine::Machine;

use super::park::{ram, Fixed, Read, Regs};

/// A pass shorter than this is a busy-wait candidate, not a block loop: one
/// SHA-256 block takes thousands of instructions.
const MIN_PASS: u64 = 256;
/// The longest pass recorded, in instructions.
const MAX_PASS: usize = 1 << 16;
/// Jumps back to one target, the same distance apart, before its loop is
/// recorded.
const REPEATS: u8 = 2;
/// A skip of fewer passes is not worth its probe.
const MIN_SKIP: u64 = 4;
/// What a probe XORs into the registers it takes to be dead.
const SCRAMBLE: u64 = 0xA5A5_5A5A_A5A5_5A5A;

/// SHA-256's round constants (FIPS 180-4 §4.2.2).
pub(super) const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256's compression function (FIPS 180-4 §6.2.2): `state` after one
/// 64-byte block.
pub(super) fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, c) in block.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_be_bytes(*c);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *s = s.wrapping_add(v);
    }
}

/// One memory access of a pass, in the order the pass made them, with the
/// pass's instruction that made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Access {
    addr: u64,
    size: u32,
    value: u64,
    store: bool,
    step: usize,
}

/// A loop being recorded: its passes from `head` to `head`, which have to
/// repeat instruction for instruction. `entry` is the backward-jump target
/// the loop was found by; the head is that, or a point in the pass further
/// on (see [`Fitted::Rotate`]), which the core reaches first.
struct Recording {
    head: u64,
    entry: u64,
    started: bool,
    rotated: bool,
    /// Instructions seen before reaching the head.
    waited: usize,
    effects: u64,
    fixed: Fixed,
    /// The pass in progress.
    pcs: Vec<u64>,
    acc: Vec<Access>,
    /// The flags after each instruction.
    flags: Vec<u32>,
    /// The passes done, and the state at every arrival at the head.
    passes: Vec<(Vec<u64>, Vec<Access>, Vec<u32>)>,
    heads: Vec<Regs>,
}

/// Per core: finds SHA-256 block loops.
#[derive(Default)]
pub(super) struct Finder {
    /// The last backward jump, as `(target, cycle)`: an inner loop going
    /// round jumps back to the same place again and again, quickly.
    last: (u64, u64),
    /// The backward-jump targets seen lately: `(target, cycle of the last
    /// jump there, the distance to the one before, how often in a row)`. A
    /// count of `u8::MAX` is a loop already turned down. The least lately
    /// jumped to makes room for a new one.
    far: [(u64, u64, u64, u8); 8],
    rec: Option<Box<Recording>>,
}

impl Finder {
    pub(super) fn recording(&self) -> bool {
        self.rec.is_some()
    }

    /// The core took an exception or an interrupt, or waited.
    pub(super) fn abandon(&mut self) {
        self.rec = None;
    }

    /// The core jumped back to `cpu.pc` in `cycle`. True when that starts a
    /// recording: the third pass in a row of one length, long enough not to
    /// be a busy-wait.
    pub(super) fn backward(&mut self, cpu: &Cpu, cycle: u64) -> bool {
        let t = cpu.pc;
        let (last, at) = std::mem::replace(&mut self.last, (t, cycle));
        if t == last && cycle.wrapping_sub(at) < MIN_PASS {
            return false;
        }
        let Some(e) = self.far.iter_mut().find(|e| e.0 == t) else {
            if let Some(e) = self.far.iter_mut().min_by_key(|e| e.1) {
                *e = (t, cycle, 0, 0);
            }
            return false;
        };
        if e.3 == u8::MAX {
            e.1 = cycle;
            return false;
        }
        let d = cycle.wrapping_sub(e.1);
        e.1 = cycle;
        if !(MIN_PASS..=MAX_PASS as u64).contains(&d) {
            (e.2, e.3) = (0, 0);
            return false;
        }
        if d == e.2 {
            e.3 += 1;
        } else {
            (e.2, e.3) = (d, 0);
        }
        if e.3 < REPEATS {
            return false;
        }
        e.3 = 0;
        self.rec = Some(Box::new(Recording {
            head: t,
            entry: t,
            started: true,
            rotated: false,
            waited: 0,
            effects: cpu.effects,
            fixed: Fixed::of(cpu),
            pcs: Vec::new(),
            acc: Vec::new(),
            flags: Vec::new(),
            passes: Vec::new(),
            heads: vec![Regs::of(cpu)],
        }));
        true
    }

    /// Turn the loop found at `entry` down for as long as it keeps its slot.
    fn reject(&mut self, entry: u64) {
        if let Some(e) = self.far.iter_mut().find(|e| e.0 == entry) {
            e.3 = u8::MAX;
        }
        self.rec = None;
    }

    /// The instruction at `pc` retired while recording, with the reads and
    /// the stores it made.
    pub(super) fn record(
        &mut self,
        cpu: &Cpu,
        pc: u64,
        reads: &mut Vec<Read>,
        stores: &mut Vec<Read>,
    ) {
        let Some(r) = self.rec.as_mut() else {
            return;
        };
        let entry = r.entry;
        if !r.started {
            reads.clear();
            stores.clear();
            r.waited += 1;
            if r.waited > MAX_PASS {
                return self.reject(entry);
            }
            if cpu.pc == r.head {
                r.started = true;
                r.effects = cpu.effects;
                r.fixed = Fixed::of(cpu);
                r.heads.push(Regs::of(cpu));
            }
            return;
        }
        let step = r.pcs.len();
        r.pcs.push(pc);
        let access = |store| {
            move |x: Read| Access {
                addr: x.addr,
                size: x.size,
                value: x.value,
                store,
                step,
            }
        };
        r.acc.extend(reads.drain(..).map(access(false)));
        r.acc.extend(stores.drain(..).map(access(true)));
        r.flags.push(cpu.nzcv);
        if cpu.effects != r.effects || r.pcs.len() > MAX_PASS {
            return self.reject(entry);
        }
        if cpu.pc != r.head {
            return;
        }
        if Fixed::of(cpu) != r.fixed {
            return self.reject(entry);
        }
        let pass = (
            std::mem::take(&mut r.pcs),
            std::mem::take(&mut r.acc),
            std::mem::take(&mut r.flags),
        );
        if r.passes.first().is_some_and(|(p, _, _)| *p != pass.0) {
            return self.reject(entry);
        }
        r.passes.push(pass);
        r.heads.push(Regs::of(cpu));
    }

    /// Two passes recorded: time for [`Self::fit`].
    pub(super) fn ready(&self) -> bool {
        self.rec.as_ref().is_some_and(|r| r.passes.len() == 2)
    }

    /// The loop the recording shows, if it is a SHA-256 block loop; the core
    /// is at the recording's head. A loop that has to be looked at from
    /// another point of its pass is recorded again from there.
    pub(super) fn fit(&mut self, cpu: &Cpu, m: &Machine) -> Option<Box<Loop>> {
        let r = self.rec.take()?;
        match Loop::fit(&r, cpu, m) {
            Fitted::Loop(l) => Some(l),
            Fitted::Rotate(head) if !r.rotated => {
                self.rec = Some(Box::new(Recording {
                    head,
                    entry: r.entry,
                    started: false,
                    rotated: true,
                    waited: 0,
                    effects: cpu.effects,
                    fixed: Fixed::of(cpu),
                    pcs: Vec::new(),
                    acc: Vec::new(),
                    flags: Vec::new(),
                    passes: Vec::new(),
                    heads: Vec::new(),
                }));
                None
            }
            _ => {
                self.reject(r.entry);
                None
            }
        }
    }
}

/// What two recorded passes turned out to be.
enum Fitted {
    Loop(Box<Loop>),
    /// A block loop whose passes each store the hash of the block the pass
    /// before read: the pass from here straddles two blocks. Seen from just
    /// after it stores the state, it is a plain one.
    Rotate(u64),
    No,
}

/// What a register does from one head to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// Holds its value.
    Fixed,
    /// Adds the same amount.
    Step(u64),
    /// Holds this word of the hash state.
    Word(usize),
    /// Written before it is read: whatever it held does not matter.
    Dead,
}

/// A SHA-256 block loop: its head, one pass of it, and how the core's state
/// at the head relates to the blocks it has hashed.
pub(super) struct Loop {
    /// The backward-jump target the run loop stops the core at, and the
    /// instructions from there to the head.
    pub(super) entry: u64,
    prefix: Vec<u64>,
    head: u64,
    pcs: Vec<u64>,
    /// The flags after each instruction of a pass, where they were the same
    /// in both passes recorded. A loop ends on a comparison of something
    /// that moves a pass at a time, often for equality; the flags that
    /// comparison sets differ on the far side of the end, so a pass beyond
    /// it does not pass for one of the loop's.
    flags: Vec<Option<u32>>,
    fixed: Fixed,
    /// The state at the head the loop was fitted at.
    base: Regs,
    roles: [Role; 31],
    nzcv_dead: bool,
    /// The register that steps a block at a time, and where the block a
    /// pass hashes is from it (virtual addresses).
    ptr: usize,
    block_off: u64,
    /// Where each pass stores the state (physical), and whether the next
    /// pass reads it back from there.
    state_pa: u64,
    reads_state: bool,
}

impl Loop {
    /// Fit a loop to two passes of one: find the 32 bytes a pass stores
    /// that are the compression of what the pass before stored with 64
    /// bytes a pass reads, work out what every register does, and check it
    /// all by running the next pass off the machine, twice.
    fn fit(r: &Recording, cpu: &Cpu, m: &Machine) -> Fitted {
        Self::try_fit(r, cpu, m).unwrap_or(Fitted::No)
    }

    fn try_fit(r: &Recording, cpu: &Cpu, m: &Machine) -> Option<Fitted> {
        let [(pcs, a, flags_a), (_, b, flags_b)] = &r.passes[..] else {
            return None;
        };
        let [h1, h2, h3] = r.heads[..] else {
            return None;
        };
        if a.iter().chain(b).any(|x| ram(m, x.addr, x.size).is_none()) {
            return None;
        }
        let places = |v: &[Access]| -> Vec<(u64, u32)> {
            v.iter()
                .filter(|x| x.store)
                .map(|x| (x.addr, x.size))
                .collect()
        };
        if places(a) != places(b) {
            return None;
        }
        let (stored_a, ext_a) = bytes(a)?;
        let (stored_b, ext_b) = bytes(b)?;
        // The state the second pass stored, and the block that took it
        // there from what the first pass stored: read by the second pass
        // itself (and the block before it by the first), or already by the
        // first (and the next one by the second).
        let mut found = None;
        'search: for s in windows(&stored_b, 32) {
            let (Some(old), Some(new)) = (words(&stored_a, s), words(&stored_b, s)) else {
                continue;
            };
            for (lag, from, other, next) in [(0, &ext_b, &ext_a, -64i64), (1, &ext_a, &ext_b, 64)] {
                for blk in windows(from, 64) {
                    let o = blk.wrapping_add(next as u64);
                    if !(0..64).all(|i| other.contains_key(&(o + i))) {
                        continue;
                    }
                    let mut st = old;
                    compress(&mut st, &block_of(from, blk)?);
                    if st == new {
                        found = Some((s, lag, blk, old, new));
                        break 'search;
                    }
                }
            }
        }
        let (state_pa, lag, block_pa, old, new) = found?;
        let in_state = |x: u64| in_range(x, state_pa, 32);
        if lag == 1 {
            let last = b
                .iter()
                .filter(|x| x.store && (0..u64::from(x.size)).any(|i| in_state(x.addr + i)))
                .map(|x| x.step)
                .max()?;
            let head = *pcs.get(last + 1)?;
            return (pcs.iter().filter(|&&p| p == head).count() == 1)
                .then_some(Fitted::Rotate(head));
        }
        // Memory a pass reads before it writes it has to be the state:
        // anything else carried from pass to pass in memory is not modelled.
        if ext_b
            .keys()
            .any(|&x| stored_b.contains_key(&x) && !in_state(x))
        {
            return None;
        }
        let reads_state = ext_b.keys().any(|&x| in_state(x));
        if h1.sp != h2.sp || h2.sp != h3.sp {
            return None;
        }
        let mut roles = [Role::Dead; 31];
        for (i, role) in roles.iter_mut().enumerate() {
            let (v1, v2, v3) = (h1.x[i], h2.x[i], h3.x[i]);
            *role = if v1 == v2 && v2 == v3 {
                Role::Fixed
            } else if v2.wrapping_sub(v1) == v3.wrapping_sub(v2) {
                Role::Step(v3.wrapping_sub(v2))
            } else if let Some(j) =
                (0..8).find(|&j| v3 == u64::from(new[j]) && v2 == u64::from(old[j]))
            {
                Role::Word(j)
            } else {
                Role::Dead
            };
        }
        // Without the state in memory, it has to be in the registers.
        if !reads_state && (0..8).any(|j| !roles.contains(&Role::Word(j))) {
            return None;
        }
        let ptr = roles.iter().position(|&r| r == Role::Step(64))?;
        // The block the second pass hashed, relative to the pointer at its
        // head.
        let mut c = cpu.clone();
        let ptr_pa = data_pa(&mut c, m, h2.x[ptr])?;
        let block_off = block_pa.wrapping_sub(ptr_pa);
        if block_off.wrapping_add(0x1000) >= 0x2000
            || data_pa(&mut c, m, h2.x[ptr].wrapping_add(block_off)) != Some(block_pa)
        {
            return None;
        }
        let prefix = if r.entry == r.head {
            Vec::new()
        } else {
            let at = pcs.iter().position(|&p| p == r.entry)?;
            if pcs.iter().filter(|&&p| p == r.entry).count() != 1 {
                return None;
            }
            pcs[at..].to_vec()
        };
        let l = Loop {
            entry: r.entry,
            prefix,
            head: r.head,
            pcs: pcs.clone(),
            flags: flags_a
                .iter()
                .zip(flags_b)
                .map(|(x, y)| (x == y).then_some(*y))
                .collect(),
            fixed: Fixed::of(cpu),
            base: h3,
            roles,
            nzcv_dead: !(h1.nzcv == h2.nzcv && h2.nzcv == h3.nzcv),
            ptr,
            block_off,
            state_pa,
            reads_state,
        };
        // The next pass, from where the core is, twice: the second time with
        // every register taken to be dead scrambled. Both have to follow the
        // recorded pass back to the head, end the same, and end where
        // hashing the block says.
        let block = l.block(&mut c, m, h3.x[ptr].wrapping_add(block_off))?;
        let mut next = new;
        compress(&mut next, &block);
        let (regs, stores) = l.probe(cpu, m, &h3, &new, false)?;
        if l.probe(cpu, m, &h3, &new, true)? != (regs, stores.clone()) {
            return None;
        }
        if !l.ends(&regs, &stores, &h3, 1, &next) {
            return None;
        }
        Some(Fitted::Loop(Box::new(l)))
    }

    /// Instructions a pass.
    pub(super) fn len(&self) -> u64 {
        self.pcs.len() as u64
    }

    /// The 64-byte block at virtual `va`, as the core's MMU maps it.
    fn block(&self, c: &mut Cpu, m: &Machine, va: u64) -> Option<[u8; 64]> {
        let mut out = [0u8; 64];
        let mut i = 0;
        while i < 64 {
            let a = va.wrapping_add(i as u64);
            let pa = data_pa(c, m, a)?;
            let n = ((0x1000 - (a & 0xFFF)) as usize).min(64 - i);
            for k in 0..n {
                out[i + k] = ram(m, pa + k as u64, 1)? as u8;
            }
            i += n;
        }
        Some(out)
    }

    /// Run one pass off the machine from the head, in `from` with `state`
    /// in memory (and in the registers that hold it), and dead registers
    /// scrambled if asked: the registers and the stores it ends with.
    fn probe(
        &self,
        cpu: &Cpu,
        m: &Machine,
        from: &Regs,
        state: &[u32; 8],
        scramble: bool,
    ) -> Option<(Regs, Vec<(u64, u8)>)> {
        let mut regs = *from;
        for (i, role) in self.roles.iter().enumerate() {
            match role {
                Role::Word(j) => regs.x[i] = u64::from(state[*j]),
                Role::Dead if scramble => regs.x[i] ^= SCRAMBLE,
                _ => {}
            }
        }
        if scramble && self.nzcv_dead {
            regs.nzcv ^= 0xF000_0000;
        }
        let mut c = cpu.clone();
        regs.put(&mut c);
        c.pc = self.head;
        let mut bus = ProbeBus::new(m);
        for (i, w) in state.iter().enumerate() {
            for (k, b) in w.to_le_bytes().into_iter().enumerate() {
                bus.over.insert(self.state_pa + 4 * i as u64 + k as u64, b);
            }
        }
        let preset = bus.over.clone();
        if !run(&mut c, &mut bus, &self.pcs, Some(&self.flags)) || c.pc != self.head {
            return None;
        }
        let mut stores: Vec<(u64, u8)> = bus
            .over
            .into_iter()
            .filter(|(a, b)| preset.get(a) != Some(b) || in_range(*a, self.state_pa, 32))
            .collect();
        stores.sort_unstable();
        Some((Regs::of(&c), stores))
    }

    /// Does a probe that ended in `regs` with `stores` end where `n` passes
    /// from `from` do, `state` being the hash after them?
    fn ends(
        &self,
        regs: &Regs,
        stores: &[(u64, u8)],
        from: &Regs,
        n: u64,
        state: &[u32; 8],
    ) -> bool {
        let regs_ok = self.roles.iter().enumerate().all(|(i, role)| match role {
            Role::Fixed => regs.x[i] == from.x[i],
            Role::Step(s) => regs.x[i] == from.x[i].wrapping_add(s.wrapping_mul(n)),
            Role::Word(j) => regs.x[i] == u64::from(state[*j]),
            Role::Dead => true,
        });
        let mut want = [0u8; 32];
        for (i, w) in state.iter().enumerate() {
            want[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
        }
        let got: Vec<u8> = stores
            .iter()
            .filter(|(a, _)| in_range(*a, self.state_pa, 32))
            .map(|&(_, b)| b)
            .collect();
        regs_ok && got == want
    }

    /// The core is at the loop's entry with `cycles` to go before anything
    /// else can happen. Run it on to the head, hash all but the last of the
    /// passes the rest has room for natively, and put the core where the
    /// loop would be, leaving one pass to run: that pass makes every
    /// register and every byte of the loop's scratch memory what the loop
    /// itself would have left. `Ok((instructions, blocks))` gone by, or
    /// `Err` if the core is not in the loop that was fitted any more.
    pub(super) fn skip(
        &self,
        cpu: &mut Cpu,
        m: &mut Machine,
        cycles: u64,
    ) -> Result<(u64, u64), ()> {
        let len = self.len();
        let mut gone = 0;
        if cpu.pc != self.head {
            let pre = self.prefix.len() as u64;
            if cpu.pc != self.entry || cycles < pre + (MIN_SKIP + 1) * len {
                return Ok((0, 0));
            }
            // Only RAM, so running it off the machine and keeping what it
            // did is running it.
            let mut c = cpu.clone();
            let mut bus = ProbeBus::new(m);
            if !run(&mut c, &mut bus, &self.prefix, None) || c.pc != self.head {
                return Ok((0, 0));
            }
            let over = std::mem::take(&mut bus.over);
            let base = m.ram.base();
            for (a, b) in over {
                m.ram
                    .store(base + a as u32, Width::Byte, u32::from(b))
                    .map_err(|_| ())?;
            }
            *cpu = c;
            gone = pre;
        }
        let now = Regs::of(cpu);
        if Fixed::of(cpu) != self.fixed || now.sp != self.base.sp {
            return Err(());
        }
        let d = now.x[self.ptr].wrapping_sub(self.base.x[self.ptr]);
        if !d.is_multiple_of(64) {
            return Err(());
        }
        let j = d / 64;
        for (i, role) in self.roles.iter().enumerate() {
            let ok = match role {
                Role::Fixed => now.x[i] == self.base.x[i],
                Role::Step(s) => now.x[i] == self.base.x[i].wrapping_add(s.wrapping_mul(j)),
                _ => true,
            };
            if !ok {
                return Err(());
            }
        }
        if !self.nzcv_dead && now.nzcv != self.base.nzcv {
            return Err(());
        }
        let most = ((cycles - gone) / len).saturating_sub(1);
        if most < MIN_SKIP {
            return Ok((gone, 0));
        }
        let mut state = [0u32; 8];
        for (i, w) in state.iter_mut().enumerate() {
            *w = if self.reads_state {
                ram(m, self.state_pa + 4 * i as u64, 4).ok_or(())? as u32
            } else {
                let r = self
                    .roles
                    .iter()
                    .position(|&r| r == Role::Word(i))
                    .ok_or(())?;
                now.x[r] as u32
            };
        }
        // Does the loop make pass `n` from here, the way the recorded ones
        // went? `states` is the hash after each pass, hashed ahead as far as
        // asked. Leaving the loop is for good (what moves a pass at a time
        // reaches the end), so if it makes pass `n` it makes every one
        // before it: a doubling search and a halving one find the last
        // within reach, hashing no further than that. The pass left to run
        // after a skip has to be a whole one of the loop's, or it would not
        // put back the registers the loop only uses inside its body.
        let mut c = cpu.clone();
        let mut states = vec![state];
        let base = now.x[self.ptr].wrapping_add(self.block_off);
        let mut makes = |n: u64| {
            while states.len() as u64 <= n + 1 {
                let i = states.len() as u64 - 1;
                let Some(block) = self.block(&mut c, m, base.wrapping_add(64 * i)) else {
                    return false;
                };
                let mut s = states[i as usize];
                compress(&mut s, &block);
                states.push(s);
            }
            let mut from = now;
            for (i, role) in self.roles.iter().enumerate() {
                if let Role::Step(s) = role {
                    from.x[i] = now.x[i].wrapping_add(s.wrapping_mul(n));
                }
            }
            let (st, next) = (states[n as usize], states[n as usize + 1]);
            self.probe(cpu, m, &from, &st, false)
                .is_some_and(|(r, s)| self.ends(&r, &s, &from, 1, &next))
        };
        let (mut lo, mut hi) = (0, most + 1);
        let mut n = MIN_SKIP;
        while n <= most {
            if !makes(n) {
                hi = n;
                break;
            }
            lo = n;
            n *= 2;
        }
        if hi == most + 1 && lo < most && makes(most) {
            lo = most;
        }
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if makes(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let k = lo;
        if k < MIN_SKIP {
            return Ok((gone, 0));
        }
        let state = states[k as usize];
        let mut regs = now;
        for (i, role) in self.roles.iter().enumerate() {
            match role {
                Role::Step(s) => regs.x[i] = now.x[i].wrapping_add(s.wrapping_mul(k)),
                Role::Word(w) => regs.x[i] = u64::from(state[*w]),
                _ => {}
            }
        }
        regs.put(cpu);
        let base = m.ram.base();
        for (i, w) in state.iter().enumerate() {
            let a = base + (self.state_pa + 4 * i as u64) as u32;
            m.ram.store(a, Width::Word, *w).map_err(|_| ())?;
        }
        Ok((gone + k * len, k))
    }
}

/// Step `c` through `pcs` on `bus`: false if it strays from them, sets
/// other `flags` than those given, or does anything a block loop may not.
fn run(c: &mut Cpu, bus: &mut ProbeBus, pcs: &[u64], flags: Option<&[Option<u32>]>) -> bool {
    for (i, &pc) in pcs.iter().enumerate() {
        if c.pc != pc {
            return false;
        }
        let s = c.step(bus);
        if bus.bad || !matches!(s, Step::Retired) {
            return false;
        }
        if let Some(Some(f)) = flags.map(|f| f[i]) {
            if c.nzcv != f {
                return false;
            }
        }
    }
    true
}

fn in_range(a: u64, lo: u64, len: u64) -> bool {
    a.wrapping_sub(lo) < len
}

/// Where a data read of `va` goes on this core, if it can go anywhere.
fn data_pa(c: &mut Cpu, m: &Machine, va: u64) -> Option<u64> {
    c.data_pa(&mut ProbeBus::new(m), va)
}

/// A pass's accesses as bytes: the last value stored at each byte, and the
/// first value read at each byte the pass had not stored to yet. `None` if a
/// read covers bytes of both kinds.
#[allow(clippy::type_complexity)]
fn bytes(acc: &[Access]) -> Option<(HashMap<u64, u8>, HashMap<u64, u8>)> {
    let (mut stored, mut ext) = (HashMap::new(), HashMap::new());
    let mut seen = HashSet::new();
    for x in acc {
        let each = (0..u64::from(x.size)).map(|i| (x.addr + i, (x.value >> (8 * i)) as u8));
        if x.store {
            for (a, b) in each {
                seen.insert(a);
                stored.insert(a, b);
            }
        } else {
            let inside = (0..u64::from(x.size))
                .filter(|i| seen.contains(&(x.addr + i)))
                .count();
            match inside {
                0 => {
                    for (a, b) in each {
                        ext.entry(a).or_insert(b);
                    }
                }
                n if n == x.size as usize => {}
                _ => return None,
            }
        }
    }
    Some((stored, ext))
}

/// Every 4-byte aligned address `len` bytes of `map` start at.
fn windows(map: &HashMap<u64, u8>, len: u64) -> Vec<u64> {
    let mut starts: Vec<u64> = map
        .keys()
        .copied()
        .filter(|&a| a % 4 == 0 && (0..len).all(|i| map.contains_key(&(a + i))))
        .collect();
    starts.sort_unstable();
    starts
}

/// The eight little-endian words at `a`.
fn words(map: &HashMap<u64, u8>, a: u64) -> Option<[u32; 8]> {
    let mut w = [0u32; 8];
    for (i, v) in w.iter_mut().enumerate() {
        let mut b = [0u8; 4];
        for (k, x) in b.iter_mut().enumerate() {
            *x = *map.get(&(a + 4 * i as u64 + k as u64))?;
        }
        *v = u32::from_le_bytes(b);
    }
    Some(w)
}

fn block_of(map: &HashMap<u64, u8>, a: u64) -> Option<[u8; 64]> {
    let mut out = [0u8; 64];
    for (i, x) in out.iter_mut().enumerate() {
        *x = *map.get(&(a + i as u64))?;
    }
    Some(out)
}

/// The machine as a probed pass sees it: RAM, with the pass's own stores
/// kept to one side, and nothing else.
struct ProbeBus<'a> {
    m: &'a Machine,
    over: HashMap<u64, u8>,
    /// The pass did something a block loop may not.
    bad: bool,
}

impl<'a> ProbeBus<'a> {
    fn new(m: &'a Machine) -> Self {
        ProbeBus {
            m,
            over: HashMap::new(),
            bad: false,
        }
    }
}

impl Memory for ProbeBus<'_> {
    fn fetch(&mut self, addr: u64) -> Result<u32, Abort> {
        ram(self.m, addr, 4)
            .map(|v| v as u32)
            .ok_or(Abort { addr, write: false })
    }

    fn read(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
        let mut v = 0;
        for i in 0..u64::from(size) {
            let a = addr + i;
            let b = match self.over.get(&a) {
                Some(&b) => b,
                None => match ram(self.m, a, 1) {
                    Some(b) => b as u8,
                    None => {
                        self.bad = true;
                        return Err(Abort { addr, write: false });
                    }
                },
            };
            v |= u64::from(b) << (8 * i);
        }
        Ok(v)
    }

    fn write(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
        if ram(self.m, addr, size).is_none() {
            self.bad = true;
            return Err(Abort { addr, write: true });
        }
        for i in 0..u64::from(size) {
            self.over.insert(addr + i, (value >> (8 * i)) as u8);
        }
        Ok(())
    }

    fn sysreg_read(&mut self, _key: u32) -> Option<u64> {
        self.bad = true;
        None
    }

    fn sysreg_write(&mut self, _key: u32, _value: u64) -> bool {
        self.bad = true;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compresses_abc_the_way_fips_180_says() {
        let mut block = [0u8; 64];
        block[..4].copy_from_slice(b"abc\x80");
        block[63] = 24;
        let mut state = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        compress(&mut state, &block);
        assert_eq!(
            state,
            [
                0xba7816bf, 0x8f01cfea, 0x414140de, 0x5dae2223, 0xb00361a3, 0x96177a9c, 0xb410ff61,
                0xf20015ad
            ]
        );
    }
}
