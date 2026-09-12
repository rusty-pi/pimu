<!-- generated from specs/cm.toml by `cargo run -- spec-docs --update` – do not edit -->

# `cm` – Clock manager, with the A2W PLL control in the same window

- Bus: `vpu` (VPU bus address)
- Base: `0x7E101000`
- Size: `0x2000`

The analogue PLLs are not modelled: every PLL reads locked, every *_CTL register reads BUSY clear, and everything else is stored with the password byte masked on read-back.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, General Purpose GPIO Clocks: the CM_*CTL / CM_*DIV layout and the 0x5A password
- decompile (high): EEPROM bootloader programs a PLL and polls for lock before trusting the SPI clock

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x0F0` | [`UARTCTL`](#uartctl) | rw | 32 | 2, best high |
| `0x0F4` | [`UARTDIV`](#uartdiv) | rw | 32 | 1, best high |
| `0x100` | [`DELAY`](#delay) | rw | 32 | 1, best high |
| `0x114` | [`LOCK`](#lock) | r | 32 | 1, best medium |

## `UARTCTL`

Offset `0x0F0` · access `rw` · 32 bits

UART clock control. Linux later reprograms it to SRC 6 / MASH 1; start4 itself writes SRC 1.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3:0 | `SRC` | rw | Clock source: 1 = oscillator, 6 = PLLD. |
| 4 | `ENAB` | rw | Generator on. |
| 7 | `BUSY` | r | Generator running. Real silicon holds it at 1 while running; the model always reads 0, because the shutdown path 0x3EC7F0BA spins on it waiting for clocks the model stops instantly. Same position in every *_CTL register. |
| 10:9 | `MASH` | rw | MASH noise-shaping stages. |
| 31:24 | `PASSWD` | rw | 0x5A on write; reads back masked. |

Sources:

- measured (high): /dev/mem read of 0xFE1010F0 on rpi-dev, Linux idle: 0x00000296
- decompile (high): console writer 0x3ED85E9C and clock-change callback 0x3EC799BC gate on ENAB

`SRC` sources:

- datasheet (high): BCM2711 ARM Peripherals, CM_GPxCTL: SRC

`ENAB` sources:

- datasheet (high): BCM2711 ARM Peripherals, CM_GPxCTL: ENAB

`BUSY` sources:

- datasheet (high): BCM2711 ARM Peripherals, CM_GPxCTL: BUSY
- decompile (high): shutdown path 0x3EC7F0BA polls BUSY with a 1000-iteration escape

`MASH` sources:

- datasheet (high): BCM2711 ARM Peripherals, CM_GPxCTL: MASH

`PASSWD` sources:

- datasheet (high): BCM2711 ARM Peripherals, CM_GPxCTL: PASSWD

## `UARTDIV`

Offset `0x0F4` · access `rw` · 32 bits

UART clock divisor.

Sources:

- measured (high): /dev/mem read of 0xFE1010F4 on rpi-dev, Linux idle: 0x0000fa00

## `DELAY`

Offset `0x100` · access `rw` · 32 bits

Self-clearing 'wait N clocks' register: written password | cycle count, polled until 0. Always reads 0.

Sources:

- decompile (high): the bootloader's SDHCI register-write helper 0x00081dc0 writes it and spins at 0x00081de2

## `LOCK`

Offset `0x114` · access `r` · 32 bits

One bit per PLL, set once it has locked. Always all ones.

Sources:

- decompile (medium): the bootloader polls it after programming a PLL
