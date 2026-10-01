//! JSON Schemas for the YAML files the bench reads, so an editor or a linter
//! can check them without running `pimu`. They live in `schemas/` and each file
//! points at its schema with a `# yaml-language-server: $schema=` comment.

use std::path::{Path, PathBuf};

use schemars::schema_for;
use serde_json::{json, Value};

use super::{BootScenario, Scenario};

pub fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas")
}

fn retired_counts() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "RetiredCounts",
        "description": "Instructions each core retired in a pinned boot: `vpu0` and `vpu1` on the VideoCore, then `arm0`.. once the ARM is released.",
        "type": "object",
        "propertyNames": { "pattern": "^(vpu|arm)[0-9]+$" },
        "additionalProperties": { "type": "integer", "minimum": 0 }
    })
}

/// Every schema as the file name it is kept under and its text.
pub fn schemas() -> Vec<(&'static str, String)> {
    let render = |v: Value| format!("{}\n", serde_json::to_string_pretty(&v).expect("schema"));
    let of = |s: schemars::Schema| render(s.to_value());
    vec![
        ("boot-scenario.schema.json", of(schema_for!(BootScenario))),
        ("scenario.schema.json", of(schema_for!(Scenario))),
        ("retired-counts.schema.json", render(retired_counts())),
    ]
}

/// The schema files that differ from what the types say, rewriting them when
/// `update` is set.
pub fn sync(update: bool) -> Result<Vec<PathBuf>, String> {
    let dir = schema_dir();
    let mut stale = Vec::new();
    for (name, text) in schemas() {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
            continue;
        }
        if update {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        stale.push(path);
    }
    Ok(stale)
}
