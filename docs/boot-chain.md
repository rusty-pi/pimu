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

## Where `rpi-machine-id` comes from

The string a [`rpi-mkosi`](https://github.com/valtzu/rpi-mkosi) image turns into
its root-LUKS passphrase is derived in **stage 1**, not stage 2 — which is why
bumping the EEPROM can move the passphrase just as bumping `start4.elf` can.

The first stage builds a tagged handoff structure at `0xC004_0000` (`BSTE`,
then `BVER`, `BSTS`, `BSTN`, `BSTM`, `BUSB` sub-blocks) and, inside the `BVER`
block at offset `+0x8c`, deposits 16 bytes:

```
SHA-256( le32(otp[28]) ‖ le32(otp[35]) ‖ le32(otp[30])
         ‖ le32(otp[64]) ‖ le32(otp[65]) )  [..16]
```

- `otp[28]`, `otp[35]` — low and high halves of the 64-bit board serial
- `otp[30]` — the revision code
- `otp[64]`, `otp[65]` — the Ethernet MAC

Rows go in as native little-endian words, in that order (the order the
bootloader assembles them on its stack, not numeric row order), and the 32-byte
digest is truncated to its first 16 bytes. Only public identity rows take part;
nothing on this path touches the secure-boot key hash or the device private key.

`start4.elf` then only *republishes* it: `0x3ECC_5190` finds the `BVER` block and
does `memcpy(out, BVER + 0x8c, 16)`, and the caller hex-encodes the result into
`/chosen/rpi-machine-id`. `start4` carries its own fallback for a board with no
such block — a different function, `SHA-256(otp[28] ‖ otp[35] ‖ otp[30])` — but
on a normal boot it is never reached.

`src/identity.rs` recomputes the derivation from the modelled fuses, and a
`boot` run prints the prediction next to what the firmware actually published.
That is what lets CI answer "does this firmware pair keep the passphrase stable"
instead of only noticing afterwards that the value moved.

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
  `config.txt` hands to Linux.
- **The catch-all stub** — any peripheral offset nothing models reads back
  what was last written there (0 otherwise), and every access is logged, so an
  unimplemented poke becomes a triage note instead of a crash. The run report
  counts the accesses as `stub=`; `boot --stub-log` lists the offsets.
- **A network peer** on the other end of the GENET cable (`src/net/peer.rs`:
  DHCP, DNS, TFTP, plain HTTP over a minimal TCP). `--netboot <dir>` serves
  `<dir>`:
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
