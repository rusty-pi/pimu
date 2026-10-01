//! Every scenario in `testdata/scenarios/` must still reproduce its golden
//! transcript. What this covers is the plumbing behind `pimu run-all` —
//! discovery, YAML loading, payload building, console capture, golden diff —
//! rather than the model itself. The firmware boot needs uncommitted blobs and
//! minutes of CPU, so `pimu boot-check` runs it and `tests/boot_scenario.rs`
//! covers the rest of it.
//!
//! Regenerate goldens after an intentional change: `cargo run -- run-all --update`.

use std::path::Path;

use pimu::harness;

#[test]
fn all_scenarios_match_golden() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/scenarios");
    let files = harness::discover(&dir).expect("discover scenarios");
    assert!(
        !files.is_empty(),
        "no scenarios found under {}",
        dir.display()
    );

    let mut failures = Vec::new();
    for f in files {
        let scn = harness::Scenario::load(&f).expect("load scenario");
        match harness::verify(&scn, false) {
            Ok(run) => {
                if run.report.bus_errors != 0 {
                    failures.push(format!(
                        "{}: {} bus errors during run",
                        scn.name, run.report.bus_errors
                    ));
                }
            }
            Err(e) => failures.push(format!("{}:\n{e:#}", scn.name)),
        }
    }

    assert!(
        failures.is_empty(),
        "scenario failures:\n\n{}",
        failures.join("\n\n")
    );
}
