<!-- generated from specs/spi0.toml by `cargo run -- spec-docs --update` – do not edit -->

# `spi0` – SPI0 master, with the serial-NOR flash the EEPROM bootloader was loaded from

- Bus: `vpu` (VPU bus address)
- Base: `0x7E204000`
- Size: `0x18`

Driven in polled single-byte mode. Every shift is instantaneous; the flash answers READ, FAST_READ, RDID, RDSR, WREN / WRDI, SE and PP.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI chapter
- decompile (high): EEPROM bootloader: wait TXD, write FIFO, wait RXD, read FIFO per byte; start4's EEPROM scanner 0x3ED77E00

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CS`](#cs) | rw | 32 | 1, best high |
| `0x004` | [`FIFO`](#fifo) | rw | 32 | 1, best high |
| `0x008` | [`CLK`](#clk) | rw | 32 | 1, best high |
| `0x00C` | [`DLEN`](#dlen) | rw | 32 | 1, best high |
| `0x010` | [`LTOH`](#ltoh) | rw | 32 | 1, best high |
| `0x014` | [`DC`](#dc) | rw | 32 | 1, best high |

## `CS`

Offset `0x000` · access `rw` · 32 bits

Control and status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 1:0 | `CS` | rw | Chip select. |
| 4 | `CLEAR_TX` | w | Clear the TX FIFO. |
| 5 | `CLEAR_RX` | w | Clear the RX FIFO. |
| 7 | `TA` | rw | Transfer active; the whole command runs with it set. |
| 16 | `DONE` | r | Nothing left to shift. TX side only: start4's scanner checks it with RX bytes still queued. |
| 17 | `RXD` | r | RX FIFO holds data. |
| 18 | `TXD` | r | TX FIFO has room. |
| 19 | `RXR` | r | RX FIFO needs reading. |
| 20 | `RXF` | r | RX FIFO full. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: CS

`CS` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: CS

`CLEAR_TX` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: CS.CLEAR

`CLEAR_RX` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: CS.CLEAR

`TA` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: CS.TA

`DONE` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: CS.DONE
- decompile (high): 0x3ED77E00 treats DONE = 0 as a transfer error

`RXD` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: CS.RXD

`TXD` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: CS.TXD

`RXR` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: CS.RXR

`RXF` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: CS.RXF

## `FIFO`

Offset `0x004` · access `rw` · 32 bits

Write shifts a byte out, read takes the byte shifted in.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: FIFO

## `CLK`

Offset `0x008` · access `rw` · 32 bits

Clock divider.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: CLK

## `DLEN`

Offset `0x00C` · access `rw` · 32 bits

DMA data length.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: DLEN

## `LTOH`

Offset `0x010` · access `rw` · 32 bits

LoSSI output hold delay.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: LTOH

## `DC`

Offset `0x014` · access `rw` · 32 bits

DMA DREQ controls.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: DC
