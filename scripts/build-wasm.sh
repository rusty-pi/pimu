#!/usr/bin/env bash
#
# Build the emulator for the browser into web/pkg/. Needs the wasm target and a
# wasm-bindgen CLI of the same version as the `wasm-bindgen` crate in Cargo.lock:
#
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version "$(scripts/build-wasm.sh --version)" --locked
#
# Then serve web/ with any static server (`python3 -m http.server -d web`).
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "${1:-}" == "--version" ]]; then
    awk '/name = "wasm-bindgen"$/ { getline; gsub(/[^0-9.]/, ""); print; exit }' Cargo.lock
    exit 0
fi

cargo rustc --release --lib --crate-type cdylib \
    --target wasm32-unknown-unknown --no-default-features
wasm-bindgen --target web --out-dir web/pkg \
    target/wasm32-unknown-unknown/release/pimu.wasm
