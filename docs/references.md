# References

## Existing tools surveyed (2026-09)

Nothing executes the real Pi 4 VPU boot blobs. The relevant prior art:

| project | what | usable here? |
|---|---|---|
| QEMU `raspi4b` | emulates the **ARM** side of BCM2711; skips GPU firmware, boots an ARM kernel directly | no — never runs `start4.elf` / `pieeprom.bin` |
| [`mgottschlag/vctools`](https://github.com/mgottschlag/vctools) | C emulator: VC4 VPU + BCM**2835** peripherals + instr-DB codegen | reference only (Pi 1 era, abandoned 2013, **no license**) |
| [`librerpi/rpi-open-firmware`](https://github.com/librerpi/rpi-open-firmware) | open-source *replacement* VPU bootloader (active) | reference for what the boot VPU does |
| [`hermanhermitage/videocoreiv`](https://github.com/hermanhermitage/videocoreiv) | ISA docs + JS disassembler | encoding source |
| [`ptesarik/vc4boot`](https://github.com/ptesarik/vc4boot) | small VC4 asm boot programs (MIT) | future test payloads |
| [binutils-vc4](https://github.com/poizan42/binutils-vc4) | assembler/disassembler (GPL) | opcode-table cross-check |
| [`Idein/py-videocore6`](https://github.com/Idein/py-videocore6) | Python library for GPGPU programming on Raspberry Pi 4 (GPL) | QPU only — not the boot VPU (see note below) |
| [`Terminus-IMRC/vc6qpudisas`](https://github.com/Terminus-IMRC/vc6qpudisas) | VideoCore VI **QPU** shader disassembler | QPU only — not the boot VPU (see note below) |

### "VC6" vs the boot VPU

The Pi 4 (BCM2711) 3D block is *VideoCore VI*, and its **QPU** (vector shader)
ISA did change from VC4. `py-videocore6` and `vc6qpudisas` target that QPU —
GPGPU compute kernels, not firmware.

The **scalar VPU** that executes `start4.elf` / `bootcode` is unchanged from the
VC4 lineage: `start4.elf` is built with the vc4 toolchain and disassembles
cleanly against Hermitage's VC4 tables (our decoder cross-checks 0 length
mismatches over the whole binary). So the QPU references only become relevant if
this bench ever needs to emulate GPU compute shaders — the machine-id / crypto /
DTB boot path never touches the QPU.

## ISA / hardware

- BCM2711 ARM Peripherals datasheet (Raspberry Pi)
- BCM2835 ARM Peripherals datasheet (still the best register-level doc for
  blocks unchanged since Pi 1: UART, AUX, system timer, mailbox, GPIO)
- Herman Hermitage, "VideoCore IV Programmers Manual" (community wiki)
- `raspberrypi/firmware` `boot/` and `hardware/` headers

## Boot / firmware

- <https://wiki.beyondlogic.org/index.php?title=Understanding_RaspberryPi_Boot_Process>
- `raspberrypi/rpi-eeprom` — `rpi-eeprom-config`, `firmware-2711/` images,
  `release-notes.md`
- Raspberry Pi bootloader configuration docs (`BOOT_ORDER`, `BOOT_UART`, ...)
