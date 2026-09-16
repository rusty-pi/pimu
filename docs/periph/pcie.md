<!-- generated from specs/pcie.toml by `cargo run -- spec-docs --update` – do not edit -->

# `pcie` – PCIe root complex (`pcie-brcmstb`), with the VL805 xHCI controller behind it

- Bus: `vpu` (VPU bus address)
- Base: `0x7D500000`
- Size: `0x9310`

`+0x0000..+0x0FFF` is the root port's own configuration space (seeded from a Raspberry Pi 4B d03115; its layout is PCI's, not listed here beyond the two words with behaviour). The VPU reaches the endpoint's BAR0 only by 40-bit DMA through the outbound window; the endpoint's DMA comes back through inbound window 2.

Sources:

- measured (high): `/proc/device-tree/scb/pcie@7d500000/reg` on a real board: `<0x0 0x7d500000 0x0 0x9310>`
- linux (high): `drivers/pci/controller/pcie-brcmstb.c`
- decompile (high): bootcode `0x8000AB4A`; second-stage bootloader `pcie_reset` `0x000A7034`, `pcie_init` `0x000A6CA2` / `0x000A6DCC`, link poll `0x000A6F7E`, bus scan `0x000A712C`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x0BC` | [`RC_LNKCTL`](#rc_lnkctl) | rw | 32 | 2, best high |
| `0x43C` | [`PRIV1_ID_VAL3`](#priv1_id_val3) | rw | 32 | 3, best high |
| `0x1100` | [`MDIO_ADDR`](#mdio_addr) | rw | 32 | 1, best high |
| `0x1104` | [`MDIO_WR_DATA`](#mdio_wr_data) | rw | 32 | 1, best high |
| `0x1108` | [`MDIO_RD_DATA`](#mdio_rd_data) | r | 32 | 1, best high |
| `0x4008` | [`MISC_CTRL`](#misc_ctrl) | rw | 32 | 1, best high |
| `0x400C` | [`MEM_WIN0_LO`](#mem_win0_lo) | rw | 32 | 2, best high |
| `0x4010` | [`MEM_WIN0_HI`](#mem_win0_hi) | rw | 32 | 1, best high |
| `0x402C` | [`RC_BAR1_CONFIG_LO`](#rc_bar1_config_lo) | rw | 32 | 2, best high |
| `0x4034` | [`RC_BAR2_CONFIG_LO`](#rc_bar2_config_lo) | rw | 32 | 1, best high |
| `0x4038` | [`RC_BAR2_CONFIG_HI`](#rc_bar2_config_hi) | rw | 32 | 1, best high |
| `0x403C` | [`RC_BAR3_CONFIG_LO`](#rc_bar3_config_lo) | rw | 32 | 2, best high |
| `0x4044` | [`MSI_BAR_CONFIG_LO`](#msi_bar_config_lo) | rw | 32 | 1, best high |
| `0x4048` | [`MSI_BAR_CONFIG_HI`](#msi_bar_config_hi) | rw | 32 | 1, best high |
| `0x404C` | [`MSI_DATA_CONFIG`](#msi_data_config) | rw | 32 | 1, best high |
| `0x4068` | [`MISC_PCIE_STATUS`](#misc_pcie_status) | r | 32 | 2, best high |
| `0x406C` | [`MISC_REVISION`](#misc_revision) | r | 32 | 2, best high |
| `0x4070` | [`MEM_WIN0_BASE_LIMIT`](#mem_win0_base_limit) | rw | 32 | 2, best high |
| `0x4080` | [`MEM_WIN0_BASE_HI`](#mem_win0_base_hi) | rw | 32 | 2, best high |
| `0x4084` | [`MEM_WIN0_LIMIT_HI`](#mem_win0_limit_hi) | rw | 32 | 2, best high |
| `0x4204` | [`HARD_DEBUG`](#hard_debug) | rw | 32 | 1, best high |
| `0x4300` | [`INTR2_CPU_STATUS`](#intr2_cpu_status) | rw | 32 | 1, best high |
| `0x4304` | [`INTR2_CPU_SET`](#intr2_cpu_set) | rw | 32 | 1, best medium |
| `0x4308` | [`INTR2_CPU_CLR`](#intr2_cpu_clr) | rw | 32 | 1, best medium |
| `0x430C` | [`INTR2_CPU_MASK_STATUS`](#intr2_cpu_mask_status) | rw | 32 | 1, best medium |
| `0x4310` | [`INTR2_CPU_MASK_SET`](#intr2_cpu_mask_set) | rw | 32 | 1, best medium |
| `0x4314` | [`INTR2_CPU_MASK_CLR`](#intr2_cpu_mask_clr) | rw | 32 | 1, best medium |
| `0x4500` | [`MSI_INTR2_STATUS`](#msi_intr2_status) | r | 32 | 1, best high |
| `0x4504` | [`MSI_INTR2_SET`](#msi_intr2_set) | w | 32 | 1, best high |
| `0x4508` | [`MSI_INTR2_CLR`](#msi_intr2_clr) | w | 32 | 1, best high |
| `0x450C` | [`MSI_INTR2_MASK_STATUS`](#msi_intr2_mask_status) | r | 32 | 1, best high |
| `0x4510` | [`MSI_INTR2_MASK_SET`](#msi_intr2_mask_set) | w | 32 | 1, best high |
| `0x4514` | [`MSI_INTR2_MASK_CLR`](#msi_intr2_mask_clr) | w | 32 | 1, best high |
| `0x8000`–`0x8FFC` (1024 × 0x4) | [`EXT_CFG_DATA`](#ext_cfg_data) | rw | 32 | 2, best high |
| `0x9000` | [`EXT_CFG_INDEX`](#ext_cfg_index) | rw | 32 | 1, best high |
| `0x9210` | [`RGR1_SW_INIT_1`](#rgr1_sw_init_1) | rw | 32 | 2, best high |

## `RC_LNKCTL`

Offset `0x0BC` · access `rw` · 32 bits

Root port link control; link status in the top half (5 GT/s x1 with slot clock once the link is up).

Sources:

- linux (high): `pcie-brcmstb.c`: `BRCM_PCIE_CAP_REGS` (`0xAC`) + `PCI_EXP_LNKCTL`
- measured (high): Raspberry Pi 4B d03115 `lspci`: `LnkSta: Speed 5GT/s, Width x1, SlotClk+`

## `PRIV1_ID_VAL3`

Offset `0x43C` · access `rw` · 32 bits · reset `0x20060400`

Revision (top byte) and class code; the header word at `+0x08` is a view of it, which is how Linux turns the block into a PCI-to-PCI bridge.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_RC_CFG_PRIV1_ID_VAL3`
- measured (high): Raspberry Pi 4B d03115 `/sys/bus/pci/devices/0000:00:00.0/config` at `0x43C`
- decompile (high): the bootloader writes it at `0x000A6E20`

## `MDIO_ADDR`

Offset `0x1100` · access `rw` · 32 bits

SerDes MDIO command packet: register address and direction.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 15:0 | `REGAD` | rw | SerDes register; `0x1F` selects the block the others address. |
| 20 | `CMD_READ` | rw | Read, rather than write. |

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_RC_DL_MDIO_ADDR`, `brcm_pcie_mdio_form_pkt`

`REGAD` sources:

- linux (high): `pcie-brcmstb.c`: `MDIO_REGAD`

`CMD_READ` sources:

- linux (high): `pcie-brcmstb.c`: `MDIO_CMD_READ << 20`

## `MDIO_WR_DATA`

Offset `0x1104` · access `rw` · 32 bits

SerDes MDIO write data.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 31 | `DONE` | rw | Set by the host to start a write; clear once it is done. |

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_RC_DL_MDIO_WR_DATA`

`DONE` sources:

- linux (high): `pcie-brcmstb.c`: `MDIO_DATA_DONE_MASK`

## `MDIO_RD_DATA`

Offset `0x1108` · access `r` · 32 bits

SerDes MDIO read data.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 31 | `DONE` | r | The read has completed. |

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_RC_DL_MDIO_RD_DATA`

`DONE` sources:

- linux (high): `pcie-brcmstb.c`: `MDIO_DATA_DONE_MASK`

## `MISC_CTRL`

Offset `0x4008` · access `rw` · 32 bits

Miscellaneous control. Stored.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_MISC_CTRL`

## `MEM_WIN0_LO`

Offset `0x400C` · access `rw` · 32 bits

PCI bus address of the outbound window, low word; the bootloader writes `0x80000000`.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_CPU_2_PCIE_MEM_WIN0_LO`
- decompile (high): `0x000A725C`

## `MEM_WIN0_HI`

Offset `0x4010` · access `rw` · 32 bits

PCI bus address of the outbound window, high word.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_CPU_2_PCIE_MEM_WIN0_HI`

## `RC_BAR1_CONFIG_LO`

Offset `0x402C` · access `rw` · 32 bits

Inbound window 1; switched off by the bootloader and Linux. Stored.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_RC_BAR1_CONFIG_LO`
- decompile (high): the bootloader clears it at `0x000A6D60`

## `RC_BAR2_CONFIG_LO`

Offset `0x4034` · access `rw` · 32 bits

Inbound window 2, the endpoint's path to system memory: size code in the low bits, PCI bus base above. Its CPU side is hard-wired to physical 0.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 4:0 | `SIZE` | rw | `1..0x15` = 64 KiB..64 GiB, `0x1C..0x1F` = 4..32 KiB, anything else off. |

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_RC_BAR2_CONFIG_LO`, `brcm_pcie_encode_ibar_size()`; `brcm_pcie_get_inbound_wins()`: 'the BAR2 cpu_addr is hardwired to the start of system memory'

`SIZE` sources:

- linux (high): `pcie-brcmstb.c`: `brcm_pcie_encode_ibar_size()`

## `RC_BAR2_CONFIG_HI`

Offset `0x4038` · access `rw` · 32 bits

Inbound window 2 PCI bus base, high word.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_RC_BAR2_CONFIG_HI`

## `RC_BAR3_CONFIG_LO`

Offset `0x403C` · access `rw` · 32 bits

Inbound window 3; switched off. Stored.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_RC_BAR3_CONFIG_LO`
- decompile (high): the bootloader clears it at `0x000A6D70`

## `MSI_BAR_CONFIG_LO`

Offset `0x4044` · access `rw` · 32 bits

PCI bus address MSI writes are caught at; bit 0 is the enable.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_MSI_BAR_CONFIG_LO`

## `MSI_BAR_CONFIG_HI`

Offset `0x4048` · access `rw` · 32 bits

MSI catch address, high word.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_MSI_BAR_CONFIG_HI`

## `MSI_DATA_CONFIG`

Offset `0x404C` · access `rw` · 32 bits

Match mask (top half) and pattern (bottom half) for MSI data; Linux writes `0xFFE06540`, the low five bits pick the vector.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_MSI_DATA_CONFIG`

## `MISC_PCIE_STATUS`

Offset `0x4068` · access `r` · 32 bits

Link state. A live link reads `0xB0`; with no endpoint only the port-mode strap.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 4 | `PHYLINKUP` | r | PHY link up. |
| 5 | `DL_ACTIVE` | r | Data link active. |
| 7 | `PORT_RC` | r | Strapped as a root complex, whatever the link does. |

Sources:

- linux (high): `pcie-brcmstb.c`: `brcm_pcie_link_up()` and `brcm_pcie_rc_mode()`
- decompile (high): link-up predicate `0x000A6F7E`; `PCIe timeout: 0x%08x` prints the whole word

`PHYLINKUP` sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_PCIE_STATUS_PCIE_PHYLINKUP_MASK`

`DL_ACTIVE` sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_PCIE_STATUS_PCIE_DL_ACTIVE_MASK`

`PORT_RC` sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_PCIE_STATUS_PCIE_PORT_MASK`; `PCIe RC controller misconfigured as Endpoint` when clear

## `MISC_REVISION`

Offset `0x406C` · access `r` · 32 bits · reset `0x303`

Hardware revision. Linux picks the 32-vector MSI block at `+0x4500` from 3.3 on.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_REVISION`, `BRCM_PCIE_HW_REV_33`
- measured (medium): the MSI domain on a Raspberry Pi 4B d03115 is 32 wide (`/sys/kernel/debug/irq/domains/unknown-1`: `size: 32`), so at least 3.3 — _the exact revision was not read; 3.3 is the lower bound_

## `MEM_WIN0_BASE_LIMIT`

Offset `0x4070` · access `rw` · 32 bits

CPU-side extent of the outbound window in MiB: base in 15:4, limit in 31:20. The bootloader writes `0x3FF00000`.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_CPU_2_PCIE_MEM_WIN0_BASE_LIMIT`
- decompile (high): `0x000A72C6`

## `MEM_WIN0_BASE_HI`

Offset `0x4080` · access `rw` · 32 bits

CPU-side base above bit 31; written 6, putting the window at `0x6_0000_0000`.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_CPU_2_PCIE_MEM_WIN0_BASE_HI`
- decompile (high): `0x000A72DE`

## `MEM_WIN0_LIMIT_HI`

Offset `0x4084` · access `rw` · 32 bits

CPU-side limit above bit 31; written 6.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_CPU_2_PCIE_MEM_WIN0_LIMIT_HI`
- decompile (high): `0x000A72F0`

## `HARD_DEBUG`

Offset `0x4204` · access `rw` · 32 bits

PCIe hard-debug control. Stored.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MISC_HARD_PCIE_HARD_DEBUG`

## `INTR2_CPU_STATUS`

Offset `0x4300` · access `rw` · 32 bits

Root-complex level-2 interrupt controller (legacy MSI block). Stored.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_INTR2_CPU_BASE`

## `INTR2_CPU_SET`

Offset `0x4304` · access `rw` · 32 bits

Set status bits. Stored.

Sources:

- linux (medium): `pcie-brcmstb.c`: `PCIE_INTR2_CPU_BASE + 0x4`

## `INTR2_CPU_CLR`

Offset `0x4308` · access `rw` · 32 bits

Clear status bits. Stored.

Sources:

- linux (medium): `pcie-brcmstb.c`: `PCIE_INTR2_CPU_BASE + 0x8`

## `INTR2_CPU_MASK_STATUS`

Offset `0x430C` · access `rw` · 32 bits

Mask. Stored.

Sources:

- linux (medium): `pcie-brcmstb.c`: `PCIE_INTR2_CPU_BASE + 0xC`

## `INTR2_CPU_MASK_SET`

Offset `0x4310` · access `rw` · 32 bits

Mask bits. Stored.

Sources:

- linux (medium): `pcie-brcmstb.c`: `PCIE_INTR2_CPU_BASE + 0x10`

## `INTR2_CPU_MASK_CLR`

Offset `0x4314` · access `rw` · 32 bits

Unmask bits. Stored.

Sources:

- linux (medium): `pcie-brcmstb.c`: `PCIE_INTR2_CPU_BASE + 0x14`

## `MSI_INTR2_STATUS`

Offset `0x4500` · access `r` · 32 bits

MSI vectors pending; a caught MSI write sets its bit and drives GIC SPI 148 while unmasked.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MSI_INTR2_BASE`, status at `+0x0`

## `MSI_INTR2_SET`

Offset `0x4504` · access `w` · 32 bits

Set pending bits.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MSI_INTR2_BASE + 0x4`

## `MSI_INTR2_CLR`

Offset `0x4508` · access `w` · 32 bits

Clear pending bits.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MSI_INTR2_BASE + 0x8`

## `MSI_INTR2_MASK_STATUS`

Offset `0x450C` · access `r` · 32 bits

Mask.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MSI_INTR2_BASE + 0xC`

## `MSI_INTR2_MASK_SET`

Offset `0x4510` · access `w` · 32 bits

Mask vectors.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MSI_INTR2_BASE + 0x10`

## `MSI_INTR2_MASK_CLR`

Offset `0x4514` · access `w` · 32 bits

Unmask vectors.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_MSI_INTR2_BASE + 0x14`

## `EXT_CFG_DATA`

Offset `0x8000`, 1024 elements 0x4 apart · access `rw` · 32 bits

4 KiB view of the configuration space of the function `EXT_CFG_INDEX` selects. Bus 1 device 0 is the VL805; everything else, bus 0 included, reads all-ones.

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_EXT_CFG_DATA`; `brcm_pcie_map_conf()` sends the root bus to `base + where` instead
- measured (high): `examples-on-real-hardware/sd-card-boot.log`: the bus scan prints only `00001106:00003483`

## `EXT_CFG_INDEX`

Offset `0x9000` · access `rw` · 32 bits

Selects the function `EXT_CFG_DATA` shows.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 14:12 | `FUNC` | rw | Function. |
| 19:15 | `SLOT` | rw | Device. |
| 27:20 | `BUSNUM` | rw | Bus. |

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_EXT_CFG_INDEX`, `bus << 20 | slot << 15 | fn << 12`

`FUNC` sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_EXT_FUNC_SHIFT`

`SLOT` sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_EXT_SLOT_SHIFT`

`BUSNUM` sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_EXT_BUSNUM_SHIFT`

## `RGR1_SW_INIT_1`

Offset `0x9210` · access `rw` · 32 bits

Resets. Releasing PERST# with the endpoint present trains the link on the spot; asserting it resets the endpoint.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `PERST` | rw | PERST# asserted. |
| 1 | `INIT` | rw | Bridge soft reset. |

Sources:

- linux (high): `pcie-brcmstb.c`: `PCIE_RGR1_SW_INIT_1`
- decompile (high): bootcode `0x8000AB4A` writes 2 then 3; `pcie_init` releases the bridge at `0x000A6CA2` and PERST# at `0x000A6DCC`

`PERST` sources:

- linux (high): `pcie-brcmstb.c`: `RGR1_SW_INIT_1_PERST_MASK`

`INIT` sources:

- linux (high): `pcie-brcmstb.c`: `RGR1_SW_INIT_1_INIT_GENERIC_MASK`
