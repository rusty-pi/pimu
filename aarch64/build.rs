use std::path::PathBuf;

fn main() {
    // Absolute path rather than `-Tlink.ld`: cargo does not promise which
    // directory the linker is spawned from, and a relative script that silently
    // fails to resolve gives you a default-layout image that QEMU loads and
    // then jumps into nothing.
    let script = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("link.ld");
    println!("cargo::rustc-link-arg=-T{}", script.display());
    // The image is a flat blob: no dynamic loader, no program-header games.
    println!("cargo::rustc-link-arg=--no-dynamic-linker");
    println!("cargo::rerun-if-changed={}", script.display());
    println!("cargo::rerun-if-changed=build.rs");
}
