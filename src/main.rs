//! `rpi-virt-fw` command-line entry point.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

use rpi_virt_fw::harness::{self, GoldenOutcome};
use rpi_virt_fw::vpu::decode::decode;
use rpi_virt_fw::vpu::length::insn_len_bytes;

const USAGE: &str = "\
rpi-virt-fw — virtual bench for Raspberry Pi VideoCore boot firmware

USAGE:
    rpi-virt-fw run <scenario.toml> [--update] [-v]
    rpi-virt-fw run-all [<dir>] [--update] [-v]
    rpi-virt-fw disasm <file> [--base <hex>] [--count <n>]

COMMANDS:
    run       Run one scenario and check it against its golden transcript.
    run-all   Run every *.toml scenario in <dir> (default: testdata/scenarios).
    disasm    Disassemble a flat binary with the (partial) VPU decoder.

FLAGS:
    --update  Rewrite golden files instead of failing on mismatch.
    -v        Print the full run report and transcript.
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
    let Some(cmd) = args.first() else {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };

    match cmd.as_str() {
        "run" => cmd_run(&args[1..]),
        "run-all" => cmd_run_all(&args[1..]),
        "disasm" => cmd_disasm(&args[1..]),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => bail!("unknown command '{other}' (try --help)"),
    }
}

fn cmd_run(args: &[String]) -> Result<ExitCode> {
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

fn cmd_run_all(args: &[String]) -> Result<ExitCode> {
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

fn cmd_disasm(args: &[String]) -> Result<ExitCode> {
    let mut path: Option<PathBuf> = None;
    let mut base: u32 = 0;
    let mut count: usize = 64;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--base" => {
                base = parse_u32(it.next().context("--base needs a value")?)?;
            }
            "--count" => {
                count = it.next().context("--count needs a value")?.parse()?;
            }
            s if !s.starts_with('-') => path = Some(PathBuf::from(s)),
            s => bail!("unexpected argument '{s}'"),
        }
    }
    let path = path.context("disasm: missing <file>")?;
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;

    let mut pc = base;
    let mut off = 0usize;
    for _ in 0..count {
        if off + 2 > bytes.len() {
            break;
        }
        let p0 = u16::from_le_bytes([bytes[off], bytes[off + 1]]);
        let len = insn_len_bytes(p0) as usize;
        if off + len > bytes.len() {
            println!("{pc:#010x}:  (truncated {len}-byte insn)");
            break;
        }
        let insn = decode(&bytes[off..off + len], pc);
        let raw: String = bytes[off..off + len]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        println!("{pc:#010x}:  {raw:<20}  {:?}", insn.op);
        pc = pc.wrapping_add(len as u32);
        off += len;
    }
    Ok(ExitCode::SUCCESS)
}

fn parse_u32(s: &str) -> Result<u32> {
    let s = s.trim();
    let v = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)?
    } else {
        s.parse()?
    };
    Ok(v)
}
