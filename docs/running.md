# Running a boot

The basics — installing, the first boot, the media — are in the
[README](../README.md). This page is the rest of it.

## The cards

`scripts/make-sd.sh` writes the images, with no root and no loop devices; it
needs `sfdisk`, `mtools` and `e2fsprogs`.

| Command | Card |
|---|---|
| `scripts/make-sd.sh` | `firmware/sd.img`: the default, booting Linux to a busybox shell. |
| `KERNEL=halt scripts/make-sd.sh firmware/sd-halt.img` | The same, with a `kernel8.img` that parks the ARM, so the run ends at the hand-off. |
| `WIRELESS=1 scripts/make-sd.sh firmware/sd-wireless.img` | The card an imager writes, without the `dtoverlay=disable-bt` / `disable-wifi` lines. |
| `BRCMFMAC=1 scripts/make-sd.sh firmware/sd-brcmfmac.img` | That card with the WiFi driver on its root filesystem. |
| `START4=start4cd KERNEL=halt scripts/make-sd.sh firmware/sd-halt-start4cd.img` | The cut-down `start4cd.elf` instead of `start4.elf`. |
| `OTG=1 scripts/make-sd.sh` | A card whose `config.txt` hands the USB-C controller to Linux. |
| `scripts/make-uefi-sd.sh` | A card that hands the ARM to the RPi4 UEFI firmware instead of a kernel. |

On the `WIRELESS=1` card Linux's console is the mini-UART (`ttyS0`), the PL011
is the Bluetooth modem's, and the WiFi chip answers on the legacy EMMC host.
`BRCMFMAC=1` adds
what `testdata/boot/linux-wifi.yaml` loads by hand to pin how far the chip's
bring-up gets: `brcmfmac` and the modules it needs under `/lib/modules`, a
`modprobe` for the one the kernel fetches by itself, the CYW43455's own firmware
under `/lib/firmware/brcm`, and `insmod`/`ip`/`iw`. The network `iw dev wlan0
scan` finds there is invented — `src/periph/sdpcm.rs` says so beside it, and
the scenario's own header says so again.

Each boot scenario in `testdata/boot/` carries the exact flags and EEPROM
settings for its medium, and `pimu boot --scenario <file> --plan` prints
them — the shortest way to see how a given boot is set up.

## Booting a directory

Every option that names a file can be left out when the working directory holds
that file: `pieeprom.bin` is `--eeprom`, and so are `sd.img`, `usb.img`,
`otg.img`, `tftp/`, `http/`, `otp.json` or `otp.bin`, `bootconf.txt` (a `--bootconf`
line each) and `pubkey.bin`.

```bash
cd firmware && pimu boot       # boots from whatever is there, and says what it picked up
pimu boot firmware/            # the same, without the cd (`pimu -C <dir> boot` too)
```

A directory holding a boot partition's own files — a `start4.elf` or a
`config.txt` in it — is the card itself: the MBR and the FAT32 volume around
them are built on the fly, and the files are read from the directory as the
firmware asks for them.

An `http://` or `https://` argument is that directory served over HTTP. A GitHub
URL is listed through the API; any other server has to index the directory
itself. Only the listing is read up front, since that is what the FAT32 volume
is built from; a file's bytes are fetched when the firmware first reads a block
of it and kept in `$XDG_CACHE_HOME/pimu/remote`, so booting the Raspberry Pi
firmware repository costs 13 MB of the directory's 150 and a second run none of
it.

The container image's working directory is `/boot`, so
`docker run --rm -v "$PWD:/boot" ghcr.io/rusty-pi/pimu:latest boot` boots the
directory you run it in.

Every medium reads its argument the same way: `--sd`, `--emmc`, `--usb` and
`--otg` each take a disk image, a directory of a boot partition's files, or
either of those on a server, so the files can go on the stick as easily as on
the card. (`--sd-dir` is what `--sd <dir>` does; it is kept for old command
lines.) A remote *image* is read in 1 MiB `Range` requests as the guest asks
for blocks, so a multi-gigabyte image is never downloaded whole — a server that
answers no `Range` request has its image fetched once into the cache instead.

`--tftp <url>` and `--http <url>` serve what is under the URL over TFTP
or HTTP, fetching each name the first time the guest asks for it; no listing is
needed, since a network boot client asks by name.

An image compressed with `xz` is read in place, block by block: the stream's own
index says where each block starts and how much it decodes to, so a
distribution image boots as it is published — neither unpacked on the host nor
downloaded whole from a server.

```bash
pimu boot --eeprom pieeprom.bin --sd https://cdimage.ubuntu.com/releases/24.04.3/release/ubuntu-24.04.3-preinstalled-server-arm64+raspi.img.xz
pimu boot --eeprom pieeprom.bin --sd ~/Downloads/ubuntu-24.04.3-preinstalled-server-arm64+raspi.img.xz
```

That image is 3850 blocks of 1 MiB, 1.2 GB compressed and 3.76 GiB raw, and a
boot reads the blocks it touches. It needs `xz` installed, and a stream written
as one block (plain `xz -9`, no threads) carries no random access at all — such
an image has to be `xz -d`'d first. Detection is the stream's magic, so the name
does not matter, and `sd.img.xz` beside a `sd.img` is picked up by zero config
the same way.

Every option that names a file takes a URL too — `--eeprom`, `--hat`,
`--display-edid`, `--eeprom-pubkey`, `--maskrom` — fetched into the same cache,
so a whole machine can be described without a local file:

```bash
pimu boot --eeprom https://example.org/pieeprom.bin \
  --sd https://example.org/boot/ --usb https://example.org/disk.img
```

`--config-txt <LINE>` (repeatable) appends to the card's `config.txt`, under an
`[all]` header, and `--cmdline <text>` is its `cmdline.txt`; both are held in
memory, so a read-only directory or a URL takes them too.

A directory with no EEPROM image of its own needs one named with `--eeprom` — a
firmware checkout carries no bootloader, and `start4.elf` run from its ELF entry
stalls silently. Raspberry Pi publishes the stock image in
[`raspberrypi/rpi-eeprom`](https://github.com/raspberrypi/rpi-eeprom)
(`firmware-2711/`); `--eeprom` takes a URL too, so nothing has to be downloaded
first. Without one, `boot` stops and says so.

## Options from a file: `--config`

A long command line can live in a file. `--config <file>` takes a YAML mapping
(or a JSON object) keyed by long option name, without the dashes, and expands
in place where it stands on the command line:

```yaml
# machine.yaml
eeprom: https://example.org/pieeprom.bin
usb: https://example.org/disk.img
boot-order: "0x5"           # a string, so YAML does not read it as a number
max-wall: 600
bootconf:                    # an array repeats the option
  - HTTP_HOST=boot.example.org
  - HTTP_PORT=80
send-after:                  # a nested array is one option with several values
  - ["/ # ", "uname -a\n"]
stdin: true                  # true is a flag, false leaves it out
v: true                      # a one-letter key is the short option
```

```bash
pimu boot --config machine.yaml --max-wall 900
```

That is the same as writing every option out. Options after `--config` win over
the file's, and a repeatable one adds to it, so the `--max-wall 900` above gives
this run 900 s. `file` is the positional argument, a file cannot name another
`--config`, and every option also takes the `--option=value` form.

## The host's network with `--net passt`

`--net passt` plugs the Ethernet cable into the host's network through
[passt](https://passt.top/), started on a socket pair; `--net passt:<socket>`
connects to one already listening. HTTP boot works through it as is. TFTP boot
needs static addresses, since passt's DHCP has no PXE option 43, and a TFTP
server on the host's port 69.

A run over passt follows the host's clock, so it is not deterministic, and CI
stays on the built-in peer (`--tftp`, `--http`).

## Checking a boot: `--scenario`

A scenario is a YAML file that says what to boot and what the run has to show.
`pimu boot --scenario <file>` boots it, checks the run and exits 0 on a pass and
1 on a failure; given a directory it runs every `*.yaml` in it, one after
another, and ends with a `PASS`/`FAIL` line each.

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/rusty-pi/pimu/<tag>/schemas/boot-scenario.schema.json
name: "usb-dock"
description: "Boot from a stick behind a USB-C dock."

boot:
  eeprom: "out/pieeprom.bin"     # paths are relative to this file; a medium may be a URL
  otg: "out/sd.img"
  boot_order: "0x5"
  wall_secs: 200

milestones:
  - why: "The stick was found behind both hubs."
    line: "MSD device"
  - why: "The firmware reached the ARM hand-off."
    line: "arm_loader: Starting ARM"

golden:                          # optional: diff the whole console against this
  path: "golden/usb-dock.txt"
  retired: false                 # keep the instruction counts out of it
```

- **`milestones`** are substrings that must (or, with `absent: true`, must not)
  appear in the run's output, each with the reason it matters, which is quoted
  when it fails. `line` may be a list: the parts have to appear in that order on
  one line.
- **`golden`** is the whole console, normalised (clocks and kernel timestamps
  stripped), diffed line by line. `retired` pins how many instructions each core
  ran, which is exact for one build of the model and one firmware, so a firmware
  that changes often leaves it `false`. With no `golden` only the milestones
  judge the run.
- **A card is a disk image, a directory, a URL — or a list of files.** `sd`,
  `usb` and `otg` take what `--sd` takes, or `files:`, which builds the boot
  partition from the files named, so a scenario needs no image built beforehand:

  ```yaml
  otg:
    files:
      start4.elf: "../out/start4.elf"             # a path, relative to the scenario
      bcm2711-rpi-4-b.dtb: "https://example.org/bcm2711-rpi-4-b.dtb"   # or a URL, cached
      config.txt: { inline: "arm_64bit=1\nuart_2ndstage=1\n" }
      kernel8.img: { builtin: halt }              # a kernel that parks the ARM
      overlays/x.dtbo: "../x.dtbo"                # a `/` makes a directory
  ```

  The files are copied to `<log>.cards/<medium>/` and booted as a directory.
  `tftp` and `http` take the same `files:` for what the peer serves.
  `uart_2ndstage=1` is what makes `start4.elf` print on the UART, so a
  milestone can match what it says. The `halt` kernel is the one `scripts/make-sd.sh` uses for
  `KERNEL=halt`; a boot that ends at the handover needs nothing more of the ARM.
- The other options of [`boot`](#boot) in a scenario's `boot:` section are the
  same as the command line's: `sd`, `usb`, `tftp`, `http`, `bootconf`, `otp_row`,
  `stepping`, `board_rev`, `until`, `input` and the rest are listed in the
  schema (`schemas/boot-scenario.schema.json`), which an editor reads from the
  comment on the first line.

Options after `--scenario` go to the boot and override the file's own, so
`--max-wall 600` gives a slower machine more time. `--record` rewrites the golden
and the retired counts from the run (and refuses when a milestone failed, so a
broken run is never recorded as the baseline), `--plan` prints the `boot`
arguments instead of running, `--from-log <log>` checks the log of an earlier
run without booting, and `--output <log>` names the combined log
(`boot-<name>.log` by default; with a directory, the directory the logs go in).
The console is written beside the log, as `<log>.console`.

## Wall budgets

`--max-wall` defaults to 140 s (none with `--stdin`), and there is no
instruction cap unless you pass `--max-steps`. Reaching the last milestone takes
longer than 140 s, so each boot scenario carries its own budget (`wall_secs`,
overridable with `--max-wall`).

## Speed

The model holds the guest to real time by default: whenever its modelled clock
runs ahead of the host's — where the run loop jumps the counter over an idle
`sleep`, a `wfi` or a fast-forwarded delay — the loop sleeps the lead off. A
booting guest is slower than the board it models and so runs unhindered; an idle
one leaves the host idle too, instead of a core at 100%.

`--speed <factor>` allows that multiple of real time, and `--speed max` runs as
fast as the host manages, which is what a regression run wants: `boot --scenario`
passes it, so CI is unpaced. Pacing changes nothing the guest sees — same
instructions, same modelled time — and the time spent asleep does not count
against `--max-wall`; `-v` reports it.

## The OTP fuses

A reset keeps the fuses, but the next run starts from the model's own unless
`--otp json:<file>` or `--otp binary:<file>` keeps them. The file is read
before the boot when it exists, and written back after the run when the firmware
programmed a row. A missing file is created from the model's own fuses, so it is
also the way to get the array out, edit it, and boot a board fused differently.
`json:` is an object of row to value, one a line; `binary:` has row *n* at byte
4*n*, little-endian.

A file made from a real board's fuses holds that board's secrets: keep it out of
the repository, and see the OTP rule in [`../CLAUDE.md`](../CLAUDE.md) for why.

## Asking the firmware questions

`--mbox-property` and `--mbox-raw` post property requests to the firmware after
it has booted, the way a booted Linux would through `/dev/vcio`, and `--gencmd`
runs a `vcgencmd` command over VCHIQ. See [`mailbox.md`](mailbox.md).
