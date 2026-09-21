<!-- generated from specs/gpio.toml by `cargo run -- spec-docs --update` – do not edit -->

# `gpio` – The 58 GPIO pins: function select, output latch, pin levels, edge detect and the BCM2711 pull control

- Bus: `vpu` (VPU bus address)
- Base: `0x7E200000`
- Size: `0x1000`
- Interrupts: `ANY` VPU source 116 · `BANK0` VPU source 113 · `BANK1` VPU source 114 · `BANK1_MIRROR` VPU source 115 · `ANY` GIC id 148 (`GIC_SPI 116`) · `BANK0` GIC id 145 (`GIC_SPI 113`) · `BANK1` GIC id 146 (`GIC_SPI 114`) · `BANK1_MIRROR` GIC id 147 (`GIC_SPI 115`)

Pins 0..57, in two banks of 32. Every register but `GPFSEL` is a pair (`0` for pins 0..31, `1` for 32..57), and the model keeps a function, an output latch and a termination per pin. A `GPLEV` bit is the output latch for a pin whose function is `output`, and the termination otherwise — pull-up reads 1, pull-down and no pulling read 0 — because nothing outside the model drives a pin. A pin the firmware drives itself does move, though, so the detect enables work: an edge or a level latches `GPEDS` and raises the bank's interrupt line. What each pin is wired to is the board's, not the chip's: `src/periph/gpio.rs` carries that map for the 4B, the CM4 and the Pi 400, and the `gpio` log channel names the pin it reports. Which pins are muxed also decides what two masters reach: SPI0 the boot flash (GPIO 40..43 on ALT4, `specs/spi0.toml`) and I²C 0 the 40-pin header (GPIO 0/1 on ALT0, `specs/bsc.toml`).

Sources:

- datasheet (high): BCM2711 ARM Peripherals, chapter 5 (General Purpose I/O)
- linux (high): `gpio@7e200000`, `brcm,bcm2711-gpio`, `reg = <0x7e200000 0xb4>`, 58 `gpio-line-names` (`firmware/bcm2711-rpi-4-b.dtb`); driven by `pinctrl-bcm2835`
- measured (high): the whole window read through `/dev/gpiomem` on a Raspberry Pi 4B d03115 running Linux: `GPFSEL2` `0x12000000` (GPIO 28/29 on ALT5, the RGMII MDIO bus), `GPFSEL3` `0x3fffffff` (30..39 on ALT3, Bluetooth and the WiFi SDIO), `GPFSEL4` `0x64` (40/41 on ALT0, 42 an output), `GPLEV0` `0x1000c1ff`, `GPLEV1` `0x38fb`, every edge-detect register 0
- decompile (high): start4's GPIO driver: function select `FUN_0ecc94f8` (`&DAT_7e200000 + reg * 4`, three bits a pin), level `FUN_0ecc7fca` / `FUN_0ecc9478` (`GPSET`/`GPCLR`, pins up to `0x39`), pull `FUN_0ecc95ec` (the 2835 `GPPUD` path, `vcfw/drivers/chip/vciv/2708/gpio.c`) or `FUN_0ecc9762` -> `FUN_0ecc83e4(&DAT_7e2000e4, pin, pull)` (the BCM2711 one, which is what a Pi 4 takes)
- trace (high): a `firmware-boot` run touches `GPFSEL0`..`GPFSEL4`, `GPSET1`, `GPCLR1`, `PIN_MUX`, `PAD_CFG` and all four `PUP_PDN` registers, and nothing else in the window: no `GPLEV`, no edge detect and no legacy pull register

Interrupts (`ANY` VPU source 116 · `BANK0` VPU source 113 · `BANK1` VPU source 114 · `BANK1_MIRROR` VPU source 115 · `ANY` GIC id 148 (`GIC_SPI 116`) · `BANK0` GIC id 145 (`GIC_SPI 113`) · `BANK1` GIC id 146 (`GIC_SPI 114`) · `BANK1_MIRROR` GIC id 147 (`GIC_SPI 115`)):

Four lines, up while a pin they cover has its `GPEDS` bit latched: one a bank, and an 'any bank' line. The block is built for three banks and this SoC fills two, so bank 1's output is mirrored onto the third bank's line as well — a bank 0 edge raises `BANK0` and `ANY`, a bank 1 edge raises `BANK1`, `BANK1_MIRROR` and `ANY`. The VPU's own controller takes the same four as sources 113 to 116 (`64 + GPU IRQ`, as for `systimer` and `uart0`), and a firmware that wants an edge while the ARM is down enables `ANY` there. Nothing outside the model drives a pin, so the only edges are the ones the firmware makes itself — driving an output, or moving the termination of an input — and no firmware in a boot enables a detector, so the lines have yet to go up in a run.

- linux (high): `bcm2711.dtsi`: `&gpio { interrupts = <GIC_SPI 113 ...>, <GIC_SPI 114 ...>, <GIC_SPI 115 ...>, <GIC_SPI 116 ...> }`, and the `bcm283x.dtsi` comment above the node for which bank reaches which line
- linux (high): `gpio@7e200000`, `interrupts = <GIC_SPI 0x71 IRQ_TYPE_LEVEL_HIGH>, <GIC_SPI 0x72 IRQ_TYPE_LEVEL_HIGH>` (`firmware/bcm2711-rpi-4-b.dtb`) — _The device tree the pinned firmware carries names only the two bank lines; the kernel tree it came from names all four._
- inferred (medium): VPU sources 113 to 116: the legacy blocks' VPU source and `GIC_SPI` number are both `64 + GPU IRQ` (`systimer` 64 to 67, `uart0` 121, `emmc` 126), and the GPIO lines are GPU IRQs 49 to 52 — _Not seen in a trace: no boot enables a detector, so no line has been observed on either controller._
- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102: VC peripheral IRQs 49 to 52 are `GPIO 0` to `GPIO 3`, VPU sources 113 to 116 — _Confirms the inference above; which bank reaches which line is still the Linux binding’s, not the table’s._

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000`–`0x014` (6 × 0x4) | [`GPFSEL`](#gpfsel) | rw | 32 | 2, best high |
| `0x01C`–`0x020` (2 × 0x4) | [`GPSET`](#gpset) | w | 32 | 2, best high |
| `0x028`–`0x02C` (2 × 0x4) | [`GPCLR`](#gpclr) | w | 32 | 2, best high |
| `0x034`–`0x038` (2 × 0x4) | [`GPLEV`](#gplev) | r | 32 | 2, best high |
| `0x040`–`0x044` (2 × 0x4) | [`GPEDS`](#gpeds) | w1c | 32 | 1, best high |
| `0x04C`–`0x050` (2 × 0x4) | [`GPREN`](#gpren) | rw | 32 | 1, best high |
| `0x058`–`0x05C` (2 × 0x4) | [`GPFEN`](#gpfen) | rw | 32 | 1, best high |
| `0x064`–`0x068` (2 × 0x4) | [`GPHEN`](#gphen) | rw | 32 | 1, best high |
| `0x070`–`0x074` (2 × 0x4) | [`GPLEN`](#gplen) | rw | 32 | 1, best high |
| `0x07C`–`0x080` (2 × 0x4) | [`GPAREN`](#gparen) | rw | 32 | 1, best high |
| `0x088`–`0x08C` (2 × 0x4) | [`GPAFEN`](#gpafen) | rw | 32 | 1, best high |
| `0x094` | [`GPPUD`](#gppud) | rw | 32 | 2, best high |
| `0x098`–`0x09C` (2 × 0x4) | [`GPPUDCLK`](#gppudclk) | rw | 32 | 1, best high |
| `0x0D0` | [`PIN_MUX`](#pin_mux) | rw | 32 | 3, best high |
| `0x0D4` | [`PAD_CFG`](#pad_cfg) | rw | 32 | 2, best high |
| `0x0E4`–`0x0F0` (4 × 0x4) | [`PUP_PDN`](#pup_pdn) | rw | 32 | 3, best high |

## `GPFSEL`

Offset `0x000`, 6 elements 0x4 apart · access `rw` · 32 bits · reset `0x0`

Function select, three bits a pin, ten pins a register: 0 input, 1 output, 4 ALT0, 5 ALT1, 6 ALT2, 7 ALT3, 3 ALT4, 2 ALT5. `GPFSEL5` holds pins 50..57; the eighteen bits above them read 0. The firmware rewrites `GPFSEL4` constantly, because GPIO 40..43 carry both the SPI NOR flash on ALT4 (`specs/spi0.toml`) and, on a 4B, PWM audio on 40/41 and the activity LED as an output on 42.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPFSEL0`..`GPFSEL5`
- trace (high): start4 puts GPIO 0/1 on ALT0 for the HAT EEPROM (`GPFSEL0` <- `0x4`, then `0x24`, at `0x3ECC9562`) and 14/15 on ALT0 for the console (`GPFSEL1` `0x24000`)

## `GPSET`

Offset `0x01C`, 2 elements 0x4 apart · access `w` · 32 bits · reset `0x6770696F`

Write 1 to drive a pin high; a 0 bit does nothing. A read answers `0x6770696f` — `"gpio"`, the block's own tag — which is what the `reset` here records. The EEPROM bootloader reads the register before it writes it (`ld r3, [r1+32]; bitset r3, #10; st r3, [r1+32]` at `0x8000A706`, the activity LED on GPIO 42), so on a real board it writes `0x67706d6f` and the stray bits do nothing because those pins are not outputs.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPSET0` / `GPSET1`
- measured (high): `/dev/gpiomem` on a Raspberry Pi 4B d03115: `0x1c` and `0x20` both read `0x6770696f`, as do `GPCLR0/1` and the reserved words between them

## `GPCLR`

Offset `0x028`, 2 elements 0x4 apart · access `w` · 32 bits · reset `0x6770696F`

Write 1 to drive a pin low. Reads answer the block tag, as `GPSET` does.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPCLR0` / `GPCLR1`
- measured (high): `/dev/gpiomem` on a Raspberry Pi 4B d03115: `0x28` and `0x2c` read `0x6770696f`

## `GPLEV`

Offset `0x034`, 2 elements 0x4 apart · access `r` · 32 bits · reset `0x0`

The level of each pin. An output reads its own latch; anything else reads its termination, since nothing outside the model drives a pin.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPLEV0` / `GPLEV1`
- measured (high): `/dev/gpiomem` on a Raspberry Pi 4B d03115: `GPLEV0` `0x1000c1ff`, `GPLEV1` `0x38fb`

## `GPEDS`

Offset `0x040`, 2 elements 0x4 apart · access `w1c` · 32 bits · reset `0x0`

Edge / level detect status, one bit a pin, cleared by writing 1, and the two interrupt lines follow it. A level detect re-latches its bit for as long as the pin sits at that level, so clearing it there only holds until the next access. A detector watches the pad, so a pin the firmware drives itself is detected like any other — that part is the model's reading of the block, not something a boot has shown.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPEDS0` / `GPEDS1`

## `GPREN`

Offset `0x04C`, 2 elements 0x4 apart · access `rw` · 32 bits · reset `0x0`

Rising-edge detect enable.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPREN0` / `GPREN1`

## `GPFEN`

Offset `0x058`, 2 elements 0x4 apart · access `rw` · 32 bits · reset `0x0`

Falling-edge detect enable.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPFEN0` / `GPFEN1`

## `GPHEN`

Offset `0x064`, 2 elements 0x4 apart · access `rw` · 32 bits · reset `0x0`

High-level detect enable.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPHEN0` / `GPHEN1`

## `GPLEN`

Offset `0x070`, 2 elements 0x4 apart · access `rw` · 32 bits · reset `0x0`

Low-level detect enable.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPLEN0` / `GPLEN1`

## `GPAREN`

Offset `0x07C`, 2 elements 0x4 apart · access `rw` · 32 bits · reset `0x0`

Asynchronous rising-edge detect enable.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPAREN0` / `GPAREN1`

## `GPAFEN`

Offset `0x088`, 2 elements 0x4 apart · access `rw` · 32 bits · reset `0x0`

Asynchronous falling-edge detect enable.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPAFEN0` / `GPAFEN1`

## `GPPUD`

Offset `0x094` · access `rw` · 32 bits · reset `0x0`

The BCM2835 pull control: a direction here, then the pins in `GPPUDCLK`. The BCM2711 pads do not listen to it — `GPIO_PUP_PDN_CNTRL_REG` replaces it — so the model keeps the word and changes no pull. start4 has both paths and takes the BCM2711 one on a Pi 4, so nothing in a boot writes this.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPPUD`, marked as having no effect on this chip
- decompile (high): start4 `FUN_0ecc95ec` writes `GPPUD` then `GPPUDCLK`, with two short delay loops around it, but only when the flag at `gp+0x1564` is clear; `FUN_0ecc9762` takes the `GPIO_PUP_PDN_CNTRL_REG0` path instead

## `GPPUDCLK`

Offset `0x098`, 2 elements 0x4 apart · access `rw` · 32 bits · reset `0x0`

Which pins the `GPPUD` direction applies to on a BCM2835. No effect here either.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPPUDCLK0` / `GPPUDCLK1`

## `PIN_MUX`

Offset `0x0D0` · access `rw` · 32 bits · reset `0x0`

Undocumented, and the firmware writes it on every boot. Bit 1 routes the SD card slot: set for the legacy EMMC controller at `0x7E300000`, clear for EMMC2 — which is what the model acts on. Bit 0 is the first thing start4's Ethernet pin setup does, before it puts GPIO 28/29 on ALT5 (the RGMII MDIO bus) and terminates 46..57, and a running board reads `1`: it looks like the RGMII pad bank, but nothing proves that, and the model only stores the bit.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 1 | `SD_LEGACY` | rw | Set: the card is on the legacy EMMC controller. Clear: on EMMC2. |

Sources:

- decompile (medium): start4db `FUN_0ed0fd54`, whose neighbours assert out of `tools/bootrom/rpiboot/genet.c`: `_DAT_7e2000d0 | 1` first, then GPIO 28/29 to function 2 (ALT5), 28 pulled up and 29 down (the driver's pull enum is the BCM2835 one, 1 down / 2 up), then 46..57 pulled down. Elsewhere `_DAT_7e2000d0 & 0xfffffffd | 1`, and `& 0xfffffffd` before EMMC2 is used
- trace (high): pieeprom-2020-09-03 writes `0x2` right before it drives the legacy EMMC and never touches EMMC2 (#66); the 2026 bootloader never writes the register; start4 sets bit 0 at `0x3ED4A1CE` and boots from EMMC2
- measured (high): `/dev/gpiomem` on a Raspberry Pi 4B d03115 booted from an SD card: `0xd0` reads `0x00000001`

`SD_LEGACY` sources:

- trace (high): 2020-era bootcode sets it before its SD init on `0x7E300000`; start4db clears it (`& 0xfffffffd`) before it uses EMMC2 (#66)

## `PAD_CFG`

Offset `0x0D4` · access `rw` · 32 bits · reset `0x0`

Undocumented. Only the EEPROM bootloader writes it, and only from its DRAM bring-up: `0xc000` in one case and `0` in the other, each with a pair of words at `0x7C40_4380` / `0x7C40_43A8`. A running board reads 0, and the model only stores the word.

Sources:

- decompile (medium): bootloader `FUN_00006394`: one branch writes `0x7f` / `0` / `0` to `0x7C404380`, `0x7C4043A8` and `PAD_CFG`, the other `0x10` / `0x1e000000` / `0xc000`. Its callers are the DRAM path — `FUN_000064ec`, which trains through `0x7DC30000` and writes `SDC` at `0x7E001000`, calls it with 0 when it is done and 1 while it runs
- measured (high): `/dev/gpiomem` on a Raspberry Pi 4B d03115: `0xd4` reads 0

## `PUP_PDN`

Offset `0x0E4`, 4 elements 0x4 apart · access `rw` · 32 bits · reset `0x0`

The BCM2711 pull control: two bits a pin, sixteen pins a register — 0 no pulling, 1 pull-up, 2 pull-down, 3 reserved. `PUP_PDN3` holds pins 48..57. This is the register the firmware actually uses, and the one a `GPLEV` bit falls back on for a pin that is not an output.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, GPIO: `GPIO_PUP_PDN_CNTRL_REG0`..`REG3`
- measured (high): `/dev/gpiomem` on a Raspberry Pi 4B d03115: `REG0` `0x4aa95555` — GPIO 0..7 at `0b01`, the pull-ups the ID EEPROM and I²C 1 lines need — `REG1` `0x19aaaaaa`, `REG2` `0x55505544`, `REG3` `0xaaaaa`
- trace (high): start4 ends a boot with `REG0` `0x40000005`: GPIO 0 and 15 pulled up, 14 not pulled, which is `pin@p14` / `pin@p15` of `pins_4b` in `firmware/dt-blob.dts`
