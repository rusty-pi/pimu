//! `run`, `run-all` and `boot-check`: the regression scenarios
//! (`src/harness/`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use rpi_virt_fw::harness::boot::{GoldenCheck, RetiredCounts};
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

/// `boot-check <scenario.toml>`: the firmware regression (#97).
///
/// Runs the boot the scenario describes, once — it takes minutes, and the wall
/// clock has little headroom — and checks what it left behind three ways: the
/// console against the golden transcript, the combined output against the
/// milestones and the pinned retired counts (#85). `--update` rewrites the
/// golden and the counts instead of failing on them.
///
/// * `--output <log>`: where the combined stdout and stderr go
///   (`boot-<scenario>.log` when not given), with the console next to it as
///   `<log>.console`.
/// * `--from <log>`: check the pair an earlier run left instead of booting.
/// * `--max-wall <secs>`: the wall budget, instead of the scenario's.
/// * `--plan`: print the `boot` invocation instead of running it — a
///   `wall=<secs>` line, then one argument a line — for running the workload
///   by hand or under `perf`. The scenario file stays the only place the
///   workload is written down.
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
    if std::env::var_os("RVF_BOOT_WALL").is_some() {
        eprintln!("warning: RVF_BOOT_WALL is gone, use boot-check --max-wall <secs>");
    }
    let path = path.context("boot-check: missing <scenario.toml>")?;
    let mut scn = harness::BootScenario::load(&path)?;
    if let Some(secs) = max_wall {
        scn.boot.wall_secs = secs;
    }
    // Named after the scenario when the caller does not say. A fixed
    // `boot.log` is shared state: two checks running at once in one checkout
    // overwrite each other's log and console half-way through, and each then
    // judges a mix of both runs — milestones "missing" from lines that are
    // plainly on the terminal, and a transcript that belongs to the other
    // scenario. Boots take minutes, so running two is the normal thing to do.
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

/// Name the files the run needs and does not have, each with the command that
/// makes it, rather than boot without a card. False when one is missing.
fn inputs_present(scn: &harness::BootScenario) -> bool {
    let missing = scn.missing_inputs();
    if missing.is_empty() {
        return true;
    }
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
    false
}

/// The terminal and the run log at once.
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

/// Boot the scenario as a child of this binary. Its stdout and stderr share
/// one pipe, so `log` gets them interleaved the way they came — which is what
/// the milestones read — and the terminal sees them as they come. `Some` exit
/// code when the boot failed in a way that leaves nothing to check.
fn run_boot(scn: &harness::BootScenario, log: &Path, console: &Path) -> Result<Option<ExitCode>> {
    // Never check a console an earlier run left behind: a boot that cannot
    // start writes none, and the check would diff the stale one instead.
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
        // `cmd` goes here, and with it this side's write ends of the pipe:
        // the copy below ends when the boot's do.
    };

    // The boot stops itself at its wall budget. This is for one that does
    // not, a hang on the host side: 40 s more, then a SIGINT.
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
    // 0: the boot got where it was meant to. 1: it did not, and the
    // milestones say which. Past the wall: the milestones judge that too.
    if !late && !matches!(status.code(), Some(0 | 1)) {
        let code = status
            .code()
            .and_then(|c| u8::try_from(c).ok())
            .unwrap_or(1);
        return Ok(Some(ExitCode::from(code)));
    }
    // ...or `boot` itself failed, before it ran or part-way through, and then
    // it writes no console: its `error:` line above is the whole story.
    if !console.exists() {
        eprintln!(
            "the boot wrote no console ({}): see the error above; nothing to check",
            console.display()
        );
        return Ok(Some(ExitCode::FAILURE));
    }
    Ok(None)
}

/// Check a finished run: the console against the golden transcript, `log`
/// against the milestones and the retired counts. `update` rewrites the golden
/// and the counts first, unless the run failed a milestone.
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

    if update {
        // Never record a bad run as the new truth. A boot that was starved of
        // CPU stops at the wall clock part-way through, and its transcript
        // looks like a perfectly good — and much shorter — boot.
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
            scn.golden_path().display(),
            transcript.lines().count()
        );
        // The milestones passed, so the report is there.
        let counts = RetiredCounts::from_log(&log_text)
            .context("the run log has no retired counts to record")?;
        let changed = match harness::boot::check_retired(scn, &counts) {
            Ok(GoldenCheck::Mismatch(diff)) => diff,
            _ => String::new(),
        };
        harness::boot::write_retired(scn, &counts)?;
        println!("updated retired counts {}", scn.retired_path().display());
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
