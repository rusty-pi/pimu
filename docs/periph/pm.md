<!-- generated from specs/pm.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pm` – Power management: reset control, reset status, watchdog, power-domain registers

- Bus: `vpu` (VPU bus address)
- Base: `0x7E100000`
- Size: `0x1000`

Writes carry the 0x5A password in the top byte, which reads back masked. The watchdog counts down at 65536 Hz from the last WDOG value once RSTC arms a full reset.

Sources:

- linux (high): drivers/watchdog/bcm2835_wdt.c and drivers/pmdomain/bcm/bcm2835-power.c map this block
- decompile (high): bootloader self-update reset (WDOG = PASSWORD | 10, RSTC = PASSWORD | full reset); start4 arms the dog at 0x3ED62334 / 0x3ED62342 with dtparam=watchdog=on
- trace (high): --trace-mmio of the pinned firmware (pieeprom.bin and start4.elf as firmware.sha256 lists them) booted through the ARM release; every PC cited below is in those images

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x01C` | [`RSTC`](#rstc) | rw | 32 | 1, best high |
| `0x020` | [`RSTS`](#rsts) | rw | 32 | 2, best high |
| `0x024` | [`WDOG`](#wdog) | rw | 32 | 1, best high |
| `0x02C` | [`PADS2`](#pads2) | rw | 32 | 3, best high |
| `0x030` | [`PADS3`](#pads3) | rw | 32 | 3, best high |
| `0x034` | [`PADS4`](#pads4) | rw | 32 | 3, best high |
| `0x040`–`0x05C` (8 × 0x4) | [`DOMAIN_STATUS`](#domain_status) | rw | 32 | 1, best low |
| `0x074` | [`SPAREW`](#sparew) | rw | 32 | 2, best high |
| `0x07C` | [`AVS_RSTDR`](#avs_rstdr) | rw | 32 | 2, best high |
| `0x080` | [`AVS_STAT`](#avs_stat) | rw | 32 | 2, best high |
| `0x084` | [`AVS_EVENT`](#avs_event) | rw | 32 | 2, best high |
| `0x088` | [`AVS_INTEN`](#avs_inten) | rw | 32 | 2, best high |
| `0x108` | [`IMAGE`](#image) | rw | 32 | 3, best high |
| `0x10C` | [`GRAFX`](#grafx) | rw | 32 | 2, best high |
| `0x110` | [`PROC`](#proc) | rw | 32 | 2, best high |

## `RSTC`

Offset `0x01C` · access `rw` · 32 bits

Reset control. Arming a full reset starts the watchdog; clearing WRCFG stops it.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 5:4 | `WRCFG` | rw | 2 = full reset when the watchdog expires. |
| 31:24 | `PASSWD` | rw | Must be 0x5A for the write to count. |

Sources:

- linux (high): bcm2835_wdt_start writes PM_PASSWORD | 0x3222 style values, bcm2835_wdt_stop writes 0x102

`WRCFG` sources:

- linux (high): bcm2835_wdt.c: PM_RSTC_WRCFG_FULL_RESET = 0x20

`PASSWD` sources:

- linux (high): bcm2835_wdt.c: PM_PASSWORD = 0x5a000000

## `RSTS`

Offset `0x020` · access `rw` · 32 bits · reset `0x20`

Which reset source fired last. The bootloader also packs the partition to boot into the even bits 0..10, which the odd HADWRF bit does not disturb.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 5 | `HADWRF` | rw | Last reset was a full watchdog reset: what power-on sequencing leaves. |

Sources:

- measured (high): the reference board's bootloader prints 'PM_RSTS 00000020' after power-on
- decompile (high): partition field in the even bits (mask 0x555): FUN_00000578 / FUN_00000666 in firmware/source/pieeprom.bin.c

`HADWRF` sources:

- linux (high): bcm2835_wdt.c: PM_RSTS_HADWRF_SET

## `WDOG`

Offset `0x024` · access `rw` · 32 bits

Watchdog timeout on write, ticks left on read (1 tick = 1/65536 s).

| Bits | Field | Access | Notes |
|---|---|---|---|
| 19:0 | `TIME` | rw | Timeout in watchdog ticks; 0xFFFFF is 16 s. |
| 31:24 | `PASSWD` | rw | Must be 0x5A. |

Sources:

- linux (high): bcm2835_wdt.c: PM_WDOG_TIME_SET = 0x000fffff, get_timeleft reads it back

`TIME` sources:

- linux (high): bcm2835_wdt.c: PM_WDOG_TIME_SET

`PASSWD` sources:

- linux (high): bcm2835_wdt.c: PM_PASSWORD

## `PADS2`

Offset `0x02C` · access `rw` · 32 bits

Pad control for GPIO bank 0 (GPIO 0-27). start4 writes 0x19 with each pin it sets up from dt-blob.bin, and ends on 0x1F. Bits 3 and 4 are set in every value it writes.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 2:0 | `DRIVE` | rw | Drive strength, in 2 mA steps from 2 mA. |
| 31:24 | `PASSWD` | rw | Must be 0x5A. |

Sources:

- linux (high): drivers/pmdomain/bcm/bcm2835-power.c: PM_PADS2
- trace (high): start4 0x3ED55F88, interleaved with the GPIO function and pull writes of its pin configuration
- inferred (medium): bank: PADS3 and PADS4 end on the drive dt-blob.bin gives GPIO 40/41 and GPIO 46, which leaves bank 0 here

`DRIVE` sources:

- inferred (medium): the Pi 4B pin_config asks 16 mA of GPIO 40/41 (PADS3 ends on 7) and 14 mA of GPIO 46 (PADS4 ends on 6)

`PASSWD` sources:

- linux (high): bcm2835-power.c: PM_PASSWORD

## `PADS3`

Offset `0x030` · access `rw` · 32 bits

Pad control for GPIO bank 1 (GPIO 28-45), laid out as PADS2. start4 writes 0x19 with each pin it sets up and ends on 0x1F, the 16 mA dt-blob.bin gives the audio PWM pins.

Sources:

- linux (high): bcm2835-power.c: PM_PADS3
- trace (high): start4 0x3ED55F92
- inferred (medium): dt-blob.dts pins_4b: pin@p40 / pin@p41 drive_strength_mA = <16>

## `PADS4`

Offset `0x034` · access `rw` · 32 bits

Pad control for GPIO bank 2 (GPIO 46 up), laid out as PADS2. start4 writes 0x1E, the 14 mA dt-blob.bin asks of this bank.

Sources:

- linux (high): bcm2835-power.c: PM_PADS4
- trace (high): start4 0x3ED55F9C
- inferred (medium): dt-blob.dts pins_4b: pin@p46 drive_strength_mA = <14>, 'Dummy pin for HSTL drive on bank 2'

## `DOMAIN_STATUS`

Offset `0x040`, 8 elements 0x4 apart · access `rw` · 32 bits · reset `0x7040`

Power-domain words the firmware reads for 'powered, clocks stable'. Which domain each word is was not decoded; the model answers powered so the powered-up branch runs.

Sources:

- inferred (low): the model's choice: status reads here gate a branch the boot has to take

## `SPAREW`

Offset `0x074` · access `rw` · 32 bits

Linux's spare writable word. bootmain writes 0x400001 early on.

Sources:

- linux (high): bcm2835-power.c: PM_SPAREW
- trace (high): bootmain: 0x5A400001 at 0x000804AE

## `AVS_RSTDR`

Offset `0x07C` · access `rw` · 32 bits

Linux's BCM2835 name. start4 writes 0x4 to it and the next three words before it touches its first PLL, which on this SoC looks more like PLL power than AVS.

Sources:

- linux (medium): bcm2835-power.c: PM_AVS_RSTDR — _a BCM2835 define; the BCM2711 firmware's use does not fit the name_
- trace (high): start4: 0x5A000004 at 0x3ED55D4E

## `AVS_STAT`

Offset `0x080` · access `rw` · 32 bits

Linux's BCM2835 name. start4 writes 0x4 to it with AVS_RSTDR.

Sources:

- linux (medium): bcm2835-power.c: PM_AVS_STAT — _a BCM2835 define; the BCM2711 firmware's use does not fit the name_
- trace (high): start4: 0x5A000004 at 0x3ED55D52

## `AVS_EVENT`

Offset `0x084` · access `rw` · 32 bits

Linux's BCM2835 name. The bootcode pulses it (0x800004, then 0x4) right before it brings PLLC up, and start4 writes 0x4 with AVS_RSTDR.

Sources:

- linux (medium): bcm2835-power.c: PM_AVS_EVENT — _a BCM2835 define; the BCM2711 firmware uses it as PLLC's power control_
- trace (high): bootcode 0x8000AB8A / 0x8000AB8E, and bootmain again at 0x000AE4EE / 0x000AE4F2: 0x5A800004, 0x5A000004; start4: 0x5A000004 at 0x3ED55D54

## `AVS_INTEN`

Offset `0x088` · access `rw` · 32 bits

Linux's BCM2835 name. The bootcode pulses it (0x800004, then 0x4) right before it brings PLLD up, and start4 writes 0x4 with AVS_RSTDR.

Sources:

- linux (medium): bcm2835-power.c: PM_AVS_INTEN — _a BCM2835 define; the BCM2711 firmware uses it as PLLD's power control_
- trace (high): bootcode 0x8000AB8A / 0x8000AB8E, and bootmain again at 0x000AE4EE / 0x000AE4F2: 0x5A800004, 0x5A000004; start4: 0x5A000004 at 0x3ED55D56

## `IMAGE`

Offset `0x108` · access `rw` · 32 bits

Image power domain: ISP and H264 resets among others.

Sources:

- linux (high): bcm2835-power.c: PM_IMAGE, PM_ISPRSTN = BIT(8), PM_H264RSTN = BIT(7)
- decompile (high): power-domain switch 0x3ED54E40 writes 0x7E100108 & ~BIT(8) / & ~BIT(7)
- trace (high): start4: 0x5A0003C0 at 0x3ED48734, during its graphics bring-up

## `GRAFX`

Offset `0x10C` · access `rw` · 32 bits

Graphics power domain: the V3D reset.

Sources:

- linux (high): bcm2835-power.c: PM_GRAFX, PM_V3DRSTN = BIT(6)
- decompile (high): power-domain switch 0x3ED54E40 writes 0x7E10010C & ~BIT(6)

## `PROC`

Offset `0x110` · access `rw` · 32 bits

Linux's name for the processor power domain. The pinned start4 writes 0, 0x400, 0xC00 and 0xFFF to it after it has released the ARM, between PLLB hold and release writes. What the bits do here is not known.

Sources:

- linux (high): bcm2835-power.c: PM_PROC
- trace (high): start4: 0x5A000000 at 0x3EC82024, 0x5A000400 at 0x3EC82066, 0x5A000C00 at 0x3EC820AA, 0x5A000FFF at 0x3EC820EE
