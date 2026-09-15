//! `run`, `run-all` and `boot-check`: the regression scenarios
//! (`src/harness/`).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

use rpi_virt_fw::harness::{self, GoldenOutcome};

/// `run <scenario.toml>`: one in-process scenario against its golden
/// transcript.
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
    let path = path.context("run: missing <scenario.toml>")?;
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
        bail!("no *.toml scenarios in {}", dir.display());
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

/// `boot-check <scenario.toml> ...` — the firmware-boot regression.
///
/// Two modes, because the boot itself is expensive (minutes) and must be run
/// exactly once per check:
///
/// * `--plan` prints the `boot` invocation the scenario describes, for
///   `scripts/boot-check.sh` to run. The scenario file stays the only place
///   the workload is written down.
/// * `--log <combined.log> --console <console.bin>` checks that finished run:
///   the console against the golden transcript, the log against the
///   milestones. `--update` rewrites the golden instead of failing on it.
pub fn cmd_boot_check(args: &[String]) -> Result<ExitCode> {
    let mut path: Option<PathBuf> = None;
    let mut log: Option<PathBuf> = None;
    let mut console: Option<PathBuf> = None;
    let mut plan = false;
    let mut update = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--plan" => plan = true,
            "--update" => update = true,
            "--log" => log = Some(PathBuf::from(it.next().context("--log needs a path")?)),
            "--console" => {
                console = Some(PathBuf::from(it.next().context("--console needs a path")?))
            }
            s if !s.starts_with('-') => path = Some(PathBuf::from(s)),
            s => bail!("unexpected argument '{s}'"),
        }
    }
    let path = path.context("boot-check: missing <scenario.toml>")?;
    let scn = harness::BootScenario::load(&path)?;

    if plan {
        // Name what is missing, and how to make it, rather than plan a boot
        // that cannot open its card.
        let missing = scn.missing_inputs();
        if !missing.is_empty() {
            eprintln!("{}: the run needs files that are not there:", scn.name);
            // The network root and its key come out of one command.
            let mut make: Vec<&str> = Vec::new();
            for i in &missing {
                eprintln!("  {}", harness::boot::tidy_path(&i.path).display());
                if !make.contains(&i.make.as_str()) {
                    make.push(&i.make);
                }
            }
            let them = if missing.len() == 1 { "it" } else { "them" };
            eprintln!("make {them} with:");
            for m in make {
                eprintln!("  {m}");
            }
            return Ok(ExitCode::FAILURE);
        }
        // Shell-readable and quoting-proof: `wall=<n>` on the first line for
        // the outer timeout, then one `boot` argument per line.
        let console = console.unwrap_or_else(|| PathBuf::from("boot-console.bin"));
        println!("wall={}", scn.wall_secs());
        for a in scn.boot_args(&console) {
            println!("{a}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let log_path = log.context("boot-check: --log <path> (or --plan)")?;
    let log_text = std::fs::read_to_string(&log_path)
        .with_context(|| format!("reading run log {}", log_path.display()))?;
    let console_path = console.context("boot-check: --console <path> is required with --log")?;
    let console_bytes = std::fs::read(&console_path).with_context(|| {
        format!(
            "reading console log {} (boot writes it with --console-log)",
            console_path.display()
        )
    })?;
    let transcript = harness::boot::normalise_console(&console_bytes);

    if update {
        // Never record a bad run as the new truth. A boot that was starved of
        // CPU stops at the wall clock part-way through, and its transcript
        // looks like a perfectly good — and much shorter — boot.
        let milestones = harness::boot::check_milestones(&scn, &log_text);
        if !milestones.is_empty() {
            for f in &milestones {
                eprintln!("{f}");
            }
            eprintln!(
                "refusing to update the golden: this run failed {} milestone(s), so it is \
                 not a baseline. Fix the run (or raise RVF_BOOT_WALL if it was starved) first.",
                milestones.len()
            );
            return Ok(ExitCode::FAILURE);
        }
        harness::boot::write_golden(&scn, &transcript)?;
        println!(
            "updated golden {} ({} lines)",
            scn.golden_path().display(),
            transcript.lines().count()
        );
    }

    let failures = harness::boot::check_run(&scn, &log_text, &transcript)?;
    println!(
        "\n{}: {} milestone(s) + golden transcript ({} lines)",
        scn.name,
        scn.milestones.len(),
        transcript.lines().count()
    );
    if failures.is_empty() {
        println!("boot check passed");
        return Ok(ExitCode::SUCCESS);
    }
    for f in &failures {
        eprintln!("{f}");
    }
    eprintln!("boot check FAILED ({} problem(s))", failures.len());
    Ok(ExitCode::FAILURE)
}
