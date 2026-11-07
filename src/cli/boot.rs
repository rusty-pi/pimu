//! `boot`: boot the machine from an EEPROM image, as a Pi 4 does, or run a VPU ELF, then report on the run.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use anyhow::{bail, Context, Result};
use clap::{ArgMatches, FromArgMatches};

use pimu::boot::{boot_outcome, locate_fdt, CardEdits, Carry, FlashEdits, USB_ROOT_PORT};
use pimu::emulator::{Emulator, RunLimits, RunReport};
use pimu::firmware::Payload;
use pimu::log::{Log, Spec};
use pimu::machine::Machine;
use pimu::mem::Region;
use pimu::periph::usb::{HostSilent, StickModel};
use pimu::soc::{Board, Stepping};
use pimu::vpu::decode::decode;
use pimu::vpu::length::insn_len_bytes;
use pimu::vpu::UnimplPolicy;

use crate::boot_args::{BootArgs, HostNet};
use crate::mbox::{mbox_property_exchange, MboxRequest};
use crate::otp::{Format, OtpFile};

struct BootOpts {
    path: PathBuf,
    /// `--snapshot-save`: stop when the ARM is released and save the machine
    /// there; `--snapshot-load`: go on from such a file (`src/snap.rs`).
    snapshot_save: Option<PathBuf>,
    snapshot_load: Option<PathBuf>,
    entry: Option<u32>,
    usb_mb: Option<u64>,
    otg_dock: bool,
    /// `--usb-uas` / `--usb-uas-refuse`: the `--usb` stick speaks UAS too.
    usb_uas: UsbUas,
    /// `--usb-model`.
    usb_model: StickModel,
    /// `--usb-wedge host-silent:<seconds>|off`.
    usb_host_silent: HostSilent,
    display: bool,
    display_edid: Option<PathBuf>,
    /// No cap by default: the wall clock is the useful bound.
    max_steps: Option<u64>,
    max_wall_secs: u64,
    /// Real-time cap, `None` for `--speed max`.
    speed: Option<f64>,
    eeprom: bool,
    trace: bool,
    trace_full: bool,
    trace_mmio: bool,
    exc_vbase: u32,
    trace_from: u32,
    smp: bool,
    parallel: bool,
    as_core1: bool,
    patches: Vec<(u32, u32)>,
    dumps: Vec<(u32, u32)>,
    disasms: Vec<(u32, u32)>,
    sd: Option<Medium>,
    card_edits: CardEdits,
    emmc: Option<Medium>,
    hat_eeprom: Option<PathBuf>,
    check_coherency: bool,
    check_alignment: bool,
    jitter: Option<u64>,
    faults: Option<u64>,
    console_log: Option<PathBuf>,
    dump_fdt: Option<PathBuf>,
    dump_eeprom: Option<PathBuf>,
    print_fdt: bool,
    /// Several exchanges against one booted firmware: a failed crypto tag leaves an
    /// error code only the *next* request can ask for (`0x0003008e`).
    mbox_tags: Vec<MboxRequest>,
    /// `--gencmd` command lines, all run over one VCHIQ connection.
    gencmds: Vec<String>,
    usb: Option<Medium>,
    otg: Option<Medium>,
    tftp: Option<NetRoot>,
    http: Option<NetRoot>,
    host_net: Option<HostNet>,
    boot_order: Option<String>,
    bootconf: Vec<String>,
    eeprom_pubkey: Option<PathBuf>,
    maskrom_path: Option<PathBuf>,
    stepping: Option<Stepping>,
    board_rev: Option<u32>,
    dram_map: bool,
    eeprom_map: bool,
    stub_log: bool,
    control_transfers: bool,
    skip_signed_boot: bool,
    tryboot: bool,
    orderly_reboot: bool,
    skip_unimpl: bool,
    until: Option<String>,
    sends: Vec<(String, Vec<u8>)>,
    magic_packet_after: Option<String>,
    stdin: bool,
    verbose: bool,
    quiet: bool,
    log: Spec,
    log_file: Option<String>,
    otp: Option<OtpFile>,
    /// `--otp-row`: rows to fuse before the boot, over the model's own.
    otp_rows: Vec<(u32, u32)>,
}

/// What a medium option names: a disk image, or a directory of a boot
/// partition's files that the card is built around — on the host, or on a
/// server. A URL that ends in `/` is a directory, as it is in a browser.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Medium {
    Image(PathBuf),
    Files(PathBuf),
    /// A YAML file of partitions, as `partitions:<file>`: a GPT disk built
    /// from them ([`pimu::layout`]).
    Partitions(PathBuf),
    RemoteImage(String),
    RemoteFiles(String),
}

impl Medium {
    fn from_arg(s: &str) -> Medium {
        if let Some(file) = s.strip_prefix(pimu::layout::PARTITIONS_PREFIX) {
            return Medium::Partitions(PathBuf::from(file));
        }
        match pimu::remote::is_url(s) {
            true if s.ends_with('/') => Medium::RemoteFiles(s.to_string()),
            true => Medium::RemoteImage(s.to_string()),
            false if Path::new(s).is_dir() => Medium::Files(PathBuf::from(s)),
            false => Medium::Image(PathBuf::from(s)),
        }
    }

    /// `--sd-dir`, which took the files even where the name says nothing.
    fn files_from_arg(s: &str) -> Medium {
        match pimu::remote::is_url(s) {
            true => Medium::RemoteFiles(s.to_string()),
            false => Medium::Files(PathBuf::from(s)),
        }
    }

    fn is_files(&self) -> bool {
        matches!(self, Medium::Files(_) | Medium::RemoteFiles(_))
    }

    /// The medium as the machine takes it: `min_bytes` of capacity at the
    /// least, reading as zeros past what backs it (`--usb-mb`).
    fn disk(
        &self,
        edits: &CardEdits,
        min_bytes: u64,
        log: &Log,
        what: &'static str,
    ) -> Result<pimu::periph::disk::Disk> {
        let disk = match self {
            Medium::Image(path) => pimu::periph::disk::Disk::open(path, min_bytes)
                .with_context(|| format!("opening {what} image {}", path.display()))?,
            Medium::RemoteImage(url) => {
                let probe = pimu::remote::probe(url)
                    .with_context(|| format!("reading the headers of {what} image {url}"))?;
                // Without `Range` there is no reading a part of it, so the
                // whole image is fetched once and cached.
                if !probe.ranges {
                    eprintln!(
                        "remote: {what} is {url}, {} fetched whole: \
                         the server answers no Range request",
                        mib(probe.len)
                    );
                    let path = pimu::remote::fetch_to_cache(url)?;
                    return Ok(pimu::periph::disk::Disk::open(&path, min_bytes)
                        .with_context(|| format!("opening {what} image {}", path.display()))?
                        .with_log(log.clone(), what));
                }
                eprintln!(
                    "remote: {what} is {url}, {} read in pieces as the guest asks",
                    mib(probe.len)
                );
                pimu::periph::disk::Disk::remote(url.clone(), probe.len, min_bytes)?
            }
            Medium::Partitions(file) => {
                let (spec, base) = pimu::layout::PartitionsSpec::load(file)?;
                let card = spec
                    .card(&base)
                    .with_context(|| format!("building a disk out of {self}"))?;
                pimu::periph::disk::Disk::from_card(card)
                    .with_context(|| format!("opening the partitions in {self}"))?
                    .with_capacity(min_bytes)
            }
            Medium::Files(_) | Medium::RemoteFiles(_) => {
                let card = self
                    .card(edits)
                    .with_context(|| format!("building a card out of {self}"))?;
                pimu::periph::disk::Disk::from_card(card)
                    .with_context(|| format!("opening the files in {self}"))?
                    .with_capacity(min_bytes)
            }
        };
        Ok(disk.with_log(log.clone(), what))
    }

    fn card(&self, edits: &CardEdits) -> Result<pimu::fat::Card> {
        let mut entries = match self {
            Medium::Files(dir) => pimu::fat::entries_of_dir(dir)?,
            Medium::RemoteFiles(url) => {
                eprintln!("remote: {url} is a card, its files read as the guest asks");
                pimu::remote::listing(url)?
            }
            _ => bail!("{self} is a disk image, not a directory of files"),
        };
        edits.apply(&mut entries)?;
        pimu::fat::card_from_entries(entries)
    }
}

impl std::fmt::Display for Medium {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Medium::Image(path) | Medium::Files(path) => write!(f, "{}", path.display()),
            Medium::Partitions(file) => {
                write!(f, "{}{}", pimu::layout::PARTITIONS_PREFIX, file.display())
            }
            Medium::RemoteImage(url) | Medium::RemoteFiles(url) => write!(f, "{url}"),
        }
    }
}

/// `--tftp` and `--http`: what the built-in peer serves over each.
#[derive(Clone, Debug, PartialEq, Eq)]
enum NetRoot {
    Dir(PathBuf),
    Url(String),
}

impl NetRoot {
    fn from_arg(flag: &str, s: &str) -> Result<NetRoot> {
        if pimu::remote::is_url(s) {
            return Ok(NetRoot::Url(s.to_string()));
        }
        let dir = PathBuf::from(s);
        if !dir.is_dir() {
            bail!("{flag} {s}: not a directory (the peer serves what is in it)");
        }
        Ok(NetRoot::Dir(dir))
    }

    fn peer_root(&self, protocol: &str) -> pimu::net::PeerRoot {
        match self {
            NetRoot::Dir(dir) => pimu::net::PeerRoot::Dir(dir.clone()),
            NetRoot::Url(url) => {
                eprintln!("remote: the network peer serves {url} over {protocol}, fetched as the guest asks");
                pimu::net::PeerRoot::Url(url.clone())
            }
        }
    }
}

impl std::fmt::Display for NetRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetRoot::Dir(dir) => write!(f, "{}", dir.display()),
            NetRoot::Url(url) => write!(f, "{url}"),
        }
    }
}

/// A size for a person, not a number of bytes.
fn mib(bytes: u64) -> String {
    match bytes >> 20 {
        0 => format!("{} KiB", bytes.div_ceil(1 << 10)),
        mib => format!("{mib} MiB"),
    }
}

/// One medium, one argument: two of them naming the same host is a mistake,
/// not a last-one-wins.
fn take_medium(slot: &mut Option<Medium>, what: &str, medium: Medium) -> Result<()> {
    if let Some(taken) = slot {
        bail!("{what} {medium}: {taken} is already there, and a machine has one of each");
    }
    *slot = Some(medium);
    Ok(())
}

/// A file option's argument: a URL is fetched into the cache, so every option
/// that reads a file takes one.
fn file_arg(what: &str, s: &str) -> Result<PathBuf> {
    match pimu::remote::is_url(s) {
        true => pimu::remote::fetch_to_cache(s).with_context(|| format!("{what} {s}")),
        false => Ok(PathBuf::from(s)),
    }
}

/// Zero-config: an option left out takes the file of that name in the working
/// directory, under the names the Pi's own tooling gives. Only an option the
/// command line is silent about is filled in, and only when nothing rules it out
/// (`--emmc` is the same host as `--sd`, `--net` the same cable as `--tftp` and `--http`).
struct ZeroConfig<'a> {
    dir: &'a Path,
    found: Vec<String>,
}

impl<'a> ZeroConfig<'a> {
    fn new(dir: &'a Path) -> Self {
        Self {
            dir,
            found: Vec::new(),
        }
    }

    fn file(&mut self, slot: &mut Option<PathBuf>, name: &str) {
        self.pick(slot, name, |p| p.is_file())
    }

    fn dir(&mut self, slot: &mut Option<PathBuf>, name: &str) {
        self.pick(slot, name, |p| p.is_dir())
    }

    fn pick(&mut self, slot: &mut Option<PathBuf>, name: &str, there: fn(&Path) -> bool) {
        if slot.is_some() {
            return;
        }
        let path = self.dir.join(name);
        if there(&path) {
            self.found.push(name.to_string());
            *slot = Some(path);
        }
    }

    /// An image of that name in the directory, as the medium it belongs to —
    /// or the `.xz` of it, which is read without being unpacked.
    fn image(&mut self, slot: &mut Option<Medium>, name: &str) {
        if slot.is_some() {
            return;
        }
        for name in [name.to_string(), format!("{name}.xz")] {
            let path = self.dir.join(&name);
            if path.is_file() {
                self.found.push(name);
                *slot = Some(Medium::Image(path));
                return;
            }
        }
    }

    /// A directory of boot-partition files rather than an image of one; `boot`
    /// builds the card around them (`pimu::fat`).
    fn boot_partition(&mut self, slot: &mut Option<Medium>) {
        let dir = if self.dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            self.dir
        };
        if pimu::fat::is_boot_partition(dir) {
            self.found.push(format!("{} as the card", dir.display()));
            *slot = Some(Medium::Files(dir.to_path_buf()));
        }
    }

    /// The name carries the format, so both present means an explicit `--otp` is needed.
    fn otp(&mut self) -> Result<Option<OtpFile>> {
        let mut file: Option<OtpFile> = None;
        for (name, format) in [("otp.json", Format::Json), ("otp.bin", Format::Binary)] {
            let path = self.dir.join(name);
            if !path.is_file() {
                continue;
            }
            if file.is_some() {
                bail!(
                    "otp.json and otp.bin are both here: say which with \
                     --otp json:<file> or --otp binary:<file>"
                );
            }
            self.found.push(name.to_string());
            file = Some(OtpFile { format, path });
        }
        Ok(file)
    }

    /// Every line of `bootconf.txt`, as `--bootconf` gives one. A `[section]`
    /// header stays, since the file the lines are appended to has those too.
    fn bootconf(&mut self) -> Result<Vec<String>> {
        let name = "bootconf.txt";
        let path = self.dir.join(name);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut lines = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let section = line.starts_with('[') && line.ends_with(']');
            if !section && !line.contains('=') {
                bail!(
                    "{}:{}: expected KEY=VALUE or [section], got '{line}'",
                    path.display(),
                    n + 1
                );
            }
            lines.push(line.to_string());
        }
        if !lines.is_empty() {
            self.found.push(name.to_string());
        }
        Ok(lines)
    }

    /// On stderr, to leave the serial console on stdout alone.
    fn announce(&self) {
        if !self.found.is_empty() {
            eprintln!("zero-config: {}", self.found.join(" "));
        }
    }
}

impl BootOpts {
    /// `boot`'s command line, without the command, or `None` for `--help`.
    /// `dir` is where [`ZeroConfig`] looks.
    #[cfg(test)]
    fn parse(args: &[String], dir: &Path) -> Result<Option<Self>> {
        use clap::CommandFactory;
        let argv = std::iter::once("boot").chain(args.iter().map(String::as_str));
        match BootArgs::command().try_get_matches_from(argv) {
            Err(e) if e.kind() == clap::error::ErrorKind::DisplayHelp => Ok(None),
            m => Self::from_matches(&m?, dir).map(Some),
        }
    }

    fn from_matches(m: &ArgMatches, dir: &Path) -> Result<Self> {
        let mut args = BootArgs::from_arg_matches(m)?;
        let mbox_tags = args.take_mbox_requests(m);
        Self::resolve(args, mbox_tags, dir)
    }

    fn resolve(a: BootArgs, mbox_tags: Vec<MboxRequest>, dir: &Path) -> Result<Self> {
        if let Some(flag) = a.trace_wanted().filter(|_| !pimu::diag::ON) {
            bail!("{flag} needs a build with the `diag` feature: cargo build --release --features diag");
        }
        let sends = a.send_after()?;
        let mut sd = match (&a.sd, &a.sd_dir) {
            (Some(s), _) => Some(Medium::from_arg(s)),
            (None, Some(s)) => Some(Medium::files_from_arg(s)),
            (None, None) => None,
        };
        let emmc = a.emmc.as_deref().map(Medium::from_arg);
        let mut usb = a.usb.as_deref().map(Medium::from_arg);
        let mut otg = a.otg.as_deref().map(Medium::from_arg);
        let mut tftp = a
            .tftp
            .as_deref()
            .map(|s| NetRoot::from_arg("--tftp", s))
            .transpose()?;
        let mut http = a
            .http
            .as_deref()
            .map(|s| NetRoot::from_arg("--http", s))
            .transpose()?;
        let flash = a
            .eeprom
            .as_deref()
            .map(|p| file_arg("--eeprom", p))
            .transpose()?;

        // A URL argument reads as the path one does: ending in `/` it is a
        // directory, and so the card the machine boots from — the EEPROM
        // bootloader for it comes from --eeprom or the fallback below, as it
        // does for a firmware checkout. Anything else is the file to boot,
        // fetched into the cache, and an image named like an EEPROM one is
        // booted as one.
        let mut eeprom = flash.is_some();
        let mut path = match a.target.as_deref() {
            Some(url) if pimu::remote::is_url(url) && url.ends_with('/') => {
                take_medium(&mut sd, "boot <url>", Medium::RemoteFiles(url.to_string()))?;
                None
            }
            Some(url) if pimu::remote::is_url(url) => {
                eeprom |= url
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.starts_with("pieeprom"));
                Some(file_arg("boot", url)?)
            }
            Some(file) => Some(PathBuf::from(file)),
            None => None,
        };
        // A directory to boot: read from there, as `-C` does, and it is itself
        // the card when it holds a boot partition's files.
        let from = match path.take_if(|p| p.is_dir()) {
            Some(p) => p,
            None => dir.to_path_buf(),
        };
        if let (Some(file), Some(flash)) = (&path, &flash) {
            bail!(
                "boot {} --eeprom {}: two images to boot. Give the EEPROM image to \
                 --eeprom alone, or a directory to read the rest from",
                file.display(),
                flash.display()
            );
        }
        path = path.or(flash);

        let mut zero = ZeroConfig::new(&from);
        if path.is_none() {
            zero.file(&mut path, "pieeprom.bin");
            eeprom |= path.is_some();
        }
        if emmc.is_none() {
            zero.image(&mut sd, "sd.img");
        }
        if sd.is_none() && emmc.is_none() {
            zero.boot_partition(&mut sd);
        }
        zero.image(&mut usb, "usb.img");
        zero.image(&mut otg, "otg.img");
        if a.net.is_none() {
            if tftp.is_none() {
                let mut found = None;
                zero.dir(&mut found, "tftp");
                tftp = found.map(NetRoot::Dir);
            }
            if http.is_none() {
                let mut found = None;
                zero.dir(&mut found, "http");
                http = found.map(NetRoot::Dir);
            }
        }
        let otp = match a.otp {
            Some(otp) => Some(otp),
            None => zero.otp()?,
        };
        let bootconf = match a.bootconf.is_empty() {
            true => zero.bootconf()?,
            false => a.bootconf,
        };
        let mut eeprom_pubkey = a
            .eeprom_pubkey
            .as_deref()
            .map(|p| file_arg("--eeprom-pubkey", p))
            .transpose()?;
        zero.file(&mut eeprom_pubkey, "pubkey.bin");
        zero.announce();

        let Some(path) = path else {
            bail!(
                "no EEPROM image to boot with: neither a firmware checkout nor a disk \
                 image is a bootloader. Give a pieeprom.bin with --eeprom, or keep one \
                 beside the files"
            );
        };
        if sd.is_some() && emmc.is_some() {
            bail!("--sd and --emmc are the same host: give one");
        }
        let usb_uas = match (a.usb_uas, a.usb_uas_refuse) {
            (_, true) => UsbUas::Refused,
            (true, false) => UsbUas::On,
            (false, false) => UsbUas::Off,
        };
        let usb_model = a.usb_model.unwrap_or_default();
        let usb_host_silent = a.usb_wedge.unwrap_or_default();
        if (usb_host_silent != HostSilent::Model || usb_model != StickModel::Generic)
            && usb.is_none()
        {
            bail!("--usb-model and --usb-wedge change the --usb stick: give --usb too");
        }
        if usb_model == StickModel::RpiFlashDrive && usb_uas == UsbUas::On {
            bail!("--usb-uas is for the generic stick: the Raspberry Pi USB Flash Drive does UAS already");
        }
        if usb_uas != UsbUas::Off && usb.is_none() {
            bail!("--usb-uas and --usb-uas-refuse change the --usb stick: give --usb too");
        }
        let card_edits = CardEdits {
            config: a.config_txt,
            cmdline: a.cmdline,
        };
        let files_somewhere = [&sd, &usb, &otg, &emmc]
            .into_iter()
            .flatten()
            .any(Medium::is_files);
        if !card_edits.is_empty() && !files_somewhere {
            bail!(
                "--config-txt and --cmdline edit a card built out of files: give a \
                 directory or a URL ending in `/` to --sd, --usb, --otg or --emmc, \
                 since a disk image is opaque"
            );
        }
        let mut log = Spec::default();
        for spec in &a.log {
            log.add(*spec).map_err(anyhow::Error::msg)?;
        }
        Ok(Self {
            path,
            snapshot_save: a.snapshot_save,
            snapshot_load: a.snapshot_load,
            entry: a.entry,
            usb_mb: a.usb_mb,
            otg_dock: a.otg_dock,
            usb_uas,
            usb_model,
            usb_host_silent,
            display: a.display || a.display_edid.is_some(),
            display_edid: a
                .display_edid
                .as_deref()
                .map(|p| file_arg("--display-edid", p))
                .transpose()?,
            max_steps: a.max_steps,
            max_wall_secs: a.max_wall.unwrap_or(140),
            speed: a.speed.map_or(Some(1.0), |s| s.0),
            eeprom,
            trace: a.trace || a.trace_full || a.trace_from.is_some(),
            trace_full: a.trace_full,
            trace_mmio: a.trace_mmio,
            exc_vbase: a.exc_vbase.unwrap_or(0),
            trace_from: a.trace_from.unwrap_or(0),
            smp: a.smp,
            parallel: a.parallel,
            as_core1: a.as_core1,
            patches: a.patch,
            dumps: a.dump,
            disasms: a.disasm,
            sd,
            card_edits,
            emmc,
            hat_eeprom: a.hat.as_deref().map(|p| file_arg("--hat", p)).transpose()?,
            check_coherency: a.check_coherency,
            check_alignment: a.check_alignment,
            jitter: a.jitter,
            faults: a.faults,
            console_log: a.console_log,
            dump_fdt: a.dump_fdt,
            dump_eeprom: a.dump_eeprom,
            print_fdt: a.print_fdt,
            mbox_tags,
            gencmds: a.gencmd,
            usb,
            otg,
            tftp,
            http,
            host_net: a.net,
            boot_order: a.boot_order,
            bootconf,
            eeprom_pubkey,
            maskrom_path: a
                .maskrom
                .as_deref()
                .map(|p| file_arg("--maskrom", p))
                .transpose()?,
            stepping: a.stepping,
            board_rev: a.board_rev,
            dram_map: a.dram_map,
            eeprom_map: a.eeprom_map,
            stub_log: a.stub_log,
            control_transfers: a.control_transfers,
            skip_signed_boot: a.skip_signed_boot,
            tryboot: a.tryboot,
            orderly_reboot: a.orderly_reboot,
            skip_unimpl: a.skip_unimpl,
            until: a.until,
            sends,
            magic_packet_after: a.magic_packet_after,
            stdin: a.stdin,
            verbose: a.verbose,
            quiet: a.quiet,
            log,
            log_file: a.log_file,
            otp,
            otp_rows: a.otp_row,
        })
    }
}

/// A finished boot: its last run and the machine that run left behind.
struct Booted {
    report: RunReport,
    emu: Emulator,
    start: u32,
    limits: RunLimits,
    reboots: u32,
    /// The fuses the first boot started with, to tell what the firmware programmed.
    fuses_at_start: BTreeMap<u32, u32>,
}

/// `boot`: `args` is its command line, `m` what the parser made of it.
pub fn cmd_boot(args: &[String], m: &ArgMatches) -> Result<ExitCode> {
    if let Some(flags) = crate::check::Flags::from_matches(m)? {
        return crate::check::run(&flags, &crate::check::extras(args));
    }
    let opts = BootOpts::from_matches(m, Path::new(""))?;
    pimu::log::warn_replaced_env();
    let booted = run_boot(&opts)?;
    print_report(&opts, booted)
}

/// Build the machine and run it. A firmware-requested reset rebuilds it from the
/// updated flash, up to four times.
fn run_boot(opts: &BootOpts) -> Result<Booted> {
    let image =
        std::fs::read(&opts.path).with_context(|| format!("reading {}", opts.path.display()))?;
    let log = open_log(opts)?;
    let usb_disk = open_usb_disk(&opts.usb, "usb", opts, &log)?;
    let otg_disk = open_usb_disk(&opts.otg, "otg", opts, &log)?;
    let edits = flash_edits(opts)?;

    let mut flash = image.clone();
    edits.apply(&mut flash, opts.verbose);
    if opts.eeprom && (opts.verbose || opts.eeprom_map) {
        print_eeprom(&flash, opts.eeprom_map);
    }

    let limits = run_limits(opts);
    // Made once: it owns the stdin reader and the terminal's raw mode.
    let mut host_input = opts.stdin.then(pimu::stdio::HostInput::stdin);
    let rig = Rig::new(opts, image, log, usb_disk, otg_disk)?;

    let mut reboots = 0u32;
    // A reset does not blank OTP: each boot starts with the rows the one before programmed.
    let mut carry = Carry::first(flash, load_otp(opts, rig.board)?);
    let mut fuses_at_start = None;
    let mut rebooted_orderly = false;
    // Sent in one boot, it is not owed to the next.
    let mut magic_packet_after = opts.magic_packet_after.clone();
    let (report, emu, start) = 'boot: loop {
        let mut machine = rig.machine(&carry.flash)?;
        // After the file, so `--otp-row` fuses a row whatever the run started from.
        carry.restore(&mut machine, &opts.otp_rows);
        // One-shot: the bootcode clears it as it reads it.
        if reboots == 0 && opts.tryboot {
            machine.pm.request_tryboot();
        }
        fuses_at_start.get_or_insert_with(|| machine.config_otp.fuses().clone());
        let start = rig.stage(&mut machine, reboots)?;
        let mut emu = rig.emulator(machine, start);
        emu.input.script = opts.sends.iter().cloned().collect();
        emu.input.magic_packet_after = magic_packet_after.clone();
        emu.input.host = host_input.take();
        let mut loaded_console = Vec::new();
        if let (0, Some(path)) = (reboots, &opts.snapshot_load) {
            loaded_console = pimu::snap::load(path, &mut emu)?;
            if emu.stream_console && !opts.quiet {
                print!("{}", String::from_utf8_lossy(&loaded_console));
            }
        }
        emu.stop_at_release = opts.snapshot_save.is_some();
        let mut report = emu.run(&limits);
        if !loaded_console.is_empty() {
            loaded_console.extend_from_slice(&report.console);
            report.console = loaded_console;
        }
        if let (pimu::emulator::RunEnd::Released, Some(path)) = (&report.end, &opts.snapshot_save) {
            pimu::snap::save(path, &mut emu, &report.console)?;
        }
        host_input = emu.input.host.take();
        if emu.input.magic_packet_after.is_none() {
            magic_packet_after = None;
        }
        let report = if opts.orderly_reboot
            && !rebooted_orderly
            && report.end == pimu::emulator::RunEnd::Until
        {
            rebooted_orderly = true;
            orderly_reboot(&mut emu, &limits)?
        } else {
            report
        };
        rig.dump_segment(&emu, &carry.flash, reboots);

        if report.end == pimu::emulator::RunEnd::Reset {
            reboots += 1;
            if !opts.quiet && (opts.verbose || !report.console_streamed) {
                print!("{}", String::from_utf8_lossy(&report.console));
            }
            if reboots <= 4 {
                // The next boot's ARM side starts a profile of its own.
                if let Some(a) = &mut emu.arm {
                    a.settle(&emu.machine);
                    if let Some(prof) = &a.prof {
                        println!("\n--- ARM cores before the reset ---");
                        print_arm_prof(prof);
                    }
                }
                println!("\n=== RESET (reboot {reboots}) — re-running from updated flash ===");
                // The flash with the self-update's change to `SIGNED_BOOT` made again.
                carry = Carry::leave(&mut emu.machine, &edits);
                if carry.expander().is_some_and(|x| !x.sd_powered()) {
                    println!(
                        "expander: SD_PWR_ON is driven low across the reset, the card has no power"
                    );
                }
                if carry.has_stick() {
                    println!("usb: the stick keeps its power and its state across the reset");
                }
                println!();
                continue 'boot;
            }
            println!("\n=== RESET (reboot {reboots}) — giving up after 4 reboots ===");
        }
        break 'boot (report, emu, start);
    };
    drop(host_input);
    if emu.machine.ram.coherency.is_on() {
        println!(
            "coherency: {} lines written through a cached alias, {} written by DMA, \
             {} read stale",
            emu.machine.ram.coherency.marks(),
            emu.machine.ram.coherency.dma_marks(),
            emu.machine.ram.coherency.reports()
        );
    }
    if pimu::jitter::is_on() {
        let (count, added, faults) = pimu::jitter::report();
        println!(
            "jitter: {count} intervals stretched, {} ms added in all, {faults} faults",
            added / 1000
        );
    }
    if emu.machine.alignment.is_on() {
        println!(
            "alignment: {} misaligned scalar accesses",
            emu.machine.alignment.reports()
        );
    }
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

/// What Linux's `reboot` does before the watchdog fires: the firmware is told
/// twice, the card's power is switched off through GPIO 134, then the PM
/// watchdog is armed for a full reset. Runs on until the reset lands.
fn orderly_reboot(emu: &mut Emulator, limits: &RunLimits) -> Result<RunReport> {
    use pimu::bus::{Bus, Width};

    const NOTIFY_REBOOT: u32 = 0x0003_0048;
    const SET_GPIO_STATE: u32 = 0x0003_8041;
    const CARD_POWER: u32 = 134;
    const ACTIVITY_LED: u32 = 130;
    let set_gpio = |gpio, state| (SET_GPIO_STATE, Some(8), vec![gpio, state]);
    let notify = || (NOTIFY_REBOOT, None, Vec::new());
    // The kernel's `mmc_rescan` on an empty slot: three power-up/power-down
    // pulses, so the last request before the notice leaves the card off.
    let rescan = (0..3).flat_map(|_| [set_gpio(CARD_POWER, 1), set_gpio(CARD_POWER, 0)]);
    const GET_CLOCK_RATE: u32 = 0x0003_0002;
    const SET_CLOCK_RATE: u32 = 0x0003_8002;
    const CORE_CLOCK: u32 = 4;
    let get_rate = || (GET_CLOCK_RATE, Some(8), vec![CORE_CLOCK, 0]);
    let set_rate = |hz| (SET_CLOCK_RATE, Some(12), vec![CORE_CLOCK, hz, 0]);
    // Captured on a Raspberry Pi 4B d03115 with a kprobe on
    // `rpi_firmware_property`, over the serial console, through `reboot`.
    let shutdown = [
        notify(),
        set_gpio(CARD_POWER, 0),
        set_gpio(ACTIVITY_LED, 1),
        get_rate(),
        set_rate(500_000_000),
        get_rate(),
        get_rate(),
        get_rate(),
        get_rate(),
        set_rate(200_000_000),
        get_rate(),
        get_rate(),
        notify(),
    ];
    for tag in rescan.chain(shutdown) {
        mbox_property_exchange(emu, limits, &MboxRequest::Tags(vec![tag]))?;
    }

    use pimu::spec::pm::{RSTC, RSTC_WRCFG_SHIFT, WDOG};
    const PASSWD: u32 = 0x5A00_0000;
    let base = pimu::soc::bcm2711::PM_BASE;
    let arm = |emu: &mut Emulator, reg, value| {
        emu.machine
            .store(base + reg, Width::Word, value)
            .map_err(|e| anyhow::anyhow!("arming the PM watchdog: {e}"))
    };
    arm(emu, WDOG, PASSWD | 10)?;
    arm(emu, RSTC, PASSWD | (2 << RSTC_WRCFG_SHIFT))?;

    let slice = RunLimits {
        until: None,
        max_wall: Some(std::time::Duration::from_secs(10)),
        idle_spin_limit: 0,
        silent_us: u64::MAX,
        speed: None,
        ..limits.clone()
    };
    Ok(emu.run(&slice))
}

/// The fuses a run before left. They go over the model's own rows rather than
/// replacing them, and row 30 is the revision code, so it has to be this board's.
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

/// Write the fuses back when the firmware programmed a row, or there is no file yet.
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

/// `--log` / `--log-file` (`src/log/`). Made once, so it spans the resets.
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

/// What kind of stick `--usb` is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UsbUas {
    Off,
    On,
    Refused,
}

type SharedUsbDisk = Rc<RefCell<pimu::periph::usb::Disk>>;

/// `--usb <img>`: a Bulk-Only Transport mass-storage device on xHCI root port 2
/// (the socket map is in [`crate::periph::xhci`]); `--otg <img>` is the same
/// device on the BCM2711's own xHCI. `--usb-mb <n>` sizes the stick, image at its
/// start. Read on demand; what the guest writes outlives the resets.
fn open_usb_disk(
    medium: &Option<Medium>,
    what: &'static str,
    opts: &BootOpts,
    log: &Log,
) -> Result<Option<SharedUsbDisk>> {
    Ok(match medium {
        Some(medium) => {
            let disk = medium.disk(&opts.card_edits, opts.usb_mb.unwrap_or(0) << 20, log, what)?;
            if opts.verbose {
                println!("{what} medium  {medium} ({} blocks)", disk.blocks());
            }
            Some(std::rc::Rc::new(std::cell::RefCell::new(disk)))
        }
        None => None,
    })
}

fn flash_edits(opts: &BootOpts) -> Result<FlashEdits> {
    let pubkey = match &opts.eeprom_pubkey {
        Some(p) => Some(std::fs::read(p).with_context(|| format!("reading {}", p.display()))?),
        None => None,
    };
    FlashEdits::new(
        opts.eeprom,
        opts.skip_signed_boot,
        opts.boot_order.as_deref(),
        &opts.bootconf,
        opts.usb.is_some(),
        pubkey,
    )
    .with_context(|| match &opts.eeprom_pubkey {
        Some(p) => format!("--eeprom-pubkey {}", p.display()),
        None => "the EEPROM edits".to_string(),
    })
}

/// The decoded `bootconf.txt`, and with `--eeprom-map` the section table
/// `bootloader_eeprom_find_files` walks.
fn print_eeprom(flash: &[u8], map: bool) {
    let Ok(img) = pimu::firmware::eeprom::EepromImage::parse(flash) else {
        return;
    };
    if map {
        println!("eeprom     {} sections", img.sections.len());
        print!("{}", img.summary());
    }
    let Some(conf) = img.bootconf() else { return };
    let mut label = !map;
    let mut line = |text: String| {
        let tag = if std::mem::take(&mut label) {
            "eeprom    "
        } else {
            "          "
        };
        println!("{tag} {text}");
    };
    for (g, k, v) in &conf.entries {
        let g = if g.is_empty() { "all" } else { g.as_str() };
        line(format!("[{g}] {k}={v}"));
    }
    if let Some(order) = conf.boot_order_names() {
        line(format!("BOOT_ORDER: {order}"));
    }
}

fn run_limits(opts: &BootOpts) -> RunLimits {
    let BootOpts {
        max_steps,
        max_wall_secs,
        speed,
        stdin,
        ref until,
        ..
    } = *opts;
    RunLimits {
        max_steps,
        max_wall: (!stdin).then(|| std::time::Duration::from_secs(max_wall_secs)),
        stop_pc: None,
        idle_spin_limit: 200_000,
        // A minute of modelled silence. The worst legitimate gap is the kernel
        // load, about thirteen seconds, so a wedge reports long before the wall clock.
        silent_us: if stdin { 0 } else { 60_000_000 },
        until: until.clone(),
        speed,
    }
}

/// What every boot of the run shares. Made once, so it outlives the resets.
struct Rig<'a> {
    opts: &'a BootOpts,
    image: Vec<u8>,
    log: Log,
    usb_disk: Option<SharedUsbDisk>,
    otg_disk: Option<SharedUsbDisk>,
    bootrom: pimu::firmware::bootrom::BootRom,
    maskrom_image: Option<Vec<u8>>,
    board: Board,
}

impl<'a> Rig<'a> {
    fn new(
        opts: &'a BootOpts,
        image: Vec<u8>,
        log: Log,
        usb_disk: Option<SharedUsbDisk>,
        otg_disk: Option<SharedUsbDisk>,
    ) -> Result<Self> {
        let BootOpts {
            ref maskrom_path,
            stepping,
            board_rev,
            verbose,
            ..
        } = *opts;

        // The model's first stage for an EEPROM boot (`firmware::bootrom`). Its
        // HMAC key, when supplied, comes from the environment and never the repo.
        let bootrom = pimu::firmware::bootrom::BootRom::from_env()?;

        // A real maskROM dump at `0x6000_0000`, executed from the reset vector
        // instead of the behavioural stage. The dump is never committed.
        let maskrom_image = match &maskrom_path {
            Some(p) => {
                let b =
                    std::fs::read(p).with_context(|| format!("reading maskROM {}", p.display()))?;
                if verbose {
                    println!(
                        "maskrom    {} ({} bytes, experimental)",
                        p.display(),
                        b.len()
                    );
                }
                Some(b)
            }
            None => None,
        };

        // Naming only one of `--stepping` / `--board-rev` gets a board that fits it.
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
            otg_disk,
            bootrom,
            maskrom_image,
            board,
        })
    }

    fn machine(&self, flash: &[u8]) -> Result<Machine> {
        let BootOpts {
            eeprom,
            ref sd,
            ref emmc,
            ref hat_eeprom,
            check_coherency,
            check_alignment,
            jitter,
            faults,
            ref tftp,
            ref http,
            ref host_net,
            trace_mmio,
            ..
        } = *self.opts;
        // The EEPROM bootloader also touches the `0x6000_0000` L2-SRAM window,
        // which the model folds into DRAM past 512 MiB — room every board has.
        let mut machine = Machine::new(self.board.memory_bytes());
        machine.set_board(self.board);
        machine.set_log(self.log.clone());
        if eeprom {
            machine.spi0.attach_flash(flash.to_vec())?;
        }
        // The card reads its image or its files on demand. After a reset
        // `Carry::restore` puts the card of the boot before in its place.
        if let Some(sd) = &sd {
            let disk = sd.disk(&self.opts.card_edits, 0, &self.log, "sd")?;
            machine.emmc2.insert_disk(disk);
        }
        if let Some(emmc) = &emmc {
            let disk = emmc.disk(&self.opts.card_edits, 0, &self.log, "emmc")?;
            machine.emmc2.insert_mmc_disk(disk);
        }
        if self.opts.display {
            // A monitor on HDMI0. Not the same lever as `hdmi_force_hotplug=1`
            // — see `Hdmi::with_display`.
            let edid = match &self.opts.display_edid {
                Some(p) => {
                    let blob = std::fs::read(p)
                        .with_context(|| format!("--display-edid {}", p.display()))?;
                    if blob.len() != 128 && blob.len() != 256 {
                        anyhow::bail!(
                            "--display-edid {}: {} bytes, expected 128 or 256",
                            p.display(),
                            blob.len()
                        );
                    }
                    blob
                }
                None => pimu::periph::hdmi_ddc::DEFAULT_EDID.to_vec(),
            };
            machine.hdmi0 = pimu::periph::Hdmi::new("hdmi0").with_display();
            machine.hdmi_ddc0 = pimu::periph::HdmiDdc::new("hdmi-ddc0").with_edid(edid);
        }
        if check_coherency {
            *machine.ram.coherency = pimu::coherency::Coherency::on(self.log.clone());
        }
        if check_alignment {
            machine.alignment = pimu::align::Alignment::on(self.log.clone());
        }
        if let Some(seed) = jitter {
            pimu::jitter::arm(seed, self.log.clone());
            if let Some(one_in) = faults {
                pimu::jitter::set_faults(one_in);
            }
        }
        if let Some(p) = &hat_eeprom {
            let bytes = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
            machine
                .bsc0
                .attach_eeprom(pimu::periph::hat::HatEeprom::new(bytes));
        }
        if let Some(disk) = &self.usb_disk {
            use pimu::periph::usb::{MassStorage, UasStorage};
            let stick: Box<dyn pimu::periph::usb::UsbDevice> =
                match (self.opts.usb_model, self.opts.usb_uas) {
                    (StickModel::RpiFlashDrive, UsbUas::Refused) => {
                        Box::new(UasStorage::rpi_flash_drive(disk.clone()).refusing_uas())
                    }
                    (StickModel::RpiFlashDrive, _) => {
                        Box::new(UasStorage::rpi_flash_drive(disk.clone()))
                    }
                    (StickModel::Generic, UsbUas::Off) => {
                        Box::new(MassStorage::with_disk(disk.clone()))
                    }
                    (StickModel::Generic, UsbUas::On) => {
                        Box::new(UasStorage::with_disk(disk.clone()))
                    }
                    (StickModel::Generic, UsbUas::Refused) => {
                        Box::new(UasStorage::with_disk(disk.clone()).refusing_uas())
                    }
                };
            machine.pcie.endpoint.attach(USB_ROOT_PORT, stick);
            if let Some(limit_us) = self.opts.usb_host_silent.limit_us(self.opts.usb_model) {
                machine.pcie.endpoint.xhci.set_host_silent_limit(limit_us);
            }
        }
        // The BCM2711's own xHCI: what `BOOT_ORDER` digit `0x5` boots from and
        // what `otg_mode=1` gives Linux.
        if let Some(disk) = &self.otg_disk {
            let stick = pimu::periph::usb::MassStorage::with_disk_hs(disk.clone());
            let device: Box<dyn pimu::periph::usb::UsbDevice> = if self.opts.otg_dock {
                Box::new(pimu::periph::usb::Hub::dock(stick))
            } else {
                Box::new(stick)
            };
            machine.xhci_otg.attach(device);
        }
        // The built-in peer (`src/net/peer.rs`): DHCP, DNS, TFTP and HTTP.
        if tftp.is_some() || http.is_some() || self.opts.magic_packet_after.is_some() {
            let mut peer = pimu::net::BuiltinPeer::new().with_log(self.log.clone());
            if let Some(root) = tftp {
                peer = peer.tftp_root(&root.peer_root("TFTP"));
            }
            if let Some(root) = http {
                peer = peer.http_root(&root.peer_root("HTTP"));
            }
            machine.attach_net(Box::new(peer));
        }
        // A new connection (and a new passt) per boot, like a cable replugged.
        if let Some(host_net) = &host_net {
            let net = match host_net {
                HostNet::Passt => pimu::net::StreamBackend::spawn_passt().map_err(|e| {
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
                HostNet::Socket(sock) => pimu::net::StreamBackend::connect(sock)
                    .with_context(|| format!("connecting to {}", sock.display()))?,
            };
            machine.attach_net(Box::new(net));
        }
        machine.mmio_trace = trace_mmio;
        // Only inside that address range: tracing the whole bus across a boot is
        // unusable, in volume and in formatting cost alike.
        if let Some((lo, hi)) = std::env::var("PIMU_TRACE_MMIO")
            .ok()
            .and_then(|v| parse_addr_range(&v))
        {
            machine.mmio_trace = true;
            machine.mmio_trace_range = Some((lo, hi));
        }
        Ok(machine)
    }

    /// Stage the first instruction stream, apply the `--patch`es, and say where to start.
    fn stage(&self, machine: &mut Machine, reboots: u32) -> Result<u32> {
        let BootOpts {
            eeprom,
            entry,
            ref patches,
            verbose,
            ..
        } = *self.opts;
        // The modelled boot ROM talks to the peripherals the real one does rather
        // than reaching around them; for a raw ELF this is just segment placement.
        let start = if eeprom {
            if let Some(rom) = &self.maskrom_image {
                // The real maskROM stages the bootcode itself off SPI0 and OTP.
                machine.attach_maskrom(rom.clone());
                if reboots == 0 && verbose {
                    println!("maskROM: executing from reset vector 0x60000000 (experimental)");
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
            use pimu::bus::Bus;
            machine.store32(a, v).ok();
            if verbose {
                println!("patch [{a:#010x}] = {v:#010x}");
            }
        }
        Ok(start)
    }

    fn emulator(&self, machine: Machine, start: u32) -> Emulator {
        let BootOpts {
            skip_unimpl,
            trace,
            trace_full,
            exc_vbase,
            trace_from,
            as_core1,
            smp,
            parallel,
            quiet,
            ..
        } = *self.opts;
        let mut emu = Emulator::new(machine, start);
        emu.stream_console = !quiet;
        emu.parallel_arm = parallel;
        // Faulting is the default: silently stepping over an unknown instruction
        // makes the firmware quietly not do whatever it was for.
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

    fn dump_segment(&self, emu: &Emulator, flash: &[u8], reboots: u32) {
        // The self-update-modified EEPROM image after every run segment, as
        // `<path>.<n>`, so a run that stops before RESET still yields the burned
        // image. Feed it back as `boot <path>.<n> --eeprom`.
        if self.opts.eeprom {
            if let Ok(p) = std::env::var("PIMU_DUMP_FLASH") {
                let cur = emu.machine.spi0.flash_bytes();
                if cur != flash {
                    let _ = std::fs::write(format!("{p}.{}", reboots + 1), cur);
                    eprintln!("wrote {p}.{} ({} bytes)", reboots + 1, cur.len());
                }
            }
        }
        // SDRAM the same way, before a reset replaces it: a kernel that dies
        // before its console comes up still has its log buffer in there.
        if let Ok(p) = std::env::var("PIMU_DUMP_RAM") {
            let ram = &emu.machine.ram;
            let path = format!("{p}.{}", reboots + 1);
            let _ = write_ram(ram, &path);
            eprintln!("wrote {path} ({} bytes)", ram.len());
        }
    }
}

/// What `boot` prints once the run is over.
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

    // A core parked in a busy-wait loop is behind until it is brought up to date.
    if let Some(a) = &mut emu.arm {
        a.settle(&emu.machine);
    }
    if let Some(path) = &opts.dump_eeprom {
        std::fs::write(path, emu.machine.spi0.flash_bytes())
            .with_context(|| format!("--dump-eeprom {}", path.display()))?;
    }
    // Located once: the device-state section and `report_fdt` read the same bytes.
    let handoff = emu.arm.as_ref().and_then(|a| a.handoff);
    let fdt_blob = locate_fdt(&mut emu.machine, handoff, &report.console);
    if verbose {
        print_summary(&report, &emu, start);
        print_arm_cores(&emu, opts.eeprom);
        print_property_replies(&emu.machine);
        print_device_state(&emu.machine, fdt_blob.as_ref().map(|(_, b)| b.as_slice()));
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
    if opts.control_transfers {
        print_control_transfers(&emu);
    }
    if opts.stub_log {
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
    if !opts.gencmds.is_empty() {
        crate::vchiq::gencmd_exchange(&mut emu, &limits, &opts.gencmds)?;
    }
    if let Some(file) = &opts.otp {
        save_otp(file, &fuses_at_start, emu.machine.config_otp.fuses())?;
    }
    report_fdt(opts, fdt_blob)?;
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
    // Left out of an unpaced run, whose golden logs would otherwise all gain a line.
    if !report.slept.is_zero() {
        println!("paced      {:?} asleep (--speed)", report.slept);
    }
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

/// The ARM side: the hand-off, each core, and where it stopped.
fn print_arm_cores(emu: &Emulator, eeprom: bool) {
    if let Some(a) = &emu.arm {
        println!("\n--- ARM cores ---");
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
            println!(
                "            TLB: {} hits, {} walks, {} flushes ({} for a translation register), {} pages dropped",
                c.cpu.tlb.hits,
                c.cpu.tlb.walks,
                c.cpu.tlb.flushes,
                c.cpu.tlb.reshaped,
                c.cpu.tlb.drops
            );
            if c.sha_blocks > 0 {
                println!(
                "            {} SHA-256 block loop(s), {} blocks hashed natively (PIMU_NO_SHA_SKIP=1 to compare)",
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
        #[cfg(feature = "jit")]
        if let Some(jit) = &a.jit {
            println!(
                "  jit       {} runs compiled in {:.2} s, entered {} times, {} instructions retired in them; of their instructions {} inline, {} through handlers (PIMU_NO_JIT=1 to compare)",
                jit.runs,
                jit.compile_ns as f64 / 1e9,
                jit.entries,
                jit.retired,
                jit.inline,
                jit.called
            );
        }
        print_arm_blocks(&a.cores);
    } else if eeprom {
        println!("\n--- ARM cores ---\n  never released");
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
            let errors = if t.errors > 0 {
                format!("  errors {}", t.errors)
            } else {
                String::new()
            };
            println!(
                "  tag {tag:#010x}  marked {:<4} unmarked {:<4} last value {last}{errors}",
                t.marked, t.unmarked
            );
        }
    }
}

/// What the devices hold where the run ended, decoded. Neither the golden
/// transcript nor the retired counts see a value a driver wrote into a register
/// and never mentioned; a device belongs here once it holds a value worth a diff.
fn print_device_state(machine: &Machine, fdt: Option<&[u8]>) {
    use pimu::periph::bluetooth::{format_bd_address, published_bd_address};
    use pimu::periph::sdpcm::MacSource;

    let mac = machine.genet.mac_state();
    println!("\n--- device state ---");
    println!("  emmc2   {}", machine.emmc2.describe());
    let [a, b, c, d, e, f] = mac.addr;
    println!(
        "  genet   MAC {a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}  tx {}  rx {}  promisc {}",
        on_off(mac.tx_en),
        on_off(mac.rx_en),
        on_off(mac.promisc),
    );

    // Two addresses: the chip's own until a host writes the board's over it. Neither
    // reaches the console, so a bump that moves the derivation is otherwise invisible.
    let bt = machine.bluetooth.bd_addr();
    println!(
        "  bt      chip {}, {}",
        format_bd_address(bt),
        if machine.bluetooth.bd_addr_written() {
            "written by the host"
        } else {
            "as it came up"
        },
    );
    let published = fdt
        .and_then(|blob| pimu::fdt::Fdt::parse(blob).ok())
        .map(|fdt| published_bd_address(&fdt));
    match published {
        Some(p) => match p.enabled {
            Some((path, addr)) => println!(
                "          device tree {} on {path}",
                format_bd_address(addr)
            ),
            None => println!(
                "          device tree: no bluetooth node enabled ({} disabled)",
                p.nodes
            ),
        },
        None => println!("          device tree: none handed over"),
    }

    // A third address from a third place: the chip's own, until the card's nvram
    // `macaddr=` overrides it and until a host writes one over that.
    if let Some(chip) = machine.emmc.card().and_then(|card| card.chip()) {
        let [a, b, c, d, e, f] = chip.sdpcm().mac();
        println!(
            "  cyw43455 MAC {a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}, {}",
            match chip.sdpcm().mac_source() {
                MacSource::Otp => "as it came up",
                MacSource::Nvram => "from the card's nvram",
                MacSource::Host => "written by the host",
            },
        );
        print_wifi_events(chip.sdpcm());
    }
}

/// Which firmware events the host asked the WiFi chip for, and what the chip did.
/// The mask never reaches the console, and it is the whole of what the chip may
/// say unasked — so a bring-up that stopped asking is invisible without this.
fn print_wifi_events(chip: &pimu::periph::sdpcm::Sdpcm) {
    use pimu::periph::sdpcm::EventMaskSource;

    let wanted = chip.events_wanted();
    println!(
        "           events {} of {} wanted, {}",
        wanted.len(),
        chip.event_mask().len() * 8,
        match chip.event_mask_source() {
            EventMaskSource::Firmware => "and nothing has set the mask",
            EventMaskSource::EventMsgs => "last set with `event_msgs`",
            EventMaskSource::EventMsgsExt => "last set with `event_msgs_ext`",
        },
    );
    for line in wanted.chunks(16) {
        let codes: Vec<String> = line.iter().map(|c| c.to_string()).collect();
        println!("                  {}", codes.join(" "));
    }
    println!(
        "           events {} sent, {} dropped as unwanted; {} frames in on the data channel",
        chip.events_sent(),
        chip.events_dropped(),
        chip.data_frames_in(),
    );
    // A scan is an iovar in and events out: one answered with nothing looks
    // exactly like one the chip never saw.
    let found = chip.escan_results();
    println!(
        "           scans {} answered, {found} network{} reported",
        chip.escans(),
        if found == 1 { "" } else { "s" },
    );
}

fn on_off(v: bool) -> &'static str {
    if v {
        "on"
    } else {
        "off"
    }
}

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

/// The UART bytes on their own, which `boot --scenario` normalises into the golden
/// transcript — picking them out of the combined log afterwards would be
/// guesswork. On a run that rebooted, the last segment only.
fn write_console_log(p: &Path, console: &[u8], verbose: bool) -> Result<()> {
    std::fs::write(p, console).with_context(|| format!("writing console log {}", p.display()))?;
    if verbose {
        println!("console log {} ({} bytes)", p.display(), console.len());
    }
    Ok(())
}

fn print_network(machine: &mut Machine) {
    if let Some(net) = machine.net.as_mut() {
        let st = machine.genet.stats;
        println!("\n--- network (GENET <-> {}) ---", net.name());
        println!(
            "  tx {} (dropped {})  rx {} (filtered {}, dropped {})",
            st.tx, st.tx_dropped, st.rx, st.rx_filtered, st.rx_dropped
        );
        if st.rx_wake != 0 {
            println!("  magic packets detected {}", st.rx_wake);
        }
        for line in net.take_log() {
            println!("  {line}");
        }
    }
}

fn print_console(report: &RunReport, verbose: bool) {
    if !report.console.is_empty() && verbose {
        println!("\n--- console ({} bytes) ---", report.console.len());
        if report.console_streamed {
            println!("(streamed above; PIMU_LIVE_CONSOLE=0 to buffer it here instead)");
        } else {
            println!("{}", String::from_utf8_lossy(&report.console));
        }
    } else if !report.console_streamed {
        print!("{}", String::from_utf8_lossy(&report.console));
    }
}

fn print_dump(machine: &mut Machine, a: u32, n: u32) {
    use pimu::bus::Bus;
    print!("dump {a:#010x}:");
    for i in 0..n {
        if i % 32 == 0 {
            print!("\n  {:#010x} ", a + i);
        }
        print!(
            "{:02x}",
            machine.load(a + i, pimu::bus::Width::Byte).unwrap_or(0) as u8
        );
    }
    println!();
}

fn print_disasm(machine: &mut Machine, a: u32, count: u32) {
    use pimu::bus::Bus;
    println!("disasm {a:#010x}:");
    let mut pc = a;
    let mut buf = [0u8; 10];
    for _ in 0..count {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = machine
                .load(pc + i as u32, pimu::bus::Width::Byte)
                .unwrap_or(0) as u8;
        }
        let len = insn_len_bytes(u16::from_le_bytes([buf[0], buf[1]])) as usize;
        let insn = decode(&buf[..len], pc);
        let hex: String = buf[..len].iter().map(|b| format!("{b:02x}")).collect();
        println!("  {pc:#010x}:  {hex:<20}  {:?}", insn.op);
        pc = pc.wrapping_add(len as u32);
    }
}

/// The boot-progress tags start4 writes (`0xcec02000`), as text.
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

/// The VPU's last control transfers, newest first. Off by default, `-v` included:
/// only a derailed boot has a use for them.
fn print_control_transfers(emu: &Emulator) {
    // Collapse repeats so a spin doesn't hide the history that led into it.
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

/// The peripheral-window offsets nothing models. Off by default, `-v` included:
/// the run report's `stub=` count is all a passing boot needs.
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

/// The whole DRAM into `path`, zeros and all.
fn write_ram(ram: &pimu::mem::Ram, path: &str) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    for (_, region) in ram.regions() {
        match region {
            Region::Bytes(b) => out.write_all(b)?,
            Region::Zeros(n) => {
                std::io::copy(&mut std::io::repeat(0).take(n as u64), &mut out)?;
            }
        }
    }
    out.flush()
}

/// Which DRAM pages are non-zero when the run ends: the RAM a snapshot would have
/// to carry. The model starts RAM zeroed, so "not all zero" is exact for that.
fn print_dram_map(machine: &Machine) {
    const PAGE: usize = 4096;
    let ram = &machine.ram;
    let base = u64::from(ram.base());
    let mut runs: Vec<(u64, u64)> = Vec::new();
    let mut nonzero_pages = 0usize;
    let total_pages = ram.len() / PAGE as u64;
    for (off, region) in ram.regions() {
        let Region::Bytes(bytes) = region else {
            continue;
        };
        for (i, page) in bytes.chunks(PAGE).enumerate() {
            if page.iter().all(|&b| b == 0) {
                continue;
            }
            let addr = base + off + (i * PAGE) as u64;
            nonzero_pages += 1;
            match runs.last_mut() {
                // Bridge gaps of up to 64 KiB; their zero bytes are counted separately.
                Some(last) if addr <= last.1 + 0x1_0000 => last.1 = addr + PAGE as u64,
                _ => runs.push((addr, addr + PAGE as u64)),
            }
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

/// The DRAM refresh interval start4 rescales from the LPDDR4 MR4 code once the ARM
/// is running. It logs the change after handing the UART to Linux, so only the
/// controller state can be checked.
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

/// The device tree `arm_loader` leaves for the ARM, and the `/chosen` identity
/// properties it patched in: a firmware bump that moves one has to be caught
/// here rather than on a deployed card. The blob comes from [`locate_fdt`], and
/// its header is validated before anything is believed.
fn report_fdt(opts: &BootOpts, located: Option<(u32, Vec<u8>)>) -> Result<()> {
    let BootOpts {
        verbose,
        print_fdt,
        ref dump_fdt,
        ..
    } = *opts;
    match located {
        Some((addr, blob)) => {
            match pimu::fdt::Fdt::parse(&blob) {
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
                        // `/chosen` is what the regression pins; the subject is the
                        // whole tree, which `--print-fdt` / `--dump-fdt` give.
                        match fdt.properties_of("/chosen") {
                            Some(props) => {
                                for p in &props {
                                    // The kernel command line can be long.
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
            "\n(disassemble any of these with:  pimu disasm {} --vaddr <pc> --count 1)",
            path.display()
        );
    }
}

/// `PIMU_ARM_PROF`'s table: the hottest ARM steps by core, EL and PC bucket.
fn print_arm_prof(prof: &std::collections::HashMap<(usize, u32, u64), u64>) {
    let total: u64 = prof.values().sum();
    let mut v: Vec<_> = prof.iter().collect();
    v.sort_by_key(|(_, &n)| std::cmp::Reverse(n));
    println!("  PIMU_ARM_PROF: steps by core, EL and 256-byte PC bucket (total {total})");
    for ((core, el, pc), &n) in v.into_iter().take(30) {
        println!(
            "    core {core} EL{el} {pc:#014x}  {n:>13}  {:5.1}%",
            100.0 * n as f64 / total as f64
        );
    }
}

/// `PIMU_ARM_BLOCKS`'s table: straight-line run lengths and re-entry counts.
fn print_arm_blocks(cores: &[pimu::arm::Core]) {
    use pimu::arm::blocks::Blocks;
    let mut all = Blocks::default();
    for c in cores {
        if let Some(b) = &c.blocks {
            all.merge(b);
        }
    }
    if all.insns == 0 {
        return;
    }
    println!(
        "  PIMU_ARM_BLOCKS: {} instruction(s) in {} straight-line run(s), {} distinct, \
         mean {:.1} instruction(s) per run, {} exception cut(s)",
        all.insns,
        all.runs,
        all.distinct(),
        all.insns as f64 / all.runs.max(1) as f64,
        all.cuts,
    );
    print!("    instructions in runs of at least");
    for len in [2, 4, 8, 16, 32, 64] {
        print!("  {len}: {:.1}%", all.insns_in_runs_of(len));
    }
    println!();
    print!("    instructions in runs entered at least");
    for n in [2, 10, 100, 10_000] {
        print!("  {n}x: {:.1}%", all.insns_in_runs_entered(n));
    }
    println!();
    println!("    the runs covering the most instructions:");
    for ((pa, el), entries, insns) in all.hottest().into_iter().take(10) {
        println!(
            "      EL{el} {pa:#014x}  {entries:>12} entries  {insns:>13} insns  {:5.1}%",
            100.0 * insns as f64 / all.insns as f64
        );
    }
    println!("    the commonest run lengths, by instructions covered:");
    for (len, runs, insns) in all.by_length().into_iter().take(10) {
        println!(
            "      {len:>6} insn(s)  {runs:>12} run(s)  {insns:>13} insns  {:5.1}%",
            100.0 * insns as f64 / all.insns as f64
        );
    }
}

/// `<lo>-<hi>` (hex) as a half-open range; anything else, the bare `1` that arms
/// a `PIMU_TRACE_ON_*` trigger included, yields `None`.
fn parse_addr_range(s: &str) -> Option<(u32, u32)> {
    let (lo, hi) = s.trim().split_once('-')?;
    let p = |t: &str| u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok();
    let (lo, hi) = (p(lo)?, p(hi)?);
    (lo < hi).then_some((lo, hi))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;
    use crate::boot_args::parse_host_silent;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn help_wherever_an_option_can_go() {
        assert!(BootOpts::parse(&args(&["--help"]), Path::new(""))
            .unwrap()
            .is_none());
        assert!(
            BootOpts::parse(&args(&["--eeprom", "x.bin", "-h"]), Path::new(""))
                .unwrap()
                .is_none()
        );
        assert!(
            BootOpts::parse(&args(&["x.elf", "--until", "--help"]), Path::new(""))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_magic_packet_is_asked_for_by_the_text_that_precedes_it() {
        let parse = |v: &[&str]| BootOpts::parse(&args(&[&["x.elf"], v].concat()), Path::new(""));
        let opts = parse(&["--magic-packet-after", "WoL armed"])
            .unwrap()
            .unwrap();
        assert_eq!(opts.magic_packet_after.as_deref(), Some("WoL armed"));
        assert!(parse(&[]).unwrap().unwrap().magic_packet_after.is_none());
        assert!(parse(&["--magic-packet-after"]).is_err());
        let err = parse(&["--magic-packet-after", "x", "--net", "passt"])
            .err()
            .expect("--net has no peer to send it")
            .to_string();
        assert!(err.contains("--net"), "{err}");
    }

    #[test]
    fn speed_is_real_time_unless_told_otherwise() {
        let speed = |v: &[&str]| {
            BootOpts::parse(&args(&[&["x.elf"], v].concat()), Path::new(""))
                .map(|o| o.unwrap().speed)
        };
        assert_eq!(speed(&[]).unwrap(), Some(1.0));
        assert_eq!(speed(&["--speed", "max"]).unwrap(), None);
        assert_eq!(speed(&["--speed", "2.5"]).unwrap(), Some(2.5));
        for bad in [&["--speed", "0"], &["--speed", "-1"], &["--speed", "fast"]] {
            assert!(speed(bad).is_err(), "--speed {} took", bad[1]);
        }
    }

    /// Named after the test, so two never share one.
    fn zero_dir(what: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pimu-zero-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn zero_config_takes_the_files_the_command_line_left_out() {
        let dir = zero_dir("all");
        for name in ["pieeprom.bin", "sd.img", "usb.img", "otg.img", "pubkey.bin"] {
            put(&dir, name, "");
        }
        put(&dir, "otp.json", "{}");
        put(
            &dir,
            "bootconf.txt",
            "# a comment\n\n[all]\nBOOT_ORDER=0xf41  # trailing\n",
        );
        std::fs::create_dir_all(dir.join("tftp")).unwrap();
        std::fs::create_dir_all(dir.join("http")).unwrap();

        let o = BootOpts::parse(&args(&[]), &dir).unwrap().unwrap();
        assert_eq!(o.path, dir.join("pieeprom.bin"));
        assert!(o.eeprom, "pieeprom.bin is an EEPROM image");
        assert_eq!(o.sd, Some(Medium::Image(dir.join("sd.img"))));
        assert_eq!(o.usb, Some(Medium::Image(dir.join("usb.img"))));
        assert_eq!(o.otg, Some(Medium::Image(dir.join("otg.img"))));
        assert_eq!(o.tftp, Some(NetRoot::Dir(dir.join("tftp"))));
        assert_eq!(o.http, Some(NetRoot::Dir(dir.join("http"))));
        assert_eq!(o.eeprom_pubkey, Some(dir.join("pubkey.bin")));
        assert_eq!(
            o.otp,
            Some(OtpFile {
                format: Format::Json,
                path: dir.join("otp.json"),
            })
        );
        assert_eq!(o.bootconf, vec!["[all]", "BOOT_ORDER=0xf41"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `--otp-row` fuses a row the model leaves blank: it is a board out of the
    /// factory, so the device private key in 56-63 is not there until a run asks.
    #[test]
    fn otp_row_takes_a_row_and_a_value() {
        let dir = zero_dir("rows");
        put(&dir, "pieeprom.bin", "");
        let o = BootOpts::parse(
            &args(&["--otp-row", "56=0x52504956", "--otp-row", "63=1"]),
            &dir,
        )
        .unwrap()
        .unwrap();
        assert_eq!(o.otp_rows, vec![(56, 0x5250_4956), (63, 1)]);

        for (bad, says) in [("56", "ROW=VALUE"), ("56=nope", "--otp-row")] {
            let Err(e) = BootOpts::parse(&args(&["--otp-row", bad]), &dir) else {
                panic!("--otp-row {bad} is not a row and a value")
            };
            assert!(e.to_string().contains(says), "{e:#}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_command_line_wins_over_the_files_beside_it() {
        let dir = zero_dir("given");
        for name in ["pieeprom.bin", "sd.img", "otp.bin"] {
            put(&dir, name, "");
        }
        put(&dir, "bootconf.txt", "HTTP_HOST=files\n");

        let o = BootOpts::parse(
            &args(&[
                "given.elf",
                "--sd",
                "given.img",
                "--otp",
                "json:given.json",
                "--bootconf",
                "HTTP_HOST=given",
            ]),
            &dir,
        )
        .unwrap()
        .unwrap();
        assert_eq!(o.path, PathBuf::from("given.elf"));
        assert!(!o.eeprom, "an ELF given by name is not an EEPROM boot");
        assert_eq!(o.sd, Some(Medium::Image(PathBuf::from("given.img"))));
        assert_eq!(o.otp.unwrap().path, PathBuf::from("given.json"));
        assert_eq!(o.bootconf, vec!["HTTP_HOST=given"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Filling one in from the directory would make a command line `parse` refuses.
    #[test]
    fn an_option_keeps_the_one_it_rules_out_from_being_picked_up() {
        let dir = zero_dir("exclusive");
        put(&dir, "sd.img", "");
        std::fs::create_dir_all(dir.join("tftp")).unwrap();

        let o = BootOpts::parse(&args(&["x.elf", "--emmc", "e.img", "--net", "passt"]), &dir)
            .unwrap()
            .unwrap();
        assert_eq!(o.sd, None);
        assert_eq!(o.tftp, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn both_otp_formats_at_once_asks_for_an_explicit_otp() {
        let dir = zero_dir("otp");
        put(&dir, "otp.json", "{}");
        put(&dir, "otp.bin", "");
        let Err(e) = BootOpts::parse(&args(&["x.elf"]), &dir) else {
            panic!("two OTP files are ambiguous")
        };
        assert!(e.to_string().contains("--otp"), "{e:#}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_boot_partitions_files_are_the_card() {
        let dir = zero_dir("bootdir");
        put(&dir, "pieeprom.bin", "");
        put(&dir, "start4.elf", "");
        put(&dir, "config.txt", "arm_64bit=1\n");
        let o = BootOpts::parse(&args(&[]), &dir).unwrap().unwrap();
        assert_eq!(o.sd, Some(Medium::Files(dir.clone())));
        assert!(o.eeprom);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An image beside them is still the card.
    #[test]
    fn an_sd_image_wins_over_the_directory_it_is_in() {
        let dir = zero_dir("bootdir-img");
        put(&dir, "pieeprom.bin", "");
        put(&dir, "start4.elf", "");
        put(&dir, "sd.img", "");
        let o = BootOpts::parse(&args(&[]), &dir).unwrap().unwrap();
        assert_eq!(o.sd, Some(Medium::Image(dir.join("sd.img"))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_argument_is_where_everything_is_read_from() {
        let dir = zero_dir("arg-dir");
        put(&dir, "pieeprom.bin", "");
        put(&dir, "config.txt", "");
        let o = BootOpts::parse(&args(&[dir.to_str().unwrap()]), Path::new(""))
            .unwrap()
            .unwrap();
        assert_eq!(o.path, dir.join("pieeprom.bin"));
        assert_eq!(o.sd, Some(Medium::Files(dir.clone())));
        assert!(o.eeprom);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    const BOOT_URL: &str =
        "https://raw.githubusercontent.com/raspberrypi/firmware/refs/heads/master/boot/";

    #[test]
    fn usb_wedge_host_silent_takes_seconds_or_off_and_needs_a_stick() {
        assert_eq!(
            parse_host_silent("host-silent:13.4").unwrap(),
            HostSilent::AfterUs(13_400_000)
        );
        assert_eq!(
            parse_host_silent("host-silent:off").unwrap(),
            HostSilent::Off
        );
        assert!(parse_host_silent("host-silent:-1").is_err());
        assert!(parse_host_silent("host-silent:soon").is_err());
        assert!(parse_host_silent("deaf").is_err());
        let parse = |extra: &[&str]| {
            let mut a = vec!["--eeprom", "pieeprom.bin"];
            a.extend_from_slice(extra);
            BootOpts::parse(&args(&a), Path::new(""))
        };
        assert!(
            parse(&["--usb-wedge", "host-silent:5"]).is_err(),
            "needs --usb"
        );
        let o = parse(&["--usb", "stick.img", "--usb-wedge", "host-silent:5"])
            .unwrap()
            .unwrap();
        assert_eq!(o.usb_host_silent, HostSilent::AfterUs(5_000_000));
        let o = parse(&["--usb", "stick.img"]).unwrap().unwrap();
        assert_eq!(o.usb_host_silent, HostSilent::Model);
    }

    #[test]
    fn usb_model_picks_the_stick_and_needs_one() {
        let parse = |extra: &[&str]| {
            let mut a = vec!["--eeprom", "pieeprom.bin"];
            a.extend_from_slice(extra);
            BootOpts::parse(&args(&a), Path::new(""))
        };
        assert!(
            parse(&["--usb-model", "rpi-flash-drive"]).is_err(),
            "needs --usb"
        );
        assert!(parse(&["--usb", "s.img", "--usb-model", "floppy"]).is_err());
        let o = parse(&["--usb", "s.img", "--usb-model", "rpi-flash-drive"])
            .unwrap()
            .unwrap();
        assert_eq!(o.usb_model, StickModel::RpiFlashDrive);
        assert_eq!(
            o.usb_host_silent.limit_us(o.usb_model),
            Some(pimu::periph::usb::RPI_FLASH_DRIVE_HOST_SILENT_US)
        );
        let o = parse(&[
            "--usb",
            "s.img",
            "--usb-model",
            "rpi-flash-drive",
            "--usb-wedge",
            "host-silent:off",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(o.usb_host_silent.limit_us(o.usb_model), None);
        let o = parse(&["--usb", "s.img"]).unwrap().unwrap();
        assert_eq!(o.usb_model, StickModel::Generic);
        assert_eq!(
            o.usb_host_silent.limit_us(o.usb_model),
            None,
            "generic never wedges"
        );
        assert!(
            parse(&[
                "--usb",
                "s.img",
                "--usb-model",
                "rpi-flash-drive",
                "--usb-uas"
            ])
            .is_err(),
            "the preset is UAS already"
        );
    }

    #[test]
    fn usb_uas_changes_the_usb_stick_and_needs_one() {
        let parse = |extra: &[&str]| {
            let mut a = vec!["--eeprom", "pieeprom.bin"];
            a.extend_from_slice(extra);
            BootOpts::parse(&args(&a), Path::new(""))
        };
        let o = parse(&["--usb", "stick.img", "--usb-uas"])
            .unwrap()
            .unwrap();
        assert_eq!(o.usb_uas, UsbUas::On);
        let o = parse(&["--usb", "stick.img", "--usb-uas-refuse"])
            .unwrap()
            .unwrap();
        assert_eq!(o.usb_uas, UsbUas::Refused);
        let o = parse(&["--usb", "stick.img"]).unwrap().unwrap();
        assert_eq!(o.usb_uas, UsbUas::Off);
        assert!(parse(&["--usb-uas"]).is_err());
    }

    #[test]
    fn a_url_is_the_card_and_not_the_file_to_boot() {
        let o = BootOpts::parse(
            &args(&["--eeprom", "pieeprom.bin", BOOT_URL]),
            Path::new(""),
        )
        .unwrap()
        .unwrap();
        assert_eq!(o.path, PathBuf::from("pieeprom.bin"));
        assert!(o.eeprom);
        assert_eq!(o.sd, Some(Medium::RemoteFiles(BOOT_URL.to_string())));
    }

    #[test]
    fn every_medium_takes_an_image_a_directory_or_a_url() {
        let dir = zero_dir("media");
        put(&dir, "start4.elf", "");
        let files = dir.to_str().unwrap();
        let o = BootOpts::parse(
            &args(&[
                "--eeprom",
                "pieeprom.bin",
                "--sd",
                files,
                "--usb",
                "stick.img",
                "--otg",
                "https://example.org/otg.img",
                "--tftp",
                "https://example.org/tftp/",
                "--http",
                "https://example.org/http/",
            ]),
            Path::new(""),
        )
        .unwrap()
        .unwrap();
        assert_eq!(o.sd, Some(Medium::Files(dir.clone())));
        assert_eq!(o.usb, Some(Medium::Image(PathBuf::from("stick.img"))));
        assert_eq!(
            o.otg,
            Some(Medium::RemoteImage("https://example.org/otg.img".into()))
        );
        assert_eq!(
            o.tftp,
            Some(NetRoot::Url("https://example.org/tftp/".into()))
        );
        assert_eq!(
            o.http,
            Some(NetRoot::Url("https://example.org/http/".into()))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A URL is read the way a path is: `/` at the end makes it a directory.
    #[test]
    fn a_trailing_slash_is_what_makes_a_url_a_directory() {
        assert_eq!(
            Medium::from_arg("https://example.org/boot/"),
            Medium::RemoteFiles("https://example.org/boot/".into())
        );
        assert_eq!(
            Medium::from_arg("https://example.org/sd.img"),
            Medium::RemoteImage("https://example.org/sd.img".into())
        );
        // --sd-dir said which it meant, whatever the name looks like.
        assert_eq!(
            Medium::files_from_arg("https://example.org/boot"),
            Medium::RemoteFiles("https://example.org/boot".into())
        );
    }

    #[test]
    fn a_url_that_is_not_a_directory_is_the_file_to_boot() {
        let dir = zero_dir("url-file");
        // Served from a file:// no one has: the fetch is what would fail, so
        // only the split into <url> the file and <url>/ the card is checked.
        assert_eq!(
            Medium::from_arg("https://example.org/pieeprom.bin"),
            Medium::RemoteImage("https://example.org/pieeprom.bin".into())
        );
        let Err(e) = BootOpts::parse(
            &args(&["https://127.0.0.1:1/pieeprom.bin"]),
            Path::new(dir.to_str().unwrap()),
        ) else {
            panic!("nothing is served there")
        };
        assert!(e.to_string().contains("pieeprom.bin"), "{e:#}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_network_roots_want_a_directory_that_is_there() {
        for flag in ["--tftp", "--http"] {
            let Err(e) = BootOpts::parse(
                &args(&["--eeprom", "pieeprom.bin", flag, "no-such-dir"]),
                Path::new(""),
            ) else {
                panic!("the peer would serve nothing")
            };
            assert!(e.to_string().contains("not a directory"), "{e:#}");
        }
    }

    #[test]
    fn two_arguments_for_one_medium_are_an_error() {
        for (other, says) in [
            (["--sd", "boot"], "already there"),
            (["--sd-dir", "boot"], "already there"),
            (["--emmc", "e.img"], "same host"),
        ] {
            let Err(e) = BootOpts::parse(
                &args(&["--eeprom", "pieeprom.bin", BOOT_URL, other[0], other[1]]),
                Path::new(""),
            ) else {
                panic!("{} is a second card", other[0])
            };
            assert!(e.to_string().contains(says), "{e:#}");
        }
    }

    #[test]
    fn sd_dir_takes_a_url_too() {
        let o = BootOpts::parse(
            &args(&["--eeprom", "pieeprom.bin", "--sd-dir", BOOT_URL]),
            Path::new(""),
        )
        .unwrap()
        .unwrap();
        assert_eq!(o.sd, Some(Medium::RemoteFiles(BOOT_URL.to_string())));
    }

    /// The name and the bytes of every file in a card's root.
    fn root_files(entries: &[pimu::fat::Entry]) -> Vec<(String, String)> {
        entries
            .iter()
            .filter_map(|e| match &e.kind {
                pimu::fat::Kind::File {
                    source: pimu::fat::Source::Bytes(bytes),
                    ..
                } => Some((e.name.clone(), String::from_utf8(bytes.clone()).unwrap())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn config_txt_lines_are_appended_under_an_all_header() {
        let dir = zero_dir("config-txt");
        put(&dir, "start4.elf", "");
        put(
            &dir,
            "config.txt",
            "arm_64bit=1\n[pi400]\ndtparam=audio=on\n",
        );
        let mut entries = pimu::fat::entries_of_dir(&dir).unwrap();
        let edits = CardEdits {
            config: vec!["enable_uart=1".into(), "dtoverlay=disable-bt".into()],
            cmdline: None,
        };
        edits.apply(&mut entries).unwrap();
        assert_eq!(
            root_files(&entries),
            [(
                "config.txt".to_string(),
                "arm_64bit=1\n[pi400]\ndtparam=audio=on\n\
                 [all]\nenable_uart=1\ndtoverlay=disable-bt\n"
                    .to_string()
            )]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `raspberrypi/firmware`'s `boot/` has neither file.
    #[test]
    fn a_card_without_them_gets_both_files_in_name_order() {
        let dir = zero_dir("config-txt-new");
        put(&dir, "start4.elf", "");
        put(&dir, "bcm2711-rpi-4-b.dtb", "");
        let mut entries = pimu::fat::entries_of_dir(&dir).unwrap();
        let edits = CardEdits {
            config: vec!["enable_uart=1".into()],
            cmdline: Some("console=ttyAMA0,115200 earlycon".into()),
        };
        edits.apply(&mut entries).unwrap();
        assert_eq!(
            root_files(&entries),
            [
                (
                    "cmdline.txt".to_string(),
                    "console=ttyAMA0,115200 earlycon\n".to_string()
                ),
                (
                    "config.txt".to_string(),
                    "[all]\nenable_uart=1\n".to_string()
                ),
            ]
        );
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "bcm2711-rpi-4-b.dtb",
                "cmdline.txt",
                "config.txt",
                "start4.elf"
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cmdline_replaces_the_cards_own() {
        let dir = zero_dir("cmdline");
        put(&dir, "start4.elf", "");
        put(&dir, "cmdline.txt", "root=/dev/mmcblk0p2 rootwait\n");
        let mut entries = pimu::fat::entries_of_dir(&dir).unwrap();
        CardEdits {
            config: Vec::new(),
            cmdline: Some("console=ttyAMA0,115200".into()),
        }
        .apply(&mut entries)
        .unwrap();
        assert_eq!(
            root_files(&entries),
            [(
                "cmdline.txt".to_string(),
                "console=ttyAMA0,115200\n".to_string()
            )]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn they_are_taken_from_the_command_line_and_need_a_card_of_files() {
        let o = BootOpts::parse(
            &args(&[
                "--eeprom",
                "pieeprom.bin",
                "--sd-dir",
                "boot",
                "--config-txt",
                "enable_uart=1",
                "--config-txt",
                "dtoverlay=disable-bt",
                "--cmdline",
                "console=ttyAMA0,115200",
            ]),
            Path::new(""),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            o.card_edits,
            CardEdits {
                config: vec!["enable_uart=1".into(), "dtoverlay=disable-bt".into()],
                cmdline: Some("console=ttyAMA0,115200".into()),
            }
        );

        let Err(e) = BootOpts::parse(
            &args(&[
                "--eeprom",
                "pieeprom.bin",
                "--sd",
                "sd.img",
                "--config-txt",
                "enable_uart=1",
            ]),
            Path::new(""),
        ) else {
            panic!("a disk image cannot take a config.txt line")
        };
        assert!(e.to_string().contains("built out of files"), "{e:#}");
    }

    #[test]
    fn an_empty_directory_boots_the_published_eeprom() {
        let dir = zero_dir("empty");
        match BootOpts::parse(&args(&[]), &dir) {
            Ok(opts) => assert!(opts.unwrap().eeprom),
            Err(e) => assert!(!e.to_string().contains("missing <file>"), "{e:#}"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The options `boot --help` lists: the lines that start with one.
    #[test]
    fn the_declarations_hold_together() {
        BootArgs::command().debug_assert();
    }

    #[test]
    fn a_directory_beside_eeprom_is_the_card() {
        let dir = zero_dir("eeprom-beside");
        put(&dir, "start4.elf", "");
        put(&dir, "config.txt", "");
        let o = BootOpts::parse(
            &args(&[dir.to_str().unwrap(), "--eeprom", "pieeprom.bin"]),
            Path::new(""),
        )
        .unwrap()
        .unwrap();
        assert_eq!(o.path, PathBuf::from("pieeprom.bin"));
        assert!(o.eeprom);
        assert_eq!(o.sd, Some(Medium::Files(dir.clone())));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn eeprom_takes_its_value_either_way_and_always() {
        let path = |v: &[&str]| BootOpts::parse(&args(v), Path::new("")).map(|o| o.unwrap().path);
        assert_eq!(
            path(&["--eeprom", "e.bin"]).unwrap(),
            PathBuf::from("e.bin")
        );
        assert_eq!(path(&["--eeprom=e.bin"]).unwrap(), PathBuf::from("e.bin"));
        assert!(path(&["e.bin", "--eeprom"]).is_err());
        let err = path(&["x.elf", "--eeprom", "e.bin"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("two images"), "{err}");
    }

    #[test]
    fn a_second_positional_is_an_error() {
        assert!(BootOpts::parse(&args(&["a.elf", "b.elf"]), Path::new("")).is_err());
    }

    #[test]
    fn a_flag_takes_no_value() {
        assert!(BootOpts::parse(&args(&["x.elf", "--quiet=1"]), Path::new("")).is_err());
    }
}
