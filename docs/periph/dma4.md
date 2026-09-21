<!-- generated from specs/dma4.toml by `cargo run -- spec-docs --update` – do not edit -->

# `dma4` – DMA4 (`dma40`) channel: the 40-bit DMA engine the bootloader and start4 use

- Bus: `vpu` (VPU bus address)
- Base: `0x7E007B00`
- Size: `0x100`
- Carried by: [`dma`](dma.md)
- Interrupts: VPU source 89 · GIC id 121 (`GIC_SPI 89`)

Sits in channel 11's slot of the legacy controller (`dma`) and is decoded ahead of it. Its control blocks carry address bits 39:32 in `SRCI` / `DESTI`, which is how a 32-bit VPU reaches the PCIe window at `0x6_0000_0000`. Those addresses are CPU-physical, with no alias bits to say where an access lands, but the engine is behind the L2 like the legacy one.

Sources:

- decompile (high): bootloader submit `0x0008b0xx` and status check `0x0008b3f4`
- decompile (high): start4 dmalib: `dma_set_cs` `0x3EC98E7C`, `dma_chain_start` `0x3EC97544`, channel 11 for the xHCI takeover
- trace (high): stock bootloader USB boot: the control block it starts the channel with lives at `0xBEF6D4A0` — the `0x8000_0000` (L2) alias — and is rewritten before every transfer with no flush in between — _The firmware's own USB boot depends on the engine reading what the VPU left in the L2, so `--check-coherency` treats this channel as coherent. The word it DMAs a register into is still read back through `0xC000_0000` by both stock and ours, which is what makes that safe either way._
- datasheet (high): BCM2711 ARM Peripherals, §4.6: the DMA4 engines have an uncoupled read/write design, reach the full 35-bit address bus and so bypass the `PAGE` / `PAGELITE` windows the other engines page through, and "DMA channel 11 is additionally able to access the PCIe interface"
- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 37: channel 11's registers are `CS` `0xB00`, `CB` `0xB04`, `DEBUG` `0xB0C`, then CB words `TI` `0xB10`, `SRC` `0xB14`, `SRCI` `0xB18`, `DEST` `0xB1C`, `DESTI` `0xB20`, `LEN` `0xB24`, `NEXT_CB` `0xB28` and `DEBUG2` `0xB2C`

Carried by [`dma`](dma.md):

Sits in channel 11's slot of the legacy controller and is decoded ahead of it.

- decompile (high): start4 dmalib `dma_set_cs` `0x3EC98E7C` computes `0x7E007000 + ch * 0x100`; channel 11 is this 40-bit engine

Interrupts (VPU source 89 · GIC id 121 (`GIC_SPI 89`)):

Channel 11's completion. The four DMA4 channels 11 to 14 take their lines in order, 89 to 92, and the same number on both controllers.

- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102: VC peripheral IRQs 25 to 28 are `DMA 11` to `DMA 14`, VPU sources 89 to 92 and, 64 above the IRQ number again, `GIC_SPI 89` to `GIC_SPI 92`
- linux (high): `firmware/bcm2711-rpi-4-b.dtb` `/scb/dma@7e007b00`: `interrupts = <0x0 0x59 0x4>, <0x0 0x5a 0x4>, <0x0 0x5b 0x4>, <0x0 0x5c 0x4>`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CS`](#cs) | rw | 32 | 1, best high |
| `0x004` | [`CB`](#cb) | rw | 32 | 1, best high |
| `0x00C` | [`DEBUG`](#debug) | w1c | 32 | 1, best high |
| `0x028` | [`NEXT_CB`](#next_cb) | rw | 32 | 2, best high |

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

## `NEXT_CB`

Offset `0x028` · access `rw` · 32 bits

Word 6 of the control block the channel loaded: the address of the next one, or 0 to end the chain. The bootloader writes 0 here once, as it puts the channel away after its last transfer, and never reads it.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §4.2.1.2 Table 37: `11_NEXT_CB` at `0x7E007B28`, "DMA4 Channel 11 CB Word 6"
- trace (high): bootmain `0x0008B668`: `0x00000000` to `0x7E007B28`, the only access to the word in a boot — _recorded as `REG_28`, meaning unknown, until the datasheet named it_
