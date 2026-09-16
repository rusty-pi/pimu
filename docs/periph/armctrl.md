<!-- generated from specs/armctrl.toml by `cargo run -- spec-docs --update` – do not edit -->

# `armctrl` – ARM control block as the VPU sees it, below the mailboxes: where `arm_loader` releases the ARM

- Bus: `vpu` (VPU bus address)
- Base: `0x7E00B000`
- Size: `0x880`

The whole pinned boot touches this block a handful of times. Only the release bit has a known meaning; everything else is stored and read back.

Sources:

- trace (high): `RVF_TRACE_MMIO=7e00b000-7e101000 recon`: the writes right after `arm_loader: Starting ARM`, all from start4's MMIO write helper `0xFEC0043A`
- inferred (high): size: up to the mailboxes at `0x7E00B880`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CONTROL`](#control) | rw | 32 | 2, best high |
| `0x008` | [`REG_008`](#reg_008) | rw | 32 | 1, best high |
| `0x41C` | [`REG_41C`](#reg_41c) | rw | 32 | 2, best high |
| `0x440` | [`REG_440`](#reg_440) | rw | 32 | 1, best high |

## `CONTROL`

Offset `0x000` · access `rw` · 32 bits

Written `0x200` shortly before the ARM starts, and `0x1000` as the last access before it does, which clears `0x200` again.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 12 | `RELEASE` | rw | Let the ARM out of reset. The ARM starts at PC 0 in EL3. |

Sources:

- trace (high): `0x7E00B000` <- `0x00000200` (early), <- `0x00001000` (after `arm_loader`)
- trace (high): `--trace-mmio` through the ARM release: the `0x200` write comes after the UART handover and the PLLB bring-up, some 1300 accesses before the release — _the `recon` above calls the `0x200` write early; a trace that runs past the release puts it close to the release_

`RELEASE` sources:

- inferred (medium): it is the last write before the ARM runs, and nothing but PM housekeeping follows

## `REG_008`

Offset `0x008` · access `rw` · 32 bits

Written `0x3030` at the ARM release. Meaning unknown.

Sources:

- trace (high): `0x7E00B008` <- `0x00003030` after `arm_loader`

## `REG_41C`

Offset `0x41C` · access `rw` · 32 bits

Written `0x1F3` shortly before the release and `0xA` at it. Meaning unknown.

Sources:

- trace (high): `0x7E00B41C` <- `0x0000000A` after `arm_loader`
- trace (high): `0x7E00B41C` <- `0x000001F3` at `0x3EC53594`, before the release

## `REG_440`

Offset `0x440` · access `rw` · 32 bits

Written 0 just after the ARM release. Meaning unknown.

Sources:

- trace (high): `0x7E00B440` <- `0x00000000` at `0x3EC81F8A`, after the release
