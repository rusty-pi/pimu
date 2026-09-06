//! Build an [`Emulator`] from a [`Scenario`], run it, and diff the console
//! transcript against the golden file.

use anyhow::{bail, Context, Result};

use crate::emulator::{Emulator, RunLimits, RunReport};
use crate::firmware::Payload;
use crate::harness::capture::transcript;
use crate::harness::scenario::{PayloadKind, Scenario};
use crate::machine::Machine;
use crate::payloads;

/// Outcome of comparing a run against its golden.
#[derive(Debug)]
pub enum GoldenOutcome {
    Match,
    Mismatch {
        expected: String,
        actual: String,
    },
    /// Golden file did not exist; `actual` is what a `--update` run would write.
    Missing {
        actual: String,
    },
}

pub struct ScenarioRun {
    pub report: RunReport,
    pub transcript: String,
}

pub fn build_payload(scn: &Scenario) -> Result<Payload> {
    match scn.payload.kind {
        PayloadKind::Builtin => {
            let bytes = payloads::by_name(&scn.payload.source)
                .with_context(|| format!("unknown builtin payload '{}'", scn.payload.source))?;
            let load = scn.payload.load_addr.expect("validated in Scenario::load");
            Ok(Payload::RawBinary {
                load_addr: load,
                entry: scn.payload.entry.unwrap_or(load),
                bytes,
            })
        }
        PayloadKind::Raw => {
            let bytes = std::fs::read(scn.payload_path())
                .with_context(|| format!("reading raw payload {}", scn.payload_path().display()))?;
            let load = scn.payload.load_addr.expect("validated in Scenario::load");
            Ok(Payload::RawBinary {
                load_addr: load,
                entry: scn.payload.entry.unwrap_or(load),
                bytes,
            })
        }
        PayloadKind::Elf => {
            let bytes = std::fs::read(scn.payload_path())
                .with_context(|| format!("reading ELF payload {}", scn.payload_path().display()))?;
            let mut p = Payload::from_elf_bytes(&bytes)?;
            if let (Some(e), Payload::Elf(elf)) = (scn.payload.entry, &mut p) {
                elf.entry = e; // scenario entry override
            }
            Ok(p)
        }
        PayloadKind::Eeprom => {
            let bytes = std::fs::read(scn.payload_path()).with_context(|| {
                format!("reading EEPROM image {}", scn.payload_path().display())
            })?;
            let mut p = Payload::from_eeprom_bytes(&bytes)?;
            if let (Some(e), Payload::RawBinary { entry, .. }) = (scn.payload.entry, &mut p) {
                *entry = e; // scenario entry override
            }
            Ok(p)
        }
    }
}

pub fn run_scenario(scn: &Scenario) -> Result<ScenarioRun> {
    let payload = build_payload(scn)?;

    let ram_bytes = (scn.machine.ram_mb as usize)
        .checked_mul(1024 * 1024)
        .context("ram_mb too large")?;
    let mut machine = Machine::new(ram_bytes);
    machine.console = scn.machine.console.into();

    payload
        .load_into(&mut machine)
        .with_context(|| "loading payload into machine")?;

    let entry = payload.entry();
    let mut emu = Emulator::new(machine, entry);
    emu.set_unimpl_policy(scn.run.unimpl.into());

    let limits = RunLimits {
        max_steps: scn.run.max_steps,
        max_wall: Some(std::time::Duration::from_secs(60)),
        stop_pc: scn.run.stop_pc,
        idle_spin_limit: scn.run.idle_spin_limit,
    };
    let report = emu.run(&limits);
    let transcript = transcript(&report.console);

    Ok(ScenarioRun { report, transcript })
}

pub fn check_golden(scn: &Scenario, actual: &str) -> Result<GoldenOutcome> {
    let path = scn.golden_path();
    match std::fs::read_to_string(&path) {
        Ok(expected) if expected == actual => Ok(GoldenOutcome::Match),
        Ok(expected) => Ok(GoldenOutcome::Mismatch {
            expected,
            actual: actual.to_string(),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(GoldenOutcome::Missing {
            actual: actual.to_string(),
        }),
        Err(e) => Err(e).with_context(|| format!("reading golden {}", path.display())),
    }
}

pub fn write_golden(scn: &Scenario, actual: &str) -> Result<()> {
    let path = scn.golden_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(&path, actual).with_context(|| format!("writing golden {}", path.display()))?;
    Ok(())
}

/// Run + compare in one call. `update` rewrites the golden instead of failing.
pub fn verify(scn: &Scenario, update: bool) -> Result<ScenarioRun> {
    let run = run_scenario(scn)?;
    match check_golden(scn, &run.transcript)? {
        GoldenOutcome::Match => Ok(run),
        GoldenOutcome::Missing { actual } | GoldenOutcome::Mismatch { actual, .. } if update => {
            write_golden(scn, &actual)?;
            Ok(run)
        }
        GoldenOutcome::Missing { .. } => {
            bail!(
                "no golden for scenario '{}' (run with --update to create it)",
                scn.name
            )
        }
        GoldenOutcome::Mismatch { expected, actual } => {
            bail!(
                "golden mismatch for scenario '{}':\n{}",
                scn.name,
                unified_diff(&expected, &actual)
            )
        }
    }
}

/// Tiny line-oriented diff — no dependency, good enough for short transcripts.
pub fn unified_diff(expected: &str, actual: &str) -> String {
    let mut out = String::new();
    let exp: Vec<&str> = expected.lines().collect();
    let act: Vec<&str> = actual.lines().collect();
    let n = exp.len().max(act.len());
    for i in 0..n {
        match (exp.get(i), act.get(i)) {
            (Some(e), Some(a)) if e == a => {} // context elided
            (Some(e), Some(a)) => {
                out.push_str(&format!("  @{i}\n  - {e}\n  + {a}\n"));
            }
            (Some(e), None) => out.push_str(&format!("  @{i}\n  - {e}\n")),
            (None, Some(a)) => out.push_str(&format!("  @{i}\n  + {a}\n")),
            (None, None) => {}
        }
    }
    if out.is_empty() {
        out.push_str("  (differs only in trailing whitespace / newline)\n");
    }
    out
}
