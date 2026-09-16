<!-- generated from specs/asb.toml by `cargo run -- spec-docs --update` – do not edit -->

# `asb` – AXI async slave bridges: the stop / acknowledge handshake before gating the V3D, ISP and H264 power domains

- Bus: `vpu` (VPU bus address)
- Base: `0x7E00A000`
- Size: `0x1000`

`ACK` mirrors `REQ_STOP` immediately (no AXI traffic to drain), queues always read empty. The bridges gate nothing: there is no V3D / ISP / H264 block behind them.

Sources:

- linux (high): `drivers/pmdomain/bcm/bcm2835-power.c` names every register and bit and maps this base
- decompile (high): start4 power-domain switch `FUN_0ED54E40`: domain bit `0x40` -> `+0x08`/`+0x0C`, `0x08` -> `+0x10`/`+0x14`, `0x04` -> `+0x18`/`+0x1C`, each followed by the matching PM reset bit

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`BRDG_VERSION`](#brdg_version) | rw | 32 | 1, best high |
| `0x004` | [`CPR_CTRL`](#cpr_ctrl) | rw | 32 | 1, best high |
| `0x008`–`0x01C` (6 × 0x4) | [`CTRL`](#ctrl) | rw | 32 | 2, best high |
| `0x020` | [`AXI_BRDG_ID`](#axi_brdg_id) | r | 32 | 1, best high |

## `BRDG_VERSION`

Offset `0x000` · access `rw` · 32 bits

Bridge version. Plain storage reading 0: nothing reads it.

Sources:

- linux (high): `bcm2835-power.c`: `ASB_BRDG_VERSION`

## `CPR_CTRL`

Offset `0x004` · access `rw` · 32 bits

Clock / power-reduction control. Plain storage.

Sources:

- linux (high): `bcm2835-power.c`: `ASB_CPR_CTRL`

## `CTRL`

Offset `0x008`, 6 elements 0x4 apart · access `rw` · 32 bits

One control word per bridge: V3D slave, V3D master, ISP slave, ISP master, H264 slave, H264 master. Request first, then wait for `ACK` to follow.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `REQ_STOP` | rw | Ask the bridge to stop. The only writable bit. |
| 1 | `ACK` | r | The bridge has stopped (follows `REQ_STOP`). |
| 2 | `EMPTY` | r | Transaction queue empty. |
| 3 | `FULL` | r | Transaction queue full. |

Sources:

- linux (high): `bcm2835-power.c`: `ASB_V3D_S_CTRL` .. `ASB_H264_M_CTRL` at `0x08..0x1C`; `bcm2835_asb_control()` polls `ACK` after setting `REQ_STOP`
- decompile (high): `0xced550a2..0xced550ac`: bset `REQ_STOP` at `[r4+28]`, then spin while `ACK == 0` (the boot's wall before this block was mapped)

`REQ_STOP` sources:

- linux (high): `bcm2835-power.c`: `ASB_REQ_STOP`

`ACK` sources:

- linux (high): `bcm2835-power.c`: `ASB_ACK`

`EMPTY` sources:

- linux (high): `bcm2835-power.c`: `ASB_EMPTY`

`FULL` sources:

- linux (high): `bcm2835-power.c`: `ASB_FULL`

## `AXI_BRDG_ID`

Offset `0x020` · access `r` · 32 bits · reset `0x62726467`

`brdg` little-endian; Linux's probe requires it.

Sources:

- linux (high): `bcm2835-power.c`: `ASB_AXI_BRDG_ID` must read `BCM2835_BRDG_ID`
