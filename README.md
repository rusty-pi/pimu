# `pimu` – Pi Emulator

A whole-machine Raspberry Pi 4 (BCM2711) emulator that boots the **real
firmware** — `pieeprom.bin`, `start4.elf`, `fixup4.dat` on the VideoCore VPU —
and then Linux on the four Cortex-A72 cores it releases.

It runs the blobs as they are, so a firmware bump — or a change to a
custom-built one — shows up as a diff in the serial transcript. The boot
firmware runs on the VideoCore VPU, not the ARM cores, which is why an ARM
emulator like QEMU's `raspi4b` cannot test it: it stubs the GPU firmware out
entirely. Accuracy over speed, every core lock-stepped in one host thread, so a
run is deterministic. No off-the-shelf tool does this;
[`docs/references.md`](docs/references.md) surveys the prior art.

> [!WARNING]
> **The arm/linux-side emulation is slow, and it stays slow.** Every core is interpreted and
> lock-stepped in one host thread, because a run has to be deterministic to be
> diffable. That buys tens of millions of guest instructions a second: the
> firmware's own boot is under a minute, a Linux boot to a shell is a few
> minutes, and a full distribution image with a real userspace is over an hour.
> Nothing on the roadmap changes the order of magnitude — the host is not an
> ARM, so there is no KVM to fall back on, the VPU has no host equivalent at
> all, and a profile-guided build (`scripts/pgo-build.sh`) is worth about
> 1.45x, not 100x. Use it to see what firmware *does*, not to get work done on
> a fast Pi.

> [!NOTE]
> For transparency: this is AI slop. The author cannot write Rust at all.
> If you're a Rust expert, feel free to comment how bad it is.

## Run a boot

Nothing to install and nothing to fetch — this boots Raspberry Pi's own firmware
repository straight off GitHub:

```bash
docker run --rm ghcr.io/rusty-pi/pimu:latest boot \
  https://raw.githubusercontent.com/raspberrypi/firmware/refs/heads/master/boot/ \
  --config-txt arm_64bit=1 --config-txt enable_uart=1 --config-txt uart_2ndstage=1 \
  --config-txt dtoverlay=disable-bt --cmdline "console=ttyAMA0,115200 earlycon"
```

The URL is a boot partition's own files served over HTTP: the MBR and the FAT32
volume around them are built on the fly, and each file is fetched as the firmware
reads it. That directory carries no EEPROM bootloader, so the one
[`rusty-pi/pi4-firmware`](https://github.com/rusty-pi/pi4-firmware) publishes
stands in, and no `config.txt`, so `--config-txt` and `--cmdline` supply the
lines a card would — the firmware's own defaults leave the serial console off,
exactly as they do on a real board, and `uart_2ndstage=1` is what adds
`start4.elf`'s own log to the bootloader's. A local directory, or a bare
`pimu boot` in one, works the same way; [`docs/running.md`](docs/running.md) has
the rest.
That directory is a boot partition and nothing else, so the boot ends where a
real board's would: the kernel panics for want of a root filesystem.

`boot` prints the serial console as it goes and ends with one line saying
whether the boot got where it was meant to (`result: ok — the firmware started
the ARM`), with exit status 1 when it did not. `-v` adds the full run report,
`--log <channel>` says what a device did
([`docs/diagnostics.md`](docs/diagnostics.md)), and `--help` lists every option.

### Install it

Every release carries a binary, a `.deb` and an `.rpm` for x86-64 and for
aarch64, and a container image for both, tagged with the version and as
`:latest`. They are built against Debian 12's glibc, so they run on that release
and anything newer, and they carry the EEPROM bootloader built in
([`docs/building.md`](docs/building.md) has the details).

```bash
gh release download -R rusty-pi/pimu -p 'pimu-x86_64-linux.tar.gz'
sudo apt install ./pimu_*_amd64.deb    # or the .rpm
cargo install --path .                 # or build it from a checkout
```

### Boot a card of your own

```bash
./scripts/fetch-firmware.sh           # the real blobs, a kernel and busybox into firmware/ (gitignored)
KERNEL=halt ./scripts/make-sd.sh firmware/sd-halt.img

# EEPROM bootloader + start4.elf off an SD image, up to a kernel that parks the ARM.
pimu boot --eeprom firmware/pieeprom.bin --sd firmware/sd-halt.img

# ...or on into Linux, with your terminal as the serial console (Ctrl-A x quits).
./scripts/make-sd.sh                  # firmware/sd.img
pimu boot --eeprom firmware/pieeprom.bin --sd firmware/sd.img --stdin
```

The model does not stop at the firmware's hand-off to the ARM: the A72 cores
run too, so a boot goes wherever the card's `kernel8.img` takes it.

`make-sd.sh` needs `sfdisk`, `mtools` and `e2fsprogs`; no root, no loop devices.
The cards it can write — a stock `config.txt` with Bluetooth and WiFi on, the
cut-down firmware, a UEFI armstub — are in
[`docs/running.md`](docs/running.md).

### Boot it from something else

| Option | Medium |
|---|---|
| `--sd <img>` | The SD card. |
| `--usb <img>` | A USB mass-storage device on the VL805 (`BOOT_ORDER` 0x4). |
| `--otg <img>` | A stick in the USB-C socket, on the BCM2711's own xHCI (`BOOT_ORDER` 0x5). |
| `--netboot <dir>` | The Ethernet cable, into a built-in DHCP, DNS, TFTP and HTTP peer serving `<dir>` (`scripts/make-netboot.sh` builds one). |
| `--net passt` | The host's network, through [passt](https://passt.top/). |

The same image boots from any of them: `--usb firmware/sd-halt.img` is the card
above in a USB enclosure.

## Commands

| Command | What it does |
|---|---|
| `boot` | Boot the machine from an EEPROM image, as a Pi 4 does, or run a VPU ELF. |
| `disasm <file>` | Disassemble a flat binary or ELF with the VPU decoder: `disasm firmware/start4.elf --base 0xcec00200 --count 40`. |

A build from this tree has four more — `run`, `run-all`, `boot-check` and
`spec-docs`, the regression and documentation checks. They read golden files out
of the checkout, so a released binary leaves them out;
[`building.md`](docs/building.md) describes them.

## Features

The whole chain: EEPROM → BOOTLOADER → `start4.elf` → `arm_loader` from SD card,
USB mass storage on the VL805 and on the USB-C port's own xHCI, TFTP and HTTP
network boot; then the four A72 cores `arm_loader` releases, through the
firmware's own armstub into the kernel, and Linux off the card's ext4 root to a
busybox shell on the serial console. No firmware behaviour is short-circuited,
and no boot needs an opt-in shim or environment variable — the HDMI EDID read
fails the way it does on a board with no monitor plugged in, because the DDC
I²C masters are modelled and nothing answers the EDID EEPROM's address.

- **VideoCore IV interpreter** (`src/vpu/`) — the 16-, 32- and 48-bit scalar
  forms `start4` executes, both VPU cores, exception and interrupt delivery
  through the ThreadX vector table, and the vector unit: decoded in full and
  executed for the forms `insn::VecInsn::executable` accepts, against a modelled
  vector register file. Instruction *lengths* always decode correctly, so an encoding the executor
  does not accept stops as `Unimpl` rather than derailing the PC.
  [`docs/vpu-isa.md`](docs/vpu-isa.md) carries the evidence for each form.
- **AArch64 interpreter** (`src/aarch64/`) — integer A64, SIMD and floating
  point with ARM-exact soft-float, stage 1 MMU, EL3..EL0 and
  the system registers Linux touches. `tests/a64_diff.rs` checks it
  differentially against `qemu-aarch64` user-mode on random instruction streams.
  Four cores run in lock-step with the VPU, paced by the system timer
  (`src/arm/`, `src/armstub.rs`).
- **Machine model** (`src/machine.rs`, `src/periph/`) — RAM, the `0xC000_0000`
  uncached SDRAM alias, address decode, and one model per block the boot chain
  touches: the UARTs, timers, SDRAM and clock controllers, DMA, eMMC2 + SD card,
  SPI EEPROM, GPIO, I²C with the PMIC, HDMI and HVS, the mailbox and its crypto
  service, PCIe + VL805 xHCI + USB storage, the CYW43455's SDIO and Bluetooth
  sides, GENET, and on the ARM side the GIC-400 and the generic timer. The board
  sheet in [`docs/README.md`](docs/README.md) draws all of them and
  [`docs/periph/`](docs/periph/) has a page each; a logging catch-all takes the
  rest.
- **Network peer** (`src/net/`) — `--netboot <dir>` plugs the Ethernet cable
  into a built-in DHCP, DNS, TFTP and plain HTTP server serving `<dir>`.
  `--net passt` plugs it into the host's network through
  [passt](https://passt.top/) instead, for reaching a server on the host such as
  `mkosi serve`; that runs on the host's clock, so it is not deterministic and CI
  stays on the built-in peer ([`docs/running.md`](docs/running.md) has the
  caveats).
- **Serial console input** — `--send-after <prompt> <text>` types into the PL011
  deterministically, keyed to the transcript; `--stdin` makes the host terminal
  the console for an interactive session.
- **Register specs** (`specs/*.toml`) — machine-readable register maps with
  per-register provenance. `build.rs` generates the constants the device models
  match on, `tests/specs.rs` checks them against the model, and
  [`docs/periph/`](docs/periph/) is generated from them. Each block also says
  what carries it (`parent`) and which interrupt lines it drives (`irq`), which
  is what the board sheet in [`docs/README.md`](docs/README.md) draws and
  `tests/board_sheet.rs` checks the drawing against.
- **Firmware pipeline** — `pieeprom.bin` self-update trailer, EEPROM config
  parse, GPT/MBR + FAT32 walk, `fixup4.dat`, RSA signature check.
- **What a booted Linux gets** — the property mailbox, and `start4`'s crypto
  service through `/dev/vcio_crypto`: `linux.toml` pins the HMAC that
  [rpi-mkosi](https://github.com/valtzu/rpi-mkosi)'s root LUKS passphrase is
  derived from, computed by `start4.elf`'s own mbedTLS from the OTP key. USB
  mass storage carries far enough to boot that project's image with `--usb`.
- **Regression harness** (`src/harness/`) — scenarios in, console transcript
  out, diffed against a golden file. See
  [`testdata/README.md`](testdata/README.md).

### Limits

- **HTTPS network boot** is not supported: the built-in peer serves plain HTTP,
  and the bootloader only goes HTTPS when `HTTP_HOST` is left at Raspberry Pi's
  own server, whose TLS stack and CA certificate are inside `pieeprom.bin`.
- **The vector unit's last sub-ops.** A sweep of `start4.elf`'s `.text` finds
  some 15100 vector instructions and 291 of them do not execute — jump tables a
  sweep only *disassembles* as vector code, plus sub-ops no probe settled.
  [`docs/vpu-isa.md`](docs/vpu-isa.md) splits the leftovers up. None is reached
  on a boot: `boot` stops on an unimplemented instruction by default.
- **Linux's own display and Ethernet drivers are not driven by any scenario.**
  Both blocks are modelled and exercised from the firmware's side — network boot
  runs over the GENET, and `--display` puts a monitor on HDMI0 — but the boots
  stop at the `bcmgenet` probe and a registered `eth0`, with no link up and no
  KMS driver loaded.
- **WiFi has no radio.** `brcmfmac` loads the CYW43455's firmware, brings
  `wlan0` up and scans (finding the model's own `pimu-model-ap`), but the
  interface stays `NO-CARRIER`; `linux-wifi.toml` pins how far it gets.
- **No camera and no 3D.** The V3D/QPU unit and the CSI-2 camera interface are
  not modelled; nothing on a boot path touches either.

## Tests

```bash
cargo test
```

The boots themselves are too slow for `cargo test` and run under their own
commands, which [`building.md`](docs/building.md) lists.

`.github/workflows/boot-log.yml` runs fmt, clippy and the tests, then every boot
in parallel on each push and PR to `main`. The tests include `tests/specs.rs`,
which fails when `docs/periph/` differs from what the specs generate.
[`testdata/README.md`](testdata/README.md) describes the scenario files, the
golden transcripts and the images each boot needs.

## Documentation

| Document | What is in it |
|---|---|
| [`docs/README.md`](docs/README.md) | The board sheet: every part the model knows about and what reaches what |
| [`docs/boot-chain.md`](docs/boot-chain.md) | The boot stages, from the VPU ROM to Linux, and what each one reads |
| [`docs/running.md`](docs/running.md) | Boot media, SD-card variants, OTP files, wall budgets |
| [`docs/device-tree.md`](docs/device-tree.md) | Getting the patched device tree out of a run |
| [`docs/diagnostics.md`](docs/diagnostics.md) | The `--log` channels and `PIMU_*` switches that find a wall |
| [`docs/mailbox.md`](docs/mailbox.md) | Asking the booted firmware a property or a `vcgencmd`, and what it answers |
| [`docs/building.md`](docs/building.md) | The source layout, build profiles, the `diag` feature, PGO, the release builds |
| [`docs/periph/`](docs/periph/) | One page per register block, generated from `specs/*.toml` |
| [`docs/vpu-isa.md`](docs/vpu-isa.md) | The VideoCore IV instruction set, with the evidence for each statement |
| [`docs/references.md`](docs/references.md) | Outside sources: datasheets, kernel drivers, other reverse engineering |
| [`testdata/README.md`](testdata/README.md) | The scenario files, the goldens and the images each boot needs |

## Licence

MIT — see [`LICENSE`](LICENSE).

That covers this repository's own code, specs and documentation. It does not
cover the boot blobs a run needs: `scripts/fetch-firmware.sh` downloads those
from Raspberry Pi and Debian at your request, under their own licences, and
none of them is committed here.

This is an independent project. It is not affiliated with, sanctioned by or
endorsed by Raspberry Pi Ltd. or Broadcom. Everything it says about the
hardware comes from public documentation, from Linux drivers and device trees,
from static analysis of published firmware images and from measurements on a
Raspberry Pi 4B — each statement carries its source, in `specs/*.toml` and
`isa/vpu.toml`. No Broadcom or Raspberry Pi material is reproduced.
"Raspberry Pi" is a trademark of Raspberry Pi Ltd., used here only to say which
hardware this models.
