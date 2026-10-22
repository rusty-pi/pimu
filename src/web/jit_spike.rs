//! A spike for a JIT that emits wasm (#209): build a module at run time,
//! install its functions in this module's table and call them from Rust, and
//! call back into Rust from them through the same table. What it measures is
//! what decides whether compiled runs can pay for themselves in a browser.

use js_sys::{Function, Object, Reflect, Uint8Array, WebAssembly};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_time::Instant;

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

/// What each function of a spike module does with its `i64` argument.
#[derive(Clone, Copy)]
enum Body {
    /// Adds 1, this many times.
    Adds(u32),
    /// Passes it through the function at this table index, this many times.
    Calls(u32, u32),
    /// Loads the `i64` at that address.
    Peek,
}

/// A module of `funcs` functions `f0`.., each `(i64) -> i64`, that imports
/// `env.memory` and `env.table` from this module.
fn module(funcs: u32, body: Body) -> Vec<u8> {
    const I64: u8 = 0x7E;
    let mut m = b"\0asm\x01\0\0\0".to_vec();
    let mut types = Vec::new();
    leb_u(&mut types, 1);
    types.extend_from_slice(&[0x60, 1, I64, 1, I64]);
    section(&mut m, 1, &types);

    let mut imports = Vec::new();
    leb_u(&mut imports, 2);
    name(&mut imports, "env");
    name(&mut imports, "memory");
    imports.push(0x02);
    if cfg!(pimu_threads) {
        // Shared, as the threaded build's is, up to its 4 GiB.
        imports.push(0x03);
        leb_u(&mut imports, 1);
        leb_u(&mut imports, 65536);
    } else {
        imports.push(0x00);
        leb_u(&mut imports, 1);
    }
    name(&mut imports, "env");
    name(&mut imports, "table");
    imports.extend_from_slice(&[0x01, 0x70, 0x00]);
    leb_u(&mut imports, 1);
    section(&mut m, 2, &imports);

    let mut fs = Vec::new();
    leb_u(&mut fs, funcs.into());
    for _ in 0..funcs {
        fs.push(0);
    }
    section(&mut m, 3, &fs);

    let mut exports = Vec::new();
    leb_u(&mut exports, funcs.into());
    for i in 0..funcs {
        name(&mut exports, &format!("f{i}"));
        exports.push(0x00);
        leb_u(&mut exports, i.into());
    }
    section(&mut m, 7, &exports);

    let mut code = Vec::new();
    leb_u(&mut code, funcs.into());
    for _ in 0..funcs {
        let mut f = vec![0];
        f.extend_from_slice(&[0x20, 0]);
        match body {
            Body::Adds(n) => {
                for _ in 0..n {
                    f.push(0x42);
                    leb_s(&mut f, 1);
                    f.push(0x7C);
                }
            }
            Body::Calls(at, n) => {
                for _ in 0..n {
                    f.push(0x41);
                    leb_s(&mut f, at.into());
                    f.extend_from_slice(&[0x11, 0, 0]);
                }
            }
            Body::Peek => f.extend_from_slice(&[0xA7, 0x29, 3, 0]),
        }
        f.push(0x0B);
        leb_u(&mut code, f.len() as u64);
        code.extend_from_slice(&f);
    }
    section(&mut m, 10, &code);
    m
}

type Compiled = extern "C" fn(i64) -> i64;

struct Installed {
    compile_us: f64,
    instantiate_us: f64,
    install_us: f64,
    fns: Vec<Compiled>,
}

fn install(bytes: &[u8], funcs: u32) -> Result<Installed, JsValue> {
    let t = Instant::now();
    let module = WebAssembly::Module::new(&Uint8Array::from(bytes).into())?;
    let compile_us = t.elapsed().as_secs_f64() * 1e6;
    let t = Instant::now();
    let env = Object::new();
    Reflect::set(&env, &"memory".into(), &wasm_bindgen::memory())?;
    Reflect::set(&env, &"table".into(), &wasm_bindgen::function_table())?;
    let imports = Object::new();
    Reflect::set(&imports, &"env".into(), &env)?;
    let instance = WebAssembly::Instance::new(&module, &imports)?;
    let instantiate_us = t.elapsed().as_secs_f64() * 1e6;
    let t = Instant::now();
    let exports = instance.exports();
    let table: WebAssembly::Table = wasm_bindgen::function_table().unchecked_into();
    let base = table.grow(funcs)?;
    let mut fns = Vec::with_capacity(funcs as usize);
    for i in 0..funcs {
        let f: Function = Reflect::get(&exports, &format!("f{i}").into())?.unchecked_into();
        table.set(base + i, &f)?;
        // A function pointer in wasm32 is its index in this module's table,
        // where the export now is, with the type `Compiled` names.
        fns.push(unsafe { std::mem::transmute::<usize, Compiled>((base + i) as usize) });
    }
    Ok(Installed {
        compile_us,
        instantiate_us,
        install_us: t.elapsed().as_secs_f64() * 1e6,
        fns,
    })
}

#[inline(never)]
extern "C" fn plus_one(x: i64) -> i64 {
    x.wrapping_add(1)
}

/// Nanoseconds a call of `f`, over `n` calls.
fn per_call(f: Compiled, n: u32) -> f64 {
    let t = Instant::now();
    let mut x = 0i64;
    for _ in 0..n {
        x = std::hint::black_box(f)(x);
    }
    std::hint::black_box(x);
    t.elapsed().as_secs_f64() * 1e9 / f64::from(n)
}

/// The spike's measurements, as JSON.
#[wasm_bindgen]
pub fn jit_spike() -> Result<String, JsValue> {
    let mut out = Vec::new();
    for &(funcs, adds) in &[(1u32, 10u32), (16, 10), (128, 10), (1024, 10), (1, 1000), (128, 100)] {
        let bytes = module(funcs, Body::Adds(adds));
        let i = install(&bytes, funcs)?;
        let right = (i.fns[0])(5) == 5 + i64::from(adds);
        out.push(format!(
            r#"{{"what":"module","funcs":{funcs},"adds":{adds},"bytes":{},"compile_us":{:.1},"instantiate_us":{:.1},"install_us":{:.1},"right":{right}}}"#,
            bytes.len(),
            i.compile_us,
            i.instantiate_us,
            i.install_us
        ));
    }

    let mut word = 0x1234_5678_9ABC_DEF0i64;
    let peek = install(&module(1, Body::Peek), 1)?.fns[0];
    let seen = peek(std::ptr::addr_of_mut!(word) as usize as i64);
    out.push(format!(r#"{{"what":"peek","right":{}}}"#, seen == word));

    let one = install(&module(1, Body::Adds(1)), 1)?.fns[0];
    let helper = plus_one as *const () as usize as u32;
    let calls = install(&module(1, Body::Calls(helper, 100)), 1)?.fns[0];
    out.push(format!(r#"{{"what":"helper","right":{}}}"#, calls(0) == 100));
    for round in 0..6 {
        let rust = per_call(plus_one, 2_000_000);
        let compiled = per_call(one, 2_000_000);
        let back = per_call(calls, 20_000) / 100.0;
        out.push(format!(
            r#"{{"what":"calls","round":{round},"rust_to_rust_ns":{rust:.2},"rust_to_compiled_ns":{compiled:.2},"compiled_to_rust_ns":{back:.2}}}"#
        ));
    }
    Ok(format!("[{}]", out.join(",")))
}
