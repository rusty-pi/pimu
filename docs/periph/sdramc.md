<!-- generated from specs/sdramc.toml by `cargo run -- spec-docs --update` – do not edit -->

# `sdramc` – LPDDR4 controller and PHY, below the 0x7E window

- Bus: `vpu` (VPU bus address)
- Base: `0x7DC00000`
- Size: `0x40000`

DRAM is not simulated electrically: every training step reports success with the values the firmware's self-consistency checks expect. The PHY preset arrays copied in from memsysNN.bin read straight back. The per-byte-lane PHY blocks around +0x400..+0xC00 and +0x30000 are plain storage and not listed.

Sources:

- decompile (high): EEPROM bootcode init_sdram_* path, driven by the memsysNN.bin PHY presets from pieeprom.bin
- trace (high): --trace-mmio over the SDRAM bring-up

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x20010` | [`CTRL_STATUS`](#ctrl_status) | r | 32 | 1, best high |
| `0x20014` | [`CTRL_CMD`](#ctrl_cmd) | rw | 32 | 1, best high |
| `0x32010` | [`PHY_CAL_TRIGGER`](#phy_cal_trigger) | rw | 32 | 1, best high |
| `0x32014` | [`PHY_CAL_BUSY`](#phy_cal_busy) | r | 32 | 1, best high |
| `0x32100` | [`PHY_RES_VALID`](#phy_res_valid) | rw | 32 | 1, best high |
| `0x32104` | [`PHY_RES_CMD`](#phy_res_cmd) | rw | 32 | 1, best high |
| `0x32108` | [`PHY_RES_PARAM`](#phy_res_param) | rw | 32 | 1, best medium |
| `0x3210C` | [`PHY_RES_SIGNATURE`](#phy_res_signature) | rw | 32 | 2, best high |
| `0x32110` | [`PHY_RES_STATUS`](#phy_res_status) | rw | 32 | 1, best high |
| `0x32114` | [`PHY_RES_SUM5`](#phy_res_sum5) | rw | 32 | 1, best high |
| `0x32118` | [`PHY_RES_SUM6`](#phy_res_sum6) | rw | 32 | 1, best high |
| `0x34000`–`0x34FFC` (1024 × 0x4) | [`PHY_A`](#phy_a) | rw | 32 | 1, best medium |
| `0x38000`–`0x3BFFC` (4096 × 0x4) | [`PHY_B`](#phy_b) | rw | 32 | 2, best high |

## `CTRL_STATUS`

Offset `0x20010` · access `r` · 32 bits

Controller status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `DONE` | r | Last command complete. |

Sources:

- decompile (high): 0x80003898 polls btest [+0x10], #0

`DONE` sources:

- decompile (high): 0x80003898

## `CTRL_CMD`

Offset `0x20014` · access `rw` · 32 bits

Controller command port (0x55010000 style words); retires at once.

Sources:

- trace (high): the bootcode writes 0x5501_0000 and then polls CTRL_STATUS

## `PHY_CAL_TRIGGER`

Offset `0x32010` · access `rw` · 32 bits

PHY calibration handshake: write 1, wait for PHY_CAL_BUSY, write 0.

Sources:

- decompile (high): 0x8000685e

## `PHY_CAL_BUSY`

Offset `0x32014` · access `r` · 32 bits

Non-zero while a calibration runs; the firmware wants it stable 10 times.

Sources:

- decompile (high): 0x8000685e

## `PHY_RES_VALID`

Offset `0x32100` · access `rw` · 32 bits

Calibration request / result block, word 0: seeded 1 (valid).

Sources:

- decompile (high): the firmware seeds [+0x00] = 1 before pulsing the trigger

## `PHY_RES_CMD`

Offset `0x32104` · access `rw` · 32 bits

Calibration command; 0x101 asks the PHY to report its signature.

Sources:

- decompile (high): signature check 0x800068ce

## `PHY_RES_PARAM`

Offset `0x32108` · access `rw` · 32 bits

Sub-parameter.

Sources:

- decompile (medium): the firmware seeds [+0x08]

## `PHY_RES_SIGNATURE`

Offset `0x3210C` · access `rw` · 32 bits · reset `0x2230000`

Seed on the way in; on the way out the PHY signature for command 0x101, else 0 for 'no error'. The signature is a memsys version tag, not a constant: the bootloader loads it from the memsys config record it selected and compares the PHY's echo (Cmp r7, [+0x0C] at pc 0x800067de). The model reconstructs it from the PHY microcode the firmware wrote to PHY_B (marker 0x1860_02vv at +0x388, optional companion 0xA863_llll at +0x38C -> 0x02vv_llll); see src/periph/sdramc.rs report_signature. The reset value is the pinned board's 0x0223 memsys signature, used as the fall-back.

Sources:

- decompile (high): 0x800067de compares [+0x0C] with r7 (the MCB signature); 0x800065f6 / 0x800066a6 want 0
- trace (high): write of the version marker to 0x7DC3_8388 (pc 0x800066ba/0x8000675e) vs the expected r7 across the 0x0220/0x0222/0x0223 memsys versions

## `PHY_RES_STATUS`

Offset `0x32110` · access `rw` · 32 bits

Result: rank << 8 for command 0x101, else 0.

Sources:

- decompile (high): 0x800068ce checks ([+0x10] >> 8) & 0xFF against the rank it verifies

## `PHY_RES_SUM5`

Offset `0x32114` · access `rw` · 32 bits

Sum of words +0x00..+0x10.

Sources:

- decompile (high): 'sum the leading words, compare the trailing word' checks

## `PHY_RES_SUM6`

Offset `0x32118` · access `rw` · 32 bits

Sum of words +0x00..+0x14.

Sources:

- decompile (high): 'sum the leading words, compare the trailing word' checks

## `PHY_A`

Offset `0x34000`, 1024 elements 0x4 apart · access `rw` · 32 bits

PHY register array filled from a memsys preset and sum-checked.

Sources:

- trace (medium): the bootcode copies a preset in and reads it back

## `PHY_B`

Offset `0x38000`, 4096 elements 0x4 apart · access `rw` · 32 bits

Larger PHY register array, as PHY_A. Byte-addressable: the 2022-04-26 bootcode decompresses mcb.bin straight into it (a 16 KB buffer at +0x38000) and hashes it a byte at a time, so narrow reads get their lane and narrow writes merge (#69).

Sources:

- trace (medium): the bootcode copies a preset in and reads it back
- trace (high): 2022-04-26 bootcode: mcb.bin decompressed to 0x7DC3_8000 (0x1540 of the buffer's 0x4000 bytes), then SHA-256 over it; a wrong lane there is 'mcb.bin mismatch' / 'Missing SDRAM FW' (#69)
