<!-- generated from specs/pvt.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pvt` – Per-channel PVT (process / voltage / temperature) monitors

- Bus: `vpu` (VPU bus address)
- Base: `0x7D5D8000`
- Size: `0x480`
- Banks: 18 × `0x40`; offsets below are for bank 0

Eighteen channels, one bank each. Carved out of the `clkmon` window and decoded ahead of it. The measured per-channel thresholds and readings live in src/periph/pvt.rs; a register can only carry one reset value, and these differ per channel.

Sources:

- decompile (high): FUN_0ec300fa(ch, ...) reads ch * 0x40 + 0x7d5d8010 / +0x1c; start4 initialises k = 0..17
- measured (high): /dev/mem at 0xFD5D8000 on a Raspberry Pi 4B d03115: all eighteen channels carry the magic

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`INDEX`](#index) | r | 32 | 1, best high |
| `0x010` | [`MAGIC`](#magic) | r | 32 | 2, best high |
| `0x014` | [`THRESHOLD_A`](#threshold_a) | rw | 32 | 1, best high |
| `0x018` | [`THRESHOLD_B`](#threshold_b) | rw | 32 | 1, best high |
| `0x01C` | [`READING`](#reading) | r | 32 | 2, best high |

## `INDEX`

Offset `0x000` · access `r` · 32 bits

The channel's own index.

Sources:

- measured (high): reads 0..17 across the channels on a Raspberry Pi 4B d03115

## `MAGIC`

Offset `0x010` · access `r` · 32 bits · reset `0x7FFF50CF`

Present marker. Reading 0 would say the channel is absent.

Sources:

- decompile (high): FUN_0ec300fa only reads +0x1C when *(ch * 0x40 + 0x7d5d8010) == 0x7fff50cf
- measured (high): 0x7fff50cf on all eighteen channels of a Raspberry Pi 4B d03115

## `THRESHOLD_A`

Offset `0x014` · access `rw` · 32 bits

Threshold pair; can be read before anything writes it, so the model seeds it from hardware.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 15:0 | `LO` | rw | Low threshold. |
| 31:16 | `HI` | rw | High threshold. |

Sources:

- decompile (high): FUN_0ec30276 reads and splits it; FUN_0ec3030e writes (lo & 0xffff) | (hi << 16)

`LO` sources:

- decompile (high): FUN_0ec3030e

`HI` sources:

- decompile (high): FUN_0ec3030e

## `THRESHOLD_B`

Offset `0x018` · access `rw` · 32 bits

Second threshold pair, as THRESHOLD_A.

Sources:

- decompile (high): FUN_0ec30276 / FUN_0ec3030e

## `READING`

Offset `0x01C` · access `r` · 32 bits

Measurement pair. Held constant in the model: a stable reading means 'no adaptive correction'.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 15:0 | `LO` | r | Low half. |
| 31:16 | `HI` | r | High half. |

Sources:

- decompile (high): FUN_0ec300fa: hi = reading >> 16, lo = (ushort)reading; both zeroed below 10
- measured (high): per-channel values off a Raspberry Pi 4B d03115, e.g. 0x04270799 on channel 0

`LO` sources:

- decompile (high): FUN_0ec300fa

`HI` sources:

- decompile (high): FUN_0ec300fa
