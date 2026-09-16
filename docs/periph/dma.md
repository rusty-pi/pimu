<!-- generated from specs/dma.toml by `cargo run -- spec-docs --update` – do not edit -->

# `dma` – Legacy DMA controller: 15 channels `0x100` apart plus the controller-wide interrupt status and enable words

- Bus: `vpu` (VPU bus address)
- Base: `0x7E007000`
- Size: `0x1000`

start4 copies anything of 1 KiB or more through here (`dma_memcpy`). Channel 11's slot at `+0xB00` is the DMA4 channel (`dma4`), decoded ahead of this block. The `0x7EE04100` controller (`dma_vpu`) has the same channel layout.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA Controller chapter: channel register blocks `0x100` apart, `INT_STATUS` / `ENABLE` at `0xFE0` / `0xFF0`
- decompile (high): `dma_memcpy` `0x3EC981CC`; `dma_set_cs` `0x3EC98E7C`: `base = ch < 15 ? 0x7E007000 : 0x7EE04100`, `start = *(base + ch * 0x100) = flags | 1`

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
| `0x020`–`0xE20` (15 × 0x100) | [`DEBUG`](#debug) | rw | 32 | 1, best high |
| `0xFE0` | [`INT_STATUS`](#int_status) | w1c | 32 | 1, best high |
| `0xFF0` | [`ENABLE`](#enable) | rw | 32 | 1, best high |

## `CS`

Offset `0x000`, 15 elements 0x100 apart · access `rw` · 32 bits

Channel control and status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `ACTIVE` | rw | Channel running. |
| 1 | `END` | rw | Chain finished. |
| 2 | `INT` | rw | Interrupt pending. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `CS`

`ACTIVE` sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `CS`

`END` sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `CS`

`INT` sources:

- decompile (high): `dma_interrupt` `0x3EC980E8` only enters `dma_chan_interrupt` for a channel with `CS & 4`

## `CONBLK_AD`

Offset `0x004`, 15 elements 0x100 apart · access `rw` · 32 bits

Control-block address. Writing a non-null chain while `ACTIVE` starts it, which is how dmalib starts every transfer.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `CONBLK_AD`
- decompile (high): `dma_chain_start` `0x3EC97544` writes only `*(base + ch * 0x100 + 4) = cb`

## `TI`

Offset `0x008`, 15 elements 0x100 apart · access `r` · 32 bits

Transfer information, loaded from the control block. Control blocks use the same bit layout.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `INTEN` | r | Interrupt when this control block is done. |
| 1 | `TDMODE` | r | 2D mode: `TXFR_LEN` is `YLENGTH:XLENGTH` and `STRIDE` applies. |
| 4 | `DEST_INC` | r | Increment the destination address. |
| 8 | `SRC_INC` | r | Increment the source address. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `TI`

`INTEN` sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `TI`

`TDMODE` sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `TI`

`DEST_INC` sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `TI`

`SRC_INC` sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `TI`

## `SOURCE_AD`

Offset `0x00C`, 15 elements 0x100 apart · access `r` · 32 bits

Source address.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `SOURCE_AD`

## `DEST_AD`

Offset `0x010`, 15 elements 0x100 apart · access `r` · 32 bits

Destination address.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `DEST_AD`

## `TXFR_LEN`

Offset `0x014`, 15 elements 0x100 apart · access `r` · 32 bits

Transfer length; `YLENGTH` (29:16) and `XLENGTH` (15:0) in 2D mode.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `TXFR_LEN`

## `STRIDE`

Offset `0x018`, 15 elements 0x100 apart · access `r` · 32 bits

2D strides: signed destination (31:16) and source (15:0) advance per row.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `STRIDE`

## `NEXTCONBK`

Offset `0x01C`, 15 elements 0x100 apart · access `rw` · 32 bits

Next control block in the chain.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `NEXTCONBK`

## `DEBUG`

Offset `0x020`, 15 elements 0x100 apart · access `rw` · 32 bits

Error flags.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `DEBUG`

## `INT_STATUS`

Offset `0xFE0` · access `w1c` · 32 bits

One interrupt bit per channel.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `INT_STATUS`

## `ENABLE`

Offset `0xFF0` · access `rw` · 32 bits

One enable bit per channel.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, DMA: `ENABLE`
