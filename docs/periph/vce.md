<!-- generated from specs/vce.toml by `cargo run -- spec-docs --update` – do not edit -->

# `vce` – VCE vector/codec engine: data memory, program memory and register file

- Bus: `vpu` (VPU bus address)
- Base: `0x7F100000`
- Size: `0x21000`

The control block sits apart at `0x7F140000` (`vce_ctrl`); `0x7F130000` is not claimed. The engine's own ISA is not emulated, so a launch leaves the register file zeroed, which is the honest answer for the codec-licence check on a board whose licence rows are blank.

Sources:

- decompile (high): start4's `vcfw/drivers/chip/vciv/2708/vce.c` (function names from `start4db.elf`'s assert strings)

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000`–`0xFFFC` (16384 × 0x4) | [`DATA`](#data) | rw | 32 | 1, best high |
| `0x10000`–`0x1FFFC` (16384 × 0x4) | [`PROG`](#prog) | rw | 32 | 1, best high |
| `0x20000`–`0x20FFC` (1024 × 0x4) | [`REG`](#reg) | rw | 32 | 1, best high |

## `DATA`

Offset `0x000`, 16384 elements 0x4 apart · access `rw` · 32 bits

Data memory, 64 KiB: `vce_loaddata` copies the caller's blob in, `vce_launch_complete` copies results out.

Sources:

- decompile (high): `vce_loaddata` / `vce_launch_complete` use `0x7F100000`

## `PROG`

Offset `0x10000`, 16384 elements 0x4 apart · access `rw` · 32 bits

Program memory, 64 KiB; the driver asserts a program is under `0x4000` bytes.

Sources:

- decompile (high): `vce_loadprogram`'s destination `0x7F110000`

## `REG`

Offset `0x20000`, 1024 elements 0x4 apart · access `rw` · 32 bits

Register file. The highest register the firmware touches is 62, the run's result word.

Sources:

- decompile (high): `vce_getreg` / `vce_setreg`: `(n * 4 + 0x120000) | 0x7F000000`; `vce_run` reads `0x7F1200F8`
