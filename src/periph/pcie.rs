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
//!   that function's configuration space. Bus 1 device 0 is the VL805.
//!   Everything else — bus 0 included, see [`Pcie::ext_target`] — reads back
//!   all-ones, which is how PCI says "nothing there". The root port's own
//!   config space is reachable only through the direct view at `+0x0000`.
//!
//! ## The outbound window, and how the VPU reaches BAR0
//!
//! It does not reach it with a load. `CPU_2_PCIE_MEM_WIN0` (`+0x400C`,
//! `+0x4010`, `+0x4070`, `+0x4080`, `+0x4084`) puts the endpoint's registers at
//! CPU-physical `0x6_0000_0000..0x6_3FFF_FFFF` — 35 bits, which a 32-bit VPU
//! cannot form — so the firmware reads and writes them with the **40-bit DMA4
//! channel** instead: `0x000A701E` builds a control block whose `SRC` is
//! `0x6_0200_0004` (`src = 0x0200_0004`, `srci = 0x1006`, high byte 6), DMAs
//! four bytes into a bounce buffer at `0xC031B000`, and reads the buffer.
//! `docs/usb-xhci.md` §5.1 has the full trace.
//!
//! So this file translates CPU-physical → PCI bus → BAR0 offset
//! ([`Pcie::mmio_read`] / [`Pcie::mmio_write`]), and
//! [`Machine::run_dma4`](crate::machine::Machine) calls it with the composed
//! 40-bit address. Until that landed the transfer read modelled DRAM, the
//! capability registers came back zero, and the bring-up hung forever at
//! `0x000AA3C0` — which is why the endpoint used to be detached by default.
//!
//! ## The endpoint is attached by default
//!
//! A Pi 4B has the VL805 soldered on, so attached is the honest model of the
//! reference board, and with BAR0 answering, the bootloader's bring-up gets the
//! same numbers a real board prints
//! (`examples-on-real-hardware/sd-card-boot.log` lines 27-32):
//!
//! ```text
//!   2.75 PCIe scan 00001106:00003483
//!   3.31 xHC0 ver: 256 HCS: 05000420 fc000031 00e70004 HCC: 002841eb
//!   3.31 USBSTS 1
//!   3.31 xHC0 ports 5 slots 32 intrs 4
//! ```
//!
//! What it does *not* find is a device: the model has no USB device behind the
//! root hub and no VIA hub on port 1, so all five ports read "powered, empty"
//! and the bootloader falls through to the SD entry of `BOOT_ORDER` without the
//! `USB2[1] … connected` / `HUB init` lines the real board prints. That is
//! stage 3 of `docs/usb-xhci.md`.
//!
//! `RVF_PCIE_DEVICE=0` unsolders the endpoint again — the link never trains,
//! `MISC_PCIE_STATUS` reads `0` and the bootloader prints
//! `PCIe timeout: 0x00000000` / `USB xHC init failed`, the pre-stage-1
//! transcript. It is for reproducing that, nothing else.
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
/// `CPU_2_PCIE_MEM_WIN0_LO` / `_HI` — the PCI bus address the outbound window
/// maps to. The bootloader writes `0x8000_0000` / `0` at `0x000A725C`.
const MEM_WIN0_LO: u32 = 0x400C;
const MEM_WIN0_HI: u32 = 0x4010;
/// `CPU_2_PCIE_MEM_WIN0_BASE_LIMIT` — the CPU-side extent, in MiB: base in bits
/// `[15:4]`, limit in bits `[31:20]`, the same field order `pcie-brcmstb` uses
/// (`PCIE_MEM_WIN0_BASE_LIMIT_BASE_MASK` = `0xFFF0`). The bootloader writes
/// `0x3FF0_0000` at `0x000A72C6` — base 0, limit `0x3FF` MiB.
const MEM_WIN0_BASE_LIMIT: u32 = 0x4070;
/// The bits above `[31:20]` of the CPU-side base and limit. Both are written
/// `6` (`0x000A72DE` / `0x000A72F0`), putting the window at CPU-physical
/// `0x6_0000_0000..0x6_3FFF_FFFF` — exactly the 1 GiB `ranges` property and
/// exactly what `dmesg` reports (`MEM 0x0600000000..0x063fffffff`).
const MEM_WIN0_BASE_HI: u32 = 0x4080;
const MEM_WIN0_LIMIT_HI: u32 = 0x4084;
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
        // A Pi 4B has the VL805 soldered on, so attached is what the reference
        // board looks like. `RVF_PCIE_DEVICE=0` unsolders it — for reproducing
        // the pre-stage-1 transcript, nothing else.
        Pcie::with_device(std::env::var("RVF_PCIE_DEVICE").as_deref() != Ok("0"))
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

    /// The CPU-physical extent of outbound window 0, as the firmware programmed
    /// it. Both ends are inclusive and 40 bits wide — well outside anything a
    /// 32-bit VPU load can form, which is why the firmware reaches through it by
    /// DMA (see [`crate::machine::Machine::run_dma4`]).
    fn outbound_window(&self) -> Option<(u64, u64, u64)> {
        let base_limit = self.storage.get(&MEM_WIN0_BASE_LIMIT).copied().unwrap_or(0);
        let base_hi = self.storage.get(&MEM_WIN0_BASE_HI).copied().unwrap_or(0) as u64;
        let limit_hi = self.storage.get(&MEM_WIN0_LIMIT_HI).copied().unwrap_or(0) as u64;
        let base_mb = (base_hi << 12) | ((base_limit >> 4) & 0xFFF) as u64;
        let limit_mb = (limit_hi << 12) | ((base_limit >> 20) & 0xFFF) as u64;
        if limit_mb < base_mb {
            return None;
        }
        let bus = ((self.storage.get(&MEM_WIN0_HI).copied().unwrap_or(0) as u64) << 32)
            | self.storage.get(&MEM_WIN0_LO).copied().unwrap_or(0) as u64;
        Some((base_mb << 20, (limit_mb << 20) | 0xF_FFFF, bus))
    }

    /// Translate a CPU-physical address into a PCI bus address, if outbound
    /// window 0 covers it.
    pub fn outbound_bus_addr(&self, cpu: u64) -> Option<u64> {
        let (base, limit, bus) = self.outbound_window()?;
        if cpu < base || cpu > limit {
            return None;
        }
        Some(bus + (cpu - base))
    }

    /// Which endpoint register a CPU-physical address lands on, if any: through
    /// the outbound window, then through the endpoint's BAR0.
    fn bar0_offset(&self, cpu: u64) -> Option<u32> {
        if !self.link_up {
            return None;
        }
        let bus = self.outbound_bus_addr(cpu)?;
        let bar = self.endpoint.bar0_bus_addr()?;
        if bus < bar || bus >= bar + crate::periph::vl805::BAR0_SIZE as u64 {
            return None;
        }
        Some((bus - bar) as u32)
    }

    /// A read of endpoint MMIO, addressed CPU-physically. `None` means nothing
    /// decodes there — the DMA engine then falls back to DRAM, the way an
    /// unclaimed address does on real silicon.
    pub fn mmio_read(&mut self, cpu: u64, width: Width) -> Option<u32> {
        let off = self.bar0_offset(cpu)?;
        Some(self.endpoint.bar0_read(off, width))
    }

    /// A write of endpoint MMIO, addressed CPU-physically. `mem` is host
    /// memory: an xHCI doorbell write makes the endpoint fetch TRBs from DRAM
    /// and post events back into it.
    pub fn mmio_write(
        &mut self,
        cpu: u64,
        width: Width,
        value: u32,
        mem: &mut dyn crate::periph::xhci::HostMem,
    ) -> bool {
        match self.bar0_offset(cpu) {
            Some(off) => {
                self.endpoint.bar0_write(off, width, value, mem);
                true
            }
            None => false,
        }
    }

    /// Which function the `EXT_CFG_DATA` window currently points at, if any.
    ///
    /// Bus 0 is deliberately not one of them (#21). The window turns its index
    /// into a configuration request on the link, and the link's far side is the
    /// secondary bus — the root port's own config space is never reachable this
    /// way, only through the direct `+0x0000` view. That is why `pcie-brcmstb`
    /// special-cases it in `brcm_pcie_map_conf()`:
    ///
    /// ```c
    /// /* Accesses to the RC go right to the RC registers if slot==0 */
    /// if (pci_is_root_bus(bus))
    ///     return PCI_SLOT(devfn) ? NULL : base + where;
    /// ```
    ///
    /// and it is what the reference transcript shows. The bootloader's scan
    /// (`0x000A712C`) starts at bus 0, slot 0 and prints `PCIe scan %08x:%08x`
    /// for every function whose vendor id is not `0xFFFF`, *before* it looks at
    /// the header type — the header-type byte at `0x0E` only decides whether the
    /// function is recorded in the device list (`0x000A71CC`, non-zero = bridge
    /// = skip), not whether it is printed. So if bus 0 answered here the real
    /// board would print the root complex too, and
    /// `examples-on-real-hardware/sd-card-boot.log:25-27` shows it does not.
    ///
    /// The bootloader reaches the root port's own config space the same way
    /// Linux does: its config-space selector (`0x000A6888`) writes
    /// `EXT_CFG_INDEX` and points its window at `EXT_CFG_DATA` only for a real
    /// device, and points it straight at `0x7D50_0000` for the root port.
    fn ext_target(&self) -> CfgTarget {
        let bus = (self.ext_cfg_index >> EXT_BUSNUM_SHIFT) & 0xFF;
        let slot = (self.ext_cfg_index >> EXT_SLOT_SHIFT) & 0x1F;
        let func = (self.ext_cfg_index >> EXT_FUNC_SHIFT) & 0x7;
        match (bus, slot, func) {
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
        // Index 0 is bus 0 — the root port's own bus, which no configuration
        // request on the link can reach (#21). Its config space is the direct
        // `+0x0000` view, and only that.
        assert_eq!(rd(&mut p, EXT_CFG_DATA), 0xFFFF_FFFF);
        assert_eq!(rd(&mut p, RC_CFG), 0x2711_14E4);
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

    /// The exact outbound-window programming the bootloader does at
    /// `0x000A725C`–`0x000A72F0`, followed by the BAR assignment at
    /// `0x000A6918`. The window is the only way a 32-bit VPU can name the
    /// endpoint's registers, and it names them 40 bits wide.
    fn enumerated_pcie() -> Pcie {
        let mut p = link_up_pcie();
        p.write(MEM_WIN0_LO, Width::Word, 0x8000_0000).unwrap();
        p.write(MEM_WIN0_HI, Width::Word, 0).unwrap();
        p.write(MEM_WIN0_BASE_LIMIT, Width::Word, 0x3FF0_0000)
            .unwrap();
        p.write(MEM_WIN0_BASE_HI, Width::Word, 6).unwrap();
        p.write(MEM_WIN0_LIMIT_HI, Width::Word, 6).unwrap();
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        p.write(EXT_CFG_DATA + 0x10, Width::Word, 0x8200_0000)
            .unwrap();
        p.write(EXT_CFG_DATA + 0x14, Width::Word, 0).unwrap();
        p.write(EXT_CFG_DATA + 0x04, Width::Word, 0x0146).unwrap();
        p
    }

    #[test]
    fn outbound_window_spans_one_gib_at_six() {
        let p = enumerated_pcie();
        // `dmesg` on rpi-dev: `MEM 0x0600000000..0x063fffffff -> 0x00c0000000`.
        // The firmware picks `0x8000_0000` for the bus side rather than Linux's
        // `0xC000_0000`, so the model has to read the register, not the DT.
        assert_eq!(p.outbound_bus_addr(0x6_0000_0000), Some(0x8000_0000));
        assert_eq!(p.outbound_bus_addr(0x6_3FFF_FFFF), Some(0xBFFF_FFFF));
        assert_eq!(p.outbound_bus_addr(0x5_FFFF_FFFF), None);
        assert_eq!(p.outbound_bus_addr(0x6_4000_0000), None);
    }

    /// The read the bootloader's `xHC0 ver:` line is built from. Its DMA4
    /// control block carries `src = 0x0200_0004`, `srci = 0x1006` — a 40-bit
    /// source of `0x6_0200_0004`, i.e. BAR0 + 4 = `HCSPARAMS1`.
    #[test]
    fn the_outbound_window_reaches_xhci_capability_registers() {
        let mut p = enumerated_pcie();
        assert_eq!(p.mmio_read(0x6_0200_0000, Width::Word), Some(0x0100_0020));
        assert_eq!(p.mmio_read(0x6_0200_0004, Width::Word), Some(0x0500_0420));
        assert_eq!(p.mmio_read(0x6_0200_0010, Width::Word), Some(0x0028_41EB));
        // One page, and not a byte more.
        assert_eq!(p.mmio_read(0x6_0200_1000, Width::Word), None);
    }

    /// With memory decoding still disabled, or the link down, the window
    /// decodes nothing — the DMA engine then reads DRAM, the way an unclaimed
    /// address behaves on real silicon.
    #[test]
    fn endpoint_mmio_needs_the_command_register() {
        let mut p = link_up_pcie();
        p.write(MEM_WIN0_LO, Width::Word, 0x8000_0000).unwrap();
        p.write(MEM_WIN0_BASE_LIMIT, Width::Word, 0x3FF0_0000)
            .unwrap();
        p.write(MEM_WIN0_BASE_HI, Width::Word, 6).unwrap();
        p.write(MEM_WIN0_LIMIT_HI, Width::Word, 6).unwrap();
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        p.write(EXT_CFG_DATA + 0x10, Width::Word, 0x8200_0000)
            .unwrap();
        assert_eq!(p.mmio_read(0x6_0200_0000, Width::Word), None);
    }

    /// `USBCMD.HCRST` self-clears and returns the operational registers to
    /// their power-on state; `USBSTS.HCH` follows `USBCMD.RS`. Those two are
    /// the only live bits the bootloader's bring-up waits on.
    #[test]
    fn host_controller_reset_completes() {
        let mut p = enumerated_pcie();
        let usbcmd = 0x6_0200_0020;
        let usbsts = 0x6_0200_0024;
        assert_eq!(p.mmio_read(usbsts, Width::Word), Some(1)); // HCHalted
        let mut mem = crate::periph::xhci::VecMem::default();
        p.mmio_write(usbcmd, Width::Word, 1 << 1, &mut mem); // HCRST
        assert_eq!(p.mmio_read(usbcmd, Width::Word), Some(0));
        p.mmio_write(usbcmd, Width::Word, 1, &mut mem); // Run/Stop
        assert_eq!(p.mmio_read(usbsts, Width::Word), Some(0));
        // `USBSTS` is write-1-to-clear. The bring-up's stop path writes
        // all-ones; a plain register would read that straight back.
        p.mmio_write(usbsts, Width::Word, 0xFFFF_FFFF, &mut mem);
        assert_eq!(p.mmio_read(usbsts, Width::Word), Some(0));
        // Root port 1 carries the on-board VIA hub, so it reports a connect;
        // the four USB3 ports read "powered, empty".
        assert_eq!(p.mmio_read(0x6_0200_0420, Width::Word), Some(0x4002_02E1));
        assert_eq!(p.mmio_read(0x6_0200_0460, Width::Word), Some(0x2A0));
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
