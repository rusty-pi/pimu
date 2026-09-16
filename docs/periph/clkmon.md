<!-- generated from specs/clkmon.toml by `cargo run -- spec-docs --update` – do not edit -->

# `clkmon` – VPU clock block (PLLs and frequency monitors) below the 0x7E window

- Bus: `vpu` (VPU bus address)
- Base: `0x7D5D0000`
- Size: `0x10000`

The AVS monitor (`avs`, +0x2000) and the PVT monitors (`pvt`, +0x8000) sit inside this window and are decoded ahead of it. A full traced boot touches no PLL divider or lock register here; the registers below are the complete list outside the two sub-blocks, and all are plain storage with the password byte masked.

Sources:

- decompile (high): start4's clock manager uses this block when the SoC-type switch at 0x3EC635F0 sets [gp+5476] = 1 (BCM2711)
- trace (high): RVF_TRACE_MMIO=0x7d5d0000-0x7d5e0000 over a boot to arm_loader

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x1800` | [`REG_1800`](#reg_1800) | rw | 32 | 2, best high |
| `0x1814` | [`REG_1814`](#reg_1814) | rw | 32 | 2, best high |
| `0x1820` | [`REG_1820`](#reg_1820) | rw | 32 | 2, best high |
| `0x183C` | [`CHAR_DONE`](#char_done) | rw | 32 | 2, best high |
| `0xA000` | [`REG_A000`](#reg_a000) | rw | 32 | 1, best high |

## `REG_1800`

Offset `0x1800` · access `rw` · 32 bits

FUN_0ec2ffb4 sets bit 2 from the measured core voltage.

Sources:

- decompile (high): FUN_0ec2ffb4
- measured (high): 0x00000004 on a Raspberry Pi 4B d03115 (post-start4 state)

## `REG_1814`

Offset `0x1814` · access `rw` · 32 bits

FUN_0ec2ffdc tests (reg & 0x14) != 0 as the condition of a voltage-ramp loop. Hardware reads 0x16; the model keeps 0 ('settled'), because nothing in that loop writes the register and a constant 'true' would never exit.

Sources:

- decompile (high): FUN_0ec2ffdc, loop in FUN_0ec303e8 behind a FUN_0ec30196() != 0 guard
- measured (high): 0x00000016 on a Raspberry Pi 4B d03115 — _deliberately not reproduced; see notes_

## `REG_1820`

Offset `0x1820` · access `rw` · 32 bits

FUN_0ec30196 takes bit 10 as a predicate and FUN_0ec301a6 decodes bits 23:11. Reads 0 on hardware too: not a blanked status register.

Sources:

- decompile (high): FUN_0ec30196 / FUN_0ec301a6
- measured (high): 0x00000000 on a Raspberry Pi 4B d03115

## `CHAR_DONE`

Offset `0x183C` · access `rw` · 32 bits

start4's own 'characterisation done' flag.

Sources:

- decompile (high): FUN_0ec302e2 writes it
- measured (high): 0x00000001 on a Raspberry Pi 4B d03115

## `REG_A000`

Offset `0xA000` · access `rw` · 32 bits

Written 0xA0, never read back.

Sources:

- decompile (high): FUN_0ec30200
