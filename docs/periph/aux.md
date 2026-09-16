<!-- generated from specs/aux.toml by `cargo run -- spec-docs --update` – do not edit -->

# `aux` – AUX: mini-UART (UART1) and the SPI1 / SPI2 masters

- Bus: `vpu` (VPU bus address)
- Base: `0x7E215000`
- Size: `0x100`

Only the mini-UART is modelled, transmit-only with a line that is always ready. The SPI masters at `+0x80` / `+0xC0` read 0.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries chapter

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`IRQ`](#irq) | r | 32 | 1, best high |
| `0x004` | [`ENABLES`](#enables) | rw | 32 | 1, best high |
| `0x040` | [`MU_IO`](#mu_io) | rw | 32 | 1, best high |
| `0x044` | [`MU_IER`](#mu_ier) | rw | 32 | 1, best high |
| `0x048` | [`MU_IIR`](#mu_iir) | rw | 32 | 1, best high |
| `0x04C` | [`MU_LCR`](#mu_lcr) | rw | 32 | 1, best high |
| `0x050` | [`MU_MCR`](#mu_mcr) | rw | 32 | 1, best high |
| `0x054` | [`MU_LSR`](#mu_lsr) | r | 32 | 1, best high |
| `0x058` | [`MU_MSR`](#mu_msr) | r | 32 | 1, best high |
| `0x05C` | [`MU_SCRATCH`](#mu_scratch) | rw | 32 | 1, best high |
| `0x060` | [`MU_CNTL`](#mu_cntl) | rw | 32 | 1, best high |
| `0x064` | [`MU_STAT`](#mu_stat) | r | 32 | 1, best high |
| `0x068` | [`MU_BAUD`](#mu_baud) | rw | 32 | 1, best high |

## `IRQ`

Offset `0x000` · access `r` · 32 bits

Which auxiliary has an interrupt pending. Always 0 here.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_IRQ`

## `ENABLES`

Offset `0x004` · access `rw` · 32 bits

Per-auxiliary enables.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `UART` | rw | Mini-UART on. |
| 1 | `SPI1` | rw | SPI1 on. |
| 2 | `SPI2` | rw | SPI2 on. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_ENABLES`

`UART` sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_ENABLES`

`SPI1` sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_ENABLES`

`SPI2` sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_ENABLES`

## `MU_IO`

Offset `0x040` · access `rw` · 32 bits

Data: write transmits a byte; nothing is ever received.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_IO_REG`

## `MU_IER`

Offset `0x044` · access `rw` · 32 bits

Interrupt enables.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_IER_REG`

## `MU_IIR`

Offset `0x048` · access `rw` · 32 bits

Interrupt identity on read (FIFOs on, nothing pending), FIFO clear on write.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_IIR_REG`

## `MU_LCR`

Offset `0x04C` · access `rw` · 32 bits

Line control.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_LCR_REG`

## `MU_MCR`

Offset `0x050` · access `rw` · 32 bits

Modem control.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_MCR_REG`

## `MU_LSR`

Offset `0x054` · access `r` · 32 bits

Line status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `DATA_READY` | r | A received byte is waiting. Never set here. |
| 5 | `TX_EMPTY` | r | Room for another byte. |
| 6 | `TX_IDLE` | r | Transmitter idle. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_LSR_REG`

`DATA_READY` sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_LSR_REG`

`TX_EMPTY` sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_LSR_REG`

`TX_IDLE` sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_LSR_REG`

## `MU_MSR`

Offset `0x058` · access `r` · 32 bits

Modem status.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_MSR_REG`

## `MU_SCRATCH`

Offset `0x05C` · access `rw` · 32 bits

One byte of scratch storage.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_SCRATCH`

## `MU_CNTL`

Offset `0x060` · access `rw` · 32 bits

Extra control: receiver / transmitter enables, flow control.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_CNTL_REG`

## `MU_STAT`

Offset `0x064` · access `r` · 32 bits

Extra status. The model reports the same ready bits as `MU_LSR`.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_STAT_REG`

## `MU_BAUD`

Offset `0x068` · access `rw` · 32 bits

Baud-rate counter (16 bits).

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries: `AUX_MU_BAUD_REG`
