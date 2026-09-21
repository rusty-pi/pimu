<!-- generated from specs/uart0.toml by `cargo run -- spec-docs --update` – do not edit -->

# `uart0` – PL011 UART0: the firmware's debug console and Linux's `ttyAMA0`

- Bus: `vpu` (VPU bus address)
- Base: `0x7E201000`
- Size: `0x1000`
- Interrupts: VPU source 121 · GIC id 153 (`GIC_SPI 121`)

Transmit completes instantly in the model; receive is paced at the programmed baud rate against the modelled clock.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, UART chapter (ARM PL011)
- linux (high): dtb start4 hands to Linux: `serial@7e201000`, `arm,pl011`, GIC SPI 121
- inferred (medium): size: one 4 KiB page, the granularity of this window

Interrupts (VPU source 121 · GIC id 153 (`GIC_SPI 121`)):

Linux's `ttyAMA0`. The firmware polls the FIFO instead of taking the line. The line is shared: it is the OR of all five PL011 UARTs, and `PACTL_CS` bits 16 to 20 say which of them is pending.

- linux (high): the dtb start4 hands to Linux: `serial@7e201000`, `arm,pl011`, GIC SPI 121
- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102: VC peripheral IRQ 57 is the OR of all PL011 UARTs, VPU source 121; Figure 6 puts UART0 on `PACTL_CS` bit 20

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`DR`](#dr) | rw | 32 | 2, best high |
| `0x018` | [`FR`](#fr) | r | 32 | 2, best high |
| `0x024` | [`IBRD`](#ibrd) | rw | 32 | 1, best high |
| `0x028` | [`FBRD`](#fbrd) | rw | 32 | 1, best high |
| `0x02C` | [`LCRH`](#lcrh) | rw | 32 | 1, best high |
| `0x030` | [`CR`](#cr) | rw | 32 | 1, best high |
| `0x034` | [`IFLS`](#ifls) | rw | 32 | 1, best high |
| `0x038` | [`IMSC`](#imsc) | rw | 32 | 2, best high |
| `0x03C` | [`RIS`](#ris) | r | 32 | 1, best high |
| `0x040` | [`MIS`](#mis) | r | 32 | 1, best high |
| `0x044` | [`ICR`](#icr) | w | 32 | 1, best high |

## `DR`

Offset `0x000` · access `rw` · 32 bits

Write sends a byte; read pops the receive FIFO.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `DR`
- decompile (high): start4 console writer `0x3ED85E9C` stores each byte here

## `FR`

Offset `0x018` · access `r` · 32 bits

FIFO and line flags.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3 | `BUSY` | r | Transmitting. The model never is. |
| 4 | `RXFE` | r | Receive FIFO empty. |
| 5 | `TXFF` | r | Transmit FIFO full. The model never is. |
| 6 | `RXFF` | r | Receive FIFO full. |
| 7 | `TXFE` | r | Transmit FIFO empty. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `FR`
- decompile (high): `0x3ED85E9C` polls `TXFF` before each byte; the clock-change callback `0x3EC799BC` drains `BUSY`

`BUSY` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `FR`

`RXFE` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `FR`

`TXFF` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `FR`

`RXFF` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `FR`

`TXFE` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `FR`

## `IBRD`

Offset `0x024` · access `rw` · 32 bits

Integer part of the baud divisor (16 bits).

Sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `IBRD`

## `FBRD`

Offset `0x028` · access `rw` · 32 bits

Fractional part of the baud divisor, in 64ths (6 bits).

Sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `FBRD`

## `LCRH`

Offset `0x02C` · access `rw` · 32 bits

Line control. Toggling `FEN` flushes the receive FIFO.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 4 | `FEN` | rw | FIFOs on; off makes the receiver a one-character holding register. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `LCRH`

`FEN` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `LCRH`

## `CR`

Offset `0x030` · access `rw` · 32 bits

Control.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `UARTEN` | rw | UART on. |
| 8 | `TXE` | rw | Transmitter on. |
| 9 | `RXE` | rw | Receiver on. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `CR`

`UARTEN` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `CR`

`TXE` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `CR`

`RXE` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `CR`

## `IFLS`

Offset `0x034` · access `rw` · 32 bits · reset `0x12`

FIFO interrupt trigger levels; reset is half full both ways.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 2:0 | `TXIFLSEL` | rw | Transmit trigger level. |
| 5:3 | `RXIFLSEL` | rw | Receive trigger level: 1/8, 1/4, 1/2, 3/4, 7/8 full. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `IFLS`

`TXIFLSEL` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `IFLS`

`RXIFLSEL` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `IFLS`

## `IMSC`

Offset `0x038` · access `rw` · 32 bits

Interrupt mask, same bit layout as `RIS`.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `IMSC`
- linux (high): `amba-pl011` enables `RX` and `RT` here

## `RIS`

Offset `0x03C` · access `r` · 32 bits

Raw interrupt status. `IMSC`, `MIS` and `ICR` use the same bit positions.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 4 | `RX` | r | Receive FIFO at or past its trigger level. |
| 6 | `RT` | r | Receive timeout: characters waiting and none new for 32 bit periods. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `RIS`

`RX` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `RIS`

`RT` sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `RIS`

## `MIS`

Offset `0x040` · access `r` · 32 bits

`RIS & IMSC`.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `MIS`

## `ICR`

Offset `0x044` · access `w` · 32 bits

Write 1 to clear a raw interrupt.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, UART: `ICR`
