//! `pimu` command-line entry point. Each command lives in a module of its own.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, bail, Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

mod boot;
mod boot_args;
mod check;
mod config;
mod disasm;
mod mbox;
mod otp;
#[cfg(feature = "repo")]
mod scenario;
mod vchiq;

const ABOUT: &str = "pimu — virtual bench for Raspberry Pi VideoCore boot firmware";

const AFTER: &str = "\
With no command, the options are `boot`'s: `pimu --eeprom <file> ...`.

An option left out of `boot` takes the file of that name in the working
directory when there is one — `pieeprom.bin`, `sd.img`, `usb.img`, `otg.img`,
`tftp/`, `http/`, `otp.json`/`otp.bin`, `bootconf.txt`, `pubkey.bin` — so a
directory holding those boots with a bare `pimu boot`, and `pimu boot <dir>`
reads them from <dir>. A directory of a boot partition's own files
(`start4.elf`, `config.txt`) is the card itself. `pimu boot --help` lists its
options.";

#[derive(Parser)]
#[command(
    name = "pimu",
    about = ABOUT,
    after_help = AFTER,
    disable_version_flag = true,
    subcommand_required = true
)]
struct Cli {
    /// Work in <dir>: every relative path on the command line is read from
    /// there, and it is where a bare `boot` looks for the files it was not
    /// given.
    #[arg(short = 'C', value_name = "dir")]
    chdir: Option<PathBuf>,

    /// Take a command's options from <file> as well, at that point in the
    /// command line: a JSON object (or YAML mapping) keyed by long option name,
    /// e.g. {"eeprom": "firmware/pieeprom.bin", "max-wall": 600, "v": true,
    /// "bootconf": ["A=1", "B=2"]}. `true` is a flag, an array repeats the
    /// option, `"file"` is the positional argument. Options after it on the
    /// command line win.
    #[arg(long, value_name = "file")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    // `recon` is the old name of `boot`, kept for old command lines.
    #[command(alias = "recon")]
    Boot(Box<boot_args::BootArgs>),
    /// Run one scenario and check it against its golden transcript.
    #[cfg(feature = "repo")]
    Run(scenario::RunArgs),
    /// Run every *.yaml scenario in <dir> (default: testdata/scenarios).
    #[cfg(feature = "repo")]
    RunAll(scenario::RunAllArgs),
    /// Disassemble a flat binary / ELF with the (partial) VPU decoder.
    Disasm(disasm::DisasmArgs),
    /// Check the generated docs — docs/periph/ against the register specs in
    /// specs/*.toml, and docs/board-sheet-dark.svg against the hand-drawn
    /// docs/board-sheet.svg; --update regenerates them.
    #[cfg(feature = "repo")]
    SpecDocs(SpecDocsArgs),
}

#[cfg(feature = "repo")]
#[derive(clap::Args)]
struct SpecDocsArgs {
    /// Rewrite the generated docs instead of failing on a stale one.
    #[arg(long)]
    update: bool,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(e) => match e.downcast_ref::<clap::Error>() {
            Some(e) => {
                let _ = e.print();
                ExitCode::from(e.exit_code() as u8)
            }
            None => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}

fn run(args: &[String]) -> Result<ExitCode> {
    // Before everything else: `-C` moves where a relative `--config` is read from.
    let mut args = chdir(args)?;
    // No command, only options: `boot`.
    if args
        .first()
        .is_some_and(|a| a.starts_with('-') && !matches!(a.as_str(), "-h" | "--help"))
    {
        args.insert(0, "boot".into());
    }
    let mut cli = Cli::command();
    if args.is_empty() {
        cli.print_help()?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut tail = Vec::new();
    if let Some(sub) = cli.find_subcommand_mut(&args[0]) {
        tail = config::expand(sub, &args[1..])?;
        args.truncate(1);
        args.extend(tail.iter().cloned());
    }
    let m = cli.try_get_matches_from(std::iter::once("pimu".to_string()).chain(args))?;
    let (_, sub) = m.subcommand().context("a command")?;
    match Cmd::from_arg_matches(&m)? {
        Cmd::Boot(_) => boot::cmd_boot(&tail, sub),
        #[cfg(feature = "repo")]
        Cmd::Run(a) => scenario::cmd_run(a),
        #[cfg(feature = "repo")]
        Cmd::RunAll(a) => scenario::cmd_run_all(a),
        Cmd::Disasm(a) => disasm::cmd_disasm(a),
        #[cfg(feature = "repo")]
        Cmd::SpecDocs(a) => cmd_spec_docs(a.update),
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
fn cmd_spec_docs(update: bool) -> Result<ExitCode> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declarations_hold_together() {
        Cli::command().debug_assert();
    }
}
