<!-- generated from specs/sdc.toml by `cargo run -- spec-docs --update` – do not edit -->

# `sdc` – Legacy SDRAM-controller interface: DRAM timing table, sub-controller ready bits, LPDDR4 mode-register port

- Bus: `vpu` (VPU bus address)
- Base: `0x7E001000`
- Size: `0x1000`

The DRAM clock tree is not modelled: timing words read back, every sub-controller reports ready, and the mode registers are a table seeded with the reference board's MR4.

Sources:

- decompile (high): bootloader timing setup (0x80006380 prints one word as SD_SB) and ready poll 0x8000a3e0; start4's SDRAM driver in the .drivers entry at 0x3EDFDF68

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`TIMING0`](#timing0) | rw | 32 | 1, best medium |
| `0x004` | [`REFRESH`](#refresh) | rw | 32 | 2, best high |
| `0x008`–`0x030` (11 × 0x4) | [`TIMING`](#timing) | rw | 32 | 1, best medium |
| `0x09C`–`0xF9C` (31 × 0x80) | [`STATUS`](#status) | rw | 32 | 2, best high |

## `TIMING0`

Offset `0x000` · access `rw` · 32 bits

First word of the DRAM timing table the bootloader programs.

Sources:

- decompile (medium): bootloader writes +0x00..+0x30 after PHY training

## `REFRESH`

Offset `0x004` · access `rw` · 32 bits

Timing word carrying the refresh interval, which start4 rescales from MR4 once the ARM runs.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 31:16 | `INTERVAL` | rw | Refresh interval. |

Sources:

- decompile (high): 0x3ED6BBA0 rescales [0x7E001004] >> 16 by 1 << (3 - MR4 code)
- measured (high): examples-on-real-hardware/vc4-boot.log: 'sdram: sdram refresh 1562->3124 (2)'

`INTERVAL` sources:

- decompile (high): 0x3ED6BBA0 reads and writes bits 31:16

## `TIMING`

Offset `0x008`, 11 elements 0x4 apart · access `rw` · 32 bits

The rest of the timing table (+0x08..+0x30): packed tRAS / tRC / tRCD / tRFC-style fields.

Sources:

- decompile (medium): bootloader writes +0x00..+0x30 after PHY training

## `STATUS`

Offset `0x09C`, 31 elements 0x80 apart · access `rw` · 32 bits

Per-sub-controller status, at +0x1C of each 0x80 block from +0x80; bit 31 is ready. Element 0 is also the LPDDR4 mode-register access port: write a command, poll DONE, take the byte from RDATA.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 7:0 | `ADDR` | rw | Mode register number. |
| 15:8 | `WDATA` | rw | Byte to write. |
| 23:16 | `RDATA` | rw | Byte read back. |
| 24 | `CHANNEL` | rw | LPDDR4 channel. |
| 25 | `DEVICE` | rw | Device (rank) on the channel. |
| 28 | `WRITE` | rw | Set for a write, clear for a read. |
| 30 | `ERROR` | r | Transfer failed ('SD MR %08x R timeout'). Never set here. |
| 31 | `DONE` | r | Ready / transfer complete. |

Sources:

- decompile (high): 0x8000a3e0 polls [0x7E00109C] & 0x80000000 ten times with 1 ms sleeps ('block device timeout')
- decompile (high): start4 mode-register read 0x3ED6BA90 and write 0x3ED6C084

`ADDR` sources:

- decompile (high): 0x3ED6BA90: addr | chan << 24 | dev << 25

`WDATA` sources:

- decompile (high): 0x3ED6C084 puts the data in bits 15:8

`RDATA` sources:

- decompile (high): 0x3ED6BA90 takes the result from bits 23:16

`CHANNEL` sources:

- decompile (high): 0x3ED6BA90: chan << 24

`DEVICE` sources:

- decompile (high): 0x3ED6BA90: dev << 25

`WRITE` sources:

- decompile (high): 0x3ED6C084 sets bit 28

`ERROR` sources:

- decompile (high): 0x3ED6BA90 checks bit 30 and logs the timeout

`DONE` sources:

- decompile (high): 0x8000a3e0 and 0x3ED6BA90 both poll bit 31
