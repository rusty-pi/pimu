<!-- generated from specs/aux.toml by `cargo run -- spec-docs --update` – do not edit -->

# `aux` – AUX: mini-UART (UART1) and the SPI1 / SPI2 masters

- Bus: `vpu` (VPU bus address)
- Base: `0x7E215000`
- Size: `0x100`
- Interrupts: VPU source 93 · GIC id 125 (`GIC_SPI 93`)

The mini-UART is modelled both ways: transmit completes instantly on a line that is always ready, and receive is paced at the rate `MU_BAUD` sets against the modelled clock, as `uart0` is. The SPI masters at `+0x80` / `+0xC0` read 0.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, Auxiliaries chapter

Interrupts (VPU source 93 · GIC id 125 (`GIC_SPI 93`)):

One line for the whole block, ORed from the mini-UART and the two SPI masters; `AUX_IRQ` says which of the three is pending. Linux takes it for `ttyS0`; the firmware polls `MU_LSR` instead. The VPU numbers it the same as the device tree does, as it does for every VC peripheral IRQ.

- linux (high): the dtb start4 hands to Linux: `serial@7e215040`, `brcm,bcm2835-aux-uart`, `interrupts = <0x0 0x5d 0x4>`
- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102: VC peripheral IRQ 29 is `AUX`, so VPU source 93; §6.2.4 Figure 6 has `AUX_IRQ` bits 0 to 2 (mini-UART, SPI1, SPI2) ORed into it

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
| `0x080` | [`SPI1_CNTL0`](#spi1_cntl0) | rw | 32 | 1, best high |
| `0x084` | [`SPI1_CNTL1`](#spi1_cntl1) | rw | 32 | 1, best high |
| `0x088` | [`SPI1_STAT`](#spi1_stat) | r | 32 | 1, best high |
| `0x08C` | [`SPI1_PEEK`](#spi1_peek) | r | 32 | 1, best high |
| `0x0A0`–`0x0AC` (4 × 0x4) | [`SPI1_IO`](#spi1_io) | rw | 32 | 1, best high |
| `0x0B0`–`0x0BC` (4 × 0x4) | [`SPI1_TXHOLD`](#spi1_txhold) | rw | 32 | 1, best high |
| `0x0C0` | [`SPI2_CNTL0`](#spi2_cntl0) | rw | 32 | 1, best high |
| `0x0C4` | [`SPI2_CNTL1`](#spi2_cntl1) | rw | 32 | 1, best high |
| `0x0C8` | [`SPI2_STAT`](#spi2_stat) | r | 32 | 1, best high |
| `0x0CC` | [`SPI2_PEEK`](#spi2_peek) | r | 32 | 1, best high |
| `0x0E0`–`0x0EC` (4 × 0x4) | [`SPI2_IO`](#spi2_io) | rw | 32 | 1, best high |
| `0x0F0`–`0x0FC` (4 × 0x4) | [`SPI2_TXHOLD`](#spi2_txhold) | rw | 32 | 1, best high |

## `IRQ`

Offset `0x000` · access `r` · 32 bits

Which auxiliary has an interrupt pending. Bit 0 follows the mini-UART; the SPI masters are not modelled, so bits 1 and 2 stay 0.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.1.1 (Auxiliaries): `AUX_IRQ`

## `ENABLES`

Offset `0x004` · access `rw` · 32 bits

Per-auxiliary enables.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `UART` | rw | Mini-UART on. |
| 1 | `SPI1` | rw | SPI1 on. |
| 2 | `SPI2` | rw | SPI2 on. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.1.1 (Auxiliaries): `AUX_ENABLES`

`UART` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.1.1 (Auxiliaries): `AUX_ENABLES`

`SPI1` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.1.1 (Auxiliaries): `AUX_ENABLES`

`SPI2` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.1.1 (Auxiliaries): `AUX_ENABLES`

## `MU_IO`

Offset `0x040` · access `rw` · 32 bits

Data: write transmits a byte, read pops the receive FIFO. With `MU_LCR.DLAB` set it is the baud-rate counter's low byte instead.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_IO_REG`

## `MU_IER`

Offset `0x044` · access `rw` · 32 bits

Interrupt enables. With `MU_LCR.DLAB` set it is the baud-rate counter's high byte instead.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `RX_INT` | rw | Interrupt while the receive FIFO holds a byte. The datasheet swaps this bit with bit 1; Linux's `8250` driver drives the register as a 16550's, and that is what the model follows. |
| 1 | `TX_INT` | rw | Interrupt while the transmit holding register is empty. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_IER_REG`

`RX_INT` sources:

- datasheet (high): Linux `drivers/tty/serial/8250/8250_port.c` (`UART_IER_RDI`) against `bcm2835-aux-uart`

`TX_INT` sources:

- datasheet (high): Linux `drivers/tty/serial/8250/8250_port.c` (`UART_IER_THRI`) against `bcm2835-aux-uart`

## `MU_IIR`

Offset `0x048` · access `rw` · 32 bits

Interrupt identity on read, FIFO clear on write (bit 1 clears the receive FIFO, bit 2 the transmit one).

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `PENDING` | rw | Clear while an interrupt is pending — the 16550's inverted sense. |
| 2:1 | `ID` | rw | Which interrupt: `0b10` a byte in the receive FIFO, `0b01` room in the transmit holding register. |
| 7:6 | `FIFO_ENABLES` | rw | Both set: the FIFOs are always enabled on the mini-UART. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_IIR_REG`

`PENDING` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_IIR_REG`

`ID` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_IIR_REG`

`FIFO_ENABLES` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_IIR_REG`

## `MU_LCR`

Offset `0x04C` · access `rw` · 32 bits

Line control.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `DATA_SIZE` | rw | Set for eight data bits, clear for seven. The model does not change how many bits a character takes. |
| 7 | `DLAB` | rw | While set, `MU_IO` and `MU_IER` are the low and high bytes of the baud-rate counter, not the data and interrupt-enable registers. Linux's `8250` driver programs the rate through them. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_LCR_REG`

`DATA_SIZE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_LCR_REG`

`DLAB` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_LCR_REG` (`DLAB access`, and `AUX_MU_IO_REG` / `AUX_MU_IER_REG`, whose descriptions give them the baud bytes)

## `MU_MCR`

Offset `0x050` · access `rw` · 32 bits

Modem control.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_MCR_REG`

## `MU_LSR`

Offset `0x054` · access `r` · 32 bits

Line status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `DATA_READY` | r | A received byte is waiting. |
| 1 | `RX_OVERRUN` | r | A byte arrived with the receive FIFO full. Never set here: input waits on the line instead of being dropped. |
| 5 | `TX_EMPTY` | r | Room for another byte. |
| 6 | `TX_IDLE` | r | Transmitter idle. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_LSR_REG`

`DATA_READY` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_LSR_REG`

`RX_OVERRUN` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_LSR_REG`

`TX_EMPTY` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_LSR_REG`

`TX_IDLE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_LSR_REG`

## `MU_MSR`

Offset `0x058` · access `r` · 32 bits

Modem status.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_MSR_REG`

## `MU_SCRATCH`

Offset `0x05C` · access `rw` · 32 bits

One byte of scratch storage.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_SCRATCH`

## `MU_CNTL`

Offset `0x060` · access `rw` · 32 bits

Extra control: receiver / transmitter enables, flow control.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `RX_ENABLE` | rw | Receiver on. Nothing leaves the line while it is clear. |
| 1 | `TX_ENABLE` | rw | Transmitter on. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_CNTL_REG`

`RX_ENABLE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_CNTL_REG`

`TX_ENABLE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_CNTL_REG`

## `MU_STAT`

Offset `0x064` · access `r` · 32 bits

Extra status: the FIFO fill levels and the ready bits, in the mini-UART's own bit order.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `SYMBOL_AVAILABLE` | r | A received byte is waiting, as `MU_LSR.DATA_READY` says. |
| 1 | `SPACE_AVAILABLE` | r | Room in the transmit FIFO. Always set here. |
| 2 | `RX_IDLE` | r | The receiver is not in the middle of a character. |
| 3 | `TX_IDLE` | r | The transmitter is not in the middle of a character. Always set here. |
| 4 | `RX_OVERRUN` | r | As `MU_LSR.RX_OVERRUN`. Never set here. |
| 5 | `TX_FIFO_FULL` | r | No room for another byte. Never set here. |
| 8 | `TX_FIFO_EMPTY` | r | Nothing waiting to go out. Always set here. |
| 9 | `TX_DONE` | r | `TX_FIFO_EMPTY` and `TX_IDLE` together. Always set here. |
| 19:16 | `RX_FIFO_LEVEL` | r | How many bytes the receive FIFO holds. |
| 27:24 | `TX_FIFO_LEVEL` | r | How many bytes the transmit FIFO holds. Always 0 here. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_STAT_REG`

`SYMBOL_AVAILABLE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_STAT_REG`

`SPACE_AVAILABLE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_STAT_REG`

`RX_IDLE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_STAT_REG`

`TX_IDLE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_STAT_REG`

`RX_OVERRUN` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_STAT_REG`

`TX_FIFO_FULL` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_STAT_REG`

`TX_FIFO_EMPTY` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_STAT_REG`

`TX_DONE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_STAT_REG`

`RX_FIFO_LEVEL` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_STAT_REG`

`TX_FIFO_LEVEL` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_STAT_REG`

## `MU_BAUD`

Offset `0x068` · access `rw` · 32 bits

Baud-rate counter (16 bits): the line runs at `system_clock / (8 * (MU_BAUD + 1))`, and the model paces receive by it.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.2.2 (Auxiliaries): `AUX_MU_BAUD_REG`

## `SPI1_CNTL0`

Offset `0x080` · access `rw` · 32 bits

SPI1 control word 0: clock, chip selects and shift set-up. Nothing on this SoC drives the pins the master would use in a boot — GPIO 16..21 carry it on ALT4 — so the model keeps the word and runs no transfer.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 5:0 | `SHIFT_LENGTH` | rw | How many bits a transfer shifts. |
| 6 | `SHIFT_OUT_MS_BIT` | rw | Shift out most-significant bit first. |
| 7 | `INVERT_CLK` | rw | The clock line idles high. |
| 8 | `OUT_RISING` | rw | Clock data out on the rising edge. |
| 9 | `CLEAR_FIFOS` | rw | Hold both FIFOs in reset. |
| 10 | `IN_RISING` | rw | Clock data in on the rising edge. |
| 11 | `ENABLE` | rw | Enable the interface. The FIFOs can still be written while it is off. |
| 13:12 | `DOUT_HOLD` | rw | Extra hold time on the data-out line, in system clock cycles. |
| 14 | `VARIABLE_WIDTH` | rw | Take the shift length from the data word rather than from `SHIFT_LENGTH`. |
| 15 | `VARIABLE_CS` | rw | Take the chip-select pattern from the data word. |
| 16 | `POST_INPUT` | rw | Input is sampled one clock later. |
| 19:17 | `CHIP_SELECTS` | rw | What the three CS pins carry while the transfer is on. |
| 31:20 | `SPEED` | rw | Clock divisor: the bus runs at `system_clock / (2 * (SPEED + 1))`. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`SHIFT_LENGTH` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`SHIFT_OUT_MS_BIT` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`INVERT_CLK` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`OUT_RISING` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`CLEAR_FIFOS` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`IN_RISING` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`ENABLE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`DOUT_HOLD` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`VARIABLE_WIDTH` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`VARIABLE_CS` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`POST_INPUT` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`CHIP_SELECTS` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

`SPEED` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL0_REG`

## `SPI1_CNTL1`

Offset `0x084` · access `rw` · 32 bits

SPI1 control word 1: the interrupt conditions and the shift direction.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `KEEP_INPUT` | rw | Do not clear the receive shift register between transfers. |
| 1 | `SHIFT_IN_MS_BIT` | rw | Shift in most-significant bit first. |
| 6 | `DONE_IRQ` | rw | Raise the interrupt while the interface is idle. |
| 7 | `TX_EMPTY_IRQ` | rw | Raise the interrupt while the transmit FIFO is empty. |
| 10:8 | `CS_HIGH_TIME` | rw | Extra clock cycles to hold CS high between transfers. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL1_REG`

`KEEP_INPUT` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL1_REG`

`SHIFT_IN_MS_BIT` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL1_REG`

`DONE_IRQ` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL1_REG`

`TX_EMPTY_IRQ` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL1_REG`

`CS_HIGH_TIME` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_CNTL1_REG`

## `SPI1_STAT`

Offset `0x088` · access `r` · 32 bits

SPI1 FIFO levels and the busy flag. The model reads 0: both FIFOs stay empty and the interface is never busy.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 5:0 | `BIT_COUNT` | r | Bits still to go in the transfer. |
| 6 | `BUSY` | r | A transfer is running. |
| 7 | `RX_EMPTY` | r | The receive FIFO is empty. |
| 8 | `RX_FULL` | r | The receive FIFO is full. |
| 9 | `TX_EMPTY` | r | The transmit FIFO is empty. |
| 10 | `TX_FULL` | r | The transmit FIFO is full. |
| 19:16 | `RX_FIFO_LEVEL` | r | How many words the receive FIFO holds. |
| 27:24 | `TX_FIFO_LEVEL` | r | How many words the transmit FIFO holds. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_STAT_REG`

`BIT_COUNT` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_STAT_REG`

`BUSY` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_STAT_REG`

`RX_EMPTY` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_STAT_REG`

`RX_FULL` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_STAT_REG`

`TX_EMPTY` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_STAT_REG`

`TX_FULL` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_STAT_REG`

`RX_FIFO_LEVEL` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_STAT_REG`

`TX_FIFO_LEVEL` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_STAT_REG`

## `SPI1_PEEK`

Offset `0x08C` · access `r` · 32 bits

The top of SPI1’s receive FIFO, without taking it off.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_PEEK_REG`

## `SPI1_IO`

Offset `0x0A0`, 4 elements 0x4 apart · access `rw` · 32 bits

SPI1 data: a write starts a transfer of `CNTL0.SHIFT_LENGTH` bits and a read takes the receive FIFO’s top word. Four addresses for one register, so a driver can pick the shift length from the address it writes.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_IO_REGa_REG`

## `SPI1_TXHOLD`

Offset `0x0B0`, 4 elements 0x4 apart · access `rw` · 32 bits

Like `SPI1_IO`, but CS stays asserted after the transfer, which is how a driver sends more than one word in a frame.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI1_TXHOLD_REGa_REG`

## `SPI2_CNTL0`

Offset `0x0C0` · access `rw` · 32 bits

SPI2 control word 0: clock, chip selects and shift set-up. Nothing on this SoC drives the pins the master would use in a boot — GPIO 16..21 carry it on ALT4 — so the model keeps the word and runs no transfer.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 5:0 | `SHIFT_LENGTH` | rw | How many bits a transfer shifts. |
| 6 | `SHIFT_OUT_MS_BIT` | rw | Shift out most-significant bit first. |
| 7 | `INVERT_CLK` | rw | The clock line idles high. |
| 8 | `OUT_RISING` | rw | Clock data out on the rising edge. |
| 9 | `CLEAR_FIFOS` | rw | Hold both FIFOs in reset. |
| 10 | `IN_RISING` | rw | Clock data in on the rising edge. |
| 11 | `ENABLE` | rw | Enable the interface. The FIFOs can still be written while it is off. |
| 13:12 | `DOUT_HOLD` | rw | Extra hold time on the data-out line, in system clock cycles. |
| 14 | `VARIABLE_WIDTH` | rw | Take the shift length from the data word rather than from `SHIFT_LENGTH`. |
| 15 | `VARIABLE_CS` | rw | Take the chip-select pattern from the data word. |
| 16 | `POST_INPUT` | rw | Input is sampled one clock later. |
| 19:17 | `CHIP_SELECTS` | rw | What the three CS pins carry while the transfer is on. |
| 31:20 | `SPEED` | rw | Clock divisor: the bus runs at `system_clock / (2 * (SPEED + 1))`. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`SHIFT_LENGTH` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`SHIFT_OUT_MS_BIT` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`INVERT_CLK` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`OUT_RISING` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`CLEAR_FIFOS` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`IN_RISING` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`ENABLE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`DOUT_HOLD` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`VARIABLE_WIDTH` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`VARIABLE_CS` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`POST_INPUT` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`CHIP_SELECTS` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

`SPEED` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL0_REG`

## `SPI2_CNTL1`

Offset `0x0C4` · access `rw` · 32 bits

SPI2 control word 1: the interrupt conditions and the shift direction.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `KEEP_INPUT` | rw | Do not clear the receive shift register between transfers. |
| 1 | `SHIFT_IN_MS_BIT` | rw | Shift in most-significant bit first. |
| 6 | `DONE_IRQ` | rw | Raise the interrupt while the interface is idle. |
| 7 | `TX_EMPTY_IRQ` | rw | Raise the interrupt while the transmit FIFO is empty. |
| 10:8 | `CS_HIGH_TIME` | rw | Extra clock cycles to hold CS high between transfers. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL1_REG`

`KEEP_INPUT` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL1_REG`

`SHIFT_IN_MS_BIT` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL1_REG`

`DONE_IRQ` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL1_REG`

`TX_EMPTY_IRQ` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL1_REG`

`CS_HIGH_TIME` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_CNTL1_REG`

## `SPI2_STAT`

Offset `0x0C8` · access `r` · 32 bits

SPI2 FIFO levels and the busy flag. The model reads 0: both FIFOs stay empty and the interface is never busy.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 5:0 | `BIT_COUNT` | r | Bits still to go in the transfer. |
| 6 | `BUSY` | r | A transfer is running. |
| 7 | `RX_EMPTY` | r | The receive FIFO is empty. |
| 8 | `RX_FULL` | r | The receive FIFO is full. |
| 9 | `TX_EMPTY` | r | The transmit FIFO is empty. |
| 10 | `TX_FULL` | r | The transmit FIFO is full. |
| 19:16 | `RX_FIFO_LEVEL` | r | How many words the receive FIFO holds. |
| 27:24 | `TX_FIFO_LEVEL` | r | How many words the transmit FIFO holds. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_STAT_REG`

`BIT_COUNT` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_STAT_REG`

`BUSY` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_STAT_REG`

`RX_EMPTY` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_STAT_REG`

`RX_FULL` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_STAT_REG`

`TX_EMPTY` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_STAT_REG`

`TX_FULL` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_STAT_REG`

`RX_FIFO_LEVEL` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_STAT_REG`

`TX_FIFO_LEVEL` sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_STAT_REG`

## `SPI2_PEEK`

Offset `0x0CC` · access `r` · 32 bits

The top of SPI2’s receive FIFO, without taking it off.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_PEEK_REG`

## `SPI2_IO`

Offset `0x0E0`, 4 elements 0x4 apart · access `rw` · 32 bits

SPI2 data: a write starts a transfer of `CNTL0.SHIFT_LENGTH` bits and a read takes the receive FIFO’s top word. Four addresses for one register, so a driver can pick the shift length from the address it writes.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_IO_REGa_REG`

## `SPI2_TXHOLD`

Offset `0x0F0`, 4 elements 0x4 apart · access `rw` · 32 bits

Like `SPI2_IO`, but CS stays asserted after the transfer, which is how a driver sends more than one word in a frame.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §2.3.4 (Auxiliaries): `AUX_SPI2_TXHOLD_REGa_REG`
