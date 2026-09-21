# rpi-virt-fw

A whole-machine Raspberry Pi 4 (BCM2711) emulator that boots the **real
firmware** — `pieeprom.bin`, `start4.elf`, `fixup4.dat` on the VideoCore VPU —
and then Linux on the four Cortex-A72 cores it releases.

The point is to *execute the real blobs* in a modelled SoC and regression-test
what they do — primarily their serial output — against a known-good baseline,
so a firmware bump, or a change to a custom-built firmware, shows up as a
transcript diff.

The boot firmware runs on the **VideoCore VPU**, not the ARM cores, which is why
an ARM emulator like QEMU's `raspi4b` cannot test it: it stubs the GPU firmware
out entirely. Since [#40](https://github.com/valtzu/rpi-virt-fw/issues/40) the
ARM side is ours too, Bochs-style — an interpreter, accuracy over speed, every
core lock-stepped in one host thread, so a run is deterministic.
[`docs/references.md`](docs/references.md) surveys the prior art; there is no
off-the-shelf tool for this.

## Quick start

```bash
./scripts/fetch-firmware.sh          # real blobs, kernel and busybox into firmware/ (gitignored)
KERNEL=halt ./scripts/make-sd.sh firmware/sd-halt.img

# The real boot chain: EEPROM bootloader + start4.elf off an SD image,
# up to a kernel that parks the ARM.
cargo run --release -- boot --eeprom firmware/pieeprom.bin --sd firmware/sd-halt.img

# ...or on into Linux, with the terminal as the serial console (Ctrl-A x quits).
./scripts/make-sd.sh                 # firmware/sd.img
cargo run --release -- boot --eeprom firmware/pieeprom.bin --sd firmware/sd.img --stdin
```

`make-sd.sh` needs `sfdisk`, `mtools` and `e2fsprogs`; no root, no loop devices.

`boot` prints the serial console as it goes and ends with one line saying
whether the boot got where it was meant to (`result: ok — the firmware started
the ARM`), with exit status 1 when it did not. `-v` adds the full run report.
In a directory that holds the files themselves every option naming one can be
left out — `pieeprom.bin` is `--eeprom`, and so are `sd.img`, `usb.img`,
`otg.img`, `netboot/`, `otp.json` or `otp.bin`, `bootconf.txt` (a `--bootconf`
line each) and `pubkey.bin`
([#114](https://github.com/valtzu/rpi-virt-fw/issues/114)) — so
`rpi-virt-fw boot` boots from what is there and names on stderr what it picked
up. [`docs/running.md`](docs/running.md) has the boot media, the cards and the
rest of the options; `rpi-virt-fw boot --help` lists them all.

## Commands

| Command | What it does |
|---|---|
| `boot` | Boot the machine from an EEPROM image, as a Pi 4 does, or run a VPU ELF. |
| `boot-check <scenario.toml>` | Run the boot a scenario describes and check its transcript, milestones and retired counts. `--plan` prints the `boot` invocation instead. |
| `run <scenario.toml>`, `run-all [<dir>]` | The in-process scenarios, against their golden transcripts. |
| `disasm <file>` | Disassemble a flat binary or ELF with the VPU decoder: `disasm firmware/start4.elf --base 0xcec00200 --count 40`. |
| `spec-docs [--update]` | Check (or regenerate) `docs/periph/` against `specs/*.toml`, and the dark board sheet against the hand-drawn one. |

## Features

The whole chain: EEPROM → BOOTLOADER → `start4.elf` → `arm_loader` from SD card,
USB mass storage on the VL805 and on the USB-C port's own xHCI, TFTP and HTTP
network boot; then the four A72 cores `arm_loader` releases, through the
firmware's own armstub into the kernel, and Linux off the card's ext4 root to a
busybox shell on the serial console. No firmware behaviour is short-circuited,
and no boot needs an opt-in shim or environment variable.

- **VideoCore IV scalar interpreter** (`src/vpu/`) — the 16-, 32- and 48-bit
  scalar forms `start4` executes, both VPU cores, exception and interrupt
  delivery through the ThreadX vector table. Instruction *lengths* always decode
  correctly, so unknown opcodes (the vector unit) degrade to `Unimpl` rather
  than derailing the PC.
- **AArch64 interpreter** (`src/aarch64/`) — integer A64, SIMD and floating
  point with ARM-exact soft-float, stage 1 MMU, exception levels EL3..EL0 and
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
  into a built-in DHCP, DNS, TFTP and plain HTTP server serving `<dir>`
  ([#38](https://github.com/valtzu/rpi-virt-fw/issues/38)). `--net passt` plugs
  it into the host's network through [passt](https://passt.top/) instead,
  started on a socket pair (`--net passt:<socket>` connects to one already
  listening), for reaching a server on the host such as `mkosi serve`; that runs
  on the host's clock, so it is not deterministic and CI stays on the built-in
  peer ([#45](https://github.com/valtzu/rpi-virt-fw/issues/45)). HTTP boot works
  through it as is; TFTP boot needs static addresses, since passt's DHCP has no
  PXE option 43, and a TFTP server on the host's port 69
  ([recipe](https://github.com/valtzu/rpi-virt-fw/issues/45#issuecomment-5676281814)).
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
- **Regression harness** (`src/harness/`) — scenarios in, console transcript
  out, diffed against a golden file. See
  [`testdata/README.md`](testdata/README.md).

### Limits

The HDMI EDID read fails on purpose: the DDC I²C masters are modelled and
nothing acknowledges the EDID EEPROM's address, because the reference board has
no monitor plugged in
([#15](https://github.com/valtzu/rpi-virt-fw/issues/15)).

Not there yet: most of the VPU vector unit (a short list of exactly matched
forms runs, the rest stop as `Unimpl`), HTTPS network boot
([#44](https://github.com/valtzu/rpi-virt-fw/issues/44)), and under Linux a
display and networking past the `bcmgenet` probe. Linux does reach `start4`'s
crypto service through `/dev/vcio_crypto` (`linux.toml` checks the HMAC
[rpi-mkosi#37](https://github.com/valtzu/rpi-mkosi/issues/37) needs), and USB
mass storage far enough to boot the rpi-mkosi image with `--usb`.

## Tests

```bash
cargo test                                                    # unit + ISA + peripheral + scenario tests
cargo run -- run-all -v                                       # every in-process scenario, with transcripts
cargo run --release -- boot-check testdata/boot/firmware.toml # one real boot, checked three ways
```

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
  identity.rs   the rpi-machine-id derivation
  log/          --log channels; fatmap.rs = which file a disk block belongs to
  stdio.rs      host terminal as the serial console (--stdin)
  diag.rs       RVF_* diagnostics
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
| [`docs/diagnostics.md`](docs/diagnostics.md) | The `--log` channels and `RVF_*` switches that find a wall |
| [`docs/building.md`](docs/building.md) | Build profiles, the `diag` feature, PGO, the build's CPU share |
| [`docs/periph/`](docs/periph/) | One page per register block, generated from `specs/*.toml` |
| [`docs/vpu-isa.md`](docs/vpu-isa.md) | The VideoCore IV instruction set, with the evidence for each statement |
| [`docs/references.md`](docs/references.md) | Outside sources: datasheets, kernel drivers, other reverse engineering |
| [`testdata/README.md`](testdata/README.md) | The scenario files, the goldens and the images each boot needs |
