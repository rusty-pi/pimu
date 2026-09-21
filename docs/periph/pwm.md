<!-- generated from specs/pwm.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pwm` – PWM0: two pulse-width / serialiser channels and the FIFO they share

- Bus: `vpu` (VPU bus address)
- Base: `0x7E20C000`
- `PWM1` copy: `0x7E20C800`
- Size: `0x28`

Two of these blocks, `0x800` apart. A 4B routes PWM0's two channels to GPIO 40 and 41 (`PWM0_0` / `PWM0_1` on ALT0), where the board's analogue audio sits, and the firmware has to leave those pins alone for the SPI flash on ALT4 (`specs/spi0.toml`). Nothing in a boot programs either block: the firmware plays no sound, and Linux drives its analogue audio through the mailbox rather than here. The model is the register map and the FIFO flags a driver would poll — a write to `FIF1` goes nowhere, no channel ever runs, and no output is generated.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6: "The PWM0 register base address is `0x7e20c000` and the PWM1 register base address is `0x7e20c800`"
- linux (high): `bcm2711.dtsi`: `pwm@7e20c000` and `pwm@7e20c800`, both `brcm,bcm2835-pwm`, with the PWM clock from CPRMAN

`PWM1` copy:

The second block. Its channels reach GPIO 12/13 on ALT0 and 18/19 on ALT5; the DREQs it paces are `PWM1` (1) rather than `PWM0` (5).

- datasheet (high): BCM2711 ARM Peripherals, §8.6 for the base, §4.2.1.3 for the two DREQ numbers

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CTL`](#ctl) | rw | 32 | 1, best high |
| `0x004` | [`STA`](#sta) | rw | 32 | 1, best high |
| `0x008` | [`DMAC`](#dmac) | rw | 32 | 1, best high |
| `0x010` | [`RNG1`](#rng1) | rw | 32 | 1, best high |
| `0x014` | [`DAT1`](#dat1) | rw | 32 | 1, best high |
| `0x018` | [`FIF1`](#fif1) | w | 32 | 1, best high |
| `0x020` | [`RNG2`](#rng2) | rw | 32 | 1, best high |
| `0x024` | [`DAT2`](#dat2) | rw | 32 | 1, best high |

## `CTL`

Offset `0x000` · access `rw` · 32 bits · reset `0x0`

Which channel runs and in what mode. Stored; no channel runs.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `PWEN1` | rw | Channel 1 enabled. |
| 1 | `MODE1` | rw | Channel 1 is a serialiser rather than a PWM. |
| 2 | `RPTL1` | rw | Repeat the last word when channel 1's FIFO runs dry. |
| 3 | `SBIT1` | rw | What channel 1 outputs between words. |
| 4 | `POLA1` | rw | Invert channel 1's output. |
| 5 | `USEF1` | rw | Channel 1 takes its data from the FIFO rather than from `DAT1`. |
| 6 | `CLRF` | w | Write 1 to empty the FIFO. Self-clearing, and the model's FIFO is empty anyway. |
| 7 | `MSEN1` | rw | Channel 1 uses mark/space rather than the evenly-spread PWM algorithm. |
| 8 | `PWEN2` | rw | Channel 2 enabled. Bits 8 to 15 repeat bits 0 to 7 for channel 2, without a second `CLRF`. |
| 9 | `MODE2` | rw | Channel 2 is a serialiser. |
| 10 | `RPTL2` | rw | Repeat the last word on channel 2. |
| 11 | `SBIT2` | rw | Channel 2's between-words level. |
| 12 | `POLA2` | rw | Invert channel 2's output. |
| 13 | `USEF2` | rw | Channel 2 takes its data from the FIFO. |
| 15 | `MSEN2` | rw | Channel 2 uses mark/space. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`PWEN1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`MODE1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`RPTL1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`SBIT1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`POLA1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`USEF1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`CLRF` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`MSEN1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`PWEN2` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`MODE2` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`RPTL2` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`SBIT2` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`POLA2` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`USEF2` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

`MSEN2` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 153 (`CTL`)

## `STA`

Offset `0x004` · access `rw` · 32 bits · reset `0x2`

FIFO and channel state. The error flags are write-1-to-clear; `EMPT1` and `FULL1` are the FIFO's, and here the FIFO is always empty, so a read answers `EMPT1` and whatever error bits are still latched — which is none, since nothing sets them.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `FULL1` | r | The FIFO is full. |
| 1 | `EMPT1` | r | The FIFO is empty. Set out of reset, and always set here. |
| 2 | `WERR1` | w1c | A write to a full FIFO. |
| 3 | `RERR1` | w1c | A read from an empty FIFO. |
| 4 | `GAPO1` | w1c | Channel 1 ran out of data mid-word. |
| 5 | `GAPO2` | w1c | The same for channel 2. |
| 8 | `BERR` | w1c | A bus error: a write the block could not take. |
| 9 | `STA1` | r | Channel 1 is transmitting. |
| 10 | `STA2` | r | Channel 2 is transmitting. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 154 (`STA`)

`FULL1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 154 (`STA`)

`EMPT1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 154 (`STA`)

`WERR1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 154 (`STA`)

`RERR1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 154 (`STA`)

`GAPO1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 154 (`STA`)

`GAPO2` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 154 (`STA`)

`BERR` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 154 (`STA`)

`STA1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 154 (`STA`)

`STA2` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 154 (`STA`)

## `DMAC`

Offset `0x008` · access `rw` · 32 bits · reset `0x707`

DREQ and panic thresholds, and the enable that lets the block pace a DMA channel. Stored; the model raises no DREQ.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 7:0 | `DREQ` | rw | How empty the FIFO has to be before the DREQ goes up. |
| 15:8 | `PANIC` | rw | The same for the panic signal. |
| 31 | `ENAB` | rw | Raise DREQ and panic at all. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 155 (`DMAC`)

`DREQ` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 155 (`DMAC`)

`PANIC` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 155 (`DMAC`)

`ENAB` sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 155 (`DMAC`)

## `RNG1`

Offset `0x010` · access `rw` · 32 bits · reset `0x20`

Channel 1's period, in clock cycles: how many the channel spreads `DAT1` over.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 156 (`RNG1` / `RNG2`)

## `DAT1`

Offset `0x014` · access `rw` · 32 bits · reset `0x0`

Channel 1's data word, used while `CTL.USEF1` is clear.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 157 (`DAT1` / `DAT2`)

## `FIF1`

Offset `0x018` · access `w` · 32 bits

The FIFO both channels share. A write here goes nowhere in the model, and a read answers 0.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 158 (`FIF1`)

## `RNG2`

Offset `0x020` · access `rw` · 32 bits · reset `0x20`

Channel 2's period.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 156 (`RNG1` / `RNG2`)

## `DAT2`

Offset `0x024` · access `rw` · 32 bits · reset `0x0`

Channel 2's data word.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §8.6 Table 157 (`DAT1` / `DAT2`)
