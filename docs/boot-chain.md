# BCM2711 / Raspberry Pi 4 boot chain

What the bench has to reproduce, stage by stage. Everything up to the ARM
hand-off runs on the **VideoCore VPU**, not the ARM cores.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ 0. BCM2711 boot ROM (on-die, VPU)                          [not in any blob] │
│    - minimal clock/UART bring-up                                             │
│    - reads the SPI EEPROM, loads the bootloader into VPU-visible SRAM        │
│    - jumps to it                                                             │
│    We approximate this: place pieeprom.bin's bootloader section at its       │
│    load address and start the VPU there.                                     │
├─────────────────────────────────────────────────────────────────────────────┤
│ 1. Second-stage bootloader  =  pieeprom.bin        (VPU)   [rpi-eeprom]      │
│    - parses embedded config (BOOT_UART, BOOT_ORDER, ...)                     │
│    - DDR4 PHY training -> SDRAM usable                                       │
│    - prints "BOOTMODE: ... serial ... boardrev ..." to UART (BOOT_UART=1)    │
│    - walks BOOT_ORDER, mounts boot media, loads start4.elf (+config.txt)     │
│    - loads fixup4.dat                                                        │
├─────────────────────────────────────────────────────────────────────────────┤
│ 2. GPU firmware  =  start4.elf                     (VPU)   [firmware]        │
│    - linked to run from the SDRAM uncached alias (~0x0EC0_0000 region);      │
│      REQUIRES stage 1 to have brought SDRAM up                               │
│    - fixup4.dat patches it to set the GPU/CPU memory split (gpu_mem)         │
│    - reads config.txt, sets clocks/PLLs, loads the DTB and ARM kernel        │
│    - starts the ARM stub, releases the ARM cores from reset                  │
├─────────────────────────────────────────────────────────────────────────────┤
│ 3. ARM kernel                                      (ARM)   -- out of scope   │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Observed facts (from the fetched blobs)

`start4.elf`: ELF32-LE, `e_machine = 137`, `e_entry = 0xcec00200`, 21 `PT_LOAD`
segments. Load addresses cluster around `0x0cec_xxxx` and `0x0ee0_xxxx`, most
with bit 31.. of the `0xC000_0000` uncached-SDRAM alias set. So the model needs
(a) enough RAM and (b) the `0xC000_0000+x -> RAM[x]` alias (both in place).

`pieeprom.bin`: 512 KiB SPI image, header magic `55 AA`. Contains the bootloader
binary plus a text config block (`BOOT_UART`, `BOOT_ORDER`, `BOOT_WATCHDOG_*`,
`FREEZE_VERSION`, ...) and format strings including
`"BOOTMODE: 0x%02x partition %d build-ts %s serial %08x boardrev %x stc %u"` —
that line was the first M2 regression target; the model now runs well past it
(see `board: boardrev d03115` in a `recon --eeprom` transcript).

## Peripheral scope

Only what boot needs. Modelled so far (`src/periph/`):

- **Serial** — PL011 + mini-UART (transmit capture)
- **System timer** — 1 MHz, with a busy-wait fast-forward
- **SDRAM controller** + the `0xC000_0000` uncached alias, including the LPDDR4
  mode-register port at `0x7E00_109C` — start4 polls MR4 (temperature-controlled
  refresh) once a second after the ARM handover and rescales the refresh
  interval from it
- **Clock manager** + A2W PLL (`0x7E10_1000`) and the `0x7D5D` VPU clock/PLL
  block — status bits forced ready; the analogue PLLs / frequency counters are
  *not* modelled (the current wall, [issue #1])
- **Arasan eMMC** (`0x7E34_0000`) + a read-only SD-card / FAT image backend
- **BSC/I²C master** + DA9090 PMIC register file (`0x7E20_5E00`)
- **HDMI DDC I²C masters** (`0x7EF0_4500`, `0x7EF0_9500`) — the EDID buses, with
  no monitor on either, so start4 gives up on EDID the way the reference board
  does
- **DMA4**, **power domains**, **config-OTP**, **CoreCtl**, **mcsync**, the
  `0x7EE0` boot-box, and a logging catch-all for everything else

- **PCIe root complex** (`0x7D50_0000`) — a register file only, so the block
  stops aliasing into DRAM; the link never comes up (see
  [`usb-xhci.md`](usb-xhci.md))

Still out: **USB3** (the VL805 xHCI behind that PCIe root complex — surveyed in
[`usb-xhci.md`](usb-xhci.md)) and **GENET** netboot — not needed while the
boot disk is an SD image; HDMI/display, camera, the 3D/QPU unit; and the VPU
*scalar* vector ALU (`memcpy`-style bulk ops are special-cased, the rest fall
through to `Unimpl`).

## Milestones

- **M1 (done):** VPU scalar interpreter (subset) + UART/timer + regression
  harness. Proven on hand-assembled payloads.
- **M2 (done):** run `pieeprom.bin` through its banner and boot-media
  selection — boot-ROM approximation, SPI/OTP + mailbox stubs, SDRAM fast-path,
  GPT/FAT walk off an SD image.
- **M3 (in progress):** `start4.elf` to ARM hand-off. Done: SDRAM alias,
  `fixup4.dat` apply, RSA verify, the driver sequencer up to `clkm`. Remaining:
  the clock/PLL model ([issue #1]), gpioman pin providers ([issue #2]), the
  async-mailbox completer ([issue #3]), a real DA9090/BSC model ([issue #4]),
  then the kernel/DTB load and ARM release.

[issue #1]: https://github.com/valtzu/rpi-virt-fw/issues/1
[issue #2]: https://github.com/valtzu/rpi-virt-fw/issues/2
[issue #3]: https://github.com/valtzu/rpi-virt-fw/issues/3
[issue #4]: https://github.com/valtzu/rpi-virt-fw/issues/4
