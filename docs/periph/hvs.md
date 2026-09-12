<!-- generated from specs/hvs.toml by `cargo run -- spec-docs --update` – do not edit -->

# `hvs` – HVS (Hardware Video Scaler): identification and the per-channel frame-swap words

- Bus: `vpu` (VPU bus address)
- Base: `0x7E400000`
- Size: `0x1000`

No display is modelled: scanout catches up with a queued frame immediately. Everything else is stored and read back.

Sources:

- decompile (high): bootloader diagnostic-display channel-swap wait 0x0008adc0; start4 display bring-up 0x3EC945CC

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x008` | [`DISPID`](#dispid) | r | 32 | 2, best high |
| `0x020`–`0x02C` (4 × 0x4) | [`REQUESTED`](#requested) | rw | 32 | 1, best medium |
| `0x030`–`0x03C` (4 × 0x4) | [`CURRENT`](#current) | r | 32 | 1, best medium |

## `DISPID`

Offset `0x008` · access `r` · 32 bits · reset `0x64647276`

Identification. start4 gates its whole display bring-up, and with it the HDMI provider registration, on this value; 0 reads as 'no HVS' (issue #13).

Sources:

- measured (high): /sys/kernel/debug/dri/0/hvs_regs on the reference board: SCALER_DISPID = 0x64647276
- decompile (high): 0x3EC945CC compares [0x7E400008] with 0x64647276 and returns -1 on a mismatch

## `REQUESTED`

Offset `0x020`, 4 elements 0x4 apart · access `rw` · 32 bits

Frame each channel has queued.

Sources:

- decompile (medium): 0x0008adc0 spins until (*current & 0xFFFF) == (*requested & 0xFFF), requested at +0x20 + 4 * chan

## `CURRENT`

Offset `0x030`, 4 elements 0x4 apart · access `r` · 32 bits

Frame each channel is scanning out. The model mirrors REQUESTED.

Sources:

- decompile (medium): 0x0008adc0 reads current at +0x30 + 4 * chan
