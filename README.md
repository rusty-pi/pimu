# rpi-virt-fw

A virtual bench for Raspberry Pi 4 (BCM2711) **VideoCore boot firmware**:
`pieeprom.bin`, `start4.elf`, `fixup4.dat`.

The goal is to *execute the real blobs* in a modelled SoC and regression-test
their behaviour — primarily serial output — against a known-good baseline, so a
firmware bump (or a change to a custom-built firmware) shows up as a transcript
diff.

These blobs run on the **VideoCore VPU**, not the ARM cores, which is why an
ARM emulator like QEMU's `raspi4b` cannot test them (it stubs the GPU firmware
out entirely). See [`docs/references.md`](docs/references.md) for the full
survey of prior art — there is no off-the-shelf tool for this.

## Status

The full EEPROM → BOOTLOADER → `start4.elf` chain runs in the model. A
`recon --eeprom` run boots the real `pieeprom.bin` through DDR bring-up, GPT +
FAT parsing off an SD image, the RSA-verified `start4.elf` load, and into
`start4`'s driver sequencer — it reads `config.txt` / `dt-blob.bin`, prints
`board: boardrev d03115`, and reaches the clock-manager (`clkm`) init phase
(model time ~20.8 s).

Working:

- **VideoCore IV scalar interpreter** (`src/vpu/`) — the 16-, 32- and 48-bit
  scalar instruction forms `start4` actually executes: branches (incl. 48-bit
  absolute), `ldm`/`stm`, `ld/st` addressing modes, ALU, `version`,
  coprocessor-register moves, exception/timer-IRQ delivery, dual VPU cores.
  Instruction *lengths* are always decoded correctly, so unknown opcodes
  (the vector unit) degrade to `Unimpl` rather than derailing the PC.
- **Machine model** (`src/machine.rs`, `src/periph/`) — RAM + `0xC000_0000`
  uncached SDRAM alias + address decode + peripherals: PL011 and mini-UART,
  1 MHz system timer (with busy-wait fast-forward), SDRAM controller, clock
  manager + A2W PLL, the `0x7D5D` VPU clock/PLL block, config-OTP, power
  domains, DMA4, Arasan eMMC + SD-card read, BSC/I²C + DA9090 PMIC register
  file, the two HDMI DDC I²C masters, mcsync, the `0x7EE0` boot-box, and a
  logging catch-all for the rest.
- **Firmware pipeline** — `pieeprom.bin` self-update trailer, EEPROM config
  parse, GPT/MBR + FAT32 walk, `fixup4.dat`, RSA signature check.
- **Regression harness** (`src/harness/`) — TOML scenarios in, console
  transcript out, diffed against a golden file. `--update` to re-baseline.
  The firmware boot is one of those scenarios
  (`testdata/boot/firmware-boot.toml`): a golden transcript of the whole
  console plus ~35 named milestones, each carrying the invariant it guards.
- **CI** — `.github/workflows/boot-log.yml` runs the simulated boot on every
  push / PR to `main` and fails if it regresses before `arasan_emmc_open`.

The boot needs no opt-in shims or environment variables any more — the real
ThreadX periodic tick (interrupt-enable bit plus vector-table entry 64) is now
always modelled, the way the hardware behaves. It gets as far as loading the
kernel, the device tree and the config overlays, and no firmware behaviour is
short-circuited anywhere: the last one to go was the HDMI EDID block read,
which now fails because the DDC I²C masters at `0x7EF04500` / `0x7EF09500` are
modelled and nothing acknowledges the EDID EEPROM's address — the reference
board has no monitor plugged in
([#15](https://github.com/valtzu/rpi-virt-fw/issues/15)).

`RVF_MBOX_KICK` is gone too: it released a dmalib transfer's completion word by
hand, which the firmware's own `dma_chan_interrupt` does now that the DMA
completion interrupt is modelled
([#3](https://github.com/valtzu/rpi-virt-fw/issues/3)).

Not done: the VPU vector/float unit, GENET netboot, and the ARM property
mailbox — which is what a booted Linux needs to reach `/dev/vcio` and the
firmware crypto service. See [`docs/boot-chain.md`](docs/boot-chain.md) for the
stage-by-stage map, [`docs/diagnostics.md`](docs/diagnostics.md) for the
environment variables that find a wall, and [`docs/vision.md`](docs/vision.md)
for the longer-term direction (single `boot` command, disk-image mode, keeping
the VideoCore running alongside QEMU).

## Quick start

```bash
cargo test                        # unit + ISA + peripheral + scenario tests
cargo run -- run-all -v           # run every scenario, show transcripts
cargo run -- run testdata/scenarios/hello-vpu.toml -v

./scripts/fetch-firmware.sh       # pull the real blobs into firmware/ (gitignored)
cargo run -- disasm firmware/start4.elf --base 0xcec00200 --count 40

# Run the real boot chain: EEPROM bootloader + start4.elf off an SD image.
./scripts/make-sd.sh                            # build firmware/sd.img
cargo run --release -- recon firmware/pieeprom.bin \
  --eeprom --sd firmware/sd.img
```

### Getting the patched device tree out

`arm_loader` patches `/chosen` — `rpi-machine-id`, `rpi-serial64`,
`rpi-boardrev-ext`, `rpi-sdram-size-gbit` — into the device tree just before it
releases the ARM. Those are the values `rpi-mkosi`
[#37](https://github.com/valtzu/rpi-mkosi/issues/37) needs to compare across a
firmware bump, because `rpi-machine-id` feeds the root LUKS passphrase.

Every `recon` run that gets that far prints them:

```text
--- device tree handed to the ARM ---
  at 0x2eff1e00  totalsize 0xe1b5  version 17
  /chosen/rpi-serial64           "fa1e00231aa2bb31"
  /chosen/rpi-machine-id         "2928640898f6b5035da98885da0ac498"
  ...
```

and `--dump-fdt <path>` writes the blob itself, so two firmware versions can be
compared byte for byte:

```bash
cargo run --release -- recon firmware/pieeprom.bin --eeprom --sd firmware/sd.img \
  --max-wall 200 --dump-fdt old.dtb
# …bump firmware/, rebuild the SD image, run again into new.dtb…
diff <(fdtdump old.dtb) <(fdtdump new.dtb)
```

The blob is found through the firmware's own `Device tree loaded to 0x… (size
0x…)` log line and its FDT header is validated before anything is written, so
no address is hard-coded and the flag keeps working across firmware versions.
Nothing overwrites the tree afterwards — the ARM that would consume it is not
modelled — so reading it out at the end of the run is safe.

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

`--max-wall` defaults to 140 s and there is no instruction cap unless you pass
`--max-steps`. Reaching the last milestone takes longer than 140 s, so the boot
scenario carries its own budget (`wall_secs`, 330 s, overridable with
`RVF_BOOT_WALL`). `scripts/boot-check.sh` runs that one boot and checks it two
ways; it is what CI runs, so run it locally to reproduce a CI failure.

### The boot scenario

`testdata/boot/firmware-boot.toml` describes the run (which EEPROM image, which
SD card, what wall budget) and everything asserted about it:

- the **golden transcript** in `testdata/boot/golden/`, the whole console
  diffed line by line. This is what catches output that *moved* or a value that
  shifted — a change no grep sees, because the line still matches somewhere.
  The firmware's own timestamps are stripped first: they are cycle-derived and
  reproduce exactly, but any change to what an instruction costs shifts all of
  them at once, which would bury the one line that did change.
- the **milestones**, substring assertions each carrying the reason it exists:
  the commit or issue that made it pass. This is what a raw diff cannot say —
  which invariant broke.

Both are checked against a single boot; the wall clock has little headroom, so
nothing here runs the firmware twice.

```
scripts/boot-check.sh                  # run it, check it
scripts/boot-check.sh --update         # re-record the golden from this run
RVF_BOOT_WALL=600 scripts/boot-check.sh   # slower machine, busier machine
```

After an intentional change, `--update` and then read the golden diff in the
commit: it is the change, spelled out.

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
console = "mini-uart"     # pl011 | mini-uart

[run]
max_steps = 10_000
unimpl = "fault"          # fault | skip  (skip = reconnaissance mode)

[golden]
path = "../golden/hello-vpu.txt"
```

## Layout

```
src/
  vpu/          VideoCore IV scalar core: length, decode, exec, registers
  bus.rs        Bus + MmioDevice traits
  mem.rs        RAM region
  machine.rs    Machine: owns RAM + peripherals, decodes addresses
  periph/       uart_pl011, aux, systimer, sdramc, clockman, clkmon, bsc,
                emmc2, sdcard, dma4, pm, configotp, corectl, bootbox,
                mcsync, stub (catch-all + log)
  soc/          BCM2711 memory map
  firmware/     ELF32 loader; EEPROM image parse; Payload abstraction
  emulator.rs   Emulator = Vpu + Machine, run loop
  harness/      scenario parsing, transcript capture, golden diff,
                boot.rs = the firmware-boot scenario and its milestones
  payloads.rs   hand-assembled VPU test programs
docs/           boot-chain, diagnostics, usb-xhci, vpu-isa, references, vision
scripts/        fetch-firmware.sh, make-sd.sh, provision-eeprom.sh, make-dt-blob.py
testdata/       scenarios/*.toml + golden/*.txt  (in-process, millisecond)
                boot/firmware-boot.toml + boot/golden/  (the firmware boot)
```

## Design note — borrow graph

Emulators usually fight the borrow checker because "everything pokes the bus".
Here the ownership is a tree: `Emulator` owns `Vpu` and `Machine` as siblings;
`Vpu::step` takes `&mut dyn Bus`; `Machine` implements `Bus` and owns every
peripheral, dispatching by address range to `&mut self` methods. No `Rc`,
no `RefCell`, no interior mutability.
