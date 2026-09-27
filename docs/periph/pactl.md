<!-- generated from specs/pactl.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pactl` – Peripheral activity status: which SPI, I²C or PL011 behind an ORed interrupt is the one asking

- Bus: `vpu` (VPU bus address)
- Base: `0x7E204E00`
- Size: `0x4`

The BCM2711 has more peripherals than VC peripheral IRQs, so all the SPI masters share one line, all the I²C masters another and all the PL011 UARTs a third. This one register says which member of each group has something pending: bits 0 to 6 the SPI masters, 8 to 15 the I²C masters, 16 to 20 the UARTs. SPI0 is the one master the model drives an interrupt from (`specs/spi0.toml`), so bit 0 answers its line and every other bit reads 0 — the honest answer for a machine where none of the rest has an interrupt pending.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §6.2.4: the per-peripheral statuses behind VC peripheral IRQs 53, 54 and 57 "can in turn be read from the `AUX_IRQ` … and `PACTL_CS` (at address `0x7E20 4E00`) registers", with Figure 6 for the bit positions

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CS`](#cs) | r | 32 | 1, best high |

## `CS`

Offset `0x000` · access `r` · 32 bits · reset `0x0`

One bit per peripheral, up while that peripheral's own interrupt is.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 6:0 | `SPI` | r | Bit `n` is SPI`n`, SPI0 to SPI6. Together they are VC peripheral IRQ 54, VPU source 118. |
| 15:8 | `I2C` | r | Bit `8 + n` is I2C`n`, I2C0 to I2C7. Together they are VC peripheral IRQ 53, VPU source 117. |
| 20:16 | `UART` | r | Bit 16 is UART5, 17 UART4, 18 UART3, 19 UART2 and 20 UART0 — the figure's order, not a run. Together they are VC peripheral IRQ 57, VPU source 121. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Figure 6

`SPI` sources:

- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Figure 6

`I2C` sources:

- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Figure 6

`UART` sources:

- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Figure 6
