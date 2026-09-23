<!-- generated from specs/pmic_rails.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pmic_rails` – Board PMIC owning the SDRAM and I/O rails (start4 descriptor type `0x83`)

- Bus: `i2c` (7-bit I²C address)
- Base: `0x1B`
- Size: `0x100`
- Carried by: [`bsc`](bsc.md), `PMIC` copy

On the `bsc` PMIC copy. The board revision, not a probe, selects the driver: there is no part-id register to get right. start4's init sweeps `0x00..0x14` once to log it. Registers not listed read 0.

Sources:

- decompile (high): `pmic_init` `0x3ED4DAA8` / `pmic_add` `0x3ED4D85C`; descriptor `0x3EDE962C`: type `0x83`, addr `0x1B`, 0.8..1.4 V

Carried by [`bsc`](bsc.md), `PMIC` copy:

start4's I²C driver picks its base from the bus id, and this part sits on bus 8.

- decompile (high): `FUN_0ecf0ed0`: bus id 8 -> `0x7E205E00`; `pmic_init` `0x3ED4DAA8` opens that bus before it probes

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`STATUS`](#status) | r | 8 | 2, best high |
| `0x005` | [`REG_05`](#reg_05) | rw | 8 | 3, best high |
| `0x009` | [`SETPOINT_SDRAM`](#setpoint_sdram) | rw | 8 | 2, best high |
| `0x00A` | [`SETPOINT_CORE`](#setpoint_core) | rw | 8 | 1, best high |
| `0x012` | [`SETPOINT_RAIL6`](#setpoint_rail6) | rw | 8 | 1, best high |
| `0x013` | [`SETPOINT_RAIL5`](#setpoint_rail5) | rw | 8 | 1, best high |

## `STATUS`

Offset `0x000` · access `r` · 8 bits

Status. start4 also reads it every 100 ms once the ARM runs. Its status callback takes bits 5 and 6 as a fault: when either is set it writes the value it read back, and the callback reports trouble with the supply, as it does when the read fails. The model's `0x10` has neither.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 4 | `SETTLED` | r | Voltage change complete; polled after every setpoint write. |

Sources:

- decompile (high): settle callback `0x3EC8C746`
- decompile (high): status callback `0x3EC8C5C8` (descriptor `0x3EDE962C` `+0x24`): read `0x00`; `(value & 0x60) == 0` returns 0, else a 1-byte write of the value to `0x00` and 1

`SETTLED` sources:

- decompile (high): `0x3EC8C746` polls reg `0x00` bit 4

## `REG_05`

Offset `0x005` · access `rw` · 8 bits

start4 reads it once more after its init sweep and sets bit 0 of what it read, and writes 1 to register 4. The read is not spare: a running d03115 holds `0x1E` here, and writing a bare `1` clears the four set bits and stops the board dead. The bootloader's power-off (`POWER_OFF_ON_HALT=1` with `WAKE_ON_GPIO=0`, after Linux powers the board off) writes `0x3F` here, after its ten LED blinks, and then sleeps for good: this is what switches a 0x1B board off. So the register gates the rails, one bit each; which bit is which is not known.

Sources:

- decompile (high): bootloader power-off op `0x80009ED2`: with board feature bit 2 (`[gp+776]`), `session(0x1B)` then write `0x05 = 0x3F`, then `sleep` in a loop
- measured (high): 4B rev 1.5 over I2C with the register-read path fixed: reg 0x05 reads 0x1E on a running board. `rpi-unboxed` wrote a bare 1 here and the machine stopped with no watchdog rescue; read-modify-write of `0x1E | 1` boots and runs (6/6 tryboots) — _the decompile reads as a store of 1; on silicon it has to be the read OR 1, or the other rails go off_
- trace (high): `boot --bootconf POWER_OFF_ON_HALT=1 --bootconf WAKE_ON_GPIO=0 --send-after '/ # ' 'poweroff -f\n' --log pmic` on `d03115`: after `Halt: wake: 0 power_off: 1`, `1b W 05 = 3f` at 4.08 s and nothing after

## `SETPOINT_SDRAM`

Offset `0x009` · access `rw` · 8 bits · reset `0x28`

Setpoint of rails 2..4 (`sdram_c` / `sdram_p` / `sdram_i`): `raw * 5 mV + 0.9 V`.

Sources:

- decompile (high): rail table image `0x3EE135AC`; decode callback `0x3EC8C710`
- measured (high): `vcgencmd measure_volts sdram_c|sdram_i|sdram_p` on a d03115: 1.1000 V each, i.e. raw 40

## `SETPOINT_CORE`

Offset `0x00A` · access `rw` · 8 bits

Rail 1 on this part; `pmic_init` hands rail 1 to the `0x1E` part instead, so nothing uses it.

Sources:

- decompile (high): rail table image `0x3EE135AC`; `pmic_init` does `mask[0] &= ~mask[1]`

## `SETPOINT_RAIL6`

Offset `0x012` · access `rw` · 8 bits

Rail 6 setpoint: `raw * 20 mV + 10 mV`.

Sources:

- decompile (high): rail table image `0x3EE135AC`; decode callback `0x3EC8C710`

## `SETPOINT_RAIL5`

Offset `0x013` · access `rw` · 8 bits

Rail 5 setpoint: `raw * 20 mV + 10 mV`.

Sources:

- decompile (high): rail table image `0x3EE135AC`; decode callback `0x3EC8C710`
