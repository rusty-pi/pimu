<!-- generated from specs/pm.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pm` – Power management: reset control, reset status, watchdog, power-domain registers

- Bus: `vpu` (VPU bus address)
- Base: `0x7E100000`
- Size: `0x1000`

Writes carry the 0x5A password in the top byte, which reads back masked. The watchdog counts down at 65536 Hz from the last WDOG value once RSTC arms a full reset.

Sources:

- linux (high): drivers/watchdog/bcm2835_wdt.c and drivers/pmdomain/bcm/bcm2835-power.c map this block
- decompile (high): bootloader self-update reset (WDOG = PASSWORD | 10, RSTC = PASSWORD | full reset); start4 arms the dog at 0x3ED62334 / 0x3ED62342 with dtparam=watchdog=on

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x01C` | [`RSTC`](#rstc) | rw | 32 | 1, best high |
| `0x020` | [`RSTS`](#rsts) | rw | 32 | 2, best high |
| `0x024` | [`WDOG`](#wdog) | rw | 32 | 1, best high |
| `0x040`–`0x05C` (8 × 0x4) | [`DOMAIN_STATUS`](#domain_status) | rw | 32 | 1, best low |
| `0x108` | [`IMAGE`](#image) | rw | 32 | 2, best high |
| `0x10C` | [`GRAFX`](#grafx) | rw | 32 | 2, best high |

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

## `DOMAIN_STATUS`

Offset `0x040`, 8 elements 0x4 apart · access `rw` · 32 bits · reset `0x7040`

Power-domain words the firmware reads for 'powered, clocks stable'. Which domain each word is was not decoded; the model answers powered so the powered-up branch runs.

Sources:

- inferred (low): the model's choice: status reads here gate a branch the boot has to take

## `IMAGE`

Offset `0x108` · access `rw` · 32 bits

Image power domain: ISP and H264 resets among others.

Sources:

- linux (high): bcm2835-power.c: PM_IMAGE, PM_ISPRSTN = BIT(8), PM_H264RSTN = BIT(7)
- decompile (high): power-domain switch 0x3ED54E40 writes 0x7E100108 & ~BIT(8) / & ~BIT(7)

## `GRAFX`

Offset `0x10C` · access `rw` · 32 bits

Graphics power domain: the V3D reset.

Sources:

- linux (high): bcm2835-power.c: PM_GRAFX, PM_V3DRSTN = BIT(6)
- decompile (high): power-domain switch 0x3ED54E40 writes 0x7E10010C & ~BIT(6)
