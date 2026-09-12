//! The Raspberry Pi 4B's gigabit Ethernet PHY: a Broadcom BCM54213PE on
//! GENET's UniMAC MDIO bus (see [`super::genet`]) at address 1
//! (`/scb/ethernet@7d580000/mdio@e14/ethernet-phy@1`), wired to the MAC over
//! RGMII (`phy-mode = "rgmii-rxid"`).
//!
//! Three clients drive it: the EEPROM bootloader's network boot (`GENET:
//! RESET_PHY`, `CTL %04x PHY ID %04x %03x`, then a wait for link), start4's
//! network path (`FUN_0ecc3198` / `FUN_0ecc3280` are its MDIO read / write,
//! `FUN_0ecc3260` / `FUN_0ecc3220` the `0x1C` shadow write / read), and Linux
//! (`drivers/net/phy/broadcom.c`, `bcm54xx_config_init`, and the generic
//! clause-22 code in `phylib`).
//!
//! # Ground truth
//!
//! Read through the kernel (`SIOCGMIIREG` on `eth0`) on a Pi 4B rev 1.5
//! (d03115) with Linux up and a 1 Gb/s link:
//!
//! ```text
//!   0x00 BMCR     0x1140    0x01 BMSR     0x796d    0x02/03 ID  0x600d 0x84a2
//!   0x04 ANAR     0x0de1    0x05 ANLPAR   0xc5e1    0x06 ANER   0x006d
//!   0x07 NPTX     0x2001    0x09 CTRL1000 0x0300    0x0a STAT1000 0x0800
//!   0x0f ESTATUS  0x3000    0x18 AUX_CTL  0x71e7    0x1b IMR    0xfff1
//!   0x1c SHADOW   0x386a
//! ```
//!
//! `ethtool --show-eee eth0` on the same board: EEE supported and advertised
//! for 100baseT/Full and 1000baseT/Full.
//!
//! # Link
//!
//! Without a cable ([`Bcm54213pe::set_link`] false, the default) there is no
//! link partner: auto-negotiation never completes and the link stays down.
//! BMSR reads the measured value without its link-status (bit 2) and
//! AN-complete (bit 5) bits, and the link partner registers read 0. Linux
//! reports `eth0: Link is Down` and stops there; the bootloader's network
//! boot waits for link and times out.
//!
//! With a cable the partner is the measured one: a 1 Gb/s full-duplex switch
//! port with pause, and every link-partner register reads what the reference
//! board read. Negotiation completes as soon as it is (re)started, so the
//! link is up at the first poll. The bootloader's wait-for-link loop polls
//! only BMSR, LPA and STAT1000 (MDIO reads of registers 1, 5 and 10 from
//! `0x0009196e`).
//!
//! # Register access paths
//!
//! Besides the clause-22 registers the part has three indirect spaces, all
//! used by `broadcom.c` / `bcm-phy-lib.c`:
//!
//! * `0x18` auxiliary control: eight shadow registers, selected by bits 2:0
//!   of a write. A write to shadow 7 (misc) with bit 15 clear instead selects
//!   which shadow the next read of `0x18` returns (bits 14:12).
//! * `0x1C`: thirty-two shadow registers, bits 14:10 select, bit 15 = write,
//!   bits 9:0 data. A write with bit 15 clear selects the shadow to read.
//! * `0x17` / `0x15`: expansion registers — `0x17` takes the selector
//!   (`0x0Fxx`, `0x0Dxx`, `0x0Exx` banks), `0x15` reads or writes it.
//!
//! and the IEEE clause-45-over-22 MMD window at `0x0D` / `0x0E`, which
//! `phylib` uses for the EEE registers.
//!
//! Registers with no documented behaviour here read back what was last
//! written (defaults 0). Everything is deterministic: soft reset, restart of
//! auto-negotiation and power-down complete immediately.

use std::collections::BTreeMap;

/// MDIO address on the Pi 4B (`ethernet-phy@1`).
pub const ADDR: u8 = 1;
/// `PHY_ID_BCM54213PE` in `include/linux/brcmphy.h`; measured `0x600d` /
/// `0x84a2` in registers 2 / 3.
pub const PHY_ID: u32 = 0x600d_84a2;

pub const BMCR: u8 = 0x00;
pub const BMSR: u8 = 0x01;
pub const PHYSID1: u8 = 0x02;
pub const PHYSID2: u8 = 0x03;
pub const ADVERTISE: u8 = 0x04;
pub const LPA: u8 = 0x05;
pub const EXPANSION: u8 = 0x06;
pub const NPTX: u8 = 0x07;
pub const CTRL1000: u8 = 0x09;
pub const STAT1000: u8 = 0x0a;
pub const MMD_CTRL: u8 = 0x0d;
pub const MMD_DATA: u8 = 0x0e;
pub const ESTATUS: u8 = 0x0f;
pub const ECR: u8 = 0x10;
pub const EXP_DATA: u8 = 0x15;
pub const EXP_SEL: u8 = 0x17;
pub const AUX_CTL: u8 = 0x18;
pub const ISR: u8 = 0x1a;
pub const IMR: u8 = 0x1b;
pub const SHADOW: u8 = 0x1c;

pub const BMCR_RESET: u16 = 1 << 15;
pub const BMCR_ANRESTART: u16 = 1 << 9;
/// Reset value: auto-negotiation enabled, full duplex, speed-select 1000
/// (measured `0x1140`).
const BMCR_DEFAULT: u16 = 0x1140;

/// BMSR as measured (`0x796d`) less link status (bit 2) and AN complete
/// (bit 5): 10/100 half/full, extended status, preamble suppression, AN
/// ability, extended capabilities.
const BMSR_NO_LINK: u16 = 0x7949;
/// BMSR with a partner (measured): adds link status and AN complete.
const BMSR_LINK: u16 = 0x796d;
/// Link partner abilities with a partner (measured): 10/100 half/full, pause
/// and asymmetric pause, acknowledge, next page.
const LPA_LINK: u16 = 0xc5e1;
/// ANER with a partner (measured): partner AN able, page received, next page
/// able on both ends.
const ANER_LINK: u16 = 0x006d;
/// STAT1000 with a partner (measured): partner is 1000BASE-T full duplex.
const STAT1000_LINK: u16 = 0x0800;
/// Selector 802.3, 10/100 half/full, no pause. The measured `0x0de1` is this
/// plus the pause bits Linux adds.
const ADVERTISE_DEFAULT: u16 = 0x01e1;
/// ANER with nobody on the other end: only "local next page able" (bit 2 of
/// the measured `0x006d`).
const ANER_NO_PARTNER: u16 = 0x0004;
const NPTX_DEFAULT: u16 = 0x2001;
/// Advertise 1000BASE-T half and full (measured `0x0300`).
const CTRL1000_DEFAULT: u16 = 0x0300;
/// 1000BASE-T full and half capable (measured `0x3000`).
const ESTATUS_VALUE: u16 = 0x3000;
/// "Initially all interrupts are masked in IMR" (`bcm54xx_config_init`).
const IMR_DEFAULT: u16 = 0xffff;

const AUX_SHDW_MISC: usize = 7;
const AUX_MISC_WREN: u16 = 1 << 15;
/// Misc shadow data bits. The measured `0x71e7` is the read-select field
/// (`0x7000`) and selector (`7`) around `0x01e0`: RGMII RX skew (bit 8, which
/// Linux sets for `rgmii-rxid`), RGMII enable (bit 7) and the two reserved
/// bits 6:5 `broadcom.c` says must read 0b11.
const AUX_MISC_DATA: u16 = 0x0ff8;
const AUX_MISC_DEFAULT: u16 = 0x00e0;

const SHD_WRITE: u16 = 1 << 15;
/// `BCM54XX_SHD_LEDS2`. Linux rewrites bits 3:0 and preserves bits 7:4
/// (LED4); the measured `0x386a` leaves 6 there.
const SHD_LEDS2: usize = 0x0e;

/// `BCM54XX_WOL_INT_STATUS`, clear-on-read; nothing ever raises it here.
const EXP_WOL_INT_STATUS: u16 = 0x0e94;

/// MMD devices and registers `phylib` reads for EEE.
const MMD_PCS: u16 = 3;
const MMD_AN: u16 = 7;
const MDIO_PCS_EEE_ABLE: u16 = 20;
const MDIO_AN_EEE_ADV: u16 = 60;
const MDIO_AN_EEE_LPABLE: u16 = 61;
/// EEE for 100BASE-TX and 1000BASE-T (`ethtool --show-eee`, measured).
const EEE_100TX_1000T: u16 = 0x0006;

#[derive(Debug, Clone)]
pub struct Bcm54213pe {
    /// A cable to a link partner is plugged in.
    link: bool,
    bmcr: u16,
    advertise: u16,
    nptx: u16,
    ctrl1000: u16,
    ecr: u16,
    imr: u16,
    /// Clause-22 registers with no behaviour of their own: last written value.
    plain: [u16; 32],
    aux: [u16; 8],
    aux_read_sel: usize,
    shadow: [u16; 32],
    shadow_read_sel: usize,
    exp_sel: u16,
    exp: BTreeMap<u16, u16>,
    mmd_ctrl: u16,
    mmd_addr: [u16; 32],
    eee_adv: u16,
}

impl Default for Bcm54213pe {
    fn default() -> Self {
        Bcm54213pe::new()
    }
}

impl Bcm54213pe {
    pub fn new() -> Bcm54213pe {
        let mut aux = [0; 8];
        aux[AUX_SHDW_MISC] = AUX_MISC_DEFAULT;
        let mut shadow = [0; 32];
        shadow[SHD_LEDS2] = 0x060;
        Bcm54213pe {
            link: false,
            bmcr: BMCR_DEFAULT,
            advertise: ADVERTISE_DEFAULT,
            nptx: NPTX_DEFAULT,
            ctrl1000: CTRL1000_DEFAULT,
            ecr: 0,
            imr: IMR_DEFAULT,
            plain: [0; 32],
            aux,
            aux_read_sel: 0,
            shadow,
            shadow_read_sel: 0,
            exp_sel: 0,
            exp: BTreeMap::new(),
            mmd_ctrl: 0,
            mmd_addr: [0; 32],
            eee_adv: EEE_100TX_1000T,
        }
    }

    /// Plug (`true`) or unplug the cable.
    pub fn set_link(&mut self, link: bool) {
        self.link = link;
    }

    /// The link is up: a partner is there and auto-negotiation has finished.
    pub fn link_up(&self) -> bool {
        self.link
    }

    /// Clause-22 register read, as the MDIO controller sees it.
    pub fn read(&mut self, reg: u8) -> u16 {
        match reg & 0x1f {
            BMCR => self.bmcr,
            BMSR if self.link => BMSR_LINK,
            BMSR => BMSR_NO_LINK,
            LPA if self.link => LPA_LINK,
            EXPANSION if self.link => ANER_LINK,
            STAT1000 if self.link => STAT1000_LINK,
            PHYSID1 => (PHY_ID >> 16) as u16,
            PHYSID2 => PHY_ID as u16,
            ADVERTISE => self.advertise,
            EXPANSION => ANER_NO_PARTNER,
            NPTX => self.nptx,
            CTRL1000 => self.ctrl1000,
            // Nothing on the far end: no partner abilities, no 1000BASE-T
            // status, no AUX status (0x19), no interrupt pending.
            LPA | 0x08 | STAT1000 | 0x11..=0x14 | 0x19 | ISR => 0,
            MMD_CTRL => self.mmd_ctrl,
            MMD_DATA => self.mmd_data_read(),
            ESTATUS => ESTATUS_VALUE,
            ECR => self.ecr,
            EXP_SEL => self.exp_sel,
            EXP_DATA => match self.exp_sel {
                EXP_WOL_INT_STATUS => 0,
                sel => self.exp.get(&sel).copied().unwrap_or(0),
            },
            AUX_CTL => {
                let sel = self.aux_read_sel;
                if sel == AUX_SHDW_MISC {
                    self.aux[sel] | 0x7000 | sel as u16
                } else {
                    self.aux[sel] | sel as u16
                }
            }
            IMR => self.imr,
            SHADOW => {
                let sel = self.shadow_read_sel;
                (sel as u16) << 10 | self.shadow[sel]
            }
            r => self.plain[usize::from(r)],
        }
    }

    /// Clause-22 register write.
    pub fn write(&mut self, reg: u8, v: u16) {
        match reg & 0x1f {
            BMCR => {
                if v & BMCR_RESET != 0 {
                    // Soft reset restores the defaults and self-clears; it
                    // completes before the next MDIO frame could observe it.
                    // The cable stays where it is.
                    *self = Bcm54213pe {
                        link: self.link,
                        ..Bcm54213pe::new()
                    };
                } else {
                    // Restart-AN self-clears. With no partner the restarted
                    // negotiation never completes; with one it is done by
                    // the next poll.
                    self.bmcr = v & !BMCR_ANRESTART;
                }
            }
            ADVERTISE => self.advertise = v,
            NPTX => self.nptx = v,
            CTRL1000 => self.ctrl1000 = v,
            MMD_CTRL => self.mmd_ctrl = v,
            MMD_DATA => self.mmd_data_write(v),
            ECR => self.ecr = v,
            EXP_SEL => self.exp_sel = v,
            EXP_DATA => {
                self.exp.insert(self.exp_sel, v);
            }
            AUX_CTL => {
                let sel = usize::from(v & 7);
                if sel == AUX_SHDW_MISC {
                    if v & AUX_MISC_WREN != 0 {
                        self.aux[sel] = v & AUX_MISC_DATA;
                    } else {
                        self.aux_read_sel = usize::from((v >> 12) & 7);
                    }
                } else {
                    self.aux[sel] = v & !7;
                    self.aux_read_sel = sel;
                }
            }
            IMR => self.imr = v,
            SHADOW => {
                let sel = usize::from((v >> 10) & 0x1f);
                if v & SHD_WRITE != 0 {
                    self.shadow[sel] = v & 0x3ff;
                }
                self.shadow_read_sel = sel;
            }
            // Read-only: status, IDs, partner registers, counters.
            BMSR
            | PHYSID1
            | PHYSID2
            | LPA
            | EXPANSION
            | 0x08
            | STAT1000
            | ESTATUS
            | 0x11..=0x14
            | 0x19
            | ISR => {}
            r => self.plain[usize::from(r)] = v,
        }
    }

    /// `0x0D` bits 15:14: 0 = address, else data (2 and 3 post-increment).
    fn mmd_function(&self) -> u16 {
        self.mmd_ctrl >> 14
    }

    fn mmd_dev(&self) -> u16 {
        self.mmd_ctrl & 0x1f
    }

    fn mmd_data_read(&mut self) -> u16 {
        let dev = self.mmd_dev();
        let addr = self.mmd_addr[usize::from(dev)];
        match self.mmd_function() {
            0 => addr,
            f => {
                let v = self.mmd_read(dev, addr);
                if f == 2 {
                    self.mmd_addr[usize::from(dev)] = addr.wrapping_add(1);
                }
                v
            }
        }
    }

    fn mmd_data_write(&mut self, v: u16) {
        let dev = self.mmd_dev();
        let addr = self.mmd_addr[usize::from(dev)];
        match self.mmd_function() {
            0 => self.mmd_addr[usize::from(dev)] = v,
            f => {
                if (dev, addr) == (MMD_AN, MDIO_AN_EEE_ADV) {
                    self.eee_adv = v & EEE_100TX_1000T;
                }
                if f >= 2 {
                    self.mmd_addr[usize::from(dev)] = addr.wrapping_add(1);
                }
            }
        }
    }

    fn mmd_read(&self, dev: u16, addr: u16) -> u16 {
        match (dev, addr) {
            (MMD_PCS, MDIO_PCS_EEE_ABLE) => EEE_100TX_1000T,
            (MMD_AN, MDIO_AN_EEE_ADV) => self.eee_adv,
            // No partner, so nothing advertised back.
            (MMD_AN, MDIO_AN_EEE_LPABLE) => 0,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_registers_carry_the_bcm54213pe_id() {
        let mut p = Bcm54213pe::new();
        let id = u32::from(p.read(PHYSID1)) << 16 | u32::from(p.read(PHYSID2));
        assert_eq!(id, PHY_ID);
    }

    #[test]
    fn soft_reset_restores_defaults_and_self_clears() {
        let mut p = Bcm54213pe::new();
        p.write(ADVERTISE, 0x0de1);
        p.write(CTRL1000, 0);
        p.write(BMCR, 0x0800); // power down
        p.write(BMCR, BMCR_RESET);
        assert_eq!(p.read(BMCR), BMCR_DEFAULT);
        assert_eq!(p.read(BMCR) & BMCR_RESET, 0);
        assert_eq!(p.read(ADVERTISE), ADVERTISE_DEFAULT);
        assert_eq!(p.read(CTRL1000), CTRL1000_DEFAULT);
    }

    #[test]
    fn autoneg_restart_self_clears_and_never_completes_without_a_partner() {
        let mut p = Bcm54213pe::new();
        p.write(BMCR, BMCR_DEFAULT | BMCR_ANRESTART);
        assert_eq!(p.read(BMCR), BMCR_DEFAULT);
        let bmsr = p.read(BMSR);
        assert_eq!(bmsr & (1 << 2), 0, "link down");
        assert_eq!(bmsr & (1 << 5), 0, "AN not complete");
        assert_ne!(bmsr & (1 << 3), 0, "AN capable");
        assert_eq!(p.read(LPA), 0);
    }

    #[test]
    fn with_a_cable_the_partner_registers_read_the_measured_values() {
        let mut p = Bcm54213pe::new();
        p.set_link(true);
        p.write(BMCR, BMCR_RESET);
        p.write(BMCR, BMCR_DEFAULT | BMCR_ANRESTART);
        assert_eq!(p.read(BMSR), 0x796d);
        assert_eq!(p.read(LPA), 0xc5e1);
        assert_eq!(p.read(EXPANSION), 0x006d);
        assert_eq!(p.read(STAT1000), 0x0800);
        p.set_link(false);
        assert_eq!(p.read(BMSR) & (1 << 2), 0);
    }

    #[test]
    fn aux_ctl_misc_shadow_reads_back_through_the_read_selector() {
        let mut p = Bcm54213pe::new();
        // bcm54xx_auxctl_read(MISC): select, then read.
        p.write(AUX_CTL, 0x7 | 7 << 12);
        let v = p.read(AUX_CTL);
        assert_eq!(v & 7, 7);
        // bcm54xx_config_clock_delay for rgmii-rxid: set RX skew, write back.
        p.write(AUX_CTL, v | AUX_MISC_WREN | 0x0100);
        p.write(AUX_CTL, 0x7 | 7 << 12);
        // Same as the board after Linux configured it.
        assert_eq!(p.read(AUX_CTL), 0x71e7);
    }

    #[test]
    fn shadow_1c_write_then_read() {
        let mut p = Bcm54213pe::new();
        p.write(SHADOW, SHD_WRITE | 0x0d << 10 | 0x0aa);
        p.write(SHADOW, 0x0e << 10);
        assert_eq!(p.read(SHADOW), 0x0e << 10 | 0x060);
        p.write(SHADOW, 0x0d << 10);
        assert_eq!(p.read(SHADOW), 0x0d << 10 | 0x0aa);
    }

    #[test]
    fn expansion_registers_are_selected_by_0x17() {
        let mut p = Bcm54213pe::new();
        p.write(EXP_SEL, 0x0f04);
        p.write(EXP_DATA, 0x0123);
        p.write(EXP_SEL, 0x0d00);
        assert_eq!(p.read(EXP_DATA), 0);
        p.write(EXP_SEL, 0x0f04);
        assert_eq!(p.read(EXP_DATA), 0x0123);
    }

    #[test]
    fn mmd_window_reports_eee_for_100tx_and_1000t() {
        let mut p = Bcm54213pe::new();
        // phy_read_mmd(3, 20) through the clause-22 indirection.
        p.write(MMD_CTRL, MMD_PCS);
        p.write(MMD_DATA, MDIO_PCS_EEE_ABLE);
        p.write(MMD_CTRL, 0x4000 | MMD_PCS);
        assert_eq!(p.read(MMD_DATA), EEE_100TX_1000T);
        // Nothing advertised by the (absent) partner.
        p.write(MMD_CTRL, MMD_AN);
        p.write(MMD_DATA, MDIO_AN_EEE_LPABLE);
        p.write(MMD_CTRL, 0x4000 | MMD_AN);
        assert_eq!(p.read(MMD_DATA), 0);
    }
}
