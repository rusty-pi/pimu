<!-- generated from specs/otp.toml by `cargo run -- spec-docs --update` – do not edit -->

# `otp` – Always-on config / OTP engine: one-row-at-a-time reads of the fuse array

- Bus: `vpu` (VPU bus address)
- Base: `0x7E20F000`
- Size: `0x1000`

The fuse contents live in src/periph/configotp.rs and must never be a real board's (CLAUDE.md). Offsets without a known register keep the old 'always ready' status bits (17, 18, 7) so unrelated pollers progress.

Sources:

- decompile (high): EEPROM bootloader getconfig(key): key -> +0x1C, 0 -> +0x0C / +0x08, +0x08 |= 1, poll +0x10, value <- +0x18

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x004` | [`CLKMUX`](#clkmux) | rw | 32 | 1, best medium |
| `0x008` | [`PARAM_A`](#param_a) | rw | 32 | 1, best high |
| `0x00C` | [`PARAM_B`](#param_b) | rw | 32 | 1, best medium |
| `0x010` | [`STATUS`](#status) | rw | 32 | 1, best high |
| `0x018` | [`DATA`](#data) | r | 32 | 1, best high |
| `0x01C` | [`KEY`](#key) | rw | 32 | 2, best high |

## `CLKMUX`

Offset `0x004` · access `rw` · 32 bits

Clock-mux pokes (3 / 0 / 2), absorbed.

Sources:

- decompile (medium): the bootloader writes 3, 0, 2 here

## `PARAM_A`

Offset `0x008` · access `rw` · 32 bits

Transaction parameters; bit 0 starts it.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `GO` | rw | Start the transaction. |

Sources:

- decompile (high): getconfig: writes 0, then |= 1

`GO` sources:

- decompile (high): getconfig sets bit 0 to trigger

## `PARAM_B`

Offset `0x00C` · access `rw` · 32 bits

Second transaction parameter, written 0.

Sources:

- decompile (medium): getconfig writes 0

## `STATUS`

Offset `0x010` · access `rw` · 32 bits

Transaction status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 1 | `DONE` | w1c | The row is in DATA. |

Sources:

- decompile (high): poll at 0x8000760e: btest [+0x10], #1

`DONE` sources:

- decompile (high): 0x8000760e

## `DATA`

Offset `0x018` · access `r` · 32 bits

The row read.

Sources:

- decompile (high): getconfig reads the value from +0x18

## `KEY`

Offset `0x01C` · access `rw` · 32 bits

OTP row number to read.

Sources:

- decompile (high): getconfig writes the key to +0x1C
- trace (high): RVF_DBG_OTP: the keys are row numbers (19..26 for the board-identity check, 28 serial, 30 revision)
