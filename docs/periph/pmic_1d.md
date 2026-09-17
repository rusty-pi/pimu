<!-- generated from specs/pmic_1d.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pmic_1d` – Board PMIC at `0x1D` on every Pi 4-family board but the 4B rev 1.5 (start4 descriptor type `0x81`)

- Bus: `i2c` (7-bit I²C address)
- Base: `0x1D`
- Size: `0x100`

On the `bsc` PMIC copy (#78). Alone on a 4B rev 1.1 or 1.2, where it owns every rail; next to `pmic_core` on a 4B rev 1.4, a Pi 400 or a CM4, where `pmic_core` takes the core rail. start4's probe sweeps `0x00..0x1B` once, skipping `0x0C..0x0F`, to log it; it then writes a 4-byte config to `0x03..0x06` (`0x04`, `0x23`, `0x32`, `0x43` on a 4B rev 1.2), writes `0xA5` to `0x14` on boards with `pmic_core`, `0x1E` to `0x18` on a CM4, and on a 4B sets `0x16` bit 0 and writes 1 to `0x01`. Registers not listed read 0 until written.

Sources:

- decompile (high): `pmic_init` `0x3ED4DAA8`: board flag 10 -> probe `0x3EDD20F2` -> `pmic_add` `0x3ED4D85C`; descriptor `0x3EE00570`: type `0x81`, addr `0x1D`, 0..1.39375 V
- trace (high): board-flags word `gp+0x3BAF0` (`--dump 0x3EE3E810:4`) per revision code: a03111/c03111/c03112 `0x05DE04D7` (flag 10), b03114/d03114 `0x055E0CF7`, c03130 `0x0D5E0CF7`, a03140/d03140 `0x05184CF7` (flags 10 + 11), b03115/d03115 `0x055E18F7` (flags 11 + 12)

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x00F` | [`ID`](#id) | r | 8 | 2, best high |
| `0x013` | [`SETPOINT_SDRAM`](#setpoint_sdram) | rw | 8 | 2, best high |
| `0x014` | [`SETPOINT_CORE`](#setpoint_core) | rw | 8 | 3, best high |
| `0x01A` | [`STATUS`](#status) | r | 8 | 2, best high |
| `0x016` | [`REG_16`](#reg_16) | rw | 8 | 2, best high |
| `0x019` | [`REG_19`](#reg_19) | rw | 8 | 2, best high |
| `0x01C` | [`SETPOINT_RAIL6`](#setpoint_rail6) | rw | 8 | 1, best high |
| `0x01D` | [`SETPOINT_RAIL5`](#setpoint_rail5) | rw | 8 | 1, best high |

## `ID`

Offset `0x00F` · access `r` · 8 bits · reset `0x8`

Read once by the probe, straight after four reads of `0x14`. start4 keeps it `^ 0x57`, and `pmic_get_voltage` then requires `0x0F == (0x14 as the probe read it) ^ 0xAD`, or stops with fatal error `0x46`.

Sources:

- decompile (high): probe `0x3EDD20F2` stores `0x14` at `gp+0xD361B` and `0x0F ^ 0x57` at `gp+0xD3640`; `pmic_get_voltage` `0x3ED4D960` requires `gp+0xD361B ^ 0xC9 == gp+0xD3640 ^ 0x33`
- inferred (medium): `0x08 = 0xA5 ^ 0xAD`: on boards with `pmic_core` the probe writes `0xA5` to `0x14` before reading it, so a fixed `0x0F` has to be `0x08` there

## `SETPOINT_SDRAM`

Offset `0x013` · access `rw` · 8 bits · reset `0xB0`

Setpoint of rails 2..4 (`sdram_c` / `sdram_p` / `sdram_i`): `raw * 6.25 mV`.

Sources:

- decompile (high): rail table image `0x3EE312D0`; decode callback `0x3EDD259A`
- inferred (low): 176 = 1.1 V, what the rev 1.5 board measures on the same rails; no board with this part to measure

## `SETPOINT_CORE`

Offset `0x014` · access `rw` · 8 bits · reset `0xA5`

Rail 1 (core) setpoint, 6.25 mV per step; the core rail only where `pmic_core` is not fitted. The probe reads it four times and keeps the last read for the check described under `ID`. On a 4B rev 1.2 start4 writes `0x8D` (880 mV) after the config words, then its AVS calibration moves it (`0xA5`, `0x9C`, `0xA5`, `0x9C`, ending at `0x9B` in the model), and it writes nothing more before the ARM starts at 1500 MHz.

Sources:

- decompile (high): rail table image `0x3EE312D0`; decode callback `0x3EDD259A`: `raw * 6250 uV`; encode `0x3ED20BB8`: `(uV + 6249) / 6250`
- inferred (medium): power-on `0xA5` (1.031 V): with `ID` fixed at `0x08`, the boards whose probe does not write `0x14` pass the check only from `0xA5`. It is also what the probe writes there on boards with `pmic_core` (`encode(1030000)`), and what the `0x1B` probe falls back to for the same check
- trace (high): the pinned firmware on `--board-rev c03112` under `--log pmic`: `1d W 03 = 04`, `04 = 23`, `05 = 32`, `06 = 43`, `16 = 01`, `01 = 01`, `14 = 8d`, then `14` = `a5`, `9c`, `a5`, `9c`, `9b`

## `STATUS`

Offset `0x01A` · access `r` · 8 bits

Status. start4 polls it every 100 ms once the ARM runs. Its status callback reports under-voltage unless `STATUS & mask == 0x20`, where the mask is a byte it sets to `0x60` at the end of each call, so a healthy part has to read bit 5 set and bit 6 clear. When bit 6 is set it writes `0x40` back. The model's reset value `0x10` has bit 5 clear. With it, the pinned firmware on `--board-rev b03112` answers `GET_THROTTLED` with `0x50005` (under-voltage now and throttled). On `b03112` and `b03114` it then slows the ARM and, on `b03114`, drops `pmic_core`'s setpoint and `MODE` after the release. It also writes the expander's `OUTPUT` to `0x44` after every poll. A reset value with bit 5 set would match a board with good input power; which value a real part reads is not measured.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 4 | `SETTLED` | r | Voltage change complete; polled after every setpoint write. |
| 5 | `POWER_OK` | r | Our name. start4's status poll takes the part's input power as good only while this reads 1 (with `LATCHED` clear). |
| 6 | `LATCHED` | r | Our name. start4's status poll writes `0x40` to the register when this bit is set, which reads as clearing a latched event. |

Sources:

- decompile (high): settle callback `0x3EDD25AE`
- trace (high): the pinned firmware with `--board-rev b03112` and `--mbox-property 0x00030046`: reply `0x00050005`; with `b03112` and `b03114` under `--log pmic,expander`: `1d R 1a -> 10` then `expander: W 05 = 44` every 100 ms after the ARM release

`SETTLED` sources:

- decompile (high): `0x3EDD25AE` polls reg `0x1A` bit 4

`POWER_OK` sources:

- decompile (medium): status callback `0x3EDD23F4` (descriptor `0x3EE00570` `+0x24`): `(reg & [gp+5188]) == 0x20` keeps the result 0, else it stays 1; `[gp+5188] = 0x60` before return

`LATCHED` sources:

- decompile (medium): status callback `0x3EDD23F4`: `btest r1, 6`, then a 1-byte write of `0x40` to `0x1A` through the bus ops' `+0x20` call

## `REG_16`

Offset `0x016` · access `rw` · 8 bits

start4 sets bit 0 as it configures the part on a 4B. The bootloader's power-off (`POWER_OFF_ON_HALT=1` with `WAKE_ON_GPIO=0`, after Linux powers the board off) writes 0 here, after its ten LED blinks, and then sleeps for good: this is what switches a 0x1D board off. What the other bits do is not known.

Sources:

- decompile (high): bootloader power-off op `0x80009ED2`: without board feature bit 2 (`[gp+776]`), `session(0x1D)` then write `0x16 = 0`, then `sleep` in a loop
- trace (high): `boot --bootconf POWER_OFF_ON_HALT=1 --bootconf WAKE_ON_GPIO=0 --send-after '/ # ' 'poweroff -f\n' --log pmic` with `--board-rev b03112` and `b03114`: after `Halt: wake: 0 power_off: 1`, `1d W 16 = 00` at 4.08 s and nothing after

## `REG_19`

Offset `0x019` · access `rw` · 8 bits

Both bootloader stages touch it on a board with `pmic_core` (their 2-bit board flag equal to 3; `d03114` in the model). They first write `0xA5` to `SETPOINT_CORE`, then read this register. They step its low nibble towards 9 one write at a time, each write `0xE0 | n`: from a reset value of 0 that is `0xE1`..`0xE9`. A failed transfer goes to their error handler. What the register controls is not known.

Sources:

- decompile (high): bootcode `0x800037A2` (in the bus ops table at `0x80011BF0`), bootloader `0x00094FC4`: `(flags & 3) == 3`, write `0x14 = 165`, read `0x19`, `n = value & 0xF`, then `while n != 9 { n += n < 9 ? 1 : -1; write 0x19 = n | 0xE0 }`
- trace (high): `--board-rev d03114` under `--log pmic`: bootcode `1d W 14 = a5`, `1d R 19 -> 00`, `1d W 19 = e1` .. `e9` (0.000793 to 0.000903 s); bootloader `1d W 14 = a5`, `1d R 19 -> e9` (2.1025 s); none on `d03115`, `c03112` or `b03112`

## `SETPOINT_RAIL6`

Offset `0x01C` · access `rw` · 8 bits

Rail 6 setpoint: `raw * 10 mV`.

Sources:

- decompile (high): rail table image `0x3EE312D0`; decode callback `0x3EDD259A`

## `SETPOINT_RAIL5`

Offset `0x01D` · access `rw` · 8 bits

Rail 5 setpoint: `raw * 10 mV`.

Sources:

- decompile (high): rail table image `0x3EE312D0`; decode callback `0x3EDD259A`
