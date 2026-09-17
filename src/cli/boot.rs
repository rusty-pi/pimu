//! `boot`: boot the machine from an EEPROM image, as a Pi 4 does, or run a
//! VPU ELF; then report on the run.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use anyhow::{bail, Context, Result};

use rpi_virt_fw::armstub::Handoff;
use rpi_virt_fw::emulator::{Emulator, RunLimits, RunReport};
use rpi_virt_fw::firmware::Payload;
use rpi_virt_fw::harness;
use rpi_virt_fw::log::{Log, Spec};
use rpi_virt_fw::machine::Machine;
use rpi_virt_fw::soc::{Board, Stepping};
use rpi_virt_fw::vpu::decode::decode;
use rpi_virt_fw::vpu::length::insn_len_bytes;
use rpi_virt_fw::vpu::UnimplPolicy;

use crate::mbox::{mbox_property_exchange, MboxTag};
use crate::otp::OtpFile;
use crate::parse_u32;

/// Blue socket A. Root port 1 is the USB2 port feeding the on-board VIA
/// hub, so a SuperSpeed fixture goes on port 2 (`docs/usb-xhci.md` §5.2).
const USB_ROOT_PORT: usize = 2;

/// `boot --help` (#99). Every option `BootOpts::parse` takes is here, one to a
/// line, bar the no-op `--arm`; a test holds the two to that.
const HELP: &str = "\
rpi-virt-fw boot — boot the machine from an EEPROM image, as a Pi 4 does, or
run a VPU ELF

USAGE:
    rpi-virt-fw boot --eeprom <pieeprom.bin> [<options>]
    rpi-virt-fw boot <file.elf> [<options>]

    `boot <file> --eeprom` is the same as `boot --eeprom <file>`, and with no
    command the options are `boot`'s: `rpi-virt-fw --eeprom <file> ...`.
    `--config <file>` and `--option=value` work as for every command (see
    `rpi-virt-fw --help`).

    The run prints the serial console (not with -q), what was asked for by
    name (`--dump`, `--print-fdt`, `--mbox-property`, …) and one
    `result: ok|FAILED — <why>` line; the exit status is 1 on failure.
    Besides the limits below, a run ends at an instruction the decoder does
    not implement, and once the firmware has printed nothing for a minute of
    model time.

MACHINE:
    --eeprom <pieeprom.bin>
              Put this image on the SPI flash and boot from it, through the
              modelled boot ROM. Without it the file is a VPU ELF, loaded and
              run from its entry point.
    --stepping b0|c0
              The BCM2711 silicon, C0 by default.
    --board-rev <hex>
              The OTP revision code; by default a board that stepping shipped
              on. Its memory field is the board's memory: the RAM behind the
              bus, and the LPDDR4 parts the DRAM controller reports, so the
              firmware trains and publishes that size. `total_mem` in
              `config.txt` cuts it down from there.
    --otp json:<file> | binary:<file>
              The OTP fuses, kept across runs: read before the boot when
              <file> exists, written back after the run when the firmware
              programmed a row, created from the model's own fuses when it
              does not exist. json: row -> value, one a line; binary: row n
              at byte 4n, little-endian. A file with a real board's fuses
              holds its secrets: keep it out of the repository.
    --boot-rom <rom.bin>
              Execute a real VPU maskROM dump from its reset vector,
              0x60000000, instead of the modelled boot ROM stage.
              Experimental. The dump stays a local file: never commit it.

MEDIA AND NETWORK:
    --sd <img>
              An SD card with this image. Read on demand; writes stay in
              memory, and the boot after a firmware reset starts from the file
              again.
    --usb <img>
              A USB mass-storage stick with this image, in blue socket A (xHCI
              root port 2, SuperSpeed). Read on demand; writes stay in memory
              and outlive a firmware reset.
    --usb-mb <n>
              The stick is <n> MiB, with the image at its start, as on a Pi
              whose first boot uses the rest.
    --netboot <dir>
              Plug the Ethernet cable into the built-in network peer: DHCP,
              DNS, and <dir> over TFTP and HTTP.
    --net passt[:<socket>]
              Plug the Ethernet cable into the host's network instead of the
              built-in peer, through passt: `passt` starts one (from PATH) on a
              socket pair, `passt:<socket>` connects to one already listening
              (`passt -f -s <socket>`), or to anything else speaking QEMU's
              `-netdev stream` framing on that UNIX socket. Runs on the host's
              clock, so not deterministic (#45).

EEPROM IMAGE (edits made before the first boot, and again after every
self-update, which brings back the image's own):
    --boot-order <hex>
              Add a BOOT_ORDER=<hex> line to bootconf.txt. Without one the
              bootloader uses its built-in 0xf4: SD card, then restart, and
              never USB.
    --bootconf <KEY=VALUE>
              Add any other line to bootconf.txt, e.g. HTTP_HOST=<host> for
              HTTP boot. Repeatable; a later line wins over an earlier one with
              the same key.
    --skip-signed-boot
              Set SIGNED_BOOT=0 in bootconf.txt: skip the bootloader's
              SHA-256 + RSA-2048 verify of boot.img, about half a billion
              interpreted instructions.
    --eeprom-pubkey <pubkey.bin>
              Put this RSA-2048 public key (n then e, 264 bytes) in the
              pubkey.bin slot, as `rpi-eeprom-config --pubkey` does. Signed
              images, an HTTP-booted boot.img among them, are verified against
              it.

RUNNING:
    --max-wall <secs>
              Stop after this much wall-clock time: 140 s by default, no limit
              with --stdin.
    --max-steps <n>
              Stop after <n> instructions. No cap by default: this is for
              pinning a run to an exact count (bisecting, probes).
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
    --skip-unimpl
              Step over an instruction the decoder does not implement instead
              of stopping the run: reconnaissance on firmware the decoder is
              new to.

OUTPUT:
    -v, --verbose
              Print the full run report as well — the EEPROM layout,
              registers, the ARM cores, the property replies, the peripherals
              that fell through to the stub, the device tree's `/chosen`.
    -q, --quiet
              Leave the serial console out: not streamed, not printed after the
              run, not in the -v report. --console-log still gets it, and
              --until and --send-after still see it. For a run whose log
              channels are the point, e.g. with --log jsonl:io.
    --console-log <path>
              Write the raw UART bytes of the run to <path>, with none of the
              run report interleaved. This is what `boot-check` normalises into
              the golden boot transcript.
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
    --log-file <path>
              Write the --log channels to <path> instead of stderr.
    --dump <hex>:<len>
              After the run, print <len> bytes of memory at <hex>, as the VPU
              sees it. Repeatable.
    --disasm <hex>:<count>
              After the run, disassemble <count> VPU instructions at <hex>.
              Repeatable.
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
              (GET_CRYPTO_HMAC_SHA256). Repeatable, one request each. See
              docs/diagnostics.md.
    --dram-map
              Report which DRAM pages are non-zero when the run ends, as
              address runs: the RAM a snapshot of the machine would have to
              carry (#50).
    -h, --help
              Print this help.

VPU:
    --entry <hex>
              Start there instead of at the ELF's entry point, or at the one
              the boot ROM stage hands on with --eeprom.
    --patch <hex>=<hex>
              Store a word at an address once the image is staged, before the
              first instruction of every boot. Repeatable.
    --exc-vbase <hex>
              The exception vector base to start with, for an ELF that does
              not set its own. start4 does.
    --smp     Start core 1 at the entry too, for payloads that run both cores
              from the start. A firmware boot needs none of this: start4 wakes
              core 1 itself.
    --as-core1
              Run the payload as core 1: bit 16 of `version` reads 1.

DIAGNOSTICS (a `diag` build only: cargo build --release --features diag; the
RVF_* environment variables are in docs/diagnostics.md):
    --trace   Record core 0's control transfers (branches and calls), up to
              4000000, and print them after the run.
    --trace-full
              Every instruction instead, up to 200000.
    --trace-from <hex>
              Every instruction from when core 0 first reaches <hex>, up to
              200000.
    --trace-mmio
              Log every peripheral access with the PC that made it
              (RVF_TRACE_MMIO=<lo>-<hi> for one address range).
";

/// `--net`: what on the host the Ethernet cable plugs into (#45).
enum HostNet {
    /// `--net passt`: a passt of our own, on a socket pair.
    Passt,
    /// `--net passt:<socket>`: whatever listens on that UNIX socket.
    Socket(PathBuf),
}

/// `boot`'s options, as the command line gave them.
struct BootOpts {
    path: PathBuf,
    entry: Option<u32>,
    usb_mb: Option<u64>,
    /// No instruction cap by default — a full boot retires well over a billion,
    /// and the wall clock is the useful bound. `--max-steps` is for pinning a
    /// run to an exact instruction count (bisecting, probes).
    max_steps: Option<u64>,
    max_wall_secs: u64,
    eeprom: bool,
    trace: bool,
    trace_full: bool,
    trace_mmio: bool,
    exc_vbase: u32,
    trace_from: u32,
    smp: bool,
    as_core1: bool,
    patches: Vec<(u32, u32)>,
    dumps: Vec<(u32, u32)>,
    disasms: Vec<(u32, u32)>,
    sd_image: Option<PathBuf>,
    console_log: Option<PathBuf>,
    dump_fdt: Option<PathBuf>,
    print_fdt: bool,
    /// One entry per `--mbox-property`, so several exchanges can be made
    /// against the same booted firmware. A crypto tag that fails leaves an
    /// error code behind that only the *next* request can ask for
    /// (`0x0003008e`).
    mbox_tags: Vec<Vec<MboxTag>>,
    usb_image: Option<PathBuf>,
    netboot_root: Option<PathBuf>,
    host_net: Option<HostNet>,
    boot_order: Option<String>,
    bootconf: Vec<String>,
    eeprom_pubkey: Option<PathBuf>,
    boot_rom_path: Option<PathBuf>,
    stepping: Option<Stepping>,
    board_rev: Option<u32>,
    dram_map: bool,
    skip_signed_boot: bool,
    skip_unimpl: bool,
    until: Option<String>,
    sends: Vec<(String, Vec<u8>)>,
    stdin: bool,
    /// Without `-v` the run prints the serial console, the outcome and whatever
    /// was asked for by name (#55); the full run report is for investigating.
    verbose: bool,
    /// `-q`: no serial console on stdout (#100).
    quiet: bool,
    /// `--log`: the channels to log, and how (#95).
    log: Spec,
    /// `--log-file`: where they go, stderr without it.
    log_file: Option<String>,
    /// `--otp <format>:<file>`: the fuse array across runs (#93).
    otp: Option<OtpFile>,
}

impl BootOpts {
    /// The options, or `None` for `-h` / `--help`.
    fn parse(args: &[String]) -> Result<Option<Self>> {
        let mut path: Option<PathBuf> = None;
        let mut entry: Option<u32> = None;
        let mut usb_mb: Option<u64> = None;
        let mut max_steps: Option<u64> = None;
        let mut max_wall_secs: u64 = 140;
        let mut eeprom = false;
        let mut trace = false;
        let mut trace_full = false;
        let mut trace_mmio = false;
        let mut exc_vbase: u32 = 0;
        let mut trace_from: u32 = 0;
        let mut smp = false;
        let mut as_core1 = false;
        let mut patches: Vec<(u32, u32)> = Vec::new();
        let mut dumps: Vec<(u32, u32)> = Vec::new();
        let mut disasms: Vec<(u32, u32)> = Vec::new();
        let mut sd_image: Option<PathBuf> = None;
        let mut console_log: Option<PathBuf> = None;
        let mut dump_fdt: Option<PathBuf> = None;
        let mut print_fdt = false;
        let mut mbox_tags: Vec<Vec<MboxTag>> = Vec::new();
        let mut usb_image: Option<PathBuf> = None;
        let mut netboot_root: Option<PathBuf> = None;
        let mut host_net: Option<HostNet> = None;
        let mut boot_order: Option<String> = None;
        let mut bootconf: Vec<String> = Vec::new();
        let mut eeprom_pubkey: Option<PathBuf> = None;
        let mut boot_rom_path: Option<PathBuf> = None;
        let mut stepping: Option<Stepping> = None;
        let mut board_rev: Option<u32> = None;
        let mut dram_map = false;
        let mut skip_signed_boot = false;
        let mut skip_unimpl = false;
        let mut until: Option<String> = None;
        let mut sends: Vec<(String, Vec<u8>)> = Vec::new();
        let mut stdin = false;
        let mut verbose = false;
        let mut quiet = false;
        let mut log = Spec::default();
        let mut log_file: Option<String> = None;
        let mut otp: Option<OtpFile> = None;
        let mut it = args.iter().peekable();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--entry" => entry = Some(parse_u32(it.next().context("--entry needs a value")?)?),
                // `--eeprom <image>`, or the older `<image> --eeprom` with the image
                // given as the positional argument.
                "--eeprom" => {
                    eeprom = true;
                    if path.is_none() {
                        if let Some(p) = it.next_if(|p| !p.starts_with('-')) {
                            path = Some(PathBuf::from(p));
                        }
                    }
                }
                "--max-steps" => {
                    max_steps = Some(it.next().context("--max-steps needs a value")?.parse()?)
                }
                "--max-wall" => {
                    max_wall_secs = it.next().context("--max-wall needs seconds")?.parse()?
                }
                "--trace" | "--trace-full" | "--trace-from" | "--trace-mmio"
                    if !rpi_virt_fw::diag::ON =>
                {
                    anyhow::bail!(
                        "{a} needs a build with the `diag` feature: cargo build --release --features diag"
                    )
                }
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
                "--trace-from" => {
                    trace = true;
                    trace_from = parse_u32(it.next().context("--trace-from needs a value")?)?
                }
                "--trace-mmio" => trace_mmio = true,
                // The ARM is always modelled since #52; old command lines keep
                // working.
                "--arm" => {}
                "--until" => until = Some(it.next().context("--until needs a text")?.to_string()),
                "--send-after" => {
                    let prompt = it.next().context("--send-after needs <prompt> <text>")?;
                    let text = it.next().context("--send-after needs <prompt> <text>")?;
                    let prompt = String::from_utf8(harness::boot::unescape(prompt))
                        .context("--send-after: the prompt must be UTF-8")?;
                    sends.push((prompt, harness::boot::unescape(text)));
                }
                "--stdin" => stdin = true,
                "-v" | "--verbose" => verbose = true,
                "-q" | "--quiet" => quiet = true,
                "--log" => {
                    let spec = it
                        .next()
                        .context("--log needs [text:|jsonl:]<channel>[,<channel>...]")?;
                    log.add(Spec::parse(spec).map_err(anyhow::Error::msg)?)
                        .map_err(anyhow::Error::msg)?
                }
                "--log-file" => {
                    log_file = Some(it.next().context("--log-file needs a path")?.clone())
                }
                "--otp" => {
                    otp = Some(
                        it.next()
                            .context("--otp needs json:<file> or binary:<file>")?
                            .parse()?,
                    )
                }
                "--sd" => sd_image = Some(PathBuf::from(it.next().context("--sd needs a path")?)),
                "--console-log" => {
                    console_log = Some(PathBuf::from(
                        it.next().context("--console-log needs a path")?,
                    ))
                }
                "--usb" => {
                    usb_image = Some(PathBuf::from(it.next().context("--usb needs a path")?))
                }
                "--usb-mb" => usb_mb = Some(it.next().context("--usb-mb needs a value")?.parse()?),
                "--net" => {
                    let spec = it.next().context("--net needs passt or passt:<socket>")?;
                    host_net = Some(if spec == "passt" {
                        HostNet::Passt
                    } else {
                        let sock = spec
                            .strip_prefix("passt:")
                            .or_else(|| spec.strip_prefix("stream:"))
                            .with_context(|| {
                                format!("--net {spec}: expected passt or passt:<socket>")
                            })?;
                        HostNet::Socket(PathBuf::from(sock))
                    });
                }
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
                "--boot-rom" => {
                    boot_rom_path =
                        Some(PathBuf::from(it.next().context("--boot-rom needs a file")?))
                }
                "--stepping" => {
                    stepping = Some(Stepping::parse(
                        it.next().context("--stepping needs b0 or c0")?,
                    )?)
                }
                "--board-rev" => {
                    board_rev = Some(Board::parse_revision(
                        it.next().context("--board-rev needs a revision code")?,
                    )?)
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
                "-h" | "--help" => return Ok(None),
                s if !s.starts_with('-') => path = Some(PathBuf::from(s)),
                s => bail!("unexpected argument '{s}' (try boot --help)"),
            }
        }
        let path = path.context("boot: missing <file> (try boot --help)")?;
        if netboot_root.is_some() && host_net.is_some() {
            bail!("--netboot and --net both plug in the Ethernet cable; give one");
        }
        if log.is_empty() && log_file.is_some() {
            bail!("--log-file needs --log <channel>[,<channel>...]");
        }
        Ok(Some(Self {
            path,
            entry,
            usb_mb,
            max_steps,
            max_wall_secs,
            eeprom,
            trace,
            trace_full,
            trace_mmio,
            exc_vbase,
            trace_from,
            smp,
            as_core1,
            patches,
            dumps,
            disasms,
            sd_image,
            console_log,
            dump_fdt,
            print_fdt,
            mbox_tags,
            usb_image,
            netboot_root,
            host_net,
            boot_order,
            bootconf,
            eeprom_pubkey,
            boot_rom_path,
            stepping,
            board_rev,
            dram_map,
            skip_signed_boot,
            skip_unimpl,
            until,
            sends,
            stdin,
            verbose,
            quiet,
            log,
            log_file,
            otp,
        }))
    }
}

/// A finished boot: its last run (the one after the last reset) and the
/// machine that run left behind.
struct Booted {
    report: RunReport,
    emu: Emulator,
    /// Where the last run started.
    start: u32,
    limits: RunLimits,
    /// How many times the firmware reset the machine.
    reboots: u32,
    /// The OTP fuses the first boot started with, to tell what the firmware
    /// programmed (#93).
    fuses_at_start: BTreeMap<u32, u32>,
}

pub fn cmd_boot(args: &[String]) -> Result<ExitCode> {
    let Some(opts) = BootOpts::parse(args)? else {
        print!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    };
    rpi_virt_fw::log::warn_replaced_env();
    let booted = run_boot(&opts)?;
    print_report(&opts, booted)
}

/// Build the machine and run it. A reset the firmware asks for (after an
/// EEPROM self-update) builds it again from the updated flash, up to four
/// times.
fn run_boot(opts: &BootOpts) -> Result<Booted> {
    let image =
        std::fs::read(&opts.path).with_context(|| format!("reading {}", opts.path.display()))?;
    let log = open_log(opts)?;
    let usb_disk = open_usb_disk(opts, &log)?;
    if let Some(sd_path) = opts.sd_image.as_ref().filter(|_| opts.verbose) {
        println!(
            "sd image   {} ({} blocks)",
            sd_path.display(),
            open_sd(sd_path, &log)?.blocks()
        );
    }
    let edits = FlashEdits::new(opts)?;

    // `flash` may be rewritten by an EEPROM self-update; on a firmware-requested
    // reset we rebuild from the updated image and run again.
    let mut flash = image.clone();
    edits.apply(&mut flash, opts.verbose);
    if opts.eeprom && opts.verbose {
        print_eeprom(&flash);
    }

    let limits = run_limits(opts);
    // Made once, outside the reboot loop: it owns the stdin reader and the
    // terminal's raw mode.
    let mut host_input = opts.stdin.then(rpi_virt_fw::stdio::HostInput::stdin);
    let rig = Rig::new(opts, image, log, usb_disk)?;

    let mut reboots = 0u32;
    // A reset does not blank OTP: each boot's machine starts with the rows
    // the one before programmed (#92), the first one with `--otp`'s (#93).
    let mut fuses = load_otp(opts, rig.board)?;
    let mut fuses_at_start = None;
    let mut partition = 0;
    let (report, emu, start) = 'boot: loop {
        let mut machine = rig.machine(&flash)?;
        if let Some(fuses) = fuses.take() {
            machine.config_otp.set_fuses(fuses);
        }
        machine.pm.keep_partition_bits(partition);
        fuses_at_start.get_or_insert_with(|| machine.config_otp.fuses().clone());
        let start = rig.stage(&mut machine, reboots)?;
        let mut emu = rig.emulator(machine, start);
        emu.input.script = opts.sends.iter().cloned().collect();
        emu.input.host = host_input.take();
        let report = emu.run(&limits);
        host_input = emu.input.host.take();
        rig.dump_segment(&emu, &flash, reboots);

        if report.end == rpi_virt_fw::emulator::RunEnd::Reset {
            reboots += 1;
            // Already on the terminal if it was streamed; the run report keeps
            // its copy. `--quiet` wants neither.
            if !opts.quiet && (opts.verbose || !report.console_streamed) {
                print!("{}", String::from_utf8_lossy(&report.console));
            }
            flash = emu.machine.spi0.flash_bytes().to_vec();
            edits.apply(&mut flash, false); // self-update restored SIGNED_BOOT=1
            fuses = Some(emu.machine.config_otp.fuses().clone());
            partition = emu.machine.pm.partition_bits();
            if reboots <= 4 {
                // `RVF_ARM_PROF`: the next boot's ARM side starts a profile
                // of its own, so this one's goes out now.
                if let Some(a) = &mut emu.arm {
                    a.settle(&emu.machine);
                    if let Some(prof) = &a.prof {
                        println!("\n--- ARM cores before the reset ---");
                        print_arm_prof(prof);
                    }
                }
                println!("\n=== RESET (reboot {reboots}) — re-running from updated flash ===\n");
                continue 'boot;
            }
            println!("\n=== RESET (reboot {reboots}) — giving up after 4 reboots ===");
        }
        break 'boot (report, emu, start);
    };
    // The terminal back to cooked mode before the report.
    drop(host_input);
    rig.log.flush();
    Ok(Booted {
        report,
        emu,
        start,
        limits,
        reboots,
        fuses_at_start: fuses_at_start.unwrap_or_default(),
    })
}

/// `--otp <format>:<file>`: the fuses a run before left, when there is a file
/// (#93). It replaces the whole array, and row 30 is the revision code, so it
/// has to be this board's.
fn load_otp(opts: &BootOpts, board: Board) -> Result<Option<BTreeMap<u32, u32>>> {
    let Some(file) = &opts.otp else {
        return Ok(None);
    };
    let Some(fuses) = file.load()? else {
        return Ok(None);
    };
    if let Some(&revision) = fuses.get(&30) {
        if revision != board.revision {
            bail!(
                "--otp {}: row 30 is revision {revision:06x}, but the board is {:06x} \
                 (--board-rev picks another)",
                file.path.display(),
                board.revision
            );
        }
    }
    Ok(Some(fuses))
}

/// `--otp`: write the fuses back when the firmware programmed a row, or when
/// there is no file yet (#93).
fn save_otp(file: &OtpFile, before: &BTreeMap<u32, u32>, now: &BTreeMap<u32, u32>) -> Result<()> {
    let programmed: Vec<String> = now
        .iter()
        .filter(|(row, word)| before.get(row) != Some(word))
        .map(|(row, _)| row.to_string())
        .collect();
    let exists = file.path.exists();
    if exists && programmed.is_empty() {
        return Ok(());
    }
    file.save(now)?;
    println!(
        "otp: {} {}{}",
        if exists { "updated" } else { "created" },
        file.path.display(),
        if programmed.is_empty() {
            String::new()
        } else {
            format!(", rows programmed: {}", programmed.join(" "))
        }
    );
    Ok(())
}

/// `--log` and `--log-file` (#95, `src/log/`): the channels, to stderr or a
/// file. Made once, so it spans the resets of an EEPROM self-update.
fn open_log(opts: &BootOpts) -> Result<Log> {
    if opts.log.is_empty() {
        return Ok(Log::default());
    }
    let out: Box<dyn std::io::Write> = match opts.log_file.as_deref() {
        None | Some("-") => Box::new(std::io::stderr()),
        Some(p) => Box::new(std::fs::File::create(p).with_context(|| format!("creating {p}"))?),
    };
    Ok(Log::new(opts.log, out))
}

/// The USB stick, shared: what the guest writes to it outlives the resets.
type SharedUsbDisk = Rc<RefCell<rpi_virt_fw::periph::usb::Disk>>;

/// `--usb <img>`: a Bulk-Only Transport mass-storage device in blue socket
/// A, which is xHCI root port 2 — a SuperSpeed lane straight onto the root
/// hub, so no hub traversal is involved. See `docs/usb-xhci.md` §5.2 for the
/// socket map.
///
/// `--usb-mb <n>`: the stick is that big, with the image at its start, as
/// on a Pi whose first boot uses the rest. Read on demand; what the guest
/// writes stays in memory and outlives the resets.
fn open_usb_disk(opts: &BootOpts, log: &Log) -> Result<Option<SharedUsbDisk>> {
    Ok(match &opts.usb_image {
        Some(p) => {
            let disk = rpi_virt_fw::periph::usb::Disk::open(p, opts.usb_mb.unwrap_or(0) << 20)
                .with_context(|| format!("opening USB image {}", p.display()))?
                .with_log(log.clone(), "usb");
            if opts.verbose {
                println!("usb image  {} ({} blocks)", p.display(), disk.blocks());
            }
            Some(std::rc::Rc::new(std::cell::RefCell::new(disk)))
        }
        None => None,
    })
}

/// `--sd <img>`: the card reads the image file on demand (#54), and each boot
/// after a reset starts from the file again, writes forgotten.
fn open_sd(p: &Path, log: &Log) -> Result<rpi_virt_fw::periph::disk::Disk> {
    rpi_virt_fw::periph::disk::Disk::open(p, 0)
        .map(|d| d.with_log(log.clone(), "sd"))
        .with_context(|| format!("opening SD image {}", p.display()))
}

/// The `bootconf.txt` edits the options ask for. Applied to the image before
/// the first boot, and again after every self-update reset, since the update
/// brings back the image's own settings.
struct FlashEdits {
    eeprom: bool,
    skip_signed_boot: bool,
    /// `--boot-order` as a `BOOT_ORDER=` line, then every `--bootconf`.
    conf_lines: Vec<String>,
    /// `--eeprom-pubkey`: n then e, 264 bytes.
    pubkey: Option<Vec<u8>>,
}

impl FlashEdits {
    fn new(opts: &BootOpts) -> Result<Self> {
        let conf_lines = opts
            .boot_order
            .iter()
            .map(|o| format!("BOOT_ORDER={o}"))
            .chain(opts.bootconf.iter().cloned())
            .collect();
        let pubkey = match &opts.eeprom_pubkey {
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
        Ok(Self {
            eeprom: opts.eeprom,
            skip_signed_boot: opts.skip_signed_boot,
            conf_lines,
            pubkey,
        })
    }

    /// Every edit, in order; `announce` says what each one did.
    fn apply(&self, flash: &mut Vec<u8>, announce: bool) {
        self.unsign(flash, announce);
        self.append_conf(flash, announce);
        self.set_pubkey(flash, announce);
    }

    /// `--skip-signed-boot`: flip `SIGNED_BOOT=1` -> `=0` in the EEPROM's
    /// `bootconf.txt`. That flag gates the bootloader's signature enforcement, so
    /// clearing it skips the (very slow, ~0.5 G interpreted instructions)
    /// SHA-256 + RSA-2048 verify of `boot.img`. Same length, so the byte layout
    /// is preserved; the now-stale `bootconf.sig` is not checked once the flag
    /// is 0.
    /// Re-applied after every EEPROM self-update (which restores `SIGNED_BOOT=1`).
    fn unsign(&self, flash: &mut [u8], announce: bool) {
        if !(self.skip_signed_boot && self.eeprom) {
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
    }

    /// `--boot-order <hex>`: append a `BOOT_ORDER=` line to the EEPROM's
    /// `bootconf.txt`. The pinned image does not carry one, so the bootloader
    /// falls back to its built-in `0xf4` — SD card, then restart — and never
    /// tries the USB entry, which makes `--usb` unexercisable. The section is
    /// the last one in the image and is followed by erased flash, so growing it
    /// is a length-field bump and an append; nothing moves.
    ///
    /// `--bootconf KEY=VALUE` appends any other line the same way (e.g.
    /// `HTTP_HOST` / `HTTP_PORT` / `HTTP_PATH` for HTTP boot). A later line
    /// overrides an earlier one with the same key.
    fn append_conf(&self, flash: &mut Vec<u8>, announce: bool) {
        if self.conf_lines.is_empty() || !self.eeprom {
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
        let text: String = self.conf_lines.iter().map(|l| format!("{l}\n")).collect();
        let end = hdr + 8 + len;
        let new_len = len + text.len();
        flash.splice(end..end + text.len(), text.bytes());
        flash[hdr + 4..hdr + 8].copy_from_slice(&(new_len as u32).to_be_bytes());
        if announce {
            for l in &self.conf_lines {
                println!("bootconf: appended {l} @ {:#x}", end);
            }
        }
    }

    /// `--eeprom-pubkey <pubkey.bin>`: put an RSA-2048 public key in the
    /// EEPROM's `pubkey.bin` slot, as `rpi-eeprom-config --pubkey` does (n then
    /// e, little-endian, 256 + 8 bytes). Signed images — an HTTP-booted
    /// `boot.img` among them — are verified against it; the pinned image's slot
    /// is all zeros, which verifies nothing.
    fn set_pubkey(&self, flash: &mut [u8], announce: bool) {
        let Some(k) = &self.pubkey else { return };
        match rpi_virt_fw::firmware::eeprom::replace_file(flash, "pubkey.bin", k) {
            Ok(()) if announce => println!("eeprom-pubkey: pubkey.bin replaced"),
            Ok(()) => {}
            Err(e) => eprintln!("eeprom-pubkey: {e:#}"),
        }
    }
}

/// Show the EEPROM section table `bootloader_eeprom_find_files` walks, plus
/// the decoded `bootconf.txt`, so a boot that consults EEPROM config (boot
/// order etc.) can be followed.
fn print_eeprom(flash: &[u8]) {
    if let Ok(img) = rpi_virt_fw::firmware::eeprom::EepromImage::parse(flash) {
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

fn run_limits(opts: &BootOpts) -> RunLimits {
    let BootOpts {
        max_steps,
        max_wall_secs,
        stdin,
        ref until,
        ..
    } = *opts;
    RunLimits {
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
        until: until.clone(),
    }
}

/// What every boot of the run shares. Made once, so it outlives the resets of
/// an EEPROM self-update.
struct Rig<'a> {
    opts: &'a BootOpts,
    /// The file `boot` was given: an EEPROM image or a VPU ELF.
    image: Vec<u8>,
    log: Log,
    usb_disk: Option<SharedUsbDisk>,
    bootrom: rpi_virt_fw::firmware::bootrom::BootRom,
    boot_rom_image: Option<Vec<u8>>,
    board: Board,
}

impl<'a> Rig<'a> {
    fn new(
        opts: &'a BootOpts,
        image: Vec<u8>,
        log: Log,
        usb_disk: Option<SharedUsbDisk>,
    ) -> Result<Self> {
        let BootOpts {
            ref boot_rom_path,
            stepping,
            board_rev,
            verbose,
            ..
        } = *opts;

        // The boot ROM is the model's first stage for an EEPROM boot: it verifies
        // and stages the bootcode (see `firmware::bootrom`). Its HMAC key, when the
        // operator supplies one, comes from the environment and never the repo.
        let bootrom = rpi_virt_fw::firmware::bootrom::BootRom::from_env()?;

        // `--boot-rom <file>`: experimental. Map a real maskROM dump at 0x6000_0000
        // and execute it from the reset vector instead of running the behavioural
        // stage. Most people do not have a dump, so this is optional; the dump stays
        // a local file and is never committed.
        let boot_rom_image = match &boot_rom_path {
            Some(p) => {
                let b = std::fs::read(p)
                    .with_context(|| format!("reading boot ROM {}", p.display()))?;
                if verbose {
                    println!(
                        "boot-rom   {} ({} bytes, experimental)",
                        p.display(),
                        b.len()
                    );
                }
                Some(b)
            }
            None => None,
        };

        // `--stepping` / `--board-rev`: the silicon and the board around it (#77).
        // Naming only one gets a board that fits it.
        let board = {
            let mut board = Board::for_stepping(stepping.unwrap_or_default());
            if let Some(rev) = board_rev {
                board.revision = rev;
            }
            if let Some(why) = board.mismatch() {
                eprintln!(
                    "warning: {} on a board it never shipped on: {why}",
                    board.stepping
                );
            }
            if verbose && board != Board::default() {
                println!(
                    "board      {}, revision {:06x}",
                    board.stepping, board.revision
                );
            }
            board
        };
        Ok(Self {
            opts,
            image,
            log,
            usb_disk,
            bootrom,
            boot_rom_image,
            board,
        })
    }

    /// One boot's machine, with the media, the network and the ROM plugged in.
    fn machine(&self, flash: &[u8]) -> Result<Machine> {
        let BootOpts {
            eeprom,
            ref sd_image,
            ref netboot_root,
            ref host_net,
            trace_mmio,
            ..
        } = *self.opts;
        // The board's own memory, as its revision code gives it. The EEPROM
        // bootloader also touches the `0x6000_0000` L2-SRAM window, which the
        // model folds into DRAM past the 512 MiB mark, so every board that
        // ships has room for it.
        let mut machine = Machine::new(self.board.memory_bytes());
        machine.set_board(self.board);
        machine.set_log(self.log.clone());
        if eeprom {
            machine.spi0.attach_flash(flash.to_vec());
        }
        if let Some(p) = &sd_image {
            machine.emmc2.insert_disk(open_sd(p, &self.log)?);
        }
        if let Some(disk) = &self.usb_disk {
            machine.pcie.endpoint.attach(
                USB_ROOT_PORT,
                Box::new(rpi_virt_fw::periph::usb::MassStorage::with_disk(
                    disk.clone(),
                )),
            );
        }
        // `--netboot <dir>`: plug the Ethernet cable into the built-in network
        // peer (`src/net/peer.rs`): DHCP, DNS, and `<dir>` over TFTP and HTTP.
        if let Some(dir) = &netboot_root {
            let peer =
                rpi_virt_fw::net::BuiltinPeer::with_root(dir.clone()).with_log(self.log.clone());
            machine.attach_net(Box::new(peer));
        }
        // `--net passt[:<socket>]`: the host's network (#45). A new connection
        // (and a new passt) for every boot, like a cable plugged in again
        // after a reset.
        if let Some(host_net) = &host_net {
            let net = match host_net {
                HostNet::Passt => rpi_virt_fw::net::StreamBackend::spawn_passt().map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        anyhow::anyhow!(
                            "--net passt starts passt, and there is no `passt` in PATH. \
                             Install it with one of:\n  \
                             sudo apt install passt    (Debian, Ubuntu)\n  \
                             sudo dnf install passt    (Fedora)\n\
                             or see https://passt.top/"
                        )
                    } else {
                        anyhow::Error::new(e).context("starting passt")
                    }
                })?,
                HostNet::Socket(sock) => rpi_virt_fw::net::StreamBackend::connect(sock)
                    .with_context(|| format!("connecting to {}", sock.display()))?,
            };
            machine.attach_net(Box::new(net));
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
        Ok(machine)
    }

    /// Stage the first instruction stream, apply the `--patch`es, and return
    /// where to start.
    fn stage(&self, machine: &mut Machine, reboots: u32) -> Result<u32> {
        let BootOpts {
            eeprom,
            entry,
            ref patches,
            verbose,
            ..
        } = *self.opts;
        // Stage the first instruction stream. For an EEPROM boot that is the
        // modelled boot ROM: it reads the image off the SPI flash, checks the
        // bootcode signature and stages it (see `firmware::bootrom`), talking to
        // the peripherals the real ROM does instead of reaching around them. For
        // a raw ELF it is the loader placing its segments.
        let start = if eeprom {
            if let Some(rom) = &self.boot_rom_image {
                // Execute the real maskROM from its reset vector. It reads the
                // pieeprom off SPI0, the key rows out of OTP, and stages the
                // bootcode itself — the peripherals do the rest.
                machine.attach_boot_rom(rom.clone());
                if reboots == 0 && verbose {
                    println!(
                        "boot ROM: executing real maskROM from reset vector 0x60000000 (experimental)"
                    );
                }
                entry.unwrap_or(0x6000_0000)
            } else {
                let outcome = self.bootrom.boot(machine)?;
                if reboots == 0 && verbose {
                    for line in &outcome.log {
                        println!("{line}");
                    }
                }
                entry.unwrap_or(outcome.entry)
            }
        } else {
            let payload = Payload::from_elf_bytes(&self.image)?;
            payload.load_into(machine)?;
            entry.unwrap_or(payload.entry())
        };
        for &(a, v) in patches {
            use rpi_virt_fw::bus::Bus;
            machine.store32(a, v).ok();
            if verbose {
                println!("patch [{a:#010x}] = {v:#010x}");
            }
        }
        Ok(start)
    }

    /// The emulator around one boot's machine, set up the way the options say.
    fn emulator(&self, machine: Machine, start: u32) -> Emulator {
        let BootOpts {
            skip_unimpl,
            trace,
            trace_full,
            exc_vbase,
            trace_from,
            as_core1,
            smp,
            quiet,
            ..
        } = *self.opts;
        let mut emu = Emulator::new(machine, start);
        emu.stream_console = !quiet;
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
        if as_core1 {
            emu.cpu.core_id = 1;
        }
        if smp {
            emu.start_smp(start);
        }
        emu
    }

    /// `RVF_DUMP_FLASH` and `RVF_DUMP_RAM`, after every run segment.
    fn dump_segment(&self, emu: &Emulator, flash: &[u8], reboots: u32) {
        // `RVF_DUMP_FLASH=<path>` writes the (self-update-modified) EEPROM image
        // after every run segment — `<path>.<n>` — so a run that reaches
        // "BOOT-EEPROM: UPDATED" but stops before RESET still yields the burned
        // image. Feed it back as `boot <path>.<n> --eeprom` for a fast, already
        // provisioned boot (no self-update, no reboot).
        if self.opts.eeprom {
            if let Ok(p) = std::env::var("RVF_DUMP_FLASH") {
                let cur = emu.machine.spi0.flash_bytes();
                if cur != flash {
                    let _ = std::fs::write(format!("{p}.{}", reboots + 1), cur);
                    eprintln!("wrote {p}.{} ({} bytes)", reboots + 1, cur.len());
                }
            }
        }
        // `RVF_DUMP_RAM=<path>` writes SDRAM out the same way, before a reset
        // replaces it: a kernel that dies before its console comes up still
        // has its log buffer in there.
        if let Ok(p) = std::env::var("RVF_DUMP_RAM") {
            let ram = emu.machine.ram.as_slice();
            let _ = std::fs::write(format!("{p}.{}", reboots + 1), ram);
            eprintln!("wrote {p}.{} ({} bytes)", reboots + 1, ram.len());
        }
    }
}

/// What `boot` prints once the run is over: the console unless it was
/// streamed, whatever was asked for by name, the full report with `-v`, and
/// the `result:` line.
fn print_report(opts: &BootOpts, booted: Booted) -> Result<ExitCode> {
    let Booted {
        report,
        mut emu,
        start,
        limits,
        reboots,
        fuses_at_start,
    } = booted;
    let verbose = opts.verbose;

    // A core parked in a busy-wait loop is behind on its registers and its
    // instruction count until it is brought up to date.
    if let Some(a) = &mut emu.arm {
        a.settle(&emu.machine);
    }
    if verbose {
        print_summary(&report, &emu, start);
        print_arm_cores(&emu, opts.eeprom);
        print_property_replies(&emu.machine);
        print_regs(&report);
    }
    if let Some(p) = &opts.console_log {
        write_console_log(p, &report.console, verbose)?;
    }
    if verbose {
        print_network(&mut emu.machine);
    }
    if !opts.quiet {
        print_console(&report, verbose);
    }
    for &(a, n) in &opts.dumps {
        print_dump(&mut emu.machine, a, n);
    }
    for &(a, count) in &opts.disasms {
        print_disasm(&mut emu.machine, a, count);
    }
    if verbose {
        print_phase_tags(&report);
    }
    print_traces(&emu, opts.trace);
    if verbose {
        print_control_transfers(&emu);
        print_stub_log(&emu.machine);
    }
    if opts.dram_map {
        print_dram_map(&emu.machine);
    }
    if verbose {
        print_sdram_refresh(&emu.machine);
    }
    for group in &opts.mbox_tags {
        mbox_property_exchange(&mut emu, &limits, group)?;
    }
    // After the exchanges: they are where a firmware-only boot programs.
    if let Some(file) = &opts.otp {
        save_otp(file, &fuses_at_start, emu.machine.config_otp.fuses())?;
    }
    let handoff = emu.arm.as_ref().and_then(|a| a.handoff);
    report_fdt(opts, &mut emu.machine, handoff, &report.console)?;
    if verbose {
        print_unimpl(&report, &opts.path);
    }

    let (ok, why) = boot_outcome(&report, &emu, opts.eeprom, limits.until.as_deref(), reboots);
    println!("\nresult: {} — {why}", if ok { "ok" } else { "FAILED" });
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The run in numbers: where it started and ended, and what it retired.
fn print_summary(report: &RunReport, emu: &Emulator, start: u32) {
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
}

/// The ARM side (#40): the hand-off, each core, and where it stopped.
fn print_arm_cores(emu: &Emulator, eeprom: bool) {
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
            if c.sha_blocks > 0 {
                println!(
                "            {} SHA-256 block loop(s), {} blocks hashed natively (RVF_NO_SHA_SKIP=1 to compare)",
                c.sha_loops, c.sha_blocks
            );
            }
            println!(
                "            daif {:#x}  irq line {}  gic {}",
                c.cpu.daif >> 6,
                u8::from(c.cpu.irq_line),
                emu.machine.gic.describe(i)
            );
        }
        if let Some(stop) = &a.stopped {
            println!("  stopped   {stop:x?}");
        }
        if let Some(prof) = &a.prof {
            print_arm_prof(prof);
        }
    } else if eeprom {
        println!("\n--- ARM cores (#40) ---\n  never released");
    }
}

/// What the firmware answered on the property channel, from the reply
/// buffers themselves: the only place a value Linux never checks shows up.
fn print_property_replies(machine: &Machine) {
    let prop = &machine.mbox.property;
    if prop.replies > 0 {
        println!("\n--- property replies (0x7e00_b880) ---");
        println!(
            "  {} replies, {} with an error code",
            prop.replies, prop.failed
        );
        for (tag, t) in prop.tags() {
            let last = t.last.map_or("-".to_string(), |v| format!("{v:#010x}"));
            println!(
                "  tag {tag:#010x}  marked {:<4} unmarked {:<4} last value {last}",
                t.marked, t.unmarked
            );
        }
    }
}

/// The VPU's registers where the run ended.
fn print_regs(report: &RunReport) {
    print!("regs      ");
    for (i, r) in report.regs.iter().enumerate() {
        if i % 8 == 0 {
            print!("\n  r{i:<2}");
        }
        print!(" {r:08x}");
    }
    println!();
}

/// `--console-log <path>`: the UART bytes on their own, with none of the
/// run report interleaved. `boot-check` normalises this into the golden
/// transcript; picking the console out of the combined log afterwards would
/// be guesswork, since both streams land in the same file.
///
/// On a run that rebooted (EEPROM self-update) this is the last segment
/// only, which is the one the assertions are about.
fn write_console_log(p: &Path, console: &[u8], verbose: bool) -> Result<()> {
    std::fs::write(p, console).with_context(|| format!("writing console log {}", p.display()))?;
    if verbose {
        println!("console log {} ({} bytes)", p.display(), console.len());
    }
    Ok(())
}

/// What went over the Ethernet cable, and what the other end logged.
fn print_network(machine: &mut Machine) {
    if let Some(net) = machine.net.as_mut() {
        let st = machine.genet.stats;
        println!("\n--- network (GENET <-> {}) ---", net.name());
        println!(
            "  tx {} (dropped {})  rx {} (filtered {}, dropped {})",
            st.tx, st.tx_dropped, st.rx, st.rx_filtered, st.rx_dropped
        );
        for line in net.take_log() {
            println!("  {line}");
        }
    }
}

/// The console: in full with `-v`, or whatever was not streamed already.
fn print_console(report: &RunReport, verbose: bool) {
    if !report.console.is_empty() && verbose {
        println!("\n--- console ({} bytes) ---", report.console.len());
        if report.console_streamed {
            // Already written out line by line while the run was going.
            println!("(streamed above; RVF_LIVE_CONSOLE=0 to buffer it here instead)");
        } else {
            println!("{}", String::from_utf8_lossy(&report.console));
        }
    } else if !report.console_streamed {
        print!("{}", String::from_utf8_lossy(&report.console));
    }
}

/// `--dump <addr>:<len>`: memory as the VPU sees it, in hex.
fn print_dump(machine: &mut Machine, a: u32, n: u32) {
    use rpi_virt_fw::bus::Bus;
    print!("dump {a:#010x}:");
    for i in 0..n {
        if i % 32 == 0 {
            print!("\n  {:#010x} ", a + i);
        }
        print!(
            "{:02x}",
            machine
                .load(a + i, rpi_virt_fw::bus::Width::Byte)
                .unwrap_or(0) as u8
        );
    }
    println!();
}

/// `--disasm <addr>:<count>`: VPU instructions from memory.
fn print_disasm(machine: &mut Machine, a: u32, count: u32) {
    use rpi_virt_fw::bus::Bus;
    println!("disasm {a:#010x}:");
    let mut pc = a;
    let mut buf = [0u8; 10];
    for _ in 0..count {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = machine
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

/// The boot-progress tags start4 writes (0xcec02000), as text.
fn print_phase_tags(report: &RunReport) {
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
}

/// The VPU instruction traces (`--trace*`, `RVF_TRACE_ON_*`).
fn print_traces(emu: &Emulator, trace: bool) {
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
}

/// The VPU's last control transfers, newest first.
fn print_control_transfers(emu: &Emulator) {
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
}

/// The peripheral-window offsets nothing models, which fell through to the stub.
fn print_stub_log(machine: &Machine) {
    let log = &machine.periph_stub.log;
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

/// `--dram-map`: which DRAM pages are non-zero when the run ends, and where –
/// the RAM a snapshot of the machine would have to carry (#50).
///
/// "Dirty" is approximated as "not all zero", which is exact for that: the
/// model starts RAM zeroed, so a zero page needs no saving.
fn print_dram_map(machine: &Machine) {
    const PAGE: usize = 4096;
    let ram = &machine.ram;
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
    println!("\n--- DRAM occupancy when the run ended ---");
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

/// The DRAM refresh interval start4 rescales from the LPDDR4 MR4 code
/// once the ARM is running. The firmware logs the change as
/// `sdram: sdram refresh 1562->3124 (2)`, but by then it has handed the
/// UART to Linux and only its internal message ring sees that line, so
/// the controller state is the console-independent way to check it.
fn print_sdram_refresh(machine: &Machine) {
    let sdc = &machine.sdc;
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

/// The device tree `arm_loader` leaves behind for the ARM, and the
/// `/chosen` identity properties it patched into it. This is the point
/// of the bench (rpi-mkosi#37): `rpi-machine-id` feeds the root LUKS
/// passphrase, so a firmware bump that changes how it is derived has to
/// be caught here rather than on a thousand deployed cards.
///
/// The blob is the one the armstub hands the ARM (`dtb_ptr32`), or, for a
/// run whose ARM was never released, the one the firmware's `Device tree
/// loaded to 0x%x (size 0x%x)` line names ([`locate_fdt`]). The header is
/// validated before anything is believed or written out.
fn report_fdt(
    opts: &BootOpts,
    machine: &mut Machine,
    handoff: Option<Handoff>,
    console: &[u8],
) -> Result<()> {
    let BootOpts {
        verbose,
        print_fdt,
        ref dump_fdt,
        ..
    } = *opts;
    match locate_fdt(machine, handoff, console) {
        Some((addr, blob)) => {
            match rpi_virt_fw::fdt::Fdt::parse(&blob) {
                Ok(fdt) => {
                    if verbose {
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
                            println!(
                                "  (--print-fdt for every node, --dump-fdt <path> for the blob)"
                            );
                        }
                        report_machine_id_derivation(machine, &fdt);
                    }
                    if print_fdt {
                        println!("\n{}", fdt.to_dts());
                    }
                    if let Some(out) = &dump_fdt {
                        std::fs::write(out, fdt.bytes())
                            .with_context(|| format!("writing {}", out.display()))?;
                        println!("  wrote {} ({} bytes)", out.display(), fdt.bytes().len());
                    }
                }
                Err(e) if verbose || print_fdt || dump_fdt.is_some() => {
                    println!("\n--- device tree handed to the ARM ---\n  at {addr:#010x}: {e}")
                }
                Err(_) => {}
            }
        }
        None => {
            if dump_fdt.is_some() {
                bail!(
                    "--dump-fdt: the ARM was never released and the boot never printed \
                     'Device tree loaded to ...', so there is no device tree to dump"
                );
            }
        }
    }
    Ok(())
}

/// The instructions the decoder did not know, by hit count.
fn print_unimpl(report: &RunReport, path: &Path) {
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
}

/// Did the boot do what it was run for, and in a word, what happened (#55).
///
/// An EEPROM boot succeeds once the firmware has started the ARM, which is
/// where a Pi's boot firmware is done; what the run does after that (the
/// firmware idling until the silence limit, a Linux boot) does not undo it.
/// With `--until` the text has to appear. A VPU ELF succeeds by halting.
/// Anything the model could not do — an unknown instruction, a bus fault, an
/// ARM core stopping — fails the run whenever it happens.
fn boot_outcome(
    report: &rpi_virt_fw::emulator::RunReport,
    emu: &Emulator,
    eeprom: bool,
    until: Option<&str>,
    reboots: u32,
) -> (bool, String) {
    use rpi_virt_fw::emulator::RunEnd;
    use rpi_virt_fw::vpu::exec::Stop;

    let end = match &report.end {
        RunEnd::Until => {
            return (
                true,
                format!("the console printed {:?}", until.unwrap_or("")),
            )
        }
        RunEnd::Quit => return (true, "the session was ended".into()),
        RunEnd::Halted(Stop::Fault(f)) => return (false, format!("the VPU faulted: {f:x?}")),
        RunEnd::Core1Halted(Stop::Fault(f)) => {
            return (false, format!("VPU core 1 faulted: {f:x?}"))
        }
        RunEnd::ArmStopped(s) => {
            return (
                false,
                format!("an ARM core hit something not modelled: {s:x?}"),
            )
        }
        RunEnd::Reset => {
            return (
                false,
                format!("the firmware kept resetting ({reboots} reboots)"),
            )
        }
        RunEnd::Halted(Stop::Halt(h)) | RunEnd::Core1Halted(Stop::Halt(h)) => {
            if !eeprom && until.is_none() {
                return (true, format!("the program halted ({h:?})"));
            }
            format!("the VPU halted ({h:?})")
        }
        RunEnd::StopPc(pc) => format!("reached pc {pc:#010x}"),
        RunEnd::StepLimit => "the instruction limit was reached".into(),
        RunEnd::TimeLimit => "the wall-clock limit was reached".into(),
        RunEnd::IdleSpin(pc) => format!("it spun at {pc:#010x} with no output"),
        RunEnd::Stuck { pc, silent_us, .. } => format!(
            "no console output for {:.1} s of modelled time, at pc {pc:?}",
            *silent_us as f64 / 1e6
        ),
    };
    if let Some(text) = until {
        return (false, format!("the console never printed {text:?}: {end}"));
    }
    if !eeprom {
        return (false, end);
    }
    if emu.machine.armctrl.released() {
        (true, "the firmware started the ARM".into())
    } else {
        (false, format!("the firmware never started the ARM: {end}"))
    }
}

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

/// Find the device tree blob `arm_loader` left for the ARM.
///
/// The pointer is the armstub's `dtb_ptr32`, the word the primary core puts in
/// `x0` for the kernel, so the blob is by definition the one the ARM is handed.
/// Only a run whose ARM was never released falls back to the firmware's own
/// `Device tree loaded to 0x<addr> (size 0x<len>)` console line. The console
/// cannot be the only source: the cut-down `start4cd.elf` prints nothing after
/// the bootloader starts it (#105). Neither way hard-codes an address, which
/// keeps this working across firmware versions — the entire point, since the
/// bench exists to diff one version against another.
///
/// The header's `totalsize` is trusted over the logged length, so a firmware
/// that logs a rounded figure still yields an exact blob, and so does the tree
/// [`rpi_virt_fw::armstub::add_bootargs`] grew in place. Returns the address and
/// the bytes.
fn locate_fdt(
    machine: &mut Machine,
    handoff: Option<Handoff>,
    console: &[u8],
) -> Option<(u32, Vec<u8>)> {
    use rpi_virt_fw::bus::{Bus, Width};

    let text = String::from_utf8_lossy(console);
    // Last one wins: a `tryboot` retry would load the tree more than once.
    let logged = text
        .rsplit_once("Device tree loaded to 0x")
        .and_then(|(_, tail)| {
            let (addr_hex, rest) = tail.split_once(" (size 0x")?;
            let addr = u32::from_str_radix(addr_hex.trim(), 16).ok()?;
            let len = rest
                .split_once(')')
                .and_then(|(l, _)| u32::from_str_radix(l.trim(), 16).ok())
                .unwrap_or(0);
            Some((addr, len))
        });
    // The armstub itself lives at 0, so a zero pointer is no tree at all.
    let (addr, logged_len) = match handoff.map(|h| h.dtb).filter(|&a| a != 0) {
        Some(addr) => (addr, logged.filter(|l| l.0 == addr).map_or(0, |l| l.1)),
        None => logged?,
    };

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

/// `RVF_ARM_PROF`'s table: the hottest ARM steps by core, EL and 256-byte PC
/// bucket.
fn print_arm_prof(prof: &std::collections::HashMap<(usize, u32, u64), u64>) {
    let total: u64 = prof.values().sum();
    let mut v: Vec<_> = prof.iter().collect();
    v.sort_by_key(|(_, &n)| std::cmp::Reverse(n));
    println!("  RVF_ARM_PROF: steps by core, EL and 256-byte PC bucket (total {total})");
    for ((core, el, pc), &n) in v.into_iter().take(30) {
        println!(
            "    core {core} EL{el} {pc:#014x}  {n:>13}  {:5.1}%",
            100.0 * n as f64 / total as f64
        );
    }
}

/// Recompute `/chosen/rpi-machine-id` from the modelled OTP and say whether the
/// firmware's own value still matches.
///
/// This is the one thing in the report that is a *prediction* rather than an
/// observation. `boot-check` pins the published string, which catches
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn help_wherever_an_option_can_go() {
        assert!(BootOpts::parse(&args(&["--help"])).unwrap().is_none());
        assert!(BootOpts::parse(&args(&["--eeprom", "x.bin", "-h"]))
            .unwrap()
            .is_none());
        // A value is not an option.
        assert!(BootOpts::parse(&args(&["x.elf", "--until", "--help"]))
            .unwrap()
            .is_some());
    }

    /// The options `boot --help` lists: the lines that start with one, such as
    /// `    -v, --verbose` or `    --stdin   Interactive session: ...`.
    fn documented() -> BTreeSet<&'static str> {
        HELP.lines()
            .filter(|l| l.starts_with("    -"))
            .flat_map(|l| l.trim_start().split("  ").next().unwrap_or("").split(", "))
            .filter_map(|o| o.split(' ').next())
            .collect()
    }

    /// The options `BootOpts::parse` matches on, read out of its source: the
    /// string literals there that are an option name and nothing else. (A
    /// double quote in a comment inside `parse` would throw this off.)
    fn parsed() -> BTreeSet<&'static str> {
        let src = include_str!("boot.rs");
        let start = src.find("fn parse(").unwrap();
        let end = start + src[start..].find("let path = path.context").unwrap();
        src[start..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|s| {
                s.len() > 1
                    && s.starts_with('-')
                    && s.bytes()
                        .all(|b| b == b'-' || b.is_ascii_lowercase() || b.is_ascii_digit())
            })
            .collect()
    }

    #[test]
    fn help_lists_every_option_the_parser_takes() {
        // A no-op since #52, kept for old command lines.
        let hidden = ["--arm"];
        let (documented, parsed) = (documented(), parsed());
        for o in &parsed {
            assert!(
                documented.contains(o) || hidden.contains(o),
                "{o} is missing from boot --help"
            );
        }
        for o in &documented {
            assert!(
                parsed.contains(o),
                "boot --help lists {o}, which boot does not take"
            );
        }
    }
}
