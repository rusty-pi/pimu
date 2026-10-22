#!/usr/bin/env bash
#
# Build the emulator for the browser into web/pkg/ and web/pkg-mt/. Needs the
# wasm target and a wasm-bindgen CLI of the same version as the `wasm-bindgen`
# crate in Cargo.lock:
#
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version "$(scripts/build-wasm.sh --version)" --locked
#
# web/pkg/ is the plain build, on the pinned stable toolchain. web/pkg-mt/ is
# the same code linked for shared memory, so the ARM cores can run on worker
# threads (`--parallel`, docs/web.md). That needs `std` rebuilt with atomics,
# which only a nightly does:
#
#   rustup toolchain install "$(scripts/build-wasm.sh --nightly)" --component rust-src --target wasm32-unknown-unknown
#
# `--no-threads` builds web/pkg/ alone. Then serve web/ with any static server
# (`python3 -m http.server -d web`); the threaded build only runs on a page
# that is cross-origin isolated (web/coi-serviceworker.mjs arranges that).
set -euo pipefail
cd "$(dirname "$0")/.."

# Pinned, so a nightly's churn never breaks the site build on its own.
nightly="${PIMU_WASM_NIGHTLY:-nightly-2026-10-09}"

if [[ "${1:-}" == "--version" ]]; then
    awk '/name = "wasm-bindgen"$/ { getline; gsub(/[^0-9.]/, ""); print; exit }' Cargo.lock
    exit 0
fi
if [[ "${1:-}" == "--nightly" ]]; then
    echo "$nightly"
    exit 0
fi

cargo rustc --release --lib --crate-type cdylib \
    --target wasm32-unknown-unknown --no-default-features
wasm-bindgen --target web --out-dir web/pkg \
    target/wasm32-unknown-unknown/release/pimu.wasm

if [[ "${1:-}" == "--no-threads" ]]; then
    exit 0
fi

# Shared memory of up to 4 GiB, the most a 32-bit memory can have; the TLS
# exports are what wasm-bindgen sets each thread up with. Tail calls carry a
# straight run from one instruction's handler into the next (`pimu_tail` in
# build.rs) instead of a call frame per instruction.
threads_flags=(
    -C target-feature=+atomics,+bulk-memory,+mutable-globals,+tail-call
    -C link-arg=--shared-memory
    -C link-arg=--import-memory
    -C link-arg=--max-memory=4294967296
    -C link-arg=--export=__wasm_init_tls
    -C link-arg=--export=__tls_size
    -C link-arg=--export=__tls_align
    -C link-arg=--export=__tls_base
)
RUSTFLAGS="${threads_flags[*]}" cargo "+$nightly" rustc --release --lib --crate-type cdylib \
    --target wasm32-unknown-unknown --no-default-features \
    -Z build-std=std,panic_abort --target-dir target/wasm-mt
wasm-bindgen --target web --out-dir web/pkg-mt \
    target/wasm-mt/wasm32-unknown-unknown/release/pimu.wasm
