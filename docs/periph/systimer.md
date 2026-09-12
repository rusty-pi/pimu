<!-- generated from specs/systimer.toml by `cargo run -- spec-docs --update` – do not edit -->

# `systimer` – System timer: 64-bit free-running 1 MHz counter with four compare channels

- Bus: `vpu` (VPU bus address)
- Base: `0x7E003000`
- Size: `0x1000`

Compare channel n raises VPU interrupt source 64 + n when it matches.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, System Timer chapter
- linux (high): bcm283x.dtsi: brcm,bcm2835-system-timer, reg = <0x7e003000 0x1000>
- trace (high): start4 arms C0 as its ThreadX tick (source 64) and C2 as the clock service's timeout (source 66)

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CS`](#cs) | w1c | 32 | 2, best high |
| `0x004` | [`CLO`](#clo) | r | 32 | 2, best high |
| `0x008` | [`CHI`](#chi) | r | 32 | 1, best high |
| `0x00C`–`0x018` (4 × 0x4) | [`C`](#c) | rw | 32 | 3, best high |

## `CS`

Offset `0x000` · access `w1c` · 32 bits

Match flags, one per compare channel. Write 1 to clear.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `M0` | w1c | Channel 0 matched. |
| 1 | `M1` | w1c | Channel 1 matched. |
| 2 | `M2` | w1c | Channel 2 matched. |
| 3 | `M3` | w1c | Channel 3 matched. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, System Timer: CS
- linux (high): drivers/clocksource/bcm2835_timer.c acks its channel by writing its bit to +0x00

`M0` sources:

- datasheet (high): BCM2711 ARM Peripherals, System Timer: CS

`M1` sources:

- datasheet (high): BCM2711 ARM Peripherals, System Timer: CS

`M2` sources:

- datasheet (high): BCM2711 ARM Peripherals, System Timer: CS

`M3` sources:

- datasheet (high): BCM2711 ARM Peripherals, System Timer: CS

## `CLO`

Offset `0x004` · access `r` · 32 bits

Counter, low 32 bits.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, System Timer: CLO
- linux (high): drivers/clocksource/bcm2835_timer.c reads the counter at +0x04

## `CHI`

Offset `0x008` · access `r` · 32 bits

Counter, high 32 bits.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, System Timer: CHI

## `C`

Offset `0x00C`, 4 elements 0x4 apart · access `rw` · 32 bits

Compare values. Channel n matches once when CLO reaches C[n]; it does not reload.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, System Timer: C0..C3
- linux (high): drivers/clocksource/bcm2835_timer.c programs channel n at +0x0C + 4 * n
- trace (high): start4 re-arms C0 from its tick ISR 0x3EC40B7C on every tick
