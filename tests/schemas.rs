//! The YAML files under `testdata/` and the JSON Schemas in `schemas/`. Each
//! file names its schema on line 1 (`# yaml-language-server: $schema=`), which
//! is what an editor reads. The schemas come from the same Rust types the
//! files are loaded into, whose unknown-key and type checks are what a file has
//! to pass, so loading every file is the validation.

use std::path::{Path, PathBuf};

use pimu::harness::schema::{schema_dir, schemas};
use pimu::harness::{BootScenario, Scenario};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn yaml_files(dir: &str, suffix: &str) -> Vec<PathBuf> {
    let dir = root().join(dir);
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(suffix))
        .collect();
    out.sort();
    assert!(!out.is_empty(), "{}: nothing to check", dir.display());
    out
}

fn assert_names_schema(path: &Path, schema: &str) {
    let text = std::fs::read_to_string(path).unwrap();
    let first = text.lines().next().unwrap_or("");
    let rel = first
        .strip_prefix("# yaml-language-server: $schema=")
        .unwrap_or_else(|| panic!("{}: no schema comment on line 1", path.display()));
    let named = path.parent().unwrap().join(rel);
    assert_eq!(
        named
            .canonicalize()
            .unwrap_or_else(|e| panic!("{}: {e}", named.display())),
        schema_dir().join(schema).canonicalize().unwrap(),
        "{}: names another schema",
        path.display()
    );
}

#[test]
fn boot_scenarios_load_and_name_their_schema() {
    for path in yaml_files("testdata/boot", ".yaml") {
        BootScenario::load(&path).unwrap_or_else(|e| panic!("{e:#}"));
        assert_names_schema(&path, "boot-scenario.schema.json");
    }
}

#[test]
fn in_process_scenarios_load_and_name_their_schema() {
    for path in yaml_files("testdata/scenarios", ".yaml") {
        Scenario::load(&path).unwrap_or_else(|e| panic!("{e:#}"));
        assert_names_schema(&path, "scenario.schema.json");
    }
}

#[test]
fn retired_counts_parse_and_name_their_schema() {
    for path in yaml_files("testdata/boot/golden", ".retired.yaml") {
        let text = std::fs::read_to_string(&path).unwrap();
        pimu::harness::boot::RetiredCounts::parse(&text).unwrap_or_else(|e| panic!("{e:#}"));
        assert_names_schema(&path, "retired-counts.schema.json");
    }
}

#[test]
fn a_misspelt_key_a_wrong_type_or_a_milestone_without_a_why_is_refused() {
    let good = std::fs::read_to_string(root().join("testdata/boot/firmware.yaml")).unwrap();
    assert!(yaml_serde::from_str::<BootScenario>(&good).is_ok());
    let cases = [
        ("wall_secs:", "wall_sec:"),
        ("wall_secs: ", "wall_secs: soon # "),
        ("  - why:", "  - whom:"),
    ];
    for (from, to) in cases {
        let bad = good.replacen(from, to, 1);
        assert_ne!(bad, good, "{from} not found");
        assert!(
            yaml_serde::from_str::<BootScenario>(&bad).is_err(),
            "{to} accepted"
        );
    }
}

#[test]
fn the_schemas_are_current() {
    let stale = pimu::harness::schema::sync(false).unwrap();
    assert!(
        stale.is_empty(),
        "{stale:?} out of date; run `cargo run -- spec-docs --update`"
    );
    assert_eq!(schemas().len(), 3);
}
