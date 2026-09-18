<!-- generated from specs/dma4.toml by `cargo run -- spec-docs --update` – do not edit -->

# `dma4` – DMA4 (`dma40`) channel: the 40-bit DMA engine the bootloader and start4 use

- Bus: `vpu` (VPU bus address)
- Base: `0x7E007B00`
- Size: `0x100`
- Carried by: [`dma`](dma.md)

Sits in channel 11's slot of the legacy controller (`dma`) and is decoded ahead of it. Its control blocks carry address bits 39:32 in `SRCI` / `DESTI`, which is how a 32-bit VPU reaches the PCIe window at `0x6_0000_0000`.

Sources:

- decompile (high): bootloader submit `0x0008b0xx` and status check `0x0008b3f4`
- decompile (high): start4 dmalib: `dma_set_cs` `0x3EC98E7C`, `dma_chain_start` `0x3EC97544`, channel 11 for the xHCI takeover

Carried by [`dma`](dma.md):

Sits in channel 11's slot of the legacy controller and is decoded ahead of it.

- decompile (high): start4 dmalib `dma_set_cs` `0x3EC98E7C` computes `0x7E007000 + ch * 0x100`; channel 11 is this 40-bit engine

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CS`](#cs) | rw | 32 | 1, best high |
| `0x004` | [`CB`](#cb) | rw | 32 | 1, best high |
| `0x00C` | [`DEBUG`](#debug) | w1c | 32 | 1, best high |
| `0x028` | [`REG_28`](#reg_28) | rw | 32 | 1, best high |

## `CS`

Offset `0x000` · access `rw` · 32 bits

Control and status. A transfer starts when `ACTIVE` is set with a chain in `CB`, or `CB` is written while `ACTIVE`.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `ACTIVE` | rw | Channel running. |
| 1 | `END` | rw | Chain finished. |
| 2 | `INT` | rw | Completion interrupt pending; set when a control block had `INTEN`. |
| 10 | `ERROR` | rw | Transfer failed. |
| 24 | `BUSY` | rw | Treated as 'still busy' alongside `ACTIVE`. |

Sources:

- decompile (high): bootloader sets bit 0 to start and polls `END` / `ACTIVE` / `ERROR` (`0x0008b3f4`)

`ACTIVE` sources:

- decompile (high): `0x0008b3f4`: success is `END` set, `ACTIVE` clear, `ERROR` clear

`END` sources:

- decompile (high): `0x0008b3f4`

`INT` sources:

- decompile (high): dmalib's `dma_interrupt` only services a channel with `CS & 4`

`ERROR` sources:

- decompile (high): `0x0008b3f4` returns -1 on bit 10

`BUSY` sources:

- decompile (medium): `0x0008b3f4` tests bit 24 with `ACTIVE`

## `CB`

Offset `0x004` · access `rw` · 32 bits

Control-block `address >> 5`. Null once the chain has run.

Sources:

- decompile (high): bootloader writes the control-block `address >> 5`; `dma_chain_start` `0x3EC97544` writes only this

## `DEBUG`

Offset `0x00C` · access `w1c` · 32 bits

Error latch; the bootloader writes `0x400` to clear it before each transfer.

Sources:

- decompile (high): bootloader submit path writes `DEBUG = 0x400` first

## `REG_28`

Offset `0x028` · access `rw` · 32 bits

Past the channel's control-block registers. The bootloader writes 0 here once, as it puts the channel away after its last transfer, and never reads it. Meaning unknown.

Sources:

- trace (high): bootmain `0x0008B668`: `0x00000000` to `0x7E007B28`, the only access to the word in a boot
