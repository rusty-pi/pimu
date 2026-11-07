//! `boot`'s command line, declared: every option says whether it is a flag, takes
//! one value or repeats, and the parser, `--help` and `--config` all read that.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{ArgAction, ArgMatches};

use pimu::log::Spec;
use pimu::periph::usb::{HostSilent, StickModel};
use pimu::soc::{Board, Stepping};

use crate::mbox::{MboxRequest, MboxTag};
use crate::otp::OtpFile;
use crate::{parse_hex_image, parse_u32};

const ABOUT: &str = "\
Boot the machine from an EEPROM image, as a Pi 4 does, or run a VPU ELF

    pimu boot
    pimu boot --eeprom <pieeprom.bin> [<options>]
    pimu boot <file.elf> [<options>]
    pimu boot <dir> [<options>]
    pimu boot <url> [<options>]
    pimu boot --scenario <file.yaml>|<dir> [--record|--plan|--from-log <log>] [<options>]

With no command the options are `boot`'s: `pimu --eeprom <file> ...`.
`--config <file>` works as for every command (see `pimu --help`), and every
option that takes a value takes it as `--option value` or `--option=value`.
Given twice, an option that takes one value keeps the later one.

The run prints the serial console (not with -q), what was asked for by name
(`--dump`, `--print-fdt`, `--mbox-property`, …) and one
`result: ok|FAILED — <why>` line; the exit status is 1 on failure. Besides the
limits below, a run ends at an instruction the decoder does not implement, and
once the firmware has printed nothing for a minute of model time.";

const AFTER: &str = "\
ZERO CONFIG:
    An option left out takes the file of that name in the working directory,
    when there is one, so a directory holding these boots with a bare
    `pimu boot` — and `pimu boot <dir>` reads them from <dir> instead,
    as `pimu -C <dir> boot` does:

        pieeprom.bin  --eeprom            otp.json      --otp json:<file>
        sd.img        --sd                otp.bin       --otp binary:<file>
        usb.img       --usb               bootconf.txt  --bootconf, a line each
        otg.img       --otg               pubkey.bin    --eeprom-pubkey
        tftp/         --tftp
        http/         --http

    A directory that holds a boot partition's files instead — `start4.elf`,
    `config.txt` — is the card itself: `boot` builds the FAT32 volume around
    them. A firmware checkout carries no EEPROM bootloader, so give one with
    --eeprom (a `pieeprom.bin`) or keep it beside the files.

    An option that rules another one out keeps its file out too: --emmc leaves
    sd.img alone, --net leaves tftp/ and http/. otp.json and otp.bin together say
    nothing about which to read, so that asks for an explicit --otp. The run
    names on stderr what it picked up.

OVER HTTP:
    Wherever a file or a directory is named — the argument above, every
    medium, --eeprom and the other file options — an `http://` or `https://`
    URL does as well, so nothing has to be cloned or mounted first. A URL
    that ends in `/` is a directory, as it is in a browser, and anything
    else is a file:

        pimu boot --eeprom pieeprom.bin https://raw.githubusercontent.com/raspberrypi/firmware/refs/heads/master/boot/
        pimu boot --eeprom https://example.org/pieeprom.bin --sd https://example.org/sd.img
        pimu boot --eeprom pieeprom.bin --tftp https://example.org/tftp/

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

        pimu boot --eeprom pieeprom.bin --sd https://cdimage.ubuntu.com/releases/24.04.3/release/ubuntu-24.04.3-preinstalled-server-arm64+raspi.img.xz";

/// `--net`: the host's network through passt, started here or already listening.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostNet {
    Passt,
    Socket(PathBuf),
}

/// `--speed`: a cap on how far ahead of real time the guest runs, `None` for `max`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Speed(pub Option<f64>);

#[derive(clap::Parser, Debug)]
#[command(
    name = "boot",
    bin_name = "pimu boot",
    about = ABOUT.lines().next(),
    long_about = ABOUT,
    after_long_help = AFTER,
    args_override_self = true,
    disable_version_flag = true,
)]
pub struct BootArgs {
    /// A directory to read everything from (and the card itself when it holds a
    /// boot partition's files), a URL ending in `/` for such a directory on a
    /// server, or the file to boot: a VPU ELF, or an EEPROM image fetched from a
    /// URL whose name starts with `pieeprom`.
    #[arg(value_name = "FILE|DIR|URL")]
    pub target: Option<String>,

    /// Put this image on the SPI flash and boot from it, through the modelled
    /// boot ROM. Without it the file is a VPU ELF, loaded and run from its entry
    /// point.
    #[arg(long, value_name = "pieeprom.bin", help_heading = "Machine")]
    pub eeprom: Option<String>,

    /// The BCM2711 silicon, C0 by default.
    #[arg(long, value_name = "b0|c0", value_parser = Stepping::parse, help_heading = "Machine")]
    pub stepping: Option<Stepping>,

    /// The OTP revision code; by default a board that stepping shipped on. Its
    /// memory field is the board's memory: the RAM behind the bus, and the
    /// LPDDR4 parts the DRAM controller reports, so the firmware trains and
    /// publishes that size. `total_mem` in `config.txt` cuts it down from there.
    #[arg(long, value_name = "hex", value_parser = Board::parse_revision, help_heading = "Machine")]
    pub board_rev: Option<u32>,

    /// The OTP fuses, kept across runs: read before the boot when <file> exists,
    /// written back after the run when the firmware programmed a row, created
    /// from the model's own fuses when it does not exist. Its rows go over the
    /// model's own, and a row it says nothing about keeps the value the model
    /// gives it — an unprogrammed fuse reads 0 and no format spells it out.
    /// json: row -> value, one a line; binary: row n at byte 4n, little-endian.
    /// A file with a real board's fuses holds its secrets: keep it out of the
    /// repository.
    #[arg(
        long,
        value_name = "json:<file>|binary:<file>",
        help_heading = "Machine"
    )]
    pub otp: Option<OtpFile>,

    /// Program one fuse before the boot, on top of the model's own rows and
    /// anything --otp read. The model is a board out of the factory, so a row an
    /// owner would have fused - the device private key in 56-63, say - is blank
    /// until this sets it. Repeatable.
    #[arg(long, value_name = "ROW=VALUE", value_parser = parse_otp_row, help_heading = "Machine")]
    pub otp_row: Vec<(u32, u32)>,

    /// Execute a real VPU maskROM dump from its reset vector, 0x60000000, instead
    /// of the modelled boot ROM stage. Experimental. The dump stays a local file:
    /// never commit it.
    #[arg(long, value_name = "rom.bin", help_heading = "Machine")]
    pub maskrom: Option<String>,

    /// An SD card: a disk image (`.xz` and all), or a directory whose files the
    /// boot partition holds — the MBR, the FAT32 volume and its directories are
    /// built around them, and the files are read as the firmware asks for them.
    /// What `git clone https://github.com/raspberrypi/firmware` leaves in
    /// `boot/` is such a directory. Read on demand either way; writes stay in
    /// memory and outlive a firmware reset.
    ///
    /// `partitions:<file>` builds a GPT disk out of a YAML file of partitions
    /// (`type`, `format`, `files`) instead; the paths in it are relative to the
    /// file (docs/running.md). --usb, --otg and --emmc take the same.
    #[arg(long, value_name = "img|dir|url", help_heading = "Media and network")]
    pub sd: Option<String>,

    /// What `--sd` does for a directory or a URL ending in `/`.
    #[arg(long, value_name = "dir|url", hide = true, conflicts_with = "sd")]
    pub sd_dir: Option<String>,

    /// An e-MMC part soldered to the SD host, as a Compute Module has in place of
    /// a card slot: an image or a directory of files, as --sd takes. Answers CMD1
    /// and the EXT_CSD instead of an SD card's ACMD41 and SCR. The same host as
    /// --sd.
    #[arg(
        long,
        value_name = "img|dir|url",
        conflicts_with_all = ["sd", "sd_dir"],
        help_heading = "Media and network"
    )]
    pub emmc: Option<String>,

    /// Add this line to the card's `config.txt`, under an `[all]` header so a
    /// conditional section the file ends in does not swallow it. Repeatable. A
    /// card without a `config.txt` — what `raspberrypi/firmware`'s `boot/` is —
    /// gets one holding these lines, so `--config-txt enable_uart=1` is what
    /// makes such a card print anything at all, and `uart_2ndstage=1` what adds
    /// start4.elf's own log. Only for a medium given as files; a disk image is
    /// opaque.
    #[arg(
        long,
        value_name = "LINE",
        allow_hyphen_values = true,
        help_heading = "Media and network"
    )]
    pub config_txt: Vec<String>,

    /// The card's `cmdline.txt` is <text>, whatever it held: the firmware reads
    /// the whole file as one kernel command line. Same cards as --config-txt.
    #[arg(
        long,
        value_name = "text",
        allow_hyphen_values = true,
        help_heading = "Media and network"
    )]
    pub cmdline: Option<String>,

    /// A USB mass-storage stick in blue socket A (xHCI root port 2,
    /// SuperSpeed): an image or a directory of files, as --sd takes. Read on
    /// demand; writes stay in memory and outlive a firmware reset.
    #[arg(long, value_name = "img|dir|url", help_heading = "Media and network")]
    pub usb: Option<String>,

    /// Make the --usb stick a USB Attached SCSI one: Bulk-Only on interface
    /// alternate setting 0 and UAS (command, status, data-in and data-out pipes,
    /// streams) on setting 1, as UAS sticks have them. A host that does not
    /// select setting 1 sees a plain Bulk-Only stick.
    #[arg(long, help_heading = "Media and network")]
    pub usb_uas: bool,

    /// As --usb-uas, but the stick stalls the request that selects setting 1, so
    /// a host has to fall back to Bulk-Only.
    #[arg(long, conflicts_with = "usb_uas", help_heading = "Media and network")]
    pub usb_uas_refuse: bool,

    /// Which stick --usb is. `generic` (the default) is the Samsung Bulk-Only
    /// stick, or with --usb-uas an invented UAS one. `rpi-flash-drive` is the
    /// official Raspberry Pi USB Flash Drive (2e8a:0030) as measured on a
    /// Raspberry Pi 4B d03115: UAS with Bulk-Only as the other setting, and it
    /// wedges, see --usb-wedge.
    #[arg(
        long,
        value_name = "generic|rpi-flash-drive",
        value_parser = parse_stick_model,
        help_heading = "Media and network"
    )]
    pub usb_model: Option<StickModel>,

    /// Make the --usb stick stop taking an address (Address Device fails with
    /// USB Transaction Error) when the xHCI controller first runs more than
    /// <seconds> after a warm reset the stick survived (the first power-on never
    /// wedges it), as a Raspberry Pi USB flash drive does behind the VL805 of a
    /// Raspberry Pi 4B d03115 when nothing talks to it for about 13.4 s after a
    /// warm reset. `rpi-flash-drive` does it by default, with the limit
    /// docs/usb.md calibrates (11.25 s of model time, not board time); `off`
    /// turns it off. The generic stick never does unless asked.
    #[arg(
        long,
        value_name = "host-silent:<seconds>|off",
        value_parser = parse_host_silent,
        help_heading = "Media and network"
    )]
    pub usb_wedge: Option<HostSilent>,

    /// A stick given by --usb or --otg is <n> MiB, with the image at its start,
    /// as on a Pi whose first boot uses the rest.
    #[arg(long, value_name = "n", help_heading = "Media and network")]
    pub usb_mb: Option<u64>,

    /// The same stick in the USB-C socket instead, on the BCM2711's own xHCI.
    /// --boot-order 0x5 (BCM-USB-MSD) boots from it. Linux is given that
    /// controller when the firmware booted from it, and otherwise only when the
    /// card's config.txt says otg_mode=1 (OTG=1 scripts/make-sd.sh).
    #[arg(long, value_name = "img|dir|url", help_heading = "Media and network")]
    pub otg: Option<String>,

    /// Put the --otg stick behind a USB-C dock: two hubs, an Ethernet adapter and
    /// an empty card reader besides the stick, five devices that each take an
    /// xHCI slot.
    #[arg(long, help_heading = "Media and network")]
    pub otg_dock: bool,

    /// A HAT on the 40-pin header, with this ID EEPROM image at 0x50 on I2C0
    /// (`eepmake` output). The firmware reads it where it probes the header, and
    /// applies the device-tree overlay in it.
    #[arg(long, value_name = "eep", help_heading = "Media and network")]
    pub hat: Option<String>,

    /// Plug a monitor into HDMI0: its `HOTPLUG` reports connected and a built-in
    /// EDID answers on the DDC bus. Off by default, as the reference board has
    /// no monitor on either connector.
    #[arg(long, help_heading = "Media and network")]
    pub display: bool,

    /// Serve this EDID blob (128 or 256 bytes) instead of the built-in one.
    /// Implies --display.
    #[arg(long, value_name = "file", help_heading = "Media and network")]
    pub display_edid: Option<String>,

    /// Plug the Ethernet cable into the built-in network peer: DHCP, DNS, and
    /// what is in <dir> over TFTP — the files `start4.elf` and its own network
    /// driver fetch. A URL serves what is under it instead, each name fetched
    /// the first time the guest asks for it.
    #[arg(
        long,
        value_name = "dir|url",
        conflicts_with = "net",
        help_heading = "Media and network"
    )]
    pub tftp: Option<String>,

    /// The same peer, serving <dir> over HTTP: `boot.img` and `boot.sig` under
    /// `net_install/` (`HTTP_PATH`). It may be the directory --tftp has.
    /// BOOT_ORDER picks the protocol.
    #[arg(
        long,
        value_name = "dir|url",
        conflicts_with = "net",
        help_heading = "Media and network"
    )]
    pub http: Option<String>,

    /// Plug the Ethernet cable into the host's network instead of the built-in
    /// peer, through passt: `passt` starts one (from PATH) on a socket pair,
    /// `passt:<socket>` connects to one already listening (`passt -f -s
    /// <socket>`), or to anything else speaking QEMU's `-netdev stream` framing
    /// on that UNIX socket. Runs on the host's clock, so not deterministic.
    #[arg(
        long,
        value_name = "passt[:<socket>]",
        value_parser = parse_host_net,
        help_heading = "Media and network"
    )]
    pub net: Option<HostNet>,

    /// Report every read of memory the VPU wrote through a cached alias (`0x0`,
    /// `0x4000_0000`, `0x8000_0000`) and did not flush, and every cached read of
    /// memory an ARM-side master wrote behind those caches: on real silicon both
    /// see the wrong bytes. Goes to the `coherency` log channel.
    #[arg(long, help_heading = "Media and network")]
    pub check_coherency: bool,

    /// Report every scalar VPU access that is not naturally aligned. The model
    /// reads memory by offset; the core cannot, so such an access reads other
    /// bytes on silicon. Goes to the `alignment` log channel.
    #[arg(long, help_heading = "Media and network")]
    pub check_alignment: bool,

    /// Stretch the intervals the model would otherwise take exactly — a card
    /// block, a SuperSpeed link's training, an I2C or DDC transfer — by a factor
    /// drawn from this seed, so the run is reproducible but not even. Nothing is
    /// ever shortened. Compare stock and ours on the same seed: what only ours
    /// fails is ours. Goes to the `jitter` log channel.
    #[arg(long, value_name = "seed", help_heading = "Media and network")]
    pub jitter: Option<u64>,

    /// With --jitter, let about one chance in this many go wrong the way a board
    /// does: a frame that never arrives, and nothing else yet. Off without it,
    /// when jitter only costs time.
    #[arg(long, value_name = "one-in", help_heading = "Media and network")]
    pub faults: Option<u64>,

    /// Boot what a scenario file describes and check the run against it: the
    /// milestones it names, its golden console transcript and the instruction
    /// counts it pins. The files are read relative to the scenario. A directory
    /// runs every `*.yaml` in it, one after another. Exits 0 on a pass and 1 on
    /// a failure; the other options go to the boot and override the file's
    /// (`--max-wall`, ...). The combined output goes to `boot-<name>.log` and the
    /// console beside it as `<log>.console`. See docs/running.md.
    #[arg(
        long,
        value_name = "file.yaml|dir",
        conflicts_with = "target",
        help_heading = "Scenario"
    )]
    pub scenario: Option<PathBuf>,

    /// Rewrite the golden transcript and the retired counts from this run, once
    /// it has passed its milestones.
    #[arg(long, requires = "scenario", help_heading = "Scenario")]
    pub record: bool,

    /// Print the arguments of the boot instead of running it, one a line, and
    /// name any file it needs that is missing.
    #[arg(long, requires = "scenario", help_heading = "Scenario")]
    pub plan: bool,

    /// Check the log of an earlier run, and the `<log>.console` beside it,
    /// without booting.
    #[arg(
        long,
        value_name = "log",
        requires = "scenario",
        conflicts_with_all = ["plan", "output"],
        help_heading = "Scenario"
    )]
    pub from_log: Option<PathBuf>,

    /// Where the combined output goes. For a directory of scenarios, the
    /// directory the logs go in.
    #[arg(
        long,
        value_name = "log",
        requires = "scenario",
        help_heading = "Scenario"
    )]
    pub output: Option<PathBuf>,

    /// Add a BOOT_ORDER=<hex> line to bootconf.txt. Without one the bootloader
    /// uses its built-in 0xf4: SD card, then restart, and never USB — except
    /// with --usb, which defaults to 0xf41: SD card, then the stick.
    #[arg(long, value_name = "hex", help_heading = "EEPROM image")]
    pub boot_order: Option<String>,

    /// Add any other line to bootconf.txt, e.g. HTTP_HOST=<host> for HTTP boot.
    /// Repeatable; a later line wins over an earlier one with the same key.
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_bootconf, help_heading = "EEPROM image")]
    pub bootconf: Vec<String>,

    /// Ask for a tryboot before the first boot, as `reboot '0 tryboot'` does from
    /// Linux: the bootloader reads tryboot.txt in place of config.txt, and a
    /// start_file= in it names the firmware. The request is one-shot, so a reset
    /// inside the run boots normally.
    #[arg(long, help_heading = "EEPROM image")]
    pub tryboot: bool,

    /// Once the run reaches --until, play what Linux's `reboot` does into the
    /// mailbox (an mmc rescan's three SET_GPIO_STATE 134 pulses, NOTIFY_REBOOT,
    /// SET_GPIO_STATE 134 <- 0 and 130 <- 1, NOTIFY_REBOOT) and ask the PM
    /// watchdog for a reset, then boot again. The GPIO expander is off the SoC,
    /// so it keeps what the firmware left in it across the reset, and a card
    /// whose SD_PWR_ON stays low answers nothing. A reset that leaves it low
    /// prints `expander: SD_PWR_ON is driven low across the reset`.
    #[arg(long, requires = "until", help_heading = "EEPROM image")]
    pub orderly_reboot: bool,

    /// Set SIGNED_BOOT=0 in bootconf.txt: skip the bootloader's SHA-256 +
    /// RSA-2048 verify of boot.img, about half a billion interpreted
    /// instructions.
    #[arg(long, help_heading = "EEPROM image")]
    pub skip_signed_boot: bool,

    /// Put this RSA-2048 public key (n then e, 264 bytes) in the pubkey.bin slot,
    /// as `rpi-eeprom-config --pubkey` does. Signed images, an HTTP-booted
    /// boot.img among them, are verified against it.
    #[arg(long, value_name = "pubkey.bin", help_heading = "EEPROM image")]
    pub eeprom_pubkey: Option<String>,

    /// Run until the firmware releases the ARM, save the machine there and stop.
    /// The VPU's boot is most of a Linux boot's time; a run about the ARM side
    /// can skip it with --snapshot-load.
    #[arg(long, value_name = "file", help_heading = "Running")]
    pub snapshot_save: Option<PathBuf>,

    /// Go on from a --snapshot-save file, with the same command line it was
    /// saved with otherwise. USB devices start as plugged in, not as the
    /// snapshot left them.
    #[arg(long, value_name = "file", help_heading = "Running")]
    pub snapshot_load: Option<PathBuf>,

    /// Stop after this much wall-clock time: 140 s by default, no limit with
    /// --stdin.
    #[arg(long, value_name = "secs", help_heading = "Running")]
    pub max_wall: Option<u64>,

    /// Stop after <n> instructions. No cap by default: this is for pinning a run
    /// to an exact count (bisecting, probes).
    #[arg(long, value_name = "n", help_heading = "Running")]
    pub max_steps: Option<u64>,

    /// Run each ARM core on a host thread of its own instead of taking turns in
    /// one. For a person who wants a Linux shell sooner on a host with cores to
    /// spare: a run is no longer exactly repeatable (how two cores' stores
    /// interleave depends on the host), the busy-wait shortcuts are off, and
    /// nothing here has been compared against the golden transcripts. The
    /// default stays the repeatable one.
    #[arg(long, help_heading = "Running")]
    pub parallel: bool,

    /// How fast the guest may run against real time: 1 by default, so the model
    /// sleeps whenever its clock is ahead of the host's and an idle guest leaves
    /// the host idle too. A factor above 1 allows that multiple of real time,
    /// and `max` runs as fast as the host manages, which is what CI wants.
    /// --max-wall does not count the time a paced run spends asleep.
    #[arg(long, value_name = "factor|max", value_parser = parse_speed, help_heading = "Running")]
    pub speed: Option<Speed>,

    /// End the run once the console prints <text> (e.g. the shell prompt of a
    /// Linux boot), after the last --send-after went in.
    #[arg(
        long,
        value_name = "text",
        allow_hyphen_values = true,
        help_heading = "Running"
    )]
    pub until: Option<String>,

    /// Type <text> into the serial console (PL011) once it prints <prompt>.
    /// Repeatable; each prompt is looked for only in what the console printed
    /// after the previous send. Both take \n, \r, \t, \\ and \xHH escapes.
    /// Deterministic: keyed to the transcript.
    #[arg(
        long,
        num_args = 2,
        value_names = ["prompt", "text"],
        allow_hyphen_values = true,
        action = ArgAction::Append,
        help_heading = "Running"
    )]
    pub send_after: Vec<String>,

    /// Once the console prints <text>, have the built-in network peer send the
    /// board a Wake-on-LAN magic packet: a broadcast frame of six 0xFF bytes and
    /// 16 copies of the MAC address GENET holds at that moment. Sent once in the
    /// whole run, however many resets it takes, so the boot that follows a wake
    /// sees none. Plugs the peer in even without --tftp or --http; --net has no
    /// such peer.
    #[arg(
        long,
        value_name = "text",
        allow_hyphen_values = true,
        conflicts_with = "net",
        help_heading = "Running"
    )]
    pub magic_packet_after: Option<String>,

    /// Interactive session: the host's stdin is the serial console's input, and
    /// no wall-clock or silence limit ends the run. On a terminal, keys go to
    /// the guest raw (Ctrl-C included); Ctrl-A x quits, Ctrl-A Ctrl-A sends a
    /// Ctrl-A.
    #[arg(long, help_heading = "Running")]
    pub stdin: bool,

    /// Step over an instruction the decoder does not implement instead of
    /// stopping the run: reconnaissance on firmware the decoder is new to.
    #[arg(long, help_heading = "Running")]
    pub skip_unimpl: bool,

    /// Print the full run report as well — registers, the ARM cores, the
    /// property replies, the EEPROM's boot configuration, the device tree's
    /// `/chosen`.
    #[arg(short, long, help_heading = "Output")]
    pub verbose: bool,

    /// Leave the serial console out: not streamed, not printed after the run,
    /// not in the -v report. --console-log still gets it, and --until and
    /// --send-after still see it. For a run whose log channels are the point,
    /// e.g. with --log jsonl:io.
    #[arg(short, long, help_heading = "Output")]
    pub quiet: bool,

    /// Write the raw UART bytes of the run to <path>, with none of the run
    /// report interleaved. This is what `boot --scenario` normalises into the
    /// golden boot transcript.
    #[arg(long, value_name = "path", help_heading = "Output")]
    pub console_log: Option<PathBuf>,

    /// Say what these subsystems did, on stderr or to --log-file <path>. `io` is
    /// what the machine read and wrote apart from the console: SD card and USB
    /// stick block runs with the files they belong to, SPI flash byte runs, OTP
    /// rows read and programmed, and what the network peer did (DHCP, DNS,
    /// TFTP, HTTP). The rest are one device or core each: arm-exc cmp dwc2 emmc
    /// expander gpio irqen mbox otp pcie pmic spi uart xhci, and in a `diag`
    /// build derail dma ff irqtbl sleep swirq tick vec. Repeatable; `jsonl:` for
    /// one JSON object a line. See docs/diagnostics.md.
    #[arg(
        long,
        value_name = "[text:|jsonl:]<channel>[,<channel>...]",
        value_parser = Spec::parse,
        help_heading = "Output"
    )]
    pub log: Vec<Spec>,

    /// Write the --log channels to <path> instead of stderr.
    #[arg(long, value_name = "path", requires = "log", help_heading = "Output")]
    pub log_file: Option<String>,

    /// After the run, print <len> bytes of memory at <hex>, as the VPU sees it.
    /// Repeatable.
    #[arg(long, value_name = "hex:len", value_parser = parse_addr_len, help_heading = "Output")]
    pub dump: Vec<(u32, u32)>,

    /// After the run, disassemble <count> VPU instructions at <hex>. Repeatable.
    #[arg(long, value_name = "hex:count", value_parser = parse_addr_len, help_heading = "Output")]
    pub disasm: Vec<(u32, u32)>,

    /// After the run, write the SPI flash to <path>, as the firmware left it.
    /// Compare it with the image a recovery run was given.
    #[arg(long, value_name = "path", help_heading = "Output")]
    pub dump_eeprom: Option<PathBuf>,

    /// After the run, write the flattened device tree `arm_loader` handed to the
    /// ARM to <path>. Diff two firmware versions with `fdtdump`/`dtc` to catch a
    /// bump that changes what the firmware publishes.
    #[arg(long, value_name = "path", help_heading = "Output")]
    pub dump_fdt: Option<PathBuf>,

    /// Print that whole device tree as source, every node and property, not only
    /// the `/chosen` summary the `-v` run report gives.
    #[arg(long, help_heading = "Output")]
    pub print_fdt: bool,

    /// After the boot, open the firmware's `GCMD` service over VCHIQ and send
    /// <command>, the way `vcgencmd` does, then print the answer. Repeatable;
    /// every command goes over the one connection. The harness brings up a slot
    /// area of its own (`src/cli/vchiq.rs`), because the kernel only connects
    /// when userspace asks it to. Use it with the ARM parked -- `--sd
    /// firmware/sd-halt.img`, the card `--mbox-property` is used with -- for
    /// that flag's own reason.
    #[arg(
        long,
        value_name = "command",
        allow_hyphen_values = true,
        help_heading = "Output"
    )]
    pub gencmd: Vec<String>,

    /// After the boot, post a property-interface request to the firmware the way
    /// a booted Linux would (`/dev/vcio`), and print what the still-running
    /// `start4.elf` answers. Tags are hex, e.g. `0x00000001`
    /// (GET_FIRMWARE_REVISION) or `0x00030092` (GET_CRYPTO_HMAC_SHA256).
    /// Repeatable, one request each, in command-line order with --mbox-raw. See
    /// docs/diagnostics.md.
    #[arg(
        long,
        value_name = "tag[,tag...]",
        value_parser = parse_mbox_tags,
        help_heading = "Output"
    )]
    pub mbox_property: Vec<MboxRequest>,

    /// Post an exact byte image of a property request instead of one built from
    /// tags: `--mbox-raw 2200000000000000030001000a...`. A client whose `sizeof`
    /// is wrong lays its buffer out in ways --mbox-property never would — an
    /// unaligned declared total, an end tag at an odd offset, stale bytes past
    /// the total — and that is what decides whether the firmware takes it.
    /// Repeatable, one request each.
    #[arg(long, value_name = "hex", value_parser = parse_mbox_raw, help_heading = "Output")]
    pub mbox_raw: Vec<MboxRequest>,

    /// Report which DRAM pages are non-zero when the run ends, as address runs:
    /// the RAM a snapshot of the machine would have to carry.
    #[arg(long, help_heading = "Output")]
    pub dram_map: bool,

    /// Print the EEPROM section table `bootloader_eeprom_find_files` walks, on
    /// top of the boot configuration -v decodes.
    #[arg(long, help_heading = "Output")]
    pub eeprom_map: bool,

    /// Print the peripheral-window offsets nothing models, which fell through to
    /// the catch-all stub: one line an offset, with the read and write counts
    /// and the last value.
    #[arg(long, help_heading = "Output")]
    pub stub_log: bool,

    /// Print the VPU's last control transfers, newest first, repeats collapsed:
    /// where a derailed boot came from.
    #[arg(long, help_heading = "Output")]
    pub control_transfers: bool,

    /// Start there instead of at the ELF's entry point, or at the one the boot
    /// ROM stage hands on with --eeprom.
    #[arg(long, value_name = "hex", value_parser = parse_u32, help_heading = "VPU")]
    pub entry: Option<u32>,

    /// Store a word at an address once the image is staged, before the first
    /// instruction of every boot. Repeatable.
    #[arg(long, value_name = "hex=hex", value_parser = parse_patch, help_heading = "VPU")]
    pub patch: Vec<(u32, u32)>,

    /// The exception vector base to start with, for an ELF that does not set its
    /// own. start4 does.
    #[arg(long, value_name = "hex", value_parser = parse_u32, help_heading = "VPU")]
    pub exc_vbase: Option<u32>,

    /// Start core 1 at the entry too, for payloads that run both cores from the
    /// start. A firmware boot needs none of this: start4 wakes core 1 itself.
    #[arg(long, help_heading = "VPU")]
    pub smp: bool,

    /// Run the payload as core 1: bit 16 of `version` reads 1.
    #[arg(long, help_heading = "VPU")]
    pub as_core1: bool,

    /// Record core 0's control transfers (branches and calls), up to 4000000,
    /// and print them after the run. A `diag` build only: cargo build --release
    /// --features diag; the PIMU_* environment variables are in
    /// docs/diagnostics.md.
    #[arg(long, help_heading = "Diagnostics")]
    pub trace: bool,

    /// Every instruction instead, up to 200000.
    #[arg(long, help_heading = "Diagnostics")]
    pub trace_full: bool,

    /// Every instruction from when core 0 first reaches <hex>, up to 200000.
    #[arg(long, value_name = "hex", value_parser = parse_u32, help_heading = "Diagnostics")]
    pub trace_from: Option<u32>,

    /// Log every peripheral access with the PC that made it
    /// (PIMU_TRACE_MMIO=<lo>-<hi> for one address range).
    #[arg(long, help_heading = "Diagnostics")]
    pub trace_mmio: bool,

    /// The ARM is always modelled; kept for old command lines.
    #[arg(long, hide = true)]
    pub arm: bool,
}

impl BootArgs {
    /// `--mbox-property` and `--mbox-raw` in the order the command line gave
    /// them: a failed crypto tag leaves an error code only the next request can
    /// ask for, so the order is the experiment.
    pub fn take_mbox_requests(&mut self, m: &ArgMatches) -> Vec<MboxRequest> {
        let mut at: Vec<(usize, MboxRequest)> = Vec::new();
        for (id, requests) in [
            ("mbox_property", std::mem::take(&mut self.mbox_property)),
            ("mbox_raw", std::mem::take(&mut self.mbox_raw)),
        ] {
            let indices = m.indices_of(id).into_iter().flatten();
            at.extend(indices.zip(requests));
        }
        at.sort_by_key(|(i, _)| *i);
        at.into_iter().map(|(_, r)| r).collect()
    }

    pub fn send_after(&self) -> Result<Vec<(String, Vec<u8>)>> {
        self.send_after
            .chunks(2)
            .map(|pair| {
                let prompt = String::from_utf8(pimu::harness::boot::unescape(&pair[0]))
                    .context("--send-after: the prompt must be UTF-8")?;
                Ok((prompt, pimu::harness::boot::unescape(&pair[1])))
            })
            .collect()
    }

    pub fn trace_wanted(&self) -> Option<&'static str> {
        [
            (self.trace, "--trace"),
            (self.trace_full, "--trace-full"),
            (self.trace_from.is_some(), "--trace-from"),
            (self.trace_mmio, "--trace-mmio"),
        ]
        .into_iter()
        .find_map(|(given, name)| given.then_some(name))
    }
}

fn parse_otp_row(kv: &str) -> Result<(u32, u32)> {
    let (row, value) = kv
        .split_once('=')
        .with_context(|| format!("expected ROW=VALUE, got '{kv}'"))?;
    let row = parse_u32(row).with_context(|| format!("--otp-row '{kv}'"))?;
    let value = parse_u32(value).with_context(|| format!("--otp-row '{kv}'"))?;
    Ok((row, value))
}

fn parse_bootconf(kv: &str) -> Result<String> {
    if !kv.contains('=') {
        bail!("expected KEY=VALUE, got '{kv}'");
    }
    Ok(kv.to_string())
}

fn parse_patch(spec: &str) -> Result<(u32, u32)> {
    let (a, v) = spec.split_once('=').context("expected addr=val")?;
    Ok((parse_u32(a)?, parse_u32(v)?))
}

fn parse_addr_len(spec: &str) -> Result<(u32, u32)> {
    let (a, n) = spec.split_once(':').context("expected addr:len")?;
    Ok((parse_u32(a)?, parse_u32(n)?))
}

fn parse_stick_model(name: &str) -> Result<StickModel> {
    StickModel::parse(name).context("expected generic or rpi-flash-drive")
}

/// `host-silent:<seconds>` or `host-silent:off`.
pub fn parse_host_silent(arg: &str) -> Result<HostSilent> {
    let seconds = arg
        .strip_prefix("host-silent:")
        .context("expected host-silent:<seconds> or host-silent:off")?;
    if seconds == "off" {
        return Ok(HostSilent::Off);
    }
    match seconds.parse::<f64>() {
        Ok(s) if s.is_finite() && s >= 0.0 => Ok(HostSilent::AfterUs((s * 1e6) as u64)),
        _ => bail!("the time must be a number of seconds, or off"),
    }
}

fn parse_host_net(spec: &str) -> Result<HostNet> {
    if spec == "passt" {
        return Ok(HostNet::Passt);
    }
    let sock = spec
        .strip_prefix("passt:")
        .or_else(|| spec.strip_prefix("stream:"))
        .context("expected passt or passt:<socket>")?;
    Ok(HostNet::Socket(PathBuf::from(sock)))
}

fn parse_speed(v: &str) -> Result<Speed> {
    if v == "max" {
        return Ok(Speed(None));
    }
    v.parse()
        .ok()
        .filter(|f: &f64| *f > 0.0 && f.is_finite())
        .map(|f| Speed(Some(f)))
        .context("not a factor above zero, nor `max`")
}

/// `<tag>[:<value-buffer bytes>][=<word>.<word>...]`, comma-separated. start4's
/// idea of a tag's size is not always its Linux client's `sizeof`; crypto
/// `key_id`s are **1-based**, and key 0 answers `KEY_NOT_FOUND`.
fn parse_mbox_tags(list: &str) -> Result<MboxRequest> {
    let mut group: Vec<MboxTag> = Vec::new();
    for t in list.split(',') {
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
    Ok(MboxRequest::Tags(group))
}

fn parse_mbox_raw(hex: &str) -> Result<MboxRequest> {
    Ok(MboxRequest::Raw(parse_hex_image(hex)?))
}
