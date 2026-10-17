//! The emulator in a browser tab.
//!
//! [`options`] and [`session`] are plain Rust, so they build and test on the
//! host; `bindings` is the `wasm-bindgen` surface a page calls, and exists only
//! for `wasm32`. A page hands over the EEPROM image, the files of a boot
//! partition (or disk images it reads a range at a time), and a JSON of
//! [`options::Options`]; then it calls `Pi.run` for a slice of wall time at a
//! time, from a Web Worker, and shows what comes back.

pub mod options;
pub mod session;

#[cfg(target_arch = "wasm32")]
mod bindings;
#[cfg(target_arch = "wasm32")]
mod source;
