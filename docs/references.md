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

- [BCM2711 ARM Peripherals](https://datasheets.raspberrypi.com/bcm2711/bcm2711-peripherals.pdf),
  Raspberry Pi, release 4 (18 Jan 2022, `githash: cfcff44-clean`). The same
  document also sits on the Pi 4 product page as
  [`RP-008248-DS-1`](https://pip-assets.raspberrypi.com/categories/545-raspberry-pi-4-model-b/documents/RP-008248-DS-1-bcm2711-peripherals.pdf).
  Authoritative here for: the 35-bit address map and the legacy master window,
  AUX, BSC, DMA and DMA4, GPIO, the interrupt source tables, PCM/I2S, PWM,
  SPI, the system timer, the PL011 UART, the SP804 ARM-side timer, and the ARM
  mailboxes. Its two interrupt tables are the naming for every VPU source
  (Table 102, VC peripheral IRQ `n` is what the model calls source `64 + n`)
  and for the ETH_PCIe L2 lines (Table 103).
- [BCM2835 ARM Peripherals](https://datasheets.raspberrypi.com/bcm2835/bcm2835-peripherals.pdf),
  Broadcom, 2012. The BCM2711 document is derived from it, and it stays the
  better register-level text for blocks BCM2711 kept unchanged and did not
  re-document — the EMMC (Arasan) chapter above all, which the newer document
  drops entirely. Read it with the community errata beside it: the original
  has known wrong bit offsets. Where the two disagree about a BCM2711 block,
  the BCM2711 document wins — the GPIO pull control (`GPPUD` / `GPPUDCLK`
  replaced by `GPIO_PUP_PDN_CNTRL_REG`) and the interrupt controller are the
  two that bite.
- Herman Hermitage, "VideoCore IV Programmers Manual" (community wiki)
- The `bcm2708_chip` register headers Broadcom published with the
  `brcm_usrlib` sources, mirrored as
  [`rpi-registers.html`](https://www.felloff.net/text/rpi-registers.html). A
  BCM2835 header set, so it is right only where BCM2711 kept the block: the
  `mcsync` and `corectl` specs cite it for register names, offsets and access
  types, and every one of those rows has a `measured` or `trace` source beside
  it taken on a BCM2711. Names, offsets and bit positions are facts and are
  recorded as such; no description is copied.
- `raspberrypi/firmware` `boot/` and `hardware/` headers

### What neither datasheet documents

Everything on the VideoCore side of the chip, and everything Raspberry Pi added
for the Pi 4, is reverse-engineered here — from the decompiled firmware, from
Linux drivers and device trees, and from measurements on a reference board. In
spec terms, that is every block whose sources are `decompile`, `linux`,
`measured` or `trace` rather than `datasheet`: the clock manager and the A2W
PLLs, the PCIe root complex, GENET, the HVS / HDMI / pixel-valve display path,
AVS and PVT, the SDRAM controller, the OTP engine, the VPU's own core-control
and multicore-sync blocks, and the mailbox property interface carried over the
ARM mailboxes.

## Boot / firmware

- <https://wiki.beyondlogic.org/index.php?title=Understanding_RaspberryPi_Boot_Process>
- `raspberrypi/rpi-eeprom` — `rpi-eeprom-config`, `firmware-2711/` images,
  `release-notes.md`
- Raspberry Pi bootloader configuration docs (`BOOT_ORDER`, `BOOT_UART`, ...)
- [`nstarke/raspberrypi4-bootloader-analysis`](https://github.com/nstarke/raspberrypi4-bootloader-analysis)
  — Ghidra decompiler (`haruspex`) dump of the Pi 4 VideoCore IV boot ROM /
  early EEPROM stage (455 `FUN_xxxx.c` files, low addresses ~`0x000000a8+`).
  Reference for the boot-ROM approximation (M2) — how stage 0 sets up before
  the EEPROM bootloader. Not start4.elf.
