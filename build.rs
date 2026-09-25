//! Generates the constants the model includes from the specs beside it:
//! the peripheral register maps in `specs/*.toml` (#39), and the instruction
//! set's sub-op tables in `isa/vpu.toml` (#118). A malformed spec fails the
//! build.

use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[path = "src/spec/schema.rs"]
pub mod schema;

/// `src/isa/schema.rs` reads its `Source` type out of `crate::spec::schema`,
/// which is where the library keeps it; give the build script the same path.
mod spec {
    pub use crate::schema;
}

#[allow(dead_code)]
#[path = "src/isa/schema.rs"]
mod isa_schema;

fn main() {
    println!("cargo:rerun-if-changed=specs");
    println!("cargo:rerun-if-changed=src/spec/schema.rs");
    println!("cargo:rerun-if-changed=isa/vpu.toml");
    println!("cargo:rerun-if-changed=src/isa/schema.rs");

    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let specs = match schema::load_dir(&root) {
        Ok(specs) => specs,
        Err(e) => panic!("malformed register spec:\n{e}"),
    };

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let code: String = specs.iter().map(schema::rust_module).collect();
    std::fs::write(out_dir.join("spec.rs"), code).unwrap();

    let isa = match isa_schema::load(&root.join("isa/vpu.toml")) {
        Ok(isa) => isa,
        Err(e) => panic!("malformed instruction-set spec:\n{e}"),
    };
    std::fs::write(out_dir.join("isa.rs"), isa_schema::rust_module(&isa)).unwrap();

    embed_eeprom(&out_dir);
}

/// The EEPROM image a binary carries, so that it boots a medium with nothing to
/// download: `PIMU_EMBED_EEPROM=<pieeprom.bin>` names one, which is what the
/// release build does with `rusty-pi/pi4-firmware`'s
/// (`.github/workflows/release.yml`). Unset — every build from a checkout — the
/// binary carries none and fetches the image with `gh` instead.
fn embed_eeprom(out_dir: &Path) {
    println!("cargo:rerun-if-env-changed=PIMU_EMBED_EEPROM");
    let image = match std::env::var_os("PIMU_EMBED_EEPROM") {
        Some(p) if !p.is_empty() => {
            let path = PathBuf::from(&p);
            let path = std::fs::canonicalize(&path)
                .unwrap_or_else(|e| panic!("PIMU_EMBED_EEPROM={}: {e}", path.display()));
            println!("cargo:rerun-if-changed={}", path.display());
            format!("Some(include_bytes!({path:?}))")
        }
        _ => "None".to_string(),
    };
    std::fs::write(
        out_dir.join("embedded_eeprom.rs"),
        format!("pub const EMBEDDED_EEPROM: Option<&[u8]> = {image};\n"),
    )
    .unwrap();
}
