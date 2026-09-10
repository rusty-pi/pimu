//! BCM2711 PCIe root complex at `0x7D50_0000` — the block Linux calls
//! `pcie@7d500000` and drives with `pcie-brcmstb`.
//!
//! Behind it sits the Pi 4B's VIA VL805 xHCI controller (`1106:3483`), modelled
//! in [`super::vl805`]. See [`docs/usb-xhci.md`](../../../docs/usb-xhci.md) for
//! the register-level survey and the staged plan; this file is stages 0 and 1.
//!
//! The window sits outside the `0x7E…` legacy peripheral aperture, so without
//! it every PCIe access folds onto DRAM (`addr & 0x3FFF_FFFF` = `0x3D50_xxxx`)
//! and the firmware silently scribbles ~37 KiB into the middle of modelled
//! memory. Stage 0 fixed that with a sticky register file.
//!
//! ## What stage 1 adds
//!
//! * **`RGR1_SW_INIT_1` (`+0x9210`)** — bit 0 is PERST#, bit 1 the bridge soft
//!   reset, the same bits `pcie-brcmstb` uses. The bootcode parks the block in
//!   reset (`0x8000AB4A` writes `2` then `3`), the second-stage bootloader's
//!   `pcie_reset` (`0x000A7034`) re-asserts both, then `pcie_init` releases the
//!   bridge (`0x000A6CA2`) and finally PERST# (`0x000A6DCC`).
//! * **`MISC_PCIE_STATUS` (`+0x4068`)** — the link-up poll. The bootloader's
//!   predicate at `0x000A6F7E` is
//!
//!   ```text
//!   r2 = [0x7D504068]
//!   r3 = r2 & 0x20            ; DL_ACTIVE
//!   if r3 != 0: r2 &= 0x10    ; PHYLINKUP
//!   return (both set) ? 0 : 1 ; 0 = stop polling, link is up
//!   ```
//!
//!   which is exactly Linux's `brcm_pcie_link_up()`. Bit 7 is the port-mode bit
//!   `brcm_pcie_rc_mode()` reads; set, it means root complex. So a live link
//!   reads `0xB0`.
//! * **The config-space router.** `EXT_CFG_INDEX` (`+0x9000`) selects a
//!   `(bus, slot, fn)` — `bus << 20 | slot << 15 | fn << 12`, per
//!   `pcie-brcmstb` — and the 4 KiB window at `EXT_CFG_DATA` (`+0x8000`) is
//!   that function's configuration space. Bus 0 device 0 is the root port,
//!   whose own config space is also directly visible at `+0x0000`; bus 1
//!   device 0 is the VL805. Everything else reads back all-ones, which is how
//!   PCI says "nothing there".
//!
//! ## Why the endpoint is *detached* by default
//!
//! A real Pi 4B has the VL805 soldered on, so "no device" is not what the
//! hardware looks like. It is a deliberate modelling choice, not an accident of
//! what happened to be plugged in when the ground truth was captured, and it is
//! here for one reason: **the controller behind the link is not modelled yet.**
//!
//! Bringing the link up changes what the second-stage bootloader does. Instead
//! of timing out and printing `USB xHC init failed`, it scans the bus, finds
//! the VL805, uploads the VL805's own hub firmware through the vendor port at
//! config `0x78`/`0x7C` (`0x000B6CE0`) — which this model answers — and then
//! goes on to xHCI itself, which it does not.
//!
//! That is not a theory. With `RVF_PCIE_DEVICE=1` the transcript is
//!
//! ```text
//!   2.14 PCI0 init
//!   2.14 PCI0 reset
//!   2.75 PCIe scan 000014e4:00002711
//!   2.75 PCIe scan 00001106:00003483
//!   3.31 XHCI-STOP
//!   3.31 xHC0 ver: 0 HCS: 00000000 00000000 00000000 HCC: 00000000
//!   3.31 USBSTS 0
//! ```
//!
//! and then the boot **stops dead** — `end Stuck { pc: 0x000AA3C0 }`, a
//! `udelay` poll with nothing to wait for, 60 s of modelled silence, no SD
//! fall-through, no `arm_loader`. The all-zero capability words are the tell:
//! BAR0's 4 KiB of xHCI MMIO does not exist yet, so `HCIVERSION` reads 0 and
//! the bring-up waits forever for a controller that will never answer. This is
//! exactly the trade #18 warns about — a harmless 1.32 s timeout for a hard
//! stall — and it is why the flag defaults off.
//!
//! So the default is: link never trains, `MISC_PCIE_STATUS` reads `0`, the
//! bootloader prints `PCIe timeout: 0x00000000` / `USB xHC init failed` and
//! moves on to the next `BOOT_ORDER` entry — byte-identical to stage 0,
//! `boot check passed`. Flipping the default is a one-line change once stage 3
//! lands.
//!
//! ## Register offsets the firmware actually touches
//!
//! `0x0000..0x0FFF` root-port config space (`0x0B4`/`0x0B8`/`0x0C8`/`0x188`,
//! class code at `0x043C`), `0x4008` `MISC_MISC_CTRL`, `0x402C`/`0x4034`/
//! `0x4038`/`0x403C` the RC BAR windows, `0x4044`/`0x4048`/`0x404C` the MSI BAR
//! and data, `0x4068` `MISC_PCIE_STATUS`, `0x4204`
//! `MISC_HARD_PCIE_HARD_DEBUG`, `0x4308`/`0x4310`/`0x4314` interrupt masks,
//! `0x8000` `EXT_CFG_DATA`, `0x9000` `EXT_CFG_INDEX`, `0x9210`
//! `RGR1_SW_INIT_1`.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::periph::vl805::Vl805;

/// `reg = <0x0 0x7d500000 0x0 0x9310>` in the Pi 4 device tree
/// (`/proc/device-tree/scb/pcie@7d500000/reg` on a real board).
pub const BASE: u32 = 0x7D50_0000;
pub const SIZE: u32 = 0x0000_9310;

/// The root port's own configuration space, directly mapped.
const RC_CFG: u32 = 0x0000;
const RC_CFG_SIZE: u32 = 0x1000;
/// `PCIE_EXT_CFG_DATA` — a 4 KiB view of whichever function `EXT_CFG_INDEX`
/// selected.
const EXT_CFG_DATA: u32 = 0x8000;
/// `PCIE_EXT_CFG_INDEX`.
const EXT_CFG_INDEX: u32 = 0x9000;
/// `PCIE_MISC_PCIE_STATUS`.
const MISC_PCIE_STATUS: u32 = 0x4068;
/// `PCIE_RGR1_SW_INIT_1`.
const RGR1_SW_INIT_1: u32 = 0x9210;

/// `brcm_pcie_link_up()`: data-link active and PHY link up.
const STATUS_PHYLINKUP: u32 = 1 << 4;
const STATUS_DL_ACTIVE: u32 = 1 << 5;
/// `brcm_pcie_rc_mode()`: set means the block is a root complex, not an
/// endpoint. The bootloader's `PCIe timeout: 0x%08x` prints this whole word, so
/// on a live link the value is `0xB0`.
const STATUS_PORT_RC: u32 = 1 << 7;

/// `pcie-brcmstb`'s `EXT_CFG_INDEX` field positions.
const EXT_BUSNUM_SHIFT: u32 = 20;
const EXT_SLOT_SHIFT: u32 = 15;
const EXT_FUNC_SHIFT: u32 = 12;

/// The bus number the root port assigns to its single downstream link. Fixed on
/// this topology: `lspci` on `rpi-dev` shows `00:00.0` bridge, `01:00.0` VL805.
const ENDPOINT_BUS: u32 = 1;

/// The root port's own identity, measured on `rpi-dev`:
///
/// ```text
/// $ sudo od -Ax -tx1 -v /sys/bus/pci/devices/0000:00:00.0/config
/// 000000 e4 14 11 27 06 00 10 00 20 00 04 06 00 00 01 00
/// ```
///
/// vendor `14e4`, device `2711`, revision `0x20`, class `060400` (PCI-to-PCI
/// bridge), header type 1. The bootloader writes the class code itself at
/// `0x000A6E20` (`RC_CFG_PRIV1_ID_VAL3` ← `0x060400`), so only the vendor and
/// device pair has to be seeded for a scan to recognise the bridge.
const RC_CFG_SEED: &[(u32, u32)] = &[
    (0x00, 0x2711_14E4),
    (0x08, 0x0604_0020),
    (0x0C, 0x0001_0000),
];

pub struct Pcie {
    storage: BTreeMap<u32, u32>,
    /// Last value written to `RGR1_SW_INIT_1`.
    sw_init: u32,
    /// The link has trained. Sticky until PERST# is asserted again.
    link_up: bool,
    ext_cfg_index: u32,
    device_present: bool,
    pub endpoint: Vl805,
}

impl Default for Pcie {
    fn default() -> Self {
        Pcie::new()
    }
}

impl Pcie {
    pub fn new() -> Pcie {
        Pcie::with_device(std::env::var("RVF_PCIE_DEVICE").is_ok())
    }

    pub fn with_device(device_present: bool) -> Pcie {
        let mut storage = BTreeMap::new();
        for (off, v) in RC_CFG_SEED {
            storage.insert(*off, *v);
        }
        Pcie {
            storage,
            sw_init: 0,
            link_up: false,
            ext_cfg_index: 0,
            device_present,
            endpoint: Vl805::new(),
        }
    }

    pub fn link_up(&self) -> bool {
        self.link_up
    }

    fn status(&self) -> u32 {
        if self.link_up {
            STATUS_PHYLINKUP | STATUS_DL_ACTIVE | STATUS_PORT_RC
        } else {
            0
        }
    }

    /// Which function the `EXT_CFG_DATA` window currently points at, if any.
    fn ext_target(&self) -> CfgTarget {
        let bus = (self.ext_cfg_index >> EXT_BUSNUM_SHIFT) & 0xFF;
        let slot = (self.ext_cfg_index >> EXT_SLOT_SHIFT) & 0x1F;
        let func = (self.ext_cfg_index >> EXT_FUNC_SHIFT) & 0x7;
        match (bus, slot, func) {
            (0, 0, 0) => CfgTarget::RootPort,
            (ENDPOINT_BUS, 0, 0) if self.link_up => CfgTarget::Endpoint,
            _ => CfgTarget::None,
        }
    }

    /// The value the root port's own config space holds at `off`.
    fn rc_cfg_read(&self, off: u32, width: Width) -> u32 {
        let word = self.storage.get(&(off & !3)).copied().unwrap_or(0);
        let shift = 8 * (off & 3);
        let mask: u32 = match width {
            Width::Byte => 0xFF,
            Width::Half => 0xFFFF,
            Width::Word => 0xFFFF_FFFF,
        };
        (word >> shift) & mask
    }
}

enum CfgTarget {
    RootPort,
    Endpoint,
    None,
}

impl MmioDevice for Pcie {
    fn name(&self) -> &'static str {
        "pcie-rc"
    }

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32> {
        if offset == MISC_PCIE_STATUS {
            return Ok(self.status());
        }
        if offset == EXT_CFG_INDEX {
            return Ok(self.ext_cfg_index);
        }
        if (EXT_CFG_DATA..EXT_CFG_DATA + 0x1000).contains(&offset) {
            let cfg_off = offset - EXT_CFG_DATA;
            return Ok(match self.ext_target() {
                CfgTarget::RootPort => self.rc_cfg_read(cfg_off, width),
                CfgTarget::Endpoint => self.endpoint.cfg_read(cfg_off, width),
                // No function responds: the root complex returns all-ones,
                // which is what makes a scan skip the slot.
                CfgTarget::None => match width {
                    Width::Byte => 0xFF,
                    Width::Half => 0xFFFF,
                    Width::Word => 0xFFFF_FFFF,
                },
            });
        }
        if offset < RC_CFG + RC_CFG_SIZE {
            return Ok(self.rc_cfg_read(offset, width));
        }
        Ok(self.storage.get(&(offset & !3)).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()> {
        if offset == MISC_PCIE_STATUS {
            // Read-only status; the firmware never writes it.
            return Ok(());
        }
        if offset == EXT_CFG_INDEX {
            self.ext_cfg_index = value;
            return Ok(());
        }
        if (EXT_CFG_DATA..EXT_CFG_DATA + 0x1000).contains(&offset) {
            let cfg_off = offset - EXT_CFG_DATA;
            match self.ext_target() {
                CfgTarget::Endpoint => self.endpoint.cfg_write(cfg_off, width, value),
                // Root-port writes land in the same storage the direct
                // `+0x0000` view uses, so the two stay consistent.
                CfgTarget::RootPort => {
                    self.storage.insert(cfg_off & !3, value);
                }
                CfgTarget::None => {}
            }
            return Ok(());
        }
        if offset == RGR1_SW_INIT_1 {
            let was_perst = self.sw_init & 1 != 0;
            self.sw_init = value;
            if value & 1 != 0 {
                self.link_up = false;
            } else if was_perst && self.device_present {
                // PERST# released with a device on the far side: the link
                // trains. Real silicon takes a few milliseconds and the
                // bootloader polls for it at 1 ms intervals with a 1 s budget
                // (`0x000A9038`); the model brings it up on the spot, which
                // costs the transcript a millisecond of modelled time.
                self.link_up = true;
            }
            self.storage.insert(offset, value);
            return Ok(());
        }
        if offset < RC_CFG + RC_CFG_SIZE {
            // Root-port config space is byte-addressable: the bootloader writes
            // single halfwords into the bridge header (`0x22` ← `0x8000`, and
            // so on), so a word-granular store would clobber its neighbour.
            let word_off = offset & !3;
            let shift = 8 * (offset & 3);
            let mask: u32 = match width {
                Width::Byte => 0xFF << shift,
                Width::Half => 0xFFFF << shift,
                Width::Word => 0xFFFF_FFFF,
            };
            let old = self.storage.get(&word_off).copied().unwrap_or(0);
            self.storage
                .insert(word_off, (old & !mask) | ((value << shift) & mask));
            return Ok(());
        }
        self.storage.insert(offset & !3, value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rd(p: &mut Pcie, off: u32) -> u32 {
        p.read(off, Width::Word).unwrap()
    }

    /// The bootcode/bootloader reset dance, then the link-up poll.
    #[test]
    fn perst_release_brings_the_link_up() {
        let mut p = Pcie::with_device(true);
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0);
        // bootcode parks the block in reset
        p.write(RGR1_SW_INIT_1, Width::Word, 0x3).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0);
        // pcie_init releases the bridge, then PERST#
        p.write(RGR1_SW_INIT_1, Width::Word, 0x1).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x0).unwrap();
        // 0xB0: PHYLINKUP | DL_ACTIVE | root-complex mode.
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0xB0);
        // Asserting PERST# again drops it.
        p.write(RGR1_SW_INIT_1, Width::Word, 0x1).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0);
    }

    #[test]
    fn no_device_means_no_link() {
        let mut p = Pcie::with_device(false);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x3).unwrap();
        p.write(RGR1_SW_INIT_1, Width::Word, 0x0).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0);
    }

    fn link_up_pcie() -> Pcie {
        let mut p = Pcie::with_device(true);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x3).unwrap();
        p.write(RGR1_SW_INIT_1, Width::Word, 0x0).unwrap();
        p
    }

    #[test]
    fn ext_cfg_routes_to_the_endpoint() {
        let mut p = link_up_pcie();
        // Index 0 is the root port itself.
        assert_eq!(rd(&mut p, EXT_CFG_DATA), 0x2711_14E4);
        // Bus 1, slot 0, function 0 is the VL805.
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA), 0x3483_1106);
        // The halfword reads start4's `XHCI_RESET` does at 0x3EDC6204/0x3EDC620A.
        assert_eq!(p.read(EXT_CFG_DATA, Width::Half).unwrap(), 0x1106);
        assert_eq!(p.read(EXT_CFG_DATA + 2, Width::Half).unwrap(), 0x3483);
        // Class code 0c0330 = xHCI.
        assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x08) >> 8, 0x0C_0330);
        // An empty slot answers all-ones.
        p.write(
            EXT_CFG_INDEX,
            Width::Word,
            1 << EXT_BUSNUM_SHIFT | 1 << EXT_SLOT_SHIFT,
        )
        .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA), 0xFFFF_FFFF);
    }

    #[test]
    fn endpoint_is_invisible_until_the_link_is_up() {
        let mut p = Pcie::with_device(true);
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA), 0xFFFF_FFFF);
    }

    #[test]
    fn bar0_sizes_to_four_kib() {
        let mut p = link_up_pcie();
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        // Power-on: 64-bit memory BAR, unassigned.
        assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x10), 0x0000_0004);
        p.write(EXT_CFG_DATA + 0x10, Width::Word, 0xFFFF_FFFF)
            .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x10), 0xFFFF_F004);
        // Assign it and enable memory decoding.
        p.write(EXT_CFG_DATA + 0x10, Width::Word, 0xC000_0000)
            .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x10), 0xC000_0004);
        assert_eq!(p.endpoint.bar0_bus_addr(), None);
        p.write(EXT_CFG_DATA + 0x04, Width::Word, 0x0146).unwrap();
        assert_eq!(p.endpoint.bar0_bus_addr(), Some(0xC000_0000));
    }

    /// The bootloader's VL805 hub-firmware upload (`0x000B6CE0`) writes each
    /// byte of the image to its own index and reads it straight back, giving up
    /// with `HUB2.0 fail` on the first mismatch. A sticky index/data port is
    /// the whole requirement.
    #[test]
    fn vendor_port_round_trips_the_firmware_upload() {
        let mut p = link_up_pcie();
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        for (i, byte) in [0xDEu32, 0xAD, 0xBE, 0xEF].into_iter().enumerate() {
            let idx = 0x5_2000 + i as u32;
            // The firmware replicates the byte into all four lanes.
            let word = byte * 0x0101_0101;
            p.write(EXT_CFG_DATA + 0x78, Width::Word, idx).unwrap();
            p.write(EXT_CFG_DATA + 0x7C, Width::Word, word).unwrap();
        }
        for (i, byte) in [0xDEu32, 0xAD, 0xBE, 0xEF].into_iter().enumerate() {
            p.write(EXT_CFG_DATA + 0x78, Width::Word, 0x5_2000 + i as u32)
                .unwrap();
            assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x7C) & 0xFF, byte);
        }
        assert_eq!(p.endpoint.vendor_writes, 4);
    }

    /// The root-port bridge header the bootloader builds at `0x000A6E80`
    /// onwards is written a halfword at a time; neighbours must survive.
    #[test]
    fn root_port_config_is_byte_addressable() {
        let mut p = link_up_pcie();
        p.write(0x22, Width::Half, 0x8000).unwrap();
        p.write(0x20, Width::Half, 0x1234).unwrap();
        assert_eq!(rd(&mut p, 0x20), 0x8000_1234);
    }
}
