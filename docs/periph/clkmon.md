<!-- generated from specs/clkmon.toml by `cargo run -- spec-docs --update` – do not edit -->

# `clkmon` – VPU clock block (PLLs and frequency monitors) below the `0x7E` window

- Bus: `vpu` (VPU bus address)
- Base: `0x7D5D0000`
- Size: `0x10000`

The AVS monitor (`avs`, `+0x2000`) and the PVT monitors (`pvt`, `+0x8000`) sit inside this window and are decoded ahead of it. A full traced boot touches no PLL divider or lock register here; the registers below are the complete list outside the two sub-blocks (`REG_1818` only matters on a die whose `REG_1820` bit 10 is set), and all are plain storage with the password byte masked.

Sources:

- decompile (high): start4's clock manager uses this block when the SoC-type switch at `0x3EC635F0` sets `[gp+5476] = 1` (BCM2711)
- trace (high): `RVF_TRACE_MMIO=0x7d5d0000-0x7d5e0000` over a boot to `arm_loader`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x1800` | [`REG_1800`](#reg_1800) | rw | 32 | 2, best high |
| `0x1814` | [`REG_1814`](#reg_1814) | rw | 32 | 2, best high |
| `0x1818` | [`REG_1818`](#reg_1818) | rw | 32 | 2, best high |
| `0x1820` | [`REG_1820`](#reg_1820) | rw | 32 | 2, best high |
| `0x183C` | [`CHAR_DONE`](#char_done) | rw | 32 | 4, best high |
| `0xA000` | [`REG_A000`](#reg_a000) | rw | 32 | 3, best high |

## `REG_1800`

Offset `0x1800` · access `rw` · 32 bits

`FUN_0ec2ffb4` sets bit 2 when the core rail (AVS channel 3) reads below 0.94 V (9400 tenths of a mV), and clears it otherwise. start4 updates it after its voltage calibration and again after its monitoring pass.

Sources:

- decompile (high): `FUN_0ec2ffb4`
- measured (high): `0x00000004` on a Raspberry Pi 4B d03115 (post-start4 state)

## `REG_1814`

Offset `0x1814` · access `rw` · 32 bits

`FUN_0ec2ffdc` tests `(reg & 0x14) != 0` as the condition of a voltage-ramp loop: while it holds, start4 raises the core voltage by 16 codes and measures again. It only runs when `REG_1820` bit 10 is set, and writes `REG_1818` to 0 (then waits 5 ms) before each test. Hardware reads `0x16`; the model keeps 0 ('settled'), because nothing in that loop writes the register and a constant `true` would never exit.

Sources:

- decompile (high): `FUN_0ec2ffdc`, loop in `FUN_0ec31bb2` behind a `FUN_0ec30196() != 0` guard
- measured (high): `0x00000016` on a Raspberry Pi 4B d03115 — _deliberately not reproduced; see notes_

## `REG_1818`

Offset `0x1818` · access `rw` · 32 bits

start4 writes 0 here, then waits 5 ms, before each test of `REG_1814` (`FUN_0ec30010`). Not reached on a board whose `REG_1820` bit 10 is clear.

Sources:

- decompile (high): `FUN_0ec30010`: `_DAT_7d5d1818 = 0`; sleep 5000 us
- measured (high): `0x00000000` on a Raspberry Pi 4B d03115

## `REG_1820`

Offset `0x1820` · access `rw` · 32 bits

`FUN_0ec30196` takes bit 10 as a predicate and `FUN_0ec301a6` decodes bits 23:11. Reads 0 on hardware too: not a blanked status register. With bit 10 set, start4 keeps the core voltage at or above a floor: bits 20:17 and 16:14 index two millivolt tables (`DAT_0ee00510`, 16 entries from 808; `DAT_0ee00550`, 8 entries from 808), and the floor is the larger entry times 10 plus 500, in tenths of a mV. It also runs the `REG_1814` loop.

Sources:

- decompile (high): `FUN_0ec30196` / `FUN_0ec301a6`
- measured (high): `0x00000000` on a Raspberry Pi 4B d03115

## `CHAR_DONE`

Offset `0x183C` · access `rw` · 32 bits

start4's own 'characterisation done' flag. start4 reads it before its core-voltage calibration and clears it; if it was already set, the calibration is skipped and the monitor windows from the earlier run are used as they are. It is set to 1 once a calibration succeeds.

Sources:

- decompile (high): `FUN_0ec302e2` writes it
- decompile (high): `FUN_0ec2fe38`: `FUN_0ec300c4` reads it, `FUN_0ec302e2(0)` clears it, `FUN_0ec309ec` only runs when it read 0, `FUN_0ec302e2(1)` on success
- trace (high): start4: read at `0x3EC300CA`, 0 written at `0x3EC302E8` and later 1 at the same PC
- measured (high): `0x00000001` on a Raspberry Pi 4B d03115

## `REG_A000`

Offset `0xA000` · access `rw` · 32 bits

Written `0xA0`, never read back, as start4's core-voltage calibration starts.

Sources:

- decompile (high): `FUN_0ec30200`
- trace (high): start4 `0x3EC3020A`
- measured (high): `0x000000a0` on a Raspberry Pi 4B d03115
