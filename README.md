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
  file, mcsync, the `0x7EE0` boot-box, and a logging catch-all for the rest.
- **Firmware pipeline** — `pieeprom.bin` self-update trailer, EEPROM config
  parse, GPT/MBR + FAT32 walk, `fixup4.dat`, RSA signature check.
- **Regression harness** (`src/harness/`) — TOML scenarios in, console
  transcript out, diffed against a golden file. `--update` to re-baseline.
- **CI** — `.github/workflows/boot-log.yml` runs the simulated boot on every
  push / PR to `main` and fails if it regresses before `arasan_emmc_open`.

Two opt-in shims are still needed to get as far as `clkm`
(`RVF_MBOX_KICK`, `RVF_GPIOMAN_SHIM`) — see the open issues. Current wall: the
clock-manager PLL frequency calibration never converges because the `0x7D5D`
frequency monitors aren't modelled ([#1](https://github.com/valtzu/rpi-virt-fw/issues/1)).

Not done: the VPU vector/float unit, USB3 (VL805) and GENET netboot, the ARM
kernel/DTB load and hand-off. See [`docs/boot-chain.md`](docs/boot-chain.md)
for the stage-by-stage map and [`docs/vision.md`](docs/vision.md) for the
longer-term direction (single `boot` command, disk-image mode, QEMU hand-off).

## Quick start

```bash
cargo test --lib --bins --tests   # unit + ISA + peripheral + scenario tests
cargo run -- run-all -v           # run every scenario, show transcripts
cargo run -- run testdata/scenarios/hello-vpu.toml -v

./scripts/fetch-firmware.sh       # pull the real blobs into firmware/ (gitignored)
cargo run -- disasm firmware/start4.elf --base 0xcec00200 --count 40

# Run the real boot chain: EEPROM bootloader + start4.elf off an SD image.
./scripts/make-sd.sh                            # build firmware/sd.img
RVF_MBOX_KICK=0xbef6d458 RVF_GPIOMAN_SHIM=1 \
  cargo run --release -- recon firmware/pieeprom.bin \
    --eeprom --sd firmware/sd.img --max-wall 240
```

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
  harness/      scenario parsing, transcript capture, golden diff
  payloads.rs   hand-assembled VPU test programs
docs/           boot-chain, vpu-isa, references, vision
scripts/        fetch-firmware.sh, make-sd.sh, provision-eeprom.sh, make-dt-blob.py
testdata/       scenarios/*.toml, golden/*.txt
```

## Design note — borrow graph

Emulators usually fight the borrow checker because "everything pokes the bus".
Here the ownership is a tree: `Emulator` owns `Vpu` and `Machine` as siblings;
`Vpu::step` takes `&mut dyn Bus`; `Machine` implements `Bus` and owns every
peripheral, dispatching by address range to `&mut self` methods. No `Rc`,
no `RefCell`, no interior mutability.
