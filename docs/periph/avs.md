<!-- generated from specs/avs.toml by `cargo run -- spec-docs --update` – do not edit -->

# `avs` – AVS monitor: on-die temperature sensor and the ring-oscillator / rail voltage monitors

- Bus: `vpu` (VPU bus address)
- Base: `0x7D5D2000`
- Size: `0xF00`

Carved out of the `clkmon` window and decoded ahead of it. The per-channel counts are measured and live in src/periph/avs.rs; channel 3 follows the core-rail PMIC setpoint so start4's DVFS calibration converges.

Sources:

- linux (high): avs-monitor@7d5d2000, brcm,bcm2711-avs-monitor, reg = <0x7d5d2000 0xf00>
- decompile (high): start4 DVFS code: FUN_0ed603e2, FUN_0ed6040e, FUN_0ec3007a, FUN_0ec303e8, FUN_0ec302c2

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x03C` | [`DISABLE_MASK`](#disable_mask) | rw | 32 | 2, best high |
| `0x040` | [`REG_040`](#reg_040) | rw | 32 | 2, best high |
| `0x044` | [`REG_044`](#reg_044) | rw | 32 | 2, best high |
| `0x06C` | [`REG_06C`](#reg_06c) | rw | 32 | 1, best high |
| `0x074` | [`REG_074`](#reg_074) | rw | 32 | 1, best high |
| `0x078` | [`REG_078`](#reg_078) | rw | 32 | 1, best high |
| `0x200`–`0x214` (6 × 0x4) | [`RESULT`](#result) | r | 32 | 3, best high |
| `0x220`–`0x2AC` (36 × 0x4) | [`RAIL`](#rail) | r | 32 | 2, best high |
| `0xD00`–`0xD5C` (24 × 0x4) | [`LOWER`](#lower) | rw | 32 | 2, best high |
| `0xE00`–`0xE5C` (24 × 0x4) | [`UPPER`](#upper) | rw | 32 | 1, best high |

## `DISABLE_MASK`

Offset `0x03C` · access `rw` · 32 bits

Channel disable mask: a set bit masks a channel off. Written ~(1 << ch) & 0x7F around a reading, 0 at rest.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 6:0 | `CHANNELS` | rw | One bit per channel. |

Sources:

- decompile (high): FUN_0ed6040e writes ~(1 << ch) & 0x7F, then 0
- measured (high): reads 0 on the idle reference board while all six channels report valid samples, so a clear bit cannot mean 'not selected'

`CHANNELS` sources:

- decompile (high): FUN_0ed6040e masks with 0x7F

## `REG_040`

Offset `0x040` · access `rw` · 32 bits

Enable mask written all-ones during a measurement. Reads 0 at rest on hardware, while channels read fine, so it is not a read enable.

Sources:

- decompile (medium): FUN_0ed6040e
- measured (high): reads 0 on the idle reference board

## `REG_044`

Offset `0x044` · access `rw` · 32 bits

Second enable mask, same behaviour as REG_040.

Sources:

- decompile (medium): FUN_0ed6040e
- measured (high): reads 0 on the idle reference board

## `REG_06C`

Offset `0x06C` · access `rw` · 32 bits

Written, never read back.

Sources:

- trace (high): RVF_TRACE_MMIO=0x7d5d0000-0x7d5e0000 over a boot to arm_loader

## `REG_074`

Offset `0x074` · access `rw` · 32 bits

Written, never read back.

Sources:

- trace (high): RVF_TRACE_MMIO=0x7d5d0000-0x7d5e0000 over a boot to arm_loader

## `REG_078`

Offset `0x078` · access `rw` · 32 bits

Written, never read back.

Sources:

- trace (high): RVF_TRACE_MMIO=0x7d5d0000-0x7d5e0000 over a boot to arm_loader

## `RESULT`

Offset `0x200`, 6 elements 0x4 apart · access `r` · 32 bits

Per-channel sample. Channel 0 is the temperature sensor (410040 - 487 * count mdegC), 2 / 3 / 5 are rail voltages, 1 and 4 read ~0 on real silicon.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 9:0 | `COUNT` | r | Sample. |
| 10 | `VALID` | r | Sample valid. |
| 16 | `SETTLED` | r | Sample settled. Linux ignores it; start4 will not take a sample without it. |

Sources:

- linux (high): bcm2711_thermal reads +0x200: temp_mC = 410040 - 487 * count
- decompile (high): FUN_0ed603e2 indexes the DAT_0edfbe9c table of these six words; FUN_0ecc6488 converts
- measured (high): /dev/mem reads of 0x7d5d2200..0x7d5d2214 on a Raspberry Pi 4B d03115, freshly booted and idle

`COUNT` sources:

- linux (high): bcm2711_thermal: BCM2711_TS_DATA_MASK

`VALID` sources:

- linux (high): bcm2711_thermal: BCM2711_TS_VALID_MASK

`SETTLED` sources:

- decompile (high): FUN_0ed603e2 spins until bits 10 and 16 are both set

## `RAIL`

Offset `0x220`, 36 elements 0x4 apart · access `r` · 32 bits

Per-channel ring-oscillator monitors. Channels 0x20..0x23 settle with a count of 0 on real silicon.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 14:0 | `COUNT` | r | Oscillator count. |
| 16 | `SETTLED` | r | Count settled. |

Sources:

- decompile (high): FUN_0ec3007a polls bit 16 and takes bits 14:0; FUN_0ec5f2c0 range-checks its channel against 0x23
- measured (high): /dev/mem reads one word at a time on a Raspberry Pi 4B d03115

`COUNT` sources:

- decompile (high): FUN_0ec3007a takes bits 14:0

`SETTLED` sources:

- decompile (high): FUN_0ec3007a polls bit 16

## `LOWER`

Offset `0xD00`, 24 elements 0x4 apart · access `rw` · 32 bits

Per-channel lower bounds FUN_0ec302c2 computes and FUN_0ec303e8 compares against. start4's own output, not calibration data.

Sources:

- decompile (high): FUN_0ec302c2 writes them, FUN_0ec300a4 reads them back
- measured (high): the model's computed bounds match the 44 words read off the reference board, starting 0x683 / 0x6A1 for channel 0

## `UPPER`

Offset `0xE00`, 24 elements 0x4 apart · access `rw` · 32 bits

Per-channel upper bounds, as LOWER.

Sources:

- decompile (high): FUN_0ec302c2 writes them, FUN_0ec300a4 reads them back
