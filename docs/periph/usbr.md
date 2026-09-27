<!-- generated from specs/usbr.toml by `cargo run -- spec-docs --update` – do not edit -->

# `usbr` – USB reset block at `0x7E80_8000`: the power request and acknowledge start4 waits on before it resets the DWC2 USB controller

- Bus: `vpu` (VPU bus address)
- Base: `0x7E808000`
- Size: `0x100`

The block names itself `USBR` in every reserved word of its window, so the name is an identification and not a label. BCM2835 has its HDMI `HD` block at this address, but the BCM2711 does not: its HDMI `hd` range is `0x7EF20000`, which this board's device tree confirms and which answers a different tag. Registers run `+0x00` to `+0x20`; `+0x24` upwards is the tag. Only the USB power handshake is modelled. start4's HDMI driver also owns words of this window -- it writes `+0x2C` and read-modify-writes bit 30 of `+0x38`, both past the last readable register. A tag on read means the word is not readable, not that it is unimplemented: `GPSET` answers the GPIO block's tag and its writes drive pins all the same. So both are write-only registers of this block as far as anything here can tell, and the model answers the tag on a read of either. Nothing start4 does with them runs in any boot: both sit behind an HDMI mode set, which needs a display bring-up no scenario reaches (#142).

Sources:

- measured (high): Raspberry Pi 4B d03115, `/dev/mem` at `0xFE808000`, one 32-bit word at a time with each offset read after two different preceding offsets so a stale read would show: every word from `+0x24` to `+0xFC` answers `0x55534252` -- `"USBR"` big-endian -- the way `corectl` answers `INTE` and `mcsync` answers `MULT`. No offset in the window is undecoded.
- measured (high): Raspberry Pi 4B d03115: the same sweep of `0xFEF20000`, which this board's device tree gives as the HDMI `hd` range (`hdmi@7ef00700`, `reg-names` `hdmi dvp phy rm packet metadata csc cec hd intr2`), answers the tag `0x64767064` (`"dvpd"`) and holds `0x01010101` at two offsets -- the reset value the BCM2835 database gives for `HD_MAI_THR`. So the HDMI block is there, not here. Nothing in `/proc/iomem` claims `fe808000`.
- measured (high): Raspberry Pi 4B d03115, the nine registers `+0x00`..`+0x20` with Linux idle: `0x404A1023 0x0000090A 0x00000003 0 0 0 0x0008E38E 0 0`. After `vcmailbox 0x00028001 8 8 3 3` (USB on) only two move -- `+0x08` to `0x00000007` and `+0x20` to `0x00000003` -- and `vcmailbox ... 3 2` (USB off) moves nothing back, so both stick for the rest of the boot.
- decompile (high): `SET_POWER_STATE` USB handler `0x3ED89520` (BCM2711 branch at `0x3ED8955E`); the same request at `0x3EC607B0` and `0x3ECACB0C`
- decompile (medium): start4's HDMI driver reaches into this window at six offsets, all with the base `0x7E808000` or `0x7E808040` built into the instruction: `+0x0C` (`0x3ECE7952`, read-modify-write), `+0x14` (`0x3ECEAFA2`, `0x3ECEC260`, `0x3ED4BB30`), `+0x2C` (`0x3ECE9886`), `+0x38` (`0x3ECE36D8`, `0x3ECE7716`, `0x3ECE8B46`, `0x3ECE9A02`, `0x3ECE9C84`, `0x3ECE9DEC`), `+0x40` (`0x3ECE9B62`, `0x3ECE9B8C`) and `+0x68` (`0x3ECE5CC0`). The functions holding them are entries of the HDMI driver's operation table at `0xEDF842C`, which `.drivers` lists at `0xEE01890`, so they are this chip's driver and not a retained BCM2835 variant -- the pointer-indirect twin of each (`+0x108`, `+0x10C`) lives in a second table that nothing references. — _Which of these words is a register of this block and which is an address the shared HDMI code carries from an older chip is not settled; none of the accesses runs in a boot._

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x008` | [`CTRL`](#ctrl) | rw | 32 | 2, best high |
| `0x020` | [`STATUS`](#status) | r | 32 | 2, best high |
| `0x02C` | [`HDMI_SM_DIV`](#hdmi_sm_div) | w | 32 | 3, best high |
| `0x038` | [`HDMI_CTL`](#hdmi_ctl) | w | 32 | 2, best high |

## `CTRL`

Offset `0x008` · access `rw` · 32 bits · reset `0x3`

start4 sets `POWER` before it resets the DWC2 core. Nothing clears it again, not even the USB power-off path.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 2 | `POWER` | rw | Power request for the USB controller. `STATUS.ACK` follows it. |

Sources:

- measured (high): Raspberry Pi 4B d03115 (start4 f5e89631, Linux idle), 32-bit `/dev/mem` read of `0xFE808008`: `0x3` before any USB power request, `0x7` after `vcmailbox` `SET_POWER_STATE(USB, on|wait)`, still `0x7` after `SET_POWER_STATE(USB, off|wait)`
- decompile (high): `0x3ED89564`, `0x3EC607B6` and `0x3ECACB14` set bit 2; `0x3ECACB1A..0x3ECACB28` then clear bits 0 and 1

`POWER` sources:

- decompile (medium): `0x3ED89564..0x3ED8956E`: `[+0x08] |= 4`, spin until `[+0x20] & 3 == 3`, then reset the DWC2 core at `0x7E980000` — _The name is ours, from what the firmware does next._

## `STATUS`

Offset `0x020` · access `r` · 32 bits

Both `ACK` bits read 1 while `CTRL.POWER` is set and 0 while it is clear. The model sets them as soon as `POWER` is written.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 1:0 | `ACK` | r | Power acknowledge. |

Sources:

- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE808020`: `0x0` while `CTRL` read `0x3`, `0x3` while `CTRL` read `0x7` (the same three points as `CTRL`)
- decompile (high): `0x3ED8956A` and `0x3EC607BC` spin until `[+0x20] & 3 == 3` after setting `CTRL.POWER`, with no timeout; `0x3ECACB2A` spins until bit 0 is set

`ACK` sources:

- measured (high): Raspberry Pi 4B d03115: 0 without `CTRL.POWER`, 3 with it

## `HDMI_SM_DIV`

Offset `0x02C` · access `w` · 32 bits · reset `0x55534252`

start4's HDMI driver writes a divider derived from the HDMI state-machine clock: `<divider> << 8 | <mask>`, where the divider is the clock ratio rounded to a shift of at least 24 and the mask is the low bits that shift leaves. A read answers `0x55534252` -- `"USBR"`, the block's own tag -- which is what the `reset` here records. Plain storage in the model: the write has no read side and no observed effect, and no boot reaches it.

Sources:

- decompile (high): `0x3ECE9886` (`st r0,(r3+0x2c)` with `r3 = 0x7E808000`) in the state-machine-clock writer, and its pointer-indirect twin `FUN_0ECE98CC`, which stores through `*(param_1 + 0x108)`. The only caller is `FUN_0ECEAE00`, which logs `HDMI: Setting state machine clock to %d Hz`, and its two callers are `0x3ECE7908` (the default bring-up, 100 MHz, with a 25.2 MHz pixel clock beside it) and `0x3ECE9918` `set_mode_request` (`max(config, 163682864)` Hz) -- both entries of the HDMI operation table at `0xEDF842C`, reached only through a mode set.
- measured (high): Raspberry Pi 4B d03115, in the `/dev/mem` sweep of the window: `0xFE80802C` answers `0x55534252` like every other word from `+0x24` up, with Linux up and a boot that never set a mode
- trace (high): `RVF_TRACE_ON_PC=0x3ECEAE00 RVF_TRACE_CF=1` on a `boot --display` run with the full framebuffer request never arms, and no golden transcript under `testdata/boot/golden/` holds the `HDMI: Setting state machine clock` line; neither does the firmware message ring of a Raspberry Pi 4B d03115 booted headless on stock start4 `f5e89631`

## `HDMI_CTL`

Offset `0x038` · access `w` · 32 bits · reset `0x55534252`

start4's HDMI driver read-modify-writes bit 30 here around a mode set -- set on the way in, cleared on the way out -- and `set_mode_request` clears the whole word first. What bit 30 does is unknown. A read answers the block's tag, so a real board's read-modify-write puts the tag's bits back with it, the way the EEPROM bootloader's `GPSET` read does. Plain storage in the model, and no boot reaches it.

Sources:

- decompile (high): `0x3ECE7716` sets bit 30 (`ld r0,(r2+0x38); bitset r0,0x1e; st r0,(r2+0x38)` with `r2 = 0x7E808000`) and `0x3ECE36D8` clears it; `0x3ECE8B46`, `0x3ECE9C84` and `0x3ECE9DEC` do the same from other operations, and `0x3ECE9A02` in `set_mode_request` stores 0 over the word. Each has a pointer-indirect twin that goes through `*(param_1 + 0x10C)`.
- measured (high): Raspberry Pi 4B d03115, in the same `/dev/mem` sweep: `0xFE808038` answers `0x55534252`
