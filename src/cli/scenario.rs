//! `run` and `run-all`: the in-process regression scenarios (`src/harness/`).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

use pimu::harness::{self, GoldenOutcome};

/// Run one scenario and check it against its golden transcript.
#[derive(clap::Args)]
pub struct RunArgs {
    #[arg(value_name = "scenario.yaml")]
    path: PathBuf,
    #[command(flatten)]
    golden: GoldenArgs,
}

/// Run every *.yaml scenario in <dir>.
#[derive(clap::Args)]
pub struct RunAllArgs {
    #[arg(default_value = "testdata/scenarios")]
    dir: PathBuf,
    #[command(flatten)]
    golden: GoldenArgs,
}

#[derive(clap::Args)]
struct GoldenArgs {
    /// Rewrite golden files instead of failing on mismatch.
    #[arg(long)]
    update: bool,
    /// Print the full report and transcript.
    #[arg(short, long)]
    verbose: bool,
}

pub fn cmd_run(args: RunArgs) -> Result<ExitCode> {
    let RunArgs { path, golden } = args;
    let GoldenArgs { update, verbose } = golden;
    let scn = harness::Scenario::load(&path)?;
    let outcome = run_one(&scn, update, verbose)?;
    Ok(if outcome {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

pub fn cmd_run_all(args: RunAllArgs) -> Result<ExitCode> {
    let RunAllArgs { dir, golden } = args;
    let GoldenArgs { update, verbose } = golden;

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
