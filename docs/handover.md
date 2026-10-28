# The handover block

The EEPROM bootloader leaves a tagged block in RAM for `start4.elf`, which hands
it on to the ARM. It sits at ARM physical `0x40000` (`0xC0040000` through the
VPU's uncached alias; the `bootbox` doorbell is rung with that address staged).

## Layout

| Offset | Size | Meaning |
|---|---|---|
| `+0x00` | 4 | `BSTE`, the magic (`0x45545342` read little-endian) |
| `+0x04` | 4 | Length of the whole block, `0x3b4` |
| `+0x08` | 4 | Offset of the first tag from the start of the block, `0xc` |

A tag starts with a four-letter ID, then two words: its own length including
this header, and the same length again (`0` in the last tag, `BCON`). The next
tag starts one length further on. Words are little-endian.

The tags in the order a stock 2026-08-04 bootloader writes them, with the block
offset of each:

| Tag | Offset | Length | Content |
|---|---|---|---|
| `BVER` | `0x00c` | `0x9c` | From `+0xc`: a NUL-terminated hash of the bootloader's source, the same one the log prints as `bootloader=`. At block offset `0x94`: SDRAM size in gigabits (`0x40` for 8 GB). Also holds the build date and time as text and the reset-info word |
| `BSTS` | `0x0a8` | `0x18` | Zero |
| `BSTN` | `0x0c0` | `0xc8` | Words and a string that identify the board (not decoded here) |
| `BSTM` | `0x188` | `0x2c` | Boot medium: the word at tag `+0xc` is `6` in every run seen; the one at `+0x10` is the medium |
| `BUSB` | `0x1b4` | `0x50` | USB-related words (not decoded here) |
| `RDSK` | `0x204` | `0x24` | The ramdisk, see below |
| `NVME`, `GPSE`, `IPV6`, `BPWR`, `BRTC`, `BSDR`, `BMMC`, `BSEC` | `0x228`… | | `BPWR` carries the boot watchdog: its end time (64-bit system timer microseconds) at block offset `0x300` and the budget in seconds at `0x308`. `BSEC` is 32 non-zero bytes in the signed-boot run and zero otherwise |
| `BCON` | `0x3a4` | `0x10` | Last tag |

Boot medium (`BSTM`, tag `+0x10`): `0` SD, `1` network, `2` USB on the VL805,
`4` USB-C, `5` ramdisk.

### `RDSK`

From tag `+0xc`: the ramdisk's address, its size in bytes, then four more words.
When the bootloader loaded a `boot.img`:

```
00000000: 00 00 00 18  00 00 40 00  00 20 00 00  00 02 00 00  01 00 00 00  01 00 00 00
          0x18000000   0x400000     0x2000       0x200        1            1
```

The image sits in RAM at `0x18000000`, as a bare FAT volume with no partition
table (a FAT boot sector, `eb 3c 90`, in its first bytes). The third and fourth
words are the volume's sector count and sector size; I could not tell what the
last two words, both `1`, mean. When no ramdisk was loaded the whole tag is
zero.

## How this was checked

The block is written by the stock bootloader's own code running in the VPU
model, so it is what that bootloader produces and not something pimu invents.
Two runs of nuboot's scenarios (`rusty-pi/nuboot`, `scenarios/usb-hello.yaml`
for a USB boot and `scenarios/signed-boot.yaml` for a ramdisk boot), each with

```bash
pimu boot --scenario scenarios/signed-boot.yaml --dump 0x40000:0x3c0 --dump 0x18000000:0x40
```

and the dump decoded by walking the tags from the offset at `+8`: the
lengths chain from `0xc` to `0x3a4` and end at the block length `0x3b4`; `BSTM`
read `2` in the USB run and `5` in the ramdisk run; `RDSK` was zero in the first
and held `0x18000000` / `0x400000` in the second, which is the size of
`out/boot-test.img` (4194304 bytes), and the bytes at `0x18000000` are a FAT
boot sector whose BPB says 512-byte sectors and `0x2000` sectors. The `BVER` hash
matched the `bootloader=` log line and the SDRAM size matched `ram_mib=8192`.

This has not been compared with a dump from a Raspberry Pi 4B d03115; the
layout is the model's reading of the same code.
