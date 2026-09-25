//! The VIA VL805/806 xHCI USB 3.0 controller (`1106:3483`) — the one PCIe
//! endpoint on a Pi 4B, on bus 1 device 0 behind the root complex in
//! [`super::pcie`].
//!
//! This file is the *PCI function*: its configuration space, plus the vendor
//! indirect port the firmware uploads the VL805's own hub/MCU firmware through.
//! The register block, ring engine and attached devices live in
//! [`super::xhci`] and [`super::usb`]. Registers and reset values:
//! `specs/vl805.toml`.
//!
//! Every byte of `CFG_SEED` was read off a Raspberry Pi 4B d03115
//! (`od -tx1 /sys/bus/pci/devices/0000:01:00.0/config`, cross-checked with
//! `lspci -vvv`). That dump is a *running* device, so the fields Linux had
//! already written are put back to their power-on values and the rest is
//! verbatim.

use std::collections::BTreeMap;

use crate::bus::Width;
use crate::periph::usb::UsbDevice;
use crate::periph::xhci::{HostMem, Xhci};
use crate::spec::vl805::{
    BAR0, BAR0_HI, CACHE_LAT, CAP_PTR, CLASS_REV, COMMAND_STATUS, ID, INTERRUPT, MSI_ADDR_HI,
    MSI_ADDR_LO, MSI_CTRL, MSI_DATA, PCIE_CAP, PCIE_DEVCAP, PCIE_DEVCAP2, PCIE_DEVCTL,
    PCIE_DEVCTL2, PCIE_LNKCAP, PCIE_LNKCAP2, PCIE_LNKCTL, PCIE_LNKCTL2, PM_CAP, PM_CSR, SUBSYSTEM,
    VENDOR_DATA, VENDOR_INDEX,
};
use crate::spec::Coverage;

/// Seeded storage behind a write mask: every register in the spec is modelled.
pub const COVERAGE: Coverage = Coverage {
    block: "vl805",
    decoded: &[
        ID,
        COMMAND_STATUS,
        CLASS_REV,
        CACHE_LAT,
        BAR0,
        BAR0_HI,
        SUBSYSTEM,
        CAP_PTR,
        INTERRUPT,
        VENDOR_INDEX,
        VENDOR_DATA,
        PM_CAP,
        PM_CSR,
        MSI_CTRL,
        MSI_ADDR_LO,
        MSI_ADDR_HI,
        MSI_DATA,
        PCIE_CAP,
        PCIE_DEVCAP,
        PCIE_DEVCTL,
        PCIE_LNKCAP,
        PCIE_LNKCTL,
        PCIE_DEVCAP2,
        PCIE_DEVCTL2,
        PCIE_LNKCAP2,
        PCIE_LNKCTL2,
    ],
};

/// `lspci`: `Region 0: … [size=4K]` — the xHCI register block.
pub const BAR0_SIZE: u32 = crate::spec::xhci::SIZE;

const CFG_LEN: usize = 0x1000;

/// The measured config space, at power-on values (module docs).
const CFG_SEED: &[(usize, &[u8])] = &[
    (
        0x00,
        &[
            0x06, 0x11, 0x83, 0x34, 0x00, 0x00, 0x10, 0x00, 0x01, 0x30, 0x03, 0x0c, 0x10, 0x00,
            0x00, 0x00,
        ],
    ),
    // BAR0, 64-bit memory, not yet assigned.
    (0x10, &[0x04, 0x00, 0x00, 0x00]),
    (0x2c, &[0x06, 0x11, 0x83, 0x34]),
    (0x34, &[0x80]),
    (0x3c, &[0x1b, 0x01, 0x00, 0x00]),
    // Vendor-defined, verbatim from the dump.
    (
        0x44,
        &[
            0x00, 0x01, 0x00, 0x00, 0x09, 0x00, 0x80, 0x0e, 0x04, 0x00, 0x00, 0x00, 0xc0, 0x38,
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x11, 0x83, 0x34,
        ],
    ),
    (0x60, &[0x30, 0x20]),
    // 0x78/0x7c: the vendor indirect index/data port, as the board held it.
    (0x78, &[0x08, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x18]),
    (0x80, &[0x01, 0x90, 0xc3, 0x89]),
    (0x90, &[0x05, 0xc4, 0x80, 0x00]),
    (0xc4, &[0x10, 0x00, 0x02, 0x00]),
    (0xc8, &[0x01, 0x80, 0x00, 0x00]),
    (0xcc, &[0x10, 0x28, 0x10, 0x00]),
    (0xd0, &[0x12, 0x5c, 0x06, 0x00]),
    // LnkSta 0x1012: the link is trained by the time anything reads it.
    (0xd4, &[0x00, 0x00, 0x12, 0x10]),
    (0xe8, &[0x12]),
    (0xf4, &[0x22, 0x00, 0x01, 0x00]),
    (0x100, &[0x01, 0x00, 0x01, 0x00]),
    (0x10c, &[0x31, 0x20, 0x06, 0x00]),
    (0x114, &[0x00, 0x20, 0x00, 0x00]),
];

/// Which bits of each config-space word the firmware may change. Anything else
/// is read-only, which is what makes BAR sizing work.
fn cfg_write_mask(off: usize) -> u32 {
    match off as u32 {
        CACHE_LAT => 0x0000_FFFF,
        // Command register. Status is RW1C on silicon; nothing reads it back,
        // so it is read-only here.
        COMMAND_STATUS => 0x0000_0547,
        // BAR0: 4 KiB, 64-bit memory. Bits [3:0] are the hard-wired type.
        BAR0 => !(BAR0_SIZE - 1),
        BAR0_HI => 0xFFFF_FFFF,
        INTERRUPT => 0x0000_00FF,
        VENDOR_INDEX | VENDOR_DATA => 0xFFFF_FFFF,
        PM_CSR => 0xFFFF_FFFF,
        // MSI control: enable and Multiple Message Enable; the rest is fixed.
        MSI_CTRL => 0x0071_0000,
        MSI_ADDR_LO | MSI_ADDR_HI | MSI_DATA => 0xFFFF_FFFF,
        // The control half of each PCIe capability word; the status halves
        // never latch here.
        PCIE_DEVCTL | PCIE_LNKCTL | PCIE_DEVCTL2 | PCIE_LNKCTL2 => 0x0000_FFFF,
        _ => 0,
    }
}

pub use super::xhci::CAPLENGTH;

/// The VL805 endpoint's configuration space.
pub struct Vl805 {
    cfg: Vec<u8>,
    /// The register file behind the indirect index/data port at config
    /// `0x78`/`0x7C`. Both the EEPROM bootloader and start4 stream the VL805's
    /// firmware image through it and read it back to verify, so sticky storage
    /// is the whole requirement.
    vendor_regs: BTreeMap<u32, u32>,
    /// Words pushed through the data port; an observable for tests.
    pub vendor_writes: u64,
    /// The xHCI controller behind BAR0 ([`super::xhci`]).
    pub xhci: Xhci,
}

impl Default for Vl805 {
    fn default() -> Self {
        Vl805::new()
    }
}

impl Vl805 {
    fn power_on_cfg() -> Vec<u8> {
        let mut cfg = vec![0u8; CFG_LEN];
        for (off, bytes) in CFG_SEED {
            cfg[*off..*off + bytes.len()].copy_from_slice(bytes);
        }
        cfg
    }

    /// PERST#: the function and its xHCI controller come back as they powered
    /// on, so an interrupt the firmware's last USB event left pending is gone —
    /// Linux asserts PERST# before starting the link and never clears INTA
    /// itself. The vendor port's storage survives.
    pub fn reset(&mut self) {
        self.cfg = Vl805::power_on_cfg();
        self.xhci.reset();
    }

    pub fn new() -> Vl805 {
        let mut dev = Vl805 {
            cfg: Vl805::power_on_cfg(),
            vendor_regs: BTreeMap::new(),
            vendor_writes: 0,
            xhci: Xhci::new(),
        };
        // A Pi 4B has a VIA Labs four-port hub soldered to root port 1 (the
        // USB2 half of all four sockets), plugged in or not. Ports 2 and 3 are
        // the blue sockets' SuperSpeed lanes; 4 and 5 go nowhere.
        dev.attach(1, Box::new(crate::periph::usb::Hub::new()));
        dev
    }

    /// Plug a USB device into root port `port` (1-based).
    pub fn attach(&mut self, port: usize, device: Box<dyn UsbDevice>) {
        self.xhci.attach(port, device);
    }

    fn cfg_word(&self, off: usize) -> u32 {
        u32::from_le_bytes([
            self.cfg[off],
            self.cfg[off + 1],
            self.cfg[off + 2],
            self.cfg[off + 3],
        ])
    }

    fn set_cfg_word(&mut self, off: usize, v: u32) {
        self.cfg[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// Read `width` bytes of configuration space at `off`.
    pub fn cfg_read(&mut self, off: u32, width: Width) -> u32 {
        let off = off as usize & (CFG_LEN - 1);
        // The data half reads back whatever the selected index holds.
        if off & !3 == VENDOR_DATA as usize {
            let idx = self.cfg_word(VENDOR_INDEX as usize);
            let word = self.vendor_regs.get(&idx).copied().unwrap_or(0);
            self.set_cfg_word(VENDOR_DATA as usize, word);
        }
        let mut v = 0u32;
        for i in 0..width.bytes() as usize {
            v |= (*self.cfg.get(off + i).unwrap_or(&0) as u32) << (8 * i);
        }
        v
    }

    /// Write `width` bytes of configuration space at `off`.
    pub fn cfg_write(&mut self, off: u32, width: Width, value: u32) {
        let off = off as usize & (CFG_LEN - 1);
        let word_off = off & !3;
        let shift = 8 * (off & 3);
        let byte_mask: u32 = match width {
            Width::Byte => 0xFF,
            Width::Half => 0xFFFF,
            Width::Word => 0xFFFF_FFFF,
        };
        let mask = cfg_write_mask(word_off) & (byte_mask << shift);
        if mask == 0 {
            return;
        }
        let old = self.cfg_word(word_off);
        let new = (old & !mask) | ((value << shift) & mask);
        self.set_cfg_word(word_off, new);

        if word_off == VENDOR_DATA as usize {
            let idx = self.cfg_word(VENDOR_INDEX as usize);
            self.vendor_regs.insert(idx, new);
            self.vendor_writes += 1;
        }
    }

    /// Read `width` bytes of the xHCI register block behind BAR0.
    pub fn bar0_read(&mut self, off: u32, width: Width) -> u32 {
        self.xhci.read(off, width)
    }

    /// Write BAR0. `mem` is host memory: a doorbell makes the controller
    /// fetch TRBs and post events.
    pub fn bar0_write(&mut self, off: u32, width: Width, value: u32, mem: &mut dyn HostMem) {
        self.xhci.write(off, width, value, mem);
    }

    /// The MSI the function sends as `(address, data)`, if the host enabled
    /// MSI. Only interrupter 0 is ever used, so the vector offset is zero.
    pub fn msi_message(&self) -> Option<(u64, u32)> {
        if self.cfg_word(MSI_CTRL as usize) & (1 << 16) == 0 {
            return None;
        }
        let addr = self.cfg_word(MSI_ADDR_LO as usize) as u64
            | ((self.cfg_word(MSI_ADDR_HI as usize) as u64) << 32);
        Some((addr, self.cfg_word(MSI_DATA as usize) & 0xFFFF))
    }

    /// Command register Bus Master Enable: without it the function may not
    /// write anything upstream, an MSI included.
    pub fn bus_master(&self) -> bool {
        self.cfg_word(0x04) & 0x4 != 0
    }

    /// Command register Interrupt Disable: INTA stays deasserted.
    pub fn intx_disabled(&self) -> bool {
        self.cfg_word(0x04) & 0x400 != 0
    }

    /// Where BAR0 decodes on the PCI bus, once the firmware has assigned it
    /// and enabled memory decoding. `None` = the window is off.
    pub fn bar0_bus_addr(&self) -> Option<u64> {
        if self.cfg_word(0x04) & 0x2 == 0 {
            return None; // memory space decoding disabled
        }
        let addr = ((self.cfg_word(0x14) as u64) << 32) | (self.cfg_word(0x10) & !0xF) as u64;
        if addr == 0 {
            None
        } else {
            Some(addr)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rd(dev: &mut Vl805, off: u32) -> u32 {
        dev.cfg_read(off, Width::Word)
    }

    /// The PCIe capability words reset to what `specs/vl805.toml` says.
    #[test]
    fn pcie_capability_matches_the_spec() {
        use crate::spec::vl805::*;
        let mut dev = Vl805::new();
        for (off, reset) in [
            (PCIE_CAP, PCIE_CAP_RESET),
            (PCIE_DEVCAP, PCIE_DEVCAP_RESET),
            (PCIE_DEVCTL, PCIE_DEVCTL_RESET),
            (PCIE_LNKCAP, PCIE_LNKCAP_RESET),
            (PCIE_LNKCTL, PCIE_LNKCTL_RESET),
            (PCIE_DEVCAP2, PCIE_DEVCAP2_RESET),
            (PCIE_DEVCTL2, PCIE_DEVCTL2_RESET),
            (PCIE_LNKCAP2, PCIE_LNKCAP2_RESET),
            (PCIE_LNKCTL2, PCIE_LNKCTL2_RESET),
        ] {
            assert_eq!(rd(&mut dev, off), reset, "{off:#x}");
        }
        // 5 GT/s x1, as the bootloader and Linux both see it.
        assert_eq!(rd(&mut dev, PCIE_LNKCAP) & 0x3FF, 0x012);
    }

    /// Linux's writes land in the control words; the capability words beside
    /// them stay read-only.
    #[test]
    fn pcie_control_words_are_writable_and_capabilities_are_not() {
        let mut dev = Vl805::new();
        for (cap, ctl) in [
            (PCIE_DEVCAP, PCIE_DEVCTL),
            (PCIE_LNKCAP, PCIE_LNKCTL),
            (PCIE_DEVCAP2, PCIE_DEVCTL2),
            (PCIE_LNKCAP2, PCIE_LNKCTL2),
        ] {
            let before = rd(&mut dev, cap);
            dev.cfg_write(cap, Width::Word, 0xFFFF_FFFF);
            assert_eq!(rd(&mut dev, cap), before, "{cap:#x} is read-only");

            let status = rd(&mut dev, ctl) & 0xFFFF_0000;
            dev.cfg_write(ctl, Width::Half, 0x0143);
            assert_eq!(rd(&mut dev, ctl), status | 0x0143, "{ctl:#x}");
        }
        // Linux's DevCtl on the running board, over the power-on DevSta.
        dev.cfg_write(0xcc, Width::Half, 0x281f);
        assert_eq!(rd(&mut dev, 0xcc), 0x0010_281f);
        // PERST# puts them back.
        dev.reset();
        assert_eq!(rd(&mut dev, 0xcc), 0x0010_2810);
        assert_eq!(rd(&mut dev, 0xd4), 0x1012_0000);
    }
}
