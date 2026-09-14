<!-- generated from specs/pmic_1d.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pmic_1d` – Board PMIC at 0x1D on every Pi 4-family board but the 4B rev 1.5 (start4 descriptor type 0x81)

- Bus: `i2c` (7-bit I²C address)
- Base: `0x1D`
- Size: `0x100`

On the `bsc` PMIC copy (#78). Alone on a 4B rev 1.1 or 1.2, where it owns every rail; next to pmic_core on a 4B rev 1.4, a Pi 400 or a CM4, where pmic_core takes the core rail. start4's probe sweeps 0x00..0x1B once, skipping 0x0C..0x0F, to log it; it then writes a 4-byte config to 0x03..0x06, writes 0xA5 to 0x14 on boards with pmic_core, 0x1E to 0x18 on a CM4, and on a 4B sets 0x16 bit 0 and writes 1 to 0x01. Registers not listed read 0 until written.

Sources:

- decompile (high): pmic_init 0x3ED4DAA8: board flag 10 -> probe 0x3EDD20F2 -> pmic_add 0x3ED4D85C; descriptor 0x3EE00570: type 0x81, addr 0x1D, 0..1.39375 V
- trace (high): board-flags word gp+0x3BAF0 (--dump 0x3EE3E810:4) per revision code: a03111/c03111/c03112 0x05DE04D7 (flag 10), b03114/d03114 0x055E0CF7, c03130 0x0D5E0CF7, a03140/d03140 0x05184CF7 (flags 10 + 11), b03115/d03115 0x055E18F7 (flags 11 + 12)

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x00F` | [`ID`](#id) | r | 8 | 2, best high |
| `0x013` | [`SETPOINT_SDRAM`](#setpoint_sdram) | rw | 8 | 2, best high |
| `0x014` | [`SETPOINT_CORE`](#setpoint_core) | rw | 8 | 2, best high |
| `0x01A` | [`STATUS`](#status) | r | 8 | 1, best high |
| `0x01C` | [`SETPOINT_RAIL6`](#setpoint_rail6) | rw | 8 | 1, best high |
| `0x01D` | [`SETPOINT_RAIL5`](#setpoint_rail5) | rw | 8 | 1, best high |

## `ID`

Offset `0x00F` · access `r` · 8 bits · reset `0x8`

Read once by the probe, straight after four reads of 0x14. start4 keeps it ^ 0x57, and pmic_get_voltage then requires 0x0F == (0x14 as the probe read it) ^ 0xAD, or stops with fatal error 0x46.

Sources:

- decompile (high): probe 0x3EDD20F2 stores 0x14 at gp+0xD361B and 0x0F ^ 0x57 at gp+0xD3640; pmic_get_voltage 0x3ED4D960 requires gp+0xD361B ^ 0xC9 == gp+0xD3640 ^ 0x33
- inferred (medium): 0x08 = 0xA5 ^ 0xAD: on boards with pmic_core the probe writes 0xA5 to 0x14 before reading it, so a fixed 0x0F has to be 0x08 there

## `SETPOINT_SDRAM`

Offset `0x013` · access `rw` · 8 bits · reset `0xB0`

Setpoint of rails 2..4 (sdram_c / sdram_p / sdram_i): raw * 6.25 mV.

Sources:

- decompile (high): rail table image 0x3EE312D0; decode callback 0x3EDD259A
- inferred (low): 176 = 1.1 V, what the rev 1.5 board measures on the same rails; no board with this part to measure

## `SETPOINT_CORE`

Offset `0x014` · access `rw` · 8 bits · reset `0xA5`

Rail 1 (core) setpoint, 6.25 mV per step; the core rail only where pmic_core is not fitted. The probe reads it four times and keeps the last read for the check described under ID.

Sources:

- decompile (high): rail table image 0x3EE312D0; decode callback 0x3EDD259A: raw * 6250 uV; encode 0x3ED20BB8: (uV + 6249) / 6250
- inferred (medium): power-on 0xA5 (1.031 V): with ID fixed at 0x08, the boards whose probe does not write 0x14 pass the check only from 0xA5. It is also what the probe writes there on boards with pmic_core (encode(1030000)), and what the 0x1B probe falls back to for the same check

## `STATUS`

Offset `0x01A` · access `r` · 8 bits

Status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 4 | `SETTLED` | r | Voltage change complete; polled after every setpoint write. |

Sources:

- decompile (high): settle callback 0x3EDD25AE

`SETTLED` sources:

- decompile (high): 0x3EDD25AE polls reg 0x1A bit 4

## `SETPOINT_RAIL6`

Offset `0x01C` · access `rw` · 8 bits

Rail 6 setpoint: raw * 10 mV.

Sources:

- decompile (high): rail table image 0x3EE312D0; decode callback 0x3EDD259A

## `SETPOINT_RAIL5`

Offset `0x01D` · access `rw` · 8 bits

Rail 5 setpoint: raw * 10 mV.

Sources:

- decompile (high): rail table image 0x3EE312D0; decode callback 0x3EDD259A
