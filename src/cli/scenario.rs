//! `run`, `run-all` and `boot-check`: the regression scenarios (`src/harness/`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use pimu::harness::boot::{GoldenCheck, RetiredCounts};
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

/// `boot-check <scenario.yaml>`: the firmware regression. Boots once — it takes
/// minutes — and checks the console against the golden transcript, the combined
/// output against the milestones, and the pinned retired counts. `--update`
/// rewrites the golden and the counts; `--from <log>` checks a pair an earlier
/// run left; `--plan` prints the `boot` invocation instead of running it, so the
/// scenario file stays the only place the workload is written down.
pub fn cmd_boot_check(args: &[String]) -> Result<ExitCode> {
    let mut path: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut from: Option<PathBuf> = None;
    let mut max_wall: Option<u64> = None;
    let mut plan = false;
    let mut update = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--plan" => plan = true,
            "--update" => update = true,
            "--output" => output = Some(PathBuf::from(it.next().context("--output needs a path")?)),
            "--from" => from = Some(PathBuf::from(it.next().context("--from needs a path")?)),
            "--max-wall" => {
                let secs = it.next().context("--max-wall needs seconds")?;
                max_wall = Some(
                    secs.parse()
                        .with_context(|| format!("--max-wall {secs}: not a number of seconds"))?,
                );
            }
            s if !s.starts_with('-') => path = Some(PathBuf::from(s)),
            s => bail!("unexpected argument '{s}'"),
        }
    }
    if from.is_some() && (plan || output.is_some() || max_wall.is_some()) {
        bail!("--from checks an earlier run: it takes no --plan, --output or --max-wall");
    }
    if std::env::var_os("PIMU_BOOT_WALL").is_some() {
        eprintln!("warning: PIMU_BOOT_WALL is gone, use boot-check --max-wall <secs>");
    }
    let path = path.context("boot-check: missing <scenario.yaml>")?;
    let mut scn = harness::BootScenario::load(&path)?;
    if let Some(secs) = max_wall {
        scn.boot.wall_secs = secs;
    }
    // Named after the scenario: a fixed `boot.log` would be shared state, and two
    // checks at once in one checkout would each judge a mix of both runs.
    let log = from
        .as_ref()
        .or(output.as_ref())
        .cloned()
        .unwrap_or_else(|| PathBuf::from(format!("boot-{}.log", scn.name)));
    let mut console = log.clone().into_os_string();
    console.push(".console");
    let console = PathBuf::from(console);

    if from.is_none() {
        if !inputs_present(&scn) {
            return Ok(ExitCode::FAILURE);
        }
        if plan {
            println!("wall={}", scn.wall_secs());
            for a in scn.boot_args(&console) {
                println!("{a}");
            }
            return Ok(ExitCode::SUCCESS);
        }
        if let Some(failed) = run_boot(&scn, &log, &console)? {
            return Ok(failed);
        }
    }
    check_boot(&scn, &log, &console, update)
}

/// Name the files the run needs and does not have, with the command that makes each.
fn inputs_present(scn: &harness::BootScenario) -> bool {
    let missing = scn.missing_inputs();
    if missing.is_empty() {
        return true;
    }
    eprintln!("{}: the run needs files that are not there:", scn.name);
    let mut make: Vec<&str> = Vec::new();
    for i in &missing {
        eprintln!("  {}", harness::boot::tidy_path(&i.path).display());
        if !make.contains(&i.make.as_str()) {
            make.push(&i.make);
        }
    }
    if !Path::new("scripts/make-sd.sh").exists() {
        return false;
    }
    let them = if missing.len() == 1 { "it" } else { "them" };
    eprintln!("make {them} with:");
    for m in make {
        eprintln!("  {m}");
    }
    false
}

struct Tee<A, B>(A, B);

impl<A: Write, B: Write> Write for Tee<A, B> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write_all(buf)?;
        self.0.flush()?;
        self.1.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()?;
        self.1.flush()
    }
}

/// Boot the scenario as a child of this binary. Its stdout and stderr share one
/// pipe, so `log` gets them interleaved the way the milestones read them.
fn run_boot(scn: &harness::BootScenario, log: &Path, console: &Path) -> Result<Option<ExitCode>> {
    // A boot that cannot start writes no console; never diff a stale one.
    if let Err(e) = std::fs::remove_file(console) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e).with_context(|| format!("removing {}", console.display()));
        }
    }
    let mut file =
        std::fs::File::create(log).with_context(|| format!("creating {}", log.display()))?;
    let (mut output, input) = std::io::pipe().context("making a pipe for the boot's output")?;
    let mut child = {
        let mut cmd = Command::new(std::env::current_exe().context("finding this binary")?);
        cmd.args(scn.boot_args(console))
            .stdout(input.try_clone().context("sharing the pipe")?)
            .stderr(input);
        cmd.spawn().context("starting the boot")?
        // `cmd` drops here with this side's write ends, so the copy below ends when the boot's do.
    };

    // For a boot that does not stop itself at its wall budget: 40 s more, then a SIGINT.
    let (done, finished) = mpsc::channel::<()>();
    let (pid, budget) = (child.id(), Duration::from_secs(scn.wall_secs() + 40));
    let watchdog = std::thread::spawn(move || {
        let late = finished.recv_timeout(budget) == Err(mpsc::RecvTimeoutError::Timeout);
        if late {
            let _ = Command::new("kill")
                .args(["-INT", &pid.to_string()])
                .status();
        }
        late
    });
    let copied = std::io::copy(&mut output, &mut Tee(std::io::stdout(), &mut file));
    let status = child.wait().context("waiting for the boot")?;
    let _ = done.send(());
    let late = watchdog.join().unwrap_or(false);
    copied.context("copying the boot's output")?;

    println!(
        "boot {status}{}",
        if late { ", past its wall budget" } else { "" }
    );
    // 0 and 1 are both for the milestones to judge; any other code is not.
    if !late && !matches!(status.code(), Some(0 | 1)) {
        let code = status
            .code()
            .and_then(|c| u8::try_from(c).ok())
            .unwrap_or(1);
        return Ok(Some(ExitCode::from(code)));
    }
    if !console.exists() {
        eprintln!(
            "the boot wrote no console ({}): see the error above; nothing to check",
            console.display()
        );
        return Ok(Some(ExitCode::FAILURE));
    }
    Ok(None)
}

/// Check a finished run against the golden transcript, the milestones and the retired counts.
fn check_boot(
    scn: &harness::BootScenario,
    log_path: &Path,
    console_path: &Path,
    update: bool,
) -> Result<ExitCode> {
    let log_text = std::fs::read_to_string(log_path)
        .with_context(|| format!("reading run log {}", log_path.display()))?;
    let console_bytes = std::fs::read(console_path).with_context(|| {
        format!(
            "reading console log {} (boot writes it with --console-log)",
            console_path.display()
        )
    })?;
    let transcript = harness::boot::normalise_console(&console_bytes);

    if update && scn.golden.is_none() {
        bail!("{}: no `golden` to update", scn.name);
    }
    if update {
        // Never record a bad run as truth: a CPU-starved boot stops at the wall
        // clock and its short transcript still looks like a good boot.
        let milestones = harness::boot::check_milestones(scn, &log_text);
        if !milestones.is_empty() {
            for f in &milestones {
                eprintln!("{f}");
            }
            eprintln!(
                "refusing to update the golden: this run failed {} milestone(s), so it is \
                 not a baseline. Fix the run (or raise --max-wall if it was starved) first.",
                milestones.len()
            );
            return Ok(ExitCode::FAILURE);
        }
        harness::boot::write_golden(scn, &transcript)?;
        println!(
            "updated golden {} ({} lines)",
            scn.golden_path().unwrap_or_default().display(),
            transcript.lines().count()
        );
        let counts = RetiredCounts::from_log(&log_text)
            .context("the run log has no retired counts to record")?;
        let changed = match harness::boot::check_retired(scn, &counts) {
            Ok(GoldenCheck::Mismatch(diff)) => diff,
            _ => String::new(),
        };
        harness::boot::write_retired(scn, &counts)?;
        if let Some(path) = scn.retired_path() {
            println!("updated retired counts {}", path.display());
        }
        for line in changed.lines() {
            println!("  {line}");
        }
    }

    let failures = harness::boot::check_run(scn, &log_text, &transcript)?;
    println!(
        "\n{}: {} milestone(s) + golden transcript ({} lines) + retired counts ({} core(s))",
        scn.name,
        scn.milestones.len(),
        transcript.lines().count(),
        RetiredCounts::from_log(&log_text).map_or(0, |c| c.0.len())
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
