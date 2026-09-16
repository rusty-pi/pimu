//! The firmware boot as a scenario: one TOML file describes the workload (which
//! EEPROM image, which SD card, what wall budget) and every assertion made
//! about the run.
//!
//! There are three complementary kinds of assertion and they catch different
//! things:
//!
//! * The **golden transcript** — the whole normalised UART console, diffed
//!   line by line. It catches "something changed" with the change shown in
//!   place, including output that *moved* or a value that shifted, neither of
//!   which a set of greps can see.
//! * The **milestones** — named substring assertions, each carrying the reason
//!   it exists (the commit and issue that made it pass). They say *which
//!   invariant* broke, which a raw diff cannot.
//! * The **retired counts** — how many instructions each core ran (#85). The
//!   transcript has its clocks stripped, so a change that makes the firmware
//!   or Linux run differently without printing anything different gets past
//!   it; the counts catch that.
//!
//! All three are checked against a single boot run: the golden against the
//! console bytes the run wrote out (`boot --console-log`), the milestones and
//! the counts against the combined log, which also holds the parts of the
//! evidence that never reach a UART (the device tree handed to the ARM, the
//! SDRAM refresh history, the retired/skipped counters).

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::harness::capture::transcript;
use crate::harness::regression::unified_diff;

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
    #[serde(default)]
    pub sd: Option<String>,
    /// Mass-storage image in USB socket A, relative to the scenario file.
    #[serde(default)]
    pub usb: Option<String>,
    /// Directory the built-in network peer serves over TFTP and HTTP (plugs
    /// the Ethernet cable in), relative to the scenario file.
    #[serde(default)]
    pub netboot: Option<String>,
    /// `BOOT_ORDER` to append to the EEPROM's `bootconf.txt`.
    #[serde(default)]
    pub boot_order: Option<String>,
    /// Further `KEY=VALUE` lines to append to `bootconf.txt`.
    #[serde(default)]
    pub bootconf: Vec<String>,
    /// RSA public key (`pubkey.bin` format) to put in the EEPROM, for boots
    /// that verify a signed `boot.img`; relative to the scenario file.
    #[serde(default)]
    pub eeprom_pubkey: Option<String>,
    /// BCM2711 stepping, `b0` or `c0` (`boot --stepping`); C0 when left out.
    #[serde(default)]
    pub stepping: Option<String>,
    /// OTP revision code, hex (`boot --board-rev`); a board the stepping
    /// shipped on when left out.
    #[serde(default)]
    pub board_rev: Option<String>,
    /// Wall-clock budget for the run, in seconds.
    pub wall_secs: u64,
    /// Largest acceptable skipped-instruction count in the run report. `boot`
    /// stops on an instruction the decoder does not implement rather than
    /// stepping over it, so a skip can only come from the remaining recon
    /// leniencies (`bkpt` padding, `sleep`, an unhandled `swi`) — nothing in a
    /// clean boot should need even those.
    #[serde(default)]
    pub max_skipped: u64,
    /// Property-interface tags to ask the still-running firmware for once the
    /// boot has handed over, as `boot --mbox-property` would (#23). Empty =
    /// do not exchange anything.
    #[serde(default)]
    pub mbox_property: Vec<String>,
    /// Ignored: the ARM is always modelled since #52. Kept so a scenario file
    /// that still says `arm = true` loads.
    #[serde(default)]
    pub arm: bool,
    /// End the run once the console prints this (`boot --until`), after the
    /// last [`Self::input`] line went in.
    #[serde(default)]
    pub until: Option<String>,
    /// Lines typed into the serial console, each once its prompt has printed
    /// (`boot --send-after`).
    #[serde(default)]
    pub input: Vec<ConsoleLine>,
}

/// `[[boot.input]]`: send `text` once the console prints `after`.
#[derive(Debug, Clone, Deserialize)]
pub struct ConsoleLine {
    pub after: String,
    pub text: String,
}

/// Make console text one plain line — `boot-check --plan` prints one `boot`
/// argument per line — with C-style escapes for backslashes and control bytes.
/// [`unescape`] reverses it.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii_control() => out.push_str(&format!("\\x{:02x}", c as u8)),
            c => out.push(c),
        }
    }
    out
}

/// `\n`, `\r`, `\t`, `\\` and `\xHH` into bytes; anything else stays as typed.
pub fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 == b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let hex = s
            .get(i + 2..i + 4)
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b[i + 1], hex) {
            (b'n', _) => out.push(b'\n'),
            (b'r', _) => out.push(b'\r'),
            (b't', _) => out.push(b'\t'),
            (b'\\', _) => out.push(b'\\'),
            (b'x', Some(h)) => {
                out.push(h);
                i += 2;
            }
            (other, _) => out.extend([b'\\', other]),
        }
        i += 2;
    }
    out
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

/// A file or directory a boot scenario's run reads, and how to make it.
#[derive(Debug, Clone)]
pub struct BootInput {
    pub path: PathBuf,
    /// The command that fetches or builds it, run from the repository root.
    pub make: String,
}

/// `path` for a message: `..` folded away without touching the filesystem (it
/// may not exist yet), and relative to the working directory when under it.
pub fn tidy_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            c => out.push(c),
        }
    }
    match std::env::current_dir() {
        Ok(cwd) => out.strip_prefix(&cwd).map(Path::to_path_buf).unwrap_or(out),
        Err(_) => out,
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

    pub fn sd_path(&self) -> Option<PathBuf> {
        self.boot.sd.as_ref().map(|p| self.base_dir.join(p))
    }

    pub fn golden_path(&self) -> PathBuf {
        self.base_dir.join(&self.golden.path)
    }

    /// The pinned [`RetiredCounts`], beside the golden transcript:
    /// `golden/firmware-boot.txt` has `golden/firmware-boot.retired.toml`.
    pub fn retired_path(&self) -> PathBuf {
        self.golden_path().with_extension("retired.toml")
    }

    /// Every file the run reads, each with the command that makes it. None of
    /// them is committed and a fresh checkout or worktree has none, so
    /// `boot-check --plan` names the missing ones instead of planning a boot
    /// that cannot open its card.
    pub fn inputs(&self) -> Vec<BootInput> {
        let b = &self.boot;
        let mut v = vec![BootInput {
            path: self.eeprom_path(),
            make: "scripts/fetch-firmware.sh".into(),
        }];
        for img in [&b.sd, &b.usb].into_iter().flatten() {
            let path = self.base_dir.join(img);
            // `-halt` is how the repository names the card whose kernel parks
            // the ARM (#52).
            let kernel = if img.contains("-halt") {
                "KERNEL=halt "
            } else {
                ""
            };
            let make = format!("{kernel}scripts/make-sd.sh {}", tidy_path(&path).display());
            v.push(BootInput { path, make });
        }
        for p in [&b.netboot, &b.eeprom_pubkey].into_iter().flatten() {
            v.push(BootInput {
                path: self.base_dir.join(p),
                make: "KERNEL=halt scripts/make-sd.sh firmware/sd-halt.img \
                       && scripts/make-netboot.sh firmware/sd-halt.img"
                    .into(),
            });
        }
        v
    }

    /// The [`Self::inputs`] that are not there.
    pub fn missing_inputs(&self) -> Vec<BootInput> {
        self.inputs()
            .into_iter()
            .filter(|i| !i.path.exists())
            .collect()
    }

    /// The wall budget: the scenario's `wall_secs`, which `boot-check
    /// --max-wall` overrides.
    pub fn wall_secs(&self) -> u64 {
        self.boot.wall_secs
    }

    /// The `boot` argument vector that runs this scenario. `boot-check` runs
    /// exactly this and `--plan` prints it, so the scenario file stays the
    /// only description of the workload.
    pub fn boot_args(&self, console_log: &Path) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "boot".into(),
            self.eeprom_path().display().to_string(),
            "--eeprom".into(),
            // The milestones read the run report too, not only the console.
            "--verbose".into(),
        ];
        let b = &self.boot;
        for (flag, path) in [
            ("--sd", &b.sd),
            ("--usb", &b.usb),
            ("--netboot", &b.netboot),
        ] {
            if let Some(p) = path {
                args.push(flag.into());
                args.push(self.base_dir.join(p).display().to_string());
            }
        }
        if let Some(order) = &b.boot_order {
            args.push("--boot-order".into());
            args.push(order.clone());
        }
        for kv in &b.bootconf {
            args.push("--bootconf".into());
            args.push(kv.clone());
        }
        if let Some(k) = &b.eeprom_pubkey {
            args.push("--eeprom-pubkey".into());
            args.push(self.base_dir.join(k).display().to_string());
        }
        for (flag, value) in [("--stepping", &b.stepping), ("--board-rev", &b.board_rev)] {
            if let Some(v) = value {
                args.push(flag.into());
                args.push(v.clone());
            }
        }
        args.extend([
            "--max-wall".into(),
            self.wall_secs().to_string(),
            "--console-log".into(),
            console_log.display().to_string(),
        ]);
        if !self.boot.mbox_property.is_empty() {
            args.push("--mbox-property".into());
            args.push(self.boot.mbox_property.join(","));
        }
        for line in &self.boot.input {
            args.push("--send-after".into());
            args.push(escape(&line.after));
            args.push(escape(&line.text));
        }
        if let Some(until) = &self.boot.until {
            args.push("--until".into());
            args.push(until.clone());
        }
        args
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
    // Linux's printk prefix, `[    1.858355] ` — seconds, a dot, microseconds.
    if let Some((stamp, tail)) = line.strip_prefix('[').and_then(|r| r.split_once(']')) {
        let is_stamp = matches!(stamp.trim_start().split_once('.'), Some((s, f))
            if !s.is_empty()
                && s.bytes().all(|b| b.is_ascii_digit())
                && f.len() == 6
                && f.bytes().all(|b| b.is_ascii_digit()));
        if is_stamp {
            return format!("[t]{tail}");
        }
    }
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
            return format!(
                "[t] {}",
                scrub_image_digest(&scrub_fat_oem(&scrub_stc(tail)))
            );
        }
    }
    scrub_fat_oem(&scrub_stc(line))
}

/// Replace the FAT OEM name the partition scan prints.
///
/// `type: 32 lba: 2048 'MTOO4049' ' RPIBOOT    ' ...` — that quoted field is
/// written into the filesystem by `mformat`, and mtools stamps its own version
/// into it, so an SD image built on one machine differs from one built on
/// another (`MTOO4049` here, `MTOO4043` on the CI runner). It describes the
/// tool that made the fixture, not anything the firmware did, so it has no
/// business in a transcript that is diffed for firmware changes. The volume
/// label beside it is ours (`RPIBOOT`, set by `scripts/make-sd.sh`) and stays.
fn scrub_fat_oem(line: &str) -> String {
    let Some(i) = line.find("lba: ") else {
        return line.to_string();
    };
    let Some(open) = line[i..].find('\'').map(|o| i + o) else {
        return line.to_string();
    };
    let Some(close) = line[open + 1..].find('\'').map(|c| open + 1 + c) else {
        return line.to_string();
    };
    format!("{}[oem]{}", &line[..open + 1], &line[close..])
}

/// Replace the digest and signature the bootloader prints for a downloaded
/// `boot.img` (`hash: <sha256>`, `rsa2048: <hex>` from its `boot.sig`).
///
/// Both are functions of the image's bytes, and those depend on the tools that
/// built the fixture (`scripts/make-netboot.sh` with the builder's mtools) as
/// much as on anything the firmware did — the same reason the FAT OEM name is
/// scrubbed. Whether the signature *verified* is what matters, and that stays
/// in the transcript (`rsa-verify pass`).
fn scrub_image_digest(line: &str) -> String {
    for (prefix, marker) in [("hash: ", "[sha256]"), ("rsa2048: ", "[signature]")] {
        if let Some(value) = line.strip_prefix(prefix) {
            if !value.is_empty() && value.bytes().all(|b| b.is_ascii_hexdigit()) {
                return format!("{prefix}{marker}");
            }
        }
    }
    line.to_string()
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

/// How many instructions each core retired, as the `boot --verbose` report
/// gives them: `vpu0`, `vpu1` once the firmware has woken VPU core 1, then
/// `arm0`.. for each ARM core once released (#85).
///
/// The model's clock is driven by retired cycles, and so is everything that
/// ends a scenario's run (console silence in modelled time, or a prompt), so
/// the counts reproduce exactly from one machine to the next. Pinning them
/// catches a change that makes a boot run differently without printing
/// anything different.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetiredCounts(pub Vec<(String, u64)>);

impl RetiredCounts {
    /// The counts in the report in `log`; `None` when it has no `retired`
    /// line.
    pub fn from_log(log: &str) -> Option<RetiredCounts> {
        let lines: Vec<&str> = log.lines().collect();
        let last = |prefix: &str| lines.iter().rev().find(|l| l.starts_with(prefix));
        // `retired    419898464  (skipped 0, cycles 419898467)`
        let vpu0 = last("retired ")?.split_whitespace().nth(1)?.parse().ok()?;
        let mut counts = vec![("vpu0".to_string(), vpu0)];
        // `core1      pc 0x3ec40014  retired 125  end None`
        let vpu1 = last("core1 ")
            .and_then(|l| l.split_once(" retired "))
            .and_then(|(_, rest)| rest.split_whitespace().next()?.parse().ok());
        if let Some(n) = vpu1 {
            counts.push(("vpu1".into(), n));
        }
        // `  core 1    still in the armstub`, then
        // `            180077 instructions, 0 exceptions, ...`
        if let Some(at) = lines.iter().rposition(|l| *l == "--- ARM cores (#40) ---") {
            let mut core: Option<usize> = None;
            let section = lines[at + 1..]
                .iter()
                .take_while(|l| l.is_empty() || l.starts_with(' '));
            for l in section {
                if let Some(rest) = l.strip_prefix("  core ") {
                    core = rest.split_whitespace().next().and_then(|i| i.parse().ok());
                } else if let Some((n, _)) = l.trim_start().split_once(" instructions, ") {
                    if let (Some(i), Ok(n)) = (core.take(), n.parse::<u64>()) {
                        counts.push((format!("arm{i}"), n));
                    }
                }
            }
        }
        Some(RetiredCounts(counts))
    }

    /// A counts file: TOML, one `name = count` line per core.
    pub fn parse(text: &str) -> Result<RetiredCounts> {
        let table: toml::Table = toml::from_str(text)?;
        let mut counts = Vec::with_capacity(table.len());
        for (name, value) in table {
            match value.as_integer().map(u64::try_from) {
                Some(Ok(n)) => counts.push((name, n)),
                _ => anyhow::bail!("{name} = {value}: not an instruction count"),
            }
        }
        Ok(RetiredCounts(counts))
    }

    /// The counts file for these counts.
    pub fn render(&self) -> String {
        let mut out = String::from(
            "# Instructions each core retired in this boot (#85): vpu0 and vpu1 on the\n\
             # VideoCore, vpu1 only once the firmware woke it, then arm0.. once the ARM\n\
             # is released. `boot-check <scenario> --update` rewrites this file.\n",
        );
        for (name, n) in &self.0 {
            out.push_str(&format!("{name} = {}\n", grouped(*n)));
        }
        out
    }

    pub fn get(&self, name: &str) -> Option<u64> {
        self.0.iter().find(|(k, _)| k == name).map(|&(_, n)| n)
    }

    /// One line for each count that differs from `expected`, empty when none
    /// does.
    pub fn diff(expected: &RetiredCounts, actual: &RetiredCounts) -> Vec<String> {
        let mut names: Vec<&str> = actual.0.iter().map(|(k, _)| k.as_str()).collect();
        for (k, _) in &expected.0 {
            if !names.contains(&k.as_str()) {
                names.push(k);
            }
        }
        let show = |n: Option<u64>| n.map_or_else(|| "none".to_string(), grouped);
        names
            .into_iter()
            .filter_map(|name| {
                let (old, new) = (expected.get(name), actual.get(name));
                if old == new {
                    return None;
                }
                let delta = match (old, new) {
                    (Some(o), Some(n)) => {
                        let sign = if n < o { '-' } else { '+' };
                        format!("  ({sign}{})", grouped(n.abs_diff(o)))
                    }
                    _ => String::new(),
                };
                Some(format!("{name:<5} {} -> {}{delta}", show(old), show(new)))
            })
            .collect()
    }
}

/// `419898464` as `419_898_464`, which TOML reads back as the same integer.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('_');
        }
        out.push(c);
    }
    out
}

/// Compare a run's counts against the scenario's pinned ones. A mismatch
/// carries [`RetiredCounts::diff`], a line per count.
pub fn check_retired(scn: &BootScenario, actual: &RetiredCounts) -> Result<GoldenCheck> {
    let path = scn.retired_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(GoldenCheck::Missing),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let expected =
        RetiredCounts::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
    let diff = RetiredCounts::diff(&expected, actual);
    Ok(if diff.is_empty() {
        GoldenCheck::Match
    } else {
        GoldenCheck::Mismatch(diff.join("\n"))
    })
}

pub fn write_retired(scn: &BootScenario, counts: &RetiredCounts) -> Result<()> {
    let path = scn.retired_path();
    std::fs::write(&path, counts.render()).with_context(|| format!("writing {}", path.display()))
}

/// How the run ended, from the report's `end` line: `Until`, `Stuck { .. }`,
/// `TimeLimit`.
pub fn run_end(log: &str) -> Option<&str> {
    log.lines()
        .rev()
        .find_map(|l| l.strip_prefix("end "))
        .map(str::trim)
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

    if run_end(log) == Some("TimeLimit") {
        failures.push(
            "END:     the run ended on the wall clock (--max-wall), not on its own\n         \
             why: a scenario's run ends on console silence in modelled time or on a prompt, \
             so its retired counts reproduce; cut off by the host instead, they only say how \
             fast the host was\n"
                .into(),
        );
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

    // Without a report line there is nothing to compare, and the milestones
    // already fail on that.
    if let Some(counts) = RetiredCounts::from_log(log) {
        match check_retired(scn, &counts)? {
            GoldenCheck::Match => {}
            GoldenCheck::Missing => failures.push(format!(
                "MISSING: no retired counts at {}\n         \
                 why: the boot has no instruction counts to be compared against; \
                 re-run with --update to record them\n",
                scn.retired_path().display()
            )),
            GoldenCheck::Mismatch(diff) => {
                let mut f = format!(
                    "RETIRED: the cores ran a different number of instructions than {} says\n",
                    scn.retired_path().display()
                );
                for line in diff.lines() {
                    f.push_str(&format!("         {line}\n"));
                }
                f.push_str(
                    "         why: the counts reproduce exactly, so the boot ran differently, \
                     even where the console does not show it (#85)\n         \
                     (--update rewrites the counts once the change is understood and wanted)\n",
                );
                failures.push(f);
            }
        }
    }

    failures.extend(check_milestones(scn, log));
    Ok(failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_input_escapes_round_trip() {
        let text = "echo a\\b\tc\x03\r\n";
        let line = escape(text);
        assert!(!line.contains('\n'));
        assert_eq!(line, "echo a\\\\b\\tc\\x03\\r\\n");
        assert_eq!(unescape(&line), text.as_bytes());
        assert_eq!(unescape("\\q \\x1"), b"\\q \\x1");
    }

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
    fn a_downloaded_image_digest_and_signature_are_scrubbed() {
        let raw =
            b" 18.51 hash: c2288cbb31ccd4e9d3c3052a15f980049fa593beeb4e51c636f76e3db8f888ba\n\
                    18.51 rsa2048: 9168816c3291\n\
                    44.89 rsa-verify pass (0x0)\n";
        let once = normalise_console(raw);
        assert_eq!(
            once,
            "[t] hash: [sha256]\n[t] rsa2048: [signature]\n[t] rsa-verify pass (0x0)\n"
        );
        assert_eq!(normalise_console(once.as_bytes()), once);
    }

    #[test]
    fn printk_timestamps_are_stripped() {
        let raw = b"[    1.858355] Run /sbin/init as init process\r\n[  OK  ] not a clock\n";
        let once = normalise_console(raw);
        assert_eq!(
            once,
            "[t] Run /sbin/init as init process\n[  OK  ] not a clock\n"
        );
        // Idempotent: the golden is fed back through this.
        assert_eq!(normalise_console(once.as_bytes()), once);
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

    /// Cut from a CI run of `linux-boot.toml`, with the `RVF_ARM_PROF` lines a
    /// diag build adds.
    const LINUX_REPORT: &str = "\
end        Until
final pc   0x3ec40014
retired    930349796  (skipped 0, cycles 930349868)
core1      pc 0x3ec40014  retired 125  end None

--- ARM cores (#40) ---
  ran       2383170777 cycles, 802580388 of them with every core asleep
  core 0    left the armstub at cycle 72 for 0x200000 in EL2, x0 = 0x2eff1e00
            191482186 instructions, 833 exceptions, 689 interrupts; now pc 0xffffffe5d4c75e00  EL1  sp 0xffffffe5d5733d80  (wfi)
            daif 0x0  irq line 0  gic -
  core 1    left the armstub at cycle 124611862 for 0x15a04b0 in EL2, x0 = 0x0
            1376195290 instructions, 823 exceptions, 747 interrupts; now pc 0xffffffe5d3f39f18  EL1  sp 0xffffffc080a7bb90
  core 2    still in the armstub
            180077 instructions, 0 exceptions, 0 interrupts; now pc 0x80  EL2  sp 0x0  (wfi)
  RVF_ARM_PROF: steps by core, EL and 256-byte PC bucket (total 5)
    core 3 EL1 0x00000000000080              5  100.0%

--- property replies (0x7e00_b880) ---
  1 replies, 0 with an error code
";

    #[test]
    fn retired_counts_come_from_the_report() {
        let counts = RetiredCounts::from_log(LINUX_REPORT).expect("counts");
        let want = [
            ("vpu0", 930349796),
            ("vpu1", 125),
            ("arm0", 191482186),
            ("arm1", 1376195290),
            ("arm2", 180077),
        ];
        let want: Vec<(String, u64)> = want.iter().map(|&(k, n)| (k.into(), n)).collect();
        assert_eq!(counts.0, want);
        assert_eq!(run_end(LINUX_REPORT), Some("Until"));

        let firmware_only = "retired    419898464  (skipped 0, cycles 419898467)\n\n\
                             --- ARM cores (#40) ---\n  never released\nregs\n";
        let counts = RetiredCounts::from_log(firmware_only).expect("counts");
        assert_eq!(counts.0, vec![("vpu0".to_string(), 419898464)]);
        assert_eq!(RetiredCounts::from_log("no report here\n"), None);
    }

    #[test]
    fn a_counts_file_reads_back_what_was_written() {
        let counts = RetiredCounts::from_log(LINUX_REPORT).expect("counts");
        let text = counts.render();
        assert!(text.contains("\nvpu0 = 930_349_796\n"), "{text}");
        assert!(text.contains("\nvpu1 = 125\n"), "{text}");
        assert!(text.contains("\narm1 = 1_376_195_290\n"), "{text}");
        let back = RetiredCounts::parse(&text).expect("parse");
        assert!(RetiredCounts::diff(&counts, &back).is_empty());
        assert!(RetiredCounts::parse("vpu0 = -1\n").is_err());
        assert!(RetiredCounts::parse("vpu0 = \"1\"\n").is_err());
    }

    #[test]
    fn a_counts_diff_shows_each_change() {
        let old = RetiredCounts(vec![("vpu0".into(), 1_000), ("vpu1".into(), 125)]);
        let new = RetiredCounts(vec![("vpu0".into(), 998), ("arm0".into(), 1_234_567)]);
        assert_eq!(
            RetiredCounts::diff(&old, &new),
            [
                "vpu0  1_000 -> 998  (-2)",
                "arm0  none -> 1_234_567",
                "vpu1  125 -> none",
            ]
        );
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(100), "100");
        assert_eq!(grouped(1000), "1_000");
    }

    #[test]
    fn a_run_cut_off_by_the_wall_clock_is_no_baseline() {
        let scn: BootScenario = toml::from_str(
            "name = \"x\"\n[boot]\neeprom = \"e\"\nwall_secs = 1\n[golden]\npath = \"g\"\n",
        )
        .expect("scenario");
        let log = "end        TimeLimit\nretired    5  (skipped 0, cycles 5)\n";
        let failures = check_milestones(&scn, log);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("wall clock"), "{}", failures[0]);
        assert!(check_milestones(&scn, &log.replace("TimeLimit", "Until")).is_empty());
    }
}
