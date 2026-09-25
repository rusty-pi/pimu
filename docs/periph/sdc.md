<!-- generated from specs/sdc.toml by `cargo run -- spec-docs --update` – do not edit -->

# `sdc` – Legacy SDRAM-controller interface: DRAM timing table, sub-controller ready bits, LPDDR4 mode-register port

- Bus: `vpu` (VPU bus address)
- Base: `0x7E001000`
- Size: `0x1000`

The DRAM clock tree is not modelled: timing words read back, every sub-controller reports ready, and the mode registers are a table seeded with the reference board's MR4 and an MR8 for the parts the board is fitted with. MR8 `OP[5:2]` is the density of one die (`0b0100` 16 Gb, `0b0110` 32 Gb) and `OP[7:6]` its width; the firmware multiplies the density by the ranks it finds into the size it logs (`total-size: NNGbit`) and keys its MCB record on, so the parts decide how much memory the board has: one rank of 16 Gb dies is a 2 GB Pi 4, one rank of 32 Gb dies a 4 GB one, and two ranks of 32 Gb dies the 8 GB reference board, whose `/memory@0` then carries four ranges (the 32-bit size cell cannot hold a bank above 4 GB in one). The model picks them from the RAM behind the bus.

Sources:

- decompile (high): bootloader timing setup (`0x80006380` prints one word as `SD_SB`) and ready poll `0x8000a3e0`; start4's SDRAM driver in the `.drivers` entry at `0x3EDFDF68`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`TIMING0`](#timing0) | rw | 32 | 1, best medium |
| `0x004` | [`REFRESH`](#refresh) | rw | 32 | 4, best high |
| `0x008`–`0x030` (11 × 0x4) | [`TIMING`](#timing) | rw | 32 | 1, best medium |
| `0x09C`–`0xF9C` (31 × 0x80) | [`STATUS`](#status) | rw | 32 | 2, best high |

## `TIMING0`

Offset `0x000` · access `rw` · 32 bits

First word of the DRAM timing table the bootloader programs.

Sources:

- decompile (medium): bootloader writes `+0x00..+0x30` after PHY training

## `REFRESH`

Offset `0x004` · access `rw` · 32 bits

Timing word carrying the refresh interval, which start4 rescales from MR4 once the ARM runs. At most once a second it reads MR4 from every rank on every channel and takes the highest code (bits 2:0; a failed read counts as 7). If that differs from the last one (3 to begin with), the interval becomes the bootloader's value shifted left by `3 - code` for codes 1 and 2, right by `code - 3` for 3 to 5, and left as it was for anything else (`Unexpected sdram refresh code`); bits 15:0 are kept.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 31:16 | `INTERVAL` | rw | Refresh interval. |

Sources:

- decompile (high): `0x3ED6BBA0` rescales `[0x7E001004] >> 16` by `1 << (3 - MR4 code)`
- decompile (high): `0x3ED6BBA0`: returns early unless 1 000 000 us have passed (the first call backdates its timestamp by that much), `bmask r0, 3` / `max` over `MR4` reads, compares with `[gp+4380]`, `shl` for codes below 3, `asr` above, `(v << 16) | (old & 0xffff)`; its caller `0x3ED7C75C` sleeps 100 ms per pass and passes the rank count (2 if the `BSDR` boot-info word at `+0x0C` is set, else 1) and the channel count (`BSDR` `+0x14`, at least 1)
- trace (high): `boot` of the pinned EEPROM, `PIMU_TRAP=0x3ED6BC14`: previous code 3 on the first pass, then 2; `PIMU_TRACE_MMIO`: one pass reads `MR4` with `+0x9C <- 0x4` and `0x2000004`, then `+0x04 <- 0x0C3406F4` from `0x061A06F4`; 589 calls and 59 reads between the ARM start and the end of the run
- measured (high): start4 on a Raspberry Pi 4B d03115: `sdram: sdram refresh 1562->3124 (2)`

`INTERVAL` sources:

- decompile (high): `0x3ED6BBA0` reads and writes bits 31:16

## `TIMING`

Offset `0x008`, 11 elements 0x4 apart · access `rw` · 32 bits

The rest of the timing table (`+0x08..+0x30`): packed tRAS / tRC / tRCD / tRFC-style fields.

Sources:

- decompile (medium): bootloader writes `+0x00..+0x30` after PHY training

## `STATUS`

Offset `0x09C`, 31 elements 0x80 apart · access `rw` · 32 bits

Per-sub-controller status, at `+0x1C` of each `0x80` block from `+0x80`; bit 31 is ready. Element 0 is also the LPDDR4 mode-register access port: write a command, poll `DONE`, take the byte from `RDATA`.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 7:0 | `ADDR` | rw | Mode register number. |
| 15:8 | `WDATA` | rw | Byte to write. |
| 23:16 | `RDATA` | rw | Byte read back. |
| 24 | `DEVICE` | rw | Device (rank, i.e. chip select). A rank that is not fitted answers nothing, so every mode register reads 0 there; on a dual-rank board both answer alike, which is how the firmware counts the second one. |
| 25 | `CHANNEL` | rw | LPDDR4 channel. |
| 28 | `WRITE` | rw | Set for a write, clear for a read. |
| 30 | `ERROR` | r | Transfer failed (`SD MR %08x R timeout`). Never set here. |
| 31 | `DONE` | r | Ready / transfer complete. |

Sources:

- decompile (high): `0x8000a3e0` polls `[0x7E00109C] & 0x80000000` ten times with 1 ms sleeps ('block device timeout')
- decompile (high): start4 mode-register read `0x3ED6BA90` and write `0x3ED6C084`

`ADDR` sources:

- decompile (high): `0x3ED6BA90`: `addr | chan << 24 | dev << 25`

`WDATA` sources:

- decompile (high): `0x3ED6C084` puts the data in bits 15:8

`RDATA` sources:

- decompile (high): `0x3ED6BA90` takes the result from bits 23:16

`DEVICE` sources:

- decompile (high): `0x3ED6BA90` logs `RD: MR addr: %d device: %d channel: %d` with the argument it shifts to bit 24 as the device
- decompile (high): 2023-05-11 bootcode rank detection (`0x800056a4`): x2 only when MR8 reads the same with bit 24 clear and set

`CHANNEL` sources:

- decompile (high): `0x3ED6BA90` logs the argument it shifts to bit 25 as the channel

`WRITE` sources:

- decompile (high): `0x3ED6C084` sets bit 28

`ERROR` sources:

- decompile (high): `0x3ED6BA90` checks bit 30 and logs the timeout

`DONE` sources:

- decompile (high): `0x8000a3e0` and `0x3ED6BA90` both poll bit 31
