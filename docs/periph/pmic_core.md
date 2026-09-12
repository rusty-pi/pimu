<!-- generated from specs/pmic_core.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pmic_core` – Board PMIC owning the SoC core rail (start4 descriptor type 0x82)

- Bus: `i2c` (7-bit I²C address)
- Base: `0x1E`
- Size: `0x100`

On the `bsc` PMIC copy. The AVS monitor's channel 3 follows this part's setpoint. start4's init sweeps 0x01..0x4B once to log it. Registers not listed read 0.

Sources:

- decompile (high): pmic_add 0x3ED4D85C; descriptor 0x3EDE9658: type 0x82, addr 0x1E, 0.3..1.9 V

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x002` | [`STATUS`](#status) | r | 8 | 1, best high |
| `0x025` | [`SETPOINT_CORE`](#setpoint_core) | rw | 8 | 2, best high |

## `STATUS`

Offset `0x002` · access `r` · 8 bits

Status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3 | `SETTLED` | r | Voltage change complete. |

Sources:

- decompile (high): settle callback 0x3EC8C9FC

`SETTLED` sources:

- decompile (high): 0x3EC8C9FC polls reg 0x02 bit 3

## `SETPOINT_CORE`

Offset `0x025` · access `rw` · 8 bits · reset `0x55`

Core-rail setpoint, 10 mV per step. The boot rewrites it across 0x54..0x6E as it moves the ARM clock.

Sources:

- decompile (high): rail table image 0x3EE1360C; decode callback 0x3EC8C9F6: raw * 10000 uV
- inferred (low): reset 85 (0.85 V): no power-on ground truth; the reference board's 0.926 V is a live DVFS point, not a multiple of the step. 85 is inside the descriptor range and the band the boot writes
