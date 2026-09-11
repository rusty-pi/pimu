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
        max_steps: Some(scn.run.max_steps),
        max_wall: Some(std::time::Duration::from_secs(60)),
        stop_pc: scn.run.stop_pc,
        idle_spin_limit: scn.run.idle_spin_limit,
        silent_us: 0,
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

/// Number of unchanged lines printed either side of a change.
const DIFF_CONTEXT: usize = 3;

/// Line-oriented diff with context, aligned by longest common subsequence so a
/// single inserted or deleted line does not make everything after it look
/// changed. That alignment is what makes the boot transcript diffable at all —
/// the firmware prints ~200 lines and a one-line insertion in the middle is the
/// common case.
///
/// No dependency: the common prefix and suffix are trimmed first, which on a
/// real regression leaves a handful of lines, and the O(n*m) table is only
/// built for what is left (with a positional fallback if that is still huge).
pub fn unified_diff(expected: &str, actual: &str) -> String {
    let exp: Vec<&str> = expected.lines().collect();
    let act: Vec<&str> = actual.lines().collect();

    // Trim the common head and tail; everything in between is the real work.
    let head = exp
        .iter()
        .zip(act.iter())
        .take_while(|(e, a)| e == a)
        .count();
    let max_tail = exp.len().min(act.len()) - head;
    let tail = exp
        .iter()
        .rev()
        .zip(act.iter().rev())
        .take_while(|(e, a)| e == a)
        .count()
        .min(max_tail);
    let e_mid = &exp[head..exp.len() - tail];
    let a_mid = &act[head..act.len() - tail];

    // One entry per output line: (kind, line number to show, text), with kind
    // in {' ', '-', '+'}. The trimmed head and tail go back in as context —
    // they were only skipped to keep the alignment table small.
    let mut ops: Vec<(char, usize, &str)> = (0..head).map(|i| (' ', i + 1, exp[i])).collect();
    if e_mid.len().saturating_mul(a_mid.len()) > 4_000_000 {
        // Too big to align; fall back to position-by-position.
        for i in 0..e_mid.len().max(a_mid.len()) {
            if let Some(l) = e_mid.get(i) {
                ops.push(('-', head + i + 1, l));
            }
            if let Some(l) = a_mid.get(i) {
                ops.push(('+', head + i + 1, l));
            }
        }
    } else {
        ops.extend(lcs_ops(e_mid, a_mid, head));
    }
    for i in 0..tail {
        let e_i = exp.len() - tail + i;
        ops.push((' ', e_i + 1, exp[e_i]));
    }

    if ops.iter().all(|&(k, _, _)| k == ' ') {
        return if expected == actual {
            String::new()
        } else {
            "  (differs only in trailing whitespace / newline)\n".to_string()
        };
    }

    // Emit the changed runs with `DIFF_CONTEXT` lines either side, collapsing
    // the untouched stretches between them.
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, &(k, _, _))| k != ' ')
        .map(|(i, _)| i)
        .collect();
    let mut out = String::new();
    let mut emitted_to = 0usize;
    let mut i = 0usize;
    while i < changed.len() {
        let lo = changed[i].saturating_sub(DIFF_CONTEXT);
        let mut j = i;
        // Merge hunks whose context windows touch.
        while j + 1 < changed.len() && changed[j + 1] <= changed[j] + 2 * DIFF_CONTEXT {
            j += 1;
        }
        let hi = (changed[j] + DIFF_CONTEXT + 1).min(ops.len());
        let lo = lo.max(emitted_to);
        if lo > emitted_to {
            out.push_str("  ...\n");
        }
        for &(kind, no, line) in &ops[lo..hi] {
            out.push_str(&format!("  {kind} {no:>5}  {line}\n"));
        }
        emitted_to = hi;
        i = j + 1;
    }
    if emitted_to < ops.len() {
        out.push_str("  ...\n");
    }
    out
}

/// Longest-common-subsequence alignment of two line slices. `offset` is how
/// many lines were trimmed off the front, for the reported line numbers.
fn lcs_ops<'a>(e: &[&'a str], a: &[&'a str], offset: usize) -> Vec<(char, usize, &'a str)> {
    let (n, m) = (e.len(), a.len());
    // table[i][j] = LCS length of e[i..] and a[j..]
    let mut table = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[at(i, j)] = if e[i] == a[j] {
                table[at(i + 1, j + 1)] + 1
            } else {
                table[at(i + 1, j)].max(table[at(i, j + 1)])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if e[i] == a[j] {
            ops.push((' ', offset + i + 1, e[i]));
            i += 1;
            j += 1;
        } else if table[at(i + 1, j)] >= table[at(i, j + 1)] {
            ops.push(('-', offset + i + 1, e[i]));
            i += 1;
        } else {
            ops.push(('+', offset + j + 1, a[j]));
            j += 1;
        }
    }
    while i < n {
        ops.push(('-', offset + i + 1, e[i]));
        i += 1;
    }
    while j < m {
        ops.push(('+', offset + j + 1, a[j]));
        j += 1;
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::unified_diff;

    #[test]
    fn identical_inputs_produce_no_diff() {
        assert_eq!(unified_diff("a\nb\n", "a\nb\n"), "");
    }

    #[test]
    fn an_insertion_is_one_line_not_a_cascade() {
        let expected = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let actual = "a\nb\nc\nd\nNEW\ne\nf\ng\nh\n";
        let d = unified_diff(expected, actual);
        assert_eq!(d.lines().filter(|l| l.starts_with("  +")).count(), 1);
        assert_eq!(d.lines().filter(|l| l.starts_with("  -")).count(), 0);
        assert!(d.contains("NEW"), "{d}");
    }

    #[test]
    fn a_changed_line_shows_both_sides_with_context() {
        let d = unified_diff("a\nb\nc\n", "a\nX\nc\n");
        assert!(d.contains("-     2  b"), "{d}");
        assert!(d.contains("+     2  X"), "{d}");
        assert!(d.contains("a"), "{d}");
    }

    #[test]
    fn truncation_is_reported_as_deletions() {
        let d = unified_diff("a\nb\nc\nd\n", "a\nb\n");
        assert_eq!(d.lines().filter(|l| l.starts_with("  -")).count(), 2);
    }
}
