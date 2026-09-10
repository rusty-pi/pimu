//! `rpi-virt-fw` command-line entry point.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

use rpi_virt_fw::emulator::{Emulator, RunLimits};
use rpi_virt_fw::firmware::Payload;
use rpi_virt_fw::harness::{self, GoldenOutcome};
use rpi_virt_fw::machine::Machine;
use rpi_virt_fw::vpu::decode::decode;
use rpi_virt_fw::vpu::length::insn_len_bytes;
use rpi_virt_fw::vpu::UnimplPolicy;

const USAGE: &str = "\
rpi-virt-fw — virtual bench for Raspberry Pi VideoCore boot firmware

USAGE:
    rpi-virt-fw run <scenario.toml> [--update] [-v]
    rpi-virt-fw run-all [<dir>] [--update] [-v]
    rpi-virt-fw recon <file> [--entry <hex>] [--ram-mb <n>] [--max-steps <n>] [--eeprom]
                             [--max-wall <secs>] [--sd <img>] [--skip-signed-boot]
                             [--dump <hex>:<len>] [--disasm <hex>:<count>] [--patch <hex>=<hex>]
    rpi-virt-fw disasm <file> [--base <hex>] [--count <n>] [--vaddr <hex>]

COMMANDS:
    run       Run one scenario and check it against its golden transcript.
    run-all   Run every *.toml scenario in <dir> (default: testdata/scenarios).
    recon     Load an ELF (or --eeprom image) and run it in skip-on-unimplemented
              mode, reporting how far it got and which instructions it needs.
    disasm    Disassemble a flat binary / ELF with the (partial) VPU decoder.

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
        "recon" => cmd_recon(&args[1..]),
        "disasm" => cmd_disasm(&args[1..]),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => bail!("unknown command '{other}' (try --help)"),
    }
}

fn cmd_recon(args: &[String]) -> Result<ExitCode> {
    let mut path: Option<PathBuf> = None;
    let mut entry: Option<u32> = None;
    let mut ram_mb: Option<u32> = None;
    let mut max_steps: u64 = 20_000_000;
    let mut max_wall_secs: u64 = 120;
    let mut eeprom = false;
    let mut trace = false;
    let mut trace_full = false;
    let mut trace_mmio = false;
    let mut exc_vbase: u32 = 0;
    let mut trace_from: u32 = 0;
    let mut core1_entry: Option<u32> = None;
    let mut smp = false;
    let mut as_core1 = false;
    let mut patches: Vec<(u32, u32)> = Vec::new();
    let mut dumps: Vec<(u32, u32)> = Vec::new();
    let mut disasms: Vec<(u32, u32)> = Vec::new();
    let mut sd_image: Option<PathBuf> = None;
    let mut skip_signed_boot = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--entry" => entry = Some(parse_u32(it.next().context("--entry needs a value")?)?),
            "--ram-mb" => ram_mb = Some(it.next().context("--ram-mb needs a value")?.parse()?),
            "--max-steps" => max_steps = it.next().context("--max-steps needs a value")?.parse()?,
            "--max-wall" => {
                max_wall_secs = it.next().context("--max-wall needs seconds")?.parse()?
            }
            "--eeprom" => eeprom = true,
            "--trace" => trace = true,
            "--trace-full" => {
                trace = true;
                trace_full = true;
            }
            "--exc-vbase" => {
                exc_vbase = parse_u32(it.next().context("--exc-vbase needs a value")?)?
            }
            "--smp" => smp = true,
            "--as-core1" => as_core1 = true,
            "--core1-entry" => {
                core1_entry =
                    Some(parse_u32(it.next().context("--core1-entry needs a value")?)?)
            }
            "--trace-from" => {
                trace = true;
                trace_from = parse_u32(it.next().context("--trace-from needs a value")?)?
            }
            "--trace-mmio" => trace_mmio = true,
            "--sd" => sd_image = Some(PathBuf::from(it.next().context("--sd needs a path")?)),
            "--skip-signed-boot" => skip_signed_boot = true,
            "--dump" => {
                let spec = it.next().context("--dump needs <hexaddr>:<len>")?;
                let (a, n) = spec.split_once(':').context("--dump: expected addr:len")?;
                dumps.push((parse_u32(a)?, parse_u32(n)?));
            }
            "--disasm" => {
                let spec = it.next().context("--disasm needs <hexaddr>:<count>")?;
                let (a, n) = spec.split_once(':').context("--disasm: expected addr:count")?;
                disasms.push((parse_u32(a)?, parse_u32(n)?));
            }
            "--patch" => {
                let spec = it.next().context("--patch needs <hexaddr>=<hexval>")?;
                let (a, v) = spec.split_once('=').context("--patch: expected addr=val")?;
                patches.push((parse_u32(a)?, parse_u32(v)?));
            }
            s if !s.starts_with('-') => path = Some(PathBuf::from(s)),
            s => bail!("unexpected argument '{s}'"),
        }
    }
    let path = path.context("recon: missing <file>")?;
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;

    // The EEPROM bootloader touches the 0x6000_0000 L2-SRAM window, which
    // our model folds into DRAM past the 512 MiB mark — give it room by default.
    let ram_mb = ram_mb.unwrap_or(if eeprom { 2048 } else { 512 });
    let sd_img = match &sd_image {
        Some(sd_path) => {
            let img = std::fs::read(sd_path)
                .with_context(|| format!("reading SD image {}", sd_path.display()))?;
            println!("sd image   {} ({} blocks)", sd_path.display(), img.len() / 512);
            Some(img)
        }
        None => None,
    };

    // `--skip-signed-boot`: flip `SIGNED_BOOT=1` -> `=0` in the EEPROM's
    // `bootconf.txt`. That flag gates the bootloader's signature enforcement, so
    // clearing it skips the (very slow, ~0.5 G interpreted instructions) SHA-256
    // + RSA-2048 verify of `boot.img`. Same length, so the byte layout is
    // preserved; the now-stale `bootconf.sig` is not checked once the flag is 0.
    // Re-applied after every EEPROM self-update (which restores `SIGNED_BOOT=1`).
    let unsign = |flash: &mut Vec<u8>, announce: bool| {
        if !(skip_signed_boot && eeprom) {
            return;
        }
        let needle = b"SIGNED_BOOT=1";
        if let Some(i) = flash.windows(needle.len()).position(|w| w == needle) {
            flash[i + needle.len() - 1] = b'0';
            if announce {
                println!("skip-signed-boot: patched bootconf SIGNED_BOOT=0 @ {i:#x}");
            }
        } else if announce {
            eprintln!("skip-signed-boot: 'SIGNED_BOOT=1' not found in EEPROM image");
        }
    };

    // `flash` may be rewritten by an EEPROM self-update; on a firmware-requested
    // reset we rebuild from the updated image and run again.
    let mut flash = bytes.clone();
    unsign(&mut flash, true);

    // Show the EEPROM section table `bootloader_eeprom_find_files` walks, plus
    // the decoded `bootconf.txt`, so a boot that consults EEPROM config (boot
    // order etc.) can be followed.
    if eeprom {
        if let Ok(img) = rpi_virt_fw::firmware::eeprom::EepromImage::parse(&flash) {
            println!("eeprom     {} sections", img.sections.len());
            print!("{}", img.summary());
            if let Some(conf) = img.bootconf() {
                for (g, k, v) in &conf.entries {
                    let g = if g.is_empty() { "all" } else { g.as_str() };
                    println!("           [{g}] {k}={v}");
                }
                if let Some(order) = conf.boot_order_names() {
                    println!("           BOOT_ORDER: {order}");
                }
            }
        }
    }

    let limits = RunLimits {
        max_steps,
        max_wall: Some(std::time::Duration::from_secs(max_wall_secs)),
        stop_pc: None,
        idle_spin_limit: 200_000,
    };

    let mut reboots = 0u32;
    #[allow(unused_mut)]
    let (report, mut emu, start) = 'boot: loop {
        let payload = if eeprom {
            Payload::from_eeprom_bytes(&flash)?
        } else {
            Payload::from_elf_bytes(&bytes)?
        };
        let mut machine = Machine::new(ram_mb as usize * 1024 * 1024);
        if eeprom {
            machine.spi0.attach_flash(flash.clone());
        }
        if let Some(img) = &sd_img {
            machine.emmc2.insert_card(img.clone());
        }
        machine.mmio_trace = trace_mmio;
        payload.load_into(&mut machine)?;
        for &(a, v) in &patches {
            use rpi_virt_fw::bus::Bus;
            machine.store32(a, v).ok();
            println!("patch [{a:#010x}] = {v:#010x}");
        }
        let start = entry.unwrap_or(payload.entry());

        let mut emu = Emulator::new(machine, start);
        emu.set_unimpl_policy(UnimplPolicy::Skip);
        emu.cpu.trace = trace;
        emu.cpu.exc_vbase = exc_vbase;
        emu.cpu.trace_cf_only = trace && !trace_full && trace_from == 0;
        emu.cpu.trace_cap = if trace_full || trace_from != 0 {
            200_000
        } else {
            4_000_000
        };
        emu.cpu.trace_from = trace_from;
        emu.core1_entry = core1_entry;
        if as_core1 {
            emu.cpu.core_id = 1;
        }
        if smp {
            emu.start_smp(start);
        }
        let report = emu.run(&limits);

        // `RVF_DUMP_FLASH=<path>` writes the (self-update-modified) EEPROM image
        // after every run segment — `<path>.<n>` — so a run that reaches
        // "BOOT-EEPROM: UPDATED" but stops before RESET still yields the burned
        // image. Feed it back as `recon <path>.<n> --eeprom` for a fast, already
        // provisioned boot (no self-update, no reboot).
        if eeprom {
            if let Ok(p) = std::env::var("RVF_DUMP_FLASH") {
                let cur = emu.machine.spi0.flash_bytes();
                if cur != flash.as_slice() {
                    let _ = std::fs::write(format!("{p}.{}", reboots + 1), cur);
                    eprintln!("wrote {p}.{} ({} bytes)", reboots + 1, cur.len());
                }
            }
        }

        if report.end == rpi_virt_fw::emulator::RunEnd::Reset {
            reboots += 1;
            print!("{}", String::from_utf8_lossy(&report.console));
            flash = emu.machine.spi0.flash_bytes().to_vec();
            unsign(&mut flash, false); // self-update restored SIGNED_BOOT=1
            if reboots <= 4 {
                println!("\n=== RESET (reboot {reboots}) — re-running from updated flash ===\n");
                continue 'boot;
            }
            println!("\n=== RESET (reboot {reboots}) — giving up after 4 reboots ===");
        }
        break 'boot (report, emu, start);
    };
    // Collapse consecutive-identical transfers so a spin doesn't hide the
    // history that led into it.
    let mut cf_tail: Vec<(u32, u32, u32)> = Vec::new();
    for &(f, t) in &emu.cpu.cf_trace {
        match cf_tail.last_mut() {
            Some((lf, lt, n)) if *lf == f && *lt == t => *n += 1,
            _ => cf_tail.push((f, t, 1)),
        }
    }
    let cf_tail: Vec<(u32, u32, u32)> = cf_tail.into_iter().rev().take(30).collect();

    println!("entry      {start:#010x}");
    println!("end        {:?}", report.end);
    println!("final pc   {:#010x}", report.pc);
    println!(
        "retired    {}  (skipped {}, cycles {})",
        report.retired, report.skipped, report.cycles
    );
    println!(
        "stub hits  {}   bus errors {}",
        report.stub_hits, report.bus_errors
    );
    println!("wall       {:?}", report.wall);
    if let Some(pc1) = report.core1_pc {
        println!(
            "core1      pc {:#010x}  retired {}  end {:?}",
            pc1,
            report.core1_retired.unwrap_or(0),
            report.core1_end
        );
    }
    print!("regs      ");
    for (i, r) in report.regs.iter().enumerate() {
        if i % 8 == 0 {
            print!("\n  r{i:<2}");
        }
        print!(" {r:08x}");
    }
    println!();

    if !report.console.is_empty() {
        println!("\n--- console ({} bytes) ---", report.console.len());
        if report.console_streamed {
            // Already echoed to stderr line by line while the run was going.
            println!("(streamed above; RVF_LIVE_CONSOLE=0 to buffer it here instead)");
        } else {
            println!("{}", String::from_utf8_lossy(&report.console));
        }
    }

    for &(a, n) in &dumps {
        use rpi_virt_fw::bus::Bus;
        print!("dump {a:#010x}:");
        for i in 0..n {
            if i % 32 == 0 {
                print!("\n  {:#010x} ", a + i);
            }
            print!("{:02x}", emu.machine.load(a + i, rpi_virt_fw::bus::Width::Byte).unwrap_or(0) as u8);
        }
        println!();
    }

    for &(a, count) in &disasms {
        use rpi_virt_fw::bus::Bus;
        println!("disasm {a:#010x}:");
        let mut pc = a;
        let mut buf = [0u8; 10];
        for _ in 0..count {
            for (i, b) in buf.iter_mut().enumerate() {
                *b = emu
                    .machine
                    .load(pc + i as u32, rpi_virt_fw::bus::Width::Byte)
                    .unwrap_or(0) as u8;
            }
            let len = insn_len_bytes(u16::from_le_bytes([buf[0], buf[1]])) as usize;
            let insn = decode(&buf[..len], pc);
            let hex: String = buf[..len].iter().map(|b| format!("{b:02x}")).collect();
            println!("  {pc:#010x}:  {hex:<20}  {:?}", insn.op);
            pc = pc.wrapping_add(len as u32);
        }
    }

    if !report.phase_tags.is_empty() {
        let tags: Vec<String> = report
            .phase_tags
            .iter()
            .map(|&v| {
                v.to_le_bytes()
                    .iter()
                    .map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' })
                    .collect()
            })
            .collect();
        println!("\n--- start4 boot-progress tags (0xcec02000) ---");
        println!("  {}", tags.join(" -> "));
    }

    if trace || !emu.cpu.trace_log.is_empty() {
        println!(
            "\n--- instruction trace ({} lines) ---",
            emu.cpu.trace_log.len()
        );
        for l in &emu.cpu.trace_log {
            println!("{l}");
        }
    }

    if let Some(c1) = &emu.cpu1 {
        if !c1.trace_log.is_empty() {
            println!("\n--- core 1 instruction trace ({} lines) ---", c1.trace_log.len());
            for l in &c1.trace_log {
                println!("{l}");
            }
        }
    }

    if !cf_tail.is_empty() {
        println!("\n--- last control transfers (newest first, repeats collapsed) ---");
        for (from, to, n) in &cf_tail {
            let tag = if *n > 1 {
                format!("  (x{n})")
            } else {
                String::new()
            };
            println!("  {from:#010x}  ->  {to:#010x}{tag}");
        }
    }

    {
        let log = &emu.machine.periph_stub.log;
        if !log.is_empty() {
            use std::collections::BTreeMap;
            let mut per: BTreeMap<u32, (u32, u32, u32)> = BTreeMap::new(); // off -> (reads, writes, last_val)
            for a in log {
                let e = per.entry(a.offset).or_insert((0, 0, 0));
                if a.write {
                    e.1 += 1;
                } else {
                    e.0 += 1;
                }
                e.2 = a.value;
            }
            println!(
                "\n--- peripheral-window stub: {} distinct offsets ({} accesses logged) ---",
                per.len(),
                log.len()
            );
            for (off, (r, w, v)) in &per {
                println!(
                    "  0x7e00_{:04x}  r={:<5} w={:<5} last={:#010x}",
                    off, r, w, v
                );
            }
        }
    }

    if !report.unimpl.is_empty() {
        println!(
            "\n--- distinct unimplemented instructions ({}, top 40 by hit count) ---",
            report.unimpl.len()
        );
        for h in report.unimpl.iter().take(40) {
            println!(
                "  {:>9}x  pc={:#010x}  {:>2}-bit  raw={:012x}  {:?}",
                h.count,
                h.pc,
                h.len * 8,
                h.raw,
                h.class
            );
        }
        println!(
            "\n(disassemble any of these with:  rpi-virt-fw disasm {} --vaddr <pc> --count 1)",
            path.display()
        );
    }

    Ok(ExitCode::SUCCESS)
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
    let mut vaddr: Option<u32> = None;
    let mut lengths_only = false;
    let mut eeprom = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--base" => base = parse_u32(it.next().context("--base needs a value")?)?,
            "--count" => count = it.next().context("--count needs a value")?.parse()?,
            "--vaddr" => vaddr = Some(parse_u32(it.next().context("--vaddr needs a value")?)?),
            "--lengths" => lengths_only = true,
            "--eeprom" => eeprom = true,
            s if !s.starts_with('-') => path = Some(PathBuf::from(s)),
            s => bail!("unexpected argument '{s}'"),
        }
    }
    let path = path.context("disasm: missing <file>")?;
    let raw = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;

    // ELF: locate the segment containing `vaddr` (or the entry) and disassemble
    // from there. Flat binary: disassemble from file offset 0 at `--base`.
    let (bytes, mut pc): (Vec<u8>, u32) = if eeprom {
        use rpi_virt_fw::firmware::eeprom::{EepromImage, BOOTCODE_ENTRY_OFFSET, BOOTCODE_LOAD_ADDR};
        let img = EepromImage::parse(&raw)?;
        let bc = img.bootcode().context("EEPROM image has no bootcode section")?;
        let target = vaddr.unwrap_or(BOOTCODE_LOAD_ADDR + BOOTCODE_ENTRY_OFFSET);
        let skip = (target - BOOTCODE_LOAD_ADDR) as usize;
        (bc.body[skip..].to_vec(), target)
    } else if raw.starts_with(b"\x7fELF") {
        let elf = rpi_virt_fw::firmware::elf32::Elf32::parse(&raw)?;
        let target = vaddr.unwrap_or(elf.entry);
        let seg = elf
            .segments
            .iter()
            .find(|s| target >= s.vaddr && (target as u64) < s.vaddr as u64 + s.data.len() as u64)
            .with_context(|| format!("no loadable segment contains vaddr {target:#x}"))?;
        let skip = (target - seg.vaddr) as usize;
        (seg.data[skip..].to_vec(), target)
    } else {
        (raw, vaddr.unwrap_or(base))
    };

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
        if lengths_only {
            println!("{pc:#010x} {len} {}", insn.op.mnemonic());
        } else {
            let hex: String = bytes[off..off + len]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            println!("{pc:#010x}:  {hex:<20}  {:?}", insn.op);
        }
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
