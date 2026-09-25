//! Peripheral register maps, generated from `specs/*.toml`.
//!
//! `build.rs` turns each spec into one module of constants here, so a register
//! offset is written down once, next to where it came from. Format:
//! `specs/README.md`; the same files produce `docs/periph/`.

use std::path::{Path, PathBuf};

pub mod schema;

include!(concat!(env!("OUT_DIR"), "/spec.rs"));

/// The offsets a device model actually decodes; anything else in its spec is
/// reported as stubbed by `tests/specs.rs`.
pub struct Coverage {
    pub block: &'static str,
    pub decoded: &'static [u32],
}

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

pub fn load() -> Result<Vec<schema::Spec>, String> {
    schema::load_dir(root())
}

pub fn doc_dir() -> PathBuf {
    root().join("docs/periph")
}

pub fn docs(specs: &[schema::Spec]) -> Vec<(String, String)> {
    let mut out = vec![("README.md".to_string(), schema::index_markdown(specs))];
    for spec in specs {
        out.push((format!("{}.md", spec.block.name), schema::markdown(spec)));
    }
    out
}

/// The files under `docs/periph/` that differ from what the specs generate;
/// with `update`, after fixing them.
pub fn sync_docs(update: bool) -> Result<Vec<String>, String> {
    let specs = load()?;
    let dir = doc_dir();
    let wanted = docs(&specs);
    let mut stale = Vec::new();

    for (name, text) in &wanted {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            stale.push(name.clone());
            if update {
                std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !wanted.iter().any(|(n, _)| *n == name) {
                stale.push(name);
                if update {
                    std::fs::remove_file(entry.path())
                        .map_err(|e| format!("{}: {e}", entry.path().display()))?;
                }
            }
        }
    }
    stale.sort();
    Ok(stale)
}
