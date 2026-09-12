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
                             [--max-wall <secs>] [--sd <img>] [--usb <img>]
                             [--boot-order <hex>] [--bootconf <KEY=VALUE>]...
                             [--skip-signed-boot] [--netboot <dir>]
                             [--eeprom-pubkey <pubkey.bin>]
                             [--skip-unimpl]
              (no --max-steps = no instruction cap; --max-wall defaults to 140s)
              (an unknown instruction stops the run; --skip-unimpl steps over it
               instead, for reconnaissance on firmware the decoder is new to)
                             [--dump <hex>:<len>] [--disasm <hex>:<count>] [--patch <hex>=<hex>]
                             [--dump-fdt <path>] [--print-fdt] [--console-log <path>]
                             [--mbox-property <tag>[,<tag>...]] [--arm] [--until <text>]
                             [--send-after <prompt> <text>]... [--stdin]
    rpi-virt-fw boot-check <scenario.toml> --plan [--console <path>]
    rpi-virt-fw boot-check <scenario.toml> --log <path> --console <path> [--update]
    rpi-virt-fw disasm <file> [--base <hex>] [--count <n>] [--vaddr <hex>]
    rpi-virt-fw spec-docs [--update]

COMMANDS:
    run       Run one scenario and check it against its golden transcript.
    run-all   Run every *.toml scenario in <dir> (default: testdata/scenarios).
    recon     Load an ELF (or --eeprom image) and run it, reporting how far it
              got and what it touched. Stops on an instruction the decoder does
              not implement (--skip-unimpl steps over it instead).
    boot-check
              Check a finished firmware boot against a boot scenario: the
              golden console transcript plus every named milestone. `--plan`
              prints the `recon` invocation the scenario describes, which is
              how `scripts/boot-check.sh` runs the boot without repeating the
              workload description.
    disasm    Disassemble a flat binary / ELF with the (partial) VPU decoder.
    spec-docs Check docs/periph/ against the register specs in specs/*.toml;
              --update regenerates it.

FLAGS:
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
              not only the `/chosen` summary the run report gives by default.
    --mbox-property <tag>[,<tag>...]
              After the boot, post a property-interface request to the firmware
              the way a booted Linux would (`/dev/vcio`), and print what the
              still-running `start4.elf` answers. Tags are hex, e.g.
              `0x00000001` (GET_FIRMWARE_REVISION) or `0x00030092`
              (GET_CRYPTO_HMAC_SHA256). See docs/diagnostics.md.
    --arm     Model the ARM (#40): release the four Cortex-A72 cores when
              `arm_loader` writes the ARM control block, at PC 0 in EL3 like
              the SoC, and run them in lock-step with the VPU. The run ends if
              a core hits something not modelled yet.
    --until <text>
              End the run once the console prints <text> (e.g. the shell
              prompt of a Linux boot), after the last --send-after went in.
    --send-after <prompt> <text>
              Type <text> into the serial console (PL011) once it prints
              <prompt>. Repeatable; each prompt is looked for only in what the
              console printed after the previous send. Both take \\n, \\r, \\t,
              \\\\ and \\xHH escapes. Deterministic: keyed to the transcript.
    --stdin   Interactive session: the host's stdin is the serial console's
              input, and no wall-clock or silence limit ends the run. On a
              terminal, keys go to the guest raw (Ctrl-C included); Ctrl-A x
              quits, Ctrl-A Ctrl-A sends a Ctrl-A.
    --dram-map
              Report which DRAM pages are non-zero when the run ends, as
              address runs. Proof of concept for the QEMU hand-off: this is the
              state that would have to cross the line (docs/vision.md §3).
    -v        Print the full run report and transcript.
";

/// The offset of the `bootconf.txt` `MAGIC_FILE` section header in an EEPROM
/// image, found through the section walk rather than by searching for the name
/// — the bootcode carries a string table with the same names in it.
fn find_bootconf_header(flash: &[u8]) -> Option<usize> {
    let img = rpi_virt_fw::firmware::eeprom::EepromImage::parse(flash).ok()?;
    img.sections
        .iter()
        .find(|s| s.filename.as_deref() == Some("bootconf.txt"))
        .map(|s| s.header_offset)
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
    let Some(cmd) = args.first() else {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };

    match cmd.as_str() {
        "run" => cmd_run(&args[1..]),
        "run-all" => cmd_run_all(&args[1..]),
        "recon" => cmd_recon(&args[1..]),
        "boot-check" => cmd_boot_check(&args[1..]),
        "disasm" => cmd_disasm(&args[1..]),
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

fn cmd_recon(args: &[String]) -> Result<ExitCode> {
    let mut path: Option<PathBuf> = None;
    let mut entry: Option<u32> = None;
    let mut ram_mb: Option<u32> = None;
    // No instruction cap by default — a full boot retires well over a billion,
    // and the wall clock is the useful bound. `--max-steps` is for pinning a
    // run to an exact instruction count (bisecting, probes).
    let mut max_steps: Option<u64> = None;
    let mut max_wall_secs: u64 = 140;
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
    /// Blue socket A. Root port 1 is the USB2 port feeding the on-board VIA
    /// hub, so a SuperSpeed fixture goes on port 2 (`docs/usb-xhci.md` §5.2).
    const USB_ROOT_PORT: usize = 2;

    let mut sd_image: Option<PathBuf> = None;
    let mut console_log: Option<PathBuf> = None;
    let mut dump_fdt: Option<PathBuf> = None;
    let mut print_fdt = false;
    // One entry per `--mbox-property`, so several exchanges can be made
    // against the same booted firmware. A crypto tag that fails leaves an error
    // code behind that only the *next* request can ask for (`0x0003008e`).
    let mut mbox_tags: Vec<Vec<MboxTag>> = Vec::new();
    let mut usb_image: Option<PathBuf> = None;
    let mut netboot_root: Option<PathBuf> = None;
    let mut boot_order: Option<String> = None;
    let mut bootconf: Vec<String> = Vec::new();
    let mut eeprom_pubkey: Option<PathBuf> = None;
    let mut dram_map = false;
    let mut skip_signed_boot = false;
    let mut skip_unimpl = false;
    let mut arm = false;
    let mut until: Option<String> = None;
    let mut sends: Vec<(String, Vec<u8>)> = Vec::new();
    let mut stdin = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--entry" => entry = Some(parse_u32(it.next().context("--entry needs a value")?)?),
            "--ram-mb" => ram_mb = Some(it.next().context("--ram-mb needs a value")?.parse()?),
            "--max-steps" => {
                max_steps = Some(it.next().context("--max-steps needs a value")?.parse()?)
            }
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
                core1_entry = Some(parse_u32(
                    it.next().context("--core1-entry needs a value")?,
                )?)
            }
            "--trace-from" => {
                trace = true;
                trace_from = parse_u32(it.next().context("--trace-from needs a value")?)?
            }
            "--trace-mmio" => trace_mmio = true,
            "--arm" => arm = true,
            "--until" => until = Some(it.next().context("--until needs a text")?.to_string()),
            "--send-after" => {
                let prompt = it.next().context("--send-after needs <prompt> <text>")?;
                let text = it.next().context("--send-after needs <prompt> <text>")?;
                let prompt = String::from_utf8(harness::boot::unescape(prompt))
                    .context("--send-after: the prompt must be UTF-8")?;
                sends.push((prompt, harness::boot::unescape(text)));
            }
            "--stdin" => stdin = true,
            "--sd" => sd_image = Some(PathBuf::from(it.next().context("--sd needs a path")?)),
            "--console-log" => {
                console_log = Some(PathBuf::from(
                    it.next().context("--console-log needs a path")?,
                ))
            }
            "--usb" => usb_image = Some(PathBuf::from(it.next().context("--usb needs a path")?)),
            "--netboot" => {
                netboot_root = Some(PathBuf::from(
                    it.next().context("--netboot needs a directory")?,
                ))
            }
            "--boot-order" => {
                boot_order = Some(it.next().context("--boot-order needs a value")?.to_string())
            }
            "--eeprom-pubkey" => {
                eeprom_pubkey = Some(PathBuf::from(
                    it.next().context("--eeprom-pubkey needs a file")?,
                ))
            }
            "--bootconf" => {
                let kv = it.next().context("--bootconf needs KEY=VALUE")?;
                if !kv.contains('=') {
                    bail!("--bootconf: expected KEY=VALUE, got '{kv}'");
                }
                bootconf.push(kv.to_string())
            }
            "--skip-signed-boot" => skip_signed_boot = true,
            "--skip-unimpl" => skip_unimpl = true,
            "--dump" => {
                let spec = it.next().context("--dump needs <hexaddr>:<len>")?;
                let (a, n) = spec.split_once(':').context("--dump: expected addr:len")?;
                dumps.push((parse_u32(a)?, parse_u32(n)?));
            }
            "--disasm" => {
                let spec = it.next().context("--disasm needs <hexaddr>:<count>")?;
                let (a, n) = spec
                    .split_once(':')
                    .context("--disasm: expected addr:count")?;
                disasms.push((parse_u32(a)?, parse_u32(n)?));
            }
            "--print-fdt" => print_fdt = true,
            "--mbox-property" => {
                let list = it.next().context("--mbox-property needs a tag list")?;
                let mut group: Vec<MboxTag> = Vec::new();
                for t in list.split(',') {
                    // `<tag>[:<value-buffer bytes>][=<word>.<word>...]`.
                    // The size override exists because start4's idea of how
                    // much room a tag needs is not always its Linux client's
                    // `sizeof`; the request words exist because most crypto
                    // tags take a `key_id`, and those are **1-based** — asking
                    // for key 0 answers `KEY_NOT_FOUND` on a part whose only
                    // key is key 1.
                    let (head, req) = match t.split_once('=') {
                        Some((a, b)) => (
                            a,
                            b.split('.').map(parse_u32).collect::<Result<Vec<u32>>>()?,
                        ),
                        None => (t, Vec::new()),
                    };
                    let (tag, size) = match head.split_once(':') {
                        Some((a, b)) => (parse_u32(a)?, Some(parse_u32(b)?)),
                        None => (parse_u32(head)?, None),
                    };
                    group.push((tag, size, req));
                }
                mbox_tags.push(group);
            }
            "--dram-map" => dram_map = true,
            "--dump-fdt" => {
                dump_fdt = Some(PathBuf::from(it.next().context("--dump-fdt needs a path")?))
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
    // `--usb <img>`: a Bulk-Only Transport mass-storage device in blue socket
    // A, which is xHCI root port 2 — a SuperSpeed lane straight onto the root
    // hub, so no hub traversal is involved. See `docs/usb-xhci.md` §5.2 for the
    // socket map.
    let usb_img = match &usb_image {
        Some(p) => {
            let img =
                std::fs::read(p).with_context(|| format!("reading USB image {}", p.display()))?;
            println!("usb image  {} ({} blocks)", p.display(), img.len() / 512);
            Some(img)
        }
        None => None,
    };

    let sd_img = match &sd_image {
        Some(sd_path) => {
            let img = std::fs::read(sd_path)
                .with_context(|| format!("reading SD image {}", sd_path.display()))?;
            println!(
                "sd image   {} ({} blocks)",
                sd_path.display(),
                img.len() / 512
            );
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

    // `--boot-order <hex>`: append a `BOOT_ORDER=` line to the EEPROM's
    // `bootconf.txt`. The pinned image does not carry one, so the bootloader
    // falls back to its built-in `0xf4` — SD card, then restart — and never
    // tries the USB entry, which makes `--usb` unexercisable. The section is
    // the last one in the image and is followed by erased flash, so growing it
    // is a length-field bump and an append; nothing moves.
    //
    // `--bootconf KEY=VALUE` appends any other line the same way (e.g.
    // `HTTP_HOST` / `HTTP_PORT` / `HTTP_PATH` for HTTP boot). A later line
    // overrides an earlier one with the same key.
    let conf_lines: Vec<String> = boot_order
        .iter()
        .map(|o| format!("BOOT_ORDER={o}"))
        .chain(bootconf.iter().cloned())
        .collect();
    let set_boot_order = |flash: &mut Vec<u8>, announce: bool| {
        if conf_lines.is_empty() || !eeprom {
            return;
        }
        let Some(hdr) = find_bootconf_header(flash) else {
            if announce {
                eprintln!("boot-order: no bootconf.txt section in the EEPROM image");
            }
            return;
        };
        let len = u32::from_be_bytes([
            flash[hdr + 4],
            flash[hdr + 5],
            flash[hdr + 6],
            flash[hdr + 7],
        ]) as usize;
        let text: String = conf_lines.iter().map(|l| format!("{l}\n")).collect();
        let end = hdr + 8 + len;
        let new_len = len + text.len();
        flash.splice(end..end + text.len(), text.bytes());
        flash[hdr + 4..hdr + 8].copy_from_slice(&(new_len as u32).to_be_bytes());
        if announce {
            for l in &conf_lines {
                println!("bootconf: appended {l} @ {:#x}", end);
            }
        }
    };

    // `--eeprom-pubkey <pubkey.bin>`: put an RSA-2048 public key in the
    // EEPROM's `pubkey.bin` slot, as `rpi-eeprom-config --pubkey` does (n then
    // e, little-endian, 256 + 8 bytes). Signed images — an HTTP-booted
    // `boot.img` among them — are verified against it; the pinned image's slot
    // is all zeros, which verifies nothing.
    let pubkey = match &eeprom_pubkey {
        Some(p) => {
            let k = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
            if k.len() != 264 {
                bail!(
                    "{}: {} bytes, want 264 (RSA-2048 n + e)",
                    p.display(),
                    k.len()
                );
            }
            Some(k)
        }
        None => None,
    };
    let set_pubkey = |flash: &mut Vec<u8>, announce: bool| {
        let Some(k) = &pubkey else { return };
        match rpi_virt_fw::firmware::eeprom::replace_file(flash, "pubkey.bin", k) {
            Ok(()) if announce => println!("eeprom-pubkey: pubkey.bin replaced"),
            Ok(()) => {}
            Err(e) => eprintln!("eeprom-pubkey: {e:#}"),
        }
    };

    // `flash` may be rewritten by an EEPROM self-update; on a firmware-requested
    // reset we rebuild from the updated image and run again.
    let mut flash = bytes.clone();
    unsign(&mut flash, true);
    set_boot_order(&mut flash, true);
    set_pubkey(&mut flash, true);

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
        // An interactive session lasts as long as its user wants it to.
        max_wall: (!stdin).then(|| std::time::Duration::from_secs(max_wall_secs)),
        stop_pc: None,
        idle_spin_limit: 200_000,
        // Stop once the firmware has gone quiet for a minute of modelled time.
        // The model's worst legitimate gap is the kernel load, about thirteen
        // seconds, so this has plenty of headroom; when the boot wedges it
        // reports in seconds instead of running out the wall clock. A shell
        // waiting for its user is quiet too, though.
        silent_us: if stdin { 0 } else { 60_000_000 },
        until,
    };
    // Made once, outside the reboot loop: it owns the stdin reader and the
    // terminal's raw mode.
    let mut host_input = stdin.then(rpi_virt_fw::stdio::HostInput::stdin);

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
        if let Some(img) = &usb_img {
            machine.pcie.endpoint.attach(
                USB_ROOT_PORT,
                Box::new(rpi_virt_fw::periph::usb::MassStorage::new(img.clone())),
            );
        }
        // `--netboot <dir>`: plug the Ethernet cable into the built-in network
        // peer (`src/net/peer.rs`): DHCP, DNS, and `<dir>` over TFTP and HTTP.
        if let Some(dir) = &netboot_root {
            machine.attach_net(Box::new(rpi_virt_fw::net::BuiltinPeer::with_root(
                dir.clone(),
            )));
        }
        machine.mmio_trace = trace_mmio;
        // `RVF_TRACE_MMIO=<lo>-<hi>` (hex): trace peripheral accesses from the
        // first instruction, but only inside that address range. Tracing the
        // whole bus across a boot is unusable — both in volume and in the time
        // the formatting costs — when the question is about one block.
        if let Some((lo, hi)) = std::env::var("RVF_TRACE_MMIO")
            .ok()
            .and_then(|v| parse_addr_range(&v))
        {
            machine.mmio_trace = true;
            machine.mmio_trace_range = Some((lo, hi));
        }
        payload.load_into(&mut machine)?;
        for &(a, v) in &patches {
            use rpi_virt_fw::bus::Bus;
            machine.store32(a, v).ok();
            println!("patch [{a:#010x}] = {v:#010x}");
        }
        let start = entry.unwrap_or(payload.entry());

        let mut emu = Emulator::new(machine, start);
        // Faulting is the default: an instruction the decoder does not know
        // would otherwise be silently stepped over, and the firmware would
        // quietly not do whatever it was for. `--skip-unimpl` restores the old
        // behaviour for reconnaissance on firmware the decoder has not been
        // taught yet.
        emu.set_unimpl_policy(if skip_unimpl {
            UnimplPolicy::Skip
        } else {
            UnimplPolicy::ReconFault
        });
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
        emu.arm_enabled = arm;
        if as_core1 {
            emu.cpu.core_id = 1;
        }
        if smp {
            emu.start_smp(start);
        }
        emu.input.script = sends.iter().cloned().collect();
        emu.input.host = host_input.take();
        let report = emu.run(&limits);
        host_input = emu.input.host.take();

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
            set_boot_order(&mut flash, false);
            set_pubkey(&mut flash, false);
            if reboots <= 4 {
                println!("\n=== RESET (reboot {reboots}) — re-running from updated flash ===\n");
                continue 'boot;
            }
            println!("\n=== RESET (reboot {reboots}) — giving up after 4 reboots ===");
        }
        break 'boot (report, emu, start);
    };
    // The terminal back to cooked mode before the report.
    drop(host_input);
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
    let ic = &emu.cpu.icache;
    println!(
        "decode     cache hits {}  fills {}  stale {}",
        ic.hits, ic.fills, ic.stale
    );
    if let Some(pc1) = report.core1_pc {
        println!(
            "core1      pc {:#010x}  retired {}  end {:?}",
            pc1,
            report.core1_retired.unwrap_or(0),
            report.core1_end
        );
    }
    if let Some(a) = &emu.arm {
        println!("\n--- ARM cores (#40) ---");
        if let Some(h) = a.handoff {
            println!(
                "  armstub   kernel_entry32 {:#010x}  dtb_ptr32 {:#010x}",
                h.kernel, h.dtb
            );
        }
        match &a.bootargs {
            Some(Ok((old, new))) if old != new => println!(
                "  bootargs  \"{}\" prepended",
                new.strip_suffix(old.as_str()).unwrap_or(new).trim_end()
            ),
            Some(Err(e)) => println!("  bootargs  not patched: {e}"),
            _ => {}
        }
        println!(
            "  ran       {} cycles, {} of them with every core asleep",
            a.cycles, a.slept
        );
        for (i, c) in a.cores.iter().enumerate() {
            match c.entered {
                Some((cycles, el, pc, x0)) => println!(
                    "  core {i}    left the armstub at cycle {cycles} for {pc:#x} in EL{el}, x0 = {x0:#x}"
                ),
                None => println!("  core {i}    still in the armstub"),
            }
            println!(
                "            {} instructions, {} exceptions, {} interrupts; now pc {:#x}  EL{}  sp {:#x}{}",
                c.insns,
                c.exceptions,
                c.interrupts,
                c.cpu.pc,
                c.cpu.el,
                c.cpu.sp(),
                if c.waiting { "  (wfi)" } else { "" }
            );
        }
        if let Some(stop) = &a.stopped {
            println!("  stopped   {stop:x?}");
        }
    } else if arm {
        println!("\n--- ARM cores (#40) ---\n  never released");
    }
    if report.core1_release_never_resolved {
        println!(
            "core1      RELEASED BUT NEVER SPAWNED — the ThreadX-SMP dispatch \
             global at gp+3672 stayed zero.\n\
             \x20          Most likely this firmware's .sdata layout differs \
             from the one that offset was read from (#25)."
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

    // `--console-log <path>`: the UART bytes on their own, with none of the
    // run report interleaved. `boot-check` normalises this into the golden
    // transcript; picking the console out of the combined log afterwards would
    // be guesswork, since both streams land in the same file.
    //
    // On a run that rebooted (EEPROM self-update) this is the last segment
    // only, which is the one the assertions are about.
    if let Some(p) = &console_log {
        std::fs::write(p, &report.console)
            .with_context(|| format!("writing console log {}", p.display()))?;
        println!(
            "console log {} ({} bytes)",
            p.display(),
            report.console.len()
        );
    }

    if let Some(net) = emu.machine.net.as_mut() {
        let st = emu.machine.genet.stats;
        println!("\n--- network (GENET <-> {}) ---", net.name());
        println!(
            "  tx {} (dropped {})  rx {} (filtered {}, dropped {})",
            st.tx, st.tx_dropped, st.rx, st.rx_filtered, st.rx_dropped
        );
        for line in net.take_log() {
            println!("  {line}");
        }
    }

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
            print!(
                "{:02x}",
                emu.machine
                    .load(a + i, rpi_virt_fw::bus::Width::Byte)
                    .unwrap_or(0) as u8
            );
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
                    .map(|&c| {
                        if (0x20..0x7f).contains(&c) {
                            c as char
                        } else {
                            '.'
                        }
                    })
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
            println!(
                "\n--- core 1 instruction trace ({} lines) ---",
                c1.trace_log.len()
            );
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

    if dram_map {
        // Proof of concept for the QEMU hand-off (docs/vision.md section 3):
        // how much of DRAM is actually dirty when `arm_loader` releases the
        // ARM, and where. Everything QEMU would have to be told about has to
        // come out of here, so the size and the shape of it decide whether a
        // hand-off is a file copy or a subsystem.
        //
        // "Dirty" is approximated as "not all zero", which is exact for this
        // purpose: the model starts RAM zeroed and QEMU's guest RAM is zeroed
        // too, so a zero page needs no transfer either way.
        const PAGE: usize = 4096;
        let ram = &emu.machine.ram;
        let base = ram.base();
        let mut runs: Vec<(u32, u32)> = Vec::new();
        let mut nonzero_pages = 0usize;
        let total_pages = ram.len() / PAGE;
        for p in 0..total_pages {
            let addr = base + (p * PAGE) as u32;
            let dirty = ram
                .read_slice(addr, PAGE)
                .map(|s| s.iter().any(|&b| b != 0))
                .unwrap_or(false);
            if !dirty {
                continue;
            }
            nonzero_pages += 1;
            match runs.last_mut() {
                // Bridge gaps of up to 64 KiB so the report is readable; the
                // bytes in the gap are zero and are counted separately.
                Some(last) if addr <= last.1 + 0x1_0000 => last.1 = addr + PAGE as u32,
                _ => runs.push((addr, addr + PAGE as u32)),
            }
        }
        println!("\n--- DRAM occupancy at the ARM hand-off ---");
        println!(
            "  {} MiB of RAM, {} of {} 4K pages non-zero ({} MiB), {} regions",
            ram.len() >> 20,
            nonzero_pages,
            total_pages,
            (nonzero_pages * PAGE) >> 20,
            runs.len()
        );
        for (lo, hi) in &runs {
            println!("  {lo:#010x}..{hi:#010x}  {:>8} KiB", (hi - lo) / 1024);
        }
    }

    {
        // The DRAM refresh interval start4 rescales from the LPDDR4 MR4 code
        // once the ARM is running. The firmware logs the change as
        // `sdram: sdram refresh 1562->3124 (2)`, but by then it has handed the
        // UART to Linux and only its internal message ring sees that line, so
        // the controller state is the console-independent way to check it.
        let sdc = &emu.machine.sdc;
        let history = sdc.refresh_history();
        if !history.is_empty() {
            let steps: Vec<String> = history.iter().map(|v| v.to_string()).collect();
            println!("\n--- sdram controller (0x7e00_1000) ---");
            println!(
                "  refresh interval {}  ({} mode-register reads)",
                steps.join(" -> "),
                sdc.mode_register_reads()
            );
        }
    }

    for group in &mbox_tags {
        mbox_property_exchange(&mut emu, &limits, group)?;
    }

    {
        // The device tree `arm_loader` leaves behind for the ARM, and the
        // `/chosen` identity properties it patched into it. This is the point
        // of the bench (rpi-mkosi#37): `rpi-machine-id` feeds the root LUKS
        // passphrase, so a firmware bump that changes how it is derived has to
        // be caught here rather than on a thousand deployed cards.
        //
        // Nothing hands the blob's address over in a register we can read — no
        // ARM core runs on this bench — so it is taken from the firmware's own
        // `Device tree loaded to 0x%x (size 0x%x)` line, which is the last word
        // start4 says about the blob before it releases the ARM. The header is
        // validated before anything is believed or written out.
        match locate_fdt(&mut emu.machine, &report.console) {
            Some((addr, blob)) => match rpi_virt_fw::fdt::Fdt::parse(&blob) {
                Ok(fdt) => {
                    let h = fdt.header();
                    println!("\n--- device tree handed to the ARM ---");
                    println!(
                        "  at {addr:#010x}  totalsize {:#x}  version {}",
                        h.totalsize, h.version
                    );
                    let nodes = fdt.nodes();
                    println!(
                        "  {} nodes, {} properties",
                        nodes.len(),
                        nodes.iter().map(|n| n.2.len()).sum::<usize>()
                    );
                    // `/chosen` is what the regression pins today, so it is
                    // always in the report. It is not special otherwise — the
                    // subject is the whole tree, and a firmware bump may move
                    // what it publishes into a node that does not exist yet,
                    // which is what `--print-fdt` and `--dump-fdt` are for.
                    match fdt.properties_of("/chosen") {
                        Some(props) => {
                            for p in &props {
                                // `bootargs` is the kernel command line and can
                                // be long; everything else in /chosen is short.
                                println!("  /chosen/{:<22} {}", p.name, p.display());
                            }
                        }
                        None => println!("  (no /chosen node)"),
                    }
                    if !print_fdt {
                        println!("  (--print-fdt for every node, --dump-fdt <path> for the blob)");
                    }
                    report_machine_id_derivation(&emu.machine, &fdt);
                    if print_fdt {
                        println!("\n{}", fdt.to_dts());
                    }
                    if let Some(out) = &dump_fdt {
                        std::fs::write(out, fdt.bytes())
                            .with_context(|| format!("writing {}", out.display()))?;
                        println!("  wrote {} ({} bytes)", out.display(), fdt.bytes().len());
                    }
                }
                Err(e) => {
                    println!("\n--- device tree handed to the ARM ---\n  at {addr:#010x}: {e}")
                }
            },
            None => {
                if dump_fdt.is_some() {
                    bail!(
                        "--dump-fdt: the boot never printed 'Device tree loaded to ...', \
                         so there is no device tree to dump"
                    );
                }
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
                "  {:>9}x  pc={:#010x}  {:>2}-bit  raw={:020x}  {:?}",
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

/// `boot-check <scenario.toml> ...` — the firmware-boot regression.
///
/// Two modes, because the boot itself is expensive (minutes) and must be run
/// exactly once per check:
///
/// * `--plan` prints the `recon` invocation the scenario describes, for
///   `scripts/boot-check.sh` to run. The scenario file stays the only place
///   the workload is written down.
/// * `--log <combined.log> --console <console.bin>` checks that finished run:
///   the console against the golden transcript, the log against the
///   milestones. `--update` rewrites the golden instead of failing on it.
fn cmd_boot_check(args: &[String]) -> Result<ExitCode> {
    let mut path: Option<PathBuf> = None;
    let mut log: Option<PathBuf> = None;
    let mut console: Option<PathBuf> = None;
    let mut plan = false;
    let mut update = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--plan" => plan = true,
            "--update" => update = true,
            "--log" => log = Some(PathBuf::from(it.next().context("--log needs a path")?)),
            "--console" => {
                console = Some(PathBuf::from(it.next().context("--console needs a path")?))
            }
            s if !s.starts_with('-') => path = Some(PathBuf::from(s)),
            s => bail!("unexpected argument '{s}'"),
        }
    }
    let path = path.context("boot-check: missing <scenario.toml>")?;
    let scn = harness::BootScenario::load(&path)?;

    if plan {
        // Shell-readable and quoting-proof: `wall=<n>` on the first line for
        // the outer timeout, then one `recon` argument per line.
        let console = console.unwrap_or_else(|| PathBuf::from("boot-console.bin"));
        println!("wall={}", scn.wall_secs());
        for a in scn.recon_args(&console) {
            println!("{a}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let log_path = log.context("boot-check: --log <path> (or --plan)")?;
    let log_text = std::fs::read_to_string(&log_path)
        .with_context(|| format!("reading run log {}", log_path.display()))?;
    let console_path = console.context("boot-check: --console <path> is required with --log")?;
    let console_bytes = std::fs::read(&console_path).with_context(|| {
        format!(
            "reading console log {} (recon writes it with --console-log)",
            console_path.display()
        )
    })?;
    let transcript = harness::boot::normalise_console(&console_bytes);

    if update {
        // Never record a bad run as the new truth. A boot that was starved of
        // CPU stops at the wall clock part-way through, and its transcript
        // looks like a perfectly good — and much shorter — boot.
        let milestones = harness::boot::check_milestones(&scn, &log_text);
        if !milestones.is_empty() {
            for f in &milestones {
                eprintln!("{f}");
            }
            eprintln!(
                "refusing to update the golden: this run failed {} milestone(s), so it is \
                 not a baseline. Fix the run (or raise RVF_BOOT_WALL if it was starved) first.",
                milestones.len()
            );
            return Ok(ExitCode::FAILURE);
        }
        harness::boot::write_golden(&scn, &transcript)?;
        println!(
            "updated golden {} ({} lines)",
            scn.golden_path().display(),
            transcript.lines().count()
        );
    }

    let failures = harness::boot::check_run(&scn, &log_text, &transcript)?;
    println!(
        "\n{}: {} milestone(s) + golden transcript ({} lines)",
        scn.name,
        scn.milestones.len(),
        transcript.lines().count()
    );
    if failures.is_empty() {
        println!("boot check passed");
        return Ok(ExitCode::SUCCESS);
    }
    for f in &failures {
        eprintln!("{f}");
    }
    eprintln!("boot check FAILED ({} problem(s))", failures.len());
    Ok(ExitCode::FAILURE)
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
        use rpi_virt_fw::firmware::eeprom::{
            EepromImage, BOOTCODE_ENTRY_OFFSET, BOOTCODE_LOAD_ADDR,
        };
        let img = EepromImage::parse(&raw)?;
        let bc = img
            .bootcode()
            .context("EEPROM image has no bootcode section")?;
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

/// Find the device tree blob `arm_loader` left for the ARM, using the
/// firmware's own `Device tree loaded to 0x<addr> (size 0x<len>)` console line
/// as the pointer.
///
/// Reading it out of the log rather than hard-coding an address is what keeps
/// this working across firmware versions — which is the entire point, since the
/// bench exists to diff one version against another. The line is emitted after
/// the overlays are merged and `/chosen` is patched, and nothing overwrites the
/// blob afterwards: the ARM that would consume it is not modelled.
///
/// The length in the log is the tree's own `totalsize`, but the header is read
/// first and trusted over it, so a firmware that logs a rounded figure still
/// yields an exact blob. Returns the address and the bytes.
fn locate_fdt(machine: &mut Machine, console: &[u8]) -> Option<(u32, Vec<u8>)> {
    use rpi_virt_fw::bus::{Bus, Width};

    let text = String::from_utf8_lossy(console);
    // Last one wins: a `tryboot` retry would load the tree more than once.
    let tail = text.rsplit_once("Device tree loaded to 0x")?.1;
    let (addr_hex, rest) = tail.split_once(" (size 0x")?;
    let addr = u32::from_str_radix(addr_hex.trim(), 16).ok()?;
    let logged_len = rest
        .split_once(')')
        .and_then(|(l, _)| u32::from_str_radix(l.trim(), 16).ok())
        .unwrap_or(0);

    let byte = |m: &mut Machine, a: u32| m.load(a, Width::Byte).unwrap_or(0) as u8;
    let read = |m: &mut Machine, a: u32, n: u32| -> Vec<u8> {
        (0..n).map(|i| byte(m, a.wrapping_add(i))).collect()
    };
    let head = read(machine, addr, 8);
    let totalsize = u32::from_be_bytes([head[4], head[5], head[6], head[7]]);
    // Believe the header only if it is plausible; otherwise fall back to the
    // logged length so `Fdt::parse` can report what is actually there.
    let len = if u32::from_be_bytes([head[0], head[1], head[2], head[3]])
        == rpi_virt_fw::fdt::FDT_MAGIC
        && (40..=8 << 20).contains(&totalsize)
    {
        totalsize
    } else {
        logged_len.max(40)
    };
    Some((addr, read(machine, addr, len)))
}

/// Recompute `/chosen/rpi-machine-id` from the modelled OTP and say whether the
/// firmware's own value still matches.
///
/// This is the one thing in the report that is a *prediction* rather than an
/// observation. `scripts/boot-check.sh` pins the published string, which catches
/// a firmware bump that moves the root-LUKS passphrase — but only after the fact
/// and only for this board's fuses. The derivation is documented in
/// `src/identity.rs`; recomputing it here turns "the value changed" into "the
/// algorithm changed", which is the distinction rpi-mkosi#37 actually needs.
///
/// A mismatch is not by itself a bug in the model: it means the EEPROM
/// bootloader no longer derives the identity the way `src/identity.rs` says, and
/// that is exactly the event worth failing on.
fn report_machine_id_derivation(machine: &Machine, fdt: &rpi_virt_fw::fdt::Fdt) {
    use rpi_virt_fw::identity::{expected_machine_id_hex, MACHINE_ID_ROWS};

    let published = fdt
        .properties_of("/chosen")
        .and_then(|props| {
            props
                .iter()
                .find(|p| p.name == "rpi-machine-id")
                .and_then(|p| p.as_str())
        })
        .map(|s| s.trim().to_string());
    let Some(published) = published else {
        return;
    };

    let mut rows = [0u32; 5];
    for (slot, key) in rows.iter_mut().zip(MACHINE_ID_ROWS) {
        *slot = machine.config_otp.row(key);
    }
    let expected = expected_machine_id_hex(&rows);
    let inputs: Vec<String> = MACHINE_ID_ROWS
        .iter()
        .zip(rows)
        .map(|(k, v)| format!("otp[{k}]={v:#010x}"))
        .collect();

    println!("\n--- rpi-machine-id derivation (#22) ---");
    println!("  SHA-256({})[..16]", inputs.join(" | "));
    if expected == published {
        println!("  {expected}  (same as published)");
    } else {
        println!("  predicted {expected}");
        println!("  published {published}");
    }
}

/// Parse `<lo>-<hi>` (hex, `0x` optional) into a half-open address range.
/// Anything else — including the bare `1` that arms the trace from a
/// `RVF_TRACE_ON_*` trigger — yields `None`.
fn parse_addr_range(s: &str) -> Option<(u32, u32)> {
    let (lo, hi) = s.trim().split_once('-')?;
    let p = |t: &str| u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok();
    let (lo, hi) = (p(lo)?, p(hi)?);
    (lo < hi).then_some((lo, hi))
}

/// Where the request buffer is built. Well clear of everything `--dram-map`
/// reports dirty at `arm_loader` — the kernel ends below `0x0280_0000`, the
/// device tree sits at `0x2eff_1e00`, and start4's own image is above
/// `0x3ebe_4000`.
const MBOX_BUFFER: u32 = 0x1000_0000;

/// Post a property-interface request to the still-running firmware, the way a
/// booted Linux does through `/dev/vcio`, and report what comes back.
///
/// The ARM is not modelled, so this stands in for it: build the buffer, ring
/// the doorbell, keep stepping the VPU, and read the reply. The address on the
/// wire is `0xC000_0000 | phys` because Linux allocates the buffer coherently
/// and `/soc` carries `dma-ranges = <0xc0000000 0x0 0x0 0x40000000>` — the
/// uncached alias, which the model already maps to the same DRAM.
/// One tag in a `--mbox-property` request: the tag, an optional override of the
/// value-buffer size, and optional request words (a `key_id`, most often).
type MboxTag = (u32, Option<u32>, Vec<u32>);

fn mbox_property_exchange(emu: &mut Emulator, limits: &RunLimits, tags: &[MboxTag]) -> Result<()> {
    use rpi_virt_fw::bus::{Bus, Width};

    println!("\n--- ARM property mailbox (0x7e00_b880) ---");

    // Each tag names its own value-buffer size, and the firmware walks the
    // request by those sizes — so one wrong size desynchronises every tag after
    // it and the whole buffer comes back `0x80000001` (parse error). A fixed
    // 64-byte slot for everything did exactly that.
    //
    // Sizes and request payloads follow raspberrypi/utils `rpifwcrypto.c`,
    // which is the Linux-side client of the same interface. The service itself
    // lives in `start4.elf` (`arm_crypto_*`, with its own mbedTLS) — this is
    // only the caller, standing in for an ARM the bench does not have.
    let spec = |tag: u32| -> (u32, Vec<u32>) {
        match tag {
            // `flags, key_id` in; `status, length, key[]` back. The buffer has
            // to hold the key, so it is sized by the client's maxima:
            // 512 bytes of public key, 1024 of private key.
            0x0003_0093 => (8 + 512, vec![0, 0]),
            0x0003_0094 => (8 + 1024, vec![0, 0]),
            // `flags, key_id` in, nothing back.
            0x0003_0095 => (8, vec![0, 0]),
            // `key_id, status` / `key_id, usage` in.
            0x0003_8090 | 0x0003_809c => (8, vec![0, 0]),
            // `key_id` in, one word back.
            0x0003_0090 | 0x0003_009c => (4, vec![0]),
            // `flags, key_id, length, hash[32]` in; `status, length, sig[]`
            // back, so the buffer has to be the larger of the two.
            0x0003_0091 => (128, vec![0, 0, 32]),
            // `flags, key_id, length, message[]` in; `status, length,
            // hmac[32]` back. A fixed short message keeps the result stable
            // across runs, which is what makes it a regression.
            0x0003_0092 => {
                let mut v = vec![0, 0, 16];
                v.extend_from_slice(&[0x6c6c6548, 0x77202c6f, 0x646c726f, 0x00000021]);
                (128, v)
            }
            // Everything else: one word in, one word back.
            _ => (4, vec![0]),
        }
    };

    let mut words: Vec<u32> = vec![0, 0];
    for (tag, override_size, override_req) in tags {
        let (tag, override_size) = (*tag, *override_size);
        let (size, payload) = spec(tag);
        let size = override_size.unwrap_or(size);
        let payload = if override_req.is_empty() {
            payload
        } else {
            override_req.clone()
        };
        words.push(tag);
        words.push(size);
        words.push(0);
        let slot = (size / 4) as usize;
        for i in 0..slot {
            words.push(payload.get(i).copied().unwrap_or(0));
        }
    }
    // End marker, then slack. The firmware rejects a buffer whose declared
    // total ends exactly at the marker: the last tag comes back unhandled and
    // the whole buffer gets `0x80000001`, whichever tag is last. `rpifwcrypto.c`
    // never hits this because it declares `sizeof(msg)` — its value arrays are
    // bigger than the `tag_buf_size` it asks for, so its total always carries
    // spare room past the marker.
    words.push(0);
    words.extend_from_slice(&[0; 4]);
    words[0] = (words.len() as u32) * 4;

    for (i, w) in words.iter().enumerate() {
        emu.machine
            .store(MBOX_BUFFER + (i as u32) * 4, Width::Word, *w)
            .map_err(|e| anyhow::anyhow!("staging the request buffer: {e}"))?;
    }

    let bus_addr = 0xC000_0000 | MBOX_BUFFER;
    let message = (bus_addr & !0xF) | rpi_virt_fw::periph::mbox::CHANNEL_PROPERTY;
    println!(
        "  posting {message:#010x}  ({} tags, {} byte buffer at {MBOX_BUFFER:#010x})",
        tags.len(),
        words.len() * 4
    );
    if !emu.machine.mbox.post_from_arm(message) {
        bail!("the mailbox is full — the firmware has not drained earlier requests");
    }

    // Let the firmware run. It is parked in the ThreadX idle loop by now, so a
    // short budget is plenty if it is going to answer at all.
    // The firmware is parked in the ThreadX idle loop by now, so the two stop
    // conditions that end a *boot* would end this instantly and wrongly: the
    // idle-spin detector fires on the idle loop itself, and the silence
    // watchdog fires because a serviced mailbox request prints nothing.
    //
    // Nothing in `RunLimits` can say "stop when the reply lands", so run in
    // short slices and check between them. The answer takes a few million
    // instructions once the interrupt gets through; the budget is there for
    // the case where it does not.
    // Short slices, because the check between them is also what dates the
    // reply: the firmware idles through `sleep`, so half a second of wall
    // clock is ten of modelled time — ten times what a Linux client waits.
    let slice = RunLimits {
        max_steps: None,
        max_wall: Some(std::time::Duration::from_millis(10)),
        idle_spin_limit: 0,
        silent_us: u64::MAX,
        ..limits.clone()
    };
    let budget = std::time::Duration::from_secs(20);
    let started = std::time::Instant::now();
    let retired_before = emu.cpu.retired;
    let us_before = emu.machine.systimer.now_us();
    let replies_before = emu.machine.mbox.writes;
    let mut console = Vec::new();
    let mut report = emu.run(&slice);
    loop {
        console.extend_from_slice(&report.console);
        let answered =
            !emu.machine.mbox.request_outstanding() && emu.machine.mbox.writes > replies_before;
        if answered || started.elapsed() >= budget {
            break;
        }
        report = emu.run(&slice);
    }
    // Modelled time is what a real client's timeout counts (Linux's
    // `raspberrypi-firmware` gives up after one second); the wall clock only
    // says how long the interpreter took.
    println!(
        "  resumed: {} instructions, {} us modelled, over {:.1?}, ended {:?} at {:#010x}",
        report.retired.saturating_sub(retired_before),
        emu.machine.systimer.now_us().saturating_sub(us_before),
        started.elapsed(),
        report.end,
        report.pc
    );
    println!(
        "  mailbox: config1 {:#x}, {} requests taken, {} replies written",
        emu.machine.mbox.interrupt_armed(),
        emu.machine.mbox.reads,
        emu.machine.mbox.writes
    );

    match emu.machine.mbox.take_reply() {
        Some(reply) => println!("  reply {reply:#010x}"),
        None if emu.machine.mbox.request_outstanding() => {
            println!("  no reply: the firmware never read the request off MAIL1");
            println!("  (the `mbox_read` task at 0x3ed1d724 waits on its driver's receive");
            println!("   lock, so this means the wake never arrived: check that the config");
            println!("   word above carries the pending bit 4 that 0x3ec58302 releases on)");
            return Ok(());
        }
        None => println!("  the request was read, but no reply was written to MAIL0"),
    }

    let code = emu.machine.load(MBOX_BUFFER + 4, Width::Word).unwrap_or(0);
    println!(
        "  response code {code:#010x} ({})",
        match code {
            0x8000_0000 => "success",
            0x8000_0001 => "parse error",
            _ => "not a response",
        }
    );
    let total = emu.machine.load(MBOX_BUFFER, Width::Word).unwrap_or(0);
    if code != 0x8000_0000 {
        // The tag walk below trusts the sizes it staged. When the firmware
        // disagrees about them that walk is exactly what cannot be trusted, so
        // print the buffer as the firmware left it and decode by hand.
        println!("  raw reply buffer ({total} bytes by its own header):");
        let n = (total.min(1024) / 4).max(4);
        for row in 0..n.div_ceil(4) {
            let mut line = format!("  {:#010x} ", MBOX_BUFFER + row * 16);
            for col in 0..4 {
                let i = row * 4 + col;
                if i < n {
                    let w = emu
                        .machine
                        .load(MBOX_BUFFER + i * 4, Width::Word)
                        .unwrap_or(0);
                    line.push_str(&format!(" {w:08x}"));
                }
            }
            println!("{line}");
        }
    }
    let mut off = 8;
    while off + 12 <= total.min(4096) {
        let tag = emu
            .machine
            .load(MBOX_BUFFER + off, Width::Word)
            .unwrap_or(0);
        if tag == 0 {
            break;
        }
        // Bit 31 of the third word is the firmware's "I handled this" mark. A
        // tag it does not know is left exactly as it was staged, so the word
        // reads back 0 — which is how an unknown tag is told apart from a
        // handler that answered with nothing.
        let resp = emu
            .machine
            .load(MBOX_BUFFER + off + 8, Width::Word)
            .unwrap_or(0);
        let len = resp & 0x7FFF_FFFF;
        let mut vals = Vec::new();
        // Enough for the longest answer worth reading inline: a
        // 32-byte HMAC plus its status and length words.
        for i in 0..(len / 4).min(16) {
            vals.push(format!(
                "{:#010x}",
                emu.machine
                    .load(MBOX_BUFFER + off + 12 + i * 4, Width::Word)
                    .unwrap_or(0)
            ));
        }
        let mark = if resp & 0x8000_0000 != 0 {
            "answered"
        } else {
            "not handled"
        };
        println!(
            "  tag {tag:#010x}  {mark:>11}  {len:>3} bytes  {}",
            vals.join(" ")
        );
        let slot = emu
            .machine
            .load(MBOX_BUFFER + off + 4, Width::Word)
            .unwrap_or(0);
        off += 12 + ((slot.max(len) + 3) & !3);
    }
    // A trace armed by `RVF_TRACE_ON_PC` inside the exchange is collected here,
    // after the recon report that normally prints one has already run — so
    // print it, or investigating a tag handler silently produces nothing.
    if !emu.cpu.trace_log.is_empty() {
        println!(
            "\n--- instruction trace while servicing the request ({} entries) ---",
            emu.cpu.trace_log.len()
        );
        for l in &emu.cpu.trace_log {
            println!("{l}");
        }
    }
    if !console.is_empty() {
        // Anything the firmware printed while servicing the request.
        let tail = String::from_utf8_lossy(&console);
        for line in tail.lines().filter(|l| !l.is_empty()) {
            println!("  console: {line}");
        }
    }
    Ok(())
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
