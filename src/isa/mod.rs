//! The VPU instruction-set reference, generated from `isa/vpu.toml`.
//!
//! As with the peripheral specs in [`crate::spec`], every statement is written
//! down once beside its evidence, and `docs/vpu-isa.md` is rendered from it.

use std::path::{Path, PathBuf};

pub mod schema;

include!(concat!(env!("OUT_DIR"), "/isa.rs"));

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

pub fn load() -> Result<schema::Isa, String> {
    schema::load(&root().join("isa/vpu.toml"))
}

pub fn doc_path() -> PathBuf {
    root().join("docs/vpu-isa.md")
}

/// Compare `docs/vpu-isa.md` with what the spec generates; `update` rewrites.
pub fn sync_docs(update: bool) -> Result<Vec<PathBuf>, String> {
    let isa = load()?;
    let text = schema::markdown(&isa);
    let path = doc_path();
    if std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
        return Ok(Vec::new());
    }
    if update {
        std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(vec![path])
}
