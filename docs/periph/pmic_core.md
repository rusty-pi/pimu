<!-- generated from specs/pmic_core.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pmic_core` – Board PMIC owning the SoC core rail (start4 descriptor type `0x82`)

- Bus: `i2c` (7-bit I²C address)
- Base: `0x1E`
- Size: `0x100`

On the `bsc` PMIC copy. The AVS monitor's channel 3 follows this part's setpoint. start4's init sweeps `0x01..0x15`, `0x20..0x27` and `0x48..0x4B` once to log it. Registers not listed read 0. start4's driver keeps the last voltage asked for and the last setpoint written, writes `SETPOINT_CORE` only when the setpoint changes, and then polls `STATUS` until `SETTLED`.

Sources:

- decompile (high): `pmic_add` `0x3ED4D85C`; descriptor `0x3EDE9658`: type `0x82`, addr `0x1E`, 0.3..1.9 V

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x002` | [`STATUS`](#status) | r | 8 | 1, best high |
| `0x024` | [`MODE`](#mode) | rw | 8 | 3, best high |
| `0x025` | [`SETPOINT_CORE`](#setpoint_core) | rw | 8 | 4, best high |

## `STATUS`

Offset `0x002` · access `r` · 8 bits

Status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3 | `SETTLED` | r | Voltage change complete. |

Sources:

- decompile (high): settle callback `0x3EC8C9FC`

`SETTLED` sources:

- decompile (high): `0x3EC8C9FC` polls reg `0x02` bit 3

## `MODE`

Offset `0x024` · access `rw` · 8 bits

Written 5 while the ARM is set to run faster than start4's PMIC turbo threshold (600 MHz), 15 otherwise, and only when that changes. On a Pi 4 at 1800 MHz start4 writes 5 once, right before it raises `SETPOINT_CORE` for the ARM clock. What the part does with it is not known.

Sources:

- decompile (high): descriptor `0x3EDE9658` callback `+0x1C` (`0x3EC8C97C`): `r3 = arg ? 5 : 15`, written to reg 36 if it differs from the byte cached at `gp+6916`; called from the DVFS clock change `FUN_0ed71e8c` when clock 3's new rate exceeds `[gp+0xccd84]` MHz
- trace (high): `0x1E` W `0x24 = 0x05`, then W `0x25 = 0x68` and a `STATUS` read, before PLLB goes to 3600 MHz
- trace (high): the pinned firmware on `--board-rev d03115` without `arm_boost`: `0x1E` W `0x24 = 0x05` and no `0x25` write before PLLB goes to 3000 MHz

## `SETPOINT_CORE`

Offset `0x025` · access `rw` · 8 bits · reset `0x55`

Core-rail setpoint, 10 mV per step. start4 encodes a voltage as `ceil(uV / 10000)`, after holding it to 0.835..1.1 V on a Pi 4. The boot writes `0x58` (880 mV) right after the log sweep, then its AVS calibration moves it (`0x67`, `0x62`, `0x67`, `0x62` in the model, ending at `0x61`), and before the ARM clock goes to 1800 MHz it writes 880 mV + the calibration's gain + 3.3 mV per percent above 1500 MHz: `0x68` in the model. For a 1500 MHz ARM that sum is the setpoint already written, so nothing is written; `MODE` still gets 5.

Sources:

- decompile (high): rail table image `0x3EE1360C`; decode callback `0x3EC8C9F6`: `raw * 10000 uV`
- decompile (high): encode callback `0x3ED20B96`: `(uV + 9999) / 10000`; `pmic_set_voltage` `0x3ED4DC3C` skips the write when the uV or the encoded byte is unchanged; the rail-1 wrapper `0x3ED56116` clamps to `FUN_0ed49ab0() * 1000` (835 mV) .. `FUN_0ed49946() * 1000` (1100 mV)
- decompile (high): `FUN_0ed49b80` for clock 3: `FUN_0ed493a6(over_voltage) + [gp+0xccd30] + FUN_0ed4ac14`, the last `(rate / 1500 MHz - 1) * 330000 uV` with `dvfs` 3 or 4; `[gp+0xccd30]` is the setpoint read back after the calibration, minus 880000, plus 10000 per bit set in OTP row 44
- inferred (low): reset 85 (0.85 V): no power-on ground truth; the reference board's 0.926 V is a live DVFS point, not a multiple of the step. 85 is inside the descriptor range and the band the boot writes
