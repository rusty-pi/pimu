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

    // The run loop's wall budget, baked in. A guest cannot ask QEMU how long
    // the host's `timeout` will let it run, so the number has to travel in the
    // image — and it must be under the host's, or a wedged boot is killed with
    // no report instead of stopping itself and printing one. 20 seconds of
    // slack covers the `.bss` clear, the banner and the final milestone dump.
    // Default 1800: a full boot retires about a billion VPU instructions and
    // the interpreter is itself running under TCG here.
    let wall: u64 = std::env::var("RVF_KERNEL_WALL")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(1800);
    println!(
        "cargo::rustc-env=RVF_IMAGE_WALL_SECS={}",
        wall.saturating_sub(20).max(5)
    );
    println!("cargo::rerun-if-env-changed=RVF_KERNEL_WALL");
}
