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
(a) enough RAM and (b) the `0xC000_0000+x -> RAM[x]` alias before M3.

`pieeprom.bin`: 512 KiB SPI image, header magic `55 AA`. Contains the bootloader
binary plus a text config block (`BOOT_UART`, `BOOT_ORDER`, `BOOT_WATCHDOG_*`,
`FREEZE_VERSION`, ...) and format strings including
`"BOOTMODE: 0x%02x partition %d build-ts %s serial %08x boardrev %x stc %u"` —
that line is the natural first regression target for M2.

## Peripheral scope

Only what boot needs:

1. **USB3** — VL805 XHCI (PCIe-attached) for the boot disk
2. **Ethernet** — BCM GENET v5 (`0x7d58_0000`) for netboot
3. **Serial** — PL011 + mini-UART (done in M1)

Explicitly out: SD/EMMC, HDMI/display, camera, the 3D/QPU vector-graphics unit.
(The VPU *scalar* vector ALU ops may still need modelling if firmware uses them
for `memcpy`-style work — that is an ISA question, not a peripheral.)

## Milestones

- **M1 (done):** VPU scalar interpreter (subset) + UART/timer + regression
  harness. Proven on hand-assembled payloads.
- **M2:** run `pieeprom.bin` far enough to emit its `BOOTMODE:` banner. Needs:
  boot-ROM approximation, fuller VPU ISA, SPI/OTP + mailbox stubs, SDRAM-training
  fast-path.
- **M3:** `start4.elf` to ARM hand-off. Needs: SDRAM alias mapping, `fixup4.dat`
  apply, clock manager, mailbox property interface, and the scoped peripherals
  (USB3 boot disk / GENET netboot), plus VC4 vector ops as they turn up.
