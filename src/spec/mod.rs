//! Peripheral register maps, generated from `specs/*.toml` (#39).
//!
//! `build.rs` turns each spec into one module of constants here —
//! `spec::mcsync::DOORBELL`, `spec::systimer::CS_M0_MASK` and so on — so a
//! register offset is written down in exactly one place, next to where it came
//! from. The format is described in `specs/README.md`; the same files also
//! produce the Markdown under `docs/periph/` (`pimu spec-docs`).

use std::path::{Path, PathBuf};

pub mod schema;

include!(concat!(env!("OUT_DIR"), "/spec.rs"));

/// What a device model gives real behaviour to, out of its spec: the offsets
/// (bank 0, element 0) of the registers it decodes. Anything else in the spec
/// falls through to the device's default arm and is reported as stubbed by
/// `tests/specs.rs`.
pub struct Coverage {
    /// `block.name` of the spec.
    pub block: &'static str,
    pub decoded: &'static [u32],
}

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Load and validate every spec.
pub fn load() -> Result<Vec<schema::Spec>, String> {
    schema::load_dir(root())
}

/// Directory the generated Markdown lives in. It holds nothing else.
pub fn doc_dir() -> PathBuf {
    root().join("docs/periph")
}

/// Every generated Markdown file, as `(file name, contents)`.
pub fn docs(specs: &[schema::Spec]) -> Vec<(String, String)> {
    let mut out = vec![("README.md".to_string(), schema::index_markdown(specs))];
    for spec in specs {
        out.push((format!("{}.md", spec.block.name), schema::markdown(spec)));
    }
    out
}

/// Compare `docs/periph/` with what the specs generate and return the files
/// that differ, are missing or should not be there. With `update`, fix them.
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
