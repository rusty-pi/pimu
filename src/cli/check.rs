//! `boot --scenario`: boot what a scenario file describes and check the run
//! against it — the golden console transcript, every milestone and the retired
//! counts. A directory runs every `*.yaml` in it, one after another.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use pimu::harness;
use pimu::harness::boot::{GoldenCheck, RetiredCounts};

/// What `--scenario` and the options that only mean something with it ask for.
pub struct Flags {
    path: PathBuf,
    record: bool,
    plan: bool,
    from_log: Option<PathBuf>,
    output: Option<PathBuf>,
}

/// Take the scenario options out of `args`. What is left goes to the boot as it
/// is, after the scenario's own options, so it overrides them.
pub fn split(args: &[String]) -> Result<Option<(Flags, Vec<String>)>> {
    let mut path = None;
    let mut flags = Flags {
        path: PathBuf::new(),
        record: false,
        plan: false,
        from_log: None,
        output: None,
    };
    let mut rest = Vec::with_capacity(args.len());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |what: &str| {
            it.next()
                .map(PathBuf::from)
                .with_context(|| format!("{what} needs a path"))
        };
        match a.as_str() {
            "--scenario" => path = Some(value("--scenario")?),
            "--record" => flags.record = true,
            "--plan" => flags.plan = true,
            "--from-log" => flags.from_log = Some(value("--from-log")?),
            "--output" => flags.output = Some(value("--output")?),
            _ => rest.push(a.clone()),
        }
    }
    let Some(path) = path else {
        for (given, name) in [
            (flags.record, "--record"),
            (flags.plan, "--plan"),
            (flags.from_log.is_some(), "--from-log"),
            (flags.output.is_some(), "--output"),
        ] {
            if given {
                bail!("{name} goes with --scenario");
            }
        }
        return Ok(None);
    };
    flags.path = path;
    if flags.from_log.is_some() && (flags.plan || flags.output.is_some()) {
        bail!("--from-log checks an earlier run: it takes no --plan or --output");
    }
    Ok(Some((flags, rest)))
}

fn wall_override(extras: &[String]) -> Result<Option<u64>> {
    let Some(at) = extras.iter().rposition(|a| a == "--max-wall") else {
        return Ok(None);
    };
    let secs = extras.get(at + 1).context("--max-wall needs seconds")?;
    Ok(Some(secs.parse().with_context(|| {
        format!("--max-wall {secs}: not a number of seconds")
    })?))
}

/// The scenario files `path` names: itself, or the `*.yaml` in it.
fn scenario_files(path: &Path) -> Result<Vec<PathBuf>> {
    if !path.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let files: Vec<PathBuf> = harness::discover(path)
        .with_context(|| format!("listing {}", path.display()))?
        .into_iter()
        .filter(|p| !p.to_string_lossy().ends_with(".retired.yaml"))
        .collect();
    if files.is_empty() {
        bail!("no *.yaml scenarios in {}", path.display());
    }
    Ok(files)
}

pub fn run(flags: &Flags, extras: &[String]) -> Result<ExitCode> {
    if std::env::var_os("PIMU_BOOT_WALL").is_some() {
        eprintln!("warning: PIMU_BOOT_WALL is gone, use --max-wall <secs>");
    }
    let files = scenario_files(&flags.path)?;
    let several = flags.path.is_dir();
    let wall = wall_override(extras)?;
    let mut results: Vec<(String, bool)> = Vec::new();
    for file in &files {
        let mut scn = harness::BootScenario::load(file)?;
        if let Some(secs) = wall {
            scn.boot.wall_secs = secs;
        }
        if several {
            println!("== {} ({})", scn.name, file.display());
        }
        let passed = run_one(flags, &scn, extras, several)? == ExitCode::SUCCESS;
        results.push((scn.name.clone(), passed));
    }
    if !several || flags.plan {
        return Ok(if results.iter().all(|(_, ok)| *ok) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    println!();
    for (name, ok) in &results {
        println!("{} {name}", if *ok { "PASS" } else { "FAIL" });
    }
    let failed = results.iter().filter(|(_, ok)| !ok).count();
    println!("{} scenario(s), {failed} failed", results.len());
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Where a scenario's combined log goes: named after the scenario, because a
/// fixed `boot.log` would be shared state and two checks at once in one
/// checkout would each judge a mix of both runs. With a folder of scenarios
/// `--output` names the directory the logs go in.
fn log_path(flags: &Flags, scn: &harness::BootScenario, several: bool) -> PathBuf {
    let named = format!("boot-{}.log", scn.name);
    match (&flags.from_log, &flags.output) {
        (Some(p), _) if !several => p.clone(),
        (Some(dir), _) => dir.join(named),
        (None, Some(dir)) if several => dir.join(named),
        (None, Some(p)) => p.clone(),
        (None, None) => PathBuf::from(named),
    }
}

fn run_one(
    flags: &Flags,
    scn: &harness::BootScenario,
    extras: &[String],
    several: bool,
) -> Result<ExitCode> {
    let log = log_path(flags, scn, several);
    if several {
        if let Some(dir) = log.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
    }
    let mut console = log.clone().into_os_string();
    console.push(".console");
    let console = PathBuf::from(console);

    if flags.from_log.is_none() {
        if !inputs_present(scn) {
            return Ok(ExitCode::FAILURE);
        }
        let mut staged = scn.clone();
        let mut cards = log.clone().into_os_string();
        cards.push(".cards");
        staged.stage(Path::new(&cards))?;
        if flags.plan {
            println!("wall={}", scn.wall_secs());
            for a in staged.boot_args(&console).iter().chain(extras) {
                println!("{a}");
            }
            return Ok(ExitCode::SUCCESS);
        }
        if let Some(failed) = run_boot(&staged, extras, &log, &console)? {
            return Ok(failed);
        }
    }
    check_boot(scn, &log, &console, flags.record)
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
fn run_boot(
    scn: &harness::BootScenario,
    extras: &[String],
    log: &Path,
    console: &Path,
) -> Result<Option<ExitCode>> {
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
            .args(extras)
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
    let mut checked = vec![format!("{} milestone(s)", scn.milestones.len())];
    if scn.golden_path().is_some() {
        checked.push(format!(
            "golden transcript ({} lines)",
            transcript.lines().count()
        ));
    }
    if scn.retired_path().is_some() {
        checked.push(format!(
            "retired counts ({} core(s))",
            RetiredCounts::from_log(&log_text).map_or(0, |c| c.0.len())
        ));
    }
    println!("\n{}: {}", scn.name, checked.join(" + "));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_scenario_options_come_out_and_the_rest_goes_to_the_boot() {
        let (flags, rest) = split(&args(&[
            "--max-wall",
            "600",
            "--scenario",
            "s.yaml",
            "--plan",
            "--output",
            "out.log",
            "-v",
        ]))
        .unwrap()
        .expect("a scenario");
        assert_eq!(flags.path, PathBuf::from("s.yaml"));
        assert!(flags.plan && !flags.record);
        assert_eq!(flags.output, Some(PathBuf::from("out.log")));
        assert_eq!(rest, args(&["--max-wall", "600", "-v"]));
        assert_eq!(wall_override(&rest).unwrap(), Some(600));
    }

    #[test]
    fn a_plain_boot_is_left_alone_and_a_stray_scenario_option_is_refused() {
        assert!(split(&args(&["--sd", "x.img"])).unwrap().is_none());
        let err = split(&args(&["--record"])).err().expect("refused");
        assert!(err.to_string().contains("goes with --scenario"), "{err}");
        let err = split(&args(&[
            "--scenario",
            "s.yaml",
            "--from-log",
            "l",
            "--plan",
        ]))
        .err()
        .expect("refused");
        assert!(err.to_string().contains("--from-log"), "{err}");
    }

    #[test]
    fn logs_are_named_after_the_scenario_and_a_folder_gets_a_directory() {
        let scn: harness::BootScenario =
            yaml_serde::from_str("name: x\nboot:\n  eeprom: e\n  wall_secs: 1\n").unwrap();
        let flags = |output: Option<&str>| Flags {
            path: PathBuf::new(),
            record: false,
            plan: false,
            from_log: None,
            output: output.map(PathBuf::from),
        };
        assert_eq!(
            log_path(&flags(None), &scn, false),
            PathBuf::from("boot-x.log")
        );
        assert_eq!(
            log_path(&flags(Some("a.log")), &scn, false),
            PathBuf::from("a.log")
        );
        assert_eq!(
            log_path(&flags(Some("logs")), &scn, true),
            PathBuf::from("logs/boot-x.log")
        );
    }
}
