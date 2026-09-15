//! Generates the register constants `src/spec/mod.rs` includes, from
//! `specs/*.toml` (#39).
//! A malformed spec fails the build.

use std::path::PathBuf;

#[allow(dead_code)]
#[path = "src/spec/schema.rs"]
mod schema;

fn main() {
    println!("cargo:rerun-if-changed=specs");
    println!("cargo:rerun-if-changed=src/spec/schema.rs");

    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let specs = match schema::load_dir(&root) {
        Ok(specs) => specs,
        Err(e) => panic!("malformed register spec:\n{e}"),
    };

    let code: String = specs.iter().map(schema::rust_module).collect();
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("spec.rs");
    std::fs::write(out, code).unwrap();
}
