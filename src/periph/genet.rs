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
//! # Not modelled yet: packet DMA and interrupts
//!
//! Descriptor RAM and ring registers are plain storage, so rings can be set
//! up and read back, but nothing moves packets: a TDMA producer-index bump
//! never advances the consumer index, and no frame is ever received. A ring
//! engine would hook in here the way DMA4 does in `Machine::store` (a
//! `take_...()` flag after a write, run with host memory), driving:
//!
//! * ring 16 (the bootloader / start4 default ring) and rings 0..4 (Linux),
//!   each block laid out as `genet_dma_ring_regs_v4`: `+0x00` RDMA write /
//!   TDMA read pointer, `+0x08` RDMA producer / TDMA consumer index, `+0x0C`
//!   RDMA consumer / TDMA producer index, `+0x10 RING_BUF_SIZE`,
//!   `+0x14 START_ADDR`, `+0x1C END_ADDR`, `+0x24 MBUF_DONE_THRESH`, `+0x28`
//!   RDMA XON/XOFF / TDMA flow period, `+0x2C` RDMA read / TDMA write
//!   pointer (each pointer and address with a `_HI` word at `+4`);
//! * `DMA_RING_CFG`, `DMA_CTRL`, `DMA_SCB_BURST_SIZE` and `INDEX2RING`;
//! * the descriptors (`length_status`, `address_lo`, `address_hi`);
//! * `INTRL2_0` `RXDMA_*` / `TXDMA_*` bits and `INTRL2_1` per-ring bits, and
//!   the two GIC lines (SPI 157 / 158 in the Pi 4 device tree).
//!
//! The MDIO-done / MDIO-error bits of `INTRL2_0` are set here on every
//! transaction, but no interrupt line is raised anywhere.

use super::bcm54213pe::{self, Bcm54213pe};
use crate::bus::{BusResult, MmioDevice, Width};

pub const SIZE: u32 = 0x1_0000;

// SYS
pub const SYS_REV_CTRL: u32 = 0x0000;
pub const SYS_PORT_CTRL: u32 = 0x0004;
pub const SYS_RBUF_FLUSH_CTRL: u32 = 0x0008;
pub const SYS_TBUF_FLUSH_CTRL: u32 = 0x000c;
// EXT
pub const EXT_PWR_MGMT: u32 = 0x0080;
pub const EXT_RGMII_OOB_CTRL: u32 = 0x008c;
// INTRL2 (two instances, 0x40 apart)
pub const INTRL2_0: u32 = 0x0200;
pub const INTRL2_1: u32 = 0x0240;
const INTRL2_CPU_STAT: u32 = 0x00;
const INTRL2_CPU_SET: u32 = 0x04;
const INTRL2_CPU_CLEAR: u32 = 0x08;
const INTRL2_CPU_MASK_STATUS: u32 = 0x0c;
const INTRL2_CPU_MASK_SET: u32 = 0x10;
const INTRL2_CPU_MASK_CLEAR: u32 = 0x14;
pub const UMAC_IRQ_MDIO_DONE: u32 = 1 << 23;
pub const UMAC_IRQ_MDIO_ERROR: u32 = 1 << 24;
// RBUF / TBUF
pub const RBUF_CTRL: u32 = 0x0300;
pub const TBUF_BP_MC: u32 = 0x060c;
// UMAC
pub const UMAC: u32 = 0x0800;
pub const UMAC_HD_BKP_CTRL: u32 = UMAC + 0x004;
pub const UMAC_CMD: u32 = UMAC + 0x008;
pub const UMAC_PAUSE_QUANTA: u32 = UMAC + 0x018;
pub const UMAC_MODE: u32 = UMAC + 0x044;
pub const UMAC_TX_IPG_LEN: u32 = UMAC + 0x05c;
pub const UMAC_EEE_CTRL: u32 = UMAC + 0x064;
pub const UMAC_MIB_START: u32 = UMAC + 0x400;
pub const UMAC_MIB_CTRL: u32 = UMAC + 0x580;
pub const UMAC_MDIO_CMD: u32 = UMAC + 0x614;
pub const UMAC_MDIO_CFG: u32 = UMAC + 0x618;
// DMA register blocks: rdma/tdma offset + 256 descriptors x 12 bytes +
// 17 rings x 0x40 (`GENET_RDMA_REG_OFF + DMA_RINGS_SIZE`).
pub const RDMA_REGS: u32 = 0x3040;
pub const TDMA_REGS: u32 = 0x5040;
const DMA_CTRL: u32 = 0x04;
const DMA_STATUS: u32 = 0x08;

// UMAC_CMD bits
const CMD_SPEED_SHIFT: u32 = 2;
const CMD_HD_EN: u32 = 1 << 10;
const CMD_RX_PAUSE_IGNORE: u32 = 1 << 8;
const CMD_TX_PAUSE_IGNORE: u32 = 1 << 28;
// MDIO_CMD bits
pub const MDIO_START_BUSY: u32 = 1 << 29;
pub const MDIO_READ_FAIL: u32 = 1 << 28;
pub const MDIO_RD: u32 = 2 << 26;
pub const MDIO_WR: u32 = 1 << 26;
pub const MDIO_PMD_SHIFT: u32 = 21;
pub const MDIO_REG_SHIFT: u32 = 16;

/// `SYS_REV_CTRL`, measured: major 6 in bits 27:24, which `bcmgenet` maps to
/// GENET v5 (`GENET 5.0 EPHY: 0x0000`).
pub const REV_CTRL_VALUE: u32 = 0x0600_0000;

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
        // nor by Linux for an RGMII PHY (see the module docs).
        set(EXT_PWR_MGMT, 0x051f_02c3);
        set(TBUF_BP_MC, 0x0000_ffff);
        set(UMAC_HD_BKP_CTRL, 0x0000_0014);
        set(UMAC_PAUSE_QUANTA, 0x0000_ffff);
        set(UMAC_TX_IPG_LEN, 0x0000_3c00);
        set(UMAC_MDIO_CFG, 0x0000_0091);
        // Measured 0x00f00050: Linux sets RGMII_MODE_EN (bit 6) and RGMII_LINK
        // (bit 4) and leaves bits 23:20 alone; the bootloader writes 0xf000xx
        // itself.
        set(EXT_RGMII_OOB_CTRL, 0x00f0_0000);
        // Measured 0xc043: Linux only sets bits 1:0 (RBUF_ALIGN_2B,
        // RBUF_64B_EN).
        set(RBUF_CTRL, 0x0000_c040);
        // Measured 0x48: Linux sets EEE_EN (bit 3) once EEE is active.
        set(UMAC_EEE_CTRL, 0x0000_0040);
        Genet {
            regs,
            irq_stat: [0; 2],
            // Everything masked until a driver unmasks it.
            irq_mask: [u32::MAX; 2],
            phy: Bcm54213pe::new(),
        }
    }

    fn reg(&self, off: u32) -> u32 {
        self.regs[(off / 4) as usize]
    }

    fn read_word(&self, off: u32) -> u32 {
        match off {
            SYS_REV_CTRL => REV_CTRL_VALUE,
            INTRL2_0..=0x027f => {
                let i = ((off - INTRL2_0) / 0x40) as usize;
                match (off - INTRL2_0) % 0x40 {
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
            INTRL2_0..=0x027f => {
                let i = ((off - INTRL2_0) / 0x40) as usize;
                match (off - INTRL2_0) % 0x40 {
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
    }

    /// `UMAC_MODE` (read-only): bits 1:0 speed, bit 2 half duplex, bit 3 RX
    /// pause, bit 4 TX pause, bit 5 link. Speed, duplex and pause follow
    /// `UMAC_CMD`; the link bit is the RGMII link, which is down with no
    /// cable. Measured `0x3a` with `UMAC_CMD = 0xb` and the link up.
    fn umac_mode(&self) -> u32 {
        let cmd = self.reg(UMAC_CMD);
        let mut mode = (cmd >> CMD_SPEED_SHIFT) & 3;
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
        match v & (3 << 26) {
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

    #[test]
    fn descriptor_ram_is_storage() {
        let mut g = Genet::new();
        wr(&mut g, 0x2000 + 16 * 12, 0x0600_6000);
        assert_eq!(rd(&mut g, 0x2000 + 16 * 12), 0x0600_6000);
        g.write(0x4001, Width::Byte, 0xab).unwrap();
        assert_eq!(rd(&mut g, 0x4000), 0xab00);
    }
}
