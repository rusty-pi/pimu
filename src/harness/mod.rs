//! The regression bench: scenario files in, pass/fail (+ transcript) out.

pub mod capture;
pub mod regression;
pub mod scenario;

pub use regression::{
    build_payload, check_golden, run_scenario, unified_diff, verify, GoldenOutcome, ScenarioRun,
};
pub use scenario::Scenario;

use std::path::{Path, PathBuf};

/// Collect `*.toml` scenario files under `dir`, sorted by name.
pub fn discover(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}
