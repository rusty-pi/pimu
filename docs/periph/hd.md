<!-- generated from specs/hd.toml by `cargo run -- spec-docs --update` – do not edit -->

# `hd` – Control block at 0x7E80_8000: the power request and acknowledge start4 waits on before it resets the DWC2 USB controller

- Bus: `vpu` (VPU bus address)
- Base: `0x7E808000`
- Size: `0x100`

Only the USB power handshake is modelled; the rest of the window is plain storage. The name is a label, not an identification: BCM2835 has its HDMI 'HD' block at this address and start4's HDMI code writes +0x2C (FUN_0ece981c, from the HDMI state-machine clock) and bit 30 of +0x38 (0x3ECE36D8, 0x3ECE7716) here, but BCM2711's Linux binding puts its 'hd' range at 0x7EF20000.

Sources:

- decompile (high): SET_POWER_STATE USB handler 0x3ED89520 (BCM2711 branch at 0x3ED8955E); the same request at 0x3EC607B0 and 0x3ECACB0C

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x008` | [`CTRL`](#ctrl) | rw | 32 | 2, best high |
| `0x020` | [`STATUS`](#status) | r | 32 | 2, best high |

## `CTRL`

Offset `0x008` · access `rw` · 32 bits · reset `0x3`

start4 sets POWER before it resets the DWC2 core. Nothing clears it again, not even the USB power-off path.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 2 | `POWER` | rw | Power request for the USB controller. STATUS.ACK follows it. |

Sources:

- measured (high): rpi-dev (start4 f5e89631, Linux idle), 32-bit /dev/mem read of 0xFE808008: 0x3 before any USB power request, 0x7 after vcmailbox SET_POWER_STATE(USB, on|wait), still 0x7 after SET_POWER_STATE(USB, off|wait)
- decompile (high): 0x3ED89564, 0x3EC607B6 and 0x3ECACB14 set bit 2; 0x3ECACB1A..0x3ECACB28 then clear bits 0 and 1

`POWER` sources:

- decompile (medium): 0x3ED89564..0x3ED8956E: [+0x08] |= 4, spin until [+0x20] & 3 == 3, then reset the DWC2 core at 0x7E980000 — _The name is ours, from what the firmware does next._

## `STATUS`

Offset `0x020` · access `r` · 32 bits

Both ACK bits read 1 while CTRL.POWER is set and 0 while it is clear. The model sets them as soon as POWER is written.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 1:0 | `ACK` | r | Power acknowledge. |

Sources:

- measured (high): rpi-dev, 32-bit /dev/mem read of 0xFE808020: 0x0 while CTRL read 0x3, 0x3 while CTRL read 0x7 (the same three points as CTRL)
- decompile (high): 0x3ED8956A and 0x3EC607BC spin until [+0x20] & 3 == 3 after setting CTRL.POWER, with no timeout; 0x3ECACB2A spins until bit 0 is set

`ACK` sources:

- measured (high): rpi-dev: 0 without CTRL.POWER, 3 with it
