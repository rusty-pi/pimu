<!-- generated from specs/hdmi.toml by `cargo run -- spec-docs --update` – do not edit -->

# `hdmi` – HDMI controller core registers (the `hdmi` window of each connector), with no monitor attached: the packet-RAM handshake, the FIFO recenter and the hotplug state

- Bus: `vpu` (VPU bus address)
- Base: `0x7EF00700`
- `HDMI1` copy: `0x7EF05700`
- Size: `0x300`

No encoder or PHY behind it, and by default no sink on either connector (`boot --display` puts one on HDMI0). What firmware waits on is modelled: a packet slot's status bit follows its enable in `RAM_PACKET_CONFIG` at once, a FIFO recenter completes as soon as it is asked for, and `HOTPLUG` reports nothing plugged in. Everything else is stored and read back.

Sources:

- linux (high): `arch/arm/boot/dts/broadcom/bcm2711.dtsi`: `hdmi0: hdmi@7ef00700`, `reg <0x7ef00700 0x300>`, `reg-names "hdmi"`
- linux (high): `drivers/gpu/drm/vc4/vc4_hdmi_regs.h`: `vc5_hdmi_hdmi0_fields`, `vc5_hdmi_hdmi1_fields`

`HDMI1` copy:

HDMI1's core registers; HDMI0's are the block base.

- linux (high): `arch/arm/boot/dts/broadcom/bcm2711.dtsi`: `hdmi1: hdmi@7ef05700`, `reg <0x7ef05700 0x300>`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x074` | [`FIFO_CTL`](#fifo_ctl) | rw | 32 | 2, best high |
| `0x0BC` | [`RAM_PACKET_CONFIG`](#ram_packet_config) | rw | 32 | 2, best high |
| `0x0C4` | [`RAM_PACKET_STATUS`](#ram_packet_status) | r | 32 | 2, best high |
| `0x1A8` | [`HOTPLUG`](#hotplug) | r | 32 | 3, best high |

## `FIFO_CTL`

Offset `0x074` · access `rw` · 32 bits

Control of the FIFO between the pixel valve and the encoder. After a mode set, software pulses `RECENTER` and waits for `RECENTER_DONE`. The 2020-era bootcode does that on every boot, monitor or not, and spins with no timeout; on plain storage the bit never set and those EEPROM images went no further (#63). The model sets `RECENTER_DONE` on any write with `RECENTER` set. The other bits are stored: the bootcode writes `0x5`, `MASTER_SLAVE_N | CAPTURE_PTR` in Linux's names.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 6 | `RECENTER` | rw | Starts a recenter. |
| 14 | `RECENTER_DONE` | r | Set once a recenter has finished. Recentring needs no sink: headless boards boot the 2020 bootcode, which cannot get past this bit otherwise. |

Sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_hdmi_regs.h`: `VC4_HDMI_REG(HDMI_FIFO_CTL, 0x074)` in `vc5_hdmi_hdmi0_fields`
- trace (high): pieeprom-2020-09-03 bootcode, `RVF_TRACE_MMIO`: `0x80007894` writes `0x5`, `0x8000775e` `0x45`, `0x80007776` `0x5`, then `0x8000777e` reads it forever

`RECENTER` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `VC4_HDMI_FIFO_CTL_RECENTER` `BIT(6)`; `vc4_hdmi.c`: `vc4_hdmi_recenter_fifo()` writes it clear, then set, twice

`RECENTER_DONE` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `VC4_HDMI_FIFO_CTL_RECENTER_DONE` `BIT(14)`; `vc4_hdmi.c`: `vc4_hdmi_recenter_fifo()` waits 1 ms for it and warns on timeout
- decompile (high): pieeprom-2020-09-03 bootcode `0x8000777e..0x80007784`: `ld r0, [r2+0x74]; btest r0, #14; beq back`, no timeout

## `RAM_PACKET_CONFIG`

Offset `0x0BC` · access `rw` · 32 bits

One enable bit per packet slot. To rewrite a packet start4 clears its slot's bit, waits for the status to drop, writes the packet and sets the bit again (`0x3ECDFD68`, the AV-mute general control packet in slot 0).

| Bits | Field | Access | Notes |
|---|---|---|---|
| 15:0 | `PACKETS` | rw |  |

Sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_hdmi_regs.h`: `VC4_HDMI_REG(HDMI_RAM_PACKET_CONFIG, 0x0bc)` in `vc5_hdmi_hdmi0_fields`
- decompile (high): `0x3ECDFD68` clears and sets bit 0 of `[ctx+0xe8]+0xbc` around a packet write

`PACKETS` sources:

- linux (medium): `drivers/gpu/drm/vc4/vc4_hdmi.c`: `HDMI_RAM_PACKET_CONFIG & ~BIT(packet_id)`, `| BIT(packet_id)` around each infoframe write

## `RAM_PACKET_STATUS`

Offset `0x0C4` · access `r` · 32 bits

Which packet slots the encoder is sending. The model mirrors `RAM_PACKET_CONFIG.PACKETS`, so both of start4's waits (bit clear after disabling, bit set after enabling) finish on the first read.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 15:0 | `PACKETS` | r |  |

Sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_hdmi.c`: `wait_for(!(HDMI_READ(HDMI_RAM_PACKET_STATUS) & BIT(packet_id)), 100)`, then `wait_for(HDMI_READ(HDMI_RAM_PACKET_STATUS) & BIT(packet_id), 100)`
- decompile (high): `0x3ECDFD68` spins on bit 0 of `[ctx+0xe8]+0xc4`, with no timeout, after each change of the enable

`PACKETS` sources:

- linux (medium): `drivers/gpu/drm/vc4/vc4_hdmi.c`: `HDMI_RAM_PACKET_STATUS & BIT(packet_id)`

## `HOTPLUG`

Offset `0x1A8` · access `r` · 32 bits

Whether a monitor is on this connector. Read-only: a write changes nothing either way. With none attached `CONNECTED` reads 0 and Linux's `vc4` reports the connector disconnected, which is the default and what the reference board does; `boot --display` attaches one and it reads set. Setting it is not the same lever as a board's `hdmi_force_hotplug=1`, which does not make the firmware behave as though the line were asserted -- see the measurement below.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `CONNECTED` | r |  |

Sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_hdmi_regs.h`: `VC4_HDMI_REG(HDMI_HOTPLUG, 0x1a8)` in `vc5_hdmi_hdmi0_fields`; `vc4_hdmi.c`: `vc5_hdmi_hp_detect()`
- measured (medium): Raspberry Pi 4B d03115, no monitor attached: `/sys/class/drm/card1-HDMI-A-1/status` and `card1-HDMI-A-2/status` read disconnected — _The connector state, not the register: `vc4` reports disconnected when `CONNECTED` is clear and the node has no `hpd-gpios`._
- measured (high): Raspberry Pi 4B d03115, no monitor, stock start4 `f5e89631` booted with `hdmi_force_hotplug=1`, `hdmi_group=1`, `hdmi_drive=2` (confirmed applied by `vcgencmd get_config int`): the firmware still logs `HDMI0:EDID error reading EDID block 0 attempt 0` and gives up after that one attempt. With `CONNECTED` set in the model instead, it retries ten times. So the config option and this bit are different levers, and a `--display` boot is not a forced-hotplug board.

`CONNECTED` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `VC4_HDMI_HOTPLUG_CONNECTED` `BIT(0)`; `vc4_hdmi.c`: `vc5_hdmi_hp_detect()` returns `HDMI_HOTPLUG & VC4_HDMI_HOTPLUG_CONNECTED`
