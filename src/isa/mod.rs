//! The VPU instruction-set reference, generated from `isa/vpu.toml`.
//!
//! The same idea as the peripheral specs in [`crate::spec`]: every statement
//! about the instruction set is written down once, beside the evidence for it,
//! and `docs/vpu-isa.md` is rendered from that rather than edited by hand.

use std::path::{Path, PathBuf};

pub mod schema;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Load and validate the instruction-set spec.
pub fn load() -> Result<schema::Isa, String> {
    schema::load(&root().join("isa/vpu.toml"))
}

/// The generated Markdown's path.
pub fn doc_path() -> PathBuf {
    root().join("docs/vpu-isa.md")
}

/// Compare `docs/vpu-isa.md` with what the spec generates; with `update`,
/// rewrite it. Returns the path when it is out of date.
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
