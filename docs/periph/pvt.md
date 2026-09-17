<!-- generated from specs/pvt.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pvt` – Per-channel PVT (process / voltage / temperature) monitors

- Bus: `vpu` (VPU bus address)
- Base: `0x7D5D8000`
- Size: `0x480`
- Banks: 18 × `0x40`; offsets below are for bank 0
- Carried by: [`clkmon`](clkmon.md)

Eighteen channels, one bank each. Carved out of the `clkmon` window and decoded ahead of it. The measured per-channel thresholds and readings live in `src/periph/pvt.rs`; a register can only carry one reset value, and these differ per channel. start4 leaves channels 2 and 3 out of its core-voltage characterisation (mask `0xC`, `FUN_0ec3503e`), which is why their thresholds read 0 on hardware.

Sources:

- decompile (high): `FUN_0ec300fa(ch, ...)` reads `ch * 0x40 + 0x7d5d8010` / `+0x1c`; start4 initialises `k = 0..17`
- measured (high): `/dev/mem` at `0xFD5D8000` on a Raspberry Pi 4B d03115: all eighteen channels carry the magic

Carried by [`clkmon`](clkmon.md):

Carved out of the clock block's window and decoded ahead of it, like `avs`.

- decompile (high): `FUN_0ec300fa(ch, ...)` addresses `ch * 0x40 + 0x7d5d8010`, inside `clkmon`'s window

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`INDEX`](#index) | r | 32 | 1, best high |
| `0x010` | [`MAGIC`](#magic) | rw | 32 | 4, best high |
| `0x014` | [`THRESHOLD_A`](#threshold_a) | rw | 32 | 3, best high |
| `0x018` | [`THRESHOLD_B`](#threshold_b) | rw | 32 | 2, best high |
| `0x01C` | [`READING`](#reading) | r | 32 | 3, best high |

## `INDEX`

Offset `0x000` · access `r` · 32 bits

The channel's own index.

Sources:

- measured (high): reads 0..17 across the channels on a Raspberry Pi 4B d03115

## `MAGIC`

Offset `0x010` · access `rw` · 32 bits · reset `0x7FFF50CF`

Present marker. Reading 0 would say the channel is absent. start4 writes `0x7FFF50CF` here itself before it reads a channel, 2 us per channel: all eighteen as the characterisation starts, then again for every channel but 2 and 3. Whether hardware holds the value without that write is not known; the model returns it either way.

Sources:

- decompile (high): `FUN_0ec300fa` only reads `+0x1C` when `*(ch * 0x40 + 0x7d5d8010) == 0x7fff50cf`
- measured (high): `0x7fff50cf` on all eighteen channels of a Raspberry Pi 4B d03115
- decompile (high): `FUN_0ec3022a` writes `0x7fff50cf` to `ch * 0x40 + 0x7d5d8010`, called from `FUN_0ec303be` for every channel `FUN_0ec30c40` does not mask
- trace (high): start4 `0x3EC30238`: eighteen writes, then sixteen (channels 2 and 3 skipped)

## `THRESHOLD_A`

Offset `0x014` · access `rw` · 32 bits

The target window for the upper half of `READING`; can be read before anything writes it, so the model seeds it from hardware. start4 writes it at the end of its core-voltage characterisation: `LO = count(13756)`, `HI = count(13756 + slope / 100)`, where `count(s) = (s / 54) * 0x7FFF / 10000` and slope is 59975 on channels 0..2 and 47784 on the rest. That gives `0x03640340` and `0x035D0340`, the values hardware holds. With start4's parameters the window does not depend on anything it measured. Its monitoring pass raises the core voltage while the upper half is below `LO`, and keeps it while the half is at or below `HI`.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 15:0 | `LO` | rw | Low threshold. |
| 31:16 | `HI` | rw | High threshold. |

Sources:

- decompile (high): `FUN_0ec30276` reads and splits it; `FUN_0ec3030e` writes `(lo & 0xffff) | (hi << 16)`
- decompile (high): `FUN_0ec30828` computes it through `FUN_0ec313c0` from `DAT_0edfe0c8` (targets) and `DAT_0ede6a80` (slopes, index 1 for channels 0..2, else 3); `FUN_0ec311d2` compares against it
- trace (high): start4 `0x3EC30320`: `0x03640340` on channels 0 and 1, `0x035D0340` on 4..17, after `THRESHOLD_B`

`LO` sources:

- decompile (high): `FUN_0ec3030e`

`HI` sources:

- decompile (high): `FUN_0ec3030e`

## `THRESHOLD_B`

Offset `0x018` · access `rw` · 32 bits

The target window for the lower half of `READING`, written just before `THRESHOLD_A`: `LO = count(25952)`, `HI = count(25952 + 59975 / 100) = 0x06480624` on every channel start4 uses.

Sources:

- decompile (high): `FUN_0ec30276` / `FUN_0ec3030e`
- trace (high): start4 `0x3EC3032A`: `0x06480624` on channels 0, 1 and 4..17

## `READING`

Offset `0x01C` · access `r` · 32 bits

Measurement pair. start4 reads it at both of its measuring points and works out, for each half, the core voltage at which the channel would run at its target speed; the highest such voltage over all monitors is what it sets. Held constant in the model: counts that barely move leave that voltage at the lower measuring point.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 15:0 | `LO` | r | Low half. |
| 31:16 | `HI` | r | High half. |

Sources:

- decompile (high): `FUN_0ec300fa`: `hi = reading >> 16`, `lo = (ushort)reading`; both zeroed below 10
- decompile (high): `FUN_0ec303e8` passes each half to `FUN_0ec72aae(count_high, count_low, reading_low, reading_high, target)`, targets 13756 (upper half) and 25952 (lower half) from `DAT_0edfe0c8`
- measured (high): per-channel values off a Raspberry Pi 4B d03115, e.g. `0x04270799` on channel 0

`LO` sources:

- decompile (high): `FUN_0ec300fa`

`HI` sources:

- decompile (high): `FUN_0ec300fa`
