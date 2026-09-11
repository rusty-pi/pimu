//! The firmware boot as a scenario: one TOML file describes the workload (which
//! EEPROM image, which SD card, what wall budget) and every assertion made
//! about the run.
//!
//! There are two complementary kinds of assertion and they catch different
//! things:
//!
//! * The **golden transcript** — the whole normalised UART console, diffed
//!   line by line. It catches "something changed" with the change shown in
//!   place, including output that *moved* or a value that shifted, neither of
//!   which a set of greps can see.
//! * The **milestones** — named substring assertions, each carrying the reason
//!   it exists (the commit and issue that made it pass). They say *which
//!   invariant* broke, which a raw diff cannot.
//!
//! Both are checked against a single boot run: the golden against the console
//! bytes the run wrote out (`recon --console-log`), the milestones against the
//! combined log, which also holds the parts of the evidence that never reach a
//! UART (the device tree handed to the ARM, the SDRAM refresh history, the
//! retired/skipped counters).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::harness::capture::transcript;
use crate::harness::regression::unified_diff;

/// Wall-clock budget override, the same knob `scripts/boot-check.sh` has always
/// had. The scenario's own `wall_secs` is the default.
pub const WALL_ENV: &str = "RVF_BOOT_WALL";

#[derive(Debug, Clone, Deserialize)]
pub struct BootScenario {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub boot: BootSpec,
    pub golden: GoldenSpec,
    /// `[[milestone]]` entries, in the order they should be reported.
    #[serde(default, rename = "milestone")]
    pub milestones: Vec<Milestone>,

    /// Directory the scenario file lives in; relative paths resolve against it.
    #[serde(skip)]
    pub base_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BootSpec {
    /// `pieeprom.bin` image, relative to the scenario file.
    pub eeprom: String,
    /// SD card image, relative to the scenario file.
    pub sd: String,
    /// Wall-clock budget for the run, in seconds.
    pub wall_secs: u64,
    /// Largest acceptable skipped-instruction count in the run report. `recon`
    /// stops on an instruction the decoder does not implement rather than
    /// stepping over it, so a skip can only come from the remaining recon
    /// leniencies (`bkpt` padding, `sleep`, an unhandled `swi`) — nothing in a
    /// clean boot should need even those.
    #[serde(default)]
    pub max_skipped: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GoldenSpec {
    /// Path to the golden console transcript, relative to the scenario file.
    pub path: String,
}

/// One named assertion about the run, with the reason it exists.
#[derive(Debug, Clone, Deserialize)]
pub struct Milestone {
    /// What this proves — quoted verbatim when it fails. This is the half of
    /// the regression a golden diff cannot carry, so it is mandatory.
    pub why: String,
    /// A substring, or a list of substrings that must appear in that order on
    /// one line. Plain text, never a regex: every pattern the bash check used
    /// was either literal or a literal-with-`.*`, and the ordered-substrings
    /// form covers both without a regex dependency.
    pub line: Pattern,
    /// The line must not appear at all.
    #[serde(default)]
    pub absent: bool,
    /// Exactly this many matching lines.
    #[serde(default)]
    pub count: Option<usize>,
    /// At most this many matching lines.
    #[serde(default)]
    pub max_count: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Pattern {
    One(String),
    Parts(Vec<String>),
}

impl Pattern {
    fn parts(&self) -> &[String] {
        match self {
            Pattern::One(s) => std::slice::from_ref(s),
            Pattern::Parts(v) => v,
        }
    }

    /// True when every part occurs in `line`, in order and without overlap.
    pub fn matches(&self, line: &str) -> bool {
        let mut rest = line;
        for p in self.parts() {
            match rest.find(p.as_str()) {
                Some(i) => rest = &rest[i + p.len()..],
                None => return false,
            }
        }
        true
    }
}

impl std::fmt::Display for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pattern::One(s) => write!(f, "{s:?}"),
            Pattern::Parts(v) => {
                let joined: Vec<String> = v.iter().map(|s| format!("{s:?}")).collect();
                write!(f, "{}", joined.join(" ... "))
            }
        }
    }
}

impl Milestone {
    /// Report this milestone against `log`, returning a failure description.
    pub fn check(&self, log: &str) -> Option<String> {
        let hits: Vec<&str> = log.lines().filter(|l| self.line.matches(l)).collect();
        let n = hits.len();
        let bad = if self.absent {
            n != 0
        } else if let Some(want) = self.count {
            n != want
        } else if let Some(max) = self.max_count {
            n > max
        } else {
            n == 0
        };
        if !bad {
            return None;
        }
        let head = if self.absent {
            format!("PRESENT: {} ({n} matching lines)", self.line)
        } else if let Some(want) = self.count {
            format!("COUNT:   {} is {n}, want {want}", self.line)
        } else if let Some(max) = self.max_count {
            format!("COUNT:   {} is {n}, want <= {max}", self.line)
        } else {
            format!("MISSING: {}", self.line)
        };
        let mut out = format!("{head}\n         why: {}\n", self.why.trim());
        // Show what did match, so a count failure or an unexpected line is
        // readable without going back to the log.
        for l in hits.iter().take(4) {
            out.push_str(&format!("         got: {}\n", l.trim()));
        }
        if n > 4 {
            out.push_str(&format!("         ... and {} more\n", n - 4));
        }
        Some(out)
    }
}

impl BootScenario {
    pub fn load(path: &Path) -> Result<BootScenario> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading boot scenario {}", path.display()))?;
        let mut s: BootScenario = toml::from_str(&text)
            .with_context(|| format!("parsing boot scenario {}", path.display()))?;
        s.base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        Ok(s)
    }

    pub fn eeprom_path(&self) -> PathBuf {
        self.base_dir.join(&self.boot.eeprom)
    }

    pub fn sd_path(&self) -> PathBuf {
        self.base_dir.join(&self.boot.sd)
    }

    pub fn golden_path(&self) -> PathBuf {
        self.base_dir.join(&self.golden.path)
    }

    /// The wall budget, with the `RVF_BOOT_WALL` override applied.
    pub fn wall_secs(&self) -> u64 {
        std::env::var(WALL_ENV)
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(self.boot.wall_secs)
    }

    /// The `recon` argument vector that runs this scenario. `scripts/boot-check.sh`
    /// asks for this rather than spelling the run out a second time, so the
    /// scenario file stays the only description of the workload.
    pub fn recon_args(&self, console_log: &Path) -> Vec<String> {
        vec![
            "recon".into(),
            self.eeprom_path().display().to_string(),
            "--eeprom".into(),
            "--sd".into(),
            self.sd_path().display().to_string(),
            "--max-wall".into(),
            self.wall_secs().to_string(),
            "--console-log".into(),
            console_log.display().to_string(),
        ]
    }
}

/// Normalise raw console bytes into a stable, diffable transcript.
///
/// On top of the byte-level normalisation in [`transcript`], this strips the
/// clock out of the transcript.
///
/// The model's clock is driven by retired cycles, so two runs on the same build
/// do print the same timestamps (measured: two boots minutes apart, under
/// different load, byte-identical consoles). Stripping them is not about run-to-
/// run noise but about *what a diff is worth*: a change to what any instruction
/// costs, or to when the tick lands, shifts every timestamp in the file at once
/// and buries the one line that actually changed under 141 lines of drift. The
/// timestamps are also the one thing here no assertion has ever been made
/// about, so nothing is lost by dropping them.
///
/// Three forms carry a clock:
///
/// * the bootloader's `%6.2f ` line prefix (`  2.14 EEPROM ID 0xef4018`),
/// * `start4`'s `MESS:hh:mm:ss.uuuuuu:<core>:` prefix,
/// * the `stc <n>` field of the `BOOTMODE:` line, which is the raw system
///   timer count.
///
/// Everything else is kept, including values that look incidental: a changed
/// clock divisor or buffer size is exactly the kind of drift this golden is
/// here to catch.
pub fn normalise_console(raw: &[u8]) -> String {
    let text = transcript(raw);
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        out.push_str(&normalise_line(line));
        out.push('\n');
    }
    out
}

fn normalise_line(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("MESS:") {
        // `00:00:12.882804:0: brfs: ...` — the timestamp and the core id are
        // digits, colons and one dot; keep the core id, drop the clock.
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == ':' || c == '.'))
            .unwrap_or(rest.len());
        if end == 0 {
            // Already normalised (`MESS:[t]:0:`), or not a timestamped line at
            // all. Normalising has to be idempotent: a golden file is fed back
            // through this when it is checked for drift.
            return line.to_string();
        }
        let (stamp, tail) = rest.split_at(end);
        let core = stamp.trim_end_matches(':').rsplit(':').next().unwrap_or("");
        return format!("MESS:[t]:{core}:{tail}");
    }
    // `  2.14 EEPROM ID 0xef4018` — a right-aligned seconds count, then a space.
    let trimmed = line.trim_start_matches(' ');
    if let Some((stamp, tail)) = trimmed.split_once(' ') {
        let is_stamp = matches!(stamp.split_once('.'), Some((s, f))
            if !s.is_empty()
                && s.bytes().all(|b| b.is_ascii_digit())
                && f.len() == 2
                && f.bytes().all(|b| b.is_ascii_digit()));
        if is_stamp {
            return format!("[t] {}", scrub_stc(tail));
        }
    }
    scrub_stc(line)
}

/// Replace the `stc <n>` system-timer reading on the `BOOTMODE:` line.
fn scrub_stc(line: &str) -> String {
    let Some(i) = line.find("stc ") else {
        return line.to_string();
    };
    let rest = &line[i + 4..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return line.to_string();
    }
    format!("{}stc [n]{}", &line[..i], &rest[end..])
}

/// What a golden comparison found.
pub enum GoldenCheck {
    Match,
    Mismatch(String),
    Missing,
}

/// Compare a normalised transcript against the scenario's golden file.
pub fn check_golden(scn: &BootScenario, actual: &str) -> Result<GoldenCheck> {
    let path = scn.golden_path();
    match std::fs::read_to_string(&path) {
        Ok(expected) if expected == actual => Ok(GoldenCheck::Match),
        Ok(expected) => Ok(GoldenCheck::Mismatch(unified_diff(&expected, actual))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(GoldenCheck::Missing),
        Err(e) => Err(e).with_context(|| format!("reading golden {}", path.display())),
    }
}

pub fn write_golden(scn: &BootScenario, actual: &str) -> Result<()> {
    let path = scn.golden_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(&path, actual).with_context(|| format!("writing golden {}", path.display()))
}

/// The skipped-instruction count from the last `retired ...` report line.
pub fn skipped_count(log: &str) -> Option<u64> {
    log.lines()
        .rev()
        .filter(|l| l.starts_with("retired "))
        .find_map(|l| {
            let rest = l.split_once("(skipped ")?.1;
            let end = rest.find(|c: char| !c.is_ascii_digit())?;
            rest[..end].parse().ok()
        })
}

/// Every milestone, plus the skipped-instruction guard, against the combined
/// run log. Kept separate from the golden because `--update` has to know
/// whether the run it is about to record was a good one.
pub fn check_milestones(scn: &BootScenario, log: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for m in &scn.milestones {
        if let Some(f) = m.check(log) {
            failures.push(f);
        }
    }

    match skipped_count(log) {
        None => failures.push(
            "MISSING: could not read the skipped-instruction count from the report\n         \
             why: without the report line the run did not finish, so nothing below it is proven\n"
                .into(),
        ),
        Some(n) if n > scn.boot.max_skipped => failures.push(format!(
            "COUNT:   skipped instructions is {n}, want <= {}\n         \
             why: an instruction the decoder does not implement now stops the run, so a skip \
             can only be a recon leniency — a clean boot needs none\n",
            scn.boot.max_skipped
        )),
        Some(_) => {}
    }
    failures
}

/// Run every assertion in the scenario against one boot, returning the
/// failures. `log` is the combined run log, `console` the normalised transcript.
pub fn check_run(scn: &BootScenario, log: &str, console: &str) -> Result<Vec<String>> {
    let mut failures = Vec::new();

    match check_golden(scn, console)? {
        GoldenCheck::Match => {}
        GoldenCheck::Missing => failures.push(format!(
            "MISSING: no golden transcript at {}\n         \
             why: the boot has nothing to be compared against; \
             re-run with --update to record one\n",
            scn.golden_path().display()
        )),
        GoldenCheck::Mismatch(diff) => failures.push(format!(
            "TRANSCRIPT: the console differs from {}\n\
             {diff}\
             \n         (--update rewrites the golden once the change is \
             understood and wanted)\n",
            scn.golden_path().display()
        )),
    }

    failures.extend(check_milestones(scn, log));
    Ok(failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootloader_timestamps_are_stripped() {
        let raw = b"  2.14 EEPROM ID 0xef4018\r\n 12.30 Watchdog stopped\r\n";
        assert_eq!(
            normalise_console(raw),
            "[t] EEPROM ID 0xef4018\n[t] Watchdog stopped\n"
        );
    }

    #[test]
    fn mess_timestamps_are_stripped_but_the_core_id_is_kept() {
        let raw = b"MESS:00:00:12.882804:0: brfs: File read: 97 bytes\n";
        assert_eq!(
            normalise_console(raw),
            "MESS:[t]:0: brfs: File read: 97 bytes\n"
        );
    }

    #[test]
    fn the_system_timer_count_is_stripped() {
        let raw = b"  2.14 BOOTMODE: 0x00 partition 0 serial 1aa2bb31 stc 2140397\n";
        assert_eq!(
            normalise_console(raw),
            "[t] BOOTMODE: 0x00 partition 0 serial 1aa2bb31 stc [n]\n"
        );
    }

    #[test]
    fn a_value_that_is_not_a_clock_survives_normalisation() {
        // The whole point of the golden: a changed divisor must show up.
        let raw = b"  5.86 SD HOST: 200000000 div: 4 (2) delay: 2\n";
        assert_eq!(
            normalise_console(raw),
            "[t] SD HOST: 200000000 div: 4 (2) delay: 2\n"
        );
    }

    #[test]
    fn ordered_substrings_match_within_one_line() {
        let p = Pattern::Parts(vec!["MESS:".into(), "arasan_emmc_open".into()]);
        assert!(p.matches("MESS:00:00:03.1:0: arasan_emmc_open"));
        assert!(!p.matches("arasan_emmc_open MESS:"));
        assert!(!p.matches("MESS: something else"));
    }

    #[test]
    fn a_milestone_reports_what_it_proves() {
        let m = Milestone {
            why: "the reset cause the bootloader reports".into(),
            line: Pattern::One("PM_RSTS 00000020".into()),
            absent: false,
            count: None,
            max_count: None,
        };
        assert!(m.check("  2.14 PM_RSTS 00000020\n").is_none());
        let f = m.check("  2.14 PM_RSTS 00001000\n").expect("should fail");
        assert!(f.contains("MISSING"), "{f}");
        assert!(f.contains("the reset cause"), "{f}");
    }

    #[test]
    fn counts_and_absence_behave_like_the_greps_they_replace() {
        let log = "a hit\nb\na hit\n";
        let exact = |n| Milestone {
            why: "w".into(),
            line: Pattern::One("hit".into()),
            absent: false,
            count: Some(n),
            max_count: None,
        };
        assert!(exact(2).check(log).is_none());
        assert!(exact(1).check(log).is_some());

        let at_most = |n| Milestone {
            why: "w".into(),
            line: Pattern::One("hit".into()),
            absent: false,
            count: None,
            max_count: Some(n),
        };
        assert!(at_most(2).check(log).is_none());
        assert!(at_most(1).check(log).is_some());

        let absent = Milestone {
            why: "w".into(),
            line: Pattern::One("hit".into()),
            absent: true,
            count: None,
            max_count: None,
        };
        assert!(absent.check(log).is_some());
        assert!(absent.check("nothing here\n").is_none());
    }

    #[test]
    fn the_skipped_count_comes_from_the_last_report_line() {
        let log = "retired 10  (skipped 3, cycles 1)\nretired 99  (skipped 0, cycles 2)\n";
        assert_eq!(skipped_count(log), Some(0));
        assert_eq!(skipped_count("no report here\n"), None);
    }
}
