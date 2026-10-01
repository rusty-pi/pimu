//! `run` and `run-all`: the in-process regression scenarios (`src/harness/`).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

use pimu::harness::{self, GoldenOutcome};

/// `run <scenario.yaml>`: one in-process scenario against its golden transcript.
pub fn cmd_run(args: &[String]) -> Result<ExitCode> {
    let mut path: Option<PathBuf> = None;
    let mut update = false;
    let mut verbose = false;
    for a in args {
        match a.as_str() {
            "--update" => update = true,
            "-v" | "--verbose" => verbose = true,
            s if !s.starts_with('-') => path = Some(PathBuf::from(s)),
            s => bail!("unexpected argument '{s}'"),
        }
    }
    let path = path.context("run: missing <scenario.yaml>")?;
    let scn = harness::Scenario::load(&path)?;
    let outcome = run_one(&scn, update, verbose)?;
    Ok(if outcome {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// `run-all [<dir>]`: every scenario in `<dir>`.
pub fn cmd_run_all(args: &[String]) -> Result<ExitCode> {
    let mut dir = PathBuf::from("testdata/scenarios");
    let mut update = false;
    let mut verbose = false;
    for a in args {
        match a.as_str() {
            "--update" => update = true,
            "-v" | "--verbose" => verbose = true,
            s if !s.starts_with('-') => dir = PathBuf::from(s),
            s => bail!("unexpected argument '{s}'"),
        }
    }

    let files = harness::discover(&dir)
        .with_context(|| format!("discovering scenarios in {}", dir.display()))?;
    if files.is_empty() {
        bail!("no *.yaml scenarios in {}", dir.display());
    }

    let mut failed = 0;
    for f in &files {
        let scn = harness::Scenario::load(f)?;
        if !run_one(&scn, update, verbose)? {
            failed += 1;
        }
    }

    println!("\n{} scenario(s), {} failed", files.len(), failed);
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn run_one(scn: &harness::Scenario, update: bool, verbose: bool) -> Result<bool> {
    let run = harness::run_scenario(scn)?;
    let outcome = harness::check_golden(scn, &run.transcript)?;

    let (ok, tag, note) = match &outcome {
        GoldenOutcome::Match => (true, "PASS", String::new()),
        GoldenOutcome::Missing { .. } if update => {
            harness::regression::write_golden(scn, &run.transcript)?;
            (true, "NEW ", " (golden created)".into())
        }
        GoldenOutcome::Mismatch { .. } if update => {
            harness::regression::write_golden(scn, &run.transcript)?;
            (true, "UPD ", " (golden updated)".into())
        }
        GoldenOutcome::Missing { .. } => (false, "MISS", " (no golden; run --update)".into()),
        GoldenOutcome::Mismatch { expected, actual } => (
            false,
            "FAIL",
            format!("\n{}", harness::unified_diff(expected, actual)),
        ),
    };

    println!(
        "[{tag}] {:<24} {:>10} insn  end={:?}  stub={}  skipped={}{note}",
        scn.name, run.report.retired, run.report.end, run.report.stub_hits, run.report.skipped,
    );

    if verbose {
        println!("--- report ---\n{:#?}", run.report);
        println!("--- transcript ---\n{}", run.transcript);
    }

    Ok(ok)
}
