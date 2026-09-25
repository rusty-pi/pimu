<!-- generated from specs/hvs.toml by `cargo run -- spec-docs --update` – do not edit -->

# `hvs` – HVS (Hardware Video Scaler): identification, the per-channel frame-swap words, and end of frame

- Bus: `vpu` (VPU bus address)
- Base: `0x7E400000`
- Size: `0x1000`
- Interrupts: VPU source 97 · GIC id 129 (`GIC_SPI 97`)

No display is modelled. Scanout catches up with a queued frame immediately, and a running channel raises its `DISPSTAT` frame flags once per 640x480@60 frame time (16683 µs) and, with their interrupts enabled, VPU interrupt source 97. Everything else is stored and read back.

Sources:

- decompile (high): bootloader diagnostic-display channel-swap wait `0x0008adc0`; start4 display bring-up `0x3EC945CC`
- linux (high): `arch/arm/boot/dts/broadcom/bcm2711.dtsi`: `hvs@7e400000` `interrupts = <GIC_SPI 97>`; start4 registers its HVS handler `0x3ECEED5C` on VPU source 97 (`--log irqtbl`)

Interrupts (VPU source 97 · GIC id 129 (`GIC_SPI 97`)):

A channel with `DISPEIRQx` and the flag enabled drives the line once per frame. The two controllers carry the same number because both are `64 + VC peripheral IRQ`: for every VC peripheral IRQ `n` the VPU source and the device tree's `GIC_SPI` number are both `64 + n` (the GIC id is 32 above that again), and the HVS is IRQ 33.

- linux (high): `arch/arm/boot/dts/broadcom/bcm2711.dtsi`: `hvs@7e400000`, `interrupts = <GIC_SPI 97 IRQ_TYPE_LEVEL_HIGH>`
- trace (high): start4 registers its HVS handler `0x3ECEED5C` on VPU source 97 (`--log irqtbl`)
- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102: VC peripheral IRQ 33 is `HVS`, source 97; §6.3 Figure 7 lands the same 64 IRQs on GIC SPI ids 96 to 159

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`DISPCTRL`](#dispctrl) | rw | 32 | 2, best high |
| `0x004` | [`DISPSTAT`](#dispstat) | w1c | 32 | 2, best high |
| `0x008` | [`DISPID`](#dispid) | r | 32 | 2, best high |
| `0x020`–`0x02C` (4 × 0x4) | [`REQUESTED`](#requested) | rw | 32 | 1, best medium |
| `0x030`–`0x03C` (4 × 0x4) | [`CURRENT`](#current) | r | 32 | 1, best medium |
| `0x040`–`0x060` (3 × 0x10) | [`DISPCTRLX`](#dispctrlx) | rw | 32 | 2, best high |
| `0x048`–`0x068` (3 × 0x10) | [`DISPSTATX`](#dispstatx) | r | 32 | 2, best high |

## `DISPCTRL`

Offset `0x000` · access `rw` · 32 bits

Global control. The per-channel frame-interrupt enables are the HVS5 ones, four bits a channel (`vc4_regs.h` `SCALER5_DISPCTRL_*`); the SLUR enables in between are not modelled.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 31 | `ENABLE` | rw |  |
| 16 | `DSPEIEOLN2` | rw |  |
| 15 | `DSPEIEOF2` | rw |  |
| 14 | `DSPEIVST2` | rw |  |
| 12 | `DSPEIEOLN1` | rw |  |
| 11 | `DSPEIEOF1` | rw |  |
| 10 | `DSPEIVST1` | rw |  |
| 8 | `DSPEIEOLN0` | rw |  |
| 7 | `DSPEIEOF0` | rw |  |
| 6 | `DSPEIVST0` | rw |  |
| 3 | `DISPEIRQ2` | rw |  |
| 2 | `DISPEIRQ1` | rw |  |
| 1 | `DISPEIRQ0` | rw |  |

Sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPCTRL`
- decompile (high): `0x3ECED90C` writes `3 << (chan * 2 + 7) | 1 << (chan + 1) | 0x80000000`; `0x3ECEDE98` and `0x3ECEF33C` clear the same bits

`ENABLE` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPCTRL_ENABLE`

`DSPEIEOLN2` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER5_DISPCTRL_DSPEIEOLN(x)` `BIT(8 + 4x)`

`DSPEIEOF2` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER5_DISPCTRL_DSPEIEOF(x)` `BIT(7 + 4x)`

`DSPEIVST2` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER5_DISPCTRL_DSPEIVST(x)` `BIT(6 + 4x)`

`DSPEIEOLN1` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER5_DISPCTRL_DSPEIEOLN(x)` `BIT(8 + 4x)`

`DSPEIEOF1` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER5_DISPCTRL_DSPEIEOF(x)` `BIT(7 + 4x)`

`DSPEIVST1` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER5_DISPCTRL_DSPEIVST(x)` `BIT(6 + 4x)`
- measured (high): start4 leaves `DISPCTRL = 0x9a0ddfff` with channel 1 running: every enable of all three channels set (`PIMU_TRACE_MMIO`)

`DSPEIEOLN0` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER5_DISPCTRL_DSPEIEOLN(x)` `BIT(8 + 4x)`

`DSPEIEOF0` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER5_DISPCTRL_DSPEIEOF(x)` `BIT(7 + 4x)`

`DSPEIVST0` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER5_DISPCTRL_DSPEIVST(x)` `BIT(6 + 4x)`

`DISPEIRQ2` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPCTRL_DISPEIRQ(x)` `BIT(1 + x)`

`DISPEIRQ1` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPCTRL_DISPEIRQ(x)` `BIT(1 + x)`

`DISPEIRQ0` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPCTRL_DISPEIRQ(x)` `BIT(1 + x)`

## `DISPSTAT`

Offset `0x004` · access `w1c` · 32 bits

Only the frame flags are modelled: a running channel raises `EOLN` half-way through each frame, `EOF` after its last active line and `VSTART` as the next frame starts. `IRQDISPx` reads as set while one of channel x's flags is set with its enable; start4's handler writes back what it read less those bits. On this HVS (start4's flag `gp+0x1564 = 1`) the handler posts a channel's display events on `VSTART`.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 30 | `VSTART2` | w1c |  |
| 22 | `VSTART1` | w1c |  |
| 14 | `VSTART0` | w1c |  |
| 28 | `EOLN2` | w1c |  |
| 20 | `EOLN1` | w1c |  |
| 12 | `EOLN0` | w1c |  |
| 24 | `EOF2` | w1c |  |
| 16 | `EOF1` | w1c |  |
| 8 | `EOF0` | w1c |  |
| 3 | `IRQDISP2` | w1c |  |
| 2 | `IRQDISP1` | w1c |  |
| 1 | `IRQDISP0` | w1c |  |

Sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTAT`
- decompile (high): the source-97 handler `0x3ECEED5C` reads `[0x7E400004]` and writes the value back `& ~0xE`; with `gp+0x1564` set it posts channel x's display event on bit `14 + 8x` (`0x3ECEEFCE`), without it it does the channel's display-list work on bit `12 + 8x` when bit `8 + 8x` is clear (`0x3ECEF014`)

`VSTART2` sources:

- decompile (high): `0x3ECEEFFE`: with `gp+0x1564` set, the source-97 handler posts channel 2's display event on bit 30

`VSTART1` sources:

- decompile (high): `0x3ECEEFEA`: with `gp+0x1564` set, the source-97 handler posts channel 1's display event on bit 22

`VSTART0` sources:

- decompile (high): `0x3ECEEFD6`: with `gp+0x1564` set, the source-97 handler posts channel 0's display event on bit 14

`EOLN2` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTAT_EOLN(x)` `BIT(12 + 8x)`

`EOLN1` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTAT_EOLN(x)` `BIT(12 + 8x)`
- decompile (high): `0x3ECEEF66` tests bit 20 before channel 1's display-list work

`EOLN0` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTAT_EOLN(x)` `BIT(12 + 8x)`
- decompile (high): `0x3ECEEF32` tests bit 12 before channel 0's display-list work

`EOF2` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTAT_EOF(x)` `BIT(8 + 8x)`

`EOF1` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTAT_EOF(x)` `BIT(8 + 8x)`

`EOF0` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTAT_EOF(x)` `BIT(8 + 8x)`

`IRQDISP2` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTAT_IRQDISP(x)` `BIT(1 + x)`

`IRQDISP1` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTAT_IRQDISP(x)` `BIT(1 + x)`

`IRQDISP0` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTAT_IRQDISP(x)` `BIT(1 + x)`

## `DISPID`

Offset `0x008` · access `r` · 32 bits · reset `0x64647276`

Identification. start4 gates its whole display bring-up, and with it the HDMI provider registration, on this value; 0 reads as 'no HVS'.

Sources:

- measured (high): `/sys/kernel/debug/dri/0/hvs_regs` on the reference board: `SCALER_DISPID = 0x64647276`
- decompile (high): `0x3EC945CC` compares `[0x7E400008]` with `0x64647276` and returns -1 on a mismatch

## `REQUESTED`

Offset `0x020`, 4 elements 0x4 apart · access `rw` · 32 bits

Frame each channel has queued.

Sources:

- decompile (medium): `0x0008adc0` spins until `(*current & 0xFFFF) == (*requested & 0xFFF)`, requested at `+0x20 + 4 * chan`

## `CURRENT`

Offset `0x030`, 4 elements 0x4 apart · access `r` · 32 bits

Frame each channel is scanning out. The model mirrors `REQUESTED`.

Sources:

- decompile (medium): `0x0008adc0` reads current at `+0x30 + 4 * chan`

## `DISPCTRLX`

Offset `0x040`, 3 elements 0x10 apart · access `rw` · 32 bits

Per-channel control. A channel runs while this `ENABLE` and `DISPCTRL.ENABLE` are both set.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 31 | `ENABLE` | rw |  |

Sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPCTRL0`, `SCALER_DISPCTRLX(x)`
- decompile (high): `0x3ECED90C` sets bit 31 of `[0x7E400040 + chan * 0x10]` to start a channel; `0x3ECEF584` clears it to finish a pause

`ENABLE` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPCTRLX_ENABLE`

## `DISPSTATX`

Offset `0x048`, 3 elements 0x10 apart · access `r` · 32 bits

Per-channel status: a running channel reads `MODE` run, a stopped one `MODE` disabled with its FIFO `EMPTY`. start4 pauses a channel at once only in that last state (`0x3ECEF33C` tests `(stat & 0xD0000000) == 0x10000000`); otherwise it waits for the channel's end of frame.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 31:30 | `MODE` | r |  |
| 28 | `EMPTY` | r |  |

Sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTAT0`, `SCALER_DISPSTATX_*`
- decompile (high): `0x3ECEF33C` reads `[0x7E400048 + chan * 0x10]` before choosing an immediate or a delayed pause

`MODE` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTATX_MODE` (0 disabled, 1 init, 2 run, 3 `EOF`)

`EMPTY` sources:

- linux (high): `drivers/gpu/drm/vc4/vc4_regs.h`: `SCALER_DISPSTATX_EMPTY`
