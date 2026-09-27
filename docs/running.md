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
what `testdata/boot/linux-wifi.toml` loads by hand to pin how far the chip's
bring-up gets: `brcmfmac` and the modules it needs under `/lib/modules`, a
`modprobe` for the one the kernel fetches by itself, the CYW43455's own firmware
under `/lib/firmware/brcm`, and `insmod`/`ip`/`iw`. The network `iw dev wlan0
scan` finds there is invented — `src/periph/sdpcm.rs` says so beside it, and
the scenario's own header says so again.

Each boot scenario in `testdata/boot/` carries the exact flags and EEPROM
settings for its medium, and `pimu boot-check <scenario> --plan` prints
them — the shortest way to see how a given boot is set up.

## Booting a directory

Every option that names a file can be left out when the working directory holds
that file: `pieeprom.bin` is `--eeprom`, and so are `sd.img`, `usb.img`,
`otg.img`, `netboot/`, `otp.json` or `otp.bin`, `bootconf.txt` (a `--bootconf`
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

`--config-txt <LINE>` (repeatable) appends to the card's `config.txt`, under an
`[all]` header, and `--cmdline <text>` is its `cmdline.txt`; both are held in
memory, so a read-only directory or a URL takes them too.

A directory with no EEPROM image of its own boots with the bootloader
[`rusty-pi/pi4-firmware`](https://github.com/rusty-pi/pi4-firmware) publishes. A
released binary carries that image; a build from this tree fetches it once with
`gh` and keeps it in `$XDG_CACHE_HOME/pimu` (`~/.cache/pimu`), so delete it to
take a newer one. Any boot with a medium and no EEPROM image of its own uses it,
so `pimu boot --sd card.img` boots too.

## Wall budgets

`--max-wall` defaults to 140 s (none with `--stdin`), and there is no
instruction cap unless you pass `--max-steps`. Reaching the last milestone takes
longer than 140 s, so each boot scenario carries its own budget (`wall_secs`,
overridable with `boot-check --max-wall`).

## Speed

The model holds the guest to real time by default: whenever its modelled clock
runs ahead of the host's — where the run loop jumps the counter over an idle
`sleep`, a `wfi` or a fast-forwarded delay — the loop sleeps the lead off. A
booting guest is slower than the board it models and so runs unhindered; an idle
one leaves the host idle too, instead of a core at 100%.

`--speed <factor>` allows that multiple of real time, and `--speed max` runs as
fast as the host manages, which is what a regression run wants: `boot-check`
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
it has booted, the way a booted Linux would through `/dev/vcio`. See
[`diagnostics.md`](diagnostics.md).
