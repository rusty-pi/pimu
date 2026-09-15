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
    rpi-virt-fw boot --eeprom <pieeprom.bin> | <file.elf>
                             [--entry <hex>] [--ram-mb <n>] [--max-steps <n>]
                             [--max-wall <secs>] [--sd <img>] [--usb <img>] [--usb-mb <n>]
                             [--boot-order <hex>] [--bootconf <KEY=VALUE>]...
                             [--skip-signed-boot] [--netboot <dir> | --net passt[:<socket>]]
                             [--eeprom-pubkey <pubkey.bin>] [--boot-rom <rom.bin>]
                             [--stepping b0|c0] [--board-rev <hex>] [--skip-unimpl]
              (--stepping: the BCM2711 silicon, C0 by default; --board-rev: the
               OTP revision code, by default a board that stepping shipped on)
              (no --max-steps = no instruction cap; --max-wall defaults to 140s)
              (an unknown instruction stops the run; --skip-unimpl steps over it
               instead, for reconnaissance on firmware the decoder is new to)
                             [--dump <hex>:<len>] [--disasm <hex>:<count>] [--patch <hex>=<hex>]
                             [--dump-fdt <path>] [--print-fdt] [--console-log <path>]
                             [--mbox-property <tag>[,<tag>...]] [--until <text>]
                             [--send-after <prompt> <text>]... [--stdin]
                             [--log [text:|jsonl:]<channel>[,...]]... [--log-file <path>]
                             [--otp json:<file> | binary:<file>]
    rpi-virt-fw boot-check <scenario.toml> --plan [--console <path>]
    rpi-virt-fw boot-check <scenario.toml> --log <path> --console <path> [--update]
    rpi-virt-fw disasm <file> [--base <hex>] [--count <n>] [--vaddr <hex>]
    rpi-virt-fw spec-docs [--update]

COMMANDS:
    run       Run one scenario and check it against its golden transcript.
    run-all   Run every *.toml scenario in <dir> (default: testdata/scenarios).
    boot      Boot the machine from an EEPROM image (--eeprom), as a Pi 4 does,
              or run a VPU ELF. Stops on an instruction the decoder does not
              implement (--skip-unimpl steps over it instead). `boot <file>
              --eeprom` is the same as `boot --eeprom <file>`.
    boot-check
              Check a finished firmware boot against a boot scenario: the
              golden console transcript plus every named milestone. `--plan`
              prints the `boot` invocation the scenario describes, which is
              how `scripts/boot-check.sh` runs the boot without repeating the
              workload description. It fails instead when a file the run reads
              is missing, naming the command that makes each.
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
    --console-log <path>
              Write the raw UART bytes of the run to <path>, with none of the
              run report interleaved. This is what `boot-check` normalises into
              the golden boot transcript.
    --dump-fdt <path>
              After the run, write the flattened device tree `arm_loader` handed
              to the ARM to <path>. Diff two firmware versions with
              `fdtdump`/`dtc` to catch a bump that changes what the firmware
              publishes (rpi-mkosi#37).
    --print-fdt
              Print that whole device tree as source, every node and property,
              not only the `/chosen` summary the `-v` run report gives.
    --mbox-property <tag>[,<tag>...]
              After the boot, post a property-interface request to the firmware
              the way a booted Linux would (`/dev/vcio`), and print what the
              still-running `start4.elf` answers. Tags are hex, e.g.
              `0x00000001` (GET_FIRMWARE_REVISION) or `0x00030092`
              (GET_CRYPTO_HMAC_SHA256). See docs/diagnostics.md.
    --until <text>
              End the run once the console prints <text> (e.g. the shell
              prompt of a Linux boot), after the last --send-after went in.
    --send-after <prompt> <text>
              Type <text> into the serial console (PL011) once it prints
              <prompt>. Repeatable; each prompt is looked for only in what the
              console printed after the previous send. Both take \\n, \\r, \\t,
              \\\\ and \\xHH escapes. Deterministic: keyed to the transcript.
    --net passt[:<socket>]
              Plug the Ethernet cable into the host's network instead of the
              built-in peer, through passt: `passt` starts one (from PATH) on a
              socket pair, `passt:<socket>` connects to one already listening
              (`passt -f -s <socket>`), or to anything else speaking QEMU's
              `-netdev stream` framing on that UNIX socket. Runs on the host's
              clock, so not deterministic (#45).
    --stdin   Interactive session: the host's stdin is the serial console's
              input, and no wall-clock or silence limit ends the run. On a
              terminal, keys go to the guest raw (Ctrl-C included); Ctrl-A x
              quits, Ctrl-A Ctrl-A sends a Ctrl-A.
    --log [text:|jsonl:]<channel>[,<channel>...]
              Say what these subsystems did, on stderr or to --log-file
              <path>. `io` is what the machine read and wrote apart from the
              console: SD card and USB stick block runs with the files they
              belong to, OTP rows read and programmed, and what the network
              peer did (DHCP, DNS, TFTP, HTTP). The rest are one device or
              core each: arm-exc cmp dwc2 emmc expander irqen mbox otp pcie
              pmic spi xhci, and in a `diag` build derail dma ff irqtbl sleep
              swirq tick vec. Repeatable; `jsonl:` for one JSON object a line.
              See docs/diagnostics.md.
    --otp json:<file> | binary:<file>
              The OTP fuses, kept across runs: read before the boot when
              <file> exists, written back after the run when the firmware
              programmed a row, created from the model's own fuses when it
              does not exist. json: row -> value, one a line; binary: row n
              at byte 4n, little-endian. A file with a real board's fuses
              holds its secrets: keep it out of the repository.
    --dram-map
              Report which DRAM pages are non-zero when the run ends, as
              address runs: the RAM a snapshot of the machine would have to
              carry (#50).
    -v, --verbose
              `boot`: print the full run report as well — the EEPROM layout,
              registers, the ARM cores, the property replies, the peripherals
              that fell through to the stub, the device tree's `/chosen`.
              Without it `boot` prints the serial console, what was asked for
              by name (`--dump`, `--print-fdt`, `--mbox-property`, …) and one
              `result: ok|FAILED — <why>` line; the exit status is 1 on
              failure. `run`: print the full report and transcript.
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
