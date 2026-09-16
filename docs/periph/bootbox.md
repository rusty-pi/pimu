<!-- generated from specs/bootbox.toml by `cargo run -- spec-docs --update` – do not edit -->

# `bootbox` – Boot-info handoff doorbells, and the VPU interrupt window start4's exception-12 handler reads

- Bus: `vpu` (VPU bus address)
- Base: `0x7EE00000`
- Size: `0x4000`

Whatever consumes the doorbells (a VPU sub-core or secure processor) is not modelled: their control bits read back clear, so every handshake completes, and parameter words read back what was written.

Sources:

- decompile (medium): EEPROM bootloader `0x80009594` and the stub it relocates to `0x60010000`
- inferred (medium): size: stops short of the dmalib DMA controller at `0x7EE04100`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x03C` | [`REG_03C`](#reg_03c) | rw | 32 | 1, best high |
| `0x040` | [`REG_040`](#reg_040) | rw | 32 | 1, best high |
| `0x1000` | [`L2_CTRL`](#l2_ctrl) | rw | 32 | 4, best high |
| `0x1004` | [`L2_FLUSH_START`](#l2_flush_start) | rw | 32 | 2, best medium |
| `0x1008` | [`L2_FLUSH_END`](#l2_flush_end) | rw | 32 | 2, best medium |
| `0x1080` | [`IRQ_STATUS`](#irq_status) | rw | 32 | 1, best medium |
| `0x1084` | [`IRQ_SOURCE`](#irq_source) | rw | 32 | 1, best medium |
| `0x1088` | [`IRQ_PAYLOAD`](#irq_payload) | rw | 32 | 1, best medium |
| `0x2000` | [`DOORBELL_B`](#doorbell_b) | rw | 32 | 1, best high |
| `0x2004` | [`REG_2004`](#reg_2004) | rw | 32 | 1, best high |
| `0x2008` | [`REG_2008`](#reg_2008) | rw | 32 | 1, best high |
| `0x200C` | [`REG_200C`](#reg_200c) | rw | 32 | 1, best high |
| `0x2080` | [`REG_2080`](#reg_2080) | rw | 32 | 1, best high |
| `0x2084` | [`REG_2084`](#reg_2084) | rw | 32 | 1, best high |
| `0x2088` | [`REG_2088`](#reg_2088) | rw | 32 | 1, best high |
| `0x208C` | [`REG_208C`](#reg_208c) | rw | 32 | 1, best high |
| `0x2100` | [`DOORBELL_C`](#doorbell_c) | rw | 32 | 2, best high |
| `0x2104` | [`DOORBELL_C_START`](#doorbell_c_start) | rw | 32 | 1, best high |
| `0x2108` | [`DOORBELL_C_SIZE`](#doorbell_c_size) | rw | 32 | 1, best low |
| `0x210C` | [`REG_210C`](#reg_210c) | rw | 32 | 1, best high |

## `REG_03C`

Offset `0x03C` · access `rw` · 32 bits

start4 writes `0x0EC00000` here as it starts, then `0x0EC01FFF` to `REG_040`, then `0x0EC00001` here: a base, a limit and bit 0 set last, over the start of the range start4 is linked at. Meaning unknown.

Sources:

- trace (high): pinned start4 entry: `0x0EC00000` at `0xFEC00DF6`, `0x0EC00001` at `0xFEC00E0C`

## `REG_040`

Offset `0x040` · access `rw` · 32 bits

start4 writes `0x0EC01FFF` here between its two `REG_03C` writes. Meaning unknown.

Sources:

- trace (high): pinned start4 entry: `0x0EC01FFF` at `0xFEC00E02`

## `L2_CTRL`

Offset `0x1000` · access `rw` · 32 bits

The L2 cache's maintenance port, as far as the evidence goes. The stub the bootcode relocates to `0x60010000` writes a range to `L2_FLUSH_START` / `L2_FLUSH_END`, then `0x14` here, and polls until it reads back `0x10`, right before it jumps to the next stage; bootmain does the same with `0x44` over single buffers. The low bits read back clear. The model takes `FLUSH` as clean-and-invalidate over the range, which ends the bootcode's cache-as-RAM window (`src/l2.rs`, #70). start4 configures it as it starts, `(value & 0xFFF0FFE5) | 0x430000`, and uses `0x430014`, `0x430044`, `0x430050` and `0x430054` from then on; bit 1 it sets once, as it applies `config.txt` (`0x430042` in the trace).

| Bits | Field | Access | Notes |
|---|---|---|---|
| 2 | `FLUSH` | rw | Clean and invalidate `L2_FLUSH_START..=L2_FLUSH_END`; reads back clear. |

Sources:

- decompile (medium): stub at `0x60010000` writes a trigger to `0x7EE01000` and polls it
- trace (medium): 2022-04-26 and pinned bootcode: `0x7EE01004 = 0`, `0x7EE01008 = 0x0FFFFFE0`, then `0x14` to `0x7EE01000`; pinned bootmain: `0x00A20000..0x00A20116`, then `0x44` (#70)
- decompile (high): start4 entry `0x3EC7114E..0x3EC7119E`: reads it (bit 0 test), then `And 0xFFF0FFE5`, `Or 0x430000`, `St`; `0x3ED486CE..0x3ED486D2` sets bit 1 when the word at `gp+838588` is 0
- trace (high): pinned start4: `0x430000` at `0x3EC7119E`, `0x430044` at `0x3EC715A0`, `0x430014` / `0x430054` at `0x3EC71668`, `0x430050` at `0x3EC9806A`, `0x430042` at `0x3ED486D2`

`FLUSH` sources:

- trace (low): set in both commands that follow a range, clear in the `0x50` written between them

## `L2_FLUSH_START`

Offset `0x1004` · access `rw` · 32 bits

First address of the range `L2_CTRL.FLUSH` acts on.

Sources:

- trace (medium): written right before `L2_FLUSH_END` and the `L2_CTRL` command (#70)
- trace (medium): pinned start4 writes ranges here all through the boot (`0x3EC715EE`; e.g. `0xBEF27640` with `L2_FLUSH_END` `0xBEF4763F`), the same pair to `0x7EE02104` / `DOORBELL_C_SIZE` just before, and `L2_CTRL` commands `0x430000` / `0x430014` (`0x3EC7119E`, `0x3EC71668`)

## `L2_FLUSH_END`

Offset `0x1008` · access `rw` · 32 bits

Last address of the range `L2_CTRL.FLUSH` acts on: `0x0FFFFFE0` from the bootcode's stub, so the lines are 32 bytes. It was taken for a size before #70.

Sources:

- decompile (low): stub at `0x60010000` pokes `+0x08` before the trigger
- trace (medium): bootmain writes `0x00A20000` to `+0x04` and `0x00A20116` to `+0x08` (#70)

## `IRQ_STATUS`

Offset `0x1080` · access `rw` · 32 bits

A VPU interrupt source is pending; the handler acks by writing 0.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `PENDING` | rw | A source is pending. |

Sources:

- decompile (medium): start4 exception-12 handler `0x3ED1804E` reads `0x7EE01080` and writes 0 back

`PENDING` sources:

- decompile (medium): `0x3ED1804E` tests bit 0

## `IRQ_SOURCE`

Offset `0x1084` · access `rw` · 32 bits

Id of the pending source.

Sources:

- decompile (medium): `0x3ED1804E` reads `+0x04` of the window

## `IRQ_PAYLOAD`

Offset `0x1088` · access `rw` · 32 bits

Payload of the pending source.

Sources:

- decompile (medium): `0x3ED1804E` reads `+0x08` of the window

## `DOORBELL_B`

Offset `0x2000` · access `rw` · 32 bits

Boot-info doorbell: the bootloader stages a `BSTE` / `BVER` block at `0xC0040000`, sets bit 1 and spins until it clears. start4 tests bit 0 as it starts (`0x3EC7111A`), and its cache-flush routine sets bit 1 for its flag bit 0 without waiting.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3:0 | `CONTROL` | rw | Ready / busy / trigger; always reads back clear. |

Sources:

- decompile (high): `0x80009594` sets bit 1 of `0x7EE02000` and spins

`CONTROL` sources:

- decompile (high): `0x80009594` spins until bit 1 reads back clear

## `REG_2004`

Offset `0x2004` · access `rw` · 32 bits

start4 writes `0x1139` here as it starts, and the same to `REG_2084`. Meaning unknown.

Sources:

- trace (high): pinned start4 entry: `0x00001139` at `0x3EC711A4`

## `REG_2008`

Offset `0x2008` · access `rw` · 32 bits

start4 writes `0` here as it starts, and the same to `REG_2088`. With `REG_200C` it may be a range, like the pair after `DOORBELL_C`; a guess.

Sources:

- trace (high): pinned start4 entry: `0x00000000` at `0x3EC711B0`

## `REG_200C`

Offset `0x200C` · access `rw` · 32 bits

start4 writes `0xFFFFFFFF` here as it starts, and the same to `REG_208C`. Meaning unknown.

Sources:

- trace (high): pinned start4 entry: `0xFFFFFFFF` at `0x3EC711B6`

## `REG_2080`

Offset `0x2080` · access `rw` · 32 bits

Set up like `DOORBELL_B`: start4 tests its bit 0 as it starts, and its cache-flush routine sets bit 1 here for flag bit 1, where `DOORBELL_B` takes flag bit 0.

Sources:

- decompile (high): start4 entry `0x3EC71134` tests bit 0; cache-flush routine `0x3EC7153E`..`0x3EC71550` sets bit 1 of `0x7EE02000` for flag bit 0 and of `0x7EE02080` for flag bit 1

## `REG_2084`

Offset `0x2084` · access `rw` · 32 bits

start4 writes `0x1139` here as it starts, as to `REG_2004`.

Sources:

- trace (high): pinned start4 entry: `0x00001139` at `0x3EC711A8`

## `REG_2088`

Offset `0x2088` · access `rw` · 32 bits

start4 writes `0` here as it starts, as to `REG_2008`.

Sources:

- trace (high): pinned start4 entry: `0x00000000` at `0x3EC711B2`

## `REG_208C`

Offset `0x208C` · access `rw` · 32 bits

start4 writes `0xFFFFFFFF` here as it starts, as to `REG_200C`.

Sources:

- trace (high): pinned start4 entry: `0xFFFFFFFF` at `0x3EC711B8`

## `DOORBELL_C`

Offset `0x2100` · access `rw` · 32 bits

Doorbell rung by the relocated stub, like `DOORBELL_A`. start4 tests bit 0 as it starts (`0x3EC71168`). Its cache-flush routine (`0x3EC714FC`, and a copy at `0x3EC715C4`) takes a first address, a length and flags: it writes the first and last address to `DOORBELL_C_START` / `DOORBELL_C_SIZE` and to `L2_FLUSH_START` / `L2_FLUSH_END`, sets bit 1 of `DOORBELL_B` / `REG_2080` for flag bits 0 / 1, sets bits 1 / 2 here for flag bits 2 / 3 and waits for both to read clear, then for flag bit 4 writes `(L2_CTRL & ~0x18) | 4` and waits for bit 2 to clear. Just before the ARM starts it runs over `0x00000000`..`0x2EFFFFFF`, which holds the kernel and the device tree, with flags `0x1C`: `6` here, `0x430044` to `L2_CTRL`.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3:0 | `CONTROL` | rw | Ready / busy / trigger; always reads back clear. |

Sources:

- decompile (medium): stub at `0x60010000` writes a trigger to `0x7EE02100` and polls it
- trace (high): pinned start4 before the ARM starts: `DOORBELL_C_START` `0` at `0x3EC71516`, `DOORBELL_C_SIZE` `0x2EFFFFFF` at `0x3EC71518`, the same to `L2_FLUSH_START` / `L2_FLUSH_END` at `0x3EC71526` / `0x3EC71528`, `6` here at `0x3EC71580` polled at `0x3EC71582`, `L2_CTRL` `0x430044` at `0x3EC715A0` polled at `0x3EC715A2`

`CONTROL` sources:

- decompile (medium): the stub spins until the trigger reads back clear

## `DOORBELL_C_START`

Offset `0x2104` · access `rw` · 32 bits

First address of the range a `DOORBELL_C` command acts on. start4's cache-flush routine writes it right before `DOORBELL_C_SIZE`.

Sources:

- trace (high): pinned start4: `0x00000000` at `0x3EC71516` before the ARM starts; ranges such as `0xBEF27640` at `0x3EC715DE` all through the boot

## `DOORBELL_C_SIZE`

Offset `0x2108` · access `rw` · 32 bits

Taken for a size parameter for `DOORBELL_C`; start4 writes the last address of the range here (first address + length - 1), as it does to `L2_FLUSH_END`.

Sources:

- decompile (low): stub at `0x60010000` pokes `+0x08` before the trigger

## `REG_210C`

Offset `0x210C` · access `rw` · 32 bits

start4 writes `0x02220222` here as it starts, between `REG_2084` and `REG_2008`. Meaning unknown.

Sources:

- trace (high): pinned start4 entry: `0x02220222` at `0x3EC711AC`
