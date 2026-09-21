# Running a boot

`boot` needs an EEPROM image and something to boot from. Everything else has a
default.

```bash
rpi-virt-fw boot --eeprom firmware/pieeprom.bin --sd firmware/sd-halt.img
```

The ARM is always modelled ([#52](https://github.com/valtzu/rpi-virt-fw/issues/52)):
the boot goes wherever the card's `kernel8.img` takes it.

## The media

| Option | Medium |
|---|---|
| `--sd <img>` | The SD card. |
| `--usb <img>` | A USB mass-storage device on the VL805 (`BOOT_ORDER` 0x4). |
| `--otg <img>` | A stick in the USB-C socket, on the BCM2711's own xHCI (`BOOT_ORDER` 0x5). |
| `--netboot <dir>` | The Ethernet cable, into the built-in DHCP/DNS/TFTP/HTTP peer serving `<dir>`. |

`scripts/make-netboot.sh` builds the root `--netboot` serves. The boot
scenarios in `testdata/boot/` carry the exact flags and EEPROM settings for each
medium, and `rpi-virt-fw boot-check <scenario> --plan` prints them.

## The cards

`scripts/make-sd.sh` writes the images, with no root and no loop devices; it
needs `sfdisk`, `mtools` and `e2fsprogs`.

| Command | Card |
|---|---|
| `scripts/make-sd.sh` | `firmware/sd.img`: the default, booting Linux to a busybox shell. |
| `KERNEL=halt scripts/make-sd.sh firmware/sd-halt.img` | The same, with a `kernel8.img` that parks the ARM, so the run ends at the hand-off. |
| `WIRELESS=1 scripts/make-sd.sh firmware/sd-wireless.img` | The card an imager writes, without the `dtoverlay=disable-bt` / `disable-wifi` lines. |
| `BRCMFMAC=1 scripts/make-sd.sh firmware/sd-brcmfmac.img` | That card with the WiFi driver on its root filesystem. |
| `START4=start4cd KERNEL=halt scripts/make-sd.sh firmware/sd-halt-start4cd.img` | The cut-down firmware ([#105](https://github.com/valtzu/rpi-virt-fw/issues/105)). |
| `OTG=1 scripts/make-sd.sh` | A card whose `config.txt` hands the USB-C controller to Linux. |
| `scripts/make-uefi-sd.sh` | A card that hands the ARM to the RPi4 UEFI firmware instead of a kernel. |

On the `WIRELESS=1` card Linux's console is the mini-UART (`ttyS0`), the PL011
is the Bluetooth modem's, and the WiFi chip answers on the legacy EMMC host
([#124](https://github.com/valtzu/rpi-virt-fw/issues/124)). `BRCMFMAC=1` adds
what `testdata/boot/linux-wifi.toml` loads by hand to pin how far the chip's
bring-up gets: `brcmfmac` and the modules it needs under `/lib/modules`, a
`modprobe` for the one the kernel fetches by itself, the CYW43455's own firmware
under `/lib/firmware/brcm`, and `insmod`/`ip`.

## Wall budgets

`--max-wall` defaults to 140 s (none with `--stdin`), and there is no
instruction cap unless you pass `--max-steps`. Reaching the last milestone takes
longer than 140 s, so each boot scenario carries its own budget (`wall_secs`,
overridable with `boot-check --max-wall`).

## The OTP fuses

A reset keeps the fuses, but the next run starts from the model's own unless
`--otp json:<file>` or `--otp binary:<file>` keeps them
([#93](https://github.com/valtzu/rpi-virt-fw/issues/93)). The file is read
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
