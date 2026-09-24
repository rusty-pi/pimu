<!-- generated from specs/avs.toml by `cargo run -- spec-docs --update` – do not edit -->

# `avs` – AVS monitor: on-die temperature sensor and the ring-oscillator / rail voltage monitors

- Bus: `vpu` (VPU bus address)
- Base: `0x7D5D2000`
- Size: `0xF00`
- Carried by: [`clkmon`](clkmon.md)

Carved out of the `clkmon` window and decoded ahead of it. The per-channel counts are measured and live in `src/periph/avs.rs`; channel 3 follows the core-rail PMIC setpoint so start4's DVFS calibration converges. That calibration runs once per power-up, right after start4's PMIC log sweep: it walks the core rail until channel 3 reads 1.030 V (10300 tenths of a mV, within 50), measures every ring oscillator and PVT monitor there and 32 rail codes lower, works out the voltage at which each would run at its target speed, and sets the rail to the highest of them, held to 8300..11000. It does that twice, then writes the target windows (`LOWER` / `UPPER` and the PVT thresholds) and makes one monitoring pass: out of range, move the rail back by 4 codes; else raise it by 4 if any monitor is below its window, or lower it by 2 if none is inside one. A rail code is about 1.76 mV: `uV = ((code * 0x119400) >> 16) * 100`.

Sources:

- linux (high): `avs-monitor@7d5d2000`, `brcm,bcm2711-avs-monitor`, `reg = <0x7d5d2000 0xf00>`
- decompile (high): start4 DVFS code: `FUN_0ed603e2`, `FUN_0ed6040e`, `FUN_0ec3007a`, `FUN_0ec303e8`, `FUN_0ec302c2`

Carried by [`clkmon`](clkmon.md):

Carved out of the clock block's window and decoded ahead of it.

- linux (high): `avs-monitor@7d5d2000` sits inside `clkmon`'s `reg = <0x7d5d0000 0x10000>`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x03C` | [`DISABLE_MASK`](#disable_mask) | rw | 32 | 2, best high |
| `0x040` | [`REG_040`](#reg_040) | rw | 32 | 2, best high |
| `0x044` | [`REG_044`](#reg_044) | rw | 32 | 2, best high |
| `0x06C` | [`REG_06C`](#reg_06c) | rw | 32 | 3, best high |
| `0x074` | [`REG_074`](#reg_074) | rw | 32 | 3, best high |
| `0x078` | [`REG_078`](#reg_078) | rw | 32 | 3, best high |
| `0x200`–`0x214` (6 × 0x4) | [`RESULT`](#result) | r | 32 | 4, best high |
| `0x220`–`0x2AC` (36 × 0x4) | [`RAIL`](#rail) | r | 32 | 2, best high |
| `0xD00`–`0xD5C` (24 × 0x4) | [`LOWER`](#lower) | rw | 32 | 4, best high |
| `0xE00`–`0xE5C` (24 × 0x4) | [`UPPER`](#upper) | rw | 32 | 1, best high |

## `DISABLE_MASK`

Offset `0x03C` · access `rw` · 32 bits

Channel disable mask: a set bit masks a channel off. Written `~(1 << ch) & 0x7F` around a reading, 0 at rest.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 6:0 | `CHANNELS` | rw | One bit per channel. |

Sources:

- decompile (high): `FUN_0ed6040e` writes `~(1 << ch) & 0x7F`, then 0
- measured (high): reads 0 on the idle reference board while all six channels report valid samples, so a clear bit cannot mean 'not selected'

`CHANNELS` sources:

- decompile (high): `FUN_0ed6040e` masks with `0x7F`

## `REG_040`

Offset `0x040` · access `rw` · 32 bits

Enable mask written all-ones during a measurement. Reads 0 at rest on hardware, while channels read fine, so it is not a read enable.

Sources:

- decompile (medium): `FUN_0ed6040e`
- measured (high): reads 0 on the idle reference board

## `REG_044`

Offset `0x044` · access `rw` · 32 bits

Second enable mask, same behaviour as `REG_040`.

Sources:

- decompile (medium): `FUN_0ed6040e`
- measured (high): reads 0 on the idle reference board

## `REG_06C`

Offset `0x06C` · access `rw` · 32 bits

Written, never read back. start4 pulses it, all-ones then 0, each time it arms the monitors.

Sources:

- trace (high): `PIMU_TRACE_MMIO=0x7d5d0000-0x7d5e0000` over a boot to `arm_loader`
- trace (high): start4: `0xFFFFFFFF` at `0x3EC30224`, 0 at `0x3EC30226`
- measured (high): reads 0 on a Raspberry Pi 4B d03115

## `REG_074`

Offset `0x074` · access `rw` · 32 bits

start4 sets its low seven bits (read, OR `0x7F`, write) each time it arms the monitors.

Sources:

- trace (high): `PIMU_TRACE_MMIO=0x7d5d0000-0x7d5e0000` over a boot to `arm_loader`
- decompile (high): `FUN_0ec3020e`: `_DAT_7d5d2074 |= 0x7f`
- measured (high): `0x0000007f` on a Raspberry Pi 4B d03115

## `REG_078`

Offset `0x078` · access `rw` · 32 bits

Written 1 each time start4 arms the monitors, never read back.

Sources:

- trace (high): `PIMU_TRACE_MMIO=0x7d5d0000-0x7d5e0000` over a boot to `arm_loader`
- decompile (high): `FUN_0ec3020e`
- measured (high): `0x00000001` on a Raspberry Pi 4B d03115

## `RESULT`

Offset `0x200`, 6 elements 0x4 apart · access `r` · 32 bits

Per-channel sample. Channel 0 is the temperature sensor (`410040 - 487 * count` mdegC), 2 / 3 / 5 are rail voltages, 1 and 4 read ~0 on real silicon. start4 converts a channel as `(count * scale) >> shift`: channel 0 scale -498739, shift 10, plus 410040 (mdegC); channels 1..4 scale 100571 and channel 5 scale 176000, shift 13 (tenths of a mV). Channel 3 is the SoC core rail its voltage calibration reads, as the average of up to 150 samples with every other channel masked. The thermal driver instead waits for `VALID` on channel 0 and reads the word once more.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 9:0 | `COUNT` | r | Sample. |
| 10 | `VALID` | r | Sample valid. |
| 16 | `SETTLED` | r | Sample settled. Linux ignores it; start4 will not take a sample without it. |

Sources:

- linux (high): `bcm2711_thermal` reads `+0x200`: `temp_mC = 410040 - 487 * count`
- decompile (high): `FUN_0ed603e2` indexes the `DAT_0edfbe9c` table of these six words; `FUN_0ecc6488` converts with the scales at `DAT_0edfbe84`
- trace (high): start4 `0x3ED7C92C` / `0x3ED7C932`: the thermal driver's two reads of channel 0, the first right after `TSENSCTL` is started
- measured (high): `/dev/mem` reads of `0x7d5d2200..0x7d5d2214` on a Raspberry Pi 4B d03115, freshly booted and idle

`COUNT` sources:

- linux (high): `bcm2711_thermal`: `BCM2711_TS_DATA_MASK`

`VALID` sources:

- linux (high): `bcm2711_thermal`: `BCM2711_TS_VALID_MASK`

`SETTLED` sources:

- decompile (high): `FUN_0ed603e2` spins until bits 10 and 16 are both set

## `RAIL`

Offset `0x220`, 36 elements 0x4 apart · access `r` · 32 bits

Per-channel ring-oscillator monitors. Channels `0x20..0x23` settle with a count of 0 on real silicon. start4 reads all 36 once before its voltage calibration, which then uses channels 0..21 (22 and 23 are masked, `0xC00000` in `FUN_0ec3503e`); `speed = ((count * 54000) / 0x7FFF) * 10`.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 14:0 | `COUNT` | r | Oscillator count. |
| 16 | `SETTLED` | r | Count settled. |

Sources:

- decompile (high): `FUN_0ec3007a` polls bit 16 and takes bits 14:0; `FUN_0ec5f2c0` range-checks its channel against `0x23`
- measured (high): `/dev/mem` reads one word at a time on a Raspberry Pi 4B d03115

`COUNT` sources:

- decompile (high): `FUN_0ec3007a` takes bits 14:0

`SETTLED` sources:

- decompile (high): `FUN_0ec3007a` polls bit 16

## `LOWER`

Offset `0xD00`, 24 elements 0x4 apart · access `rw` · 32 bits

Per-channel lower bounds `FUN_0ec302c2` writes and `FUN_0ec311d2` compares against. start4's own output, not calibration data: `count(target)` with `count(s) = (s / 54) * 0x7FFF / 10000` and the channel's target speed. start4's parameter block (all offsets 0) makes the bounds independent of anything it measured, which is why a model with made-up counts writes the same 44 words as hardware. Channels 22 and 23 are never written. start4 also sets slot `0x34` (`+0xDD0`) to all-ones each time it arms the monitors.

Sources:

- decompile (high): `FUN_0ec302c2` writes them, `FUN_0ec300a4` reads them back
- measured (high): the model's computed bounds match the 44 words read off the reference board, starting `0x683` / `0x6A1` for channel 0
- decompile (high): `FUN_0ec30828` through `FUN_0ec313c0`, targets `DAT_0ede6ae0`; `FUN_0ec301f0` writes `0xffffffff` to `0x7d5d2dd0` and `0x7d5d2ed0`
- measured (high): `0x7d5d2dd0` and `0x7d5d2ed0` read `0xffffffff` on a Raspberry Pi 4B d03115; channels 22 and 23 read 0

## `UPPER`

Offset `0xE00`, 24 elements 0x4 apart · access `rw` · 32 bits

Per-channel upper bounds, as `LOWER`: `count(target + slope / 100)`, the speed 10 mV above the target, with the per-channel slopes at `DAT_0ede6a80`. The monitoring pass lowers the core voltage only if every channel reads above its upper bound, and raises it if any reads below its lower one. Slot `0x34` (`+0xED0`) is set to all-ones with `LOWER`'s.

Sources:

- decompile (high): `FUN_0ec302c2` writes them, `FUN_0ec300a4` reads them back
