<!-- generated from specs/pcm.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pcm` – PCM / I²S audio: the one serial-audio interface on the chip

- Bus: `vpu` (VPU bus address)
- Base: `0x7E203000`
- Size: `0x24`
- Interrupts: VPU source 119

One module, on GPIO 18 to 21 (`PCM_CLK`, `PCM_FS`, `PCM_DIN`, `PCM_DOUT` on ALT0) or GPIO 28 to 31 on ALT2. Nothing on a 4B is wired to it unless a HAT brings its own codec, and no firmware in a boot touches the block. The model is the register map with both FIFOs empty: a transmit write is dropped, a receive read answers 0, and neither the interrupt nor the DREQs ever go up.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9: "There is only one PCM module in the BCM2711. The PCM base address for the registers is `0x7e203000`"
- linux (high): `bcm2711.dtsi`: `pcm@7e203000`, `brcm,bcm2835-i2s`, with DMA channels named `tx` and `rx`

Interrupts (VPU source 119):

- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102: VC peripheral IRQ 55 is `PCM/I2S`, VPU source 119
- trace (medium): `linux-boot`, `PIMU_TRACE_MMIO=0x7e002000-0x7e002060`: the secure service enables source 119 among the eight `IRQ_PRIO` words it writes — _Which is the firmware enabling the source, not this block raising it: nothing in a boot programs the PCM._

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CS_A`](#cs_a) | rw | 32 | 1, best high |
| `0x004` | [`FIFO_A`](#fifo_a) | rw | 32 | 1, best high |
| `0x008` | [`MODE_A`](#mode_a) | rw | 32 | 1, best high |
| `0x00C` | [`RXC_A`](#rxc_a) | rw | 32 | 1, best high |
| `0x010` | [`TXC_A`](#txc_a) | rw | 32 | 1, best high |
| `0x014` | [`DREQ_A`](#dreq_a) | rw | 32 | 1, best high |
| `0x018` | [`INTEN_A`](#inten_a) | rw | 32 | 1, best high |
| `0x01C` | [`INTSTC_A`](#intstc_a) | w1c | 32 | 1, best high |
| `0x020` | [`GRAY`](#gray) | rw | 32 | 1, best high |

## `CS_A`

Offset `0x000` · access `rw` · 32 bits · reset `0x280000`

Control and status. The bottom three bits can be written while the interface runs; the rest cannot. A read here answers the stored control bits with the FIFO flags of an idle, empty interface on top of them — `TXE`, `TXD` and `TXW` set, `RXF`, `RXD` and `RXR` clear — which is what the `reset` records.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `EN` | rw | Enable the interface. |
| 1 | `RXON` | rw | Receive. |
| 2 | `TXON` | rw | Transmit. |
| 3 | `TXCLR` | w | Write 1 to empty the transmit FIFO. Self-clearing. |
| 4 | `RXCLR` | w | The same for the receive FIFO. |
| 6:5 | `TXTHR` | rw | How empty the transmit FIFO has to be for `TXW`. |
| 8:7 | `RXTHR` | rw | How full the receive FIFO has to be for `RXR`. |
| 9 | `DMAEN` | rw | Raise the DREQs `TI.PERMAP` 2 and 3 select. |
| 13 | `TXSYNC` | r | The transmit FIFO is in sync with the frame. |
| 14 | `RXSYNC` | r | The same for the receive FIFO. |
| 15 | `TXERR` | w1c | The transmit FIFO underran. |
| 16 | `RXERR` | w1c | The receive FIFO overran. |
| 17 | `TXW` | r | The transmit FIFO wants writing. Always set here. |
| 18 | `RXR` | r | The receive FIFO wants reading. |
| 19 | `TXD` | r | The transmit FIFO can take a word. Set out of reset, and always set here. |
| 20 | `RXD` | r | The receive FIFO holds a word. Never set here. |
| 21 | `TXE` | r | The transmit FIFO is empty. Set out of reset, and always set here. |
| 22 | `RXF` | r | The receive FIFO is full. |
| 23 | `RXSEX` | rw | Sign-extend what comes out of the receive FIFO. |
| 24 | `SYNC` | rw | Write it, read it back two PCM clocks later: how a driver measures that the interface's clock is running. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`EN` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`RXON` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`TXON` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`TXCLR` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`RXCLR` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`TXTHR` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`RXTHR` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`DMAEN` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`) for the bit, §4.2.1.3 for `PCM TX` and `PCM RX`

`TXSYNC` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`RXSYNC` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`TXERR` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`RXERR` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`TXW` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`RXR` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`TXD` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`RXD` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`TXE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`RXF` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`RXSEX` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

`SYNC` sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 143 (`CS_A`)

## `FIFO_A`

Offset `0x004` · access `rw` · 32 bits

Both FIFOs: a write goes to the transmit one, a read takes from the receive one. Dropped and 0 respectively here.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 144 (`FIFO_A`)

## `MODE_A`

Offset `0x008` · access `rw` · 32 bits · reset `0x0`

Frame length and format, and whether the block is the clock master. Stored.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 145 (`MODE_A`)

## `RXC_A`

Offset `0x00C` · access `rw` · 32 bits · reset `0x0`

Where the two receive channels sit in the frame, and how wide they are. Stored.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 146 (`RXC_A`)

## `TXC_A`

Offset `0x010` · access `rw` · 32 bits · reset `0x0`

The same for the two transmit channels. Stored.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 147 (`TXC_A`)

## `DREQ_A`

Offset `0x014` · access `rw` · 32 bits

The four FIFO levels at which the DREQ and panic signals go up. Stored; none goes up.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 148 (`DREQ_A`)

## `INTEN_A`

Offset `0x018` · access `rw` · 32 bits · reset `0x0`

Which of the four conditions may raise the interrupt. Stored.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 149 (`INTEN_A`)

## `INTSTC_A`

Offset `0x01C` · access `w1c` · 32 bits · reset `0x0`

Which of them have happened; write the bit back to clear it. Nothing sets one here.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 150 (`INTSTC_A`)

## `GRAY`

Offset `0x020` · access `rw` · 32 bits · reset `0x0`

The GRAY-code receive mode, for a one-line input that carries its own clock. Stored.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §7.9 Table 151 (`GRAY`)
