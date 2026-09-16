<!-- generated from specs/hdmi_auto_i2c.toml by `cargo run -- spec-docs --update` – do not edit -->

# `hdmi_auto_i2c` – HDMI auto-i2c sequencers (the second reg window of each DDC master's node): channels that write a list of values into their connector's DDC I²C master and report when the transfer it starts has finished

- Bus: `vpu` (VPU bus address)
- Base: `0x7EF00B00`
- `HDMI1` copy: `0x7EF05B00`
- Size: `0x300`

Only what start4 1.20190925, 1.20200212 and 1.20200601 use is modelled, from their accesses: channel 2's list, START, CLEAR and DONE. They run channel 2 once at boot and wait for its DONE bit with no timeout, so on the catch-all stub (which reads 0) they never started the ARM. Later builds leave the block alone. Everything else in the window is stored and read back.

Sources:

- measured (high): Raspberry Pi 4B d03115 /proc/device-tree/soc/i2c@7ef04500: reg 0x7ef04500 0x100 0x7ef00b00 0x300
- trace (high): start4 1.20190925 on a B0 (RVF_TRACE_MMIO=0x7ef00b00-0x7ef00e00): zeroes 0x130..0x1A0, writes 4 to 0x264, a (0x100 | n, value) list from 0x134 and 0x180C0005 to 0x130, then 4 to 0x260, then polls 0x268 for bit 2

`HDMI1` copy:

HDMI1's sequencers; HDMI0's are the block base.

- decompile (high): start4 1.20190925 takes 0x7EF00B00 or 0x7EF05B00 as the base by connector (0x0ED11486 / 0x0ED11490)

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x130` | [`CTL2`](#ctl2) | rw | 32 | 1, best high |
| `0x134`–`0x1A0` (28 × 0x4) | [`LIST2`](#list2) | rw | 32 | 2, best high |
| `0x260` | [`START`](#start) | w | 32 | 1, best medium |
| `0x264` | [`CLEAR`](#clear) | w | 32 | 1, best low |
| `0x268` | [`DONE`](#done) | r | 32 | 1, best high |

## `CTL2`

Offset `0x130` · access `rw` · 32 bits

Channel 2's control word. start4 writes 0x180C0005 before it starts the channel; the fields are unknown. Stored.

Sources:

- trace (high): start4 1.20190925: 0x7EF00C30 <- 0x180C0005

## `LIST2`

Offset `0x134`, 28 elements 0x4 apart · access `rw` · 32 bits

Channel 2's command list, as (command, value) pairs. A command 0x100 | n writes the value to the DDC master's register at 4 × n. No other command is known, so anything else, the zeroed tail of the list included, ends it. start4's one list sets up and starts a two-byte write: IIC_ENABLE 0, CHIP_ADDRESS 0x60, CTL 0xD0, CNT 2, CTLHI 0x40, DATA_IN 0, IIC_ENABLE 1.

Sources:

- trace (high): start4 1.20190925: (0x10B, 0) (0x100, 0x60) (0x10A, 0xD0) (0x109, 2) (0x114, 0x40) (0x101, 0) (0x10B, 1) at 0x7EF00C34..0x7EF00C68, after zeroing the list
- inferred (medium): the low byte of each command is a word index into the DDC master's registers, i2c-brcmstb.c struct bsc_regs: 0x00 chip_address, 0x01 data_in[0], 0x09 cnt_reg, 0x0A ctl_reg, 0x0B iic_enable, 0x14 ctlhi_reg

## `START`

Offset `0x260` · access `w` · 32 bits

Write 1 << n to run channel n's list. Only channel 2's list location is known; any other channel reports done at once.

Sources:

- trace (medium): start4 1.20190925: 0x7EF00D60 <- 4 once the list is written, then it waits on DONE bit 2

## `CLEAR`

Offset `0x264` · access `w` · 32 bits

Write 1 << n to clear channel n's DONE bit. start4 writes it before it programs the channel. That it clears DONE is a guess; it could as well be an interrupt enable.

Sources:

- trace (low): start4 1.20190925: 0x7EF00D64 <- 4 before programming channel 2

## `DONE`

Offset `0x268` · access `r` · 32 bits

Bit n: channel n's list has run and the transfer it started has finished. start4 waits for its channel's bit with no timeout.

Sources:

- trace (high): start4 1.20190925 at 0x3ED114D4: ld r3, (r2+0x268); cmp r3, 0; btest r3, 2, looping until set
