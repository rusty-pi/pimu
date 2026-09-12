<!-- generated from specs/bootbox.toml by `cargo run -- spec-docs --update` – do not edit -->

# `bootbox` – Boot-info handoff doorbells, and the VPU interrupt window start4's exception-12 handler reads

- Bus: `vpu` (VPU bus address)
- Base: `0x7EE00000`
- Size: `0x4000`

Whatever consumes the doorbells (a VPU sub-core or secure processor) is not modelled: their control bits read back clear, so every handshake completes, and parameter words read back what was written.

Sources:

- decompile (medium): EEPROM bootloader 0x80009594 and the stub it relocates to 0x60010000
- inferred (medium): size: stops short of the dmalib DMA controller at 0x7EE04100

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x1000` | [`DOORBELL_A`](#doorbell_a) | rw | 32 | 1, best medium |
| `0x1008` | [`DOORBELL_A_SIZE`](#doorbell_a_size) | rw | 32 | 1, best low |
| `0x1080` | [`IRQ_STATUS`](#irq_status) | rw | 32 | 1, best medium |
| `0x1084` | [`IRQ_SOURCE`](#irq_source) | rw | 32 | 1, best medium |
| `0x1088` | [`IRQ_PAYLOAD`](#irq_payload) | rw | 32 | 1, best medium |
| `0x2000` | [`DOORBELL_B`](#doorbell_b) | rw | 32 | 1, best high |
| `0x2100` | [`DOORBELL_C`](#doorbell_c) | rw | 32 | 1, best medium |
| `0x2108` | [`DOORBELL_C_SIZE`](#doorbell_c_size) | rw | 32 | 1, best low |

## `DOORBELL_A`

Offset `0x1000` · access `rw` · 32 bits

Doorbell rung by the relocated stub: trigger in bits 2:1, polled until clear.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3:0 | `CONTROL` | rw | Ready / busy / trigger; always reads back clear. |

Sources:

- decompile (medium): stub at 0x60010000 writes a trigger to 0x7EE01000 and polls it

`CONTROL` sources:

- decompile (medium): every caller spins until the bits it set read back clear

## `DOORBELL_A_SIZE`

Offset `0x1008` · access `rw` · 32 bits

Size parameter for DOORBELL_A.

Sources:

- decompile (low): stub at 0x60010000 pokes +0x08 before the trigger

## `IRQ_STATUS`

Offset `0x1080` · access `rw` · 32 bits

A VPU interrupt source is pending; the handler acks by writing 0.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `PENDING` | rw | A source is pending. |

Sources:

- decompile (medium): start4 exception-12 handler 0x3ED1804E reads 0x7EE01080 and writes 0 back

`PENDING` sources:

- decompile (medium): 0x3ED1804E tests bit 0

## `IRQ_SOURCE`

Offset `0x1084` · access `rw` · 32 bits

Id of the pending source.

Sources:

- decompile (medium): 0x3ED1804E reads +0x04 of the window

## `IRQ_PAYLOAD`

Offset `0x1088` · access `rw` · 32 bits

Payload of the pending source.

Sources:

- decompile (medium): 0x3ED1804E reads +0x08 of the window

## `DOORBELL_B`

Offset `0x2000` · access `rw` · 32 bits

Boot-info doorbell: the bootloader stages a BSTE / BVER block at 0xC0040000, sets bit 1 and spins until it clears.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3:0 | `CONTROL` | rw | Ready / busy / trigger; always reads back clear. |

Sources:

- decompile (high): 0x80009594 sets bit 1 of 0x7EE02000 and spins

`CONTROL` sources:

- decompile (high): 0x80009594 spins until bit 1 reads back clear

## `DOORBELL_C`

Offset `0x2100` · access `rw` · 32 bits

Doorbell rung by the relocated stub, like DOORBELL_A.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3:0 | `CONTROL` | rw | Ready / busy / trigger; always reads back clear. |

Sources:

- decompile (medium): stub at 0x60010000 writes a trigger to 0x7EE02100 and polls it

`CONTROL` sources:

- decompile (medium): the stub spins until the trigger reads back clear

## `DOORBELL_C_SIZE`

Offset `0x2108` · access `rw` · 32 bits

Size parameter for DOORBELL_C.

Sources:

- decompile (low): stub at 0x60010000 pokes +0x08 before the trigger
