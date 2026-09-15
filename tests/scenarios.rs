//! The scenario harness itself: every scenario in `testdata/scenarios/` must
//! still reproduce its golden transcript.
//!
//! This is not much of a test of the *model* — the scenarios are hand-assembled
//! payloads that touch a dozen instructions and one UART, and none of the
//! firmware bugs this project has hit would show up here. What it does cover is
//! the plumbing behind `rpi-virt-fw run-all`: scenario discovery, TOML loading,
//! payload building, console capture and the golden diff. That plumbing has no
//! other test and this one costs a millisecond, so it stays.
//!
//! The firmware boot is a scenario too, but a different kind: it needs blobs
//! that are never committed and minutes of CPU, so it lives in
//! `testdata/boot/firmware-boot.toml`, `rpi-virt-fw boot-check` runs it, and
//! `tests/boot_scenario.rs` tests everything about it that does not need the
//! boot. These two stay because they are the only end-to-end exercise of
//! payload loading, the run loop and console capture that `cargo test` can
//! afford.
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
