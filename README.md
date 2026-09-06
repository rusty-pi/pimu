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

## Status — milestone M1

Working:

- **VideoCore IV scalar interpreter** (`src/vpu/`) — a subset of the 16- and
  32-bit scalar instruction forms, fetch/decode/execute. Instruction *lengths*
  are always decoded correctly, so unknown opcodes degrade to `Unimpl` rather
  than derailing the PC.
- **Machine model** (`src/machine.rs`) — RAM + address decode + peripherals:
  PL011 and mini-UART (transmit capture), 1 MHz system timer, and a logging
  catch-all for the rest of the peripheral window.
- **Regression harness** (`src/harness/`) — TOML scenarios in, console
  transcript out, diffed against a golden file. `--update` to re-baseline.
- Runs on the real `start4.elf` (loads, decodes) — just doesn't get far yet.

Not done: the fuller VPU ISA (48-bit branches, `ldm`/`stm`, float, vector),
SDRAM/DDR training, SPI/OTP/mailbox, the boot-ROM step, ARM hand-off. Peripheral
scope is deliberately narrow — USB3 (boot disk), Ethernet, serial; no SD/EMMC,
display, or 3D. See [`docs/boot-chain.md`](docs/boot-chain.md) for the M2/M3 plan.

## Quick start

```bash
cargo test                        # unit tests + scenario regression
cargo run -- run-all -v           # run every scenario, show transcripts
cargo run -- run testdata/scenarios/hello-vpu.toml -v

./scripts/fetch-firmware.sh       # pull the real blobs into firmware/ (gitignored)
cargo run -- disasm firmware/start4.elf --base 0xcec00200 --count 40
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
  periph/       uart_pl011, aux (mini-uart), systimer, stub (catch-all + log)
  soc/          BCM2711 memory map
  firmware/     ELF32 loader; Payload abstraction
  emulator.rs   Emulator = Vpu + Machine, run loop
  harness/      scenario parsing, transcript capture, golden diff
  payloads.rs   hand-assembled VPU test programs
docs/           boot-chain, vpu-isa, references
scripts/        fetch-firmware.sh
testdata/       scenarios/*.toml, golden/*.txt
```

## Design note — borrow graph

Emulators usually fight the borrow checker because "everything pokes the bus".
Here the ownership is a tree: `Emulator` owns `Vpu` and `Machine` as siblings;
`Vpu::step` takes `&mut dyn Bus`; `Machine` implements `Bus` and owns every
peripheral, dispatching by address range to `&mut self` methods. No `Rc`,
no `RefCell`, no interior mutability.
