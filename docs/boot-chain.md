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
│ 3. ARM kernel                          (four Cortex-A72 cores, modelled too) │
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
(see `board: boardrev d03115` in a `boot --eeprom` transcript).

## The EEPROM from Linux

The flash the bootloader came out of is reachable from the booted Linux too, and
that is the only way to put a new image on a board whose own bootloader stage has
no self-update: `rpi-eeprom-update` with `RPI_EEPROM_IMMEDIATE_UPDATE=1` writes
it with `flashrom -p linux_spi:dev=/dev/spidev0.0,spispeed=16000`, no
`recovery.bin` and no power cycle. It needs SPI0 moved off the header onto the
flash's own pins:

```
dtparam=spi=on
dtoverlay=audremap
dtoverlay=spi-gpio40-45
```

`spi-gpio40-45` puts the master on GPIO 40..42 and its three chip selects on GPIO
43..45 as plain outputs, and `audremap` is what takes PWM audio off 40/41 first.
`EEPROM_SPI=1 scripts/make-sd.sh firmware/sd-eeprom-spi.img` builds that card,
and `testdata/boot/eeprom-spi.yaml` boots it and reads the flash's JEDEC id and
its first bytes from userspace.

Linux drives the master quite differently from the firmware, and
`src/periph/spi0.rs` has all three shapes: `spi-bcm2835` never uses the native
chip select, so the flash follows GPIO 43's level rather than `CS.TA`; a transfer
of 96 bytes or more goes through the legacy DMA with `CS.DMAEN` set, which makes
`FIFO` 32 bits wide and `DLEN` the end of the transfer; and anything between its
polling limit and that runs off the master's interrupt. The image carries no
flashrom — it links libpci, libusb and libftdi — so the scenario drives a flash
command through the registers with `devmem`, and the driver's DMA and interrupt
paths are covered by `tests/peripherals.rs` instead.

## B0 and C0

The model is a C0 BCM2711 on a Pi 4B rev 1.5 (`d03115`) unless `boot
--stepping b0` makes it the first production stepping, on a rev 1.2 board
(`c03112`); `--board-rev <hex>` names another board. The stepping sets the VPU
`version` value (`0x0400_0161` on B0, `0x0400_0162` on C0) and where the ROM
stage puts its stand-ins for the ROM's OTP routines, which bootcode up to
2020-06-15 calls at per-stepping addresses (`src/soc/`,
`src/firmware/bootrom.rs`), and whether DMA channel 15 takes 40-bit
control blocks. The board revision also picks the PMICs on the I²C bus: a 4B
rev 1.5 has parts at `0x1B` and `0x1E`, a rev 1.4, the Pi 400 and the CM4 at
`0x1D` and `0x1E`, a rev 1.1 or 1.2 one part at `0x1D` (`src/periph/pmic.rs`). A B0 part was never
run against the model;
the B0 facts come from the bootcode's own tables and the public B0 UART logs in
the rpi-eeprom issues.

## Peripheral scope

Only what boot needs. Every modelled register block has a spec in `specs/`,
and [`docs/periph/README.md`](periph/README.md) is generated from them: that
is the current list of what the model covers, with provenance per register.
What isn't a spec'd register block:

- **Storage** — the SD card and the USB stick are disk images read on demand.
  Writes stay in memory and the image file is never modified, so every run is
  a first boot; `--usb-mb` makes the stick bigger than its image.
  `--usb <img>` puts the stick in blue socket A, on the VL805; `--otg <img>`
  puts it in the USB-C socket, on the BCM2711's own xHCI, which is what
  `BOOT_ORDER` digit `0x5` (`BCM-USB-MSD`) boots from and what `otg_mode=1` in
  `config.txt` hands to Linux. `--otg-dock` puts that stick behind a dock — two
  hubs, an Ethernet adapter and an empty card reader, five xHCI slots in all. The stock bootloader does not get through it, as on a
  Raspberry Pi 4B d03115 with a dock: the dock's hubs fail the first request
  within 10 ms of `SET_ADDRESS` with a transaction error, and a hub port reset
  takes 10 ms, during which a second reset or `SET_ADDRESS` can collide with it.
- **The catch-all stub** — any peripheral offset nothing models reads back
  what was last written there (0 otherwise), and every access is logged, so an
  unimplemented poke becomes a triage note instead of a crash. The run report
  counts the accesses as `stub=`; `boot --stub-log` lists the offsets.
- **A network peer** on the other end of the GENET cable (`src/net/peer.rs`:
  DHCP, DNS, TFTP, plain HTTP over a minimal TCP). `--tftp-boot <dir>` and
  `--http-boot <dir>` each serve a directory (the same one, for these boots):
  - TFTP (`--boot-order 0xf2`): the bootloader TFTPs `start4.elf` /
    `fixup4.dat`, and start4's own GENET driver fetches `config.txt`, the
    overlays, the dtb and `kernel8.img` through to `arm_loader`;
  - HTTP (`--boot-order 0xf7`): the bootloader fetches `boot.sig` and
    `boot.img` from `HTTP_HOST`/`net_install/` and boots the ramdisk. The
    default host forces HTTPS, so the scenario sets its own `HTTP_HOST` and
    `HTTP_PORT=80`; the image must be RSA-signed and the key must be in the
    EEPROM's `pubkey.bin` (`--eeprom-pubkey`, test key in `testdata/netboot/`)

Still out: HTTPS network boot, which is the bootloader's own TLS stack; a
camera and the 3D/QPU unit; and, on the display side, everything past the
bring-up the firmware itself does — no scenario loads Linux's KMS driver. The
[README](../README.md) keeps the current list.
