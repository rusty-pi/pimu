# rpi-virt-fw

A whole-machine Raspberry Pi 4 (BCM2711) emulator that boots the **real
firmware** — `pieeprom.bin`, `start4.elf`, `fixup4.dat` on the VideoCore VPU —
and then Linux on the four Cortex-A72 cores it releases.

The original goal, and still the main one, is to *execute the real blobs* in a
modelled SoC and regression-test their behaviour — primarily serial output —
against a known-good baseline, so a firmware bump (or a change to a
custom-built firmware) shows up as a transcript diff.

The boot firmware runs on the **VideoCore VPU**, not the ARM cores, which is
why an ARM emulator like QEMU's `raspi4b` cannot test it (it stubs the GPU
firmware out entirely). See [`docs/references.md`](docs/references.md) for the
survey of prior art — there is no off-the-shelf tool for this. Since
[#40](https://github.com/valtzu/rpi-virt-fw/issues/40) the ARM side is ours
too, Bochs-style: an interpreter, accuracy over speed, every core lock-stepped
in one host thread so a run is deterministic.

## Status

The full EEPROM → BOOTLOADER → `start4.elf` → `arm_loader` chain runs in the
model, from every boot medium CI checks: SD card, USB mass storage on the
VL805 and on the USB-C port's own xHCI, TFTP and HTTP network boot. `arm_loader` then releases the four A72 cores, the
firmware's own armstub drops them to EL2 and enters the kernel, and Linux boots
off the SD card's ext4 root partition to a busybox shell on the serial console
(a dev box reaches the prompt in about three minutes).

No firmware behaviour is short-circuited and the boot needs no opt-in shims or
environment variables. The one thing that fails on purpose is the HDMI EDID
read: the DDC I²C masters are modelled and nothing acknowledges the EDID
EEPROM's address, because the reference board has no monitor plugged in
([#15](https://github.com/valtzu/rpi-virt-fw/issues/15)).

Working:

- **VideoCore IV scalar interpreter** (`src/vpu/`) — the 16-, 32- and 48-bit
  scalar instruction forms `start4` actually executes, both VPU cores,
  exception and interrupt delivery through the ThreadX vector table.
  Instruction *lengths* are always decoded correctly, so unknown opcodes
  (the vector unit) degrade to `Unimpl` rather than derailing the PC.
- **AArch64 interpreter** (`src/aarch64/`) — integer A64, SIMD and floating
  point with ARM-exact soft-float, stage 1 MMU, exception levels EL3..EL0 and
  the system registers Linux touches. `tests/a64_diff.rs` checks it
  differentially against `qemu-aarch64` user-mode on random instruction
  streams. Four cores run in lock-step with the VPU, paced by the system timer
  (`src/arm/`, `src/armstub.rs`).
- **Machine model** (`src/machine.rs`, `src/periph/`) — RAM + the
  `0xC000_0000` uncached SDRAM alias + address decode + peripherals: PL011
  (transmit and receive) and mini-UART, system timer, SDRAM controller, clock
  manager + PLLs, config-OTP, power domains and watchdog, DMA4 and legacy DMA,
  Arasan eMMC2 (ADMA2/SDMA, writes, 1.8 V) + SD card, SPI0 EEPROM, GPIO with
  the board's pin map and the pads it hands the SPI and I²C masters, BSC/I²C
  with the DA9090 PMIC and FXL6408 GPIO expander, HDMI DDC, HVS, mailbox with
  the crypto service, RNG, AVS/PVT, PCIe + VL805 xHCI + a USB mass-storage
  device, the CYW43455's SDIO side and its Bluetooth modem, GENET with its
  BCM54213PE PHY, and on the ARM side the GIC-400, the
  ARM-local block and the generic timer. A logging catch-all takes the rest.
- **Network peer** (`src/net/`) — `--netboot <dir>` plugs the Ethernet cable
  into a built-in DHCP, DNS, TFTP and plain HTTP server serving `<dir>`
  ([#38](https://github.com/valtzu/rpi-virt-fw/issues/38)). `--net passt`
  plugs it into the host's network through [passt](https://passt.top/)
  instead, started on a socket pair (`--net passt:<socket>` connects to one
  already listening), for reaching a
  server on the host such as `mkosi serve`; that runs on the host's clock, so it
  is not deterministic and CI stays on the built-in peer
  ([#45](https://github.com/valtzu/rpi-virt-fw/issues/45)). HTTP boot works
  through it as is. TFTP boot needs static addresses, since passt's DHCP has no
  PXE option 43, and a TFTP server on the host's port 69
  ([recipe](https://github.com/valtzu/rpi-virt-fw/issues/45#issuecomment-5676281814)).
- **Serial console input** — `--send-after <prompt> <text>` types into the
  PL011 deterministically, keyed to the transcript; `--stdin` makes the host
  terminal the console for an interactive session (Ctrl-A x quits).
- **Register specs** (`specs/*.toml`) — machine-readable register maps with
  per-register provenance. `build.rs` generates the constants the device
  models match on, `tests/specs.rs` checks them against the model, and
  [`docs/periph/`](docs/periph/) is generated from them; its
  [README](docs/periph/README.md) lists every block converted so far. Each
  block also says what carries it (`parent`) and which interrupt lines it
  drives (`irq`), which is what the board sheet in
  [`docs/README.md`](docs/README.md) draws — and what
  `tests/board_sheet.rs` checks the drawing against.
- **Firmware pipeline** — `pieeprom.bin` self-update trailer, EEPROM config
  parse, GPT/MBR + FAT32 walk, `fixup4.dat`, RSA signature check.
- **Regression harness** (`src/harness/`) — TOML scenarios in, console
  transcript out, diffed against a golden file. `--update` to re-baseline.
  Each boot is a scenario in `testdata/boot/`: a golden transcript of the whole
  console, named milestones, each carrying the invariant it guards, and the
  instruction count each core retired.
- **CI** — `.github/workflows/boot-log.yml` runs fmt, clippy and the tests,
  then every boot in parallel on every push / PR to `main`: the firmware
  boot from SD, USB, the USB-C port, TFTP and HTTP, the SD boot again on a B0
  board (#77), the cut-down firmware (#105), and the Linux boot to a shell.
  The tests include `tests/specs.rs`, which fails when `docs/periph/` differs
  from what the specs generate.

Not done: most of the VPU vector unit (a short list of exactly matched forms
runs, the rest stop as `Unimpl`), HTTPS network boot (#44), and under Linux a
display and networking past the `bcmgenet` probe. Linux does reach `start4`'s
crypto service through `/dev/vcio_crypto` (`linux-boot.toml` checks the HMAC
rpi-mkosi#37 needs), and USB mass storage far enough to boot the rpi-mkosi
image with `--usb`.
See [`docs/`](docs/) for the board sheet and the rest:
[`boot-chain.md`](docs/boot-chain.md) for the stage-by-stage map,
[`diagnostics.md`](docs/diagnostics.md) for the environment variables that find
a wall, and [`vpu-isa.md`](docs/vpu-isa.md) for what the VPU interpreter
assumes.

## Quick start

```bash
cargo test                        # unit + ISA + peripheral + scenario tests
cargo run -- run-all -v           # run every scenario, show transcripts
cargo run -- run testdata/scenarios/hello-vpu.toml -v

./scripts/fetch-firmware.sh       # pull the real blobs, kernel and busybox into firmware/ (gitignored)
cargo run -- disasm firmware/start4.elf --base 0xcec00200 --count 40

# Run the real boot chain: EEPROM bootloader + start4.elf off an SD image,
# up to a kernel that parks the ARM.
KERNEL=halt ./scripts/make-sd.sh firmware/sd-halt.img
cargo run --release -- boot --eeprom firmware/pieeprom.bin \
  --sd firmware/sd-halt.img

# ...or on into Linux, with the terminal as the serial console.
./scripts/make-sd.sh                            # build firmware/sd.img
cargo run --release -- boot --eeprom firmware/pieeprom.bin \
  --sd firmware/sd.img --stdin
```

The ARM is always modelled (#52): the boot goes wherever the card's
`kernel8.img` takes it.

`boot` prints the serial console as it goes and ends with one line saying
whether the boot got where it was meant to (`result: ok — the firmware started
the ARM`), with exit status 1 when it did not. `-v` adds the full run report.

In a directory that holds the files themselves, every option naming one can be
left out: `pieeprom.bin` is `--eeprom`, and so are `sd.img`, `usb.img`,
`otg.img`, `netboot/`, `otp.json` or `otp.bin`, `bootconf.txt` (a `--bootconf`
line each) and `pubkey.bin` (#114). `rpi-virt-fw boot` on its own then boots
from what is there, and names on stderr what it picked up.

`make-sd.sh` needs `sfdisk`, `mtools` and `e2fsprogs`; no root or loop devices.
`WIRELESS=1 scripts/make-sd.sh firmware/sd-wireless.img` writes the card an
imager does, without the `dtoverlay=disable-bt` / `disable-wifi` lines: Linux's
console is then the mini-UART (`ttyS0`), the PL011 is the Bluetooth modem's,
and the WiFi chip answers on the legacy EMMC host
([#124](https://github.com/valtzu/rpi-virt-fw/issues/124)).
`BRCMFMAC=1 scripts/make-sd.sh firmware/sd-brcmfmac.img` writes that card with
the WiFi driver on it as well — `brcmfmac` and the modules it needs under
`/lib/modules`, a `modprobe` for the one the kernel fetches by itself, the
CYW43455's own firmware under `/lib/firmware/brcm`, and `insmod`/`ip` — which
is what `testdata/boot/wifi-boot.toml` loads by hand to pin how far the chip's
bring-up gets. `--usb <img>` boots
the same image as a USB stick instead, `--otg <img>` as a
stick in the USB-C socket (`BOOT_ORDER` 0x5, and `OTG=1 scripts/make-sd.sh` for
a card whose `config.txt` hands that controller to Linux), and
`scripts/make-netboot.sh` builds the root `--netboot` serves — the boot
scenarios in `testdata/boot/` carry the exact flags and EEPROM settings for
each medium, and `rpi-virt-fw boot-check <scenario> --plan` prints them.

`scripts/pgo-build.sh` does the release build with profile-guided
optimisation: an instrumented build runs the firmware boot and the start of
the Linux boot, and the release build is redone with what it counted. It needs
what `boot-check` needs and takes about 10 minutes on a Pi 4, where both
boots then run 1.45x faster; the guest runs the same instructions either way.
CI does not use it — the extra build and training cost more than the boot
jobs would save.

Cargo runs rustc through `scripts/rustc-wrapper.sh` (`.cargo/config.toml`),
which outside CI keeps the compiler on 80% of the CPUs so a build does not make
the desktop lag. `RVF_BUILD_CPU_PERCENT` changes the share (`100` lifts the
limit); with `CI` set the build gets every CPU.

### What the machine read and wrote

`--log io` writes what crossed the peripherals to stderr (or to `--log-file
<path>`), apart from the console: block runs on the SD card and the USB stick
with the files they belong to, the OTP rows the firmware read and programmed,
and what the network peer did
([#35](https://github.com/valtzu/rpi-virt-fw/issues/35)). The file names come
from the bench reading the image's partition table and FAT itself, so the
firmware stays a black box:

```text
   0.000944 io: otp  read  row 28  = 0x1aa2bb31  serial number
   6.270753 io: sd   read  lba 0x0+2  (partition table)
   6.271470 io: sd   read  lba 0x800+2  p1:(boot sector)
   6.278862 io: sd   read  lba 0x1014+5  p1:/config.txt, p1:/start4.elf
   6.291327 io: sd   read  lba 0x101c+4489  p1:/start4.elf, p1:/fixup4.dat
   6.393364 io: sd   read  lba 0x21a8+9  p1:/fixup4.dat, p1:/bcm2711-rpi-4-b.dtb
```

Each line starts with the model time in seconds, the way the firmware and
Linux stamp their own logs. An OTP line ends with what the row is for, after
Raspberry Pi's
[OTP register list](https://github.com/raspberrypi/documentation/blob/ecd7a8129d4f2cb908d6cbd6ea5a994e0091285d/documentation/asciidoc/computers/raspberry-pi/otp-bits.adoc)
([#101](https://github.com/valtzu/rpi-virt-fw/issues/101)), and with
`(blank)` when none of its fuses are programmed. A row the firmware programs
shows as `io: otp  write row <n> = <new>  <meaning> (was <old>)`, and what the
network peer did as `io: net  dhcp: ...`.
Programming works the way start4 drives the OTP block, key sequence first, and
a fuse only ever goes from 0 to 1
([#92](https://github.com/valtzu/rpi-virt-fw/issues/92)). To watch it, program two words of customer OTP (rows 36 and 37) the way
`vcmailbox 0x00038021 16 16 0 2 ...` does on a Pi, and read them back, on the
halt-kernel card (`KERNEL=halt scripts/make-sd.sh firmware/sd-halt.img`):

```bash
cargo run --release -- boot --eeprom firmware/pieeprom.bin \
  --sd firmware/sd-halt.img --log io \
  --mbox-property 0x00038021:16=0.2.0x11111111.0x22222222 \
  --mbox-property 0x00030021:16=0.2
```

A reset keeps the fuses, but the next run starts from the model's own unless
`--otp json:<file>` or `--otp binary:<file>` keeps them
([#93](https://github.com/valtzu/rpi-virt-fw/issues/93)). The file is read
before the boot when it exists, and written back after the run when the
firmware programmed a row. A missing file is created from the model's own
fuses, so it is also the way to get the array out, edit it, and boot a board
fused differently. `json:` is an object of row to value, one a line; `binary:`
has row n at byte 4n, little-endian. A file made from a real board's fuses
holds that board's secrets (see `CLAUDE.md`): keep it out of the repository.

`io` is one of the `--log` channels
([#95](https://github.com/valtzu/rpi-virt-fw/issues/95)); the others are one
device or core each, e.g. `--log io,pcie,xhci` for a USB boot, and
`docs/diagnostics.md` lists them. They share one output, so the lines come out
in the order things happened. `--log jsonl:io` writes one JSON object per line
instead, for tools.

### Getting the patched device tree out

`arm_loader` patches `/chosen` — `rpi-machine-id`, `rpi-serial64`,
`rpi-boardrev-ext`, `rpi-sdram-size-gbit` — into the device tree just before it
releases the ARM. Those are the values `rpi-mkosi`
[#37](https://github.com/valtzu/rpi-mkosi/issues/37) needs to compare across a
firmware bump, because `rpi-machine-id` feeds the root LUKS passphrase.

`boot -v` prints them on every run that gets that far:

```text
--- device tree handed to the ARM ---
  at 0x2eff1e00  totalsize 0xe1b5  version 17
  /chosen/rpi-serial64           "fa1e00231aa2bb31"
  /chosen/rpi-machine-id         "ed96a9bc626d9d0869ce37ee4aea025d"
  ...
```

and `--dump-fdt <path>` writes the blob itself, so two firmware versions can be
compared byte for byte:

```bash
cargo run --release -- boot --eeprom firmware/pieeprom.bin --sd firmware/sd-halt.img \
  --max-wall 200 --dump-fdt old.dtb
# …bump firmware/, rebuild the SD image, run again into new.dtb…
diff <(fdtdump old.dtb) <(fdtdump new.dtb)
```

The blob is the one the armstub hands the ARM (its `dtb_ptr32` word; the
firmware's own `Device tree loaded to 0x… (size 0x…)` log line when the ARM was
never released) and its FDT header is validated before anything is written, so
no address is hard-coded and the flag keeps working across firmware versions,
the cut-down `start4cd.elf` included, which logs nothing after it starts.
With a kernel that parks the ARM nothing overwrites the tree afterwards, so
reading it out at the end of the run is safe.

`arm_loader` does not compute `rpi-machine-id` itself. `0x3ECC5190` first looks
for the `BVER` block in the handoff table the EEPROM bootloader left behind and,
if it is there, hex-encodes the 16 bytes at `BVER+0x8c`; only with no such block
does it fall back to hashing OTP itself —
`SHA-256(otp[28] ‖ otp[35] ‖ otp[30])` truncated to 16 bytes, the three rows
being the serial low word, the serial high word and the revision code. A real
boot always has the block, and so does this bench (traced), so the value is
`pieeprom.bin`'s and `start4.elf` only publishes it at `0x3EC568F8`.

The identity behind it is this bench's own, not any real board's: every OTP row
involved is invented in `src/periph/configotp.rs`. See the OTP rule in
[`CLAUDE.md`](CLAUDE.md) for why a real board's must never be committed.

`--max-wall` defaults to 140 s (none with `--stdin`) and there is no
instruction cap unless you pass `--max-steps`. Reaching the last milestone takes
longer than 140 s, so each boot scenario carries its own budget (`wall_secs`,
overridable with `boot-check --max-wall`). `boot-check` runs one boot and
checks it two ways; it is what CI runs, so run it locally to reproduce a CI
failure.

### The boot scenarios

Each file in `testdata/boot/` describes one run (which EEPROM image, which boot
medium, what wall budget, what to type into the console) and everything
asserted about it:

| Scenario | Boot |
|---|---|
| `firmware-boot.toml` | SD card, through to `arm_loader` |
| `usb-boot.toml` | USB mass storage (`BOOT_ORDER` 0x4), no SD card |
| `otg-boot.toml` | USB mass storage on the USB-C port (`BOOT_ORDER` 0x5), no SD card |
| `tftp-boot.toml` | network boot over TFTP |
| `http-boot.toml` | HTTP boot of a signed `boot.img` ramdisk |
| `linux-boot.toml` | SD card, on into Linux: a busybox shell, then a few commands typed into it |

Each one has:

- the **golden transcript** in `testdata/boot/golden/`, the whole console
  diffed line by line. This is what catches output that *moved* or a value that
  shifted — a change no grep sees, because the line still matches somewhere.
  The firmware's own timestamps are stripped first: they are cycle-derived and
  reproduce exactly, but any change to what an instruction costs shifts all of
  them at once, which would bury the one line that did change.
- the **milestones**, substring assertions each carrying the reason it exists:
  the commit or issue that made it pass. This is what a raw diff cannot say —
  which invariant broke.
- the **retired counts** beside the golden (`golden/<name>.retired.toml`): how
  many instructions each VPU and ARM core ran (#85). They reproduce exactly
  from one machine to the next, so a change that makes a boot run differently
  without printing anything different fails here instead of going unnoticed.
  A run the wall clock cut off (`end TimeLimit`) fails outright: its counts
  only say how fast the host was.

All three are checked against a single boot; the wall clock has little
headroom, so nothing here runs the firmware twice.

```
cargo run --release -- boot-check testdata/boot/firmware-boot.toml   # the SD boot
cargo run --release -- boot-check testdata/boot/linux-boot.toml
cargo run --release -- boot-check testdata/boot/firmware-boot.toml --update       # re-record the golden and counts
cargo run --release -- boot-check testdata/boot/firmware-boot.toml --max-wall 600 # slower, busier machine
cargo run --release -- boot-check testdata/boot/usb-boot.toml --from boot-usb.log # an earlier run's pair
```

The combined output goes to `boot.log` (`--output` names another file) and the
console to `boot.log.console` beside it: the pair `--from` checks again
without booting. CI keeps neither; a failed boot prints `boot.log` into the
job log.

After an intentional change, `--update` and then read the golden and counts
diff in the commit: it is the change, spelled out. A change that only moves
the counts still needs the re-record, and the diff then documents that it did.

### Scenario file

```toml
name = "hello-vpu"
description = "Hand-assembled payload prints a line on the mini-UART, then swi."

[payload]
kind = "builtin"          # builtin | elf | raw
source = "hello"          # builtin name, or path for elf/raw
load_addr = 0x00010000

[machine]
ram_mb = 16
console = "mini-uart"     # pl011 | mini-uart | pins

[run]
max_steps = 10_000
unimpl = "fault"          # fault | skip  (skip = reconnaissance mode)

[golden]
path = "../golden/hello-vpu.txt"
```

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
  periph/       one file per block (see Status), stub.rs = catch-all + log
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
docs/           board-sheet.svg + README, boot-chain, diagnostics, vpu-isa,
                references, periph/
scripts/        fetch-firmware.sh, make-sd.sh, make-netboot.sh, pgo-build.sh,
                provision-eeprom.sh, make-dt-blob.py, vc4-xref.py
testdata/       scenarios/*.toml + golden/*.txt  (in-process, millisecond)
                boot/*.toml + boot/golden/  (the firmware and Linux boots)
                netboot/  test-only signing key for HTTP boot
```

## Design note — borrow graph

Emulators usually fight the borrow checker because "everything pokes the bus".
Here the ownership is a tree: `Emulator` owns the cores and `Machine` as
siblings; `Vpu::step` takes `&mut dyn Bus`; `Machine` implements `Bus` and owns every
peripheral, dispatching by address range to `&mut self` methods. No `Rc`,
no `RefCell`, no interior mutability.
