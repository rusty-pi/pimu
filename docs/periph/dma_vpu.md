<!-- generated from specs/dma_vpu.toml by `cargo run -- spec-docs --update` – do not edit -->

# `dma_vpu` – The DMA controller start4's dmalib drives: 16 channel slots, channel 15 at `0x7EE05000`

- Bus: `vpu` (VPU bus address)
- Base: `0x7EE04100`
- Size: `0x1000`

Same channel layout as the legacy controller (`dma`), but all 16 slots are channels, so there is no room for its controller-wide words. Before it was mapped, channel 15 landed in the catch-all stub.

Sources:

- decompile (high): `dma_set_cs` `0x3EC98E7C` / `dma_chain_start` `0x3EC97544`: `base = ch < 15 ? 0x7E007000 : 0x7EE04100`, registers at `base + ch * 0x100`; the transfer queue runs on channel 15

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000`–`0xF00` (16 × 0x100) | [`CS`](#cs) | rw | 32 | 1, best high |
| `0x004`–`0xF04` (16 × 0x100) | [`CONBLK_AD`](#conblk_ad) | rw | 32 | 1, best high |
| `0x008`–`0xF08` (16 × 0x100) | [`TI`](#ti) | r | 32 | 1, best medium |
| `0x00C`–`0xF0C` (16 × 0x100) | [`SOURCE_AD`](#source_ad) | r | 32 | 1, best medium |
| `0x010`–`0xF10` (16 × 0x100) | [`DEST_AD`](#dest_ad) | r | 32 | 1, best medium |
| `0x014`–`0xF14` (16 × 0x100) | [`TXFR_LEN`](#txfr_len) | r | 32 | 1, best medium |
| `0x018`–`0xF18` (16 × 0x100) | [`STRIDE`](#stride) | r | 32 | 1, best medium |
| `0x01C`–`0xF1C` (16 × 0x100) | [`NEXTCONBK`](#nextconbk) | rw | 32 | 1, best medium |
| `0x020`–`0xF20` (16 × 0x100) | [`DEBUG`](#debug) | rw | 32 | 1, best medium |

## `CS`

Offset `0x000`, 16 elements 0x100 apart · access `rw` · 32 bits

Channel control and status, laid out as `dma` `CS`.

Sources:

- decompile (high): `dma_set_cs` `0x3EC98E7C` writes `flags | 1`

## `CONBLK_AD`

Offset `0x004`, 16 elements 0x100 apart · access `rw` · 32 bits

Control-block address.

Sources:

- decompile (high): `dma_chain_start` `0x3EC97544` writes `base + ch * 0x100 + 4`

## `TI`

Offset `0x008`, 16 elements 0x100 apart · access `r` · 32 bits

Transfer information.

Sources:

- inferred (medium): the legacy channel layout, which the base selection in `dma_set_cs` treats as interchangeable

## `SOURCE_AD`

Offset `0x00C`, 16 elements 0x100 apart · access `r` · 32 bits

Source address.

Sources:

- inferred (medium): the legacy channel layout

## `DEST_AD`

Offset `0x010`, 16 elements 0x100 apart · access `r` · 32 bits

Destination address.

Sources:

- inferred (medium): the legacy channel layout

## `TXFR_LEN`

Offset `0x014`, 16 elements 0x100 apart · access `r` · 32 bits

Transfer length.

Sources:

- inferred (medium): the legacy channel layout

## `STRIDE`

Offset `0x018`, 16 elements 0x100 apart · access `r` · 32 bits

2D strides.

Sources:

- inferred (medium): the legacy channel layout

## `NEXTCONBK`

Offset `0x01C`, 16 elements 0x100 apart · access `rw` · 32 bits

Next control block.

Sources:

- inferred (medium): the legacy channel layout

## `DEBUG`

Offset `0x020`, 16 elements 0x100 apart · access `rw` · 32 bits

Error flags.

Sources:

- inferred (medium): the legacy channel layout
