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

## Install

The `latest` release carries a Linux binary for x86-64 and for aarch64, a `.deb`
and an `.rpm` of each, and a container image for both architectures. They are
built with PGO against Debian 12's glibc, so they run on that release and on
anything newer, and they carry the EEPROM bootloader below built in — nothing is
fetched on a first boot. A release is cut by hand, from whichever commit the
workflow is dispatched on.

```bash
gh release download latest -R rusty-pi/pimu -p 'pimu-x86_64-linux.tar.gz'
sudo apt install ./pimu_*_amd64.deb              # or the .rpm
docker run --rm -v "$PWD:/boot" ghcr.io/rusty-pi/pimu:latest boot
```

## Run a boot

```bash
cargo install --path .                # or: cargo build --release, then target/release/pimu

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

`boot` prints the serial console as it goes and ends with one line saying
whether the boot got where it was meant to (`result: ok — the firmware started
the ARM`), with exit status 1 when it did not. `-v` adds the full run report,
`--log <channel>` says what a device did
([`docs/diagnostics.md`](docs/diagnostics.md)), and `--help` lists every option.

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

### Or just point it at a directory

A directory of a boot partition's own files is the card itself: the MBR and the
FAT32 volume around them are built on the fly. An `http://` or `https://`
argument is such a directory served over HTTP, fetched a file at a time as the
firmware reads it, so there is nothing to clone or mount first:

```bash
docker run --rm ghcr.io/rusty-pi/pimu:latest boot \
  https://raw.githubusercontent.com/raspberrypi/firmware/refs/heads/master/boot/ \
  --config-txt arm_64bit=1 --config-txt enable_uart=1 --config-txt dtoverlay=disable-bt \
  --cmdline "console=ttyAMA0,115200 earlycon"
```

That boots Raspberry Pi's own firmware repository as it stands. It carries no
EEPROM bootloader, so the one
[`rusty-pi/pi4-firmware`](https://github.com/rusty-pi/pi4-firmware) publishes
stands in; it carries no `config.txt` either, and the firmware's own defaults
leave the serial console off, so `--config-txt` and `--cmdline` supply the lines
a card would. `pimu boot <dir>` and a bare `pimu boot` in a directory work the
same way — [`docs/running.md`](docs/running.md) has the rest.

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
  uncached SDRAM alias, address decode, and the peripherals: PL011 (transmit and
  receive) and mini-UART, system timer, SDRAM controller, clock manager + PLLs,
  config-OTP, power domains and watchdog, DMA4 and legacy DMA, Arasan eMMC2
  (ADMA2/SDMA, writes, 1.8 V) + SD card, SPI0 EEPROM, GPIO with the board's pin
  map and the pads it hands the SPI and I²C masters, BSC/I²C with the DA9090
  PMIC and FXL6408 GPIO expander, HDMI DDC, HVS, mailbox with the crypto
  service, RNG, AVS/PVT, PCIe + VL805 xHCI + a USB mass-storage device, the
  CYW43455's SDIO side and its Bluetooth modem, GENET with its BCM54213PE PHY,
  and on the ARM side the GIC-400, the ARM-local block and the generic timer.
  A logging catch-all takes the rest.
- **Network peer** (`src/net/`) — `--netboot <dir>` plugs the Ethernet cable
  into a built-in DHCP, DNS, TFTP and plain HTTP server serving `<dir>`.
  `--net passt` plugs
  it into the host's network through [passt](https://passt.top/) instead,
  started on a socket pair (`--net passt:<socket>` connects to one already
  listening), for reaching a server on the host such as `mkosi serve`; that runs
  on the host's clock, so it is not deterministic and CI stays on the built-in
  peer. HTTP boot works through it as is; TFTP boot needs static addresses,
  since passt's DHCP has no PXE option 43, and a TFTP server on the host's
  port 69.
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

- **HTTPS network boot** is not supported: the built-in peer serves plain
  HTTP, and the bootloader only goes HTTPS when `HTTP_HOST` is left at
  Raspberry Pi's own server. Out of scope, since the TLS stack and the CA
  certificate are the bootloader's own, inside `pieeprom.bin`.
- **The vector unit's last sub-ops.** A linear sweep of `start4.elf`'s `.text`
  finds some 15100 vector instructions, and 291 of them do not execute — the
  count moves with the blob, so
  [`docs/vpu-isa.md`](docs/vpu-isa.md) splits the leftovers by what
  `binutils-vc4` objdump makes of the same address: mostly jump tables and
  constant pools a sweep only *disassembles* as vector code, plus ALU and
  memory sub-ops neither decoder has a form for and a handful of one-offs no
  probe settled. None of them is reached on a boot — `boot` stops on an
  unimplemented instruction by default, and the firmware boots.
- **Linux's own display and Ethernet drivers are not driven by any scenario.**
  Both blocks are modelled and both are exercised, but from the firmware's side:
  network boot runs over the GENET, driven by `start4.elf`'s own driver, and the
  firmware brings HDMI up (`--display` puts a monitor on HDMI0, so the display
  mailbox tags answer a real geometry). What no scenario reaches is the kernel's
  end of either one: the boots stop at the `bcmgenet` probe and a registered
  `eth0`, with no link brought up and no KMS driver loaded.
- **WiFi has no radio.** `brcmfmac` downloads the CYW43455's firmware, takes the
  chip's events, brings `wlan0` up and scans — the scan finds the model's own
  `pimu-model-ap` — but the interface stays `NO-CARRIER`, since associating and
  carrying frames is still to come (`linux-wifi.toml` pins how far it gets).
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

## Layout

```
src/
  lib.rs        the library; its crate docs map the modules
  cli/          the command line: main.rs = usage + dispatch, one file per
                command (boot, scenario, disasm), mbox.rs, config.rs
  vpu/          VideoCore IV scalar core: length, decode, exec, registers
  aarch64/      A64 core: integer, SIMD/FP, MMU, system registers
  arm/          the four A72 cores, released at arm_loader, lock-stepped
  armstub.rs    armstub hand-off words, image check, bootargs patch
  bus.rs        Bus + MmioDevice traits
  mem.rs        RAM region
  l2.rs         the VPU's L2 while the bootcode runs out of it
  machine.rs    Machine: owns RAM + peripherals, decodes addresses
  periph/       one file per block, stub.rs = catch-all + log
  net/          built-in DHCP/DNS/TFTP/HTTP peer for --netboot; passt for --net
  soc/          BCM2711 memory map, stepping, board
  spec/         register-spec schema (specs/*.toml, via build.rs)
  firmware/     boot ROM stage; ELF32 loader; EEPROM image parse; dt-blob; Payload
  fdt.rs        device tree reader/patcher
  log/          --log channels; fatmap.rs = which file a disk block belongs to
  stdio.rs      host terminal as the serial console (--stdin)
  diag.rs       PIMU_* diagnostics
  emulator.rs   Emulator = VPU cores + ARM side + Machine, run loop
  harness/      scenario parsing, transcript capture, golden diff,
                boot.rs = the boot scenarios and their milestones,
                payloads.rs = hand-assembled VPU test programs
specs/          register maps with provenance; docs/periph/ is generated
docs/           board sheet, boot chain, diagnostics, VPU ISA, references
scripts/        fetch-firmware.sh, make-sd.sh, make-netboot.sh, pgo-build.sh,
                provision-eeprom.sh, make-dt-blob.py, vc4-xref.py
testdata/       in-process scenarios and the boot scenarios, with their goldens
```

## Documentation

| Document | What is in it |
|---|---|
| [`docs/README.md`](docs/README.md) | The board sheet: every part the model knows about and what reaches what |
| [`docs/boot-chain.md`](docs/boot-chain.md) | The boot stages, from the VPU ROM to Linux, and what each one reads |
| [`docs/running.md`](docs/running.md) | Boot media, SD-card variants, OTP files, wall budgets |
| [`docs/device-tree.md`](docs/device-tree.md) | Getting the patched device tree out, and where `rpi-machine-id` comes from |
| [`docs/diagnostics.md`](docs/diagnostics.md) | The `--log` channels and `PIMU_*` switches that find a wall |
| [`docs/building.md`](docs/building.md) | Build profiles, the `diag` feature, PGO, the build's CPU share |
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
