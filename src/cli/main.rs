//! `rpi-virt-fw` command-line entry point: the usage text and the command
//! dispatch. Each command lives in a module of its own.

use std::process::ExitCode;

use anyhow::{bail, Result};

mod boot;
mod config;
mod disasm;
mod mbox;
mod otp;
mod scenario;

const USAGE: &str = "\
rpi-virt-fw — virtual bench for Raspberry Pi VideoCore boot firmware

USAGE:
    rpi-virt-fw run <scenario.toml> [--update] [-v]
    rpi-virt-fw run-all [<dir>] [--update] [-v]
    rpi-virt-fw boot --eeprom <pieeprom.bin> | <file.elf> [<options>]
    rpi-virt-fw boot-check <scenario.toml> [--update] [--output <log>] [--max-wall <secs>]
    rpi-virt-fw boot-check <scenario.toml> --from <log> [--update]
    rpi-virt-fw boot-check <scenario.toml> --plan [--output <log>] [--max-wall <secs>]
    rpi-virt-fw disasm <file> [--base <hex>] [--count <n>] [--vaddr <hex>]
    rpi-virt-fw spec-docs [--update]

COMMANDS:
    run       Run one scenario and check it against its golden transcript.
    run-all   Run every *.toml scenario in <dir> (default: testdata/scenarios).
    boot      Boot the machine from an EEPROM image (--eeprom), as a Pi 4 does,
              or run a VPU ELF. `rpi-virt-fw boot --help` lists its options.
    boot-check
              Run the firmware boot a boot scenario describes and check it:
              the golden console transcript plus every named milestone. The
              combined output goes to --output (boot.log), the console next to
              it as <log>.console; --from checks such a pair from an earlier
              run without booting. --update re-records the golden; --max-wall
              overrides the scenario's wall budget.
              `--plan` prints the `boot` invocation instead, one argument a
              line. Both refuse when a file the run reads is missing, naming
              the command that makes each.
    disasm    Disassemble a flat binary / ELF with the (partial) VPU decoder.
    spec-docs Check docs/periph/ against the register specs in specs/*.toml;
              --update regenerates it.

    With no command, the options are `boot`'s: `rpi-virt-fw --eeprom <file> ...`.

FLAGS:
    --config <file>
              Take options from <file> as well, at that point in the command
              line: a JSON object (or TOML table) keyed by long option name,
              e.g. {\"eeprom\": \"firmware/pieeprom.bin\", \"max-wall\": 600,
              \"v\": true, \"bootconf\": [\"A=1\", \"B=2\"]}. `true` is a flag,
              an array repeats the option, `\"file\"` is the positional argument.
              Options after it on the command line win. Any option also takes
              the `--option=value` form.
    --update  Rewrite golden files instead of failing on mismatch.
    -v, --verbose
              `run`: print the full report and transcript. `boot`: print the
              full run report as well (see `boot --help`).
";

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
    // No command, only options: `boot` is the one they are for. Decided before
    // a config file expands, since its `"file"` would look like a command.
    let implicit_boot = args
        .first()
        .is_some_and(|a| a.starts_with('-') && !matches!(a.as_str(), "-h" | "--help"));
    let args = config::expand(args)?;
    if implicit_boot {
        return boot::cmd_boot(&args);
    }
    let Some(cmd) = args.first() else {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };

    match cmd.as_str() {
        "run" => scenario::cmd_run(&args[1..]),
        "run-all" => scenario::cmd_run_all(&args[1..]),
        // `recon` is the old name, from when most of a boot was unknown
        // instructions (#57).
        "boot" | "recon" => boot::cmd_boot(&args[1..]),
        "boot-check" => scenario::cmd_boot_check(&args[1..]),
        "disasm" => disasm::cmd_disasm(&args[1..]),
        "spec-docs" => cmd_spec_docs(&args[1..]),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => bail!("unknown command '{other}' (try --help)"),
    }
}

/// `spec-docs [--update]`: the Markdown under `docs/periph/` is generated from
/// `specs/*.toml`; report (or with `--update`, rewrite) whatever is out of date.
fn cmd_spec_docs(args: &[String]) -> Result<ExitCode> {
    let mut update = false;
    for a in args {
        match a.as_str() {
            "--update" => update = true,
            other => bail!("unknown argument '{other}'"),
        }
    }
    let stale = rpi_virt_fw::spec::sync_docs(update).map_err(anyhow::Error::msg)?;
    let dir = rpi_virt_fw::spec::doc_dir();
    for name in &stale {
        let verb = if update { "updated" } else { "stale" };
        println!("{verb}: {}", dir.join(name).display());
    }
    if stale.is_empty() || update {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("docs/periph is out of date; run `cargo run -- spec-docs --update`");
        Ok(ExitCode::FAILURE)
    }
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
