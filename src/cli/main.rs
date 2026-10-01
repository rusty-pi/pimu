//! `pimu` command-line entry point. Each command lives in a module of its own.

#[cfg(feature = "repo")]
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, bail, Context, Result};

mod boot;
mod check;
mod config;
mod disasm;
mod mbox;
mod otp;
#[cfg(feature = "repo")]
mod scenario;
mod vchiq;

/// The repository-only commands (`repo`): golden transcripts under `testdata/`
/// and docs written back through `CARGO_MANIFEST_DIR`, neither of which a
/// published binary has.
#[cfg(feature = "repo")]
const REPO_USAGE: &str = "    pimu run <scenario.yaml> [--update] [-v]
    pimu run-all [<dir>] [--update] [-v]
";

#[cfg(not(feature = "repo"))]
const REPO_USAGE: &str = "";

#[cfg(feature = "repo")]
const REPO_COMMANDS: &str =
    "    run       Run one scenario and check it against its golden transcript.
    run-all   Run every *.yaml scenario in <dir> (default: testdata/scenarios).
";

#[cfg(not(feature = "repo"))]
const REPO_COMMANDS: &str = "";

#[cfg(feature = "repo")]
const SPEC_DOCS_USAGE: &str = "    pimu spec-docs [--update]\n";

#[cfg(not(feature = "repo"))]
const SPEC_DOCS_USAGE: &str = "";

#[cfg(feature = "repo")]
const SPEC_DOCS_COMMAND: &str =
    "    spec-docs Check the generated docs — docs/periph/ against the register
              specs in specs/*.toml, and docs/board-sheet-dark.svg against the
              hand-drawn docs/board-sheet.svg; --update regenerates them.
";

#[cfg(not(feature = "repo"))]
const SPEC_DOCS_COMMAND: &str = "";

#[cfg(feature = "repo")]
const UPDATE_FLAG: &str = "    --update  Rewrite golden files instead of failing on mismatch.\n";

#[cfg(not(feature = "repo"))]
const UPDATE_FLAG: &str = "";

#[cfg(feature = "repo")]
const VERBOSE_FLAG: &str = "    -v, --verbose
              `run`: print the full report and transcript. `boot`: print the
              full run report as well (see `boot --help`).
";

#[cfg(not(feature = "repo"))]
const VERBOSE_FLAG: &str = "    -v, --verbose
              `boot`: print the full run report as well (see `boot --help`).
";

const USAGE_HEAD: &str = "\
pimu — virtual bench for Raspberry Pi VideoCore boot firmware

USAGE:
";

const BOOT_USAGE: &str = "    pimu boot --eeprom <pieeprom.bin> | <file.elf> | <dir> [<options>]
";

const DISASM_USAGE: &str = "    pimu disasm <file> [--base <hex>] [--count <n>] [--vaddr <hex>]\n";

const BOOT_COMMAND: &str = "
COMMANDS:
    boot      Boot the machine from an EEPROM image (--eeprom), as a Pi 4 does,
              or run a VPU ELF. An option left out takes the file of that name
              in the working directory when there is one — `pieeprom.bin`,
              `sd.img`, `usb.img`, `otg.img`, `tftp-boot/`, `http-boot/`, `otp.json`/`otp.bin`,
              `bootconf.txt`, `pubkey.bin` — so a directory holding those boots
              with a bare `pimu boot`, and `pimu boot <dir>` reads them
              from <dir>. A directory of a boot partition's own files
              (`start4.elf`, `config.txt`) is the card itself.
              `pimu boot --help` lists its options.
";

const DISASM_COMMAND: &str =
    "    disasm    Disassemble a flat binary / ELF with the (partial) VPU decoder.
";

const FLAGS_HEAD: &str = "
    With no command, the options are `boot`'s: `pimu --eeprom <file> ...`.

FLAGS:
    -C <dir>  Work in <dir>: every relative path on the command line is read
              from there, and it is where a bare `boot` looks for the files it
              was not given.
    --config <file>
              Take options from <file> as well, at that point in the command
              line: a JSON object (or YAML mapping) keyed by long option name,
              e.g. {\"eeprom\": \"firmware/pieeprom.bin\", \"max-wall\": 600,
              \"v\": true, \"bootconf\": [\"A=1\", \"B=2\"]}. `true` is a flag,
              an array repeats the option, `\"file\"` is the positional argument.
              Options after it on the command line win. Any option also takes
              the `--option=value` form.
";

fn usage() -> String {
    [
        USAGE_HEAD,
        BOOT_USAGE,
        REPO_USAGE,
        DISASM_USAGE,
        SPEC_DOCS_USAGE,
        BOOT_COMMAND,
        REPO_COMMANDS,
        DISASM_COMMAND,
        SPEC_DOCS_COMMAND,
        FLAGS_HEAD,
        UPDATE_FLAG,
        VERBOSE_FLAG,
    ]
    .concat()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode> {
    // Before everything else: `-C` moves where a relative `--config` is read from.
    let args = chdir(args)?;
    let args = &args[..];
    // No command, only options: `boot`. Before config expansion, whose `"file"` would look like a command.
    let implicit_boot = args
        .first()
        .is_some_and(|a| a.starts_with('-') && !matches!(a.as_str(), "-h" | "--help"));
    let args = config::expand(args)?;
    if implicit_boot {
        return boot::cmd_boot(&args);
    }
    let Some(cmd) = args.first() else {
        print!("{}", usage());
        return Ok(ExitCode::SUCCESS);
    };

    match cmd.as_str() {
        #[cfg(feature = "repo")]
        "run" => scenario::cmd_run(&args[1..]),
        #[cfg(feature = "repo")]
        "run-all" => scenario::cmd_run_all(&args[1..]),
        // `recon` is the old name of `boot`, kept for old command lines.
        "boot" | "recon" => boot::cmd_boot(&args[1..]),
        "disasm" => disasm::cmd_disasm(&args[1..]),
        #[cfg(feature = "repo")]
        "spec-docs" => cmd_spec_docs(&args[1..]),
        "-h" | "--help" | "help" => {
            print!("{}", usage());
            Ok(ExitCode::SUCCESS)
        }
        other => bail!("unknown command '{other}' (try --help)"),
    }
}

fn chdir(args: &[String]) -> Result<Vec<String>> {
    let mut rest = Vec::with_capacity(args.len());
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let Some(tail) = arg.strip_prefix("-C") else {
            rest.push(arg.clone());
            continue;
        };
        let dir = match tail.strip_prefix('=').unwrap_or(tail) {
            "" => args.next().ok_or_else(|| anyhow!("-C needs a directory"))?,
            dir => dir,
        };
        std::env::set_current_dir(dir).with_context(|| format!("-C {dir}"))?;
    }
    Ok(rest)
}

/// `spec-docs [--update]`: report (or rewrite) the generated docs that are out of date.
#[cfg(feature = "repo")]
fn cmd_spec_docs(args: &[String]) -> Result<ExitCode> {
    let mut update = false;
    for a in args {
        match a.as_str() {
            "--update" => update = true,
            other => bail!("unknown argument '{other}'"),
        }
    }
    let mut stale: Vec<PathBuf> = Vec::new();
    let dir = pimu::spec::doc_dir();
    stale.extend(
        pimu::spec::sync_docs(update)
            .map_err(anyhow::Error::msg)?
            .iter()
            .map(|name| dir.join(name)),
    );
    stale.extend(pimu::isa::sync_docs(update).map_err(anyhow::Error::msg)?);
    stale.extend(pimu::sheet::sync(update).map_err(anyhow::Error::msg)?);
    stale.extend(pimu::harness::schema::sync(update).map_err(anyhow::Error::msg)?);
    for path in &stale {
        let verb = if update { "updated" } else { "stale" };
        println!("{verb}: {}", path.display());
    }
    if stale.is_empty() || update {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("generated docs are out of date; run `cargo run -- spec-docs --update`");
        Ok(ExitCode::FAILURE)
    }
}

/// A byte image written as hex, with an optional `0x` and any grouping punctuation.
fn parse_hex_image(s: &str) -> Result<Vec<u8>> {
    let hex: String = s
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .chars()
        .filter(|c| !matches!(c, ' ' | '_' | ':' | ',' | '\n' | '\t'))
        .collect();
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("not hex: {s}");
    }
    if !hex.len().is_multiple_of(2) {
        bail!(
            "a byte image needs an even number of hex digits, got {}",
            hex.len()
        );
    }
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(Into::into))
        .collect()
}

/// A number: hex with a `0x` prefix, decimal otherwise.
fn parse_u32(s: &str) -> Result<u32> {
    let s = s.trim();
    let v = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)?
    } else {
        s.parse()?
    };
    Ok(v)
}
