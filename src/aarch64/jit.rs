//! Straight runs compiled to host code with Cranelift.
//!
//! A compiled run does what [`RunOp::go`] does from the run's first
//! instruction, and stops where it would stop: register-only instructions and
//! the conditions of branches run inline, with the general registers and
//! `NZCV` kept in host registers between them; everything else calls back
//! into the instruction's handler. A load or store that leaves something for
//! [`super::predecode::RunBus`] to report stops the run as it does there,
//! except a plain store into RAM, which the run goes on past the way
//! `Straight::run` would after it.
//!
//! Nothing is compiled until a run has been entered [`HOT`] times, and the
//! code of every run goes when the runs themselves are dropped all at once.

use std::mem::offset_of;

use cranelift_codegen::ir::condcodes::{CondCode, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, InstBuilder, MemFlagsData, SigRef, Signature, Type, Value,
};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, Module};
use std::sync::mpsc::{channel, Receiver, Sender};

use super::cpu::Cpu;
use super::exec::COND_HOLDS;
use super::mmu::HOST_LAYOUT;
use super::predecode::{handler_of, Kind, Op, RunBus, RunOp};

/// Entries before a run is compiled; `PIMU_JIT_HOT` overrides it.
const HOT: u32 = 64;

/// What a compiled run needs besides the core and the bus.
#[repr(C)]
pub struct Ctx {
    /// The write generation of the run's page when it was entered.
    pub gen: u64,
    /// Another core holds an exclusive mark, which a store may clear.
    pub marked: u64,
    /// The run's page's write generation: a store into that page stops the
    /// run.
    pub genp: *const u64,
    /// The cycle a branch may go on into the next translation before
    /// ([`Shared`]); 0 for none.
    pub chain_until: u64,
}

/// `(cpu, bus, ctx, pc, cycles, out)`; `out` gets the PC and the cycle of
/// the instruction the run stopped at, as [`RunOp::go`] returns them.
type Code = unsafe extern "C" fn(*mut Cpu, *mut u8, *const Ctx, u64, u64, *mut u64);

/// [`Code`]'s arguments and then the code, which takes them by the
/// tail-call convention.
type Enter = unsafe extern "C" fn(*mut Cpu, *mut u8, *const Ctx, u64, u64, *mut u64, usize);

const NEVER: usize = 1;
const PENDING: usize = 2;

/// A run to compile: its instructions, each with where its [`RunOp`] is,
/// which the code refers to but the compiler never reads.
struct Request {
    epoch: u64,
    first: usize,
    ops: Vec<(Op, usize)>,
    /// For [`Shared`]: the run's exits go through [`Compiler::stub`].
    chain: bool,
}

/// A run compiled: the code, or 0 when it could not be.
struct Done {
    epoch: u64,
    first: usize,
    code: usize,
    inline: u64,
    called: u64,
    ns: u64,
}

/// Runs are compiled on a thread of their own, so a core never waits for
/// Cranelift: a hot run goes on in the interpreter until its code is in.
/// `PIMU_JIT_SYNC=1` compiles in line instead.
pub struct Jit {
    to: Option<Sender<Request>>,
    from: Option<Receiver<Done>>,
    /// The compiler when it runs in line.
    here: Option<Compiler>,
    /// Requests out, and entries since the last look for answers.
    out: u32,
    since: u32,
    /// Runs compiled, and of their instructions how many run inline.
    pub runs: u64,
    pub inline: u64,
    pub called: u64,
    /// Host time spent compiling, and entries into compiled runs.
    pub compile_ns: u64,
    pub entries: u64,
    /// Instructions retired inside compiled code.
    pub retired: u64,
    hot: u32,
}

impl Jit {
    /// `None` where the host has no Cranelift backend, or `PIMU_NO_JIT` is set.
    pub fn new<M: RunBus + 'static>() -> Option<Jit> {
        if std::env::var_os("PIMU_NO_JIT").is_some() {
            return None;
        }
        let hot = std::env::var("PIMU_JIT_HOT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(HOT);
        let mut jit = Jit {
            to: None,
            from: None,
            here: None,
            out: 0,
            since: 0,
            runs: 0,
            inline: 0,
            called: 0,
            compile_ns: 0,
            entries: 0,
            retired: 0,
            hot,
        };
        if std::env::var_os("PIMU_JIT_SYNC").is_some() {
            jit.here = Some(Compiler::new::<M>()?);
            return Some(jit);
        }
        let (to, requests) = channel::<Request>();
        let (answers, from) = channel::<Done>();
        let (ready, started) = channel::<bool>();
        std::thread::Builder::new()
            .name("pimu-jit".into())
            .spawn(move || {
                let Some(mut compiler) = Compiler::new::<M>() else {
                    let _ = ready.send(false);
                    return;
                };
                let _ = ready.send(true);
                while let Ok(r) = requests.recv() {
                    if answers.send(compiler.compile(&r)).is_err() {
                        break;
                    }
                }
                // The cores that ran this code are gone with the channel.
                unsafe { compiler.module.free_memory() };
            })
            .ok()?;
        if !started.recv().unwrap_or(false) {
            return None;
        }
        jit.to = Some(to);
        jit.from = Some(from);
        Some(jit)
    }

    /// Run the run that starts at `first`, at `pc`, `c` cycles in, if it is
    /// compiled: what [`RunOp::go`] would return. `ops` is the whole run when
    /// it was looked up, `None` when a branch chained to it, which can run
    /// code compiled before but not ask for it. `epoch` is the runs'
    /// (`src/arm/runs.rs`): when it moves, every run was dropped.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub fn go<M: RunBus>(
        &mut self,
        first: &RunOp,
        ops: Option<&[RunOp]>,
        epoch: u64,
        cpu: &mut Cpu,
        bus: &mut M,
        ctx: &Ctx,
        pc: u64,
        c: u64,
    ) -> Option<(u64, u64)> {
        if self.out > 0 {
            self.since += 1;
            if self.since >= 256 {
                self.since = 0;
                self.collect(epoch);
            }
        }
        let code = first.jit.get();
        if code <= PENDING {
            if code == 0 {
                let hits = first.hits.get() + 1;
                first.hits.set(hits);
                if hits >= self.hot {
                    if let Some(ops) = ops {
                        self.ask(first, ops, epoch);
                    }
                }
            }
            return None;
        }
        if code == NEVER {
            return None;
        }
        self.entries += 1;
        let mut out = [0u64; 2];
        // Compiled for bus `M` from the run at `first`, whose instructions
        // stay put while the runs' epoch does.
        unsafe {
            let code: Code = std::mem::transmute(code);
            code(cpu, bus as *mut M as *mut u8, ctx, pc, c, out.as_mut_ptr());
        }
        self.retired += out[1] - c;
        Some((out[0], out[1]))
    }

    #[inline(never)]
    fn ask(&mut self, first: &RunOp, ops: &[RunOp], epoch: u64) {
        let r = Request {
            epoch,
            first: first as *const RunOp as usize,
            ops: ops
                .iter()
                .map(|o| (o.op, o as *const RunOp as usize))
                .collect(),
            chain: false,
        };
        if let Some(c) = &mut self.here {
            let done = c.compile(&r);
            self.install(done, epoch);
            return;
        }
        first.jit.set(PENDING);
        if self.to.as_ref().is_some_and(|to| to.send(r).is_ok()) {
            self.out += 1;
        } else {
            first.jit.set(NEVER);
        }
    }

    #[inline(never)]
    fn collect(&mut self, epoch: u64) {
        while let Some(done) = self.from.as_ref().and_then(|f| f.try_recv().ok()) {
            self.out -= 1;
            self.install(done, epoch);
        }
    }

    fn install(&mut self, done: Done, epoch: u64) {
        self.compile_ns += done.ns;
        // A run of an older epoch is gone, and its slot with it.
        if done.epoch != epoch {
            return;
        }
        let first = unsafe { &*(done.first as *const RunOp) };
        first
            .jit
            .set(if done.code == 0 { NEVER } else { done.code });
        if done.code != 0 {
            self.runs += 1;
            self.inline += done.inline;
            self.called += done.called;
        }
    }
}

/// Straight runs compiled once for every core, keyed by the physical address
/// of their first instruction: the cores on threads share them, so a run hot
/// on four cores is compiled once. The code takes the PC as an argument, so
/// one translation serves every address the page is mapped at.
///
/// A translation holds its own copy of the run's instructions, which the code
/// and the stops it reports refer to, and the write generation of its page
/// when it was decoded: a store into the page moves the generation, and the
/// translation is not used again. A translation is never freed while the
/// table lives, since a core may be inside it: one replaced is let go.
pub struct Shared {
    table: std::sync::Arc<Table>,
    to: Sender<Wanted>,
    hot: u32,
}

struct Table {
    slots: Box<[std::sync::atomic::AtomicPtr<Translation>]>,
    /// What [`Translation::go`] enters a translation through: it calls the
    /// code by the tail-call convention, which the translations chain by.
    enter: std::sync::atomic::AtomicUsize,
    runs: std::sync::atomic::AtomicU64,
    compile_ns: std::sync::atomic::AtomicU64,
}

const SHARED_SLOTS: usize = 1 << 18;

/// One run's code, for bus `M` of [`Shared::new`].
#[repr(C)]
pub struct Translation {
    pa: u64,
    gen: u64,
    code: usize,
    /// The instructions the code refers to.
    _ops: Box<[RunOp]>,
    pub len: u64,
}

// Nothing writes a translation's instructions once it is in the table.
unsafe impl Sync for Translation {}
unsafe impl Send for Translation {}

struct Wanted {
    pa: u64,
    gen: u64,
    ops: Vec<Op>,
}

fn shared_slot(pa: u64) -> usize {
    ((pa >> 2) ^ (pa >> 20)) as usize & (SHARED_SLOTS - 1)
}

impl Shared {
    /// `None` where the host has no Cranelift backend, or `PIMU_NO_JIT` is set.
    pub fn new<M: RunBus + 'static>() -> Option<Shared> {
        if std::env::var_os("PIMU_NO_JIT").is_some() {
            return None;
        }
        let hot = std::env::var("PIMU_JIT_HOT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(SHARED_HOT);
        let table = std::sync::Arc::new(Table {
            slots: (0..SHARED_SLOTS)
                .map(|_| std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()))
                .collect(),
            enter: Default::default(),
            runs: Default::default(),
            compile_ns: Default::default(),
        });
        let (to, wanted) = channel::<Wanted>();
        let wanted = std::sync::Arc::new(std::sync::Mutex::new(wanted));
        let (ready, started) = channel::<bool>();
        let threads = std::env::var("PIMU_JIT_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(SHARED_THREADS);
        for _ in 0..threads {
            let (t, wanted, ready) = (table.clone(), wanted.clone(), ready.clone());
            std::thread::Builder::new()
                .name("pimu-jit".into())
                .spawn(move || {
                    let slots = t.slots.as_ptr() as usize;
                    let enter = Compiler::new::<M>().and_then(|mut c| {
                        c.stub = c.stub(slots)?;
                        Some((c.trampoline()?, c))
                    });
                    let Some((enter, mut compiler)) = enter else {
                        let _ = ready.send(false);
                        return;
                    };
                    let _ = t.enter.compare_exchange(
                        0,
                        enter,
                        std::sync::atomic::Ordering::AcqRel,
                        std::sync::atomic::Ordering::Acquire,
                    );
                    let _ = ready.send(true);
                    loop {
                        let w = wanted.lock().ok().and_then(|w| w.recv().ok());
                        let Some(w) = w else {
                            break;
                        };
                        t.compile::<M>(&mut compiler, w);
                    }
                    // The cores that ran this code are gone with the channel.
                    unsafe { compiler.module.free_memory() };
                })
                .ok()?;
        }
        for _ in 0..threads {
            if !started.recv().unwrap_or(false) {
                return None;
            }
        }
        Some(Shared { table, to, hot })
    }

    /// What [`Translation::go`] needs of the table.
    #[inline(always)]
    pub fn enter(&self) -> usize {
        self.table.enter.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The translation of the run at `pa`, decoded at its page's current
    /// generation `gen`.
    #[inline(always)]
    pub fn find(&self, pa: u64, gen: u64) -> Option<&Translation> {
        let t = self.table.slots[shared_slot(pa)].load(std::sync::atomic::Ordering::Acquire);
        // A translation in the table is never freed while the table lives.
        let t = unsafe { t.as_ref() }?;
        (t.pa == pa && t.gen == gen).then_some(t)
    }

    /// The run `ops`, at `pa`, was entered by the interpreter: ask for it
    /// to be compiled once it has been often enough.
    #[inline(always)]
    pub fn entered(&self, ops: &[RunOp], pa: u64, gen: u64) {
        let Some(first) = ops.first() else {
            return;
        };
        let hits = first.hits.get();
        if hits >= self.hot {
            return;
        }
        first.hits.set(hits + 1);
        if hits + 1 == self.hot {
            self.want(ops, pa, gen);
        }
    }

    #[inline(never)]
    fn want(&self, ops: &[RunOp], pa: u64, gen: u64) {
        let ops = ops.iter().map(|o| o.op).collect();
        let _ = self.to.send(Wanted { pa, gen, ops });
    }

    /// Runs compiled, and the host time they took.
    pub fn compiled(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.table.runs.load(Relaxed),
            self.table.compile_ns.load(Relaxed),
        )
    }
}

impl Translation {
    /// Run it at `pc`, `c` cycles in: what [`RunOp::go`] would return, with
    /// the stop told to `bus`, which is the `M` it was compiled for.
    /// `enter` is the table's ([`Shared::enter`]); from here the code may
    /// go on into other translations while `ctx.chain_until` lets it.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub fn go<M: RunBus>(
        &self,
        enter: usize,
        cpu: &mut Cpu,
        bus: &mut M,
        ctx: &mut Ctx,
        pc: u64,
        c: u64,
    ) -> (u64, u64) {
        let mut out = [0u64; 2];
        // Compiled for bus `M`, from instructions that stay put with it,
        // and entered through the table's trampoline.
        unsafe {
            let enter: Enter = std::mem::transmute(enter);
            enter(
                cpu,
                bus as *mut M as *mut u8,
                ctx,
                pc,
                c,
                out.as_mut_ptr(),
                self.code,
            );
        }
        (out[0], out[1])
    }
}

impl Table {
    fn compile<M: RunBus>(&self, compiler: &mut Compiler, w: Wanted) {
        use std::sync::atomic::Ordering;
        let slot = &self.slots[shared_slot(w.pa)];
        if let Some(t) = unsafe { slot.load(Ordering::Acquire).as_ref() } {
            if t.pa == w.pa && t.gen == w.gen {
                return;
            }
        }
        let n = w.ops.len();
        let ops: Box<[RunOp]> = w
            .ops
            .iter()
            .enumerate()
            .map(|(i, &op)| RunOp::new::<M>(op, i + 1 == n))
            .collect();
        let r = Request {
            epoch: 0,
            first: ops.as_ptr() as usize,
            ops: ops
                .iter()
                .map(|o| (o.op, o as *const RunOp as usize))
                .collect(),
            chain: true,
        };
        let done = compiler.compile(&r);
        self.compile_ns.fetch_add(done.ns, Ordering::Relaxed);
        if done.code == 0 {
            return;
        }
        self.runs.fetch_add(1, Ordering::Relaxed);
        let t = Box::into_raw(Box::new(Translation {
            pa: w.pa,
            gen: w.gen,
            code: done.code,
            len: n as u64,
            _ops: ops,
        }));
        // The one it replaces may still be running on a core: let it go.
        slot.store(t, Ordering::Release);
    }
}

/// Threads compiling for [`Shared`]; `PIMU_JIT_THREADS` overrides it.
const SHARED_THREADS: usize = 3;

/// Entries on one core before a run is compiled for the cores on threads;
/// `PIMU_JIT_HOT` overrides it.
const SHARED_HOT: u32 = 256;

/// Cranelift and what it compiles into, on the thread that compiles.
struct Compiler {
    module: JITModule,
    /// `PIMU_JIT_PERF_MAP=1`: `/tmp/perf-<pid>.map`, which `perf report`
    /// reads to name compiled code.
    perf_map: Option<std::fs::File>,
    fctx: FunctionBuilderContext,
    epoch: u64,
    helpers: Helpers,
    /// The chain stub of [`Self::stub`], once made.
    stub: usize,
}

impl Compiler {
    fn new<M: RunBus>() -> Option<Compiler> {
        let perf_map = std::env::var_os("PIMU_JIT_PERF_MAP").and_then(|_| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(format!("/tmp/perf-{}.map", std::process::id()))
                .ok()
        });
        Some(Compiler {
            perf_map,
            module: module()?,
            fctx: FunctionBuilderContext::new(),
            epoch: 0,
            helpers: Helpers::of::<M>(),
            stub: 0,
        })
    }

    fn compile(&mut self, r: &Request) -> Done {
        let t = std::time::Instant::now();
        let (code, inline, called) = self.build(r).unwrap_or((0, 0, 0));
        Done {
            epoch: r.epoch,
            first: r.first,
            code,
            inline,
            called,
            ns: t.elapsed().as_nanos() as u64,
        }
    }

    fn build(&mut self, r: &Request) -> Option<(usize, u64, u64)> {
        if r.epoch != self.epoch {
            // The runs the old code was for are gone, and the core that ran
            // them asks for this epoch's only after they went.
            let fresh = module()?;
            let old = std::mem::replace(&mut self.module, fresh);
            unsafe { old.free_memory() };
            self.epoch = r.epoch;
        }
        let ptr = self.module.target_config().pointer_type();
        let call_conv = self.module.target_config().default_call_conv;
        let mut ctx = self.module.make_context();
        let own = if r.chain { CallConv::Tail } else { call_conv };
        ctx.func.signature = signature(own, &[ptr, ptr, ptr, types::I64, types::I64, ptr], None);
        let config = self.module.target_config();
        let (inline, called) = {
            let b = FunctionBuilder::new(&mut ctx.func, &mut self.fctx);
            let mut l = Lower::new(b, ptr, call_conv, &self.helpers);
            if r.chain {
                let i = types::I64;
                let sig = l.b.import_signature(signature(
                    CallConv::Tail,
                    &[ptr, ptr, ptr, i, i, ptr, ptr, i, i],
                    None,
                ));
                l.chain = Some((self.stub as i64, sig));
            }
            l.run(&r.ops);
            let counts = (l.inline, l.called);
            l.b.finalize(config);
            counts
        };
        let id = self
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .ok()?;
        let dump = dump_wanted();
        ctx.set_disasm(dump);
        self.module.define_function(id, &mut ctx).ok()?;
        let size = ctx.compiled_code().map_or(0, |c| c.code_info().total_size);
        if dump {
            let ops: Vec<_> = r
                .ops
                .iter()
                .map(|(o, _)| format!("{:08x}", o.insn))
                .collect();
            if let Some(vcode) = ctx.compiled_code().and_then(|c| c.vcode.as_ref()) {
                eprintln!(
                    "PIMU_JIT_DUMP {} insns [{}], {size} bytes:\n{vcode}",
                    ops.len(),
                    ops.join(" ")
                );
            }
        }
        self.module.clear_context(&mut ctx);
        self.module.finalize_definitions().ok()?;
        let code = self.module.get_finalized_function(id) as usize;
        if let Some(map) = &mut self.perf_map {
            use std::io::Write;
            let _ = writeln!(map, "{code:x} {size:x} jit_run_{:x}", r.first);
        }
        Some((code, inline, called))
    }
}

impl Compiler {
    /// The chain stub every exit of a [`Shared`] translation tail-calls:
    /// `(cpu, bus, ctx, target, c, out, o, pc, c0)`, after the branch `o` at
    /// `pc`, `c0` cycles in, which took the core to `target` for cycle `c`.
    /// While [`Ctx::chain_until`] lets it, it looks the target up in the
    /// core's code pages ([`Tlb::code_page`](super::mmu::Tlb)) and in the
    /// table at `slots`, and goes on into the translation there if its
    /// page is as it was; else it tells the bus the run stopped at `o`.
    fn stub(&mut self, slots: usize) -> Option<usize> {
        let ptr = self.module.target_config().pointer_type();
        let call_conv = self.module.target_config().default_call_conv;
        let i = types::I64;
        let mut ctx = self.module.make_context();
        ctx.func.signature =
            signature(CallConv::Tail, &[ptr, ptr, ptr, i, i, ptr, ptr, i, i], None);
        let config = self.module.target_config();
        let take = self.helpers.take;
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut self.fctx);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            b.seal_block(entry);
            let p = b.block_params(entry).to_vec();
            let (cpu, bus, cx, target, c, out, o, pc, c0) =
                (p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8]);
            let tail =
                b.import_signature(signature(CallConv::Tail, &[ptr, ptr, ptr, i, i, ptr], None));
            let take_sig =
                b.import_signature(signature(call_conv, &[ptr, ptr, ptr, i, i, i, ptr], None));
            let l = &super::mmu::CODE_LAYOUT;
            let (look, look2, check, go, fall) = (
                b.create_block(),
                b.create_block(),
                b.create_block(),
                b.create_block(),
                b.create_block(),
            );
            let until = b
                .ins()
                .load(i, FLAGS, cx, offset_of!(Ctx, chain_until) as i32);
            let room = b.ins().icmp(IntCC::UnsignedLessThan, c, until);
            let low = b.ins().band_imm_s(target, 3);
            let aligned = b.ins().icmp_imm_s(IntCC::Equal, low, 0);
            let go_on = b.ins().band(room, aligned);
            b.ins().brif(go_on, look, &[], fall, &[]);
            b.switch_to_block(look);
            b.seal_block(look);
            let el = b
                .ins()
                .load(types::I32, FLAGS, cpu, offset_of!(Cpu, el) as i32);
            let el = b.ins().uextend(i, el);
            let page = b.ins().ushr_imm_s(target, 12);
            let mix = b.ins().ushr_imm_s(page, 4);
            let idx = b.ins().bxor(page, mix);
            let idx = b.ins().band_imm_s(idx, (l.pages - 1) as i64);
            let idx = b.ins().imul_imm_s(idx, l.size as i64);
            let base = b
                .ins()
                .iadd_imm_s(cpu, (offset_of!(Cpu, tlb) + l.code) as i64);
            let entry = b.ins().iadd(base, idx);
            let tag = b.ins().load(i, FLAGS, entry, l.tag as i32);
            let va_page = b.ins().band_imm_s(target, !0xFFF);
            let want = b.ins().bor(va_page, el);
            let hit = b.ins().icmp(IntCC::Equal, tag, want);
            b.ins().brif(hit, look2, &[], fall, &[]);
            b.switch_to_block(look2);
            b.seal_block(look2);
            let pa_page = b.ins().load(i, FLAGS, entry, l.pa as i32);
            let genp = b.ins().load(ptr, FLAGS, entry, l.gen as i32);
            let off = b.ins().band_imm_s(target, 0xFFF);
            let pa = b.ins().bor(pa_page, off);
            let h1 = b.ins().ushr_imm_s(pa, 2);
            let h2 = b.ins().ushr_imm_s(pa, 20);
            let h = b.ins().bxor(h1, h2);
            let h = b.ins().band_imm_s(h, (SHARED_SLOTS - 1) as i64);
            let h = b.ins().ishl_imm_s(h, 3);
            let slot = b.ins().iadd_imm_s(h, slots as i64);
            let t = b.ins().atomic_load(ptr, FLAGS, slot);
            b.ins().brif(t, check, &[], fall, &[]);
            b.switch_to_block(check);
            b.seal_block(check);
            let tpa = b
                .ins()
                .load(i, FLAGS, t, offset_of!(Translation, pa) as i32);
            let same = b.ins().icmp(IntCC::Equal, tpa, pa);
            let tgen = b
                .ins()
                .load(i, FLAGS, t, offset_of!(Translation, gen) as i32);
            let gen = b.ins().load(i, FLAGS, genp, 0);
            let fresh = b.ins().icmp(IntCC::Equal, tgen, gen);
            let ok = b.ins().band(same, fresh);
            b.ins().brif(ok, go, &[], fall, &[]);
            b.switch_to_block(go);
            b.seal_block(go);
            b.ins().store(FLAGS, tgen, cx, offset_of!(Ctx, gen) as i32);
            b.ins().store(FLAGS, genp, cx, offset_of!(Ctx, genp) as i32);
            let code = b
                .ins()
                .load(ptr, FLAGS, t, offset_of!(Translation, code) as i32);
            b.ins()
                .return_call_indirect(tail, code, &[cpu, bus, cx, target, c, out]);
            b.switch_to_block(fall);
            b.seal_block(fall);
            let f = b.ins().iconst(ptr, take);
            b.ins()
                .call_indirect(take_sig, f, &[cpu, bus, o, pc, target, c0, out]);
            b.ins().return_(&[]);
            b.finalize(config);
        }
        let id = self
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .ok()?;
        self.module.define_function(id, &mut ctx).ok()?;
        self.module.clear_context(&mut ctx);
        self.module.finalize_definitions().ok()?;
        Some(self.module.get_finalized_function(id) as usize)
    }
}

impl Compiler {
    /// [`Enter`]: calls the code it is given, by the tail-call convention.
    fn trampoline(&mut self) -> Option<usize> {
        let ptr = self.module.target_config().pointer_type();
        let call_conv = self.module.target_config().default_call_conv;
        let args = [ptr, ptr, ptr, types::I64, types::I64, ptr];
        let mut ctx = self.module.make_context();
        ctx.func.signature = signature(call_conv, &[&args[..], &[ptr]].concat(), None);
        let config = self.module.target_config();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut self.fctx);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            b.seal_block(entry);
            let p = b.block_params(entry).to_vec();
            let sig = b.import_signature(signature(CallConv::Tail, &args, None));
            b.ins().call_indirect(sig, p[6], &p[..6]);
            b.ins().return_(&[]);
            b.finalize(config);
        }
        let id = self
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .ok()?;
        self.module.define_function(id, &mut ctx).ok()?;
        self.module.clear_context(&mut ctx);
        self.module.finalize_definitions().ok()?;
        Some(self.module.get_finalized_function(id) as usize)
    }
}

/// `PIMU_JIT_DUMP=<n>`: the disassembly of the first `n` runs compiled.
fn dump_wanted() -> bool {
    static LEFT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);
    use std::sync::atomic::Ordering::Relaxed;
    if LEFT.load(Relaxed) == -1 {
        let n = std::env::var("PIMU_JIT_DUMP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let _ = LEFT.compare_exchange(-1, n, Relaxed, Relaxed);
    }
    LEFT.fetch_sub(1, Relaxed) > 0
}

/// The helpers compiled code calls, for one bus type.
struct Helpers {
    step: fn(Kind) -> Step,
    take: i64,
    last: i64,
}

impl Helpers {
    fn of<M: RunBus>() -> Helpers {
        Helpers {
            step: step_of::<M>,
            take: take::<M> as *const () as usize as i64,
            last: last::<M> as *const () as usize as i64,
        }
    }
}

fn module() -> Option<JITModule> {
    let mut flags = settings::builder();
    let opt = std::env::var("PIMU_JIT_OPT").unwrap_or_else(|_| "speed".into());
    flags.set("opt_level", &opt).ok()?;
    flags.set("is_pic", "false").ok()?;
    flags.set("enable_verifier", "false").ok()?;
    // The translations of `Shared` tail-call each other, which Cranelift
    // builds on frame pointers.
    flags.set("preserve_frame_pointers", "true").ok()?;
    let isa = cranelift_native::builder()
        .ok()?
        .finish(settings::Flags::new(flags))
        .ok()?;
    Some(JITModule::new(JITBuilder::with_isa(
        isa,
        default_libcall_names(),
    )))
}

fn signature(call_conv: CallConv, params: &[Type], ret: Option<Type>) -> Signature {
    let mut sig = Signature::new(call_conv);
    sig.params.extend(params.iter().map(|&t| AbiParam::new(t)));
    sig.returns.extend(ret.map(AbiParam::new));
    sig
}

type Step =
    unsafe extern "C" fn(*mut Cpu, *mut u8, *const Ctx, *const RunOp, u64, u64, *mut u64) -> u32;

/// [`step`] for instructions of `kind`.
fn step_of<M: RunBus>(kind: Kind) -> Step {
    macro_rules! steps {
        ($($k:ident),*) => {
            match kind {
                $(Kind::$k => {
                    let f: unsafe extern "C" fn(*mut Cpu, *mut M, *const Ctx, *const RunOp, u64, u64, *mut u64) -> u32 =
                        step::<M, { Kind::$k as u8 }>;
                    // The bus pointer is an `M`'s either way.
                    unsafe {
                        std::mem::transmute::<
                            unsafe extern "C" fn(*mut Cpu, *mut M, *const Ctx, *const RunOp, u64, u64, *mut u64) -> u32,
                            Step,
                        >(f)
                    }
                })*
            }
        };
    }
    steps!(
        Execute, BranchReg, B, Bl, BCond, Cbz, Tbz, LdrX, LdrW, LdrH, LdrB, StrX, StrW, StrH, StrB,
        Barrier, LdStReg, LdStPair, LoadUimm, StoreUimm, LdStRegOff, LdStImm, Pair, DpImm, DpReg,
        Adr, Adrp, Nop, CmpImm, CmpImmW, CmpReg, CmpRegW, AddImmR, SubImmR, MovReg, AddImm, SubImm,
        AddsImm, SubsImm, MovConst, MovK, LogicImm, LogicReg, AddSubReg, Bitfield, Ccmp, Csel,
        Madd
    )
}

/// One instruction of kind `K` the compiled code hands back: what `go_kind`
/// does with it, without going on to the next. 0 to go on, else the run
/// stopped and `out` has where.
unsafe extern "C" fn step<M: RunBus, const K: u8>(
    cpu: *mut Cpu,
    bus: *mut M,
    ctx: *const Ctx,
    o: *const RunOp,
    pc: u64,
    c: u64,
    out: *mut u64,
) -> u32 {
    let (cpu, bus, ctx, o) = (&mut *cpu, &mut *bus, &*ctx, &*o);
    let op = &o.op;
    // `K` is one of `Kind`'s, from `step_of`.
    let kind = std::mem::transmute::<u8, Kind>(K);
    let mem = (Kind::LdrX as u8..=Kind::Pair as u8).contains(&K);
    if mem {
        bus.at(c);
    }
    let next = handler_of::<M>(kind)(cpu, op, bus, pc);
    if !cpu.faulted() && (!mem || bus.settle(ctx.gen, ctx.genp, ctx.marked != 0)) {
        return 0;
    }
    bus.stop(o, next, true);
    *out = pc;
    *out.add(1) = c;
    1
}

/// The branch `o` at `pc` taken to `next`: back to `Straight::run`, which
/// goes on into the run there, compiled too if it is.
unsafe extern "C" fn take<M: RunBus>(
    _: *mut Cpu,
    bus: *mut M,
    o: *const RunOp,
    pc: u64,
    next: u64,
    c: u64,
    out: *mut u64,
) {
    (*bus).stop(&*o, next, true);
    *out = pc;
    *out.add(1) = c;
}

/// The last instruction of the run, as the interpreter runs it.
unsafe extern "C" fn last<M: RunBus>(
    cpu: *mut Cpu,
    bus: *mut M,
    o: *const RunOp,
    pc: u64,
    c: u64,
    out: *mut u64,
) {
    let (pc, c) = (*o).go(&mut *cpu, &mut *bus, pc, c);
    *out = pc;
    *out.add(1) = c;
}

const FLAGS: MemFlagsData = MemFlagsData::trusted();
/// Guest memory: any alignment, little-endian like the A72's data accesses.
const GUEST: MemFlagsData = MemFlagsData::new()
    .with_notrap()
    .with_endianness(cranelift_codegen::ir::Endianness::Little);

/// `NZCV` as the last instruction to set it left it: packed, or still as
/// the operands of a subtraction or the result of a logical operation, so
/// that the condition a branch tests is one compare.
#[derive(Clone, Copy)]
enum Flags {
    Packed(Value),
    /// `x - y` at the width `sf` says, both already masked to it.
    Sub {
        x: Value,
        y: Value,
        sf: bool,
    },
    /// The result of an `ANDS`, `TST` and the like: C and V clear.
    Logic {
        r: Value,
        sf: bool,
    },
}

struct Lower<'a> {
    b: FunctionBuilder<'a>,
    ptr: Type,
    cpu: Value,
    bus: Value,
    ctx: Value,
    pc0: Value,
    c0: Value,
    out: Value,
    /// A general register's value and whether it is newer than the core's.
    regs: [Option<(Value, bool)>; 31],
    nzcv: Option<(Flags, bool)>,
    step: (SigRef, i64),
    step_of: fn(Kind) -> Step,
    /// The core's EL, where `SP` is, and the run's [`Ctx`] fields, once
    /// loaded.
    el: Option<Value>,
    sp_at: Option<Value>,
    sp: Option<(Value, bool)>,
    marked: Option<Value>,
    genp: Option<Value>,
    take: (SigRef, i64),
    last: (SigRef, i64),
    /// For [`Shared`]: the chain stub ([`Compiler::stub`]) every exit
    /// goes through, and its signature.
    chain: Option<(i64, SigRef)>,
    ret: Option<cranelift_codegen::ir::Block>,
    inline: u64,
    called: u64,
}

impl<'a> Lower<'a> {
    fn new(mut b: FunctionBuilder<'a>, ptr: Type, call_conv: CallConv, h: &Helpers) -> Lower<'a> {
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let p = b.block_params(entry).to_vec();
        let (pt, i) = (ptr, types::I64);
        let step = b.import_signature(signature(
            call_conv,
            &[pt, pt, pt, pt, i, i, pt],
            Some(types::I32),
        ));
        let take = b.import_signature(signature(call_conv, &[pt, pt, pt, i, i, i, pt], None));
        let last = b.import_signature(signature(call_conv, &[pt, pt, pt, i, i, pt], None));
        Lower {
            b,
            ptr,
            cpu: p[0],
            bus: p[1],
            ctx: p[2],
            pc0: p[3],
            c0: p[4],
            out: p[5],
            regs: [None; 31],
            nzcv: None,
            step: (step, 0),
            step_of: h.step,
            el: None,
            sp_at: None,
            sp: None,
            marked: None,
            genp: None,
            take: (take, h.take),
            last: (last, h.last),
            chain: None,
            ret: None,
            inline: 0,
            called: 0,
        }
    }

    /// The block a stop returns through: one for the whole run, so its
    /// epilogue is there once.
    fn ret(&mut self) -> cranelift_codegen::ir::Block {
        *self.ret.get_or_insert_with(|| self.b.create_block())
    }

    fn end(&mut self) {
        if let Some(ret) = self.ret.take() {
            self.b.switch_to_block(ret);
            self.b.seal_block(ret);
            self.b.ins().return_(&[]);
        }
    }

    fn run(&mut self, ops: &[(Op, usize)]) {
        for (i, (op, addr)) in ops.iter().enumerate() {
            let pc = self.b.ins().iadd_imm_s(self.pc0, 4 * i as i64);
            let c = self.b.ins().iadd_imm_s(self.c0, i as i64);
            let at = self.b.ins().iconst(self.ptr, *addr as i64);
            if i + 1 == ops.len() {
                if self.chain_last(op, pc, c, at) {
                    self.end();
                    return;
                }
                self.store_dirty();
                let (sig, f) = self.last;
                let f = self.b.ins().iconst(self.ptr, f);
                let args = [self.cpu, self.bus, at, pc, c, self.out];
                self.b.ins().call_indirect(sig, f, &args);
                self.b.ins().return_(&[]);
                self.called += 1;
                self.end();
                return;
            }
            if op.cond_branch() {
                let t = self.branch_taken(op);
                let target = self.b.ins().iadd_imm_s(pc, op.imm as i64);
                let (away, on) = (self.b.create_block(), self.b.create_block());
                self.b.ins().brif(t, away, &[], on, &[]);
                self.b.switch_to_block(away);
                self.b.seal_block(away);
                self.store_dirty();
                if self.chain.is_some() {
                    self.exit(target, at, pc, c);
                } else {
                    let (sig, f) = self.take;
                    let f = self.b.ins().iconst(self.ptr, f);
                    let args = [self.cpu, self.bus, at, pc, target, c, self.out];
                    self.b.ins().call_indirect(sig, f, &args);
                    self.b.ins().return_(&[]);
                }
                self.b.switch_to_block(on);
                self.b.seal_block(on);
                self.inline += 1;
                continue;
            }
            if self.inline_op(op, pc) {
                self.inline += 1;
                continue;
            }
            if self.inline_mem(op, at, pc, c) {
                self.inline += 1;
                continue;
            }
            self.store_dirty();
            self.regs.fill(None);
            self.nzcv = None;
            self.sp = None;
            let sig = self.step.0;
            let f = (self.step_of)(op.kind) as usize as i64;
            let f = self.b.ins().iconst(self.ptr, f);
            let args = [self.cpu, self.bus, self.ctx, at, pc, c, self.out];
            let call = self.b.ins().call_indirect(sig, f, &args);
            let stopped = self.b.inst_results(call)[0];
            let (ret, on) = (self.ret(), self.b.create_block());
            self.b.ins().brif(stopped, ret, &[], on, &[]);
            self.b.switch_to_block(on);
            self.b.seal_block(on);
            self.called += 1;
        }
    }

    /// Leave the run through the chain stub, for `target` after the branch
    /// `at` at `pc`, `c` cycles in, which has run: the stub goes on into the
    /// translation there or tells the bus the run stopped. The registers
    /// are the core's by now.
    fn exit(&mut self, target: Value, at: Value, pc: Value, c: Value) {
        let Some((stub, sig)) = self.chain else {
            return;
        };
        let c1 = self.b.ins().iadd_imm_s(c, 1);
        let f = self.b.ins().iconst(self.ptr, stub);
        let args = [
            self.cpu, self.bus, self.ctx, target, c1, self.out, at, pc, c,
        ];
        self.b.ins().return_call_indirect(sig, f, &args);
    }

    /// The run's last instruction, `op` at `pc`, `c` cycles in and with its
    /// [`RunOp`] at `at`: a branch runs here and leaves through
    /// [`Self::exit`], whichever way it goes, `BR`, `BLR` and `RET` too.
    /// Whether it did; else the handler runs it.
    fn chain_last(&mut self, op: &Op, pc: Value, c: Value, at: Value) -> bool {
        if self.chain.is_none() {
            return false;
        }
        match op.kind {
            Kind::B | Kind::Bl => {
                let target = self.b.ins().iadd_imm_s(pc, op.imm as i64);
                if op.kind == Kind::Bl {
                    let link = self.b.ins().iadd_imm_s(pc, 4);
                    self.set_reg(30, link);
                }
                self.store_dirty();
                self.exit(target, at, pc, c);
            }
            Kind::BCond | Kind::Cbz | Kind::Tbz => {
                let t = self.branch_taken(op);
                self.store_dirty();
                let (away, on) = (self.b.create_block(), self.b.create_block());
                self.b.ins().brif(t, away, &[], on, &[]);
                self.b.switch_to_block(away);
                self.b.seal_block(away);
                let target = self.b.ins().iadd_imm_s(pc, op.imm as i64);
                self.exit(target, at, pc, c);
                self.b.switch_to_block(on);
                self.b.seal_block(on);
                let next = self.b.ins().iadd_imm_s(pc, 4);
                self.exit(next, at, pc, c);
            }
            Kind::BranchReg => {
                // As `exec::branch_reg` takes them; `ERET` and the rest are
                // the handler's.
                let insn = op.insn;
                let opc = (insn >> 21) & 0xF;
                let plain =
                    (insn >> 16) & 0x1F == 0x1F && (insn >> 10) & 0x3F == 0 && insn & 0x1F == 0;
                if !plain || opc > 2 {
                    return false;
                }
                let target = self.xr(((insn >> 5) & 0x1F) as u8, true);
                if opc == 1 {
                    let link = self.b.ins().iadd_imm_s(pc, 4);
                    self.set_reg(30, link);
                }
                self.store_dirty();
                self.exit(target, at, pc, c);
            }
            _ => return false,
        }
        self.called += 1;
        true
    }

    // ---- the core's registers ----------------------------------------------

    fn reg(&mut self, r: u8) -> Value {
        if let Some((v, _)) = self.regs[r as usize] {
            return v;
        }
        let off = (offset_of!(Cpu, x) + 8 * r as usize) as i32;
        let v = self.b.ins().load(types::I64, FLAGS, self.cpu, off);
        self.regs[r as usize] = Some((v, false));
        v
    }

    fn set_reg(&mut self, r: u8, v: Value) {
        self.regs[r as usize] = Some((v, true));
    }

    fn get_nzcv(&mut self) -> Value {
        match self.nzcv {
            Some((Flags::Packed(v), _)) => v,
            Some((f, dirty)) => {
                let v = self.pack_flags(f);
                self.nzcv = Some((Flags::Packed(v), dirty));
                v
            }
            None => {
                let v =
                    self.b
                        .ins()
                        .load(types::I32, FLAGS, self.cpu, offset_of!(Cpu, nzcv) as i32);
                self.nzcv = Some((Flags::Packed(v), false));
                v
            }
        }
    }

    fn set_nzcv(&mut self, v: Value) {
        self.nzcv = Some((Flags::Packed(v), true));
    }

    fn set_flags(&mut self, f: Flags) {
        self.nzcv = Some((f, true));
    }

    fn pack_flags(&mut self, f: Flags) -> Value {
        match f {
            Flags::Packed(v) => v,
            Flags::Sub { x, y, sf } => {
                let y = self.b.ins().bnot(y);
                let y = self.mask(y, sf);
                self.add_with_carry(x, y, true, sf).1
            }
            Flags::Logic { r, sf } => self.logic_flags(r, sf),
        }
    }

    /// `ConditionHolds(cond)` straight from what [`Flags`] has, or `None`
    /// where it needs the packed flags.
    fn cond_direct(&mut self, cond: u32) -> Option<Value> {
        let (f, _) = self.nzcv?;
        let b = &mut self.b;
        let narrow = |b: &mut FunctionBuilder, v: Value, sf: bool| {
            if sf {
                v
            } else {
                b.ins().ireduce(types::I32, v)
            }
        };
        let cc = match (f, cond >> 1) {
            (_, 7) => return Some(b.ins().iconst(types::I8, 1)),
            (Flags::Sub { .. }, 3) | (Flags::Packed(_), _) => return None,
            (Flags::Sub { x, y, sf }, k) => {
                let (cc, signed) = match k {
                    0 => (IntCC::Equal, false),
                    1 => (IntCC::UnsignedGreaterThanOrEqual, false),
                    2 => {
                        let d = b.ins().isub(x, y);
                        let d = narrow(b, d, sf);
                        let neg = b.ins().icmp_imm_s(IntCC::SignedLessThan, d, 0);
                        return Some(if cond & 1 == 0 {
                            neg
                        } else {
                            b.ins().bxor_imm_s(neg, 1)
                        });
                    }
                    4 => (IntCC::UnsignedGreaterThan, false),
                    5 => (IntCC::SignedGreaterThanOrEqual, true),
                    _ => (IntCC::SignedGreaterThan, true),
                };
                let (x, y) = if signed {
                    (narrow(b, x, sf), narrow(b, y, sf))
                } else {
                    (x, y)
                };
                let cc = if cond & 1 == 0 { cc } else { cc.complement() };
                return Some(b.ins().icmp(cc, x, y));
            }
            (Flags::Logic { r, sf }, k) => {
                let r = narrow(b, r, sf);
                let (cc, rhs) = match k {
                    0 => (IntCC::Equal, 0),
                    1 => return Some(b.ins().iconst(types::I8, i64::from(cond & 1))),
                    2 => (IntCC::SignedLessThan, 0),
                    3 => return Some(b.ins().iconst(types::I8, i64::from(cond & 1))),
                    4 => return Some(b.ins().iconst(types::I8, i64::from(cond & 1))),
                    5 => (IntCC::SignedGreaterThanOrEqual, 0),
                    _ => (IntCC::SignedGreaterThan, 0),
                };
                (cc, r, rhs)
            }
        };
        let (cc, v, rhs) = cc;
        let cc = if cond & 1 == 0 { cc } else { cc.complement() };
        Some(b.ins().icmp_imm_s(cc, v, rhs))
    }

    /// Write back what is newer than the core's, leaving the cache as it is
    /// for the code that goes on.
    fn store_dirty(&mut self) {
        for r in 0..31 {
            if let Some((v, true)) = self.regs[r] {
                let off = (offset_of!(Cpu, x) + 8 * r) as i32;
                self.b.ins().store(FLAGS, v, self.cpu, off);
            }
        }
        if let Some((f, true)) = self.nzcv {
            // Not kept: this may be a block other code does not go through.
            let v = self.pack_flags(f);
            self.b
                .ins()
                .store(FLAGS, v, self.cpu, offset_of!(Cpu, nzcv) as i32);
        }
        if let (Some((v, true)), Some(at)) = (self.sp, self.sp_at) {
            self.b.ins().store(FLAGS, v, at, 0);
        }
    }

    fn imm(&mut self, v: u64) -> Value {
        self.b.ins().iconst(types::I64, v as i64)
    }

    fn mask(&mut self, v: Value, sf: bool) -> Value {
        if sf {
            v
        } else {
            self.b.ins().band_imm_s(v, 0xFFFF_FFFF)
        }
    }

    /// `Xr`/`Wr`, XZR for 31.
    fn xr(&mut self, r: u8, sf: bool) -> Value {
        if r == 31 {
            return self.imm(0);
        }
        let v = self.reg(r);
        self.mask(v, sf)
    }

    fn set_xr(&mut self, r: u8, sf: bool, v: Value) {
        if r != 31 {
            let v = self.mask(v, sf);
            self.set_reg(r, v);
        }
    }

    // ---- arithmetic ----------------------------------------------------------

    /// `AddWithCarry` with a constant carry: the result and its `NZCV`.
    fn add_with_carry(&mut self, x: Value, y: Value, carry: bool, sf: bool) -> (Value, Value) {
        let b = &mut self.b;
        if sf {
            let r1 = b.ins().iadd(x, y);
            let c1 = b.ins().icmp(IntCC::UnsignedLessThan, r1, x);
            let (r, cy) = if carry {
                let r = b.ins().iadd_imm_s(r1, 1);
                let c2 = b.ins().icmp(IntCC::UnsignedLessThan, r, r1);
                (r, b.ins().bor(c1, c2))
            } else {
                (r1, c1)
            };
            let xr = b.ins().bxor(x, r);
            let yr = b.ins().bxor(y, r);
            let v = b.ins().band(xr, yr);
            let v = b.ins().ushr_imm_s(v, 63);
            let n = b.ins().ushr_imm_s(r, 63);
            let z = b.ins().icmp_imm_s(IntCC::Equal, r, 0);
            let nzcv = Self::pack(b, n, z, cy, v);
            (r, nzcv)
        } else {
            let x = b.ins().band_imm_s(x, 0xFFFF_FFFF);
            let y = b.ins().band_imm_s(y, 0xFFFF_FFFF);
            let wide = b.ins().iadd(x, y);
            let wide = if carry {
                b.ins().iadd_imm_s(wide, 1)
            } else {
                wide
            };
            let r = b.ins().band_imm_s(wide, 0xFFFF_FFFF);
            let cy = b.ins().ushr_imm_s(wide, 32);
            let xr = b.ins().bxor(x, r);
            let yr = b.ins().bxor(y, r);
            let v = b.ins().band(xr, yr);
            let v = b.ins().ushr_imm_s(v, 31);
            let v = b.ins().band_imm_s(v, 1);
            let n = b.ins().ushr_imm_s(r, 31);
            let z = b.ins().icmp_imm_s(IntCC::Equal, r, 0);
            let cy = b.ins().ireduce(types::I8, cy);
            let nzcv = Self::pack(b, n, z, cy, v);
            (r, nzcv)
        }
    }

    /// `NZCV` from N and V as 0/1 in an I64, Z and C as I8 booleans.
    fn pack(b: &mut FunctionBuilder, n: Value, z: Value, c: Value, v: Value) -> Value {
        let n = b.ins().ireduce(types::I32, n);
        let v = b.ins().ireduce(types::I32, v);
        let z = b.ins().uextend(types::I32, z);
        let c = b.ins().uextend(types::I32, c);
        let n = b.ins().ishl_imm_s(n, 31);
        let z = b.ins().ishl_imm_s(z, 30);
        let c = b.ins().ishl_imm_s(c, 29);
        let v = b.ins().ishl_imm_s(v, 28);
        let nz = b.ins().bor(n, z);
        let cv = b.ins().bor(c, v);
        b.ins().bor(nz, cv)
    }

    fn logic_flags(&mut self, r: Value, sf: bool) -> Value {
        let b = &mut self.b;
        let n = b.ins().ushr_imm_s(r, if sf { 63 } else { 31 });
        let n = b.ins().band_imm_s(n, 1);
        let n = b.ins().ireduce(types::I32, n);
        let n = b.ins().ishl_imm_s(n, 31);
        let z = b.ins().icmp_imm_s(IntCC::Equal, r, 0);
        let z = b.ins().uextend(types::I32, z);
        let z = b.ins().ishl_imm_s(z, 30);
        b.ins().bor(n, z)
    }

    fn shift_reg(&mut self, v: Value, kind: u32, amount: u32, sf: bool) -> Value {
        if amount == 0 {
            return v;
        }
        let b = &mut self.b;
        let amount = i64::from(amount);
        if sf {
            match kind {
                0 => b.ins().ishl_imm_s(v, amount),
                1 => b.ins().ushr_imm_s(v, amount),
                2 => b.ins().sshr_imm_s(v, amount),
                _ => b.ins().rotr_imm_s(v, amount),
            }
        } else {
            let w = b.ins().ireduce(types::I32, v);
            let w = match kind {
                0 => b.ins().ishl_imm_s(w, amount),
                1 => b.ins().ushr_imm_s(w, amount),
                2 => b.ins().sshr_imm_s(w, amount),
                _ => b.ins().rotr_imm_s(w, amount),
            };
            b.ins().uextend(types::I64, w)
        }
    }

    /// `ConditionHolds(cond)` as an I8 boolean.
    fn cond_holds(&mut self, cond: u32) -> Value {
        if let Some(v) = self.cond_direct(cond) {
            return v;
        }
        let nzcv = self.get_nzcv();
        let b = &mut self.b;
        let table = b
            .ins()
            .iconst(types::I32, i64::from(COND_HOLDS[cond as usize & 0xF]));
        let idx = b.ins().ushr_imm_s(nzcv, 28);
        let bit = b.ins().ushr(table, idx);
        let bit = b.ins().band_imm_s(bit, 1);
        b.ins().icmp_imm_s(IntCC::NotEqual, bit, 0)
    }

    fn branch_taken(&mut self, op: &Op) -> Value {
        match op.kind {
            Kind::BCond => self.cond_holds(op.a as u32),
            Kind::Cbz => {
                let v = self.xr(op.a, op.c & 1 != 0);
                let cc = if op.c & 2 != 0 {
                    IntCC::NotEqual
                } else {
                    IntCC::Equal
                };
                self.b.ins().icmp_imm_s(cc, v, 0)
            }
            _ => {
                let v = self.xr(op.a, true);
                let v = self.b.ins().ushr_imm_s(v, i64::from(op.b));
                let v = self.b.ins().band_imm_s(v, 1);
                let cc = if op.c != 0 {
                    IntCC::NotEqual
                } else {
                    IntCC::Equal
                };
                self.b.ins().icmp_imm_s(cc, v, 0)
            }
        }
    }

    /// Emit `op` at `pc` inline, or `false` when it goes to its handler.
    fn inline_op(&mut self, op: &Op, pc: Value) -> bool {
        let (a, b, c) = (op.a, op.b, op.c);
        match op.kind {
            Kind::Nop => {}
            Kind::MovConst => {
                let v = self.imm(op.imm);
                self.set_xr(a, true, v);
            }
            Kind::MovK => {
                let sf = c != 0;
                let x = self.xr(a, sf);
                let x = self.b.ins().band_imm_s(x, !(0xFFFFi64 << b));
                let r = self.b.ins().bor_imm_s(x, op.imm as i64);
                self.set_xr(a, sf, r);
            }
            Kind::MovReg => {
                let v = self.xr(b, c != 0);
                self.set_reg(a, v);
            }
            Kind::AddImmR | Kind::SubImmR => {
                let x = self.reg(b);
                let imm = if op.kind == Kind::AddImmR {
                    op.imm
                } else {
                    op.imm.wrapping_neg()
                };
                let r = self.b.ins().iadd_imm_s(x, imm as i64);
                let r = self.mask(r, c != 0);
                self.set_reg(a, r);
            }
            Kind::AddImm | Kind::SubImm => {
                if a == 31 || b == 31 {
                    return false;
                }
                let sf = c != 0;
                let x = self.reg(b);
                let x = self.mask(x, sf);
                let imm = if op.kind == Kind::AddImm {
                    op.imm
                } else {
                    op.imm.wrapping_neg()
                };
                let r = self.b.ins().iadd_imm_s(x, imm as i64);
                let r = self.mask(r, sf);
                self.set_reg(a, r);
            }
            Kind::AddsImm | Kind::SubsImm => {
                if b == 31 {
                    return false;
                }
                let sf = c != 0;
                let sub = op.kind == Kind::SubsImm;
                let x = self.reg(b);
                let x = self.mask(x, sf);
                if sub {
                    let y = self.imm(if sf { op.imm } else { op.imm & 0xFFFF_FFFF });
                    let r = self.b.ins().isub(x, y);
                    let r = self.mask(r, sf);
                    self.set_flags(Flags::Sub { x, y, sf });
                    self.set_xr(a, sf, r);
                    return true;
                }
                let y = self.imm(op.imm);
                let (r, nzcv) = self.add_with_carry(x, y, false, sf);
                self.set_nzcv(nzcv);
                self.set_xr(a, sf, r);
            }
            Kind::CmpImm | Kind::CmpImmW => {
                let sf = op.kind == Kind::CmpImm;
                let x = self.reg(b);
                let x = self.mask(x, sf);
                let y = self.imm(if sf { op.imm } else { op.imm & 0xFFFF_FFFF });
                self.set_flags(Flags::Sub { x, y, sf });
            }
            Kind::CmpReg | Kind::CmpRegW => {
                let sf = op.kind == Kind::CmpReg;
                let x = self.xr(a, sf);
                let y = self.xr(b, sf);
                self.set_flags(Flags::Sub { x, y, sf });
            }
            Kind::LogicImm => {
                let flags = c & 3 == 3;
                if !flags && a == 31 {
                    return false;
                }
                let sf = c & 4 != 0;
                let x = self.xr(b, sf);
                let r = match c & 3 {
                    0 | 3 => self.b.ins().band_imm_s(x, op.imm as i64),
                    1 => self.b.ins().bor_imm_s(x, op.imm as i64),
                    _ => self.b.ins().bxor_imm_s(x, op.imm as i64),
                };
                if flags {
                    let r = self.mask(r, sf);
                    self.set_flags(Flags::Logic { r, sf });
                }
                self.set_xr(a, sf, r);
            }
            Kind::LogicReg => {
                let f = op.imm as u32;
                let sf = f & (1 << 11) != 0;
                let y = self.xr(c, sf);
                let mut y = self.shift_reg(y, f & 3, (f >> 2) & 0x3F, sf);
                if f & (1 << 8) != 0 {
                    y = self.b.ins().bnot(y);
                    y = self.mask(y, sf);
                }
                let x = self.xr(b, sf);
                let opc = (f >> 9) & 3;
                let r = match opc {
                    0 | 3 => self.b.ins().band(x, y),
                    1 => self.b.ins().bor(x, y),
                    _ => self.b.ins().bxor(x, y),
                };
                if opc == 3 {
                    let r = self.mask(r, sf);
                    self.set_flags(Flags::Logic { r, sf });
                }
                self.set_xr(a, sf, r);
            }
            Kind::AddSubReg => {
                let f = op.imm as u32;
                let sf = f & (1 << 11) != 0;
                let sub = f & (1 << 8) != 0;
                let y = self.xr(c, sf);
                let y = self.shift_reg(y, f & 3, (f >> 2) & 0x3F, sf);
                let y = self.mask(y, sf);
                let x = self.xr(b, sf);
                let flags = f & (1 << 9) != 0;
                if sub && flags {
                    let r = self.b.ins().isub(x, y);
                    let r = self.mask(r, sf);
                    self.set_flags(Flags::Sub { x, y, sf });
                    self.set_xr(a, sf, r);
                    return true;
                }
                let y = if sub {
                    let y = self.b.ins().bnot(y);
                    self.mask(y, sf)
                } else {
                    y
                };
                let (r, nzcv) = self.add_with_carry(x, y, sub, sf);
                if flags {
                    self.set_nzcv(nzcv);
                }
                self.set_xr(a, sf, r);
            }
            Kind::Csel => {
                let f = op.imm as u32;
                let sf = f & (1 << 6) != 0;
                let holds = self.cond_holds(f & 0xF);
                let t = self.xr(b, sf);
                let y = self.xr(c, sf);
                let e = match (f >> 4) & 3 {
                    0 => y,
                    2 => self.b.ins().iadd_imm_s(y, 1),
                    1 => self.b.ins().bnot(y),
                    _ => self.b.ins().ineg(y),
                };
                let r = self.b.ins().select(holds, t, e);
                self.set_xr(a, sf, r);
            }
            Kind::Madd => {
                let f = op.imm as u32;
                let sf = f & (1 << 6) != 0;
                let x = self.xr(b, sf);
                let y = self.xr(c, sf);
                let p = self.b.ins().imul(x, y);
                let acc = self.xr((f & 0x1F) as u8, sf);
                let r = if f & (1 << 5) != 0 {
                    self.b.ins().isub(acc, p)
                } else {
                    self.b.ins().iadd(acc, p)
                };
                self.set_xr(a, sf, r);
            }
            Kind::Ccmp => {
                let f = c as u32;
                let sf = f & (1 << 4) != 0;
                let sub = f & (1 << 5) != 0;
                let holds = self.cond_holds(f & 0xF);
                let y = if f & (1 << 6) != 0 {
                    self.imm(u64::from(b))
                } else {
                    self.xr(b, sf)
                };
                let y = if sub {
                    let y = self.b.ins().bnot(y);
                    self.mask(y, sf)
                } else {
                    y
                };
                let x = self.xr(a, sf);
                let (_, nzcv) = self.add_with_carry(x, y, sub, sf);
                let other = self
                    .b
                    .ins()
                    .iconst(types::I32, i64::from(op.imm as u32 as i32));
                let r = self.b.ins().select(holds, nzcv, other);
                self.set_nzcv(r);
            }
            Kind::Adr => {
                let r = self.b.ins().iadd_imm_s(pc, op.imm as i64);
                self.set_xr(a, true, r);
            }
            Kind::Adrp => {
                let page = self.b.ins().band_imm_s(pc, !0xFFF);
                let r = self.b.ins().iadd_imm_s(page, op.imm as i64);
                self.set_xr(a, true, r);
            }
            _ => return false,
        }
        true
    }

    // ---- loads and stores ------------------------------------------------------

    fn el(&mut self) -> Value {
        if let Some(v) = self.el {
            return v;
        }
        let v = self
            .b
            .ins()
            .load(types::I32, FLAGS, self.cpu, offset_of!(Cpu, el) as i32);
        let v = self.b.ins().uextend(types::I64, v);
        self.el = Some(v);
        v
    }

    /// Where the `SP` in force sits in the core: `sp_el[EL]` with
    /// `PSTATE.SP` set, else `sp_el[0]`. Neither moves inside a run.
    fn sp_at(&mut self) -> Value {
        if let Some(v) = self.sp_at {
            return v;
        }
        let el = self.el();
        let spsel = self
            .b
            .ins()
            .load(types::I8, FLAGS, self.cpu, offset_of!(Cpu, spsel) as i32);
        let idx = self.b.ins().band_imm_s(el, 3);
        let zero = self.imm(0);
        let idx = self.b.ins().select(spsel, idx, zero);
        let off = self.b.ins().ishl_imm_s(idx, 3);
        let base = self
            .b
            .ins()
            .iadd_imm_s(self.cpu, offset_of!(Cpu, sp_el) as i64);
        let v = self.b.ins().iadd(base, off);
        self.sp_at = Some(v);
        v
    }

    /// `Xr`, or `SP` for 31.
    fn xsp(&mut self, r: u8) -> Value {
        if r != 31 {
            return self.reg(r);
        }
        if let Some((v, _)) = self.sp {
            return v;
        }
        let at = self.sp_at();
        let v = self.b.ins().load(types::I64, FLAGS, at, 0);
        self.sp = Some((v, false));
        v
    }

    fn set_xsp(&mut self, r: u8, v: Value) {
        if r == 31 {
            self.sp_at();
            self.sp = Some((v, true));
        } else {
            self.set_reg(r, v);
        }
    }

    fn ctx_fields(&mut self) -> (Value, Value) {
        if let (Some(m), Some(g)) = (self.marked, self.genp) {
            return (m, g);
        }
        let m = self
            .b
            .ins()
            .load(types::I64, FLAGS, self.ctx, offset_of!(Ctx, marked) as i32);
        let g = self
            .b
            .ins()
            .load(self.ptr, FLAGS, self.ctx, offset_of!(Ctx, genp) as i32);
        self.marked = Some(m);
        self.genp = Some(g);
        (m, g)
    }

    /// The probe [`Cpu::host_load`] and [`Cpu::host_store`] make, inline:
    /// whether `size` bytes at `va` are in a host page the data TLB has for
    /// this EL, and where its entry is. A store also needs that no other
    /// core holds a mark and that the page is not the run's own.
    fn probe(&mut self, va: Value, size: u32, write: bool) -> (Value, Value) {
        let l = &HOST_LAYOUT;
        let el = self.el();
        let b = &mut self.b;
        let off = b.ins().band_imm_s(va, 0xFFF);
        let inside = b.ins().icmp_imm_s(
            IntCC::UnsignedLessThanOrEqual,
            off,
            0x1000 - i64::from(size),
        );
        let page = b.ins().ushr_imm_s(va, 12);
        let p5 = b.ins().ushr_imm_s(va, 17);
        let p10 = b.ins().ushr_imm_s(va, 22);
        let h = b.ins().bxor(page, p5);
        let h = b.ins().bxor(h, p10);
        let idx = b.ins().band_imm_s(h, (l.hints - 1) as i64);
        let first = if write { l.write } else { l.read };
        let idx = b.ins().iadd_imm_s(idx, first as i64);
        let byte = b.ins().imul_imm_s(idx, l.size as i64);
        let base = b
            .ins()
            .iadd_imm_s(self.cpu, (offset_of!(Cpu, tlb) + l.host) as i64);
        let entry = b.ins().iadd(base, byte);
        let key = b.ins().load(types::I64, FLAGS, entry, l.key as i32);
        let want = b.ins().band_imm_s(va, !0xFFF);
        let want = b.ins().bor(want, el);
        let hit = b.ins().icmp(IntCC::Equal, key, want);
        let mut ok = b.ins().band(inside, hit);
        if write {
            let gen = self.b.ins().load(self.ptr, FLAGS, entry, l.gen as i32);
            let (marked, genp) = self.ctx_fields();
            let b = &mut self.b;
            let other = b.ins().icmp(IntCC::NotEqual, gen, genp);
            let free = b.ins().icmp_imm_s(IntCC::Equal, marked, 0);
            let both = b.ins().band(other, free);
            ok = b.ins().band(ok, both);
        }
        (ok, entry)
    }

    /// Branch on `ok`: into the fast path, or through the handler, which
    /// may stop the run. Leaves the builder in the fast path; [`Self::join`]
    /// brings the two together again at the block returned.
    fn fast_or_slow(
        &mut self,
        ok: Value,
        op: &Op,
        at: Value,
        pc: Value,
        c: Value,
    ) -> cranelift_codegen::ir::Block {
        let (fast, slow, join) = (
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
        );
        self.b.ins().brif(ok, fast, &[], slow, &[]);
        self.b.switch_to_block(slow);
        self.b.seal_block(slow);
        self.store_dirty();
        let sig = self.step.0;
        let f = (self.step_of)(op.kind) as usize as i64;
        let f = self.b.ins().iconst(self.ptr, f);
        let args = [self.cpu, self.bus, self.ctx, at, pc, c, self.out];
        let call = self.b.ins().call_indirect(sig, f, &args);
        let stopped = self.b.inst_results(call)[0];
        let ret = self.ret();
        self.b.ins().brif(stopped, ret, &[], join, &[]);
        self.b.switch_to_block(fast);
        self.b.seal_block(fast);
        join
    }

    /// The end of a fast path: the core's registers are brought up to date
    /// on it as the handler left them on the other, and nothing cached
    /// survives.
    fn join(&mut self, join: cranelift_codegen::ir::Block) {
        self.store_dirty();
        self.b.ins().jump(join, &[]);
        self.b.switch_to_block(join);
        self.b.seal_block(join);
        self.regs = [None; 31];
        self.nzcv = None;
        self.sp = None;
        self.el = None;
        self.sp_at = None;
        self.marked = None;
        self.genp = None;
    }

    fn host(&mut self, entry: Value, va: Value) -> Value {
        let bytes = self
            .b
            .ins()
            .load(self.ptr, FLAGS, entry, HOST_LAYOUT.bytes as i32);
        let off = self.b.ins().band_imm_s(va, 0xFFF);
        self.b.ins().iadd(bytes, off)
    }

    fn bump(&mut self, entry: Value) {
        let gen = self
            .b
            .ins()
            .load(self.ptr, FLAGS, entry, HOST_LAYOUT.gen as i32);
        let g = self.b.ins().load(types::I64, FLAGS, gen, 0);
        let g = self.b.ins().iadd_imm_s(g, 1);
        self.b.ins().store(FLAGS, g, gen, 0);
    }

    fn load_n(&mut self, p: Value, bytes: u32, offset: i32) -> Value {
        let ty = match bytes {
            1 => types::I8,
            2 => types::I16,
            4 => types::I32,
            _ => types::I64,
        };
        let v = self.b.ins().load(ty, GUEST, p, offset);
        if ty == types::I64 {
            v
        } else {
            self.b.ins().uextend(types::I64, v)
        }
    }

    fn store_n(&mut self, v: Value, p: Value, bytes: u32, offset: i32) {
        let v = match bytes {
            1 => self.b.ins().ireduce(types::I8, v),
            2 => self.b.ins().ireduce(types::I16, v),
            4 => self.b.ins().ireduce(types::I32, v),
            _ => v,
        };
        self.b.ins().store(GUEST, v, p, offset);
    }

    fn sext(&mut self, v: Value, bits: u32) -> Value {
        let sh = 64 - i64::from(bits);
        let v = self.b.ins().ishl_imm_s(v, sh);
        self.b.ins().sshr_imm_s(v, sh)
    }

    /// Emit a load or store with an inline fast path, or `false` when the
    /// whole instruction goes to its handler.
    fn inline_mem(&mut self, op: &Op, at: Value, pc: Value, c: Value) -> bool {
        let (a, b, cc) = (op.a, op.b, op.c);
        match op.kind {
            Kind::LdrX | Kind::LdrW | Kind::LdrH | Kind::LdrB => {
                let n = 1u32 << cc;
                let base = self.reg(b);
                let va = self.b.ins().iadd_imm_s(base, op.imm as i64);
                let (ok, entry) = self.probe(va, n, false);
                let join = self.fast_or_slow(ok, op, at, pc, c);
                let p = self.host(entry, va);
                let v = self.load_n(p, n, 0);
                self.set_xr(a, true, v);
                self.join(join);
            }
            Kind::StrX | Kind::StrW | Kind::StrH | Kind::StrB => {
                let n = 1u32 << cc;
                let base = self.reg(b);
                let va = self.b.ins().iadd_imm_s(base, op.imm as i64);
                let v = self.xr(a, true);
                let (ok, entry) = self.probe(va, n, true);
                let join = self.fast_or_slow(ok, op, at, pc, c);
                let p = self.host(entry, va);
                self.store_n(v, p, n, 0);
                self.bump(entry);
                self.join(join);
            }
            Kind::LdStImm => {
                let f = op.imm2 as u32;
                let (scale, access, mode) = (f & 3, (f >> 2) & 3, (f >> 4) & 3);
                let n = 1u32 << scale;
                let base = self.xsp(b);
                let moved = self.b.ins().iadd_imm_s(base, op.imm as i64);
                let (va, wb) = match mode {
                    0 | 2 => (moved, None),
                    1 => (base, Some(moved)),
                    _ => (moved, Some(moved)),
                };
                let store = access == 0;
                let v = store.then(|| self.xr(a, true));
                let (ok, entry) = self.probe(va, n, store);
                let join = self.fast_or_slow(ok, op, at, pc, c);
                let p = self.host(entry, va);
                let loaded = match v {
                    Some(v) => {
                        self.store_n(v, p, n, 0);
                        self.bump(entry);
                        None
                    }
                    None => {
                        let v = self.load_n(p, n, 0);
                        Some(match access {
                            2 => (self.sext(v, 8 * n), true),
                            3 => (self.sext(v, 8 * n), false),
                            _ => (v, true),
                        })
                    }
                };
                if let Some(wb) = wb {
                    self.set_xsp(b, wb);
                }
                if let Some((v, sf)) = loaded {
                    self.set_xr(a, sf, v);
                }
                self.join(join);
            }
            Kind::Pair => {
                let f = op.imm2 as u32;
                let (scale, access, mode) = (f & 3, (f >> 2) & 3, (f >> 4) & 3);
                let n = 1u32 << scale;
                let base = self.xsp(b);
                let moved = self.b.ins().iadd_imm_s(base, op.imm as i64);
                let (va, wb) = match mode {
                    0 | 2 => (moved, None),
                    1 => (base, Some(moved)),
                    _ => (moved, Some(moved)),
                };
                let store = access == 0;
                let vals = store.then(|| (self.xr(a, true), self.xr(cc, true)));
                let (ok, entry) = self.probe(va, 2 * n, store);
                let join = self.fast_or_slow(ok, op, at, pc, c);
                let p = self.host(entry, va);
                if let Some((va_, vc)) = vals {
                    self.store_n(va_, p, n, 0);
                    self.store_n(vc, p, n, n as i32);
                    self.bump(entry);
                    if let Some(wb) = wb {
                        self.set_xsp(b, wb);
                    }
                } else {
                    let mut first = self.load_n(p, n, 0);
                    let mut second = self.load_n(p, n, n as i32);
                    if access == 2 {
                        first = self.sext(first, 8 * n);
                        second = self.sext(second, 8 * n);
                    }
                    if let Some(wb) = wb {
                        self.set_xsp(b, wb);
                    }
                    self.set_xr(a, true, first);
                    self.set_xr(cc, true, second);
                }
                self.join(join);
            }
            _ => return false,
        }
        true
    }
}
