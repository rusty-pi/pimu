//! The YAML files under `testdata/` against the JSON Schemas in `schemas/`, the
//! same check an editor does through the `# yaml-language-server: $schema=`
//! comment on each file's first line.

use std::path::{Path, PathBuf};

use pimu::harness::schema::{schema_dir, schemas};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn yaml_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    out.sort();
    out
}

fn schema_named(name: &str) -> jsonschema::Validator {
    let text = std::fs::read_to_string(schema_dir().join(name)).unwrap();
    jsonschema::validator_for(&serde_json::from_str(&text).unwrap())
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn as_json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap();
    yaml_serde::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn declared_schema(path: &Path) -> PathBuf {
    let text = std::fs::read_to_string(path).unwrap();
    let first = text.lines().next().unwrap_or("");
    let rel = first
        .strip_prefix("# yaml-language-server: $schema=")
        .unwrap_or_else(|| panic!("{}: no schema comment on line 1", path.display()));
    path.parent().unwrap().join(rel)
}

fn check_dir(dir: &str, schema: &str, filter: fn(&Path) -> bool) {
    let validator = schema_named(schema);
    let files: Vec<_> = yaml_files(&root().join(dir))
        .into_iter()
        .filter(|p| filter(p))
        .collect();
    assert!(!files.is_empty(), "{dir}: nothing to check");
    for path in files {
        let errors: Vec<String> = validator
            .iter_errors(&as_json(&path))
            .map(|e| format!("{} at {}", e, e.instance_path()))
            .collect();
        assert!(errors.is_empty(), "{}: {errors:#?}", path.display());
        assert_eq!(
            declared_schema(&path).canonicalize().unwrap(),
            schema_dir().join(schema).canonicalize().unwrap(),
            "{}: names another schema",
            path.display()
        );
    }
}

#[test]
fn boot_scenarios_match_their_schema() {
    check_dir("testdata/boot", "boot-scenario.schema.json", |_| true);
}

#[test]
fn in_process_scenarios_match_their_schema() {
    check_dir("testdata/scenarios", "scenario.schema.json", |_| true);
}

#[test]
fn retired_counts_match_their_schema() {
    check_dir("testdata/boot/golden", "retired-counts.schema.json", |p| {
        p.to_string_lossy().ends_with(".retired.yaml")
    });
}

#[test]
fn a_misspelt_key_or_a_wrong_type_is_reported() {
    let validator = schema_named("boot-scenario.schema.json");
    let good = as_json(&root().join("testdata/boot/firmware.yaml"));
    assert!(validator.is_valid(&good));

    let mut typo = good.clone();
    typo["boot"]["wall_sec"] = 1.into();
    assert!(!validator.is_valid(&typo), "unknown key accepted");

    let mut wrong = good.clone();
    wrong["boot"]["wall_secs"] = "soon".into();
    assert!(!validator.is_valid(&wrong), "string for a number accepted");

    let mut bare = good;
    bare["milestones"][0].as_object_mut().unwrap().remove("why");
    assert!(
        !validator.is_valid(&bare),
        "milestone without a why accepted"
    );
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
