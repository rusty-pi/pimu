//! Straight runs compiled to wasm at run time, for the cores on threads in
//! the browser (#209).
//!
//! A core that has entered a run [`hot`] times queues it, with the runs of
//! its page that it reaches by direct branches: a region, compiled as one
//! function that loops from run to run inside it without leaving, so that a
//! hot loop is one function V8 soon optimises. Once enough are queued the
//! core encodes them as one module, which imports this module's memory and
//! function table, and installs the module's functions at the end of the
//! table. A compiled region is then called by its table index, and calls the
//! helpers of [`super::compiled`] by theirs. As with the Cranelift backend
//! (`src/aarch64/jit.rs`), register-only instructions and the conditions of
//! branches run inline, with the general registers and `NZCV` in wasm locals
//! between them; everything else calls back into the instruction's handler.
//!
//! The function table is per instance, so every core compiles and installs
//! its own runs.

use std::mem::offset_of;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use js_sys::{Object, Reflect, Uint8Array, WebAssembly};
use wasm_bindgen::{JsCast, JsValue};
use web_time::Instant;

use super::compiled::{last, step_of, take, Ctx};
use super::cpu::Cpu;
use super::exec::COND_HOLDS;
use super::mmu::HOST_LAYOUT;
use super::predecode::{Kind, Op, RunBus, RunOp};

/// Entries before a run is compiled; 0 compiles nothing. The page sets it.
static HOT: AtomicU32 = AtomicU32::new(0);

/// Regions a core compiles at a time, as one module, unless the queue has
/// waited this many entries into runs.
const BATCH: usize = 256;
const WAIT: u32 = 1 << 16;
/// Runs a region takes at most.
const REGION: usize = 48;

const SLOTS: usize = 1 << 18;
/// Bytes of wasm a core compiles at most: V8 never frees code a table
/// refers to, and all of a page's is under one limit.
const BUDGET: usize = 64 << 20;

/// Compile runs once they have been entered `hot` times, or none for 0.
pub fn set_hot(hot: u32) {
    HOT.store(hot, Relaxed);
}

pub fn hot() -> u32 {
    HOT.load(Relaxed)
}

/// Every core's counts: runs and modules compiled, the time it took, and
/// entries into compiled runs with the instructions retired in them.
pub static STATS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

thread_local! {
    static RAN: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// Compiled code entered from the run loop ran `cycles`: counted in
/// [`STATS`] every so often, not to share a line between cores each time.
#[inline(always)]
pub fn ran(cycles: u64) {
    RAN.with(|r| {
        let (n, c) = r.get();
        let (n, c) = (n + 1, c + cycles);
        if n == 1024 {
            STATS[3].fetch_add(n, Relaxed);
            STATS[4].fetch_add(c, Relaxed);
            r.set((0, 0));
        } else {
            r.set((n, c));
        }
    });
}

/// `(cpu, bus, ctx, pc, cycles, out)`, as [`super::jit`]'s compiled runs.
pub type Code = unsafe extern "C" fn(*mut Cpu, *mut u8, *const Ctx, u64, u64, *mut u64);

/// A run some region's code starts at.
struct Translation {
    pa: u64,
    gen: u64,
    code: usize,
    len: u64,
}

/// A region to compile: runs of one page, each at its offset in words,
/// the first the hot one.
struct Wanted {
    page: u64,
    gen: u64,
    runs: Vec<(u32, Box<[RunOp]>)>,
}

/// The first instruction of the core's run at a PA, decoded at a page
/// generation, if the core has it: from [`crate::arm::runs::Runs::first`].
pub type CoreRun<'a> = &'a dyn Fn(u64, u64) -> Option<*const RunOp>;

/// One core's compiled runs.
pub struct WasmJit {
    slots: Box<[Option<Translation>]>,
    /// The instructions compiled code refers to, which have to stay put as
    /// long as it lives, as it does.
    kept: Vec<Box<[RunOp]>>,
    queue: Vec<Wanted>,
    decoded: Box<super::predecode::Decoded>,
    /// Entries since the queue's first run went in.
    waited: u32,
    hot: u32,
    /// Bytes of wasm compiled so far.
    bytes: usize,
}

fn slot(pa: u64) -> usize {
    ((pa >> 2) ^ (pa >> 16)) as usize & (SLOTS - 1)
}

impl WasmJit {
    pub fn new(hot: u32) -> WasmJit {
        WasmJit {
            slots: (0..SLOTS).map(|_| None).collect(),
            kept: Vec::new(),
            queue: Vec::new(),
            decoded: Box::new(super::predecode::Decoded::new()),
            waited: 0,
            hot,
            bytes: 0,
        }
    }

    /// The code of the run at `pa`, decoded at its page's generation `gen`,
    /// and how many instructions it has.
    #[inline(always)]
    pub fn find(&self, pa: u64, gen: u64) -> Option<(Code, u64)> {
        let t = self.slots[slot(pa)].as_ref()?;
        // A function of this instance's table, of type `Code`.
        (t.pa == pa && t.gen == gen)
            .then(|| (unsafe { std::mem::transmute::<usize, Code>(t.code) }, t.len))
    }

    /// The run `ops` at `pa` was entered by the interpreter: queue its region
    /// once it has been often enough, and compile the queue once it is full.
    /// `code` is the run's page's words, from [`crate::mem::SharedRam`].
    #[inline(always)]
    pub fn entered<M: RunBus>(
        &mut self,
        ops: &[RunOp],
        pa: u64,
        gen: u64,
        code: *const u32,
        core_run: CoreRun,
    ) {
        let Some(first) = ops.first() else {
            return;
        };
        if !self.queue.is_empty() {
            self.waited += 1;
            if self.waited >= WAIT {
                self.flush::<M>(core_run);
            }
        }
        let hits = first.hits.get();
        if hits >= self.hot {
            return;
        }
        first.hits.set(hits + 1);
        if hits + 1 == self.hot {
            self.want::<M>(pa, gen, code, core_run);
        }
    }

    /// The run at `pa` and those of its page it branches to, and they to,
    /// as a region: those of them the core has entered a fair part as often
    /// as this one.
    #[inline(never)]
    fn want<M: RunBus>(&mut self, pa: u64, gen: u64, code: *const u32, core_run: CoreRun) {
        // The core's runs, which stay put while it looks at them.
        let entries = |pa| core_run(pa, gen).map_or(0, |r| unsafe { (*r).hits.get() });
        let warm = (self.hot / 8).max(1);
        let page = pa & !0xFFF;
        let mut runs: Vec<(u32, Box<[RunOp]>)> = Vec::new();
        let mut todo = vec![((pa & 0xFFF) >> 2) as u32];
        while let Some(w) = todo.pop() {
            if runs.len() >= REGION
                || runs.iter().any(|r| r.0 == w)
                || (!runs.is_empty()
                    && (entries(page | u64::from(w) << 2) < warm
                        || self.find(page | u64::from(w) << 2, gen).is_some()))
            {
                continue;
            }
            let mut ops = Vec::new();
            for at in w..1024 {
                // Inside the page, which `SharedRam::code_page` handed out.
                let op = self
                    .decoded
                    .get(unsafe { code.add(at as usize).read_unaligned() });
                let last = op.ends_run() || at == 1023;
                ops.push(op);
                if last {
                    break;
                }
            }
            let n = ops.len();
            for (i, op) in ops.iter().enumerate() {
                let here = w + i as u32;
                if op.plain_branch() && op.kind != Kind::Bl {
                    let to = i64::from(here) + (op.imm as i64 >> 2);
                    if (0..1024).contains(&to) {
                        todo.push(to as u32);
                    }
                }
                if i + 1 == n && op.cond_branch() && here < 1023 {
                    todo.push(here + 1);
                }
            }
            let ops = ops
                .into_iter()
                .enumerate()
                .map(|(i, op)| RunOp::new::<M>(op, i + 1 == n))
                .collect();
            runs.push((w, ops));
        }
        self.queue.push(Wanted {
            page: pa & !0xFFF,
            gen,
            runs,
        });
        if self.queue.len() >= BATCH {
            self.flush::<M>(core_run);
        }
    }

    #[inline(never)]
    fn flush<M: RunBus>(&mut self, core_run: CoreRun) {
        self.waited = 0;
        let queue = std::mem::take(&mut self.queue);
        if let Err(e) = self.compile::<M>(queue, core_run) {
            web_sys_warn(&e);
            self.hot = u32::MAX;
        }
    }

    fn compile<M: RunBus>(&mut self, queue: Vec<Wanted>, core_run: CoreRun) -> Result<(), JsValue> {
        let t = Instant::now();
        let table: WebAssembly::Table = wasm_bindgen::function_table().unchecked_into();
        let base = table.grow(queue.len() as u32)?;
        let helpers = Helpers::of::<M>();
        let bodies: Vec<Vec<u8>> = queue.iter().map(|w| lower(&w.runs, &helpers)).collect();
        let bytes = module(base, &bodies);
        self.bytes += bytes.len();
        if self.bytes >= BUDGET {
            self.hot = u32::MAX;
        }
        let module = WebAssembly::Module::new(&Uint8Array::from(&bytes[..]).into())?;
        let env = Object::new();
        Reflect::set(&env, &"memory".into(), &wasm_bindgen::memory())?;
        Reflect::set(&env, &"table".into(), &table)?;
        let imports = Object::new();
        Reflect::set(&imports, &"env".into(), &env)?;
        WebAssembly::Instance::new(&module, &imports)?;
        for (i, w) in queue.into_iter().enumerate() {
            for (off, ops) in w.runs {
                let pa = w.page | u64::from(off) << 2;
                if let Some(run) = core_run(pa, w.gen) {
                    // In the core's runs, for the branches linked to it,
                    // which check its page's generation.
                    unsafe { (*run).jit.set(base as usize + i) };
                }
                self.slots[slot(pa)] = Some(Translation {
                    pa,
                    gen: w.gen,
                    code: base as usize + i,
                    len: ops.len() as u64,
                });
                self.kept.push(ops);
                STATS[0].fetch_add(1, Relaxed);
            }
        }
        STATS[1].fetch_add(1, Relaxed);
        STATS[2].fetch_add(t.elapsed().as_micros() as u64, Relaxed);
        Ok(())
    }
}

fn web_sys_warn(e: &JsValue) {
    let console = Reflect::get(&js_sys::global(), &"console".into()).ok();
    let warn = console
        .as_ref()
        .and_then(|c| Reflect::get(c, &"warn".into()).ok())
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok());
    if let (Some(c), Some(w)) = (console, warn) {
        let _ = w.call2(&c, &"wasm JIT off:".into(), e);
    }
}

/// The table indices of the helpers compiled code calls, for one bus type.
struct Helpers {
    step: fn(Kind) -> super::compiled::Step,
    take: i32,
    last: i32,
    chain: i32,
}

impl Helpers {
    fn of<M: RunBus>() -> Helpers {
        Helpers {
            step: step_of::<M>,
            take: take::<M> as *const () as usize as i32,
            last: last::<M> as *const () as usize as i32,
            chain: chain::<M> as *const () as usize as i32,
        }
    }
}

/// The compiled run at `target`, `c` cycles in, for a run that branched
/// there to go on into: its table index, or 0 to stop instead. The bus
/// keeps `ctx` up to date for it.
unsafe extern "C" fn chain<M: RunBus>(
    cpu: *mut Cpu,
    bus: *mut M,
    ctx: *mut Ctx,
    target: u64,
    c: u64,
) -> u32 {
    (*bus).chain_compiled(&*cpu, &mut *ctx, target, c) as u32
}

// ---- the module ---------------------------------------------------------------

const I32: u8 = 0x7F;
const I64: u8 = 0x7E;
/// The function types of a module: a run (and [`last`]), [`super::compiled`]'s
/// `step`, and [`take`].
const T_RUN: u8 = 0;
const T_STEP: u8 = 1;
const T_TAKE: u8 = 2;
const T_CHAIN: u8 = 3;

fn leb_u(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn leb_s(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0) {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn name(out: &mut Vec<u8>, s: &str) {
    leb_u(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn section(out: &mut Vec<u8>, id: u8, body: &[u8]) {
    out.push(id);
    leb_u(out, body.len() as u64);
    out.extend_from_slice(body);
}

/// A module whose functions are `bodies`, all runs, which it puts in the
/// imported table from `base` on as it is instantiated.
fn module(base: u32, bodies: &[Vec<u8>]) -> Vec<u8> {
    let mut m = b"\0asm\x01\0\0\0".to_vec();
    let mut types = Vec::new();
    leb_u(&mut types, 4);
    types.extend_from_slice(&[0x60, 6, I32, I32, I32, I64, I64, I32, 0]);
    types.extend_from_slice(&[0x60, 7, I32, I32, I32, I32, I64, I64, I32, 1, I32]);
    types.extend_from_slice(&[0x60, 7, I32, I32, I32, I64, I64, I64, I32, 0]);
    types.extend_from_slice(&[0x60, 5, I32, I32, I32, I64, I64, 1, I32]);
    section(&mut m, 1, &types);

    let mut imports = Vec::new();
    leb_u(&mut imports, 2);
    name(&mut imports, "env");
    name(&mut imports, "memory");
    // Shared, as the threaded build's is, up to its 4 GiB.
    imports.extend_from_slice(&[0x02, 0x03]);
    leb_u(&mut imports, 1);
    leb_u(&mut imports, 65536);
    name(&mut imports, "env");
    name(&mut imports, "table");
    imports.extend_from_slice(&[0x01, 0x70, 0x00]);
    leb_u(&mut imports, u64::from(base) + bodies.len() as u64);
    section(&mut m, 2, &imports);

    let mut funcs = Vec::new();
    leb_u(&mut funcs, bodies.len() as u64);
    funcs.extend(std::iter::repeat_n(T_RUN, bodies.len()));
    section(&mut m, 3, &funcs);

    let mut elems = Vec::new();
    leb_u(&mut elems, 1);
    elems.push(0x00);
    elems.push(0x41);
    leb_s(&mut elems, i64::from(base as i32));
    elems.push(0x0B);
    leb_u(&mut elems, bodies.len() as u64);
    for i in 0..bodies.len() {
        leb_u(&mut elems, i as u64);
    }
    section(&mut m, 9, &elems);

    let mut code = Vec::new();
    leb_u(&mut code, bodies.len() as u64);
    for b in bodies {
        leb_u(&mut code, b.len() as u64);
        code.extend_from_slice(b);
    }
    section(&mut m, 10, &code);
    m
}

// ---- lowering ------------------------------------------------------------------

/// A wasm local of type `i64`, the only type the lowering keeps values in.
#[derive(Clone, Copy)]
struct V(u32);

const CPU: u32 = 0;
const BUS: u32 = 1;
const CTX: u32 = 2;
const PC0: u32 = 3;
const C0: u32 = 4;
const OUT: u32 = 5;
const PARAMS: u32 = 6;

mod op {
    pub const UNREACHABLE: u8 = 0x00;
    pub const BLOCK: u8 = 0x02;
    pub const LOOP: u8 = 0x03;
    pub const IF: u8 = 0x04;
    pub const BR: u8 = 0x0C;
    pub const BR_TABLE: u8 = 0x0E;
    pub const I64_GE_U: u8 = 0x5A;
    pub const I32_OR: u8 = 0x72;
    pub const END: u8 = 0x0B;
    pub const RETURN: u8 = 0x0F;
    pub const CALL_INDIRECT: u8 = 0x11;
    pub const RETURN_CALL_INDIRECT: u8 = 0x13;
    pub const SELECT: u8 = 0x1B;
    pub const LOCAL_GET: u8 = 0x20;
    pub const LOCAL_SET: u8 = 0x21;
    pub const ELSE: u8 = 0x05;
    pub const I32_LOAD: u8 = 0x28;
    pub const I32_LOAD8_U: u8 = 0x2D;
    pub const I64_LOAD8_U: u8 = 0x31;
    pub const I64_LOAD16_U: u8 = 0x33;
    pub const I64_LOAD32_U: u8 = 0x35;
    pub const I64_STORE8: u8 = 0x3C;
    pub const I64_STORE16: u8 = 0x3D;
    pub const I64_STORE32: u8 = 0x3E;
    pub const I32_ADD: u8 = 0x6A;
    pub const I32_MUL: u8 = 0x6C;
    pub const I32_AND: u8 = 0x71;
    pub const I64_EQ: u8 = 0x51;
    pub const I64_LE_U: u8 = 0x58;
    pub const I64_LOAD: u8 = 0x29;
    pub const I32_STORE: u8 = 0x36;
    pub const I64_STORE: u8 = 0x37;
    pub const I32_CONST: u8 = 0x41;
    pub const I64_CONST: u8 = 0x42;
    pub const I64_EQZ: u8 = 0x50;
    pub const I64_NE: u8 = 0x52;
    pub const I64_LT_U: u8 = 0x54;
    pub const I64_ADD: u8 = 0x7C;
    pub const I64_SUB: u8 = 0x7D;
    pub const I64_MUL: u8 = 0x7E;
    pub const I64_AND: u8 = 0x83;
    pub const I64_OR: u8 = 0x84;
    pub const I64_XOR: u8 = 0x85;
    pub const I64_SHL: u8 = 0x86;
    pub const I64_SHR_S: u8 = 0x87;
    pub const I64_SHR_U: u8 = 0x88;
    pub const I64_ROTR: u8 = 0x8A;
    pub const I32_SHL: u8 = 0x74;
    pub const I32_SHR_S: u8 = 0x75;
    pub const I32_SHR_U: u8 = 0x76;
    pub const I32_ROTR: u8 = 0x78;
    pub const I32_WRAP_I64: u8 = 0xA7;
    pub const I64_EXTEND_I32_U: u8 = 0xAD;
}

/// Whether a register's local holds its value, and whether that is newer
/// than the core's.
#[derive(Clone, Copy, PartialEq)]
enum Held {
    No,
    Clean,
    Dirty,
}

/// Each general register has a local of its own for the whole region, and
/// `NZCV` the one after them; then the region's budget and its page's write
/// generation, where it is, and the temporaries.
const REGS: u32 = PARAMS;
const NZCV: u32 = PARAMS + 31;
const UNTIL: u32 = PARAMS + 32;
const GEN: u32 = PARAMS + 33;
const GENP: u32 = PARAMS + 34;
const TEMPS: u32 = PARAMS + 35;

struct Lower<'a> {
    code: Vec<u8>,
    locals: u32,
    /// Temporaries the instruction at hand has taken.
    temp: u32,
    regs: [Held; 31],
    /// `NZCV`, packed in the low 32 bits.
    nzcv: Held,
    /// What [`Self::fast_or_slow`] found, for the other path to start from.
    before: Option<[Held; 31]>,
    h: &'a Helpers,
    /// The region's runs, by their offsets in words, in the order their
    /// code comes in the function; the run at hand, and its offset.
    starts: Vec<u32>,
    run: usize,
    off: u32,
}

/// The body of the function for the region `runs`: a loop round a
/// dispatch on the PC's offset in the page, into the code of the run there.
fn lower(runs: &[(u32, Box<[RunOp]>)], h: &Helpers) -> Vec<u8> {
    let starts: Vec<u32> = runs.iter().map(|r| r.0).collect();
    let mut l = Lower {
        code: Vec::new(),
        locals: 0,
        temp: 0,
        regs: [Held::No; 31],
        nzcv: Held::No,
        before: None,
        h,
        starts,
        run: 0,
        off: 0,
    };
    l.prologue();
    l.region(runs);
    let mut body = Vec::new();
    leb_u(&mut body, 1);
    leb_u(&mut body, u64::from(TEMPS - PARAMS + l.locals));
    body.push(I64);
    body.extend_from_slice(&l.code);
    body.push(op::END);
    body
}

impl Lower<'_> {
    /// Load the budget and generation the region checks.
    fn prologue(&mut self) {
        for (local, off, wide) in [
            (UNTIL, offset_of!(Ctx, chain_until), true),
            (GEN, offset_of!(Ctx, gen), true),
            (GENP, offset_of!(Ctx, genp), false),
        ] {
            self.get_param(CTX);
            if wide {
                self.code.extend_from_slice(&[op::I64_LOAD, 3]);
                leb_u(&mut self.code, off as u64);
            } else {
                self.code.extend_from_slice(&[op::I32_LOAD, 2]);
                leb_u(&mut self.code, off as u64);
                self.code.push(op::I64_EXTEND_I32_U);
            }
            self.code.push(op::LOCAL_SET);
            leb_u(&mut self.code, local.into());
        }
    }

    fn region(&mut self, runs: &[(u32, Box<[RunOp]>)]) {
        let k = runs.len() as u32;
        self.code
            .extend_from_slice(&[op::LOOP, 0x40, op::BLOCK, 0x40]);
        for _ in 0..k {
            self.code.extend_from_slice(&[op::BLOCK, 0x40]);
        }
        self.get_param(PC0);
        self.i64(0xFFC);
        self.code.push(op::I64_AND);
        let at = self.set();
        let mut sorted: Vec<(u32, u32)> = (0..k).map(|j| (self.starts[j as usize], j)).collect();
        sorted.sort_unstable();
        self.which(at, &sorted, k);
        self.code.push(op::BR_TABLE);
        leb_u(&mut self.code, k.into());
        for j in 0..k {
            leb_u(&mut self.code, j.into());
        }
        leb_u(&mut self.code, k.into());
        for (j, (off, ops)) in runs.iter().enumerate() {
            self.code.push(op::END);
            (self.run, self.off) = (j, *off);
            self.start_run();
            self.run_code(ops);
        }
        self.code
            .extend_from_slice(&[op::END, op::UNREACHABLE, op::END]);
    }

    /// The index of the run of `sorted` (word, index) whose word the byte
    /// offset `at` is, or `none`, by halves: an `i32` on the stack.
    fn which(&mut self, at: V, sorted: &[(u32, u32)], none: u32) {
        if let [(w, j)] = sorted {
            self.i32(*j as i32);
            self.i32(none as i32);
            self.get(at);
            self.i64(i64::from(*w) << 2);
            self.code.extend_from_slice(&[op::I64_EQ, op::SELECT]);
            return;
        }
        let (low, high) = sorted.split_at(sorted.len() / 2);
        self.get(at);
        self.i64(i64::from(high[0].0) << 2);
        self.code.extend_from_slice(&[op::I64_LT_U, op::IF, I32]);
        self.which(at, low, none);
        self.code.push(op::ELSE);
        self.which(at, high, none);
        self.code.push(op::END);
    }

    /// The registers as a run of the region starts, whichever way it was
    /// reached: all in the core, none in their locals yet.
    fn start_run(&mut self) {
        self.regs.fill(Held::No);
        self.nzcv = Held::No;
    }

    /// After the branch `at`, the run's instruction `i`, to `target`: on
    /// into the region's run there if it has one at word `to` and the
    /// budget and the page allow, else out as [`Self::exit`] goes. `nest` is
    /// how many blocks deeper than the run's code this is.
    fn branch(&mut self, at: i32, i: usize, target: V, to: i64, nest: u32) {
        let inside = (0..1024).contains(&to) && self.starts.contains(&(to as u32));
        if !inside {
            return self.exit(at, i, target);
        }
        self.store_dirty();
        self.cycle(i + 1);
        let c = self.set();
        self.get(c);
        self.get(V(UNTIL));
        self.code.push(op::I64_GE_U);
        self.get(V(GENP));
        self.code
            .extend_from_slice(&[op::I32_WRAP_I64, op::I64_LOAD, 3, 0]);
        self.get(V(GEN));
        self.code
            .extend_from_slice(&[op::I64_NE, op::I32_OR, op::IF, 0x40]);
        self.exit(at, i, target);
        self.code.push(op::END);
        self.get(target);
        self.code.push(op::LOCAL_SET);
        leb_u(&mut self.code, PC0.into());
        self.get(c);
        self.code.push(op::LOCAL_SET);
        leb_u(&mut self.code, C0.into());
        self.code.push(op::BR);
        let depth = self.starts.len() as u32 - self.run as u32 + nest;
        leb_u(&mut self.code, depth.into());
    }

    /// The word a branch at the run's instruction `i` goes to with `imm`.
    fn word(&self, i: usize, imm: u64) -> i64 {
        i64::from(self.off) + i as i64 + (imm as i64 >> 2)
    }

    fn run_code(&mut self, ops: &[RunOp]) {
        for (i, o) in ops.iter().enumerate() {
            self.temp = 0;
            let op = &o.op;
            let at = o as *const RunOp as usize as i32;
            if i + 1 == ops.len() && op.plain_branch() {
                // On in the region, or into the next compiled code, rather
                // than through the interpreter's links, which go to neither.
                if op.kind == Kind::Bl {
                    self.pc(i);
                    self.i64(4);
                    self.code.push(op::I64_ADD);
                    let lr = self.set();
                    self.set_reg(30, lr);
                }
                self.pc(i);
                self.i64(op.imm as i64);
                self.code.push(op::I64_ADD);
                let away = self.set();
                let to = self.word(i, op.imm);
                if op.kind != Kind::B && op.kind != Kind::Bl {
                    let t = self.branch_taken(op);
                    let regs = self.regs;
                    let nzcv = self.nzcv;
                    self.get(t);
                    self.code
                        .extend_from_slice(&[op::I32_WRAP_I64, op::IF, 0x40]);
                    self.branch(at, i, away, to, 1);
                    self.code.push(op::END);
                    (self.regs, self.nzcv) = (regs, nzcv);
                    self.pc(i);
                    self.i64(4);
                    self.code.push(op::I64_ADD);
                    let on = self.set();
                    let on_word = i64::from(self.off) + i as i64 + 1;
                    self.branch(at, i, on, on_word, 0);
                } else {
                    self.branch(at, i, away, to, 0);
                }
                STATS_INLINE.fetch_add(1, Relaxed);
                return;
            }
            if i + 1 == ops.len() && op.indirect_branch() && (op.insn >> 21) & 3 != 3 {
                let rn = ((op.insn >> 5) & 31) as u8;
                let target = self.xr(rn, true);
                self.chain_or_last(at, i, target, (op.insn >> 21) & 3 == 1);
                STATS_INLINE.fetch_add(1, Relaxed);
                return;
            }
            if i + 1 == ops.len() {
                self.store_dirty();
                for p in [CPU, BUS] {
                    self.get_param(p);
                }
                self.i32(at);
                self.pc(i);
                self.cycle(i);
                self.get_param(OUT);
                self.i32(self.h.last);
                self.code
                    .extend_from_slice(&[op::RETURN_CALL_INDIRECT, T_RUN, 0]);
                return;
            }
            if op.cond_branch() {
                let t = self.branch_taken(op);
                self.pc(i);
                self.i64(op.imm as i64);
                self.code.push(op::I64_ADD);
                let target = self.set();
                let to = self.word(i, op.imm);
                let (regs, nzcv) = (self.regs, self.nzcv);
                self.get(t);
                self.code
                    .extend_from_slice(&[op::I32_WRAP_I64, op::IF, 0x40]);
                self.branch(at, i, target, to, 1);
                self.code.push(op::END);
                (self.regs, self.nzcv) = (regs, nzcv);
                STATS_INLINE.fetch_add(1, Relaxed);
                continue;
            }
            if self.inline_op(op, i) || self.inline_mem(op, at, i) {
                STATS_INLINE.fetch_add(1, Relaxed);
                continue;
            }
            self.store_dirty();
            self.regs.fill(Held::No);
            self.nzcv = Held::No;
            for p in [CPU, BUS, CTX] {
                self.get_param(p);
            }
            self.i32(at);
            self.pc(i);
            self.cycle(i);
            self.get_param(OUT);
            self.i32((self.h.step)(op.kind) as usize as i32);
            self.code.extend_from_slice(&[op::CALL_INDIRECT, T_STEP, 0]);
            self.code
                .extend_from_slice(&[op::IF, 0x40, op::RETURN, op::END]);
            STATS_CALLED.fetch_add(1, Relaxed);
        }
    }

    /// Leave the run after the branch `at`, its instruction `i`, which goes
    /// to `target`: on into the compiled run there where the bus has one,
    /// else back to the run loop.
    fn exit(&mut self, at: i32, i: usize, target: V) {
        self.store_dirty();
        for p in [CPU, BUS, CTX] {
            self.get_param(p);
        }
        self.get(target);
        self.cycle(i + 1);
        self.i32(self.h.chain);
        self.code
            .extend_from_slice(&[op::CALL_INDIRECT, T_CHAIN, 0, op::I64_EXTEND_I32_U]);
        let code = self.set();
        self.get(code);
        self.code
            .extend_from_slice(&[op::I32_WRAP_I64, op::IF, 0x40]);
        for p in [CPU, BUS, CTX] {
            self.get_param(p);
        }
        self.get(target);
        self.cycle(i + 1);
        self.get_param(OUT);
        self.get(code);
        self.code.extend_from_slice(&[
            op::I32_WRAP_I64,
            op::RETURN_CALL_INDIRECT,
            T_RUN,
            0,
            op::END,
        ]);
        for p in [CPU, BUS] {
            self.get_param(p);
        }
        self.i32(at);
        self.pc(i);
        self.get(target);
        self.cycle(i);
        self.get_param(OUT);
        self.i32(self.h.take);
        self.code
            .extend_from_slice(&[op::RETURN_CALL_INDIRECT, T_TAKE, 0]);
    }

    /// `BR`, `BLR` (`link`) or `RET`, the run's instruction `i` at `at`, to
    /// `target`: on into the compiled run there where the bus has one, else
    /// the instruction as the interpreter runs it, with the registers as
    /// they were before it.
    fn chain_or_last(&mut self, at: i32, i: usize, target: V, link: bool) {
        self.store_dirty();
        for p in [CPU, BUS, CTX] {
            self.get_param(p);
        }
        self.get(target);
        self.cycle(i + 1);
        self.i32(self.h.chain);
        self.code
            .extend_from_slice(&[op::CALL_INDIRECT, T_CHAIN, 0, op::I64_EXTEND_I32_U]);
        let code = self.set();
        self.get(code);
        self.code
            .extend_from_slice(&[op::I32_WRAP_I64, op::IF, 0x40]);
        if link {
            self.get_param(CPU);
            self.pc(i);
            self.i64(4);
            self.code
                .extend_from_slice(&[op::I64_ADD, op::I64_STORE, 3]);
            leb_u(&mut self.code, (offset_of!(Cpu, x) + 8 * 30) as u64);
        }
        for p in [CPU, BUS, CTX] {
            self.get_param(p);
        }
        self.get(target);
        self.cycle(i + 1);
        self.get_param(OUT);
        self.get(code);
        self.code.extend_from_slice(&[
            op::I32_WRAP_I64,
            op::RETURN_CALL_INDIRECT,
            T_RUN,
            0,
            op::END,
        ]);
        for p in [CPU, BUS] {
            self.get_param(p);
        }
        self.i32(at);
        self.pc(i);
        self.cycle(i);
        self.get_param(OUT);
        self.i32(self.h.last);
        self.code
            .extend_from_slice(&[op::RETURN_CALL_INDIRECT, T_RUN, 0]);
    }

    // ---- emitting -------------------------------------------------------------

    /// A temporary, alive until the next instruction: they are taken
    /// afresh for each, as a function's locals are all cleared every time
    /// it is called.
    fn local(&mut self) -> V {
        let v = V(TEMPS + self.temp);
        self.temp += 1;
        self.locals = self.locals.max(self.temp);
        v
    }

    fn get(&mut self, v: V) {
        self.code.push(op::LOCAL_GET);
        leb_u(&mut self.code, v.0.into());
    }

    fn get_param(&mut self, p: u32) {
        self.code.push(op::LOCAL_GET);
        leb_u(&mut self.code, p.into());
    }

    /// What is on the stack, into a new local.
    fn set(&mut self) -> V {
        let v = self.local();
        self.code.push(op::LOCAL_SET);
        leb_u(&mut self.code, v.0.into());
        v
    }

    fn i32(&mut self, v: i32) {
        self.code.push(op::I32_CONST);
        leb_s(&mut self.code, v.into());
    }

    fn i64(&mut self, v: i64) {
        self.code.push(op::I64_CONST);
        leb_s(&mut self.code, v);
    }

    fn imm(&mut self, v: u64) -> V {
        self.i64(v as i64);
        self.set()
    }

    /// The PC of the run's instruction `i`.
    fn pc(&mut self, i: usize) {
        self.get_param(PC0);
        if i != 0 {
            self.i64(4 * i as i64);
            self.code.push(op::I64_ADD);
        }
    }

    /// The cycle of the run's instruction `i`.
    fn cycle(&mut self, i: usize) {
        self.get_param(C0);
        if i != 0 {
            self.i64(i as i64);
            self.code.push(op::I64_ADD);
        }
    }

    fn bin(&mut self, o: u8, x: V, y: V) -> V {
        self.get(x);
        self.get(y);
        self.code.push(o);
        self.set()
    }

    fn bin_imm(&mut self, o: u8, x: V, imm: i64) -> V {
        self.get(x);
        self.i64(imm);
        self.code.push(o);
        self.set()
    }

    /// A compare whose `i32` result goes into a local as 0 or 1.
    fn cmp(&mut self, o: u8, x: V, y: V) -> V {
        self.get(x);
        self.get(y);
        self.code.extend_from_slice(&[o, op::I64_EXTEND_I32_U]);
        self.set()
    }

    fn cmp_imm(&mut self, o: u8, x: V, imm: i64) -> V {
        self.get(x);
        self.i64(imm);
        self.code.extend_from_slice(&[o, op::I64_EXTEND_I32_U]);
        self.set()
    }

    fn eqz(&mut self, x: V) -> V {
        self.get(x);
        self.code
            .extend_from_slice(&[op::I64_EQZ, op::I64_EXTEND_I32_U]);
        self.set()
    }

    fn not(&mut self, x: V) -> V {
        self.bin_imm(op::I64_XOR, x, -1)
    }

    /// `holds ? t : e`, `holds` 0 or 1.
    fn select(&mut self, holds: V, t: V, e: V) -> V {
        self.get(t);
        self.get(e);
        self.get(holds);
        self.code.extend_from_slice(&[op::I32_WRAP_I64, op::SELECT]);
        self.set()
    }

    fn mask(&mut self, v: V, sf: bool) -> V {
        if sf {
            v
        } else {
            self.bin_imm(op::I64_AND, v, 0xFFFF_FFFF)
        }
    }

    // ---- the core's state -------------------------------------------------------

    /// Load `Xr` into its local.
    fn load_reg(&mut self, r: u8) {
        self.get_param(CPU);
        self.code.extend_from_slice(&[op::I64_LOAD, 3]);
        leb_u(&mut self.code, (offset_of!(Cpu, x) + 8 * r as usize) as u64);
        self.code.push(op::LOCAL_SET);
        leb_u(&mut self.code, u64::from(REGS + u32::from(r)));
    }

    /// `Xr`'s value as it is now, in a temporary of its own.
    fn reg(&mut self, r: u8) -> V {
        if self.regs[r as usize] == Held::No {
            self.load_reg(r);
            self.regs[r as usize] = Held::Clean;
        }
        self.get(V(REGS + u32::from(r)));
        self.set()
    }

    fn set_reg(&mut self, r: u8, v: V) {
        self.get(v);
        self.code.push(op::LOCAL_SET);
        leb_u(&mut self.code, u64::from(REGS + u32::from(r)));
        self.regs[r as usize] = Held::Dirty;
    }

    /// `Xr`/`Wr`, XZR for 31.
    fn xr(&mut self, r: u8, sf: bool) -> V {
        if r == 31 {
            return self.imm(0);
        }
        let v = self.reg(r);
        self.mask(v, sf)
    }

    fn set_xr(&mut self, r: u8, sf: bool, v: V) {
        if r != 31 {
            let v = self.mask(v, sf);
            self.set_reg(r, v);
        }
    }

    fn load_nzcv(&mut self) {
        self.get_param(CPU);
        self.code.extend_from_slice(&[op::I32_LOAD, 2]);
        leb_u(&mut self.code, offset_of!(Cpu, nzcv) as u64);
        self.code
            .extend_from_slice(&[op::I64_EXTEND_I32_U, op::LOCAL_SET]);
        leb_u(&mut self.code, NZCV.into());
    }

    fn get_nzcv(&mut self) -> V {
        if self.nzcv == Held::No {
            self.load_nzcv();
            self.nzcv = Held::Clean;
        }
        self.get(V(NZCV));
        self.set()
    }

    fn set_nzcv(&mut self, v: V) {
        self.get(v);
        self.code.push(op::LOCAL_SET);
        leb_u(&mut self.code, NZCV.into());
        self.nzcv = Held::Dirty;
    }

    /// Write back what is newer than the core's, leaving the cache as it is
    /// for the code that goes on.
    fn store_dirty(&mut self) {
        for r in 0..31 {
            if self.regs[r] == Held::Dirty {
                self.get_param(CPU);
                self.get(V(REGS + r as u32));
                self.code.extend_from_slice(&[op::I64_STORE, 3]);
                leb_u(&mut self.code, (offset_of!(Cpu, x) + 8 * r) as u64);
            }
        }
        if self.nzcv == Held::Dirty {
            self.get_param(CPU);
            self.get(V(NZCV));
            self.code
                .extend_from_slice(&[op::I32_WRAP_I64, op::I32_STORE, 2]);
            leb_u(&mut self.code, offset_of!(Cpu, nzcv) as u64);
        }
    }

    // ---- arithmetic ---------------------------------------------------------------

    /// `AddWithCarry` with a constant carry: the result and its `NZCV`.
    fn add_with_carry(&mut self, x: V, y: V, carry: bool, sf: bool) -> (V, V) {
        if sf {
            let r1 = self.bin(op::I64_ADD, x, y);
            let c1 = self.cmp(op::I64_LT_U, r1, x);
            let (r, cy) = if carry {
                let r = self.bin_imm(op::I64_ADD, r1, 1);
                let c2 = self.cmp(op::I64_LT_U, r, r1);
                (r, self.bin(op::I64_OR, c1, c2))
            } else {
                (r1, c1)
            };
            let xr = self.bin(op::I64_XOR, x, r);
            let yr = self.bin(op::I64_XOR, y, r);
            let v = self.bin(op::I64_AND, xr, yr);
            let v = self.bin_imm(op::I64_SHR_U, v, 63);
            let n = self.bin_imm(op::I64_SHR_U, r, 63);
            let z = self.eqz(r);
            (r, self.pack(n, z, cy, v))
        } else {
            let x = self.mask(x, false);
            let y = self.mask(y, false);
            let wide = self.bin(op::I64_ADD, x, y);
            let wide = if carry {
                self.bin_imm(op::I64_ADD, wide, 1)
            } else {
                wide
            };
            let r = self.mask(wide, false);
            let cy = self.bin_imm(op::I64_SHR_U, wide, 32);
            let xr = self.bin(op::I64_XOR, x, r);
            let yr = self.bin(op::I64_XOR, y, r);
            let v = self.bin(op::I64_AND, xr, yr);
            let v = self.bin_imm(op::I64_SHR_U, v, 31);
            let v = self.bin_imm(op::I64_AND, v, 1);
            let n = self.bin_imm(op::I64_SHR_U, r, 31);
            let z = self.eqz(r);
            (r, self.pack(n, z, cy, v))
        }
    }

    /// `NZCV` from each of them as 0 or 1.
    fn pack(&mut self, n: V, z: V, c: V, v: V) -> V {
        for (f, s) in [(n, 31), (z, 30), (c, 29), (v, 28)] {
            self.get(f);
            self.i64(s);
            self.code.push(op::I64_SHL);
        }
        self.code
            .extend_from_slice(&[op::I64_OR, op::I64_OR, op::I64_OR]);
        self.set()
    }

    fn logic_flags(&mut self, r: V, sf: bool) -> V {
        let n = self.bin_imm(op::I64_SHR_U, r, if sf { 63 } else { 31 });
        let n = self.bin_imm(op::I64_AND, n, 1);
        let z = self.eqz(r);
        let zero = self.imm(0);
        self.pack(n, z, zero, zero)
    }

    fn shift_reg(&mut self, v: V, kind: u32, amount: u32, sf: bool) -> V {
        if amount == 0 {
            return v;
        }
        if sf {
            let o = [op::I64_SHL, op::I64_SHR_U, op::I64_SHR_S, op::I64_ROTR][kind as usize & 3];
            self.bin_imm(o, v, amount.into())
        } else {
            let o = [op::I32_SHL, op::I32_SHR_U, op::I32_SHR_S, op::I32_ROTR][kind as usize & 3];
            self.get(v);
            self.code.push(op::I32_WRAP_I64);
            self.i32(amount as i32);
            self.code.extend_from_slice(&[o, op::I64_EXTEND_I32_U]);
            self.set()
        }
    }

    /// `ConditionHolds(cond)`, 0 or 1.
    fn cond_holds(&mut self, cond: u32) -> V {
        if cond >> 1 == 7 {
            return self.imm(1);
        }
        let nzcv = self.get_nzcv();
        let idx = self.bin_imm(op::I64_SHR_U, nzcv, 28);
        let table = self.imm(u64::from(COND_HOLDS[cond as usize & 0xF]));
        let bit = self.bin(op::I64_SHR_U, table, idx);
        self.bin_imm(op::I64_AND, bit, 1)
    }

    fn branch_taken(&mut self, op: &Op) -> V {
        match op.kind {
            Kind::BCond => self.cond_holds(op.a as u32),
            Kind::Cbz => {
                let v = self.xr(op.a, op.c & 1 != 0);
                if op.c & 2 != 0 {
                    self.cmp_imm(self::op::I64_NE, v, 0)
                } else {
                    self.eqz(v)
                }
            }
            _ => {
                let v = self.xr(op.a, true);
                let v = self.bin_imm(self::op::I64_SHR_U, v, i64::from(op.b));
                let bit = self.bin_imm(self::op::I64_AND, v, 1);
                if op.c != 0 {
                    bit
                } else {
                    self.eqz(bit)
                }
            }
        }
    }

    /// Emit `op`, the run's instruction `i`, inline, or `false` when it goes
    /// to its handler.
    fn inline_op(&mut self, op: &Op, i: usize) -> bool {
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
                let x = self.bin_imm(self::op::I64_AND, x, !(0xFFFFi64 << b));
                let r = self.bin_imm(self::op::I64_OR, x, op.imm as i64);
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
                let r = self.bin_imm(self::op::I64_ADD, x, imm as i64);
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
                let r = self.bin_imm(self::op::I64_ADD, x, imm as i64);
                let r = self.mask(r, sf);
                self.set_reg(a, r);
            }
            Kind::AddsImm | Kind::SubsImm => {
                if b == 31 {
                    return false;
                }
                let sf = c != 0;
                let x = self.reg(b);
                let x = self.mask(x, sf);
                let (y, carry) = if op.kind == Kind::SubsImm {
                    (!op.imm, true)
                } else {
                    (op.imm, false)
                };
                let y = self.imm(if sf { y } else { y & 0xFFFF_FFFF });
                let (r, nzcv) = self.add_with_carry(x, y, carry, sf);
                self.set_nzcv(nzcv);
                self.set_xr(a, sf, r);
            }
            Kind::CmpImm | Kind::CmpImmW => {
                let sf = op.kind == Kind::CmpImm;
                let x = self.reg(b);
                let x = self.mask(x, sf);
                let y = self.imm(if sf { !op.imm } else { !op.imm & 0xFFFF_FFFF });
                let (_, nzcv) = self.add_with_carry(x, y, true, sf);
                self.set_nzcv(nzcv);
            }
            Kind::CmpReg | Kind::CmpRegW => {
                let sf = op.kind == Kind::CmpReg;
                let x = self.xr(a, sf);
                let y = self.xr(b, sf);
                let y = self.not(y);
                let y = self.mask(y, sf);
                let (_, nzcv) = self.add_with_carry(x, y, true, sf);
                self.set_nzcv(nzcv);
            }
            Kind::LogicImm => {
                let flags = c & 3 == 3;
                if !flags && a == 31 {
                    return false;
                }
                let sf = c & 4 != 0;
                let x = self.xr(b, sf);
                let o = [
                    self::op::I64_AND,
                    self::op::I64_OR,
                    self::op::I64_XOR,
                    self::op::I64_AND,
                ][c as usize & 3];
                let r = self.bin_imm(o, x, op.imm as i64);
                if flags {
                    let r = self.mask(r, sf);
                    let f = self.logic_flags(r, sf);
                    self.set_nzcv(f);
                }
                self.set_xr(a, sf, r);
            }
            Kind::LogicReg => {
                let f = op.imm as u32;
                let sf = f & (1 << 11) != 0;
                let y = self.xr(c, sf);
                let mut y = self.shift_reg(y, f & 3, (f >> 2) & 0x3F, sf);
                if f & (1 << 8) != 0 {
                    y = self.not(y);
                    y = self.mask(y, sf);
                }
                let x = self.xr(b, sf);
                let opc = (f >> 9) & 3;
                let o = [
                    self::op::I64_AND,
                    self::op::I64_OR,
                    self::op::I64_XOR,
                    self::op::I64_AND,
                ][opc as usize];
                let r = self.bin(o, x, y);
                if opc == 3 {
                    let r = self.mask(r, sf);
                    let fl = self.logic_flags(r, sf);
                    self.set_nzcv(fl);
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
                let y = if sub {
                    let y = self.not(y);
                    self.mask(y, sf)
                } else {
                    y
                };
                let (r, nzcv) = self.add_with_carry(x, y, sub, sf);
                if f & (1 << 9) != 0 {
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
                    2 => self.bin_imm(self::op::I64_ADD, y, 1),
                    1 => self.not(y),
                    _ => {
                        let zero = self.imm(0);
                        self.bin(self::op::I64_SUB, zero, y)
                    }
                };
                let r = self.select(holds, t, e);
                self.set_xr(a, sf, r);
            }
            Kind::Madd => {
                let f = op.imm as u32;
                let sf = f & (1 << 6) != 0;
                let x = self.xr(b, sf);
                let y = self.xr(c, sf);
                let p = self.bin(self::op::I64_MUL, x, y);
                let acc = self.xr((f & 0x1F) as u8, sf);
                let o = if f & (1 << 5) != 0 {
                    self::op::I64_SUB
                } else {
                    self::op::I64_ADD
                };
                let r = self.bin(o, acc, p);
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
                    let y = self.not(y);
                    self.mask(y, sf)
                } else {
                    y
                };
                let x = self.xr(a, sf);
                let (_, nzcv) = self.add_with_carry(x, y, sub, sf);
                let other = self.imm(u64::from(op.imm as u32));
                let r = self.select(holds, nzcv, other);
                self.set_nzcv(r);
            }
            Kind::Bitfield => {
                let insn = op.insn;
                let sf = insn >> 31 != 0;
                let src = self.xr(b, sf);
                let rotated = self.shift_reg(src, 3, u32::from(c), sf);
                let bot = self.bin_imm(self::op::I64_AND, rotated, op.imm as i64);
                let tmask = op.imm2 as i64;
                let r = match (insn >> 29) & 3 {
                    0 => {
                        let bit =
                            self.bin_imm(self::op::I64_SHR_U, src, i64::from((insn >> 10) & 63));
                        let bit = self.bin_imm(self::op::I64_AND, bit, 1);
                        let zero = self.imm(0);
                        let top = self.bin(self::op::I64_SUB, zero, bit);
                        let top = self.bin_imm(self::op::I64_AND, top, !tmask);
                        let bot = self.bin_imm(self::op::I64_AND, bot, tmask);
                        self.bin(self::op::I64_OR, top, bot)
                    }
                    1 => {
                        let dst = self.xr(a, sf);
                        let kept = self.bin_imm(self::op::I64_AND, dst, !(op.imm as i64));
                        let bot = self.bin(self::op::I64_OR, kept, bot);
                        let bot = self.bin_imm(self::op::I64_AND, bot, tmask);
                        let top = self.bin_imm(self::op::I64_AND, dst, !tmask);
                        self.bin(self::op::I64_OR, top, bot)
                    }
                    _ => self.bin_imm(self::op::I64_AND, bot, tmask),
                };
                self.set_xr(a, sf, r);
            }
            Kind::Adr => {
                self.pc(i);
                self.i64(op.imm as i64);
                self.code.push(self::op::I64_ADD);
                let r = self.set();
                self.set_xr(a, true, r);
            }
            Kind::Adrp => {
                self.pc(i);
                self.i64(!0xFFF);
                self.code.push(self::op::I64_AND);
                self.i64(op.imm as i64);
                self.code.push(self::op::I64_ADD);
                let r = self.set();
                self.set_xr(a, true, r);
            }
            _ => return false,
        }
        true
    }
}

impl Lower<'_> {
    /// A field of the core, as an `i32` on the stack: `cpu + off`.
    fn cpu_addr(&mut self, off: usize) {
        self.get_param(CPU);
        if off != 0 {
            self.i32(off as i32);
            self.code.push(op::I32_ADD);
        }
    }

    /// An `i32` local holding a wasm address, kept as an `i64` local like
    /// every other value.
    fn wrap_set(&mut self) -> V {
        self.code.push(op::I64_EXTEND_I32_U);
        self.set()
    }

    fn addr(&mut self, v: V) {
        self.get(v);
        self.code.push(op::I32_WRAP_I64);
    }

    /// Where the `SP` in force sits in the core: `sp_el[EL]` with
    /// `PSTATE.SP` set at EL1 and above, else `sp_el[0]`.
    fn sp_at(&mut self) -> V {
        self.cpu_addr(offset_of!(Cpu, sp_el));
        self.get_param(CPU);
        self.code.extend_from_slice(&[op::I32_LOAD, 2]);
        leb_u(&mut self.code, offset_of!(Cpu, el) as u64);
        self.i32(0);
        self.get_param(CPU);
        self.code.extend_from_slice(&[op::I32_LOAD8_U, 0]);
        leb_u(&mut self.code, offset_of!(Cpu, spsel) as u64);
        self.code.push(op::SELECT);
        self.i32(3);
        self.code.extend_from_slice(&[op::I32_SHL, op::I32_ADD]);
        self.wrap_set()
    }

    /// `Xn|SP`: `SP` for 31.
    fn xsp(&mut self, r: u8) -> V {
        if r != 31 {
            return self.reg(r);
        }
        let at = self.sp_at();
        self.addr(at);
        self.code.extend_from_slice(&[op::I64_LOAD, 3, 0]);
        self.set()
    }

    /// The probe `Cpu::host_load` and `Cpu::host_store` make, inline: 1 if
    /// `size` bytes at `va` are in a host page the data TLB has for this EL,
    /// and the address of its entry. A store also needs that no other core
    /// holds a mark and that the page is not the run's own.
    fn probe(&mut self, va: V, size: u32, write: bool) -> (V, V) {
        let l = &HOST_LAYOUT;
        let off = self.bin_imm(op::I64_AND, va, 0xFFF);
        let inside = self.cmp_imm(op::I64_LE_U, off, 0x1000 - i64::from(size));
        let page = self.bin_imm(op::I64_SHR_U, va, 12);
        let p5 = self.bin_imm(op::I64_SHR_U, va, 17);
        let p10 = self.bin_imm(op::I64_SHR_U, va, 22);
        let h = self.bin(op::I64_XOR, page, p5);
        let h = self.bin(op::I64_XOR, h, p10);
        let idx = self.bin_imm(op::I64_AND, h, (l.hints - 1) as i64);
        let first = if write { l.write } else { l.read };
        self.cpu_addr(offset_of!(Cpu, tlb) + l.host);
        self.get(idx);
        self.code.push(op::I32_WRAP_I64);
        self.i32(first as i32);
        self.code.push(op::I32_ADD);
        self.i32(l.size as i32);
        self.code.extend_from_slice(&[op::I32_MUL, op::I32_ADD]);
        // Where the slots start, plus the slot's index times its size.
        let entry = self.wrap_set();
        self.addr(entry);
        self.code.extend_from_slice(&[op::I64_LOAD, 3]);
        leb_u(&mut self.code, l.key as u64);
        let key = self.set();
        self.get(va);
        self.i64(!0xFFF);
        self.code.push(op::I64_AND);
        self.get_param(CPU);
        self.code.extend_from_slice(&[op::I32_LOAD, 2]);
        leb_u(&mut self.code, offset_of!(Cpu, el) as u64);
        self.code
            .extend_from_slice(&[op::I64_EXTEND_I32_U, op::I64_OR]);
        let want = self.set();
        let hit = self.cmp(op::I64_EQ, key, want);
        let mut ok = self.bin(op::I64_AND, inside, hit);
        if write {
            self.addr(entry);
            self.code.extend_from_slice(&[op::I32_LOAD, 2]);
            leb_u(&mut self.code, l.gen as u64);
            let gen = self.wrap_set();
            self.get_param(CTX);
            self.code.extend_from_slice(&[op::I32_LOAD, 2]);
            leb_u(&mut self.code, offset_of!(Ctx, genp) as u64);
            let genp = self.wrap_set();
            let other = self.cmp(op::I64_NE, gen, genp);
            self.get_param(CTX);
            self.code.extend_from_slice(&[op::I64_LOAD, 3]);
            leb_u(&mut self.code, offset_of!(Ctx, marked) as u64);
            let marked = self.set();
            let free = self.eqz(marked);
            let both = self.bin(op::I64_AND, other, free);
            ok = self.bin(op::I64_AND, ok, both);
        }
        (ok, entry)
    }

    /// Into the fast path where `ok`; [`Self::join`] adds the other one,
    /// through the handler.
    fn fast_or_slow(&mut self, ok: V) {
        self.before = Some(self.regs);
        self.get(ok);
        self.code
            .extend_from_slice(&[op::I32_WRAP_I64, op::IF, 0x40]);
    }

    /// The handler's path, which may stop the run, starting from the
    /// registers as they were before the fast path, and reloading the ones
    /// the instruction writes, `writes`. After it the registers are as the
    /// fast path left them, on both.
    fn join(&mut self, op: &Op, at: i32, i: usize, writes: &[u8]) {
        let fast = self.regs;
        self.regs = self.before.take().unwrap_or(fast);
        self.code.push(op::ELSE);
        self.store_dirty();
        for p in [CPU, BUS, CTX] {
            self.get_param(p);
        }
        self.i32(at);
        self.pc(i);
        self.cycle(i);
        self.get_param(OUT);
        self.i32((self.h.step)(op.kind) as usize as i32);
        self.code.extend_from_slice(&[op::CALL_INDIRECT, T_STEP, 0]);
        self.code
            .extend_from_slice(&[op::IF, 0x40, op::RETURN, op::END]);
        for &r in writes {
            if r != 31 {
                self.load_reg(r);
            }
        }
        self.code.push(op::END);
        self.regs = fast;
    }

    /// The host address of `va` in the page of `entry`, as an `i32` on the
    /// stack.
    fn host(&mut self, entry: V, va: V) {
        self.addr(entry);
        self.code.extend_from_slice(&[op::I32_LOAD, 2]);
        leb_u(&mut self.code, HOST_LAYOUT.bytes as u64);
        self.get(va);
        self.code.push(op::I32_WRAP_I64);
        self.i32(0xFFF);
        self.code.extend_from_slice(&[op::I32_AND, op::I32_ADD]);
    }

    /// The page's write generation moves on if code was decoded from it, as
    /// `Memory::bump` does.
    fn bump(&mut self, entry: V) {
        self.addr(entry);
        self.code.extend_from_slice(&[op::I32_LOAD, 2]);
        leb_u(&mut self.code, HOST_LAYOUT.gen as u64);
        let gen = self.wrap_set();
        self.addr(gen);
        self.addr(gen);
        self.code.extend_from_slice(&[op::I64_LOAD, 3, 0]);
        let g = self.set();
        self.get(g);
        self.get(g);
        self.i64(1);
        self.code
            .extend_from_slice(&[op::I64_AND, op::I64_ADD, op::I64_STORE, 3, 0]);
    }

    fn load_n(&mut self, bytes: u32, offset: u32) -> V {
        let o = match bytes {
            1 => op::I64_LOAD8_U,
            2 => op::I64_LOAD16_U,
            4 => op::I64_LOAD32_U,
            _ => op::I64_LOAD,
        };
        self.code.extend_from_slice(&[o, 0]);
        leb_u(&mut self.code, offset.into());
        self.set()
    }

    /// Store `v` at the host address on the stack.
    fn store_n(&mut self, v: V, bytes: u32, offset: u32) {
        self.get(v);
        let o = match bytes {
            1 => op::I64_STORE8,
            2 => op::I64_STORE16,
            4 => op::I64_STORE32,
            _ => op::I64_STORE,
        };
        self.code.extend_from_slice(&[o, 0]);
        leb_u(&mut self.code, offset.into());
    }

    fn sext(&mut self, v: V, bits: u32) -> V {
        if bits >= 64 {
            return v;
        }
        let s = self.bin_imm(op::I64_SHL, v, i64::from(64 - bits));
        self.bin_imm(op::I64_SHR_S, s, i64::from(64 - bits))
    }

    /// `Xr` written inside a fast path.
    fn put_reg(&mut self, r: u8, v: V) {
        if r != 31 {
            self.set_reg(r, v);
        }
    }

    /// `Xn|SP` written back inside a fast path.
    fn put_xsp(&mut self, r: u8, v: V) {
        if r != 31 {
            return self.put_reg(r, v);
        }
        let at = self.sp_at();
        self.addr(at);
        self.get(v);
        self.code.extend_from_slice(&[op::I64_STORE, 3, 0]);
    }

    /// A load or store whose host page the data TLB has, inline, with the
    /// handler for the rest; `false` for the kinds it does not do.
    fn inline_mem(&mut self, op: &Op, at: i32, i: usize) -> bool {
        let (a, b, cc) = (op.a, op.b, op.c);
        match op.kind {
            Kind::LdrX
            | Kind::LdrW
            | Kind::LdrH
            | Kind::LdrB
            | Kind::StrX
            | Kind::StrW
            | Kind::StrH
            | Kind::StrB => {
                let store = op.kind as u8 >= Kind::StrX as u8;
                let n = 1u32 << cc;
                let base = self.reg(b);
                let va = self.bin_imm(self::op::I64_ADD, base, op.imm as i64);
                let v = store.then(|| self.xr(a, true));
                let (ok, entry) = self.probe(va, n, store);
                self.fast_or_slow(ok);
                self.host(entry, va);
                match v {
                    Some(v) => {
                        self.store_n(v, n, 0);
                        self.bump(entry);
                    }
                    None => {
                        let v = self.load_n(n, 0);
                        self.put_reg(a, v);
                    }
                }
                let writes = [a];
                self.join(op, at, i, if store { &[] } else { &writes });
            }
            Kind::LdStRegOff => {
                let f = op.imm as u32;
                let (scale, access, option, shift) = (f & 3, (f >> 2) & 3, (f >> 4) & 7, f >> 7);
                let n = 1u32 << scale;
                let index = self.xr(cc, true);
                let len = 8u32 << (option & 3);
                let index = if option & 4 != 0 {
                    self.sext(index, len)
                } else if len < 64 {
                    self.bin_imm(self::op::I64_AND, index, (1i64 << len) - 1)
                } else {
                    index
                };
                let index = self.bin_imm(self::op::I64_SHL, index, i64::from(shift));
                let base = self.xsp(b);
                let va = self.bin(self::op::I64_ADD, base, index);
                let store = access == 0;
                let v = store.then(|| self.xr(a, true));
                let (ok, entry) = self.probe(va, n, store);
                self.fast_or_slow(ok);
                self.host(entry, va);
                match v {
                    Some(v) => {
                        self.store_n(v, n, 0);
                        self.bump(entry);
                    }
                    None => {
                        let mut v = self.load_n(n, 0);
                        if access >= 2 {
                            v = self.sext(v, 8 * n);
                            if access == 3 {
                                v = self.mask(v, false);
                            }
                        }
                        self.put_reg(a, v);
                    }
                }
                let writes = [a];
                self.join(op, at, i, if store { &[] } else { &writes });
            }
            Kind::LdStImm | Kind::Pair => {
                let pair = op.kind == Kind::Pair;
                let f = op.imm2 as u32;
                let (scale, access, mode) = (f & 3, (f >> 2) & 3, (f >> 4) & 3);
                let n = 1u32 << scale;
                let base = self.xsp(b);
                let moved = self.bin_imm(self::op::I64_ADD, base, op.imm as i64);
                let (va, wb) = match mode {
                    0 | 2 => (moved, None),
                    1 => (base, Some(moved)),
                    _ => (moved, Some(moved)),
                };
                let store = access == 0;
                let vals = store.then(|| (self.xr(a, true), pair.then(|| self.xr(cc, true))));
                let (ok, entry) = self.probe(va, if pair { 2 * n } else { n }, store);
                self.fast_or_slow(ok);
                if let Some((x, y)) = vals {
                    self.host(entry, va);
                    self.store_n(x, n, 0);
                    if let Some(y) = y {
                        self.host(entry, va);
                        self.store_n(y, n, n);
                    }
                    self.bump(entry);
                    if let Some(wb) = wb {
                        self.put_xsp(b, wb);
                    }
                } else {
                    let signed = |access: u32| if pair { access == 2 } else { access >= 2 };
                    self.host(entry, va);
                    let mut first = self.load_n(n, 0);
                    let mut second = None;
                    if pair {
                        self.host(entry, va);
                        second = Some(self.load_n(n, n));
                    }
                    if signed(access) {
                        first = self.sext(first, 8 * n);
                        second = second.map(|s| self.sext(s, 8 * n));
                        if !pair && access == 3 {
                            first = self.mask(first, false);
                        }
                    }
                    if let Some(wb) = wb {
                        self.put_xsp(b, wb);
                    }
                    self.put_reg(a, first);
                    if let Some(s) = second {
                        self.put_reg(cc, s);
                    }
                }
                let mut writes = vec![];
                if !store {
                    writes.push(a);
                    if pair {
                        writes.push(cc);
                    }
                }
                if wb.is_some() {
                    writes.push(b);
                }
                self.join(op, at, i, &writes);
            }
            _ => return false,
        }
        true
    }
}

/// Instructions compiled inline and through their handlers, every core's.
pub static STATS_INLINE: AtomicU64 = AtomicU64::new(0);
pub static STATS_CALLED: AtomicU64 = AtomicU64::new(0);
