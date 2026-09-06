//! Every scenario in `testdata/scenarios/` must reproduce its golden transcript.
//!
//! Regenerate goldens after an intentional behaviour change with:
//!   cargo run -- run-all --update

use std::path::Path;

use rpi_virt_fw::harness;

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
                assert!(
                    run.report.bus_errors == 0,
                    "{}: {} bus errors during run",
                    scn.name,
                    run.report.bus_errors
                );
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
