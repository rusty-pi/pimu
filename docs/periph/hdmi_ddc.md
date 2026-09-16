<!-- generated from specs/hdmi_ddc.toml by `cargo run -- spec-docs --update` – do not edit -->

# `hdmi_ddc` – HDMI DDC I²C masters (`brcm,bcm2711-hdmi-i2c`), one per connector: the bus a monitor's EDID EEPROM sits on

- Bus: `vpu` (VPU bus address)
- Base: `0x7EF04500`
- `HDMI1` copy: `0x7EF09500`
- Size: `0x100`

Not the BSC: a different block with a different layout. With no monitor attached (the reference board's state) every transfer completes `INTRP | NOACK`. The second reg window in the device tree, the auto-i2c block at `0x7EF00B00`, is its own block (`hdmi_auto_i2c`); the same device answers both.

Sources:

- measured (high): Raspberry Pi 4B d03115 `/proc/device-tree/soc/i2c@7ef04500`: `compatible brcm,bcm2711-hdmi-i2c`, `reg 0x7ef04500 0x100 0x7ef00b00 0x300`, `clock-frequency 97500`
- linux (high): `drivers/i2c/busses/i2c-brcmstb.c`: `struct bsc_regs`
- decompile (high): start4's driver: read `0x3ECE69E2`, write `0x3ECE6DEC`, accessors `0x3ECE6B00` / `0x3ECE6EC8`, completion wait `0x3ECE6D5C`

`HDMI1` copy:

HDMI1's DDC master; HDMI0's is the block base.

- linux (high): dtb node `i2c@7ef09500`, `brcm,bcm2711-hdmi-i2c`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CHIP_ADDRESS`](#chip_address) | rw | 32 | 1, best high |
| `0x004`–`0x020` (8 × 0x4) | [`DATA_IN`](#data_in) | rw | 32 | 1, best high |
| `0x024` | [`CNT`](#cnt) | rw | 32 | 1, best high |
| `0x028` | [`CTL`](#ctl) | rw | 32 | 1, best high |
| `0x02C` | [`IIC_ENABLE`](#iic_enable) | rw | 32 | 3, best high |
| `0x030`–`0x04C` (8 × 0x4) | [`DATA_OUT`](#data_out) | r | 32 | 1, best high |
| `0x050` | [`CTLHI`](#ctlhi) | rw | 32 | 1, best high |
| `0x054` | [`SCL_PARAM`](#scl_param) | rw | 32 | 1, best high |

## `CHIP_ADDRESS`

Offset `0x000` · access `rw` · 32 bits

Slave address, already shifted: `addr << 1 | read`.

Sources:

- linux (high): `i2c-brcmstb.c`: `bsc_regs.chip_address`

## `DATA_IN`

Offset `0x004`, 8 elements 0x4 apart · access `rw` · 32 bits

Bytes to send, packed little-endian per word.

Sources:

- linux (high): `i2c-brcmstb.c`: `bsc_regs.data_in[8]`

## `CNT`

Offset `0x024` · access `rw` · 32 bits

Byte count.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 5:0 | `CNT1` | rw | Bytes in this transfer. |

Sources:

- linux (high): `i2c-brcmstb.c`: `bsc_regs.cnt_reg`

`CNT1` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_CNT_REG1_MASK`

## `CTL`

Offset `0x028` · access `rw` · 32 bits

Transfer control.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 1:0 | `DTF` | rw | Data transfer format; bit 0 set is a read. |
| 5:4 | `SCL_SEL` | rw | Bus clock select. Stored. |
| 6 | `INT_EN` | rw | Interrupt enable. |
| 7 | `DIV_CLK` | rw | Clock divide. Stored. |

Sources:

- linux (high): `i2c-brcmstb.c`: `bsc_regs.ctl_reg`

`DTF` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_CTL_REG_DTF_MASK`

`SCL_SEL` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_CTL_REG_SCL_SEL_MASK`

`INT_EN` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_CTL_REG_INT_EN_MASK`

`DIV_CLK` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_CTL_REG_DIV_CLK_MASK`

## `IIC_ENABLE`

Offset `0x02C` · access `rw` · 32 bits

Writing `ENABLE` starts a transfer; `INTRP` and `NOACK` are computed status. start4 runs a transfer as `CTL` `0xD0`, `CTLHI` `0x40`, a read of the core's hotplug word, `CHIP_ADDRESS`, `CTLHI |= 0x40`, `CNT`, `DATA_IN`, `CTL` direction bits set, then this register `|= 0x53`; after the polls and one more status read it writes `CTL` `0x90`, `CNT` `0` and `0` here.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `ENABLE` | rw | Run the transfer. |
| 1 | `INTRP` | rw | Transfer finished (an interrupt enable when written). |
| 2 | `NOACK` | r | The slave never acknowledged its address. |
| 4 | `NOSTOP` | rw | Hold the bus after the transfer. |
| 5 | `NOSTART` | rw | Continue without a start condition. |
| 6 | `RESTART` | rw | Repeated start. |

Sources:

- linux (high): `i2c-brcmstb.c`: `bsc_regs.iic_enable`
- decompile (high): `0x3ECE6D5C`: up to 20 polls of `read(0x2c) & 2` 5 ms apart ('timed out'), then `read(0x2c) & 4` ('no ACK')
- trace (high): pinned start4, HDMI0 with nothing attached: `0x7EF04528` `0xD0`, `0x7EF04550` `0x40`, `0x7EF008A8` read, `0x7EF04500` `0xA0`, `0x7EF04550` `0x40`, `0x7EF04524` `1`, `0x7EF04504` `0`, `0x7EF04528` `0xD0`, `0x53` here (reads `0x51`, `0x57`, `0x57`), then `0x7EF04528` `0x90`, `0x7EF04524` `0`, `0` here; all written at `0x3ECE6ED0`

`ENABLE` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_IIC_EN_ENABLE_MASK`

`INTRP` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_IIC_EN_INTRP_MASK`

`NOACK` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_IIC_EN_NOACK_MASK`

`NOSTOP` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_IIC_EN_NOSTOP_MASK`

`NOSTART` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_IIC_EN_NOSTART_MASK`

`RESTART` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_IIC_EN_RESTART_MASK`

## `DATA_OUT`

Offset `0x030`, 8 elements 0x4 apart · access `r` · 32 bits

Bytes received, packed as `DATA_IN`.

Sources:

- linux (high): `i2c-brcmstb.c`: `bsc_regs.data_out[8]`

## `CTLHI`

Offset `0x050` · access `rw` · 32 bits

More transfer control.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `WAIT_DIS` | rw | No clock stretching. |
| 1 | `IGNORE_ACK` | rw | Carry on regardless of ACK; `NOACK` is then not reported. |
| 6 | `DATAREG_SIZE` | rw | Data registers are 32 bits wide. |

Sources:

- linux (high): `i2c-brcmstb.c`: `bsc_regs.ctlhi_reg`

`WAIT_DIS` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_CTLHI_REG_WAIT_DIS_MASK`

`IGNORE_ACK` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_CTLHI_REG_IGNORE_ACK_MASK`

`DATAREG_SIZE` sources:

- linux (high): `i2c-brcmstb.c`: `BSC_CTLHI_REG_DATAREG_SIZE_MASK`

## `SCL_PARAM`

Offset `0x054` · access `rw` · 32 bits

Bus timing. Stored, otherwise ignored.

Sources:

- linux (high): `i2c-brcmstb.c`: `bsc_regs.scl_param`
