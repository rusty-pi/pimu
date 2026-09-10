//! BCM2711 PCIe root complex at `0x7D50_0000` — the block Linux calls
//! `pcie@7d500000` and drives with `pcie-brcmstb`.
//!
//! Behind it sits the Pi 4B's VIA VL805 xHCI controller (`1106:3483`), so this
//! window is the gateway to USB 3.0 and to USB boot. Neither is modelled yet;
//! see [`docs/usb-xhci.md`](../../../docs/usb-xhci.md) for the register-level
//! survey and the staged plan.
//!
//! What this device is for *today* is narrower: the window sits outside the
//! `0x7E…` legacy peripheral aperture, so without it every PCIe access folds
//! onto DRAM (`addr & 0x3FFF_FFFF` = `0x3D50_xxxx`) and the firmware silently
//! scribbles ~37 KiB into the middle of modelled memory. It also made the whole
//! PCIe conversation invisible to `RVF_TRACE_MMIO` and to the run report.
//!
//! Behaviour is deliberately unchanged from the DRAM fold: sticky per-offset
//! storage, zero for anything never written. In particular
//! `MISC_PCIE_STATUS` (`+0x4068`) still reads back 0, so the second-stage
//! bootloader still times out waiting for the link and still prints
//!
//! ```text
//!   PCI0 init
//!   PCI0 reset
//!   PCIe timeout: 0x00000000
//!   USB xHC init failed
//! ```
//!
//! and falls through to the next `BOOT_ORDER` entry, exactly as before.
//! Bringing the link up is stage 1 of the plan in the doc, and must land
//! together with an xHCI model — on its own it would only trade a harmless
//! timeout for a stall against a controller that does not exist.
//!
//! Register offsets the firmware actually touches (measured, see the doc):
//! `0x0000..0x0FFF` root-port config space, `0x4008` `MISC_MISC_CTRL`,
//! `0x402C`/`0x4034`/`0x4038`/`0x403C` the RC BAR windows, `0x4068`
//! `MISC_PCIE_STATUS`, `0x4204` `MISC_HARD_PCIE_HARD_DEBUG`, `0x8000`
//! `EXT_CFG_DATA`, `0x9000` `EXT_CFG_INDEX`, `0x9210` `RGR1_SW_INIT_1`.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

/// `reg = <0x0 0x7d500000 0x0 0x9310>` in the Pi 4 device tree
/// (`/proc/device-tree/scb/pcie@7d500000/reg` on a real board).
pub const BASE: u32 = 0x7D50_0000;
pub const SIZE: u32 = 0x0000_9310;

#[derive(Default)]
pub struct Pcie {
    storage: BTreeMap<u32, u32>,
}

impl Pcie {
    pub fn new() -> Pcie {
        Pcie::default()
    }
}

impl MmioDevice for Pcie {
    fn name(&self) -> &'static str {
        "pcie-rc"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(self.storage.get(&(offset & !3)).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        self.storage.insert(offset & !3, value);
        Ok(())
    }
}
