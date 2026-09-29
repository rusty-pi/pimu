//! `boot`: boot the machine from an EEPROM image, as a Pi 4 does, or run a VPU ELF, then report on the run.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use anyhow::{bail, Context, Result};
use sha1::{Digest, Sha1};

use pimu::armstub::Handoff;
use pimu::emulator::{Emulator, RunLimits, RunReport};
use pimu::firmware::Payload;
use pimu::harness;
use pimu::log::{Log, Spec};
use pimu::machine::Machine;
use pimu::soc::{Board, Stepping};
use pimu::vpu::decode::decode;
use pimu::vpu::length::insn_len_bytes;
use pimu::vpu::UnimplPolicy;

use crate::mbox::{mbox_property_exchange, MboxRequest, MboxTag};
use crate::otp::{Format, OtpFile};
use crate::{parse_hex_image, parse_u32};

/// Blue socket A: root port 1 is the USB2 port feeding the on-board VIA hub, so a
/// SuperSpeed fixture goes on port 2 (measured on a Raspberry Pi 4B d03115).
const USB_ROOT_PORT: usize = 2;

/// `boot --help`. A test holds this and `BootOpts::parse` to the same option list.
const HELP: &str = "\
pimu boot — boot the machine from an EEPROM image, as a Pi 4 does, or
run a VPU ELF

USAGE:
    pimu boot
    pimu boot --eeprom <pieeprom.bin> [<options>]
    pimu boot <file.elf> [<options>]
    pimu boot <dir> [<options>]
    pimu boot <url> [<options>]

    `boot <file> --eeprom` is the same as `boot --eeprom <file>`, and with no
    command the options are `boot`'s: `pimu --eeprom <file> ...`.
    `--config <file>` and `--option=value` work as for every command (see
    `pimu --help`).

    The run prints the serial console (not with -q), what was asked for by
    name (`--dump`, `--print-fdt`, `--mbox-property`, …) and one
    `result: ok|FAILED — <why>` line; the exit status is 1 on failure.
    Besides the limits below, a run ends at an instruction the decoder does
    not implement, and once the firmware has printed nothing for a minute of
    model time.

ZERO CONFIG:
    An option left out takes the file of that name in the working directory,
    when there is one, so a directory holding these boots with a bare
    `pimu boot` — and `pimu boot <dir>` reads them from <dir> instead,
    as `pimu -C <dir> boot` does:

    A directory that holds a boot partition's files instead — `start4.elf`,
    `config.txt` — is the card itself: `boot` builds the FAT32 volume around
    them (--sd-dir), and boots the EEPROM bootloader `rusty-pi/pi4-firmware`
    publishes when there is no `pieeprom.bin` to boot, since a firmware
    checkout carries none.

OVER HTTP:
    Wherever a file or a directory is named — the argument above, every
    medium, --eeprom and the other file options — an `http://` or `https://`
    URL does as well, so nothing has to be cloned or mounted first. A URL
    that ends in `/` is a directory, as it is in a browser, and anything
    else is a file:

        pimu boot https://raw.githubusercontent.com/raspberrypi/firmware/refs/heads/master/boot/
        pimu boot --eeprom https://example.org/pieeprom.bin --sd https://example.org/sd.img
        pimu boot --netboot https://example.org/tftp/

    A directory is listed first, since the FAT32 volume is built out of every
    name and length in it: a GitHub URL through the API, any other server
    has to index the directory itself (one HEAD per file). Each file is then
    fetched when the guest first reads a block of it, and a disk image is
    read in 1 MiB `Range` requests — an image is never downloaded whole, and
    a server that answers no `Range` request has its image fetched once
    instead. What is fetched is kept in `$XDG_CACHE_HOME/pimu/remote`.

    An image compressed with `xz` is read in place, block by block through
    the stream's own index, on the host as much as over HTTP — so a
    distribution image boots as it is published, neither unpacked nor
    downloaded whole (`xz` has to be installed, and a stream of one block
    has no random access in it):

        pimu boot --sd https://cdimage.ubuntu.com/releases/24.04.3/release/ubuntu-24.04.3-preinstalled-server-arm64+raspi.img.xz

        pieeprom.bin  --eeprom            otp.json      --otp json:<file>
        sd.img        --sd                otp.bin       --otp binary:<file>
        usb.img       --usb               bootconf.txt  --bootconf, a line each
        otg.img       --otg               pubkey.bin    --eeprom-pubkey
        netboot/      --netboot

    An option that rules another one out keeps its file out too: --emmc leaves
    sd.img alone, --net leaves netboot/. otp.json and otp.bin together say
    nothing about which to read, so that asks for an explicit --otp. The run
    names on stderr what it picked up.

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
              does not exist. Its rows go over the model's own, and a row it
              says nothing about keeps the value the model gives it — an
              unprogrammed fuse reads 0 and no format spells it out.
              json: row -> value, one a line; binary: row n
              at byte 4n, little-endian. A file with a real board's fuses
              holds its secrets: keep it out of the repository.
    --otp-row <ROW>=<VALUE>
              Program one fuse before the boot, on top of the model's own
              rows and anything --otp read. The model is a board out of the
              factory, so a row an owner would have fused - the device
              private key in 56-63, say - is blank until this sets it.
    --maskrom <rom.bin>
              Execute a real VPU maskROM dump from its reset vector,
              0x60000000, instead of the modelled boot ROM stage.
              Experimental. The dump stays a local file: never commit it.

MEDIA AND NETWORK:
    --sd <img>|<dir>|<url>
              An SD card: a disk image (`.xz` and all), or a directory whose files the boot
              partition holds — the MBR, the FAT32 volume and its directories
              are built around them, and the files are read as the firmware
              asks for them. What `git clone
              https://github.com/raspberrypi/firmware` leaves in `boot/` is
              such a directory. Read on demand either way; writes stay in
              memory, and the boot after a firmware reset starts from the
              medium again. Mutually exclusive with --emmc, the same host.
    --check-coherency
              Report every read of memory the VPU wrote through a cached alias
              (`0x0`, `0x4000_0000`, `0x8000_0000`) and did not flush, and
              every cached read of memory an ARM-side master wrote behind
              those caches: on real silicon both see the wrong bytes. Goes to
              the `coherency` log channel.
    --jitter <seed>
              Stretch the intervals the model would otherwise take exactly —
              a card block, a SuperSpeed link's training, an I2C or DDC
              transfer — by a factor drawn from this seed, so the run is
              reproducible but not even. Nothing is ever shortened. Compare
              stock and ours on the same seed: what only ours fails is ours.
              Goes to the `jitter` log channel.
    --faults <one-in>
              With --jitter, let about one chance in this many go wrong the
              way a board does: a frame that never arrives, and nothing else
              yet. Off without it, when jitter only costs time.
    --check-alignment
              Report every scalar VPU access that is not naturally aligned.
              The model reads memory by offset; the core cannot, so such an
              access reads other bytes on silicon. Goes to the `alignment`
              log channel.
    --hat <eep>
              A HAT on the 40-pin header, with this ID EEPROM image at 0x50 on
              I2C0 (`eepmake` output). The firmware reads it where it probes
              the header, and applies the device-tree overlay in it.
    --config-txt <LINE>
              Add this line to the card's `config.txt`, under an `[all]`
              header so a conditional section the file ends in does not
              swallow it. Repeatable. A card without a `config.txt` — what
              `raspberrypi/firmware`'s `boot/` is — gets one holding these
              lines, so `--config-txt enable_uart=1` is what makes such a
              card print anything at all, and `uart_2ndstage=1` what adds
              start4.elf's own log. Only for a medium given as files; a disk
              image is opaque.
    --cmdline <text>
              The card's `cmdline.txt` is <text>, whatever it held: the
              firmware reads the whole file as one kernel command line. Same
              cards as --config-txt.
    --emmc <img>|<dir>|<url>
              An e-MMC part soldered to the SD host, as a Compute Module has
              in place of a card slot: an image or a directory of files, as
              --sd takes. Answers CMD1 and the EXT_CSD instead of an SD
              card's ACMD41 and SCR. Mutually exclusive with --sd.
    --usb <img>|<dir>|<url>
              A USB mass-storage stick in blue socket A (xHCI root port 2,
              SuperSpeed): an image or a directory of files, as --sd takes.
              Read on demand; writes stay in memory and outlive a firmware
              reset.
    --otg <img>|<dir>|<url>
              The same stick in the USB-C socket
              instead, on the BCM2711's own xHCI. --boot-order 0x5
              (BCM-USB-MSD) boots from it. Linux is given that controller when
              the firmware booted from it, and otherwise only when the card's
              config.txt says otg_mode=1 (OTG=1 scripts/make-sd.sh).
    --usb-mb <n>
              A stick given by --usb or --otg is <n> MiB, with the image at its
              start, as on a Pi whose first boot uses the rest.
    --display
              Plug a monitor into HDMI0: its `HOTPLUG` reports connected and a
              built-in EDID answers on the DDC bus. Off by default, as the
              reference board has no monitor on either connector.
    --display-edid <file>
              Serve this EDID blob (128 or 256 bytes) instead of the built-in
              one. Implies --display.
    --netboot <dir>|<url>
              Plug the Ethernet cable into the built-in network peer: DHCP,
              DNS, and <dir> over TFTP and HTTP. A URL serves what is under
              it instead, each name fetched the first time the guest asks
              for it.
    --net passt[:<socket>]
              Plug the Ethernet cable into the host's network instead of the
              built-in peer, through passt: `passt` starts one (from PATH) on a
              socket pair, `passt:<socket>` connects to one already listening
              (`passt -f -s <socket>`), or to anything else speaking QEMU's
              `-netdev stream` framing on that UNIX socket. Runs on the host's
              clock, so not deterministic.

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
    --tryboot
              Ask for a tryboot before the first boot, as `reboot '0 tryboot'`
              does from Linux: the bootloader reads tryboot.txt in place of
              config.txt, and a start_file= in it names the firmware. The
              request is one-shot, so a reset inside the run boots normally.
    --orderly-reboot
              Once the run reaches --until, play what Linux's `reboot` does
              into the mailbox (an mmc rescan's three SET_GPIO_STATE 134
              pulses, NOTIFY_REBOOT, SET_GPIO_STATE 134 <- 0 and 130 <- 1,
              NOTIFY_REBOOT) and ask the PM watchdog for a reset, then
              boot again. The GPIO expander is off the SoC, so it keeps what
              the firmware left in it across the reset, and a card whose
              SD_PWR_ON stays low answers nothing. Needs --until.
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
    --speed <factor|max>
              How fast the guest may run against real time: 1 by default, so
              the model sleeps whenever its clock is ahead of the host's and
              an idle guest leaves the host idle too. A factor above 1 allows
              that multiple of real time, and `max` runs as fast as the host
              manages, which is what CI wants. --max-wall does not count the
              time a paced run spends asleep.
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
              Print the full run report as well — registers, the ARM cores,
              the property replies, the EEPROM's boot configuration, the
              device tree's `/chosen`.
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
              core each: arm-exc cmp dwc2 emmc expander gpio irqen mbox otp
              pcie pmic spi uart xhci, and in a `diag` build derail dma ff irqtbl
              sleep swirq tick vec. Repeatable; `jsonl:` for one JSON object
              a line.
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
              publishes.
    --print-fdt
              Print that whole device tree as source, every node and property,
              not only the `/chosen` summary the `-v` run report gives.
    --gencmd <command>
              After the boot, open the firmware's `GCMD` service over VCHIQ and
              send <command>, the way `vcgencmd` does, then print the answer.
              Repeatable; every command goes over the one connection. The
              harness brings up a slot area of its own (`src/cli/vchiq.rs`),
              because the kernel only connects when userspace asks it to. Use
              it with the ARM parked -- `--sd firmware/sd-halt.img`, the card
              `--mbox-property` is used with -- for that flag's own reason.

    --mbox-property <tag>[,<tag>...]
              After the boot, post a property-interface request to the firmware
              the way a booted Linux would (`/dev/vcio`), and print what the
              still-running `start4.elf` answers. Tags are hex, e.g.
              `0x00000001` (GET_FIRMWARE_REVISION) or `0x00030092`
              (GET_CRYPTO_HMAC_SHA256). Repeatable, one request each. See
              docs/diagnostics.md.
    --mbox-raw <hex>
              Post an exact byte image of a property request instead of one
              built from tags: `--mbox-raw 2200000000000000030001000a...`. A
              client whose `sizeof` is wrong lays its buffer out in ways
              --mbox-property never would — an unaligned declared total, an end
              tag at an odd offset, stale bytes past the total — and that is
              what decides whether the firmware takes it. Repeatable, one
              request each.
    --dram-map
              Report which DRAM pages are non-zero when the run ends, as
              address runs: the RAM a snapshot of the machine would have to
              carry.
    --eeprom-map
              Print the EEPROM section table `bootloader_eeprom_find_files`
              walks, on top of the boot configuration -v decodes.
    --stub-log
              Print the peripheral-window offsets nothing models, which fell
              through to the catch-all stub: one line an offset, with the read
              and write counts and the last value.
    --control-transfers
              Print the VPU's last control transfers, newest first, repeats
              collapsed: where a derailed boot came from.
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
PIMU_* environment variables are in docs/diagnostics.md):
    --trace   Record core 0's control transfers (branches and calls), up to
              4000000, and print them after the run.
    --trace-full
              Every instruction instead, up to 200000.
    --trace-from <hex>
              Every instruction from when core 0 first reaches <hex>, up to
              200000.
    --trace-mmio
              Log every peripheral access with the PC that made it
              (PIMU_TRACE_MMIO=<lo>-<hi> for one address range).
";

enum HostNet {
    Passt,
    Socket(PathBuf),
}

struct BootOpts {
    path: PathBuf,
    entry: Option<u32>,
    usb_mb: Option<u64>,
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
    print_fdt: bool,
    /// Several exchanges against one booted firmware: a failed crypto tag leaves an
    /// error code only the *next* request can ask for (`0x0003008e`).
    mbox_tags: Vec<MboxRequest>,
    /// `--gencmd` command lines, all run over one VCHIQ connection.
    gencmds: Vec<String>,
    usb: Option<Medium>,
    otg: Option<Medium>,
    netboot: Option<NetRoot>,
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
    RemoteImage(String),
    RemoteFiles(String),
}

impl Medium {
    fn from_arg(s: &str) -> Medium {
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
            Medium::RemoteImage(url) | Medium::RemoteFiles(url) => write!(f, "{url}"),
        }
    }
}

/// `--netboot`: the directory the built-in peer serves over TFTP and HTTP.
#[derive(Clone, Debug, PartialEq, Eq)]
enum NetRoot {
    Dir(PathBuf),
    Url(String),
}

impl NetRoot {
    fn from_arg(s: &str) -> Result<NetRoot> {
        if pimu::remote::is_url(s) {
            return Ok(NetRoot::Url(s.to_string()));
        }
        let dir = PathBuf::from(s);
        if !dir.is_dir() {
            bail!("--netboot {s}: not a directory (the peer serves what is in it)");
        }
        Ok(NetRoot::Dir(dir))
    }

    fn peer(&self) -> pimu::net::BuiltinPeer {
        match self {
            NetRoot::Dir(dir) => pimu::net::BuiltinPeer::with_root(dir.clone()),
            NetRoot::Url(url) => {
                eprintln!("remote: the network peer serves {url}, fetched as the guest asks");
                pimu::net::BuiltinPeer::with_root_url(url.clone())
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

/// What the command line adds to a card built out of files: `config.txt` lines
/// and a `cmdline.txt`. The files themselves are left alone — the edited ones
/// are held in memory, so a read-only directory or a URL takes them too.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
struct CardEdits {
    config: Vec<String>,
    cmdline: Option<String>,
}

impl CardEdits {
    fn is_empty(&self) -> bool {
        self.config.is_empty() && self.cmdline.is_none()
    }

    fn apply(&self, entries: &mut Vec<pimu::fat::Entry>) -> Result<()> {
        if !self.config.is_empty() {
            let mut text = match read_card_file(entries, CONFIG_TXT)? {
                Some(bytes) => String::from_utf8(bytes)
                    .with_context(|| format!("{CONFIG_TXT} on the card is not text"))?,
                None => String::new(),
            };
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            // Under `[all]`, so a conditional section the file ends in does not
            // swallow the lines — which is what the firmware itself would do.
            text.push_str("[all]\n");
            for line in &self.config {
                text.push_str(line);
                text.push('\n');
            }
            put_card_file(entries, CONFIG_TXT, text.into_bytes());
        }
        if let Some(cmdline) = &self.cmdline {
            // One line is the whole command line, so this replaces the card's.
            put_card_file(entries, CMDLINE_TXT, format!("{cmdline}\n").into_bytes());
        }
        Ok(())
    }
}

const CONFIG_TXT: &str = "config.txt";
const CMDLINE_TXT: &str = "cmdline.txt";

/// The bytes of a file in the card's root, wherever they come from.
fn read_card_file(entries: &[pimu::fat::Entry], name: &str) -> Result<Option<Vec<u8>>> {
    let Some(entry) = entries.iter().find(|e| e.name == name) else {
        return Ok(None);
    };
    let pimu::fat::Kind::File { source, len } = &entry.kind else {
        bail!("{name} on the card is a directory");
    };
    let bytes = match source {
        pimu::fat::Source::Path(path) => {
            std::fs::read(path).with_context(|| format!("reading {}", path.display()))?
        }
        pimu::fat::Source::Url(url) => pimu::remote::fetch(url, *len)?,
        pimu::fat::Source::Bytes(bytes) => bytes.clone(),
    };
    Ok(Some(bytes))
}

/// `name` in the card's root holds `bytes`, in place of whatever it held.
fn put_card_file(entries: &mut Vec<pimu::fat::Entry>, name: &str, bytes: Vec<u8>) {
    let kind = pimu::fat::Kind::File {
        len: bytes.len() as u64,
        source: pimu::fat::Source::Bytes(bytes),
    };
    match entries.iter().position(|e| e.name == name) {
        Some(i) => entries[i].kind = kind,
        // The order is the order the firmware finds them in, so keep it by name.
        None => {
            let at = entries.partition_point(|e| e.name.as_str() < name);
            entries.insert(
                at,
                pimu::fat::Entry {
                    name: name.to_string(),
                    kind,
                },
            );
        }
    }
}

/// Zero-config: an option left out takes the file of that name in the working
/// directory, under the names the Pi's own tooling gives. Only an option the
/// command line is silent about is filled in, and only when nothing rules it out
/// (`--emmc` is the same host as `--sd`, `--net` the same cable as `--netboot`).
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

include!(concat!(env!("OUT_DIR"), "/embedded_eeprom.rs"));

/// The EEPROM image to boot a directory of firmware files with. A firmware
/// checkout has no bootloader of its own, and `start4.elf` run from its ELF entry
/// stalls silently — the bootloader does more than place its segments. So it comes
/// from `rusty-pi/pi4-firmware`: built into a released binary, and otherwise
/// fetched once and cached under `$XDG_CACHE_HOME/pimu`.
fn fallback_eeprom() -> Result<PathBuf> {
    let cache = pimu::remote::cache_dir()?;
    if let Some(image) = EMBEDDED_EEPROM {
        return unpack_eeprom(&cache, image);
    }
    let path = cache.join("pieeprom-latest.bin");
    // Nothing on the command line says what booted the medium, so the run does.
    eprintln!("zero-config: {REPO}'s EEPROM image, {}", path.display());
    if path.is_file() {
        return Ok(path);
    }
    std::fs::create_dir_all(&cache).with_context(|| format!("creating {}", cache.display()))?;
    eprintln!("zero-config: fetching it from the newest release");
    // No tag, so this follows whatever that repository released last rather
    // than pinning this build to one of its versions.
    let out = std::process::Command::new("gh")
        .args([
            "release",
            "download",
            "-R",
            REPO,
            "-p",
            "pieeprom.bin",
            "-O",
        ])
        .arg(&path)
        .output();
    let out = match out {
        Ok(out) => out,
        // The repository is private: say what to do, not what failed.
        Err(e) => bail!(
            "no EEPROM image to boot with, and gh could not be run to fetch \
             {REPO}'s ({e}). Give one with --eeprom, or put the `pieeprom.bin` \
             of that repository's newest release at {}",
            path.display()
        ),
    };
    if !out.status.success() {
        let _ = std::fs::remove_file(&path);
        bail!(
            "downloading {REPO}'s pieeprom.bin: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(path)
}

/// The image the binary carries, written out where the boot can open it. Its name
/// is its own digest, so a binary built with another image uses another file and
/// neither goes stale.
fn unpack_eeprom(cache: &Path, image: &[u8]) -> Result<PathBuf> {
    let digest: String = Sha1::digest(image)[..5]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let path = cache.join(format!("pieeprom-{digest}.bin"));
    eprintln!("zero-config: the built-in EEPROM image of {REPO}");
    if path.metadata().is_ok_and(|m| m.len() == image.len() as u64) {
        return Ok(path);
    }
    std::fs::create_dir_all(cache).with_context(|| format!("creating {}", cache.display()))?;
    std::fs::write(&path, image).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

const REPO: &str = "rusty-pi/pi4-firmware";

impl BootOpts {
    /// The options, or `None` for `--help`. `dir` is where [`ZeroConfig`] looks.
    fn parse(args: &[String], dir: &Path) -> Result<Option<Self>> {
        let mut path: Option<PathBuf> = None;
        let mut url: Option<String> = None;
        let mut entry: Option<u32> = None;
        let mut usb_mb: Option<u64> = None;
        let mut display = false;
        let mut display_edid: Option<PathBuf> = None;
        let mut max_steps: Option<u64> = None;
        let mut max_wall_secs: u64 = 140;
        let mut speed: Option<f64> = Some(1.0);
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
        let mut sd: Option<Medium> = None;
        let mut card_edits = CardEdits::default();
        let mut emmc: Option<Medium> = None;
        let mut hat_eeprom: Option<PathBuf> = None;
        let mut check_coherency = false;
        let mut check_alignment = false;
        let mut jitter = None;
        let mut faults = None;
        let mut console_log: Option<PathBuf> = None;
        let mut dump_fdt: Option<PathBuf> = None;
        let mut print_fdt = false;
        let mut mbox_tags: Vec<MboxRequest> = Vec::new();
        let mut gencmds: Vec<String> = Vec::new();
        let mut usb: Option<Medium> = None;
        let mut otg: Option<Medium> = None;
        let mut netboot: Option<NetRoot> = None;
        let mut host_net: Option<HostNet> = None;
        let mut boot_order: Option<String> = None;
        let mut bootconf: Vec<String> = Vec::new();
        let mut otp_rows: Vec<(u32, u32)> = Vec::new();
        let mut eeprom_pubkey: Option<PathBuf> = None;
        let mut maskrom_path: Option<PathBuf> = None;
        let mut stepping: Option<Stepping> = None;
        let mut board_rev: Option<u32> = None;
        let mut dram_map = false;
        let mut eeprom_map = false;
        let mut stub_log = false;
        let mut control_transfers = false;
        let mut skip_signed_boot = false;
        let mut tryboot = false;
        let mut orderly_reboot = false;
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
                "--eeprom" => {
                    eeprom = true;
                    if path.is_none() {
                        if let Some(p) = it.next_if(|p| !p.starts_with('-')) {
                            path = Some(file_arg("--eeprom", p)?);
                        }
                    }
                }
                "--max-steps" => {
                    max_steps = Some(it.next().context("--max-steps needs a value")?.parse()?)
                }
                "--max-wall" => {
                    max_wall_secs = it.next().context("--max-wall needs seconds")?.parse()?
                }
                "--speed" => {
                    let v = it.next().context("--speed needs a factor or `max`")?;
                    speed = if v == "max" {
                        None
                    } else {
                        let f: f64 = v
                            .parse()
                            .ok()
                            .filter(|f: &f64| *f > 0.0 && f.is_finite())
                            .with_context(|| {
                                format!("--speed {v}: not a factor above zero, nor `max`")
                            })?;
                        Some(f)
                    };
                }
                "--trace" | "--trace-full" | "--trace-from" | "--trace-mmio" if !pimu::diag::ON => {
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
                // The ARM is always modelled; kept for old command lines.
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
                "--sd" => take_medium(
                    &mut sd,
                    "--sd",
                    Medium::from_arg(
                        it.next()
                            .context("--sd needs a path, a directory or a URL")?,
                    ),
                )?,
                // What `--sd` does for a directory or a URL ending in `/`.
                "--sd-dir" => take_medium(
                    &mut sd,
                    "--sd-dir",
                    Medium::files_from_arg(it.next().context("--sd-dir needs a path or a URL")?),
                )?,
                "--config-txt" => card_edits.config.push(
                    it.next()
                        .context("--config-txt needs a config.txt line")?
                        .to_string(),
                ),
                "--cmdline" => {
                    card_edits.cmdline = Some(
                        it.next()
                            .context("--cmdline needs a command line")?
                            .to_string(),
                    )
                }
                "--emmc" => take_medium(
                    &mut emmc,
                    "--emmc",
                    Medium::from_arg(
                        it.next()
                            .context("--emmc needs a path, a directory or a URL")?,
                    ),
                )?,
                "--hat" => {
                    hat_eeprom = Some(file_arg("--hat", it.next().context("--hat needs a path")?)?)
                }
                "--console-log" => {
                    console_log = Some(PathBuf::from(
                        it.next().context("--console-log needs a path")?,
                    ))
                }
                "--usb" => take_medium(
                    &mut usb,
                    "--usb",
                    Medium::from_arg(
                        it.next()
                            .context("--usb needs a path, a directory or a URL")?,
                    ),
                )?,
                "--otg" => take_medium(
                    &mut otg,
                    "--otg",
                    Medium::from_arg(
                        it.next()
                            .context("--otg needs a path, a directory or a URL")?,
                    ),
                )?,
                "--usb-mb" => usb_mb = Some(it.next().context("--usb-mb needs a value")?.parse()?),
                "--display" => display = true,
                "--display-edid" => {
                    display = true;
                    display_edid = Some(file_arg(
                        "--display-edid",
                        it.next().context("--display-edid needs a <file>")?,
                    )?);
                }
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
                    netboot = Some(NetRoot::from_arg(
                        it.next().context("--netboot needs a directory or a URL")?,
                    )?)
                }
                "--boot-order" => {
                    boot_order = Some(it.next().context("--boot-order needs a value")?.to_string())
                }
                "--eeprom-pubkey" => {
                    eeprom_pubkey = Some(file_arg(
                        "--eeprom-pubkey",
                        it.next().context("--eeprom-pubkey needs a file")?,
                    )?)
                }
                "--maskrom" => {
                    maskrom_path = Some(file_arg(
                        "--maskrom",
                        it.next().context("--maskrom needs a file")?,
                    )?)
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
                "--otp-row" => {
                    let kv = it.next().context("--otp-row needs ROW=VALUE")?;
                    let (row, value) = kv
                        .split_once('=')
                        .with_context(|| format!("--otp-row: expected ROW=VALUE, got '{kv}'"))?;
                    let row = parse_u32(row).with_context(|| format!("--otp-row '{kv}'"))?;
                    let value = parse_u32(value).with_context(|| format!("--otp-row '{kv}'"))?;
                    otp_rows.push((row, value))
                }
                "--skip-signed-boot" => skip_signed_boot = true,
                "--tryboot" => tryboot = true,
                "--orderly-reboot" => orderly_reboot = true,
                "--skip-unimpl" => skip_unimpl = true,
                "--check-coherency" => check_coherency = true,
                "--check-alignment" => check_alignment = true,
                "--faults" => {
                    let rate = it.next().context("--faults needs a number")?;
                    faults =
                        Some(rate.parse::<u64>().with_context(|| {
                            format!("--faults: expected a number, got '{rate}'")
                        })?);
                }
                "--jitter" => {
                    let seed = it.next().context("--jitter needs a seed")?;
                    jitter =
                        Some(seed.parse::<u64>().with_context(|| {
                            format!("--jitter: expected a number, got '{seed}'")
                        })?);
                }
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
                "--gencmd" => {
                    let command = it.next().context("--gencmd needs a command line")?;
                    gencmds.push(command.clone());
                }
                "--mbox-property" => {
                    let list = it.next().context("--mbox-property needs a tag list")?;
                    let mut group: Vec<MboxTag> = Vec::new();
                    for t in list.split(',') {
                        // `<tag>[:<value-buffer bytes>][=<word>.<word>...]`. start4's
                        // idea of a tag's size is not always its Linux client's
                        // `sizeof`; crypto `key_id`s are **1-based**, and key 0
                        // answers `KEY_NOT_FOUND`.
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
                    mbox_tags.push(MboxRequest::Tags(group));
                }
                "--mbox-raw" => {
                    let hex = it.next().context("--mbox-raw needs a hex image")?;
                    mbox_tags.push(MboxRequest::Raw(parse_hex_image(hex)?));
                }
                "--dram-map" => dram_map = true,
                "--eeprom-map" => eeprom_map = true,
                "--stub-log" => stub_log = true,
                "--control-transfers" => control_transfers = true,
                "--dump-fdt" => {
                    dump_fdt = Some(PathBuf::from(it.next().context("--dump-fdt needs a path")?))
                }
                "--patch" => {
                    let spec = it.next().context("--patch needs <hexaddr>=<hexval>")?;
                    let (a, v) = spec.split_once('=').context("--patch: expected addr=val")?;
                    patches.push((parse_u32(a)?, parse_u32(v)?));
                }
                "-h" | "--help" => return Ok(None),
                s if pimu::remote::is_url(s) => url = Some(s.to_string()),
                s if !s.starts_with('-') => path = Some(PathBuf::from(s)),
                s => bail!("unexpected argument '{s}' (try boot --help)"),
            }
        }
        // A URL argument reads as the path one does: ending in `/` it is a
        // directory, and so the card the machine boots from — the EEPROM
        // bootloader for it comes from the fallback below, as it does for a
        // firmware checkout. Anything else is the file to boot, fetched into
        // the cache, and an image named like an EEPROM one is booted as one.
        if let Some(url) = url.take() {
            if url.ends_with('/') {
                take_medium(&mut sd, "boot <url>", Medium::RemoteFiles(url.clone()))?;
            } else {
                let file = file_arg("boot", &url)?;
                eeprom |= url
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.starts_with("pieeprom"));
                path = Some(file);
            }
        }
        // A directory to boot: read from there, as `-C` does, and it is itself
        // the card when it holds a boot partition's files.
        let from = match path.take_if(|p| p.is_dir()) {
            Some(p) => p,
            None => dir.to_path_buf(),
        };
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
        if host_net.is_none() && netboot.is_none() {
            let mut found = None;
            zero.dir(&mut found, "netboot");
            netboot = found.map(NetRoot::Dir);
        }
        if otp.is_none() {
            otp = zero.otp()?;
        }
        if bootconf.is_empty() {
            bootconf = zero.bootconf()?;
        }
        zero.file(&mut eeprom_pubkey, "pubkey.bin");
        zero.announce();

        // Nothing to boot with: neither a firmware checkout nor a disk image is a
        // bootloader, and a board boots without any medium too, so fall back to
        // the published EEPROM image.
        if path.is_none() {
            path = Some(fallback_eeprom()?);
            eeprom = true;
        }
        let path = path.context("boot: missing <file> (try boot --help)")?;
        if netboot.is_some() && host_net.is_some() {
            bail!("--netboot and --net both plug in the Ethernet cable; give one");
        }
        if log.is_empty() && log_file.is_some() {
            bail!("--log-file needs --log <channel>[,<channel>...]");
        }
        if sd.is_some() && emmc.is_some() {
            bail!("--sd and --emmc are the same host: give one");
        }
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
        Ok(Some(Self {
            path,
            entry,
            usb_mb,
            display,
            display_edid,
            max_steps,
            max_wall_secs,
            speed,
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
            sd,
            card_edits,
            emmc,
            hat_eeprom,
            check_coherency,
            check_alignment,
            jitter,
            faults,
            console_log,
            dump_fdt,
            print_fdt,
            mbox_tags,
            gencmds,
            usb,
            otg,
            netboot,
            host_net,
            boot_order,
            bootconf,
            otp_rows,
            eeprom_pubkey,
            maskrom_path,
            stepping,
            board_rev,
            dram_map,
            eeprom_map,
            stub_log,
            control_transfers,
            skip_signed_boot,
            tryboot,
            orderly_reboot,
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

pub fn cmd_boot(args: &[String]) -> Result<ExitCode> {
    let Some(opts) = BootOpts::parse(args, Path::new(""))? else {
        print!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    };
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
    let edits = FlashEdits::new(opts)?;

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
    let mut fuses = load_otp(opts, rig.board)?;
    let mut fuses_at_start = None;
    let mut partition = 0;
    // Off the SoC: a reset leaves it as the boot before left it.
    let mut expander = None;
    let mut rebooted_orderly = false;
    let (report, emu, start) = 'boot: loop {
        let mut machine = rig.machine(&flash)?;
        if let Some(fuses) = &fuses {
            machine.config_otp.fuse_rows(fuses);
        }
        // After the file, so `--otp-row` fuses a row whatever the run started from.
        for &(row, value) in &opts.otp_rows {
            machine.config_otp.set(row, value);
        }
        machine.pm.keep_partition_bits(partition);
        if let Some(expander) = expander.take() {
            machine.bsc_pmic.fit_expander(expander);
        }
        // One-shot: the bootcode clears it as it reads it.
        if reboots == 0 && opts.tryboot {
            machine.pm.request_tryboot();
        }
        fuses_at_start.get_or_insert_with(|| machine.config_otp.fuses().clone());
        let start = rig.stage(&mut machine, reboots)?;
        let mut emu = rig.emulator(machine, start);
        emu.input.script = opts.sends.iter().cloned().collect();
        emu.input.host = host_input.take();
        let report = emu.run(&limits);
        host_input = emu.input.host.take();
        let report = if opts.orderly_reboot
            && !rebooted_orderly
            && report.end == pimu::emulator::RunEnd::Until
        {
            rebooted_orderly = true;
            orderly_reboot(&mut emu, &limits)?
        } else {
            report
        };
        rig.dump_segment(&emu, &flash, reboots);

        if report.end == pimu::emulator::RunEnd::Reset {
            reboots += 1;
            if !opts.quiet && (opts.verbose || !report.console_streamed) {
                print!("{}", String::from_utf8_lossy(&report.console));
            }
            flash = emu.machine.spi0.flash_bytes().to_vec();
            edits.apply(&mut flash, false); // self-update restored SIGNED_BOOT=1
            fuses = Some(emu.machine.config_otp.fuses().clone());
            partition = emu.machine.pm.partition_bits();
            expander = emu.machine.bsc_pmic.expander().cloned();
            if reboots <= 4 {
                // The next boot's ARM side starts a profile of its own.
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

/// The `bootconf.txt` edits the options ask for. Re-applied after every
/// self-update reset, which brings back the image's own settings.
struct FlashEdits {
    eeprom: bool,
    skip_signed_boot: bool,
    conf_lines: Vec<String>,
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

    fn apply(&self, flash: &mut Vec<u8>, announce: bool) {
        self.unsign(flash, announce);
        self.append_conf(flash, announce);
        self.set_pubkey(flash, announce);
    }

    /// `--skip-signed-boot`: flip `SIGNED_BOOT=1` to `=0`, skipping the very slow
    /// SHA-256 + RSA-2048 verify of `boot.img`. Same length, so the byte layout is
    /// preserved, and the stale `bootconf.sig` is not checked once the flag is 0.
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

    /// Append `BOOT_ORDER=` and every `--bootconf KEY=VALUE` line to the EEPROM's
    /// `bootconf.txt`; a later line overrides an earlier one with the same key.
    /// The section is last in the image and followed by erased flash, so growing
    /// it is a length-field bump and an append — nothing moves.
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

    /// An RSA-2048 public key in the EEPROM's `pubkey.bin` slot, as
    /// `rpi-eeprom-config --pubkey` writes it (n then e, little-endian, 256 + 8
    /// bytes). Signed images are verified against it; an all-zero slot verifies nothing.
    fn set_pubkey(&self, flash: &mut [u8], announce: bool) {
        let Some(k) = &self.pubkey else { return };
        match pimu::firmware::eeprom::replace_file(flash, "pubkey.bin", k) {
            Ok(()) if announce => println!("eeprom-pubkey: pubkey.bin replaced"),
            Ok(()) => {}
            Err(e) => eprintln!("eeprom-pubkey: {e:#}"),
        }
    }
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
            ref netboot,
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
            machine.spi0.attach_flash(flash.to_vec());
        }
        // The card reads its image or its files on demand; each boot after a
        // reset starts from them again, writes forgotten.
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
            machine.ram.coherency = pimu::coherency::Coherency::on(self.log.clone());
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
            machine.pcie.endpoint.attach(
                USB_ROOT_PORT,
                Box::new(pimu::periph::usb::MassStorage::with_disk(disk.clone())),
            );
        }
        // The BCM2711's own xHCI: what `BOOT_ORDER` digit `0x5` boots from and
        // what `otg_mode=1` gives Linux.
        if let Some(disk) = &self.otg_disk {
            machine
                .xhci_otg
                .attach(Box::new(pimu::periph::usb::MassStorage::with_disk_hs(
                    disk.clone(),
                )));
        }
        // The built-in peer (`src/net/peer.rs`): DHCP, DNS, TFTP and HTTP.
        if let Some(netboot) = &netboot {
            machine.attach_net(Box::new(netboot.peer().with_log(self.log.clone())));
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
            quiet,
            ..
        } = *self.opts;
        let mut emu = Emulator::new(machine, start);
        emu.stream_console = !quiet;
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
            let ram = emu.machine.ram.as_slice();
            let _ = std::fs::write(format!("{p}.{}", reboots + 1), ram);
            eprintln!("wrote {p}.{} ({} bytes)", reboots + 1, ram.len());
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

/// The UART bytes on their own, which `boot-check` normalises into the golden
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

/// Which DRAM pages are non-zero when the run ends: the RAM a snapshot would have
/// to carry. The model starts RAM zeroed, so "not all zero" is exact for that.
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
            // Bridge gaps of up to 64 KiB; their zero bytes are counted separately.
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

/// Did the boot do what it was run for, and in a word, what happened. An EEPROM
/// boot succeeds once the firmware has started the ARM — where a Pi's boot firmware
/// is done — and nothing after that undoes it; a VPU ELF succeeds by halting.
/// Anything the model could not do fails the run whenever it happens.
fn boot_outcome(
    report: &pimu::emulator::RunReport,
    emu: &Emulator,
    eeprom: bool,
    until: Option<&str>,
    reboots: u32,
) -> (bool, String) {
    use pimu::emulator::RunEnd;
    use pimu::vpu::exec::Stop;

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

/// Found through the section walk, not by searching for the name: the bootcode
/// carries a string table with the same names in it.
fn find_bootconf_header(flash: &[u8]) -> Option<usize> {
    let img = pimu::firmware::eeprom::EepromImage::parse(flash).ok()?;
    img.sections
        .iter()
        .find(|s| s.filename.as_deref() == Some("bootconf.txt"))
        .map(|s| s.header_offset)
}

/// Find the device tree blob `arm_loader` left for the ARM: the armstub's
/// `dtb_ptr32`, falling back to the firmware's own `Device tree loaded to` console
/// line only when the ARM was never released (`start4cd.elf` prints nothing there).
/// Neither way hard-codes an address, which is what keeps this working across
/// firmware versions. The header's `totalsize` is trusted over the logged length.
fn locate_fdt(
    machine: &mut Machine,
    handoff: Option<Handoff>,
    console: &[u8],
) -> Option<(u32, Vec<u8>)> {
    use pimu::bus::{Bus, Width};

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
    // Fall back to the logged length so `Fdt::parse` can report what is there.
    let len = if u32::from_be_bytes([head[0], head[1], head[2], head[3]]) == pimu::fdt::FDT_MAGIC
        && (40..=8 << 20).contains(&totalsize)
    {
        totalsize
    } else {
        logged_len.max(40)
    };
    Some((addr, read(machine, addr, len)))
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
    use std::collections::BTreeSet;

    use super::*;

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
        std::fs::create_dir_all(dir.join("netboot")).unwrap();

        let o = BootOpts::parse(&args(&[]), &dir).unwrap().unwrap();
        assert_eq!(o.path, dir.join("pieeprom.bin"));
        assert!(o.eeprom, "pieeprom.bin is an EEPROM image");
        assert_eq!(o.sd, Some(Medium::Image(dir.join("sd.img"))));
        assert_eq!(o.usb, Some(Medium::Image(dir.join("usb.img"))));
        assert_eq!(o.otg, Some(Medium::Image(dir.join("otg.img"))));
        assert_eq!(o.netboot, Some(NetRoot::Dir(dir.join("netboot"))));
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
        std::fs::create_dir_all(dir.join("netboot")).unwrap();

        let o = BootOpts::parse(&args(&["x.elf", "--emmc", "e.img", "--net", "passt"]), &dir)
            .unwrap()
            .unwrap();
        assert_eq!(o.sd, None);
        assert_eq!(o.netboot, None);
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
                "--netboot",
                "https://example.org/tftp/",
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
            o.netboot,
            Some(NetRoot::Url("https://example.org/tftp/".into()))
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
    fn netboot_wants_a_directory_that_is_there() {
        let Err(e) = BootOpts::parse(
            &args(&["--eeprom", "pieeprom.bin", "--netboot", "no-such-dir"]),
            Path::new(""),
        ) else {
            panic!("the peer would serve nothing")
        };
        assert!(e.to_string().contains("not a directory"), "{e:#}");
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
    fn documented() -> BTreeSet<&'static str> {
        HELP.lines()
            .filter(|l| l.starts_with("    -"))
            .flat_map(|l| l.trim_start().split("  ").next().unwrap_or("").split(", "))
            .filter_map(|o| o.split(' ').next())
            .collect()
    }

    /// Read out of `parse`'s source: the string literals that are an option name
    /// and nothing else. A double quote in a comment inside `parse` breaks this.
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
        // `--arm` is a no-op and `--sd-dir` is what `--sd <dir>` does; both
        // are kept for old command lines.
        let hidden = ["--arm", "--sd-dir"];
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
