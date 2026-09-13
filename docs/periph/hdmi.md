<!-- generated from specs/hdmi.toml by `cargo run -- spec-docs --update` – do not edit -->

# `hdmi` – HDMI controller core registers (the "hdmi" window of each connector): the packet-RAM handshake

- Bus: `vpu` (VPU bus address)
- Base: `0x7EF00700`
- `HDMI1` copy: `0x7EF05700`
- Size: `0x300`

No encoder behind it. A packet slot's status bit follows its enable in RAM_PACKET_CONFIG at once, which is what start4 (and Linux's vc4) wait for around every packet write. Everything else is stored and read back.

Sources:

- linux (high): arch/arm/boot/dts/broadcom/bcm2711.dtsi: hdmi0: hdmi@7ef00700, reg <0x7ef00700 0x300>, reg-names "hdmi"
- linux (high): drivers/gpu/drm/vc4/vc4_hdmi_regs.h: vc5_hdmi_hdmi0_fields, vc5_hdmi_hdmi1_fields

`HDMI1` copy:

HDMI1's core registers; HDMI0's are the block base.

- linux (high): arch/arm/boot/dts/broadcom/bcm2711.dtsi: hdmi1: hdmi@7ef05700, reg <0x7ef05700 0x300>

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x0BC` | [`RAM_PACKET_CONFIG`](#ram_packet_config) | rw | 32 | 2, best high |
| `0x0C4` | [`RAM_PACKET_STATUS`](#ram_packet_status) | r | 32 | 2, best high |

## `RAM_PACKET_CONFIG`

Offset `0x0BC` · access `rw` · 32 bits

One enable bit per packet slot. To rewrite a packet start4 clears its slot's bit, waits for the status to drop, writes the packet and sets the bit again (0x3ECDFD68, the AV-mute general control packet in slot 0).

| Bits | Field | Access | Notes |
|---|---|---|---|
| 15:0 | `PACKETS` | rw |  |

Sources:

- linux (high): drivers/gpu/drm/vc4/vc4_hdmi_regs.h: VC4_HDMI_REG(HDMI_RAM_PACKET_CONFIG, 0x0bc) in vc5_hdmi_hdmi0_fields
- decompile (high): 0x3ECDFD68 clears and sets bit 0 of [ctx+0xe8]+0xbc around a packet write

`PACKETS` sources:

- linux (medium): drivers/gpu/drm/vc4/vc4_hdmi.c: HDMI_RAM_PACKET_CONFIG & ~BIT(packet_id), | BIT(packet_id) around each infoframe write

## `RAM_PACKET_STATUS`

Offset `0x0C4` · access `r` · 32 bits

Which packet slots the encoder is sending. The model mirrors RAM_PACKET_CONFIG.PACKETS, so both of start4's waits (bit clear after disabling, bit set after enabling) finish on the first read.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 15:0 | `PACKETS` | r |  |

Sources:

- linux (high): drivers/gpu/drm/vc4/vc4_hdmi.c: wait_for(!(HDMI_READ(HDMI_RAM_PACKET_STATUS) & BIT(packet_id)), 100), then wait_for(HDMI_READ(HDMI_RAM_PACKET_STATUS) & BIT(packet_id), 100)
- decompile (high): 0x3ECDFD68 spins on bit 0 of [ctx+0xe8]+0xc4, with no timeout, after each change of the enable

`PACKETS` sources:

- linux (medium): drivers/gpu/drm/vc4/vc4_hdmi.c: HDMI_RAM_PACKET_STATUS & BIT(packet_id)
