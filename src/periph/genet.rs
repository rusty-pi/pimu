//! BCM2711 GENET v5 Ethernet MAC at `0x7D58_0000` (ARM `0xFD58_0000`,
//! `/scb/ethernet@7d580000`, `reg = <0x7d580000 0x10000>`), with its UniMAC
//! MDIO controller at `+0xE14` and the board's PHY behind it
//! ([`super::bcm54213pe`], MDIO address 1).
//!
//! # Clients
//!
//! * The EEPROM bootloader's network boot (TFTP / HTTP). Its code is in the
//!   LZ4-compressed BOOTLOADER stage of `pieeprom.bin` (not in the Ghidra
//!   decompile); disassembled, it touches the SYS, EXT, INTRL2, RBUF, UMAC,
//!   MDIO and both DMA blocks listed below, polls `MDIO_CMD` for busy, both
//!   `DMA_STATUS` registers for bit 18 after reset and for bit 0 after a stop,
//!   and programs ring 16's descriptors and ring registers.
//! * start4's network path: the same MDIO helpers (`FUN_0ecc3198` read,
//!   `FUN_0ecc3280` write), UMAC start (`FUN_0ecc3b6c`: `MAC0` / `MAC1` /
//!   `MAX_FRAME_LEN` / `MIB_CTRL` / `UMAC_CMD`) and ring 16 setup
//!   (`FUN_0ecc2d54` / `FUN_0ecc2e4c`).
//! * Linux: `drivers/net/ethernet/broadcom/genet/` and
//!   `drivers/net/mdio/mdio-bcm-unimac.c`.
//!
//! # Register map (GENET v4/v5 layout, `bcmgenet.h` / `unimac.h`)
//!
//! ```text
//!   +0x0000 SYS      REV_CTRL, PORT_CTRL, RBUF/TBUF_FLUSH_CTRL
//!   +0x0080 EXT      PWR_MGMT, RGMII_OOB_CTRL (+0x0C), GPHY_CTRL (+0x1C)
//!   +0x0200 INTRL2_0 STAT, SET, CLEAR, MASK_STATUS, MASK_SET, MASK_CLEAR
//!   +0x0240 INTRL2_1 (same layout)
//!   +0x0300 RBUF     CTRL, CHK_CTRL (+0x14), TBUF_SIZE_CTRL (+0xB4)
//!   +0x0600 TBUF     CTRL, BP_MC (+0x0C)
//!   +0x0800 UMAC     CMD (+0x08), MAC0/1, MAX_FRAME_LEN, MODE (+0x44),
//!                    MIB counters (+0x400), MIB_CTRL (+0x580),
//!                    MDIO_CMD (+0x614), MDIO_CFG (+0x618)
//!   +0x2000 RDMA     256 x 3-word descriptors, 17 x 0x40 ring register
//!                    blocks from +0x2C00, DMA registers from +0x3040
//!   +0x4000 TDMA     same layout; DMA registers from +0x5040
//!   +0x8000 HFB      filter RAM / +0xFC00 HFB registers
//! ```
//!
//! # Measured values
//!
//! Read-only `/dev/mem` reads of named registers on a Pi 4B rev 1.5 (d03115)
//! running Linux 6.12 with the link up:
//!
//! ```text
//!   SYS_REV_CTRL   0x06000000   SYS_PORT_CTRL      0x00000003
//!   EXT_PWR_MGMT   0x051f02c3   EXT_RGMII_OOB_CTRL 0x00f00050
//!   EXT_GPHY_CTRL  0x00000000   RBUF_CTRL          0x0000c043
//!   TBUF_BP_MC     0x0000ffff   UMAC_HD_BKP_CTRL   0x00000014
//!   UMAC_CMD       0x0000000b   UMAC_PAUSE_QUANTA  0x0000ffff
//!   UMAC_MODE      0x0000003a   UMAC_TX_IPG_LEN    0x00003c00
//!   UMAC_EEE_CTRL  0x00000048   MDIO_CFG           0x00000091
//!   MDIO_CMD       0x0821796d   (last op: read of PHY 1 BMSR = 0x796d)
//!   RDMA_CTRL      0x00000003   RDMA_STATUS        0x0003fffc
//!   TDMA_CTRL      0x0000003f   TDMA_STATUS        0x0003ffc0
//! ```
//!
//! Registers no client writes on that board's boot path (SD boot, Linux RGMII
//! probe and open) take those values as their reset state; the others reset
//! to 0 unless noted.
//!
//! # Packet DMA
//!
//! Each direction has 256 three-word descriptors (`length_status`,
//! `address_lo`, `address_hi`) and 17 ring register blocks laid out as
//! `genet_dma_ring_regs_v4`: `+0x00` RDMA write / TDMA read pointer, `+0x08`
//! RDMA producer / TDMA consumer index, `+0x0C` RDMA consumer / TDMA producer
//! index, `+0x10 RING_BUF_SIZE` (ring size in 31:16, buffer length in 15:0),
//! `+0x14 START_ADDR`, `+0x1C END_ADDR`. Pointers count descriptor-RAM words:
//! a ring runs from `START_ADDR` to `END_ADDR` inclusive, three words a
//! descriptor. Indices are 16 bits wide and wrap.
//!
//! A ring moves frames when `DMA_CTRL` has both `DMA_EN` (bit 0) and its
//! ring bit (`1 << (ring + 1)`) set, and the MAC side is on (`UMAC_CMD`
//! `TX_EN` / `RX_EN`) with the link up.
//!
//! * **Transmit**: every descriptor between the consumer and producer index
//!   is sent as soon as the producer index is written; `SOP` / `EOP` delimit
//!   a frame across descriptors. With `TBUF_64B_EN` the first 64 bytes are
//!   the transmit status block and are not sent. With `DMA_TX_OW_CRC` in the
//!   `SOP` descriptor the last four bytes are the slot the MAC writes the FCS
//!   into, so they are not handed to the backend either. The bootloader and
//!   start4 set it, next to `DMA_TX_APPEND_CRC`, on every frame and leave
//!   four zero bytes there (a minimum-size ARP is a 64-byte buffer); Linux
//!   sets `APPEND_CRC` alone. That the MAC overwrites rather than appends
//!   with both bits set is inferred from those buffers, not measured. Passing
//!   the slot on as payload made passt drop every DHCP DISCOVER (#45): its
//!   DHCP server wants the UDP length to match the frame exactly.
//! * **Receive**: a frame is accepted when it is broadcast, multicast,
//!   addressed to `UMAC_MAC0` / `UMAC_MAC1`, or `UMAC_CMD`'s `PROMISC` is
//!   set. With the Hardware Filter Block on (`HFB_CTRL` bit 0) the
//!   highest-numbered enabled filter that matches picks the ring through
//!   `DMA_INDEX2RING`; otherwise it goes to the default ring 16. The
//!   bootloader and start4 never turn the HFB on. Linux since 3b5d4f5a820d
//!   ("move DESC_INDEX flow to ring 0") does, with an empty 4-byte filter 0
//!   that matches everything, and receives on ring 0 only (`RDMA_CTRL` 0x3,
//!   as measured). Its ethtool rules sit at filters 1 and up, so a higher
//!   filter has to win over that catch-all; which one real hardware picks
//!   when several match is inferred from that layout, not measured. A frame
//!   for a ring that is off while receive DMA runs is dropped; with receive
//!   DMA off it waits in the backend.
//!
//!   A frame lands in one buffer behind the 64-byte receive
//!   status block when `RBUF_64B_EN`, after two pad bytes when
//!   `RBUF_ALIGN_2B`, followed by the FCS when `CMD_CRC_FWD`; frames shorter
//!   than the Ethernet minimum are padded to 60 bytes the way the sender's
//!   MAC would have. The length word, in the descriptor and in the status
//!   block, counts all of it.
//!
//! Frames come from and go to the [`crate::net::NetBackend`] the machine has
//! attached; with none attached there is no cable and the PHY reports no
//! link. Everything completes during the register write that starts it.
//!
//! # Interrupts
//!
//! `INTRL2_0` collects `TXDMA_MBDONE` / `RXDMA_MBDONE` (ring 16) and the
//! MDIO bits; `INTRL2_1` the per-ring bits of rings 0..15 (TX in 15:0, RX in
//! 31:16). Each instance drives one line, high while a status bit is set that
//! its mask lets through ([`Genet::irq_lines`]): GIC SPI 157 and 158 in the
//! Pi 4 device tree.

use super::bcm54213pe::{self, Bcm54213pe};
use crate::bus::{BusResult, MmioDevice, Width};

use crate::mem::Ram;
use crate::net::NetBackend;
use crate::spec::genet as regs;
use crate::spec::Coverage;

pub use regs::{
    EXT_PWR_MGMT, EXT_RGMII_OOB_CTRL, HFB_CTRL, RBUF_CTRL, RDMA_DESC, SIZE, SYS_PORT_CTRL,
    SYS_RBUF_FLUSH_CTRL, SYS_REV_CTRL, SYS_TBUF_FLUSH_CTRL, TBUF_BP_MC, TBUF_CTRL, TDMA_DESC,
    UMAC_CMD, UMAC_EEE_CTRL, UMAC_HD_BKP_CTRL, UMAC_MAC0, UMAC_MAC1, UMAC_MDIO_CFG, UMAC_MDIO_CMD,
    UMAC_MIB_CTRL, UMAC_MODE, UMAC_PAUSE_QUANTA, UMAC_TX_IPG_LEN,
};

/// The whole window is register storage, so every register in
/// `specs/genet.toml` is modelled; the ones with behaviour are intercepted in
/// [`Genet::read_word`] / [`Genet::write_word`].
pub const COVERAGE: Coverage = Coverage {
    block: "genet",
    decoded: &[
        regs::SYS_REV_CTRL,
        regs::SYS_PORT_CTRL,
        regs::SYS_RBUF_FLUSH_CTRL,
        regs::SYS_TBUF_FLUSH_CTRL,
        regs::EXT_PWR_MGMT,
        regs::EXT_RGMII_OOB_CTRL,
        regs::EXT_GPHY_CTRL,
        regs::EXT_24,
        regs::INTRL2_CPU_STAT,
        regs::INTRL2_CPU_SET,
        regs::INTRL2_CPU_CLEAR,
        regs::INTRL2_CPU_MASK_STATUS,
        regs::INTRL2_CPU_MASK_SET,
        regs::INTRL2_CPU_MASK_CLEAR,
        regs::RBUF_CTRL,
        regs::RBUF_CHK_CTRL,
        regs::RBUF_TBUF_SIZE_CTRL,
        regs::TBUF_CTRL,
        regs::TBUF_BP_MC,
        regs::UMAC_HD_BKP_CTRL,
        regs::UMAC_CMD,
        regs::UMAC_MAC0,
        regs::UMAC_MAC1,
        regs::UMAC_MAX_FRAME_LEN,
        regs::UMAC_PAUSE_QUANTA,
        regs::UMAC_MODE,
        regs::UMAC_TX_IPG_LEN,
        regs::UMAC_EEE_CTRL,
        regs::UMAC_MIB,
        regs::UMAC_MIB_CTRL,
        regs::UMAC_604,
        regs::UMAC_608,
        regs::UMAC_MDIO_CMD,
        regs::UMAC_MDIO_CFG,
        regs::RDMA_DESC,
        regs::RDMA_RING_WRITE_PTR,
        regs::RDMA_RING_WRITE_PTR_HI,
        regs::RDMA_RING_PROD_INDEX,
        regs::RDMA_RING_CONS_INDEX,
        regs::RDMA_RING_BUF_SIZE,
        regs::RDMA_RING_START_ADDR,
        regs::RDMA_RING_START_ADDR_HI,
        regs::RDMA_RING_END_ADDR,
        regs::RDMA_RING_END_ADDR_HI,
        regs::RDMA_RING_MBUF_DONE_THRESH,
        regs::RDMA_RING_XON_XOFF_THRESH,
        regs::RDMA_RING_READ_PTR,
        regs::RDMA_RING_READ_PTR_HI,
        regs::RDMA_RING_CFG,
        regs::RDMA_CTRL,
        regs::RDMA_STATUS,
        regs::RDMA_SCB_BURST_SIZE,
        regs::RDMA_INDEX2RING,
        regs::TDMA_DESC,
        regs::TDMA_RING_READ_PTR,
        regs::TDMA_RING_READ_PTR_HI,
        regs::TDMA_RING_CONS_INDEX,
        regs::TDMA_RING_PROD_INDEX,
        regs::TDMA_RING_BUF_SIZE,
        regs::TDMA_RING_START_ADDR,
        regs::TDMA_RING_START_ADDR_HI,
        regs::TDMA_RING_END_ADDR,
        regs::TDMA_RING_END_ADDR_HI,
        regs::TDMA_RING_MBUF_DONE_THRESH,
        regs::TDMA_RING_FLOW_PERIOD,
        regs::TDMA_RING_WRITE_PTR,
        regs::TDMA_RING_WRITE_PTR_HI,
        regs::TDMA_RING_CFG,
        regs::TDMA_CTRL,
        regs::TDMA_STATUS,
        regs::TDMA_SCB_BURST_SIZE,
        regs::HFB_RAM,
        regs::HFB_CTRL,
        regs::HFB_FLT_ENABLE,
        regs::HFB_FLT_LEN,
    ],
};

// INTRL2: two instances; the rest of the code works with offsets within one.
pub const INTRL2_0: u32 = regs::INTRL2_CPU_STAT;
pub const INTRL2_1: u32 = INTRL2_0 + INTRL2_STRIDE;
const INTRL2_STRIDE: u32 = regs::INTRL2_CPU_STAT_STRIDE;
const INTRL2_END: u32 = INTRL2_1 + INTRL2_STRIDE - 1;
const INTRL2_CPU_STAT: u32 = 0;
const INTRL2_CPU_SET: u32 = regs::INTRL2_CPU_SET - INTRL2_0;
const INTRL2_CPU_CLEAR: u32 = regs::INTRL2_CPU_CLEAR - INTRL2_0;
const INTRL2_CPU_MASK_STATUS: u32 = regs::INTRL2_CPU_MASK_STATUS - INTRL2_0;
const INTRL2_CPU_MASK_SET: u32 = regs::INTRL2_CPU_MASK_SET - INTRL2_0;
const INTRL2_CPU_MASK_CLEAR: u32 = regs::INTRL2_CPU_MASK_CLEAR - INTRL2_0;
pub const UMAC_IRQ_RXDMA_MBDONE: u32 = regs::INTRL2_CPU_STAT_RXDMA_MBDONE_MASK;
pub const UMAC_IRQ_TXDMA_MBDONE: u32 = regs::INTRL2_CPU_STAT_TXDMA_MBDONE_MASK;
pub const UMAC_IRQ_MDIO_DONE: u32 = regs::INTRL2_CPU_STAT_MDIO_DONE_MASK;
pub const UMAC_IRQ_MDIO_ERROR: u32 = regs::INTRL2_CPU_STAT_MDIO_ERROR_MASK;
// RBUF / TBUF
const RBUF_64B_EN: u32 = regs::RBUF_CTRL_STATUS64_MASK;
const RBUF_ALIGN_2B: u32 = regs::RBUF_CTRL_ALIGN_2B_MASK;
const TBUF_64B_EN: u32 = regs::TBUF_CTRL_STATUS64_MASK;
// UMAC
pub const UMAC_MIB_START: u32 = regs::UMAC_MIB;
// DMA: each direction's ring-config / control / status block, as offsets
// within it; both directions are laid out alike.
pub const RDMA_REGS: u32 = regs::RDMA_RING_CFG;
pub const TDMA_REGS: u32 = regs::TDMA_RING_CFG;
const DMA_CTRL: u32 = regs::RDMA_CTRL - RDMA_REGS;
const DMA_STATUS: u32 = regs::RDMA_STATUS - RDMA_REGS;
const DMA_EN: u32 = regs::RDMA_CTRL_EN_MASK;
/// Descriptor RAM, three words a descriptor.
const DESC_RAM_BYTES: u32 = regs::RDMA_DESC_COUNT * regs::RDMA_DESC_STRIDE;
pub const RDMA_RINGS: u32 = regs::RDMA_RING_WRITE_PTR;
pub const TDMA_RINGS: u32 = regs::TDMA_RING_READ_PTR;
const RING_STRIDE: u32 = regs::RDMA_RING_WRITE_PTR_STRIDE;
const RINGS: usize = regs::RDMA_RING_WRITE_PTR_COUNT as usize;
/// The ring frames go to when no HFB filter claims them: the one the
/// bootloader and start4 use, and Linux before 3b5d4f5a820d.
pub const DEFAULT_RING: usize = 16;
/// `DMA_INDEX2RING_0..7` in the RDMA registers: a 4-bit ring number per HFB
/// filter, eight to a word.
const DMA_INDEX2RING: u32 = regs::RDMA_INDEX2RING - RDMA_REGS;
// Hardware Filter Block. Filter RAM: 48 filters of 128 words, two frame bytes
// a word (even byte in 15:8 with its high/low nibble mask in bits 19/18, odd
// byte in 7:0 with bits 17/16), as `bcmgenet_hfb_insert_data` writes them.
const HFB_RAM: u32 = regs::HFB_RAM;
const HFB_FILTER_WORDS: usize = 128;
const HFB_FILTERS: usize = regs::HFB_RAM_COUNT as usize / HFB_FILTER_WORDS;
const HFB_EN: u32 = regs::HFB_CTRL_EN_MASK;
/// Two enable words: filters 32..47 at `+0x04`, filters 0..31 at `+0x08`.
const HFB_FLT_ENABLE: u32 = regs::HFB_FLT_ENABLE;
/// Filter lengths in bytes, one byte a filter, filter 47 first.
const HFB_FLT_LEN: u32 = regs::HFB_FLT_LEN;
// Ring registers, as offsets within one ring's block.
pub const RING_PTR: u32 = regs::RDMA_RING_WRITE_PTR - RDMA_RINGS;
pub const RDMA_PROD_INDEX: u32 = regs::RDMA_RING_PROD_INDEX - RDMA_RINGS;
pub const TDMA_CONS_INDEX: u32 = regs::TDMA_RING_CONS_INDEX - TDMA_RINGS;
pub const RDMA_CONS_INDEX: u32 = regs::RDMA_RING_CONS_INDEX - RDMA_RINGS;
pub const TDMA_PROD_INDEX: u32 = regs::TDMA_RING_PROD_INDEX - TDMA_RINGS;
pub const RING_BUF_SIZE: u32 = regs::RDMA_RING_BUF_SIZE - RDMA_RINGS;
pub const RING_START: u32 = regs::RDMA_RING_START_ADDR - RDMA_RINGS;
pub const RING_END: u32 = regs::RDMA_RING_END_ADDR - RDMA_RINGS;
// The transmit side shares the receive side's ring and DMA layout.
const _: () = assert!(
    regs::TDMA_RING_READ_PTR - TDMA_RINGS == RING_PTR
        && regs::TDMA_RING_BUF_SIZE - TDMA_RINGS == RING_BUF_SIZE
        && regs::TDMA_RING_START_ADDR - TDMA_RINGS == RING_START
        && regs::TDMA_RING_END_ADDR - TDMA_RINGS == RING_END
        && regs::TDMA_RING_READ_PTR_STRIDE == RING_STRIDE
        && regs::TDMA_CTRL - TDMA_REGS == DMA_CTRL
        && regs::TDMA_STATUS - TDMA_REGS == DMA_STATUS
);
const INDEX_MASK: u32 = 0xffff;
// Descriptor `length_status`.
const DESC_LEN_SHIFT: u32 = 16;
const DESC_LEN_MASK: u32 = 0xfff;
pub const DESC_EOP: u32 = 0x4000;
pub const DESC_SOP: u32 = 0x2000;
/// Transmit: the MAC writes the FCS over the buffer's last four bytes
/// (bcmgenet.h `DMA_TX_OW_CRC`).
const DESC_TX_OW_CRC: u32 = 0x0020;
pub const DESC_RX_BRDCAST: u32 = 0x0040;
pub const DESC_RX_MULT: u32 = 0x0020;
/// Transmit and receive status block size (`struct status_64`).
const STATUS_BLOCK: usize = 64;
const ETH_ZLEN: usize = 60;
const FCS_LEN: usize = 4;

// UMAC_CMD bits
const CMD_TX_EN: u32 = regs::UMAC_CMD_TX_EN_MASK;
const CMD_RX_EN: u32 = regs::UMAC_CMD_RX_EN_MASK;
const CMD_SPEED_SHIFT: u32 = regs::UMAC_CMD_SPEED_SHIFT;
const CMD_PROMISC: u32 = regs::UMAC_CMD_PROMISC_MASK;
const CMD_CRC_FWD: u32 = regs::UMAC_CMD_CRC_FWD_MASK;
const CMD_HD_EN: u32 = regs::UMAC_CMD_HD_EN_MASK;
const CMD_RX_PAUSE_IGNORE: u32 = regs::UMAC_CMD_RX_PAUSE_IGNORE_MASK;
const CMD_TX_PAUSE_IGNORE: u32 = regs::UMAC_CMD_TX_PAUSE_IGNORE_MASK;
// MDIO_CMD bits
pub const MDIO_START_BUSY: u32 = regs::UMAC_MDIO_CMD_START_BUSY_MASK;
pub const MDIO_READ_FAIL: u32 = regs::UMAC_MDIO_CMD_READ_FAIL_MASK;
const MDIO_OP: u32 = regs::UMAC_MDIO_CMD_OP_MASK;
pub const MDIO_RD: u32 = 2 << regs::UMAC_MDIO_CMD_OP_SHIFT;
pub const MDIO_WR: u32 = 1 << regs::UMAC_MDIO_CMD_OP_SHIFT;
pub const MDIO_PMD_SHIFT: u32 = regs::UMAC_MDIO_CMD_PMD_SHIFT;
pub const MDIO_REG_SHIFT: u32 = regs::UMAC_MDIO_CMD_REG_SHIFT;

/// `SYS_REV_CTRL`, measured: major 6 in bits 27:24, which `bcmgenet` maps to
/// GENET v5 (`GENET 5.0 EPHY: 0x0000`).
pub const REV_CTRL_VALUE: u32 = regs::SYS_REV_CTRL_RESET;

/// `DMA_STATUS` bit 0: DMA disabled. Bits 1..=17: ring `n - 1` disabled.
/// Bit 18: descriptor RAM initialisation busy (the bootloader waits for it to
/// clear); it is never busy here. The measured pairs `CTRL 0x03 / STATUS
/// 0x3fffc` and `CTRL 0x3f / STATUS 0x3ffc0` fit exactly this.
const DMA_STATUS_RINGS: u32 = 0x3_fffe;

#[derive(Debug, Clone)]
pub struct Genet {
    /// The whole window as 32-bit words; registers with behaviour are
    /// intercepted in [`Genet::read_word`] / [`Genet::write_word`].
    regs: Vec<u32>,
    irq_stat: [u32; 2],
    irq_mask: [u32; 2],
    pub phy: Bcm54213pe,
    /// A ring register was written: [`Genet::service`] has work to look at.
    kick: bool,
    /// A transmit frame whose `EOP` descriptor has not been queued yet.
    tx_frame: Vec<u8>,
    /// The frame being gathered ends in the slot for its FCS (`DMA_TX_OW_CRC`).
    tx_ow_crc: bool,
    /// A received frame whose ring has no free buffer yet.
    rx_pending: Option<Vec<u8>>,
    /// A frame the backend has handed over that is being held back until its
    /// time, and the model's clock to measure that against. A reply does not
    /// arrive the instant it was asked for; with `--jitter` the model says so
    /// too. Unused while jitter is off, when every frame is taken as it comes.
    rx_hold: Option<(u64, Vec<u8>)>,
    now_us: u64,
    pub stats: Stats,
}

/// The MAC as a run ends: the address it filters on and whether it is
/// running. [`Genet::mac_state`] reads it out for the run report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MacState {
    pub addr: [u8; 6],
    pub tx_en: bool,
    pub rx_en: bool,
    pub promisc: bool,
}

/// Frame counters, for the run report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub tx: u64,
    /// Transmitted with the MAC off, no link or nothing attached.
    pub tx_dropped: u64,
    pub rx: u64,
    /// Not for this MAC.
    pub rx_filtered: u64,
    /// Bigger than the ring's receive buffers, or the buffer is not in RAM.
    pub rx_dropped: u64,
}

impl Default for Genet {
    fn default() -> Self {
        Genet::new()
    }
}

impl Genet {
    pub fn new() -> Genet {
        let mut regs = vec![0u32; (SIZE / 4) as usize];
        let mut set = |off: u32, v: u32| regs[(off / 4) as usize] = v;
        // Measured, and not written by the bootloader or start4 on an SD boot
        // nor by Linux for an RGMII PHY (see the module docs). The last three
        // are the measured values less the bits Linux sets.
        for (off, v) in [
            (EXT_PWR_MGMT, regs::EXT_PWR_MGMT_RESET),
            (TBUF_BP_MC, regs::TBUF_BP_MC_RESET),
            (UMAC_HD_BKP_CTRL, regs::UMAC_HD_BKP_CTRL_RESET),
            (UMAC_PAUSE_QUANTA, regs::UMAC_PAUSE_QUANTA_RESET),
            (UMAC_TX_IPG_LEN, regs::UMAC_TX_IPG_LEN_RESET),
            (UMAC_MDIO_CFG, regs::UMAC_MDIO_CFG_RESET),
            (EXT_RGMII_OOB_CTRL, regs::EXT_RGMII_OOB_CTRL_RESET),
            (RBUF_CTRL, regs::RBUF_CTRL_RESET),
            (UMAC_EEE_CTRL, regs::UMAC_EEE_CTRL_RESET),
        ] {
            set(off, v);
        }
        Genet {
            regs,
            irq_stat: [0; 2],
            // Everything masked until a driver unmasks it.
            irq_mask: [u32::MAX; 2],
            phy: Bcm54213pe::new(),
            kick: false,
            tx_frame: Vec::new(),
            tx_ow_crc: false,
            rx_pending: None,
            rx_hold: None,
            now_us: 0,
            stats: Stats::default(),
        }
    }

    fn reg(&self, off: u32) -> u32 {
        self.regs[(off / 4) as usize]
    }

    fn read_word(&self, off: u32) -> u32 {
        match off {
            SYS_REV_CTRL => REV_CTRL_VALUE,
            INTRL2_0..=INTRL2_END => {
                let i = ((off - INTRL2_0) / INTRL2_STRIDE) as usize;
                match (off - INTRL2_0) % INTRL2_STRIDE {
                    INTRL2_CPU_STAT => self.irq_stat[i],
                    INTRL2_CPU_MASK_STATUS => self.irq_mask[i],
                    _ => 0,
                }
            }
            UMAC_MODE => self.umac_mode(),
            _ if off == RDMA_REGS + DMA_STATUS || off == TDMA_REGS + DMA_STATUS => {
                let ctrl = self.reg(off - DMA_STATUS + DMA_CTRL);
                !ctrl & (DMA_STATUS_RINGS | 1)
            }
            _ => self.reg(off),
        }
    }

    fn write_word(&mut self, off: u32, v: u32) {
        match off {
            SYS_REV_CTRL => {}
            INTRL2_0..=INTRL2_END => {
                let i = ((off - INTRL2_0) / INTRL2_STRIDE) as usize;
                match (off - INTRL2_0) % INTRL2_STRIDE {
                    INTRL2_CPU_SET => self.irq_stat[i] |= v,
                    INTRL2_CPU_CLEAR => self.irq_stat[i] &= !v,
                    INTRL2_CPU_MASK_SET => self.irq_mask[i] |= v,
                    INTRL2_CPU_MASK_CLEAR => self.irq_mask[i] &= !v,
                    _ => {}
                }
            }
            UMAC_MODE => {}
            UMAC_MDIO_CMD => self.mdio_cmd(v),
            UMAC_MIB_CTRL => {
                // MIB_RESET_RX / _RUNT / _TX: nothing has been counted, but a
                // reset still clears whatever the counters hold.
                if v & 7 != 0 {
                    let lo = (UMAC_MIB_START / 4) as usize;
                    self.regs[lo..(UMAC_MIB_CTRL / 4) as usize].fill(0);
                }
                self.regs[(off / 4) as usize] = v;
            }
            _ if off == RDMA_REGS + DMA_STATUS || off == TDMA_REGS + DMA_STATUS => {}
            _ => self.regs[(off / 4) as usize] = v,
        }
        // Ring and DMA registers start or unblock the rings; so does turning
        // the MAC on.
        if (RDMA_RINGS..RDMA_REGS + 0x40).contains(&off)
            || (TDMA_RINGS..TDMA_REGS + 0x40).contains(&off)
            || off == UMAC_CMD
        {
            self.kick = true;
        }
    }

    /// The two interrupt outputs, `INTRL2_0` and `INTRL2_1`.
    pub fn irq_lines(&self) -> [bool; 2] {
        [0, 1].map(|i| self.irq_stat[i] & !self.irq_mask[i] != 0)
    }

    /// A register write since the last [`Genet::service`] may have given the
    /// rings something to do.
    pub fn take_kick(&mut self) -> bool {
        std::mem::take(&mut self.kick)
    }

    /// Run the rings: send whatever the transmit rings hold, then fill free
    /// receive buffers from `net`.
    /// How long a reply takes to come back before jitter stretches it: one
    /// LAN round trip, which no register gives and which only has to be more
    /// than nothing.
    const RX_LATENCY_US: u64 = 200;

    pub fn service(&mut self, now_us: u64, ram: &mut Ram, net: &mut Option<Box<dyn NetBackend>>) {
        self.now_us = now_us;
        for ring in 0..RINGS {
            self.run_tx(ring, ram, net);
        }
        self.run_rx(ram, net);
    }

    /// The next frame off the backend, held back for a round trip when
    /// jitter is on and taken as it comes when it is not.
    fn take_frame(&mut self, net: &mut Box<dyn NetBackend>) -> Option<Vec<u8>> {
        if !crate::jitter::is_on() {
            return net.recv();
        }
        if self.rx_hold.is_none() {
            let frame = net.recv()?;
            // A frame that never arrives. The sender is not told, so this is
            // the firmware's to notice and ask again for.
            if crate::jitter::fault("a lost frame") {
                self.stats.rx_dropped += 1;
                return None;
            }
            let due =
                self.now_us + crate::jitter::stretch(Genet::RX_LATENCY_US, "a frame's return");
            self.rx_hold = Some((due, frame));
        }
        match self.rx_hold.take() {
            Some((due, frame)) if due <= self.now_us => Some(frame),
            held => {
                self.rx_hold = held;
                None
            }
        }
    }

    fn ring_base(rx: bool, ring: usize) -> u32 {
        (if rx { RDMA_RINGS } else { TDMA_RINGS }) + ring as u32 * RING_STRIDE
    }

    fn ring_enabled(&self, rx: bool, ring: usize) -> bool {
        let ctrl = self.reg(if rx { RDMA_REGS } else { TDMA_REGS } + DMA_CTRL);
        ctrl & DMA_EN != 0 && ctrl & (1 << (ring + 1)) != 0
    }

    /// The descriptor after the one at word pointer `ptr`, wrapping from
    /// `END_ADDR` back to `START_ADDR`.
    fn next_ptr(&self, base: u32, ptr: u32) -> u32 {
        let next = ptr + 3;
        if next > self.reg(base + RING_END) {
            self.reg(base + RING_START)
        } else {
            next
        }
    }

    /// Byte offset of the descriptor at word pointer `ptr`, if it is inside
    /// descriptor RAM.
    fn desc(desc_ram: u32, ptr: u32) -> Option<u32> {
        let off = desc_ram + ptr * 4;
        (off + 12 <= desc_ram + DESC_RAM_BYTES).then_some(off)
    }

    fn set_index(&mut self, at: u32, index: u32) {
        let old = self.reg(at);
        self.regs[(at / 4) as usize] = (old & !INDEX_MASK) | (index & INDEX_MASK);
    }

    fn link_active(&self, cmd_bit: u32) -> bool {
        self.reg(UMAC_CMD) & cmd_bit != 0 && self.phy.link_up()
    }

    fn run_tx(&mut self, ring: usize, ram: &mut Ram, net: &mut Option<Box<dyn NetBackend>>) {
        if !self.ring_enabled(false, ring) {
            return;
        }
        let base = Self::ring_base(false, ring);
        let prod = self.reg(base + TDMA_PROD_INDEX) & INDEX_MASK;
        let mut cons = self.reg(base + TDMA_CONS_INDEX) & INDEX_MASK;
        if prod == cons {
            return;
        }
        let mut ptr = self.reg(base + RING_PTR);
        while cons != prod {
            let Some(d) = Self::desc(TDMA_DESC, ptr) else {
                break;
            };
            let ls = self.reg(d);
            let len = ((ls >> DESC_LEN_SHIFT) & DESC_LEN_MASK) as usize;
            if ls & DESC_SOP != 0 {
                self.tx_frame.clear();
                self.tx_ow_crc = ls & DESC_TX_OW_CRC != 0;
            }
            if self.reg(d + 8) == 0 {
                if let Ok(b) = ram.read_slice(dma_addr(self.reg(d + 4), ram.len()), len) {
                    self.tx_frame.extend_from_slice(b);
                }
            }
            if ls & DESC_EOP != 0 {
                let frame = std::mem::take(&mut self.tx_frame);
                self.transmit(frame, net);
            }
            ptr = self.next_ptr(base, ptr);
            cons = (cons + 1) & INDEX_MASK;
        }
        self.regs[((base + RING_PTR) / 4) as usize] = ptr;
        self.set_index(base + TDMA_CONS_INDEX, cons);
        if ring == DEFAULT_RING {
            self.irq_stat[0] |= UMAC_IRQ_TXDMA_MBDONE;
        } else {
            self.irq_stat[1] |= 1 << ring;
        }
    }

    fn transmit(&mut self, mut frame: Vec<u8>, net: &mut Option<Box<dyn NetBackend>>) {
        if self.reg(TBUF_CTRL) & TBUF_64B_EN != 0 {
            frame.drain(..STATUS_BLOCK.min(frame.len()));
        }
        // The MAC writes the FCS over the last four bytes, and a backend takes
        // frames without one.
        if self.tx_ow_crc {
            frame.truncate(frame.len().saturating_sub(FCS_LEN));
        }
        match net {
            Some(net) if self.link_active(CMD_TX_EN) && frame.len() >= 14 => {
                self.stats.tx += 1;
                net.send(&frame);
            }
            _ => self.stats.tx_dropped += 1,
        }
    }

    /// What the driver left the MAC set to, for the run report: a client that
    /// fails to read the board's address out of the firmware programs zeroes
    /// here, and nothing else in the run says so.
    pub fn mac_state(&self) -> MacState {
        let cmd = self.reg(UMAC_CMD);
        MacState {
            addr: self.mac_addr(),
            tx_en: cmd & CMD_TX_EN != 0,
            rx_en: cmd & CMD_RX_EN != 0,
            promisc: cmd & CMD_PROMISC != 0,
        }
    }

    fn mac_addr(&self) -> [u8; 6] {
        let (hi, lo) = (self.reg(UMAC_MAC0), self.reg(UMAC_MAC1));
        let [a, b, c, d] = hi.to_be_bytes();
        let [_, _, e, f] = lo.to_be_bytes();
        [a, b, c, d, e, f]
    }

    /// The receive ring the Hardware Filter Block steers `frame` to, if the
    /// block is on and one of its filters matches.
    fn hfb_ring(&self, frame: &[u8]) -> Option<usize> {
        if self.reg(HFB_CTRL) & HFB_EN == 0 {
            return None;
        }
        let f = (0..HFB_FILTERS).rev().find(|&f| self.hfb_match(f, frame))?;
        let map = self.reg(RDMA_REGS + DMA_INDEX2RING + 4 * (f / 8) as u32);
        Some(((map >> (4 * (f % 8))) & 0xf) as usize)
    }

    fn hfb_match(&self, f: usize, frame: &[u8]) -> bool {
        let enable = self.reg(HFB_FLT_ENABLE + if f < 32 { 4 } else { 0 });
        if enable & (1 << (f % 32)) == 0 {
            return false;
        }
        let lens = self.reg(HFB_FLT_LEN + 4 * ((HFB_FILTERS - 1 - f) / 4) as u32);
        let len = ((lens >> (8 * (f % 4))) & 0xff) as usize;
        frame.len() >= len
            && (0..len).all(|i| {
                let word = self.reg(HFB_RAM + 4 * (f * HFB_FILTER_WORDS + i / 2) as u32);
                let (pattern, nibbles) = if i % 2 == 0 {
                    ((word >> 8) as u8, (word >> 18) & 3)
                } else {
                    (word as u8, (word >> 16) & 3)
                };
                let mask = if nibbles & 2 != 0 { 0xf0 } else { 0 }
                    | if nibbles & 1 != 0 { 0x0f } else { 0 };
                (frame[i] ^ pattern) & mask == 0
            })
    }

    fn run_rx(&mut self, ram: &mut Ram, net: &mut Option<Box<dyn NetBackend>>) {
        let Some(net) = net else { return };
        let rdma_on = self.reg(RDMA_REGS + DMA_CTRL) & DMA_EN != 0;
        if !rdma_on || !self.link_active(CMD_RX_EN) {
            return;
        }
        let rbuf = self.reg(RBUF_CTRL);
        let cmd = self.reg(UMAC_CMD);
        while let Some(mut frame) = self.rx_pending.take().or_else(|| self.take_frame(net)) {
            if frame.len() < 14 {
                continue;
            }
            let dst: [u8; 6] = frame[..6].try_into().unwrap();
            let group = dst[0] & 1 != 0;
            if !group && cmd & CMD_PROMISC == 0 && dst != self.mac_addr() {
                self.stats.rx_filtered += 1;
                continue;
            }
            let ring = self.hfb_ring(&frame).unwrap_or(DEFAULT_RING);
            if !self.ring_enabled(true, ring) {
                self.stats.rx_dropped += 1;
                continue;
            }
            let base = Self::ring_base(true, ring);
            let size = self.reg(base + RING_BUF_SIZE) >> 16;
            let buf_len = (self.reg(base + RING_BUF_SIZE) & 0xffff) as usize;
            let prod = self.reg(base + RDMA_PROD_INDEX) & INDEX_MASK;
            let cons = self.reg(base + RDMA_CONS_INDEX) & INDEX_MASK;
            let ptr = self.reg(base + RING_PTR);
            let free = prod.wrapping_sub(cons) & INDEX_MASK < size;
            let Some(d) = Self::desc(RDMA_DESC, ptr).filter(|_| free) else {
                self.rx_pending = Some(frame);
                break;
            };
            if frame.len() < ETH_ZLEN {
                frame.resize(ETH_ZLEN, 0);
            }
            let mut buf = Vec::with_capacity(STATUS_BLOCK + 2 + frame.len() + 4);
            if rbuf & RBUF_64B_EN != 0 {
                buf.resize(STATUS_BLOCK, 0);
            }
            if rbuf & RBUF_ALIGN_2B != 0 {
                buf.extend_from_slice(&[0, 0]);
            }
            buf.extend_from_slice(&frame);
            if cmd & CMD_CRC_FWD != 0 {
                buf.extend_from_slice(&crc32(&frame).to_le_bytes());
            }
            let mut flags = DESC_SOP | DESC_EOP;
            if dst == crate::net::BROADCAST {
                flags |= DESC_RX_BRDCAST;
            } else if group {
                flags |= DESC_RX_MULT;
            }
            let ls = (buf.len() as u32) << DESC_LEN_SHIFT | flags;
            if rbuf & RBUF_64B_EN != 0 {
                buf[..4].copy_from_slice(&ls.to_le_bytes());
            }
            if buf.len() > buf_len
                || self.reg(d + 8) != 0
                || ram
                    .write_slice(dma_addr(self.reg(d + 4), ram.len()), &buf)
                    .is_err()
            {
                self.stats.rx_dropped += 1;
                continue;
            }
            self.regs[(d / 4) as usize] = ls;
            self.regs[((base + RING_PTR) / 4) as usize] = self.next_ptr(base, ptr);
            self.set_index(base + RDMA_PROD_INDEX, prod + 1);
            if ring == DEFAULT_RING {
                self.irq_stat[0] |= UMAC_IRQ_RXDMA_MBDONE;
            } else {
                self.irq_stat[1] |= 1 << (16 + ring);
            }
            self.stats.rx += 1;
        }
    }

    /// `UMAC_MODE` (read-only): bits 1:0 speed, bit 2 half duplex, bit 3 RX
    /// pause, bit 4 TX pause, bit 5 link. Speed, duplex and pause follow
    /// `UMAC_CMD`; the link bit is the RGMII link, which follows the PHY's.
    /// Measured `0x3a` with `UMAC_CMD = 0xb` and the link up.
    fn umac_mode(&self) -> u32 {
        let cmd = self.reg(UMAC_CMD);
        let mut mode = (cmd >> CMD_SPEED_SHIFT) & 3;
        if self.phy.link_up() {
            mode |= 1 << 5;
        }
        if cmd & CMD_HD_EN != 0 {
            mode |= 1 << 2;
        }
        if cmd & CMD_RX_PAUSE_IGNORE == 0 {
            mode |= 1 << 3;
        }
        if cmd & CMD_TX_PAUSE_IGNORE == 0 {
            mode |= 1 << 4;
        }
        mode
    }

    /// A write to `MDIO_CMD`. Without `START_BUSY` it only latches the
    /// command; with it the clause-22 frame runs. A frame takes ~25 us on the
    /// wire (`unimac_mdio_poll`); here it completes before the next access, so
    /// the first busy poll already reads it done. A read addressed to a PHY
    /// that isn't there sees the bus pulled high and sets `READ_FAIL`.
    fn mdio_cmd(&mut self, v: u32) {
        let slot = (UMAC_MDIO_CMD / 4) as usize;
        if v & MDIO_START_BUSY == 0 {
            self.regs[slot] = v;
            return;
        }
        let pmd = ((v >> MDIO_PMD_SHIFT) & 0x1f) as u8;
        let reg = ((v >> MDIO_REG_SHIFT) & 0x1f) as u8;
        let present = pmd == bcm54213pe::ADDR;
        let mut done = v & !(MDIO_START_BUSY | MDIO_READ_FAIL);
        let mut irq = UMAC_IRQ_MDIO_DONE;
        match v & MDIO_OP {
            MDIO_RD => {
                done &= !0xffff;
                if present {
                    done |= u32::from(self.phy.read(reg));
                } else {
                    done |= MDIO_READ_FAIL | 0xffff;
                    irq |= UMAC_IRQ_MDIO_ERROR;
                }
            }
            MDIO_WR if present => self.phy.write(reg, v as u16),
            // A write to an empty address, or a clause-45 opcode (not
            // supported: `MDIO_CFG` is left in clause-22 mode by every
            // client), goes nowhere.
            _ => {}
        }
        self.regs[slot] = done;
        self.irq_stat[0] |= irq;
    }
}

/// GENET is a 40-bit master on the SCB, which maps DRAM 1:1 (`dma-ranges`),
/// so an address inside the board's RAM is physical; on a 2 GB board Linux's
/// buffers can sit above the first gigabyte. Only an address past the RAM
/// carries the VPU's cache-alias bits, and folds away, as for EMMC2.
fn dma_addr(addr: u32, ram_len: usize) -> u32 {
    if (addr as usize) < ram_len {
        addr
    } else {
        addr & 0x3FFF_FFFF
    }
}

/// The Ethernet FCS (CRC-32, IEEE 802.3), as it follows the frame on the
/// wire: least significant byte first.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

impl MmioDevice for Genet {
    fn name(&self) -> &'static str {
        "genet"
    }

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32> {
        let word = self.read_word(offset & !3);
        let shift = (offset & 3) * 8;
        Ok(match width {
            Width::Word => word,
            Width::Half => (word >> shift) & 0xffff,
            Width::Byte => (word >> shift) & 0xff,
        })
    }

    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()> {
        let aligned = offset & !3;
        let v = match width {
            Width::Word => value,
            _ => {
                let shift = (offset & 3) * 8;
                let mask = if width == Width::Half { 0xffff } else { 0xff } << shift;
                (self.reg(aligned) & !mask) | ((value << shift) & mask)
            }
        };
        self.write_word(aligned, v);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::periph::bcm54213pe::{BMCR, BMCR_RESET, BMSR, PHYSID1, PHYSID2};

    fn rd(g: &mut Genet, off: u32) -> u32 {
        g.read(off, Width::Word).unwrap()
    }

    fn wr(g: &mut Genet, off: u32, v: u32) {
        g.write(off, Width::Word, v).unwrap();
    }

    /// `unimac_mdio_read` / the bootloader's `0x00091952`: write the command,
    /// read it back, set `START_BUSY`, poll, then check `READ_FAIL`.
    fn mdio_read(g: &mut Genet, pmd: u32, reg: u32) -> Result<u16, u32> {
        wr(
            g,
            UMAC_MDIO_CMD,
            MDIO_RD | pmd << MDIO_PMD_SHIFT | reg << MDIO_REG_SHIFT,
        );
        let cmd = rd(g, UMAC_MDIO_CMD) | MDIO_START_BUSY;
        wr(g, UMAC_MDIO_CMD, cmd);
        let cmd = rd(g, UMAC_MDIO_CMD);
        assert_eq!(cmd & MDIO_START_BUSY, 0, "busy clears");
        if cmd & MDIO_READ_FAIL != 0 {
            Err(cmd)
        } else {
            Ok(cmd as u16)
        }
    }

    fn mdio_write(g: &mut Genet, pmd: u32, reg: u32, v: u16) -> u32 {
        let cmd = MDIO_WR | pmd << MDIO_PMD_SHIFT | reg << MDIO_REG_SHIFT | u32::from(v);
        wr(g, UMAC_MDIO_CMD, cmd);
        wr(g, UMAC_MDIO_CMD, cmd | MDIO_START_BUSY);
        rd(g, UMAC_MDIO_CMD)
    }

    #[test]
    fn rev_ctrl_reads_as_genet_v5() {
        let mut g = Genet::new();
        wr(&mut g, SYS_REV_CTRL, 0);
        let reg = rd(&mut g, SYS_REV_CTRL);
        assert_eq!(reg, REV_CTRL_VALUE);
        // bcmgenet_set_hw_params: majors 6 and 7 are GENET v5.
        assert_eq!((reg >> 24) & 0xf, 6);
        assert_eq!(reg & 0xffff, 0, "EPHY: 0x0000");
    }

    #[test]
    fn mdio_reads_the_phy_id_at_address_1() {
        let mut g = Genet::new();
        let id1 = mdio_read(&mut g, 1, u32::from(PHYSID1)).unwrap();
        let id2 = mdio_read(&mut g, 1, u32::from(PHYSID2)).unwrap();
        assert_eq!(u32::from(id1) << 16 | u32::from(id2), bcm54213pe::PHY_ID);
        assert_ne!(rd(&mut g, INTRL2_0) & UMAC_IRQ_MDIO_DONE, 0);
    }

    #[test]
    fn mdio_read_of_an_empty_address_fails() {
        let mut g = Genet::new();
        let cmd = mdio_read(&mut g, 2, 2).unwrap_err();
        assert_eq!(cmd & 0xffff, 0xffff);
        assert_ne!(rd(&mut g, INTRL2_0) & UMAC_IRQ_MDIO_ERROR, 0);
        // The next good read clears the failure.
        assert!(mdio_read(&mut g, 1, u32::from(BMSR)).is_ok());
    }

    #[test]
    fn mdio_write_reaches_the_phy_and_does_not_fail() {
        let mut g = Genet::new();
        let cmd = mdio_write(&mut g, 1, u32::from(BMCR), BMCR_RESET);
        assert_eq!(cmd & (MDIO_START_BUSY | MDIO_READ_FAIL), 0);
        let bmcr = mdio_read(&mut g, 1, u32::from(BMCR)).unwrap();
        assert_eq!(bmcr & BMCR_RESET, 0, "soft reset self-clears");
        assert_eq!(bmcr, 0x1140);
    }

    #[test]
    fn bmsr_reports_no_link() {
        let mut g = Genet::new();
        let bmsr = mdio_read(&mut g, 1, u32::from(BMSR)).unwrap();
        assert_eq!(bmsr, 0x7949);
        assert_eq!(rd(&mut g, UMAC_MODE) & (1 << 5), 0);
    }

    #[test]
    fn dma_status_follows_ctrl() {
        let mut g = Genet::new();
        // Out of reset: everything disabled, descriptor RAM not busy.
        assert_eq!(rd(&mut g, TDMA_REGS + DMA_STATUS), 0x3_ffff);
        assert_eq!(rd(&mut g, TDMA_REGS + DMA_STATUS) & (1 << 18), 0);
        // Linux enables rings 0..4 and DMA_EN: the measured pair.
        wr(&mut g, TDMA_REGS + DMA_CTRL, 0x3f);
        assert_eq!(rd(&mut g, TDMA_REGS + DMA_STATUS), 0x3_ffc0);
        wr(&mut g, RDMA_REGS + DMA_CTRL, 0x03);
        assert_eq!(rd(&mut g, RDMA_REGS + DMA_STATUS), 0x3_fffc);
        // bcmgenet_tdma_disable: clear the ring and enable bits, wait for
        // (status & mask) == mask.
        wr(&mut g, TDMA_REGS + DMA_CTRL, 0);
        let mask = 0x3f;
        assert_eq!(rd(&mut g, TDMA_REGS + DMA_STATUS) & mask, mask);
    }

    #[test]
    fn umac_mode_follows_cmd() {
        let mut g = Genet::new();
        wr(&mut g, UMAC_CMD, 0xb);
        // Measured 0x3a with the link up; no link here.
        assert_eq!(rd(&mut g, UMAC_MODE), 0x1a);
    }

    #[test]
    fn intrl2_set_clear_and_mask() {
        let mut g = Genet::new();
        assert_eq!(rd(&mut g, INTRL2_0 + INTRL2_CPU_MASK_STATUS), u32::MAX);
        wr(&mut g, INTRL2_1 + INTRL2_CPU_SET, 0x11);
        wr(&mut g, INTRL2_1 + INTRL2_CPU_CLEAR, 0x01);
        assert_eq!(rd(&mut g, INTRL2_1 + INTRL2_CPU_STAT), 0x10);
        wr(&mut g, INTRL2_1 + INTRL2_CPU_MASK_CLEAR, 0xff);
        assert_eq!(rd(&mut g, INTRL2_1 + INTRL2_CPU_MASK_STATUS), !0xff);
        assert_eq!(rd(&mut g, INTRL2_0 + INTRL2_CPU_STAT), 0);
    }

    /// A backend that records what it is sent and hands out a fixed queue.
    struct Wire {
        sent: Sent,
        incoming: std::collections::VecDeque<Vec<u8>>,
    }

    impl NetBackend for Wire {
        fn name(&self) -> &'static str {
            "test wire"
        }
        fn send(&mut self, frame: &[u8]) {
            self.sent.borrow_mut().push(frame.to_vec());
        }
        fn recv(&mut self) -> Option<Vec<u8>> {
            self.incoming.pop_front()
        }
    }

    const MAC: [u8; 6] = [0x02, 0x00, 0x5e, 0x00, 0x53, 0x01];

    /// Linux's bring-up of ring 16 in both directions with `n` descriptors
    /// starting at descriptor 0 (`bcmgenet_init_{tx,rx}_ring`), the MAC
    /// address set and the MAC on.
    fn bring_up(g: &mut Genet, n: u32) {
        for (rings, regs) in [(RDMA_RINGS, RDMA_REGS), (TDMA_RINGS, TDMA_REGS)] {
            let base = rings + 16 * 0x40;
            wr(g, base + RING_BUF_SIZE, n << 16 | 2048);
            wr(g, base + RING_START, 0);
            wr(g, base + RING_PTR, 0);
            wr(g, base + RING_END, n * 3 - 1);
            wr(g, regs + DMA_CTRL, DMA_EN | 1 << 17);
        }
        wr(g, UMAC_MAC0, 0x0200_5e00);
        wr(g, UMAC_MAC1, 0x5301);
        wr(g, RBUF_CTRL, 0xc043);
        wr(g, TBUF_CTRL, TBUF_64B_EN);
        wr(g, UMAC_CMD, CMD_TX_EN | CMD_RX_EN | 2 << CMD_SPEED_SHIFT);
    }

    fn frame(dst: [u8; 6], len: usize) -> Vec<u8> {
        let mut f = dst.to_vec();
        f.extend_from_slice(&[0x02, 0, 0x5e, 0, 0x53, 0x02, 0x08, 0x00]);
        f.resize(len, 0xa5);
        f
    }

    type Sent = std::rc::Rc<std::cell::RefCell<Vec<Vec<u8>>>>;

    fn attach(g: &mut Genet, incoming: Vec<Vec<u8>>) -> (Option<Box<dyn NetBackend>>, Sent) {
        g.phy.set_link(true);
        let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let wire = Wire {
            sent: sent.clone(),
            incoming: incoming.into(),
        };
        (Some(Box::new(wire)), sent)
    }

    #[test]
    fn transmit_sends_sop_to_eop_without_the_status_block_and_advances_cons() {
        let mut g = Genet::new();
        let mut ram = Ram::new(0, 1 << 20);
        let (mut net, sent) = attach(&mut g, vec![]);
        bring_up(&mut g, 4);
        let payload = frame(crate::net::BROADCAST, 100);
        let mut tsb_and_frame = vec![0xee; 64];
        tsb_and_frame.extend_from_slice(&payload);
        // Two descriptors for one frame: TSB + header, then the rest.
        ram.write_slice(0x1000, &tsb_and_frame[..80]).unwrap();
        ram.write_slice(0x2000, &tsb_and_frame[80..]).unwrap();
        wr(&mut g, TDMA_DESC, 80 << 16 | DESC_SOP);
        wr(&mut g, TDMA_DESC + 4, 0xC000_1000); // a VPU alias folds away
        wr(
            &mut g,
            TDMA_DESC + 12,
            (tsb_and_frame.len() as u32 - 80) << 16 | DESC_EOP,
        );
        wr(&mut g, TDMA_DESC + 16, 0x2000);
        let base = TDMA_RINGS + 16 * 0x40;
        wr(&mut g, base + TDMA_PROD_INDEX, 2);
        assert!(g.take_kick());
        g.service(0, &mut ram, &mut net);
        assert_eq!(*sent.borrow(), vec![payload]);
        assert_eq!(rd(&mut g, base + TDMA_CONS_INDEX), 2);
        assert_eq!(rd(&mut g, base + RING_PTR), 6);
        assert_ne!(rd(&mut g, INTRL2_0) & UMAC_IRQ_TXDMA_MBDONE, 0);
        assert_eq!(g.irq_lines(), [false, false], "masked");
        wr(
            &mut g,
            INTRL2_0 + INTRL2_CPU_MASK_CLEAR,
            UMAC_IRQ_TXDMA_MBDONE,
        );
        assert_eq!(g.irq_lines(), [true, false]);
    }

    #[test]
    fn transmit_without_link_is_consumed_but_dropped() {
        let mut g = Genet::new();
        let mut ram = Ram::new(0, 1 << 20);
        let mut net: Option<Box<dyn NetBackend>> = None;
        bring_up(&mut g, 4);
        wr(&mut g, TDMA_DESC, 100 << 16 | DESC_SOP | DESC_EOP);
        let base = TDMA_RINGS + 16 * 0x40;
        wr(&mut g, base + TDMA_PROD_INDEX, 1);
        g.service(0, &mut ram, &mut net);
        assert_eq!(rd(&mut g, base + TDMA_CONS_INDEX), 1);
        assert_eq!(g.stats.tx_dropped, 1);
    }

    #[test]
    fn ow_crc_keeps_the_fcs_slot_off_the_wire() {
        let mut g = Genet::new();
        let mut ram = Ram::new(0, 1 << 20);
        let (mut net, sent) = attach(&mut g, vec![]);
        bring_up(&mut g, 4);
        wr(&mut g, TBUF_CTRL, 0);
        // The bootloader's ARP: 60 bytes and four zero bytes for the FCS, with
        // the length_status flags it and start4 always use (0x7fe0: SOP, EOP,
        // the qtag bits, APPEND_CRC and OW_CRC).
        let payload = frame(crate::net::BROADCAST, 60);
        let mut buf = payload.clone();
        buf.extend_from_slice(&[0; FCS_LEN]);
        ram.write_slice(0x1000, &buf).unwrap();
        wr(&mut g, TDMA_DESC, (buf.len() as u32) << 16 | 0x7fe0);
        wr(&mut g, TDMA_DESC + 4, 0x1000);
        wr(&mut g, TDMA_RINGS + 16 * 0x40 + TDMA_PROD_INDEX, 1);
        g.service(0, &mut ram, &mut net);
        assert_eq!(*sent.borrow(), vec![payload]);
    }

    #[test]
    fn receive_fills_ring_16_with_status_block_and_2_byte_pad() {
        let mut g = Genet::new();
        let mut ram = Ram::new(0, 1 << 20);
        let other = [0x02, 0, 0x5e, 0, 0x53, 0x07];
        let (mut net, _) = attach(
            &mut g,
            vec![
                frame(crate::net::BROADCAST, 42),
                frame(other, 100),
                frame(MAC, 200),
                frame(MAC, 300),
                frame(MAC, 400),
            ],
        );
        bring_up(&mut g, 2);
        for i in 0..2 {
            wr(&mut g, RDMA_DESC + i * 12 + 4, 0x4000 + i * 0x1000);
        }
        let base = RDMA_RINGS + 16 * 0x40;
        g.service(0, &mut ram, &mut net);
        // Two buffers: the broadcast (padded to 60) and the frame for us; the
        // one for another MAC in between is filtered.
        assert_eq!(rd(&mut g, base + RDMA_PROD_INDEX), 2);
        assert_eq!(g.stats.rx_filtered, 1);
        let ls0 = rd(&mut g, RDMA_DESC);
        assert_eq!(ls0 >> 16, 64 + 2 + 60);
        assert_eq!(ls0 & 0xffff, DESC_SOP | DESC_EOP | DESC_RX_BRDCAST);
        let status = ram.read_slice(0x4000, 4).unwrap();
        assert_eq!(u32::from_le_bytes(status.try_into().unwrap()), ls0);
        assert_eq!(
            ram.read_slice(0x4000 + 66, 6).unwrap(),
            crate::net::BROADCAST
        );
        assert_eq!(rd(&mut g, RDMA_DESC + 12) >> 16, 64 + 2 + 200);
        // Ring full: the rest waits in the backend until the driver consumes.
        assert_eq!(rd(&mut g, base + RING_PTR), 0, "wrapped");
        wr(&mut g, base + RDMA_CONS_INDEX, 1);
        assert!(g.take_kick());
        g.service(0, &mut ram, &mut net);
        assert_eq!(rd(&mut g, base + RDMA_PROD_INDEX), 3);
        assert_eq!(rd(&mut g, RDMA_DESC) >> 16, 64 + 2 + 300);
        assert_eq!(g.stats.rx, 3);
    }

    /// Linux since 3b5d4f5a820d (`bcmgenet_init_rx_queues`,
    /// `bcmgenet_hfb_clear`): ring 0 over all 256 receive descriptors, ring 16
    /// off, and an empty 4-byte HFB filter 0 sending the default flow to ring
    /// 0.
    fn linux_rx_ring_0(g: &mut Genet) {
        wr(g, RDMA_RINGS + RING_BUF_SIZE, 256 << 16 | 2048);
        wr(g, RDMA_RINGS + RING_START, 0);
        wr(g, RDMA_RINGS + RING_PTR, 0);
        wr(g, RDMA_RINGS + RING_END, 256 * 3 - 1);
        // Filter 0's length is the low byte of the last length word.
        wr(g, HFB_FLT_LEN + 4 * 11, 4);
        wr(g, HFB_FLT_ENABLE + 4, 1);
        wr(g, HFB_CTRL, HFB_EN);
        wr(g, RDMA_REGS + DMA_CTRL, DMA_EN | 1 << 1);
    }

    #[test]
    fn hfb_filter_0_routes_the_default_flow_to_ring_0() {
        let mut g = Genet::new();
        let mut ram = Ram::new(0, 1 << 20);
        let (mut net, _) = attach(&mut g, vec![frame(MAC, 100)]);
        bring_up(&mut g, 2);
        linux_rx_ring_0(&mut g);
        wr(&mut g, RDMA_DESC + 4, 0x4000);
        g.service(0, &mut ram, &mut net);
        assert_eq!(rd(&mut g, RDMA_RINGS + RDMA_PROD_INDEX), 1);
        assert_eq!(rd(&mut g, RDMA_DESC) >> 16, 64 + 2 + 100);
        assert_eq!(ram.read_slice(0x4000 + 66, 6).unwrap(), MAC);
        // Ring 0's receive bit in INTRL2_1, not ring 16's in INTRL2_0.
        assert_eq!(rd(&mut g, INTRL2_1), 1 << 16);
        assert_eq!(rd(&mut g, INTRL2_0) & UMAC_IRQ_RXDMA_MBDONE, 0);
        assert_eq!(g.stats.rx, 1);
    }

    #[test]
    fn a_higher_hfb_filter_wins_over_the_catch_all() {
        let mut g = Genet::new();
        let mut ram = Ram::new(0, 1 << 20);
        let (mut net, _) = attach(
            &mut g,
            vec![frame(MAC, 100), frame(crate::net::BROADCAST, 100)],
        );
        bring_up(&mut g, 2);
        linux_rx_ring_0(&mut g);
        // An ethtool rule at location 0 (filter 1): our destination MAC, to
        // ring 1 — RX_CLS_FLOW_DISC with one receive queue, a ring that is off.
        for (i, pair) in MAC.chunks(2).enumerate() {
            let word = 0xf_0000 | u32::from(pair[0]) << 8 | u32::from(pair[1]);
            wr(&mut g, HFB_RAM + 4 * (HFB_FILTER_WORDS + i) as u32, word);
        }
        wr(&mut g, HFB_FLT_LEN + 4 * 11, 6 << 8 | 4);
        wr(&mut g, HFB_FLT_ENABLE + 4, 0b11);
        wr(&mut g, RDMA_REGS + DMA_INDEX2RING, 1 << 4);
        wr(&mut g, RDMA_DESC + 4, 0x4000);
        g.service(0, &mut ram, &mut net);
        assert_eq!(g.stats.rx_dropped, 1, "the rule's ring is off");
        assert_eq!(g.stats.rx, 1);
        assert_eq!(rd(&mut g, RDMA_DESC) & DESC_RX_BRDCAST, DESC_RX_BRDCAST);
    }

    #[test]
    fn crc_fwd_appends_the_fcs() {
        // The classic check value: CRC-32 of "123456789" is 0xcbf43926.
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        let mut g = Genet::new();
        let mut ram = Ram::new(0, 1 << 20);
        let f = frame(MAC, 64);
        let (mut net, _) = attach(&mut g, vec![f.clone()]);
        bring_up(&mut g, 2);
        wr(&mut g, RBUF_CTRL, 0);
        wr(&mut g, UMAC_CMD, CMD_TX_EN | CMD_RX_EN | CMD_CRC_FWD);
        wr(&mut g, RDMA_DESC + 4, 0x4000);
        g.service(0, &mut ram, &mut net);
        assert_eq!(rd(&mut g, RDMA_DESC) >> 16, 68);
        let fcs = ram.read_slice(0x4000 + 64, 4).unwrap();
        assert_eq!(fcs, crc32(&f).to_le_bytes());
    }

    #[test]
    fn descriptor_ram_is_storage() {
        let mut g = Genet::new();
        wr(&mut g, 0x2000 + 16 * 12, 0x0600_6000);
        assert_eq!(rd(&mut g, 0x2000 + 16 * 12), 0x0600_6000);
        g.write(0x4001, Width::Byte, 0xab).unwrap();
        assert_eq!(rd(&mut g, 0x4000), 0xab00);
    }
}
