<!-- generated from specs/genet.toml by `cargo run -- spec-docs --update` – do not edit -->

# `genet` – GENET v5 Ethernet MAC with its UniMAC MDIO controller (the PHY is `bcm54213pe`)

- Bus: `vpu` (VPU bus address)
- Base: `0x7D580000`
- Size: `0x10000`

GENET v4/v5 layout. Each DMA direction has 256 three-word descriptors and 17 ring register blocks `0x40` apart; ring 16 is the default ring. Reset values below are the measured values of registers no client writes on the reference board's boot path.

Sources:

- linux (high): `/scb/ethernet@7d580000`, `reg = <0x7d580000 0x10000>`; `drivers/net/ethernet/broadcom/genet/bcmgenet.h`, `drivers/net/mdio/mdio-bcm-unimac.c`
- decompile (high): EEPROM bootloader network boot (disassembled LZ4 BOOTLOADER stage); start4 MDIO `FUN_0ecc3198` / `FUN_0ecc3280`, UMAC start `FUN_0ecc3b6c`, ring 16 setup `FUN_0ecc2d54` / `FUN_0ecc2e4c`
- measured (high): read-only `/dev/mem` reads of named registers on a Pi 4B rev 1.5 running Linux 6.12, link up

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`SYS_REV_CTRL`](#sys_rev_ctrl) | r | 32 | 2, best high |
| `0x004` | [`SYS_PORT_CTRL`](#sys_port_ctrl) | rw | 32 | 1, best high |
| `0x008` | [`SYS_RBUF_FLUSH_CTRL`](#sys_rbuf_flush_ctrl) | rw | 32 | 1, best high |
| `0x00C` | [`SYS_TBUF_FLUSH_CTRL`](#sys_tbuf_flush_ctrl) | rw | 32 | 1, best high |
| `0x080` | [`EXT_PWR_MGMT`](#ext_pwr_mgmt) | rw | 32 | 1, best high |
| `0x08C` | [`EXT_RGMII_OOB_CTRL`](#ext_rgmii_oob_ctrl) | rw | 32 | 1, best high |
| `0x09C` | [`EXT_GPHY_CTRL`](#ext_gphy_ctrl) | rw | 32 | 1, best high |
| `0x200`–`0x240` (2 × 0x40) | [`INTRL2_CPU_STAT`](#intrl2_cpu_stat) | r | 32 | 1, best high |
| `0x204`–`0x244` (2 × 0x40) | [`INTRL2_CPU_SET`](#intrl2_cpu_set) | w | 32 | 1, best high |
| `0x208`–`0x248` (2 × 0x40) | [`INTRL2_CPU_CLEAR`](#intrl2_cpu_clear) | w | 32 | 1, best high |
| `0x20C`–`0x24C` (2 × 0x40) | [`INTRL2_CPU_MASK_STATUS`](#intrl2_cpu_mask_status) | r | 32 | 1, best high |
| `0x210`–`0x250` (2 × 0x40) | [`INTRL2_CPU_MASK_SET`](#intrl2_cpu_mask_set) | w | 32 | 1, best high |
| `0x214`–`0x254` (2 × 0x40) | [`INTRL2_CPU_MASK_CLEAR`](#intrl2_cpu_mask_clear) | w | 32 | 1, best high |
| `0x300` | [`RBUF_CTRL`](#rbuf_ctrl) | rw | 32 | 1, best high |
| `0x314` | [`RBUF_CHK_CTRL`](#rbuf_chk_ctrl) | rw | 32 | 1, best high |
| `0x3B4` | [`RBUF_TBUF_SIZE_CTRL`](#rbuf_tbuf_size_ctrl) | rw | 32 | 1, best high |
| `0x600` | [`TBUF_CTRL`](#tbuf_ctrl) | rw | 32 | 1, best high |
| `0x60C` | [`TBUF_BP_MC`](#tbuf_bp_mc) | rw | 32 | 1, best high |
| `0x804` | [`UMAC_HD_BKP_CTRL`](#umac_hd_bkp_ctrl) | rw | 32 | 1, best high |
| `0x808` | [`UMAC_CMD`](#umac_cmd) | rw | 32 | 2, best high |
| `0x80C` | [`UMAC_MAC0`](#umac_mac0) | rw | 32 | 1, best high |
| `0x810` | [`UMAC_MAC1`](#umac_mac1) | rw | 32 | 1, best high |
| `0x814` | [`UMAC_MAX_FRAME_LEN`](#umac_max_frame_len) | rw | 32 | 1, best high |
| `0x818` | [`UMAC_PAUSE_QUANTA`](#umac_pause_quanta) | rw | 32 | 1, best high |
| `0x844` | [`UMAC_MODE`](#umac_mode) | r | 32 | 2, best high |
| `0x85C` | [`UMAC_TX_IPG_LEN`](#umac_tx_ipg_len) | rw | 32 | 1, best high |
| `0x864` | [`UMAC_EEE_CTRL`](#umac_eee_ctrl) | rw | 32 | 1, best high |
| `0xC00`–`0xD7C` (96 × 0x4) | [`UMAC_MIB`](#umac_mib) | r | 32 | 1, best high |
| `0xD80` | [`UMAC_MIB_CTRL`](#umac_mib_ctrl) | rw | 32 | 1, best high |
| `0xE14` | [`UMAC_MDIO_CMD`](#umac_mdio_cmd) | rw | 32 | 2, best high |
| `0xE18` | [`UMAC_MDIO_CFG`](#umac_mdio_cfg) | rw | 32 | 1, best high |
| `0x2000`–`0x2BFC` (768 × 0x4) | [`RDMA_DESC`](#rdma_desc) | rw | 32 | 1, best high |
| `0x2C00`–`0x3000` (17 × 0x40) | [`RDMA_RING_WRITE_PTR`](#rdma_ring_write_ptr) | rw | 32 | 1, best high |
| `0x2C08`–`0x3008` (17 × 0x40) | [`RDMA_RING_PROD_INDEX`](#rdma_ring_prod_index) | rw | 32 | 1, best high |
| `0x2C0C`–`0x300C` (17 × 0x40) | [`RDMA_RING_CONS_INDEX`](#rdma_ring_cons_index) | rw | 32 | 1, best high |
| `0x2C10`–`0x3010` (17 × 0x40) | [`RDMA_RING_BUF_SIZE`](#rdma_ring_buf_size) | rw | 32 | 1, best high |
| `0x2C14`–`0x3014` (17 × 0x40) | [`RDMA_RING_START_ADDR`](#rdma_ring_start_addr) | rw | 32 | 1, best high |
| `0x2C1C`–`0x301C` (17 × 0x40) | [`RDMA_RING_END_ADDR`](#rdma_ring_end_addr) | rw | 32 | 1, best high |
| `0x3040` | [`RDMA_RING_CFG`](#rdma_ring_cfg) | rw | 32 | 1, best high |
| `0x3044` | [`RDMA_CTRL`](#rdma_ctrl) | rw | 32 | 2, best high |
| `0x3048` | [`RDMA_STATUS`](#rdma_status) | r | 32 | 2, best high |
| `0x30B0`–`0x30CC` (8 × 0x4) | [`RDMA_INDEX2RING`](#rdma_index2ring) | rw | 32 | 1, best high |
| `0x4000`–`0x4BFC` (768 × 0x4) | [`TDMA_DESC`](#tdma_desc) | rw | 32 | 1, best high |
| `0x4C00`–`0x5000` (17 × 0x40) | [`TDMA_RING_READ_PTR`](#tdma_ring_read_ptr) | rw | 32 | 1, best high |
| `0x4C08`–`0x5008` (17 × 0x40) | [`TDMA_RING_CONS_INDEX`](#tdma_ring_cons_index) | rw | 32 | 1, best high |
| `0x4C0C`–`0x500C` (17 × 0x40) | [`TDMA_RING_PROD_INDEX`](#tdma_ring_prod_index) | rw | 32 | 1, best high |
| `0x4C10`–`0x5010` (17 × 0x40) | [`TDMA_RING_BUF_SIZE`](#tdma_ring_buf_size) | rw | 32 | 1, best high |
| `0x4C14`–`0x5014` (17 × 0x40) | [`TDMA_RING_START_ADDR`](#tdma_ring_start_addr) | rw | 32 | 1, best high |
| `0x4C1C`–`0x501C` (17 × 0x40) | [`TDMA_RING_END_ADDR`](#tdma_ring_end_addr) | rw | 32 | 1, best high |
| `0x5040` | [`TDMA_RING_CFG`](#tdma_ring_cfg) | rw | 32 | 1, best high |
| `0x5044` | [`TDMA_CTRL`](#tdma_ctrl) | rw | 32 | 1, best high |
| `0x5048` | [`TDMA_STATUS`](#tdma_status) | r | 32 | 1, best high |
| `0x8000`–`0xDFFC` (6144 × 0x4) | [`HFB_RAM`](#hfb_ram) | rw | 32 | 1, best high |
| `0xFC00` | [`HFB_CTRL`](#hfb_ctrl) | rw | 32 | 1, best high |
| `0xFC04`–`0xFC08` (2 × 0x4) | [`HFB_FLT_ENABLE`](#hfb_flt_enable) | rw | 32 | 1, best high |
| `0xFC1C`–`0xFC48` (12 × 0x4) | [`HFB_FLT_LEN`](#hfb_flt_len) | rw | 32 | 1, best high |

## `SYS_REV_CTRL`

Offset `0x000` · access `r` · 32 bits · reset `0x6000000`

Major 6 in 27:24, which `bcmgenet` maps to GENET v5.

Sources:

- measured (high): `SYS_REV_CTRL` `0x06000000`
- linux (high): `bcmgenet_set_hw_params`: `GENET 5.0 EPHY: 0x0000`

## `SYS_PORT_CTRL`

Offset `0x004` · access `rw` · 32 bits

Port mode.

Sources:

- linux (high): `bcmgenet.h`: `SYS_PORT_CTRL`

## `SYS_RBUF_FLUSH_CTRL`

Offset `0x008` · access `rw` · 32 bits

Receive buffer flush.

Sources:

- linux (high): `bcmgenet.h`: `SYS_RBUF_FLUSH_CTRL`

## `SYS_TBUF_FLUSH_CTRL`

Offset `0x00C` · access `rw` · 32 bits

Transmit buffer flush.

Sources:

- linux (high): `bcmgenet.h`: `SYS_TBUF_FLUSH_CTRL`

## `EXT_PWR_MGMT`

Offset `0x080` · access `rw` · 32 bits · reset `0x51F02C3`

EXT block power management.

Sources:

- measured (high): `EXT_PWR_MGMT` `0x051f02c3`

## `EXT_RGMII_OOB_CTRL`

Offset `0x08C` · access `rw` · 32 bits · reset `0xF00000`

RGMII out-of-band control. Measured `0x00f00050`: Linux adds `RGMII_MODE_EN` (6) and `RGMII_LINK` (4) to the 23:20 the bootloader leaves.

Sources:

- measured (high): `EXT_RGMII_OOB_CTRL` `0x00f00050`

## `EXT_GPHY_CTRL`

Offset `0x09C` · access `rw` · 32 bits

Internal GPHY control; unused with the external RGMII PHY.

Sources:

- measured (high): `EXT_GPHY_CTRL` `0x00000000`

## `INTRL2_CPU_STAT`

Offset `0x200`, 2 elements 0x40 apart · access `r` · 32 bits

Interrupt status of `INTRL2_0` (ring 16 and MDIO) and `INTRL2_1` (rings 0..15: TX 15:0, RX 31:16). Each drives a line while an unmasked bit is set: GIC SPI 157 and 158.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 13 | `RXDMA_MBDONE` | r | `INTRL2_0`: ring 16 received. |
| 16 | `TXDMA_MBDONE` | r | `INTRL2_0`: ring 16 sent. |
| 23 | `MDIO_DONE` | r | `INTRL2_0`: MDIO frame done. |
| 24 | `MDIO_ERROR` | r | `INTRL2_0`: MDIO read failed. |

Sources:

- linux (high): `bcmgenet.h`: `INTRL2_CPU_STAT`, `UMAC_IRQ_*`

`RXDMA_MBDONE` sources:

- linux (high): `bcmgenet.h`: `UMAC_IRQ_RXDMA_MBDONE`

`TXDMA_MBDONE` sources:

- linux (high): `bcmgenet.h`: `UMAC_IRQ_TXDMA_MBDONE`

`MDIO_DONE` sources:

- linux (high): `bcmgenet.h`: `UMAC_IRQ_MDIO_DONE`

`MDIO_ERROR` sources:

- linux (high): `bcmgenet.h`: `UMAC_IRQ_MDIO_ERROR`

## `INTRL2_CPU_SET`

Offset `0x204`, 2 elements 0x40 apart · access `w` · 32 bits

Set status bits.

Sources:

- linux (high): `bcmgenet.h`: `INTRL2_CPU_SET`

## `INTRL2_CPU_CLEAR`

Offset `0x208`, 2 elements 0x40 apart · access `w` · 32 bits

Clear status bits.

Sources:

- linux (high): `bcmgenet.h`: `INTRL2_CPU_CLEAR`

## `INTRL2_CPU_MASK_STATUS`

Offset `0x20C`, 2 elements 0x40 apart · access `r` · 32 bits

Mask; everything masked until a driver unmasks it.

Sources:

- linux (high): `bcmgenet.h`: `INTRL2_CPU_MASK_STATUS`

## `INTRL2_CPU_MASK_SET`

Offset `0x210`, 2 elements 0x40 apart · access `w` · 32 bits

Mask bits.

Sources:

- linux (high): `bcmgenet.h`: `INTRL2_CPU_MASK_SET`

## `INTRL2_CPU_MASK_CLEAR`

Offset `0x214`, 2 elements 0x40 apart · access `w` · 32 bits

Unmask bits.

Sources:

- linux (high): `bcmgenet.h`: `INTRL2_CPU_MASK_CLEAR`

## `RBUF_CTRL`

Offset `0x300` · access `rw` · 32 bits · reset `0xC040`

Receive buffer control. Measured `0xc043` with Linux's two low bits set.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `STATUS64` | rw | Each received frame is preceded by a 64-byte status block. |
| 1 | `ALIGN_2B` | rw | Two pad bytes before the frame. |

Sources:

- measured (high): `RBUF_CTRL` `0x0000c043`

`STATUS64` sources:

- linux (high): `bcmgenet.h`: `RBUF_64B_EN`

`ALIGN_2B` sources:

- linux (high): `bcmgenet.h`: `RBUF_ALIGN_2B`

## `RBUF_CHK_CTRL`

Offset `0x314` · access `rw` · 32 bits

Receive checksum control.

Sources:

- linux (high): `bcmgenet.h`: `RBUF_CHK_CTRL`

## `RBUF_TBUF_SIZE_CTRL`

Offset `0x3B4` · access `rw` · 32 bits

Transmit buffer size control.

Sources:

- linux (high): `bcmgenet.h`: `RBUF_TBUF_SIZE_CTRL`

## `TBUF_CTRL`

Offset `0x600` · access `rw` · 32 bits

Transmit buffer control.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `STATUS64` | rw | The first 64 bytes of each frame are a transmit status block, not sent. |

Sources:

- linux (high): `bcmgenet.h`: `TBUF_CTRL`

`STATUS64` sources:

- linux (high): `bcmgenet.h`: `TBUF_64B_EN`

## `TBUF_BP_MC`

Offset `0x60C` · access `rw` · 32 bits · reset `0xFFFF`

Back-pressure mask.

Sources:

- measured (high): `TBUF_BP_MC` `0x0000ffff`

## `UMAC_HD_BKP_CTRL`

Offset `0x804` · access `rw` · 32 bits · reset `0x14`

Half-duplex back-pressure.

Sources:

- measured (high): `UMAC_HD_BKP_CTRL` `0x00000014`

## `UMAC_CMD`

Offset `0x808` · access `rw` · 32 bits

MAC command. Turning the MAC on kicks the rings.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `TX_EN` | rw | Transmit on. |
| 1 | `RX_EN` | rw | Receive on. |
| 3:2 | `SPEED` | rw | 0 = 10, 1 = 100, 2 = 1000 Mb/s. |
| 4 | `PROMISC` | rw | Accept every frame. |
| 6 | `CRC_FWD` | rw | Keep the FCS on received frames. |
| 8 | `RX_PAUSE_IGNORE` | rw | Ignore received pause frames. |
| 10 | `HD_EN` | rw | Half duplex. |
| 28 | `TX_PAUSE_IGNORE` | rw | Do not send pause frames. |

Sources:

- linux (high): `unimac.h`: `UMAC_CMD`
- measured (high): `UMAC_CMD` `0x0000000b` with the link up

`TX_EN` sources:

- linux (high): `unimac.h`: `CMD_TX_EN`

`RX_EN` sources:

- linux (high): `unimac.h`: `CMD_RX_EN`

`SPEED` sources:

- linux (high): `unimac.h`: `CMD_SPEED_SHIFT` / `CMD_SPEED_MASK`

`PROMISC` sources:

- linux (high): `unimac.h`: `CMD_PROMISC`

`CRC_FWD` sources:

- linux (high): `unimac.h`: `CMD_CRC_FWD`

`RX_PAUSE_IGNORE` sources:

- linux (high): `unimac.h`: `CMD_RX_PAUSE_IGNORE`

`HD_EN` sources:

- linux (high): `unimac.h`: `CMD_HD_EN`

`TX_PAUSE_IGNORE` sources:

- linux (high): `unimac.h`: `CMD_TX_PAUSE_IGNORE`

## `UMAC_MAC0`

Offset `0x80C` · access `rw` · 32 bits

First four octets of the station address, most significant first.

Sources:

- linux (high): `unimac.h`: `UMAC_MAC0`

## `UMAC_MAC1`

Offset `0x810` · access `rw` · 32 bits

Last two octets of the station address, in 15:0.

Sources:

- linux (high): `unimac.h`: `UMAC_MAC1`

## `UMAC_MAX_FRAME_LEN`

Offset `0x814` · access `rw` · 32 bits

Largest frame accepted.

Sources:

- linux (high): `unimac.h`: `UMAC_MAX_FRAME_LEN`

## `UMAC_PAUSE_QUANTA`

Offset `0x818` · access `rw` · 32 bits · reset `0xFFFF`

Pause quanta.

Sources:

- measured (high): `UMAC_PAUSE_QUANTA` `0x0000ffff`

## `UMAC_MODE`

Offset `0x844` · access `r` · 32 bits

Speed (1:0), half duplex (2), RX / TX pause (3 / 4), link (5); follows `UMAC_CMD` and the PHY.

Sources:

- linux (high): `unimac.h`: `UMAC_MODE`
- measured (high): `UMAC_MODE` `0x0000003a` with `UMAC_CMD` `0xb` and the link up

## `UMAC_TX_IPG_LEN`

Offset `0x85C` · access `rw` · 32 bits · reset `0x3C00`

Inter-packet gap.

Sources:

- measured (high): `UMAC_TX_IPG_LEN` `0x00003c00`

## `UMAC_EEE_CTRL`

Offset `0x864` · access `rw` · 32 bits · reset `0x40`

EEE control. Measured `0x48` once Linux has EEE active (`EEE_EN`, bit 3).

Sources:

- measured (high): `UMAC_EEE_CTRL` `0x00000048`

## `UMAC_MIB`

Offset `0xC00`, 96 elements 0x4 apart · access `r` · 32 bits

MIB counters. Nothing is counted; a MIB reset clears them anyway.

Sources:

- linux (high): `bcmgenet.h`: `UMAC_MIB_START`

## `UMAC_MIB_CTRL`

Offset `0xD80` · access `rw` · 32 bits

MIB control; bits 2:0 reset the RX / RUNT / TX counters.

Sources:

- linux (high): `bcmgenet.h`: `UMAC_MIB_CTRL`, `MIB_RESET_RX` / `_RUNT` / `_TX`

## `UMAC_MDIO_CMD`

Offset `0xE14` · access `rw` · 32 bits

MDIO command. With `START_BUSY` set the clause-22 frame runs and completes before the next access.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 15:0 | `DATA` | rw | Data written, or read back. |
| 20:16 | `REG` | rw | PHY register. |
| 25:21 | `PMD` | rw | PHY address. |
| 27:26 | `OP` | rw | 1 = write, 2 = read. |
| 28 | `READ_FAIL` | r | Nothing answered the read. |
| 29 | `START_BUSY` | rw | Run the frame; clear once done. |

Sources:

- linux (high): `mdio-bcm-unimac.c`: `MDIO_CMD`
- measured (high): `MDIO_CMD` `0x0821796d` (last op: read of PHY 1 `BMSR = 0x796d`)

`DATA` sources:

- linux (high): `mdio-bcm-unimac.c`

`REG` sources:

- linux (high): `mdio-bcm-unimac.c`: `MDIO_REG_SHIFT`

`PMD` sources:

- linux (high): `mdio-bcm-unimac.c`: `MDIO_PMD_SHIFT`

`OP` sources:

- linux (high): `mdio-bcm-unimac.c`: `MDIO_WR` / `MDIO_RD`

`READ_FAIL` sources:

- linux (high): `mdio-bcm-unimac.c`: `MDIO_READ_FAIL`

`START_BUSY` sources:

- linux (high): `mdio-bcm-unimac.c`: `MDIO_START_BUSY`

## `UMAC_MDIO_CFG`

Offset `0xE18` · access `rw` · 32 bits · reset `0x91`

MDIO configuration; left in clause-22 mode by every client.

Sources:

- measured (high): `MDIO_CFG` `0x00000091`

## `RDMA_DESC`

Offset `0x2000`, 768 elements 0x4 apart · access `rw` · 32 bits

Receive descriptor RAM: 256 descriptors of `length_status`, `address_lo`, `address_hi`.

Sources:

- linux (high): `bcmgenet.h`: `GENET_RDMA_REG_OFF`, `DMA_DESC_LENGTH_STATUS` / `ADDRESS_LO` / `ADDRESS_HI`

## `RDMA_RING_WRITE_PTR`

Offset `0x2C00`, 17 elements 0x40 apart · access `rw` · 32 bits

Per-ring write pointer, in descriptor-RAM words.

Sources:

- linux (high): `bcmgenet.c`: `genet_dma_ring_regs_v4`, `RDMA_WRITE_PTR`

## `RDMA_RING_PROD_INDEX`

Offset `0x2C08`, 17 elements 0x40 apart · access `rw` · 32 bits

Per-ring producer index (16 bits, wraps).

Sources:

- linux (high): `bcmgenet.c`: `RDMA_PROD_INDEX`

## `RDMA_RING_CONS_INDEX`

Offset `0x2C0C`, 17 elements 0x40 apart · access `rw` · 32 bits

Per-ring consumer index.

Sources:

- linux (high): `bcmgenet.c`: `RDMA_CONS_INDEX`

## `RDMA_RING_BUF_SIZE`

Offset `0x2C10`, 17 elements 0x40 apart · access `rw` · 32 bits

Ring size (31:16) and buffer length (15:0).

Sources:

- linux (high): `bcmgenet.c`: `DMA_RING_BUF_SIZE`

## `RDMA_RING_START_ADDR`

Offset `0x2C14`, 17 elements 0x40 apart · access `rw` · 32 bits

First word of the ring in descriptor RAM.

Sources:

- linux (high): `bcmgenet.c`: `DMA_START_ADDR`

## `RDMA_RING_END_ADDR`

Offset `0x2C1C`, 17 elements 0x40 apart · access `rw` · 32 bits

Last word of the ring, inclusive.

Sources:

- linux (high): `bcmgenet.c`: `DMA_END_ADDR`

## `RDMA_RING_CFG`

Offset `0x3040` · access `rw` · 32 bits

Ring configuration.

Sources:

- linux (high): `bcmgenet.h`: `DMA_RING_CFG`

## `RDMA_CTRL`

Offset `0x3044` · access `rw` · 32 bits

DMA enable (bit 0) and one enable per ring (bit `ring + 1`).

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `EN` | rw | DMA on. |

Sources:

- linux (high): `bcmgenet.h`: `DMA_CTRL`, `DMA_EN`
- measured (high): `RDMA_CTRL` `0x00000003` (Linux receives on ring 0 only)

`EN` sources:

- linux (high): `bcmgenet.h`: `DMA_EN`

## `RDMA_STATUS`

Offset `0x3048` · access `r` · 32 bits

Bit 0 DMA disabled, bits 1..17 ring `n - 1` disabled, bit 18 descriptor RAM init busy (never, here).

Sources:

- measured (high): `RDMA_CTRL` `0x03` / `RDMA_STATUS` `0x3fffc` and `TDMA_CTRL` `0x3f` / `TDMA_STATUS` `0x3ffc0` fit exactly this
- decompile (high): the bootloader waits for bit 18 after reset and bit 0 after a stop

## `RDMA_INDEX2RING`

Offset `0x30B0`, 8 elements 0x4 apart · access `rw` · 32 bits

HFB filter to receive ring: 4 bits per filter, eight filters per word.

Sources:

- linux (high): `bcmgenet.h`: `DMA_INDEX2RING_0..7`

## `TDMA_DESC`

Offset `0x4000`, 768 elements 0x4 apart · access `rw` · 32 bits

Transmit descriptor RAM, laid out as `RDMA_DESC`.

Sources:

- linux (high): `bcmgenet.h`: `GENET_TDMA_REG_OFF`

## `TDMA_RING_READ_PTR`

Offset `0x4C00`, 17 elements 0x40 apart · access `rw` · 32 bits

Per-ring read pointer.

Sources:

- linux (high): `bcmgenet.c`: `TDMA_READ_PTR`

## `TDMA_RING_CONS_INDEX`

Offset `0x4C08`, 17 elements 0x40 apart · access `rw` · 32 bits

Per-ring consumer index.

Sources:

- linux (high): `bcmgenet.c`: `TDMA_CONS_INDEX`

## `TDMA_RING_PROD_INDEX`

Offset `0x4C0C`, 17 elements 0x40 apart · access `rw` · 32 bits

Per-ring producer index; writing it sends what lies between the two indices.

Sources:

- linux (high): `bcmgenet.c`: `TDMA_PROD_INDEX`

## `TDMA_RING_BUF_SIZE`

Offset `0x4C10`, 17 elements 0x40 apart · access `rw` · 32 bits

Ring size and buffer length.

Sources:

- linux (high): `bcmgenet.c`: `DMA_RING_BUF_SIZE`

## `TDMA_RING_START_ADDR`

Offset `0x4C14`, 17 elements 0x40 apart · access `rw` · 32 bits

First word of the ring.

Sources:

- linux (high): `bcmgenet.c`: `DMA_START_ADDR`

## `TDMA_RING_END_ADDR`

Offset `0x4C1C`, 17 elements 0x40 apart · access `rw` · 32 bits

Last word of the ring, inclusive.

Sources:

- linux (high): `bcmgenet.c`: `DMA_END_ADDR`

## `TDMA_RING_CFG`

Offset `0x5040` · access `rw` · 32 bits

Ring configuration.

Sources:

- linux (high): `bcmgenet.h`: `DMA_RING_CFG`

## `TDMA_CTRL`

Offset `0x5044` · access `rw` · 32 bits

As `RDMA_CTRL`.

Sources:

- measured (high): `TDMA_CTRL` `0x0000003f`

## `TDMA_STATUS`

Offset `0x5048` · access `r` · 32 bits

As `RDMA_STATUS`.

Sources:

- measured (high): `TDMA_STATUS` `0x0003ffc0`

## `HFB_RAM`

Offset `0x8000`, 6144 elements 0x4 apart · access `rw` · 32 bits

Hardware Filter Block RAM: 48 filters of 128 words, two frame bytes per word with nibble masks in 19:16.

Sources:

- linux (high): `bcmgenet.c`: `bcmgenet_hfb_insert_data`

## `HFB_CTRL`

Offset `0xFC00` · access `rw` · 32 bits

HFB control. The bootloader and start4 never turn it on; Linux does, with a catch-all filter 0.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `EN` | rw | Filtering on. |

Sources:

- linux (high): `bcmgenet.h`: `HFB_CTRL`; commit 3b5d4f5a820d moves the `DESC_INDEX` flow to ring 0

`EN` sources:

- linux (high): `bcmgenet.h`: `RBUF_HFB_EN`

## `HFB_FLT_ENABLE`

Offset `0xFC04`, 2 elements 0x4 apart · access `rw` · 32 bits

Filter enables: filters 32..47 in word 0, 0..31 in word 1.

Sources:

- linux (high): `bcmgenet.h`: `HFB_FLT_ENABLE_V3PLUS`

## `HFB_FLT_LEN`

Offset `0xFC1C`, 12 elements 0x4 apart · access `rw` · 32 bits

Filter lengths in bytes, one byte per filter, filter 47 first.

Sources:

- linux (high): `bcmgenet.h`: `HFB_FLT_LEN_V3PLUS`
