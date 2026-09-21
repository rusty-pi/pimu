<!-- generated from specs/dma.toml by `cargo run -- spec-docs --update` – do not edit -->

# `dma` – Legacy DMA controller: 15 channels `0x100` apart plus the controller-wide interrupt status and enable words

- Bus: `vpu` (VPU bus address)
- Base: `0x7E007000`
- Size: `0x1000`
- Interrupts: `CH0` VPU source 80 · `CH1` VPU source 81 · `CH15` VPU source 95 · `CH2` VPU source 82 · `CH3` VPU source 83 · `CH4` VPU source 84 · `CH5` VPU source 85 · `CH6` VPU source 86 · `CH7_8` VPU source 87 · `CH9_10` VPU source 88 · `CH0` GIC id 112 (`GIC_SPI 80`) · `CH1` GIC id 113 (`GIC_SPI 81`) · `CH2` GIC id 114 (`GIC_SPI 82`) · `CH3` GIC id 115 (`GIC_SPI 83`) · `CH4` GIC id 116 (`GIC_SPI 84`) · `CH5` GIC id 117 (`GIC_SPI 85`) · `CH6` GIC id 118 (`GIC_SPI 86`) · `CH7_8` GIC id 119 (`GIC_SPI 87`) · `CH9_10` GIC id 120 (`GIC_SPI 88`)

start4 copies anything of 1 KiB or more through here (`dma_memcpy`). The sixteen channels are three kinds of engine: 0 to 6 the full ones, 7 to 10 DMA Lite (a 16-bit `TXFR_LEN`, no 2D mode, no `SRC_IGNORE` / `DEST_IGNORE`, half the bandwidth), and 11 to 14 DMA4, which are 40-bit and have a register map of their own. Channel 11's slot at `+0xB00` is the DMA4 channel (`dma4`), decoded ahead of this block. The `0x7EE04100` controller (`dma_vpu`) has the same channel layout. A control block's addresses are VC4 bus addresses, alias bits and all, and the engine sits behind the L2: only `0xC000_0000` goes past the caches.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.5 (DMA LITE Engines) and §4.6 (DMA4 Engines): what each kind of engine can and cannot do
- datasheet (high): BCM2711 ARM Peripherals, §4.2 (DMA Controller): channel register blocks `0x100` apart, `INT_STATUS` / `ENABLE` at `0xFE0` / `0xFF0`
- decompile (high): `dma_memcpy` `0x3EC981CC`; `dma_set_cs` `0x3EC98E7C`: `base = ch < 15 ? 0x7E007000 : 0x7EE04100`, `start = *(base + ch * 0x100) = flags | 1`
- trace (high): stock bootloader and start4 `dma_memcpy` transfers with `--log dma`: every control block this boot uses has both ends in the `0x0` alias, and the destination is read straight back through a cached alias with no flush in between — _So a transfer at a cached alias is coherent with the VPU, and `--check-coherency` only counts one at `0xC000_0000` as going behind the caches. Marking every legacy-DMA write as uncached reports ~11k stale reads in a stock boot that works on silicon._
- datasheet (high): BCM2711 ARM Peripherals, §4.2: 16 channels, channel 0 at `0x7E007000` and each next one `0x100` above it, four of them DMA Lite (7 to 10) and four DMA4 (11 to 14); channel 15 sits apart at `0x7EE05000` and is the VPU's alone

Interrupts (`CH0` VPU source 80 · `CH1` VPU source 81 · `CH15` VPU source 95 · `CH2` VPU source 82 · `CH3` VPU source 83 · `CH4` VPU source 84 · `CH5` VPU source 85 · `CH6` VPU source 86 · `CH7_8` VPU source 87 · `CH9_10` VPU source 88 · `CH0` GIC id 112 (`GIC_SPI 80`) · `CH1` GIC id 113 (`GIC_SPI 81`) · `CH2` GIC id 114 (`GIC_SPI 82`) · `CH3` GIC id 115 (`GIC_SPI 83`) · `CH4` GIC id 116 (`GIC_SPI 84`) · `CH5` GIC id 117 (`GIC_SPI 85`) · `CH6` GIC id 118 (`GIC_SPI 86`) · `CH7_8` GIC id 119 (`GIC_SPI 87`) · `CH9_10` GIC id 120 (`GIC_SPI 88`)):

A channel's line is up while its `CS.INT` is, and the driver acknowledges by writing that bit back. Channels 0 to 6 have a line each; the four 'DMA lite' channels 7 to 10 share two. The VPU's own controller takes all sixteen, as sources 80 to 95 (`src/machine.rs`, `dma_irq_source`) — channel 15 out of order at 95, with `AUX` and `ARM` on 93 and 94 in between. Channels 11 to 14 are the DMA4 engines and their lines, 89 to 92, belong to that spec (`specs/dma4.toml`); channel 15 reaches the ARM on neither controller.

- linux (high): `bcm2711.dtsi`: `dma-controller@7e007000` `interrupts = <GIC_SPI 80 ...>` through `<GIC_SPI 86 ...>`, then `87`, `87`, `88`, `88` for the `/* DMA lite 7 - 10 */` channels, named `dma0`..`dma10`
- linux (high): `bcm2835_dma_callback` (`drivers/dma/bcm2835-dma.c`) acknowledges with `writel(BCM2835_DMA_INT, c->chan_base + BCM2835_DMA_CS)`
- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102: VC peripheral IRQs 16 to 22 are `DMA 0` to `DMA 6`, 23 `DMA 7 & 8`, 24 `DMA 9 & 10` and 31 `DMA 15` — VPU sources 80 to 88 and 95. IRQs 25 to 28 (sources 89 to 92) are the DMA4 channels 11 to 14

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000`–`0xE00` (15 × 0x100) | [`CS`](#cs) | rw | 32 | 1, best high |
| `0x004`–`0xE04` (15 × 0x100) | [`CONBLK_AD`](#conblk_ad) | rw | 32 | 2, best high |
| `0x008`–`0xE08` (15 × 0x100) | [`TI`](#ti) | r | 32 | 1, best high |
| `0x00C`–`0xE0C` (15 × 0x100) | [`SOURCE_AD`](#source_ad) | r | 32 | 1, best high |
| `0x010`–`0xE10` (15 × 0x100) | [`DEST_AD`](#dest_ad) | r | 32 | 1, best high |
| `0x014`–`0xE14` (15 × 0x100) | [`TXFR_LEN`](#txfr_len) | r | 32 | 1, best high |
| `0x018`–`0xE18` (15 × 0x100) | [`STRIDE`](#stride) | r | 32 | 1, best high |
| `0x01C`–`0xE1C` (15 × 0x100) | [`NEXTCONBK`](#nextconbk) | rw | 32 | 1, best high |
| `0x020`–`0xE20` (15 × 0x100) | [`DEBUG`](#debug) | rw | 32 | 2, best high |
| `0xFE0` | [`INT_STATUS`](#int_status) | w1c | 32 | 1, best high |
| `0xFF0` | [`ENABLE`](#enable) | rw | 32 | 1, best high |

## `CS`

Offset `0x000`, 15 elements 0x100 apart · access `rw` · 32 bits

Channel control and status. The model runs a transfer to completion on the `ACTIVE` write, so the fields that describe a channel mid-transfer — `PAUSED`, `DREQ`, `WAITING_FOR_OUTSTANDING_WRITES` — are never set, and `RESET` / `ABORT` have nothing to act on.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `ACTIVE` | rw | Channel running. |
| 1 | `END` | rw | Chain finished. |
| 2 | `INT` | rw | Interrupt pending. |
| 3 | `DREQ` | rw | The DREQ `TI.PERMAP` selects, as the channel sees it right now. |
| 4 | `PAUSED` | rw | The channel is paused — `ACTIVE` is clear, or it is between control blocks. |
| 5 | `DREQ_STOPS_DMA` | rw | The channel is paused because the DREQ is low. |
| 6 | `WAITING_FOR_OUTSTANDING_WRITES` | rw | The channel is waiting for its last write to be acknowledged. |
| 8 | `ERROR` | rw | One of the `DEBUG` error bits is set. |
| 19:16 | `PRIORITY` | rw | AXI priority of this channel's bus requests. |
| 23:20 | `PANIC_PRIORITY` | rw | AXI priority to switch to while the peripheral is panicking. |
| 28 | `WAIT_FOR_OUTSTANDING_WRITES` | rw | Wait for every write to be acknowledged before the channel says it has finished. |
| 29 | `DISDEBUG` | rw | Ignore the debug pause signal. |
| 30 | `ABORT` | rw | Abort the control block the channel is on and move to the next. |
| 31 | `RESET` | rw | Reset the channel. Self-clearing. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 (DMA): `CS`

`ACTIVE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 (DMA): `CS`

`END` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 (DMA): `CS`

`INT` sources:

- decompile (high): `dma_interrupt` `0x3EC980E8` only enters `dma_chan_interrupt` for a channel with `CS & 4`

`DREQ` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 38 (`0_CS` to `6_CS`); Table 48 is the DMA Lite version

`PAUSED` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 38 (`0_CS` to `6_CS`); Table 48 is the DMA Lite version

`DREQ_STOPS_DMA` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 38 (`0_CS` to `6_CS`); Table 48 is the DMA Lite version

`WAITING_FOR_OUTSTANDING_WRITES` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 38 (`0_CS` to `6_CS`); Table 48 is the DMA Lite version

`ERROR` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 38 (`0_CS` to `6_CS`); Table 48 is the DMA Lite version

`PRIORITY` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 38 (`0_CS` to `6_CS`); Table 48 is the DMA Lite version

`PANIC_PRIORITY` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 38 (`0_CS` to `6_CS`); Table 48 is the DMA Lite version

`WAIT_FOR_OUTSTANDING_WRITES` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 38 (`0_CS` to `6_CS`); Table 48 is the DMA Lite version

`DISDEBUG` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 38 (`0_CS` to `6_CS`); Table 48 is the DMA Lite version

`ABORT` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 38 (`0_CS` to `6_CS`); Table 48 is the DMA Lite version

`RESET` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 38 (`0_CS` to `6_CS`); Table 48 is the DMA Lite version

## `CONBLK_AD`

Offset `0x004`, 15 elements 0x100 apart · access `rw` · 32 bits

Control-block address. Writing a non-null chain while `ACTIVE` starts it, which is how dmalib starts every transfer.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 (DMA): `CONBLK_AD`
- decompile (high): `dma_chain_start` `0x3EC97544` writes only `*(base + ch * 0x100 + 4) = cb`

## `TI`

Offset `0x008`, 15 elements 0x100 apart · access `r` · 32 bits

Transfer information, loaded from the control block. Control blocks use the same bit layout. The model acts on the four fields below it needs for a memory-to-memory copy; the pacing, burst and width fields are recorded but ignored, which is safe only while nothing in a boot paces a transfer off a peripheral.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `INTEN` | r | Interrupt when this control block is done. |
| 1 | `TDMODE` | r | 2D mode: `TXFR_LEN` is `YLENGTH:XLENGTH` and `STRIDE` applies. |
| 4 | `DEST_INC` | r | Increment the destination address. |
| 8 | `SRC_INC` | r | Increment the source address. |
| 3 | `WAIT_RESP` | r | Wait for each write's AXI response before going on, so writes cannot stack up in the bus pipeline. |
| 5 | `DEST_WIDTH` | r | Destination write width: 0 is 32-bit, 1 is 128-bit. |
| 6 | `DEST_DREQ` | r | Gate the destination writes on the DREQ `PERMAP` selects. |
| 7 | `DEST_IGNORE` | r | Do not write to the destination at all. |
| 9 | `SRC_WIDTH` | r | Source read width: 0 is 32-bit, 1 is 128-bit. |
| 10 | `SRC_DREQ` | r | Gate the source reads on the DREQ `PERMAP` selects. |
| 11 | `SRC_IGNORE` | r | Do not read the source at all. |
| 15:12 | `BURST_LENGTH` | r | How many words the channel tries to transfer per burst; 0 is a single transfer. |
| 20:16 | `PERMAP` | r | Which peripheral's DREQ paces the transfer, 1 to 31; 0 is a continuous unpaced transfer. The peripherals are 1 `DSI0` / `PWM1`, 2 `PCM TX`, 3 `PCM RX`, 4 `SMI`, 5 `PWM0`, 6 `SPI0 TX`, 7 `SPI0 RX`, 8 `BSC/SPI Slave TX`, 9 `BSC/SPI Slave RX`, 10 `HDMI0`, 11 `e.MMC`, 12 `UART0 TX`, 13 `SD HOST`, 14 `UART0 RX`, 15 `DSI1`, 16 `SPI1 TX`, 17 `HDMI1`, 18 `SPI1 RX`, 19 `UART3 TX` / `SPI4 TX`, 20 `UART3 RX` / `SPI4 RX`, 21 `UART5 TX` / `SPI5 TX`, 22 `UART5 RX` / `SPI5 RX`, 23 `SPI6 TX`, 24 `Scaler FIFO 0 & SMI`, 25 `Scaler FIFO 1 & SMI`. The model ignores the field: every transfer in a boot is memory to memory with `PERMAP` 0. |
| 25:21 | `WAITS` | r | Dummy cycles to burn after each read or write, to slow the channel down. |
| 26 | `NO_WIDE_BURST` | r | Do not turn a wide write into a 2-beat burst. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40 (`0_TI` to `6_TI`); Table 47 is the DMA Lite channels’ cut-down version of the same word

`INTEN` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40 (`0_TI` to `6_TI`); Table 47 is the DMA Lite channels’ cut-down version of the same word

`TDMODE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40 (`0_TI` to `6_TI`); Table 47 is the DMA Lite channels’ cut-down version of the same word

`DEST_INC` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40 (`0_TI` to `6_TI`); Table 47 is the DMA Lite channels’ cut-down version of the same word

`SRC_INC` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40 (`0_TI` to `6_TI`); Table 47 is the DMA Lite channels’ cut-down version of the same word

`WAIT_RESP` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40

`DEST_WIDTH` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40

`DEST_DREQ` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40

`DEST_IGNORE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40

`SRC_WIDTH` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40

`SRC_DREQ` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40

`SRC_IGNORE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40

`BURST_LENGTH` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40

`PERMAP` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40 for the field, §4.2.1.3 for the DREQ each number selects

`WAITS` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40

`NO_WIDE_BURST` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 40

## `SOURCE_AD`

Offset `0x00C`, 15 elements 0x100 apart · access `r` · 32 bits

Source address.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 (DMA): `SOURCE_AD`

## `DEST_AD`

Offset `0x010`, 15 elements 0x100 apart · access `r` · 32 bits

Destination address.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 (DMA): `DEST_AD`

## `TXFR_LEN`

Offset `0x014`, 15 elements 0x100 apart · access `r` · 32 bits

Transfer length; `YLENGTH` (29:16) and `XLENGTH` (15:0) in 2D mode.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 (DMA): `TXFR_LEN`

## `STRIDE`

Offset `0x018`, 15 elements 0x100 apart · access `r` · 32 bits

2D strides: signed destination (31:16) and source (15:0) advance per row.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 (DMA): `STRIDE`

## `NEXTCONBK`

Offset `0x01C`, 15 elements 0x100 apart · access `rw` · 32 bits

Next control block in the chain.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 (DMA): `NEXTCONBK`

## `DEBUG`

Offset `0x020`, 15 elements 0x100 apart · access `rw` · 32 bits

Error flags, the engine’s identity, and its state machine. The model reads back what was written and latches no error.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `READ_LAST_NOT_SET_ERROR` | rw | The AXI read data came without its last-beat marker. Write 1 to clear. |
| 1 | `FIFO_ERROR` | rw | The channel’s FIFO over- or underran. Write 1 to clear. |
| 2 | `READ_ERROR` | rw | A read came back with a slave error. Write 1 to clear. |
| 7:4 | `OUTSTANDING_WRITES` | rw | How many writes the channel is still waiting to be acknowledged. |
| 15:8 | `DMA_ID` | rw | The engine’s AXI id. |
| 24:16 | `DMA_STATE` | rw | Where the channel’s state machine is. |
| 27:25 | `VERSION` | rw | Engine version, 2 on this chip. |
| 28 | `LITE` | rw | Set on a DMA Lite engine — channels 7 to 10, which have a 16-bit length, no 2D mode and no ignore modes. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 46 (`0_DEBUG` to `6_DEBUG`); Table 49 is the DMA Lite version
- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 (DMA): `DEBUG`

`READ_LAST_NOT_SET_ERROR` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 46 (`0_DEBUG` to `6_DEBUG`); Table 49 is the DMA Lite version

`FIFO_ERROR` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 46 (`0_DEBUG` to `6_DEBUG`); Table 49 is the DMA Lite version

`READ_ERROR` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 46 (`0_DEBUG` to `6_DEBUG`); Table 49 is the DMA Lite version

`OUTSTANDING_WRITES` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 46 (`0_DEBUG` to `6_DEBUG`); Table 49 is the DMA Lite version

`DMA_ID` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 46 (`0_DEBUG` to `6_DEBUG`); Table 49 is the DMA Lite version

`DMA_STATE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 46 (`0_DEBUG` to `6_DEBUG`); Table 49 is the DMA Lite version

`VERSION` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 46 (`0_DEBUG` to `6_DEBUG`); Table 49 is the DMA Lite version

`LITE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 46 (`0_DEBUG` to `6_DEBUG`); Table 49 is the DMA Lite version

## `INT_STATUS`

Offset `0xFE0` · access `w1c` · 32 bits

One interrupt bit per channel.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 (DMA): `INT_STATUS`

## `ENABLE`

Offset `0xFF0` · access `rw` · 32 bits

One enable bit per channel, and the two nibbles that say where in SDRAM the uncached `0xC000_0000` window lands. Every channel is enabled out of reset; clearing a bit gates that engine’s clock.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 14:0 | `EN` | rw | Bit `n` enables channel `n`. All fifteen are set out of reset; channel 15 is not here. |
| 27:24 | `PAGE` | rw | Which 1 GB page of the 16 GB SDRAM space the 30-bit engines (channels 0 to 6) reach through `0xC000_0000`..`0xFFFF_FFFF`: the final address is `PAGE << 30 \| addr[29:0]`. The model leaves the window where it starts and ignores this — no boot moves it, and the DMA4 engines bypass it entirely. |
| 31:28 | `PAGELITE` | rw | The same page select for the DMA Lite engines, channels 7 to 10. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 62 (`ENABLE`)

`EN` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 62 (`ENABLE`)

`PAGE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 62 (`ENABLE`)

`PAGELITE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 62 (`ENABLE`)
