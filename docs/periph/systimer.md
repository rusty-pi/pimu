<!-- generated from specs/systimer.toml by `cargo run -- spec-docs --update` – do not edit -->

# `systimer` – System timer: 64-bit free-running 1 MHz counter with four compare channels

- Bus: `vpu` (VPU bus address)
- Base: `0x7E003000`
- Size: `0x1000`
- Interrupts: `C0` VPU source 64 · `C1` VPU source 65 · `C2` VPU source 66 · `C3` VPU source 67

Compare channel n raises VPU interrupt source 64 + n when it matches.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, System Timer chapter
- linux (high): `bcm283x.dtsi`: `brcm,bcm2835-system-timer`, `reg = <0x7e003000 0x1000>`
- trace (high): start4 arms `C0` as its ThreadX tick (source 64) and `C2` as the clock service's timeout (source 66)

Interrupts (`C0` VPU source 64 · `C1` VPU source 65 · `C2` VPU source 66 · `C3` VPU source 67):

Compare channel `n` raises `64 + n`. start4 arms `C0` as its ThreadX tick and `C2` as the clock service's timeout.

- decompile (high): start4 `enable_irq_source(64, 1)` before it arms `C0`; the clock service waits on `C2` with source 66
- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102: VC peripheral IRQs 0 to 3 are `Timer 0` to `Timer 3`, which the VPU takes as sources 64 to 67

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

- datasheet (high): BCM2711 ARM Peripherals, §10.2 (System Timer): `CS`
- linux (high): `drivers/clocksource/bcm2835_timer.c` acks its channel by writing its bit to `+0x00`

`M0` sources:

- datasheet (high): BCM2711 ARM Peripherals, §10.2 (System Timer): `CS`

`M1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §10.2 (System Timer): `CS`

`M2` sources:

- datasheet (high): BCM2711 ARM Peripherals, §10.2 (System Timer): `CS`

`M3` sources:

- datasheet (high): BCM2711 ARM Peripherals, §10.2 (System Timer): `CS`

## `CLO`

Offset `0x004` · access `r` · 32 bits

Counter, low 32 bits.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §10.2 (System Timer): `CLO`
- linux (high): `drivers/clocksource/bcm2835_timer.c` reads the counter at `+0x04`

## `CHI`

Offset `0x008` · access `r` · 32 bits

Counter, high 32 bits.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §10.2 (System Timer): `CHI`

## `C`

Offset `0x00C`, 4 elements 0x4 apart · access `rw` · 32 bits

Compare values. Channel n matches once when `CLO` reaches `C[n]`; it does not reload.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §10.2 (System Timer): `C0..C3`
- linux (high): `drivers/clocksource/bcm2835_timer.c` programs channel n at `+0x0C + 4 * n`
- trace (high): start4 re-arms `C0` from its tick ISR `0x3EC40B7C` on every tick
