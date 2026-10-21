//! The ARM cores on threads of their own, for a person who wants to get to a
//! Linux shell sooner and does not need the run to repeat.
//!
//! The machine stays on one thread, the one that runs the VPU. Each core gets
//! a host thread that owns its [`Core`] and talks to the rest of the machine
//! in two ways:
//!
//! - **DRAM** is read and written in place, with atomic operations
//!   ([`SharedRam`]). A store-exclusive becomes a compare-exchange against the
//!   value the exclusive load read, `dmb`/`dsb` become a full fence, and every
//!   other access is acquire/release, which is no weaker than the barriers the
//!   guest asks for.
//! - **Everything else** — the peripheral window, the GIC, the ARM local block,
//!   PCIe — is a request to the machine's thread. A core posts it in its own
//!   [`Slot`] and waits; the machine thread answers between two VPU steps. None
//!   of the device models has to be `Send`, and none of them is touched by two
//!   threads.
//!
//! ## Time
//!
//! Without lock-step, the cores and the VPU keep to each other by a window. A
//! core never runs more than [`WINDOW`] cycles past the VPU's clock, and the VPU
//! waits when it gets more than that ahead of the slowest core that is awake.
//! A core's own clock is the instructions it has retired, so a core that waits
//! for the machine thread (or for the host to schedule it) does not lose guest
//! time: what depends on the host is only the order other cores' stores arrive
//! in. A core asleep in `wfi` or `wfe` sits out its time and catches up to the
//! VPU's clock when it wakes.
//!
//! While the VPU sleeps for a long stretch (`sleep`, a `udelay` fast-forward),
//! [`ParHost::run_until_store`] lets the cores run up to its wake-up and ends
//! the wait at the first peripheral store, as the lock-step run does.
//!
//! ## What is given up
//!
//! The run is not repeatable: how two cores' stores interleave depends on the
//! host. The shortcuts that need one thread to see everything — parking busy
//! waits, skipping SHA-256 blocks, straight-line runs — are off, so a poll on
//! a device costs a round trip each time. The exclusive monitor sees a store
//! that puts the same value back as no store at all, the way a compare-exchange
//! does. The VPU's decoded-instruction cache is not told about the cores'
//! stores; the cores do not write the VPU's code.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::Thread;
use std::time::Duration;

use super::{
    device_read, device_write, route, spi_levels, spi_stamp, wfe_timeout, ArmStop, Core, Target,
    CORES, SPIS, STUB_END,
};
use crate::aarch64::predecode::{Decoded, RunBus, RunOp};
use crate::aarch64::{Abort, Cpu, Memory, Step};
use crate::bus::Width;
use crate::machine::Machine;
use crate::mem::SharedRam;
use crate::periph::gentimer::{self, Which};
use crate::periph::gic;

/// How far apart the cores' clocks and the VPU's may drift, in cycles of the
/// ARM clock: about 67 µs of guest time.
pub(super) const WINDOW: u64 = 100_000;

/// The VPU raises the cores' limit by this much at a time, so an idle stretch
/// does not wake every sleeper on every step.
const CHUNK: u64 = WINDOW / 4;

/// After this many identical device reads in a row the core is taken to be
/// polling, and each further one lets [`POLL_SKIP`] cycles of guest time pass.
const POLL_AFTER: u32 = 128;
const POLL_SKIP: u64 = 1_500;

/// A core looks at the shared state once per this many instructions.
const POLL_MASK: u64 = 63;

/// VPU steps between two looks at the shared clock.
const SYNC_STEPS: u32 = 64;

/// How long a core with nothing to run sleeps before it looks again.
const NAP: Duration = Duration::from_micros(100);

const FREE: u32 = 0;
const ASKED: u32 = 1;
const ANSWERED: u32 = 2;

const READ: u32 = 0;
const WRITE: u32 = 1;
const PPI: u32 = 2;

/// One core's request to the machine's thread, and the answer. A core fills it
/// in and sets `state` last; the machine thread does the same going back.
#[repr(align(128))]
#[derive(Default)]
struct Slot {
    state: AtomicU32,
    op: AtomicU32,
    size: AtomicU32,
    secure: AtomicBool,
    ok: AtomicBool,
    addr: AtomicU64,
    value: AtomicU64,
    cycles: AtomicU64,
}

/// A value alone on its cache line, so a thread that writes it often does not
/// make the neighbours' readers miss.
#[repr(align(128))]
#[derive(Default)]
struct Line<T>(T);

impl<T> std::ops::Deref for Line<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// What one core's thread publishes and the others' threads leave for it.
#[repr(align(128))]
struct PerCore {
    /// Bit 0 IRQ, bit 1 FIQ, as the GIC drives the core's inputs.
    lines: AtomicU8,
    line_gen: AtomicU64,
    events: AtomicBool,
    cycles: AtomicU64,
    running: AtomicBool,
    /// Asleep, and no sooner than `wake_at` unless a line changes; `seen_gen`
    /// is the [`Self::line_gen`] it decided that under.
    idle: AtomicBool,
    wake_at: AtomicU64,
    seen_gen: AtomicU64,
}

impl Default for PerCore {
    fn default() -> Self {
        PerCore {
            lines: AtomicU8::new(0),
            line_gen: AtomicU64::new(0),
            events: AtomicBool::new(false),
            cycles: AtomicU64::new(0),
            running: AtomicBool::new(false),
            idle: AtomicBool::new(false),
            wake_at: AtomicU64::new(u64::MAX),
            seen_gen: AtomicU64::new(0),
        }
    }
}

/// What the machine's thread and the cores' threads share.
struct Shared {
    ram: SharedRam,
    /// The runs compiled for every core.
    #[cfg(feature = "jit")]
    jit: Option<crate::aarch64::jit::Shared>,
    ram_len: u64,
    quit: AtomicBool,
    /// The machine is gone: stop waiting for it.
    abandon: AtomicBool,
    held: AtomicBool,
    /// The VPU's clock in ARM cycles since release, and how far the cores may run.
    now: AtomicU64,
    limit: AtomicU64,
    hz: AtomicU64,
    /// Bit `id` set: core `id` has a request waiting in its slot.
    pending: Line<AtomicU32>,
    slots: [Slot; CORES],
    core: [PerCore; CORES],
    /// Counts the broadcast `TLBI`s, the last [`TLBIS`] of which are in
    /// `tlbis` ([`Shared::tlbi`]).
    tlb_gen: Line<AtomicU64>,
    tlbis: [(AtomicU64, AtomicU64); TLBIS],
    stop_on_store: AtomicBool,
    /// The earliest cycle after a peripheral store since the last
    /// [`ParHost::run_until_store`] began, or `u64::MAX`.
    stored_at: Line<AtomicU64>,
    stopped: AtomicBool,
    stop: Mutex<Option<ArmStop>>,
    threads: [OnceLock<Thread>; CORES],
    main: Thread,
}

/// Broadcast `TLBI`s a core can catch up on; one further behind flushes all.
const TLBIS: usize = 64;

impl Shared {
    /// Tell the other cores' TLBs to drop `what`.
    fn tlbi(&self, what: crate::aarch64::mmu::Inval) {
        use crate::aarch64::mmu::Inval;
        let n = self.tlb_gen.fetch_add(1, Ordering::AcqRel);
        let (seq, v) = &self.tlbis[n as usize % TLBIS];
        // As a seqlock: no reader takes the value while it is being changed.
        seq.store(0, Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        v.store(
            match what {
                Inval::All => u64::MAX,
                Inval::Page(p) => p & ((1 << 44) - 1),
                Inval::Asid(a) => 1 << 62 | u64::from(a),
            },
            Ordering::Relaxed,
        );
        seq.store(n + 1, Ordering::Release);
    }

    /// Each broadcast `TLBI` from number `from` up to `to`, or everything
    /// where one was missed or is not in yet. A core's own come back to it,
    /// which drops what it dropped already.
    fn tlbis_since(&self, from: u64, to: u64, mut drop: impl FnMut(crate::aarch64::mmu::Inval)) {
        use crate::aarch64::mmu::Inval;
        if to - from > TLBIS as u64 {
            return drop(Inval::All);
        }
        for n in from..to {
            let (seq, v) = &self.tlbis[n as usize % TLBIS];
            let before = seq.load(Ordering::Acquire);
            let v = v.load(Ordering::Relaxed);
            std::sync::atomic::fence(Ordering::Acquire);
            if before != n + 1 || seq.load(Ordering::Relaxed) != n + 1 || v == u64::MAX {
                return drop(Inval::All);
            }
            drop(if v >> 62 & 1 != 0 {
                Inval::Asid(v as u16)
            } else {
                Inval::Page(v)
            });
        }
    }
    fn wake(&self, id: usize) {
        if let Some(t) = self.threads[id].get() {
            t.unpark();
        }
    }

    fn wake_all(&self) {
        (0..CORES).for_each(|id| self.wake(id));
    }
}

/// The machine thread's side: the handles to the cores and the bookkeeping the
/// run loop needs.
pub(super) struct ParHost {
    sh: Arc<Shared>,
    threads: Vec<CoreThread>,
    n: usize,
    released_at: u64,
    spis: [bool; SPIS.len()],
    spis_at: (u64, u64, u64, bool, bool),
    steps: u32,
    published: u64,
    lines_now: [u8; CORES],
}

impl ParHost {
    pub(super) fn start(m: &mut Machine, cores: Vec<Core>, released_at: u64) -> ParHost {
        let n = cores.len();
        let ram_len = m.ram.len();
        let ram = m.ram.share();
        let now = m
            .systimer
            .cycles_at(gentimer::ARM_HZ)
            .saturating_sub(released_at);
        let sh = Arc::new(Shared {
            ram,
            ram_len,
            quit: AtomicBool::new(false),
            abandon: AtomicBool::new(false),
            held: AtomicBool::new(false),
            now: AtomicU64::new(now),
            limit: AtomicU64::new(now + WINDOW),
            hz: AtomicU64::new(m.arm_local.counter_hz().unwrap_or(0)),
            pending: Line(AtomicU32::new(0)),
            slots: Default::default(),
            core: Default::default(),
            tlb_gen: Line(AtomicU64::new(0)),
            tlbis: std::array::from_fn(|_| Default::default()),
            stop_on_store: AtomicBool::new(false),
            stored_at: Line(AtomicU64::new(u64::MAX)),
            stopped: AtomicBool::new(false),
            stop: Mutex::new(None),
            threads: Default::default(),
            main: std::thread::current(),
            #[cfg(feature = "jit")]
            jit: crate::aarch64::jit::Shared::new::<ParBus<'static>>(),
        });
        let mut host = ParHost {
            sh: sh.clone(),
            threads: Vec::with_capacity(n),
            n,
            released_at,
            spis: [false; SPIS.len()],
            spis_at: (u64::MAX, u64::MAX, u64::MAX, false, false),
            steps: 0,
            published: now,
            lines_now: [0; CORES],
        };
        host.refresh_lines(m);
        for (id, core) in cores.into_iter().enumerate() {
            let sh = sh.clone();
            let start = core.par_cycles.max(now.saturating_sub(WINDOW));
            let handle = CoreThread::spawn(id, move || Worker::new(&sh, id, core, start).run());
            host.threads.push(handle);
        }
        host
    }

    /// Where the VPU's clock is, in cycles since release.
    fn vpu_cycles(&self, m: &Machine) -> u64 {
        m.systimer
            .cycles_at(gentimer::ARM_HZ)
            .saturating_sub(self.released_at)
    }

    /// Take the devices' interrupt lines into the GIC; whether any moved.
    fn read_spis(&mut self, m: &mut Machine) -> bool {
        self.spis_at = spi_stamp(m);
        let levels = spi_levels(m);
        if levels == self.spis {
            return false;
        }
        self.spis = levels;
        for (&id, &level) in SPIS.iter().zip(&self.spis) {
            m.gic.set_spi_level(id, level);
        }
        true
    }

    /// Publish what each core's interrupt inputs are, waking a core whose
    /// inputs changed.
    fn publish_lines(&mut self, m: &Machine) {
        for id in 0..self.n {
            let bits = match m.gic.signal(id) {
                Some(s) if s.fiq => 2,
                Some(_) => 1,
                None => 0,
            };
            if self.lines_now[id] != bits {
                self.lines_now[id] = bits;
                self.sh.core[id].lines.store(bits, Ordering::Release);
                self.sh.core[id].line_gen.fetch_add(1, Ordering::Release);
                self.sh.wake(id);
            }
        }
    }

    /// The GIC has been written, or a device line moved: bring both up to date.
    fn refresh_lines(&mut self, m: &mut Machine) {
        self.read_spis(m);
        self.publish_lines(m);
    }

    /// Answer every request that is waiting.
    fn service(&mut self, m: &mut Machine) {
        let mut pending = self.sh.pending.swap(0, Ordering::AcqRel);
        while pending != 0 {
            let id = pending.trailing_zeros() as usize;
            pending &= pending - 1;
            let sh = self.sh.clone();
            let slot = &sh.slots[id];
            let acc = gic::Accessor {
                cpu: id,
                secure: slot.secure.load(Ordering::Relaxed),
            };
            let addr = slot.addr.load(Ordering::Relaxed);
            let size = slot.size.load(Ordering::Relaxed);
            let value = slot.value.load(Ordering::Relaxed);
            let ok = match slot.op.load(Ordering::Relaxed) {
                READ => {
                    let target = route(m.ram.len(), addr, size, false);
                    let r = match target {
                        Ok(t) => device_read(m, t, bus_width(size), acc),
                        Err(_) => Err(()),
                    };
                    slot.value.store(r.unwrap_or(0), Ordering::Relaxed);
                    r.is_ok()
                }
                WRITE => {
                    let target = route(m.ram.len(), addr, size, true);
                    match target {
                        Ok(t) => {
                            let cycles = slot.cycles.load(Ordering::Relaxed);
                            let arm_us =
                                (self.released_at + cycles) / (gentimer::ARM_HZ / 1_000_000);
                            device_write(m, t, bus_width(size), value as u32, acc, arm_us)
                                .0
                                .is_ok()
                        }
                        Err(_) => false,
                    }
                }
                _ => {
                    m.gic.set_ppi_level(id, addr as u32, value != 0);
                    true
                }
            };
            slot.ok.store(ok, Ordering::Relaxed);
            self.refresh_lines(m);
            slot.state.store(ANSWERED, Ordering::Release);
        }
    }

    /// The machine thread's turn, once per VPU step.
    #[inline]
    pub(super) fn sync(&mut self, m: &mut Machine) {
        if self.sh.pending.load(Ordering::Relaxed) != 0 {
            self.service(m);
        }
        self.steps += 1;
        if self.steps < SYNC_STEPS {
            return;
        }
        self.steps = 0;
        self.time_sync(m);
    }

    fn time_sync(&mut self, m: &mut Machine) {
        if spi_stamp(m) != self.spis_at && self.read_spis(m) {
            self.publish_lines(m);
        }
        self.sh.held.store(m.clockman.arm_held(), Ordering::Relaxed);
        self.sh
            .hz
            .store(m.arm_local.counter_hz().unwrap_or(0), Ordering::Relaxed);
        let now = self.vpu_cycles(m);
        if now >= self.published + CHUNK {
            self.publish(now);
        }
        if self.too_far_ahead(now) {
            self.sh.wake_all();
        }
        let mut spins = 0;
        while self.too_far_ahead(now) {
            self.service(m);
            if self.sh.stopped.load(Ordering::Relaxed) || self.sh.quit.load(Ordering::Relaxed) {
                break;
            }
            Self::wait_a_little(&mut spins);
        }
    }

    /// Give the host's other threads the processor while waiting on the cores:
    /// a request wakes this thread early.
    fn wait_a_little(spins: &mut u32) {
        *spins += 1;
        if *spins < 200 {
            std::hint::spin_loop();
        } else if *spins < 400 {
            std::thread::yield_now();
        } else {
            std::thread::park_timeout(Duration::from_micros(50));
        }
    }

    fn publish(&mut self, now: u64) {
        self.published = now;
        self.sh.now.store(now, Ordering::Release);
        let limit = self.sh.limit.load(Ordering::Relaxed).max(now + WINDOW);
        self.sh.limit.store(limit, Ordering::Release);
        self.sh.wake_all();
    }

    /// The VPU is more than a window ahead of a core that is awake.
    fn too_far_ahead(&self, now: u64) -> bool {
        (0..self.n).any(|id| {
            !self.sh.core[id].idle.load(Ordering::Acquire)
                && self.sh.core[id].cycles.load(Ordering::Relaxed) + WINDOW < now
        })
    }

    pub(super) fn take_stop(&mut self) -> Option<ArmStop> {
        if !self.sh.stopped.load(Ordering::Acquire) {
            return None;
        }
        self.sh.stop.lock().unwrap().clone()
    }

    /// Let the cores run up to `until_us`, the VPU's next compare, ending the
    /// wait after the first cycle in which one writes a VPU-side peripheral.
    /// Returns the microsecond the VPU wakes in.
    pub(super) fn run_until_store(&mut self, m: &mut Machine, until_us: u64) -> u64 {
        const PER_US: u64 = gentimer::ARM_HZ / 1_000_000;
        let end = (until_us * PER_US).saturating_sub(self.released_at);
        if (0..self.n).all(|id| {
            self.sh.core[id].idle.load(Ordering::Acquire)
                || self.sh.core[id].cycles.load(Ordering::Relaxed) >= end
        }) && self.cores_rest(end)
        {
            return until_us;
        }
        let sh = self.sh.clone();
        sh.stored_at.store(u64::MAX, Ordering::SeqCst);
        sh.stop_on_store.store(true, Ordering::SeqCst);
        let mut spins = 0;
        loop {
            self.service(m);
            let stored = sh.stored_at.load(Ordering::SeqCst) != u64::MAX;
            if sh.stopped.load(Ordering::Relaxed) || sh.quit.load(Ordering::Relaxed) {
                break;
            }
            if sh.pending.load(Ordering::SeqCst) == 0
                && if stored {
                    self.all_resting()
                } else {
                    self.cores_rest(end)
                }
            {
                break;
            }
            self.advance_towards(end);
            Self::wait_a_little(&mut spins);
        }
        sh.stop_on_store.store(false, Ordering::SeqCst);
        match sh.stored_at.swap(u64::MAX, Ordering::SeqCst) {
            u64::MAX => until_us,
            at => (self.released_at + at).div_ceil(PER_US).min(until_us),
        }
    }

    /// Move the VPU's clock, and with it the cores' limit, towards `end` as
    /// fast as the slowest awake core and the earliest sleeper's wake-up
    /// allow, never more than a window past either. The cores stay within a
    /// window of each other, so a guest that moves from one to another never
    /// sees the counter go back.
    fn advance_towards(&mut self, end: u64) {
        let sh = &self.sh;
        let base = (0..self.n)
            .map(|id| {
                if sh.core[id].idle.load(Ordering::SeqCst) {
                    sh.core[id].wake_at.load(Ordering::SeqCst)
                } else {
                    sh.core[id].cycles.load(Ordering::SeqCst)
                }
            })
            .min()
            .unwrap_or(end)
            .min(end);
        let target = end.min(base.saturating_add(WINDOW));
        if base > sh.now.load(Ordering::Relaxed) {
            sh.now.store(base, Ordering::Release);
        }
        if target > sh.limit.load(Ordering::Relaxed) {
            sh.limit.store(target, Ordering::Release);
            sh.wake_all();
        }
    }

    /// Every core has either run to `end` or is asleep until after it.
    fn cores_rest(&self, end: u64) -> bool {
        (0..self.n).all(|id| {
            let sh = &self.sh;
            if sh.core[id].running.load(Ordering::SeqCst) {
                return false;
            }
            if sh.core[id].idle.load(Ordering::SeqCst) {
                sh.core[id].seen_gen.load(Ordering::SeqCst)
                    == sh.core[id].line_gen.load(Ordering::SeqCst)
                    && sh.core[id].wake_at.load(Ordering::SeqCst) > end
            } else {
                sh.core[id].cycles.load(Ordering::SeqCst) >= end
            }
        })
    }

    /// No core is executing an instruction.
    fn all_resting(&self) -> bool {
        (0..self.n).all(|id| !self.sh.core[id].running.load(Ordering::SeqCst))
    }

    /// Stop the threads and take the cores back.
    pub(super) fn pause(mut self, m: &mut Machine) -> Vec<Core> {
        self.sh.quit.store(true, Ordering::SeqCst);
        self.sh.wake_all();
        let threads = std::mem::take(&mut self.threads);
        let mut spins = 0;
        while threads.iter().any(|t| !t.is_finished()) {
            self.service(m);
            Self::wait_a_little(&mut spins);
        }
        threads.into_iter().map(CoreThread::take).collect()
    }
}

impl Drop for ParHost {
    fn drop(&mut self) {
        self.sh.abandon.store(true, Ordering::SeqCst);
        self.sh.quit.store(true, Ordering::SeqCst);
        self.sh.wake_all();
        for t in self.threads.drain(..) {
            while !t.is_finished() {
                std::thread::park_timeout(NAP);
            }
        }
    }
}

/// Whether `n` core threads can start now. On a native host they always can;
/// in the browser they need worker threads the page has started.
pub(super) fn can_spawn(n: usize) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        super::pool::threads() >= n
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = n;
        true
    }
}

/// What a core's thread leaves behind: the core, and that it is done.
struct Finished {
    core: Mutex<Option<Core>>,
    done: AtomicBool,
}

/// A core's thread, from which the core comes back. Native threads are
/// `std::thread`s; in the browser they are the page's pool of workers.
struct CoreThread {
    fin: Arc<Finished>,
    #[cfg(not(target_arch = "wasm32"))]
    handle: std::thread::JoinHandle<()>,
}

impl CoreThread {
    fn spawn(id: usize, run: impl FnOnce() -> Core + Send + 'static) -> CoreThread {
        let fin = Arc::new(Finished {
            core: Mutex::new(None),
            done: AtomicBool::new(false),
        });
        let theirs = fin.clone();
        let job = move || {
            *theirs.core.lock().unwrap() = Some(run());
            theirs.done.store(true, Ordering::Release);
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            let handle = std::thread::Builder::new()
                .name(format!("pimu-arm{id}"))
                .spawn(job)
                .expect("a host thread for an ARM core");
            CoreThread { fin, handle }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = id;
            super::pool::submit(Box::new(job));
            CoreThread { fin }
        }
    }

    fn is_finished(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        if self.handle.is_finished() {
            return true;
        }
        self.fin.done.load(Ordering::Acquire)
    }

    fn take(self) -> Core {
        #[cfg(not(target_arch = "wasm32"))]
        let _ = self.handle.join();
        self.fin
            .core
            .lock()
            .unwrap()
            .take()
            .expect("an ARM core thread panicked")
    }
}

fn bus_width(size: u32) -> Width {
    match size {
        1 => Width::Byte,
        2 => Width::Half,
        _ => Width::Word,
    }
}

/// One core's thread.
struct Worker<'a> {
    sh: &'a Shared,
    id: usize,
    core: Core,
    cycles: u64,
    timer_due: u64,
    ppi: [bool; 4],
    tlb_seen: u64,
    hz: u64,
    poll: Poll,
    /// The straight runs this core decoded, the way a lone core's are
    /// (`super::runs`), and the instruction words behind them.
    runs: Box<super::runs::Runs<ParBus<'static>>>,
    decoded: Box<Decoded>,
}

/// The last device read, and how many times in a row it came back the same.
#[derive(Default, Clone, Copy)]
struct Poll {
    addr: u64,
    value: u64,
    same: u32,
}

impl<'a> Worker<'a> {
    fn new(sh: &'a Shared, id: usize, core: Core, cycles: u64) -> Worker<'a> {
        Worker {
            sh,
            id,
            core,
            cycles,
            timer_due: cycles,
            ppi: [false; 4],
            tlb_seen: sh.tlb_gen.load(Ordering::Acquire),
            hz: u64::MAX,
            poll: Poll::default(),
            runs: Default::default(),
            decoded: Box::new(Decoded::new()),
        }
    }

    fn run(mut self) -> Core {
        let sh = self.sh;
        let id = self.id;
        let _ = sh.threads[id].set(std::thread::current());
        self.core.cpu.tlb.flush();
        while !sh.quit.load(Ordering::Acquire) {
            if sh.stopped.load(Ordering::Relaxed) {
                self.nap();
                continue;
            }
            self.refresh();
            if sh.held.load(Ordering::Relaxed) {
                self.cycles = self.cycles.max(sh.now.load(Ordering::Acquire));
                self.sit_out_hold();
                continue;
            }
            if self.core.waiting {
                if self.try_wake() {
                    sh.core[id].idle.store(false, Ordering::SeqCst);
                } else {
                    self.sleep();
                }
                continue;
            }
            sh.core[id].idle.store(false, Ordering::SeqCst);
            let limit = self.limit();
            if self.cycles >= limit {
                sh.core[id].cycles.store(self.cycles, Ordering::Release);
                self.nap();
                continue;
            }
            sh.core[id].running.store(true, Ordering::SeqCst);
            self.batch(limit);
            sh.core[id].running.store(false, Ordering::SeqCst);
            sh.core[id].cycles.store(self.cycles, Ordering::SeqCst);
        }
        sh.core[id].cycles.store(self.cycles, Ordering::Release);
        self.core.par_cycles = self.cycles;
        self.core
    }

    fn nap(&self) {
        std::thread::park_timeout(NAP);
    }

    /// The ARM channel has no clock: rest as a core asleep with nothing due
    /// would, so the VPU, which alone can release the hold, never waits on it.
    fn sit_out_hold(&self) {
        let rest = &self.sh.core[self.id];
        rest.wake_at.store(u64::MAX, Ordering::SeqCst);
        rest.seen_gen
            .store(rest.line_gen.load(Ordering::SeqCst), Ordering::SeqCst);
        rest.cycles.store(self.cycles, Ordering::Release);
        rest.idle.store(true, Ordering::SeqCst);
        self.nap();
    }

    /// How far this core may run now.
    fn limit(&self) -> u64 {
        let limit = self.sh.limit.load(Ordering::Acquire);
        if self.sh.stop_on_store.load(Ordering::SeqCst)
            && self.sh.stored_at.load(Ordering::SeqCst) != u64::MAX
        {
            limit.min(self.cycles)
        } else {
            limit
        }
    }

    /// Take in what the other threads changed: interrupt lines, `sev`s, TLB
    /// maintenance, and the counter's rate.
    #[inline]
    fn refresh(&mut self) {
        let sh = self.sh;
        let id = self.id;
        let lines = sh.core[id].lines.load(Ordering::Acquire);
        let cpu = &mut self.core.cpu;
        cpu.irq_line = lines & 1 != 0;
        cpu.fiq_line = lines & 2 != 0;
        if sh.core[id].events.load(Ordering::Relaxed)
            && sh.core[id].events.swap(false, Ordering::AcqRel)
        {
            cpu.event = true;
        }
        let gen = sh.tlb_gen.load(Ordering::Acquire);
        if gen != self.tlb_seen {
            sh.tlbis_since(self.tlb_seen, gen, |what| cpu.tlb.invalidate(what));
            self.tlb_seen = gen;
        }
        let hz = sh.hz.load(Ordering::Relaxed);
        if hz != self.hz {
            self.hz = hz;
            self.core.timer.set_hz(self.cycles, hz);
            self.timer_due = self.cycles;
        }
    }

    /// The core is in `wfi` or `wfe`: has anything ended the wait?
    fn try_wake(&mut self) -> bool {
        let core = &mut self.core;
        if let (Some(_), Some((pa, size, value))) = (core.wfe_until, core.cpu.exclusive_mark()) {
            if self.sh.ram.load(pa, size).is_some_and(|v| v != value) {
                core.cpu.lose_mark();
            }
        }
        let wfe_done = core
            .wfe_until
            .is_some_and(|t| core.cpu.event || t <= self.cycles);
        if !(core.line_wakes() || wfe_done) {
            return false;
        }
        core.waiting = false;
        if core.wfe_until.take().is_some() {
            core.cpu.event = false;
        }
        self.cycles = self.cycles.max(self.sh.now.load(Ordering::Acquire));
        true
    }

    /// Sit out the wait: jump to the next thing due if the window has room for
    /// it, else sleep until a line changes.
    fn sleep(&mut self) {
        let sh = self.sh;
        let id = self.id;
        let gen = sh.core[id].line_gen.load(Ordering::Acquire);
        let due = [self.core.timer.next_event(self.cycles), self.core.wfe_until]
            .into_iter()
            .flatten()
            .min();
        if let Some(due) = due.filter(|&d| d <= sh.limit.load(Ordering::Acquire)) {
            self.cycles = self.cycles.max(due);
            self.timer_check();
            return;
        }
        sh.core[id]
            .wake_at
            .store(due.unwrap_or(u64::MAX), Ordering::SeqCst);
        sh.core[id].seen_gen.store(gen, Ordering::SeqCst);
        sh.core[id].idle.store(true, Ordering::SeqCst);
        if sh.core[id].line_gen.load(Ordering::SeqCst) == gen && !sh.quit.load(Ordering::Relaxed) {
            std::thread::park_timeout(NAP);
        }
    }

    /// Tell the GIC which of this core's timers drive their lines now.
    fn timer_check(&mut self) {
        for (i, w) in Which::ALL.into_iter().enumerate() {
            let level = self.core.timer.line(w, self.cycles);
            if level != self.ppi[i] {
                self.ppi[i] = level;
                self.request(PPI, u64::from(w.intid()), u64::from(level));
            }
        }
        self.timer_due = self.core.timer.next_event(self.cycles).unwrap_or(u64::MAX);
        self.refresh();
    }

    /// Ask the machine's thread for something.
    fn request(&mut self, op: u32, addr: u64, value: u64) {
        let _ = ask(self.sh, self.id, op, addr, 0, value, false, self.cycles);
    }

    /// Run until `limit`, or until something the outer loop has to see.
    fn batch(&mut self, limit: u64) {
        let sh = self.sh;
        let id = self.id;
        let mut n = 0u64;
        let mut next_poll = 0u64;
        let mut cycles = self.cycles;
        let stop_on_store = sh.stop_on_store.load(Ordering::Relaxed);
        loop {
            if n >= next_poll {
                next_poll = n + POLL_MASK + 1;
                self.cycles = cycles;
                if n & 255 < POLL_MASK + 1 {
                    sh.core[id].cycles.store(cycles, Ordering::Relaxed);
                }
                self.refresh();
                if sh.quit.load(Ordering::Relaxed)
                    || sh.stopped.load(Ordering::Relaxed)
                    || sh.held.load(Ordering::Relaxed)
                    || cycles >= self.limit_hint(limit)
                {
                    break;
                }
            }
            if cycles >= self.timer_due {
                self.cycles = cycles;
                self.timer_check();
            }
            let Core {
                cpu,
                timer,
                insns,
                exceptions,
                interrupts,
                waiting,
                wfe_until,
                entered,
                ..
            } = &mut self.core;
            let mut bus = ParBus {
                sh,
                id,
                timer,
                cycles,
                secure: cpu.el == 3 || cpu.sys.scr_el3 & crate::aarch64::sysreg::SCR_NS == 0,
                expect: None,
                lost: false,
                io: false,
                stored: false,
                timer_written: false,
                gone: false,
                poll: &mut self.poll,
                skip: 0,
                stopped: std::ptr::null(),
                next: 0,
                ran: false,
                stop_on_store,
                genp: std::ptr::null(),
                chain_until: 0,
                targets: std::ptr::addr_of!(self.runs.targets),
                moved: false,
            };
            let mut pc = cpu.pc;
            let mut step = None;
            let n_before = n;
            if !cpu.irq_line && !cpu.fiq_line && entered.is_some() && !cpu.stage2_on() {
                let (el, mut va_page) = (cpu.el, pc & !0xFFF);
                let page = cpu
                    .tlb
                    .code_page(pc, el)
                    .map(|(pa, code, gen)| (pa, code as *const u32, gen as *const u64))
                    .or_else(|| {
                        let (hint, hint_el, pa) = cpu.tlb.fetch_hint()?;
                        (hint_el == el && hint == va_page).then_some(())?;
                        let (code, gen) = sh.ram.code_page(pa)?;
                        cpu.tlb
                            .remember_code(pc, el, pa, code as usize, gen as usize);
                        Some((pa, code, gen))
                    });
                if pc & 3 == 0 {
                    if let Some((mut pa_page, mut code, mut genp)) = page {
                        let effects = cpu.shared_effects;
                        let (runs, decoded) = (&mut *self.runs, &mut *self.decoded);
                        // Odd while runs are decoded from the page, so that a
                        // store into it moves it on (`SharedRam::code_page`).
                        let gen_of = |genp: *const u64| {
                            let g = unsafe { AtomicU64::from_ptr(genp as *mut u64) };
                            let v = g.load(Ordering::Relaxed);
                            if v & 1 == 0 {
                                g.store(v + 1, Ordering::Release);
                            }
                            v | 1
                        };
                        // A linked run fits before the limit from this far before it.
                        let chain_end = limit.saturating_sub(super::runs::MAX_LEN);
                        bus.genp = genp;
                        // The taken branch to link to the run looked up next.
                        let mut unlinked: Option<*const RunOp> = None;
                        loop {
                            pc = cpu.pc;
                            if cycles >= limit || n >= next_poll || pc & 3 != 0 {
                                break;
                            }
                            // Into another page this core has run code from
                            // under the translation in force: on, without the
                            // look round the loop above takes.
                            if pc & !0xFFF != va_page {
                                let Some((pa, c, g)) = cpu.tlb.code_page(pc, el) else {
                                    break;
                                };
                                (va_page, pa_page) = (pc & !0xFFF, pa);
                                (code, genp) = (c as *const u32, g as *const u64);
                                bus.genp = genp;
                                unlinked = None;
                            }
                            let gen = gen_of(genp);
                            let pa = pa_page | (pc & 0xFFF);
                            bus.expect = None;
                            bus.chain_until = chain_end
                                .min(cycles + super::runs::MAX_LEN)
                                .min(cycles + (next_poll - n));
                            #[cfg(feature = "jit")]
                            let compiled = sh
                                .jit
                                .as_ref()
                                .and_then(|jit| Some((jit, jit.find(pa, gen)?)));
                            #[cfg(not(feature = "jit"))]
                            let compiled: Option<&()> = None;
                            let (at, c) = match compiled {
                                #[cfg(feature = "jit")]
                                Some((jit, t)) => {
                                    if cycles + t.len > limit {
                                        break;
                                    }
                                    unlinked = None;
                                    let mut ctx = crate::aarch64::jit::Ctx {
                                        gen,
                                        marked: 0,
                                        genp,
                                        // Not cut at the next poll, as an
                                        // interpreted link is: the poll just
                                        // comes after.
                                        chain_until: chain_end.min(cycles + super::runs::MAX_LEN),
                                    };
                                    t.go(jit.enter(), cpu, &mut bus, &mut ctx, pc, cycles)
                                }
                                #[cfg(not(feature = "jit"))]
                                Some(_) => unreachable!(),
                                None => {
                                    let runs_epoch = runs.epoch;
                                    let ops = runs.get(pa, code, genp, decoded);
                                    #[cfg(feature = "jit")]
                                    if let Some(jit) = &sh.jit {
                                        jit.entered(ops, pa, gen);
                                    }
                                    let (len, first) = (ops.len() as u64, ops.as_ptr());
                                    if len == 0 || cycles + len > limit {
                                        break;
                                    }
                                    // Stays put while `runs.epoch` does.
                                    let first = unsafe { &*first };
                                    if let Some(branch) = unlinked.take() {
                                        if runs.epoch == runs_epoch {
                                            // `get` has checked the run against the page.
                                            unsafe { (*branch).link(first, gen) };
                                        }
                                    }
                                    let (at, c) = first.go(cpu, &mut bus, pc, cycles);
                                    if std::mem::take(&mut bus.moved) {
                                        // Looked up afresh, as the page the
                                        // run ended on may be another.
                                        va_page = u64::MAX;
                                    }
                                    if bus.ran
                                        && !cpu.faulted()
                                        && unsafe { &*bus.stopped }.op.plain_branch()
                                        && bus.next != at.wrapping_add(4)
                                        && bus.next & !0xFFF == va_page
                                    {
                                        unlinked = Some(bus.stopped);
                                    }
                                    (at, c)
                                }
                            };
                            let op: &RunOp = unsafe { &*bus.stopped };
                            *insns += c - cycles;
                            n += c - cycles;
                            cycles = c;
                            pc = at;
                            let s = if !bus.ran {
                                bus.cycles = cycles;
                                let next = op.op.handler()(cpu, &op.op, &mut bus, pc);
                                if cpu.faulted() {
                                    cpu.stopped_at(pc, next, &op.op)
                                } else {
                                    cpu.pc = next;
                                    Step::Retired
                                }
                            } else if cpu.faulted() {
                                cpu.stopped_at(pc, bus.next, &op.op)
                            } else {
                                cpu.pc = bus.next;
                                Step::Retired
                            };
                            if !matches!(s, Step::Retired)
                                || !bus.plain()
                                || bus.stored && stop_on_store
                                || cpu.sev
                                || cpu.tlb.broadcast.is_some()
                                || cpu.shared_effects != effects
                            {
                                step = Some(s);
                                break;
                            }
                            *insns += 1;
                            cycles += 1;
                            n += 1;
                            if cpu.el != el {
                                break;
                            }
                        }
                    }
                }
            }
            let step = match step {
                Some(s) => s,
                None if n != n_before => continue,
                None if cycles >= limit => break,
                None => {
                    pc = cpu.pc;
                    bus.cycles = cycles;
                    bus.expect = None;
                    cpu.step_system(&mut bus)
                }
            };
            n += 1;
            let (io, stored, timer_written, gone) =
                (bus.io, bus.stored, bus.timer_written, bus.gone);
            cycles += bus.skip;
            let mut unsupported = None;
            match step {
                Step::Retired => *insns += 1,
                Step::Wfe => {
                    *insns += 1;
                    *waiting = true;
                    *wfe_until = Some(cycles + wfe_timeout(cpu, timer));
                }
                Step::Wfi => {
                    *insns += 1;
                    *waiting = true;
                }
                Step::Took(_) => *exceptions += 1,
                Step::Interrupt { .. } => *interrupts += 1,
                Step::Unimplemented(insn) => {
                    unsupported = Some(ArmStop::Unimplemented {
                        core: id,
                        pc,
                        el: cpu.el,
                        insn,
                    })
                }
                Step::Unsupported(what) => {
                    unsupported = Some(ArmStop::Unsupported {
                        core: id,
                        pc,
                        el: cpu.el,
                        what,
                    })
                }
                Step::Exception(_) => unreachable!("step_system takes exceptions"),
            }
            if entered.is_none() && cpu.pc >= STUB_END {
                *entered = Some((cycles, cpu.el, cpu.pc, cpu.x[0]));
            }
            let broadcast = std::mem::take(&mut cpu.tlb.broadcast);
            let sev = std::mem::take(&mut cpu.sev);
            cycles += 1;
            if let Some(stop) = unsupported {
                *sh.stop.lock().unwrap() = Some(stop);
                sh.stopped.store(true, Ordering::Release);
                break;
            }
            if let Some(what) = broadcast {
                sh.tlbi(what);
                sh.wake_all();
            }
            if sev {
                for k in (0..CORES).filter(|&k| k != id) {
                    sh.core[k].events.store(true, Ordering::Release);
                    sh.wake(k);
                }
            }
            if stored && sh.stop_on_store.load(Ordering::SeqCst) {
                sh.stored_at.fetch_min(cycles, Ordering::SeqCst);
            }
            if io {
                self.cycles = cycles;
                self.refresh();
            }
            if timer_written {
                self.cycles = cycles;
                self.timer_check();
            }
            if gone || self.core.waiting {
                break;
            }
        }
        self.cycles = cycles;
    }

    /// The limit, less what a store-triggered stop takes off it.
    #[inline]
    fn limit_hint(&self, limit: u64) -> u64 {
        if self.sh.stop_on_store.load(Ordering::Relaxed)
            && self.sh.stored_at.load(Ordering::Relaxed) != u64::MAX
        {
            0
        } else {
            limit.max(self.sh.limit.load(Ordering::Relaxed))
        }
    }
}

/// One core's view of the machine while it runs on a thread of its own.
struct ParBus<'a> {
    sh: &'a Shared,
    id: usize,
    timer: &'a mut gentimer::GenericTimer,
    cycles: u64,
    secure: bool,
    expect: Option<u64>,
    lost: bool,
    io: bool,
    stored: bool,
    timer_written: bool,
    gone: bool,
    poll: &'a mut Poll,
    skip: u64,
    /// Where a straight run stopped ([`RunBus::stop`]).
    stopped: *const RunOp,
    next: u64,
    ran: bool,
    stop_on_store: bool,
    /// The write generation of the page the runs are from, and the cycle a
    /// branch may go on into a linked run before ([`RunBus::chain`]).
    genp: *const u64,
    chain_until: u64,
    /// The runs a register branch may go on into ([`RunBus::indirect`]),
    /// and whether one went into a run on another page since the run loop
    /// last looked.
    targets: *const super::runs::Targets,
    moved: bool,
}

impl RunBus for ParBus<'_> {
    const INDIRECT: bool = true;

    #[inline(always)]
    fn at(&mut self, c: u64) {
        self.cycles = c;
    }

    #[inline(always)]
    fn plain(&self) -> bool {
        !self.io
            && !self.timer_written
            && !self.gone
            && self.skip == 0
            && !(self.stored && self.stop_on_store)
    }

    #[inline(always)]
    fn stop(&mut self, op: &RunOp, next: u64, ran: bool) {
        self.stopped = op;
        self.next = next;
        self.ran = ran;
    }

    #[inline(always)]
    fn chain(&self, c: u64, gen: u64) -> bool {
        // `genp` is the runs' page's, from `SharedRam::code_page`.
        c < self.chain_until
            && unsafe { AtomicU64::from_ptr(self.genp as *mut u64) }.load(Ordering::Relaxed) == gen
    }

    #[inline(always)]
    fn indirect(&mut self, cpu: &Cpu, next: u64, c: u64) -> Option<*const RunOp> {
        if c >= self.chain_until || next & 3 != 0 {
            return None;
        }
        let (pa, _, genp) = cpu.tlb.code_page(next, cpu.el)?;
        // From `SharedRam::code_page`, by way of the run loop.
        let gen = unsafe { AtomicU64::from_ptr(genp as *mut u64) }.load(Ordering::Relaxed);
        // The core's own runs, which outlive the bus.
        let run = unsafe { &*self.targets }.get(pa | (next & 0xFFF), gen)?;
        self.genp = genp as *const u64;
        self.moved = true;
        Some(run)
    }

    /// A store-exclusive is never compiled inline, so `marked` has nothing
    /// to say here: the compare-exchange is the bus's.
    #[cfg(feature = "jit")]
    #[inline(always)]
    fn settle(&mut self, gen: u64, genp: *const u64, _: bool) -> bool {
        // The page's generation, from `SharedRam::code_page`.
        self.plain()
            && unsafe { AtomicU64::from_ptr(genp as *mut u64) }.load(Ordering::Relaxed) == gen
    }
}

/// Post a request in core `id`'s slot and wait for the machine's thread to
/// answer it. `None` if the machine is gone or the access aborted.
#[allow(clippy::too_many_arguments)]
fn ask(
    sh: &Shared,
    id: usize,
    op: u32,
    addr: u64,
    size: u32,
    value: u64,
    secure: bool,
    cycles: u64,
) -> Result<u64, Asked> {
    let slot = &sh.slots[id];
    slot.op.store(op, Ordering::Relaxed);
    slot.addr.store(addr, Ordering::Relaxed);
    slot.size.store(size, Ordering::Relaxed);
    slot.value.store(value, Ordering::Relaxed);
    slot.secure.store(secure, Ordering::Relaxed);
    slot.cycles.store(cycles, Ordering::Relaxed);
    slot.state.store(ASKED, Ordering::Release);
    sh.pending.fetch_or(1 << id, Ordering::AcqRel);
    sh.main.unpark();
    let mut spins = 0u32;
    while slot.state.load(Ordering::Acquire) != ANSWERED {
        if sh.abandon.load(Ordering::Relaxed) {
            return Err(Asked::Gone);
        }
        spins += 1;
        if spins < 4_000 {
            std::hint::spin_loop();
        } else if spins < 8_000 {
            std::thread::yield_now();
        } else {
            std::thread::park_timeout(Duration::from_micros(20));
        }
    }
    slot.state.store(FREE, Ordering::Relaxed);
    if slot.ok.load(Ordering::Relaxed) {
        Ok(slot.value.load(Ordering::Relaxed))
    } else {
        Err(Asked::Aborted)
    }
}

enum Asked {
    Aborted,
    Gone,
}

impl ParBus<'_> {
    fn device(&mut self, op: u32, addr: u64, size: u32, value: u64) -> Result<u64, Abort> {
        self.io = true;
        let answer = ask(
            self.sh,
            self.id,
            op,
            addr,
            size,
            value,
            self.secure,
            self.cycles,
        )
        .map_err(|e| {
            self.gone |= matches!(e, Asked::Gone);
            Abort {
                addr,
                write: op == WRITE,
            }
        })?;
        if op == READ {
            let poll = &mut *self.poll;
            if (poll.addr, poll.value) == (addr, answer) {
                poll.same = poll.same.saturating_add(1);
                if poll.same > POLL_AFTER {
                    self.skip = POLL_SKIP;
                }
            } else {
                *poll = Poll {
                    addr,
                    value: answer,
                    same: 0,
                };
            }
        } else {
            *self.poll = Poll::default();
        }
        Ok(answer)
    }

    /// An access of at most 4 bytes outside DRAM.
    fn device32_read(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
        match route(self.sh.ram_len, addr, size, false)? {
            Target::Ram(off) => self
                .sh
                .ram
                .load(off, size)
                .ok_or(Abort { addr, write: false }),
            _ => self.device(READ, addr, size, 0),
        }
    }

    fn device32_write(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
        match route(self.sh.ram_len, addr, size, true)? {
            Target::Ram(off) => self
                .sh
                .ram
                .store(off, size, value)
                .ok_or(Abort { addr, write: true }),
            Target::Periph(_) => {
                self.stored = true;
                self.device(WRITE, addr, size, value).map(|_| ())
            }
            _ => self.device(WRITE, addr, size, value).map(|_| ()),
        }
    }
}

impl Memory for ParBus<'_> {
    #[inline]
    fn fetch(&mut self, addr: u64) -> Result<u32, Abort> {
        let end = addr.saturating_add(4);
        if end <= self.sh.ram_len && (end <= super::RAM_LOW_END || addr >= super::RAM_HIGH_BASE) {
            return self
                .sh
                .ram
                .load(addr, 4)
                .map(|v| v as u32)
                .ok_or(Abort { addr, write: false });
        }
        self.read(addr, 4).map(|v| v as u32)
    }

    #[inline]
    fn read(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
        if let Ok(Target::Ram(off)) = route(self.sh.ram_len, addr, size, false) {
            return self
                .sh
                .ram
                .load(off, size)
                .ok_or(Abort { addr, write: false });
        }
        if size == 8 {
            let lo = self.device32_read(addr, 4)?;
            let hi = self.device32_read(addr.wrapping_add(4), 4)?;
            return Ok(lo | (hi << 32));
        }
        self.device32_read(addr, size)
    }

    #[inline]
    fn write(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
        if let Ok(Target::Ram(off)) = route(self.sh.ram_len, addr, size, true) {
            if let Some(old) = self.expect.take() {
                if let Some(won) = self.sh.ram.compare_exchange(off, size, old, value) {
                    self.lost = !won;
                    return Ok(());
                }
            }
            return self
                .sh
                .ram
                .store(off, size, value)
                .ok_or(Abort { addr, write: true });
        }
        if size == 8 {
            self.device32_write(addr, 4, value)?;
            return self.device32_write(addr.wrapping_add(4), 4, value >> 32);
        }
        self.device32_write(addr, size, value)
    }

    fn sysreg_read(&mut self, key: u32) -> Option<u64> {
        super::timer_reg(key).map(|r| self.timer.read(r, self.cycles))
    }

    fn sysreg_write(&mut self, key: u32, value: u64) -> bool {
        match super::timer_reg(key) {
            Some(r) => {
                self.timer.write(r, self.cycles, value);
                self.timer_written = true;
                true
            }
            None => false,
        }
    }

    fn barrier(&mut self) {
        std::sync::atomic::fence(Ordering::SeqCst);
    }

    /// Not while a store-exclusive is under way, which has to be the bus's
    /// compare-exchange.
    #[inline(always)]
    fn direct(&self) -> bool {
        self.expect.is_none()
    }

    fn host_page(&mut self, pa_page: u64) -> Option<(usize, usize)> {
        let end = pa_page + 0x1000;
        if end > self.sh.ram_len || !(end <= super::RAM_LOW_END || pa_page >= super::RAM_HIGH_BASE)
        {
            return None;
        }
        let (bytes, gen) = self.sh.ram.page(pa_page)?;
        Some((bytes as usize, gen as usize))
    }

    #[inline(always)]
    fn bump(&mut self, gen: usize) {
        // A generation `host_page` handed out, inside the board's RAM.
        SharedRam::<crate::mem::DefaultMap>::bump(unsafe { AtomicU64::from_ptr(gen as *mut u64) });
    }

    fn expect_unchanged(&mut self, expected: u64) {
        self.expect = Some(expected);
    }

    fn exclusive_lost(&mut self) -> bool {
        self.expect = None;
        std::mem::take(&mut self.lost)
    }
}

/// Whether the cores really run side by side here: four of them increment one
/// word with `ldxr`/`stxr`, and every increment must be counted. A page runs
/// this once before it turns parallel cores on, since a browser may refuse the
/// worker threads they need.
pub fn probe() -> bool {
    use super::ArmSide;
    use crate::bus::Width;
    #[cfg(not(target_arch = "wasm32"))]
    use std::time::Instant;
    #[cfg(target_arch = "wasm32")]
    use web_time::Instant;

    const PER_CORE: u32 = 2_000;
    // x1 = 0x800, the counter; x4 = PER_CORE
    // 10: ldxr x2,[x1]; add x2,x2,#1; stxr w3,x2,[x1]; cbnz w3,10
    //     sub x4,x4,#1; cbnz x4,10
    // then a read of the system timer's CLO, which only the machine's thread can
    // answer, and a flag at 0x808 + 8 * id, and rest.
    let program = [
        0xD538_00A0,
        0x9240_1C00,
        0xD281_0001,
        0xD280_0004 | (PER_CORE << 5),
        0xC85F_7C22,
        0x9100_0442,
        0xC803_7C22,
        0x35FF_FFA3,
        0xD100_0484,
        0xB5FF_FF64,
        0xD2BF_C008,
        0xF286_0088,
        0xB940_0107,
        0xD37D_F005,
        0x8B01_00A5,
        0xD280_0026,
        0xF900_04A6,
        0x1400_0000,
    ];
    let mut m = Machine::new(1 << 20);
    let base = m.ram.base();
    for (i, w) in program.iter().enumerate() {
        let _ = m.ram.store(base + 4 * i as u32, Width::Word, *w);
    }
    let word = |m: &Machine, at: u32| m.ram.load(base + at, Width::Word).unwrap_or(0);
    let mut arm = ArmSide::new();
    arm.set_parallel(true);
    let start = Instant::now();
    let finished = |m: &Machine| (0..CORES as u32).all(|id| word(m, 0x808 + 8 * id) != 0);
    while !finished(&m) && arm.stopped.is_none() && start.elapsed() < Duration::from_secs(5) {
        m.tick(54 * 20);
        for _ in 0..SYNC_STEPS {
            arm.catch_up(&mut m);
        }
    }
    let threaded = arm.is_parallel();
    arm.pause(&mut m);
    threaded && finished(&m) && word(&m, 0x800) == CORES as u32 * PER_CORE
}

#[cfg(test)]
mod tests {
    use super::super::ArmSide;
    use crate::bus::Width;
    use crate::machine::Machine;
    use std::time::{Duration, Instant};

    fn machine_with(code: &[u32]) -> Machine {
        let mut m = Machine::new(1 << 20);
        for (i, w) in code.iter().enumerate() {
            let base = m.ram.base();
            m.ram.store(base + 4 * i as u32, Width::Word, *w).unwrap();
        }
        m
    }

    fn word(m: &Machine, at: u32) -> u32 {
        m.ram.load(m.ram.base() + at, Width::Word).unwrap()
    }

    /// Let the VPU's clock run and the cores follow until `done`, or fail
    /// after a few seconds of host time. The cores are left stopped.
    fn drive(m: &mut Machine, arm: &mut ArmSide, done: impl Fn(&Machine) -> bool) {
        arm.set_parallel(true);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(m) && arm.stopped.is_none() {
            assert!(Instant::now() < deadline, "the cores did not get there");
            m.tick(54 * 20);
            for _ in 0..super::SYNC_STEPS {
                arm.catch_up(m);
            }
        }
        arm.pause(m);
        assert_eq!(arm.stopped, None);
        assert_eq!(arm.cores.len(), super::CORES, "the cores came back");
    }

    #[test]
    fn every_core_runs_and_knows_which_it_is() {
        // mrs x0, mpidr_el1; and x0, x0, #0xff; add x0, x0, #1;
        // lsl x1, x0, #3; add x1, x1, #0x100; str x0, [x1]; b .
        let mut m = machine_with(&[
            0xD538_00A0,
            0x9240_1C00,
            0x9100_0400,
            0xD37D_F001,
            0x9104_0021,
            0xF900_0020,
            0x1400_0000,
        ]);
        let mut arm = ArmSide::new();
        drive(&mut m, &mut arm, |m| {
            (0..4).all(|id| word(m, 0x108 + 8 * id) != 0)
        });
        for id in 0..4 {
            assert_eq!(word(&m, 0x108 + 8 * id), id + 1);
        }
    }

    #[test]
    fn the_vpu_runs_on_while_the_arm_clock_is_held() {
        use crate::bus::MmioDevice;
        use crate::spec::cm::{PLLB, PLLB_HOLDARM_MASK};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let done = Arc::new(AtomicBool::new(false));
        let watched = done.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(20));
            if !watched.load(Ordering::SeqCst) {
                eprintln!("the VPU waited for cores that had no clock");
                std::process::abort();
            }
        });
        let step = |m: &mut Machine, arm: &mut ArmSide, rounds: u32| {
            for _ in 0..rounds {
                m.tick(54 * 20);
                for _ in 0..super::SYNC_STEPS {
                    arm.catch_up(m);
                }
            }
        };
        // b .
        let mut m = machine_with(&[0x1400_0000]);
        let mut arm = ArmSide::new();
        arm.set_parallel(true);
        step(&mut m, &mut arm, 50);
        m.clockman
            .write(PLLB, Width::Word, 0x5A00_0000 | PLLB_HOLDARM_MASK)
            .unwrap();
        step(&mut m, &mut arm, 2_000);
        m.clockman.write(PLLB, Width::Word, 0x5A00_0000).unwrap();
        step(&mut m, &mut arm, 50);
        arm.pause(&mut m);
        done.store(true, Ordering::SeqCst);
        assert_eq!(arm.stopped, None);
        assert_eq!(arm.cores.len(), super::CORES);
    }

    #[test]
    fn exclusive_increments_from_every_core_are_all_counted() {
        assert!(super::probe());
    }

    #[test]
    fn sev_wakes_cores_asleep_in_wfe() {
        // Core 0 spins a while and signals; the others wait. Everyone then
        // stores a flag at 0x800 + 8 * id.
        let mut m = machine_with(&[
            0xD538_00A0,
            0x9240_1C00,
            0xB500_00C0,
            0xD290_0004,
            0xD100_0484,
            0xB5FF_FFE4,
            0xD503_209F,
            0x1400_0002,
            0xD503_205F,
            0xD37D_F005,
            0x9120_00A5,
            0xD280_0026,
            0xF900_00A6,
            0x1400_0000,
        ]);
        let mut arm = ArmSide::new();
        drive(&mut m, &mut arm, |m| {
            (0..4).all(|id| word(m, 0x800 + 8 * id) != 0)
        });
    }
}
