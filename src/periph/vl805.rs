//! The VIA VL805/806 xHCI USB 3.0 controller (`1106:3483`) — the one PCIe
//! endpoint on a Pi 4B, sitting on bus 1, device 0, function 0 behind the
//! BCM2711 root complex modelled in [`super::pcie`].
//!
//! This file is the *PCI function*: its configuration space, plus the
//! vendor-specific indirect port the firmware uploads the VL805's own hub/MCU
//! firmware through. The xHCI register block, the ring engine and the devices
//! on the root ports live in [`super::xhci`] and [`super::usb`]; a `Vl805` owns
//! one [`Xhci`] and forwards BAR0 accesses to it.
//!
//! The config-space half is what carries these conversations:
//!
//! * the second-stage EEPROM bootloader's bus scan, which walks
//!   `EXT_CFG_INDEX`/`EXT_CFG_DATA` reading vendor/device/class and prints
//!   `PCIe scan %08x:%08x` (`0x000A7118`) for each function that answers;
//! * `start4.elf`'s `XHCI_RESET` (`0x3EDC61F4`), which reads the vendor and
//!   device halfwords at `0x7D50_8000`/`0x7D50_8002` and refuses anything that
//!   is not `1106:3483`.
//!
//! ## Ground truth
//!
//! Every config-space byte below was read off a Raspberry Pi 4B d03115:
//!
//! ```text
//! $ sudo od -Ax -tx1 -v /sys/bus/pci/devices/0000:01:00.0/config
//! 000000 06 11 83 34 46 05 10 00 01 30 03 0c 10 00 00 00
//! 000010 04 00 00 c0 00 00 00 00 00 00 00 00 00 00 00 00
//! 000020 00 00 00 00 00 00 00 00 00 00 00 00 06 11 83 34
//! 000030 00 00 00 00 80 00 00 00 00 00 00 00 1b 01 00 00
//! 000040 00 00 00 00 00 01 00 00 09 00 80 0e 04 00 00 00
//! 000050 c0 38 01 00 00 00 00 00 00 00 00 00 06 11 83 34
//! 000060 30 20 00 00 00 00 00 00 00 00 00 00 00 00 00 00
//! 000070 00 00 00 00 00 00 00 00 08 00 03 00 01 00 00 18
//! 000080 01 90 c3 89 00 00 00 00 00 00 00 00 00 00 00 00
//! 000090 05 c4 a5 00 fc ff ff ff 00 00 00 00 40 65 00 00
//! ...
//! 0000c0 00 20 00 00 10 00 02 00 01 80 00 00 1f 28 19 00
//! 0000d0 12 5c 06 00 43 01 12 10 00 00 00 00 00 00 00 00
//! 0000e0 00 00 00 00 00 00 00 00 12 00 00 00 00 00 00 00
//! 0000f0 00 00 00 00 22 00 01 00 00 00 00 00 00 00 00 00
//! 000100 01 00 01 00 00 00 00 00 00 00 00 00 31 20 06 00
//! ```
//!
//! That dump is a *running* device: Linux had already enabled the command
//! register (`0x04` = `0x0546`), assigned BAR0 (`0x10` = `0xC000_0004`),
//! programmed the MSI address/data and set up the PCIe device and link control
//! words (`0xCC` = `0x0019_281F`, `0xD4` = `0x1012_0143`). The seed table below
//! restores those to their power-on values — command `0x0000`, BAR0
//! `0x0000_0004`, MSI address and data zero, DevCtl/DevSta `0x0010_2810`,
//! LnkCtl `0x0000` — and keeps everything else verbatim. `lspci -vvv` confirms
//! the shape: `Region 0: Memory at 600000000 (64-bit, non-prefetchable)
//! [size=4K]`, capabilities PM at `0x80`, MSI at `0x90`, PCIe at `0xC4`, AER at
//! `0x100`.

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

/// The whole configuration space is seeded storage behind a write mask, so
/// every register in `specs/vl805.toml` is modelled.
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

/// A PCIe function's configuration space is 4 KiB; only the first 0x200 bytes
/// are non-zero on this part.
const CFG_LEN: usize = 0x1000;

/// The measured config space, with the fields Linux had already written put
/// back to their power-on values (see the module docs).
const CFG_SEED: &[(usize, &[u8])] = &[
    // 0x00: vendor 1106, device 3483, command 0000 (power-on), status 0010,
    // rev 01, class 0c0330 (xHCI), cache line 0x10.
    (
        0x00,
        &[
            0x06, 0x11, 0x83, 0x34, 0x00, 0x00, 0x10, 0x00, 0x01, 0x30, 0x03, 0x0c, 0x10, 0x00,
            0x00, 0x00,
        ],
    ),
    // 0x10: BAR0, 64-bit memory, not yet assigned. 0x14 is its upper half.
    (0x10, &[0x04, 0x00, 0x00, 0x00]),
    // 0x2c: subsystem vendor/device = 1106:3483.
    (0x2c, &[0x06, 0x11, 0x83, 0x34]),
    // 0x34: capabilities pointer -> 0x80. 0x3c: interrupt line 0x1b, pin A.
    (0x34, &[0x80]),
    (0x3c, &[0x1b, 0x01, 0x00, 0x00]),
    // 0x44..0x5f: vendor-defined, verbatim from the dump.
    (
        0x44,
        &[
            0x00, 0x01, 0x00, 0x00, 0x09, 0x00, 0x80, 0x0e, 0x04, 0x00, 0x00, 0x00, 0xc0, 0x38,
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x11, 0x83, 0x34,
        ],
    ),
    (0x60, &[0x30, 0x20]),
    // 0x78/0x7c: the vendor indirect index/data port (see `Vl805::cfg_write`).
    // These are the values the port happened to hold on the running board.
    (0x78, &[0x08, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x18]),
    // 0x80: PCI Power Management capability, next -> 0x90.
    (0x80, &[0x01, 0x90, 0xc3, 0x89]),
    // 0x90: MSI capability, 4 vectors, 64-bit, next -> 0xc4. The address and
    // data words were programmed by Linux; power-on they are zero.
    (0x90, &[0x05, 0xc4, 0x80, 0x00]),
    // 0xc4: PCI Express capability (v2 endpoint). The capability words are
    // verbatim; the control words Linux wrote are back at their power-on
    // values.
    (0xc4, &[0x10, 0x00, 0x02, 0x00]),
    // 0xc8: DevCap — MaxPayload 256, RBE+.
    (0xc8, &[0x01, 0x80, 0x00, 0x00]),
    // 0xcc: DevCtl 0x2810 — the spec defaults RlxdOrd+ NoSnoop+ MaxReadReq 512
    // (Linux had added the four error-reporting enables, 0x281f) — and DevSta
    // 0x0010, AuxPwr+ (the running board also had CorrErr+ UnsupReq+ latched
    // from enumeration).
    (0xcc, &[0x10, 0x28, 0x10, 0x00]),
    // 0xd0: LnkCap — 5 GT/s x1, ASPM L0s L1, ClockPM+.
    (0xd0, &[0x12, 0x5c, 0x06, 0x00]),
    // 0xd4: LnkCtl 0x0000 (Linux had enabled ASPM, CommClk and ClockPM, 0x0143)
    // and LnkSta 0x1012, 5 GT/s x1 SlotClk+ — the link is trained by the time
    // anything can read it.
    (0xd4, &[0x00, 0x00, 0x12, 0x10]),
    // 0xe8: DevCap2 — completion timeout range B, TimeoutDis+.
    (0xe8, &[0x12]),
    // 0xf4: LnkCtl2 — target 5 GT/s, SpeedDis+ — and LnkSta2, -3.5 dB.
    (0xf4, &[0x22, 0x00, 0x01, 0x00]),
    // 0x100: AER extended capability.
    (0x100, &[0x01, 0x00, 0x01, 0x00]),
    (0x10c, &[0x31, 0x20, 0x06, 0x00]),
    (0x114, &[0x00, 0x20, 0x00, 0x00]),
];

/// Which bits of each config-space word the firmware may change. Everything not
/// listed is read-only, which is what makes BAR sizing work: writing all-ones
/// to `0x10` has to read back the size mask, not the value written.
fn cfg_write_mask(off: usize) -> u32 {
    match off as u32 {
        // Cache line size / latency timer.
        CACHE_LAT => 0x0000_FFFF,
        // Command register (low half). Status (high half) is write-1-to-clear
        // on real silicon; nothing in the firmware reads it back, so it is
        // modelled read-only.
        COMMAND_STATUS => 0x0000_0547,
        // BAR0: 4 KiB, 64-bit memory. Bits [3:0] are the hard-wired type.
        BAR0 => !(BAR0_SIZE - 1),
        BAR0_HI => 0xFFFF_FFFF,
        // Interrupt line.
        INTERRUPT => 0x0000_00FF,
        // The vendor index/data port.
        VENDOR_INDEX | VENDOR_DATA => 0xFFFF_FFFF,
        PM_CSR => 0xFFFF_FFFF,
        // MSI control: the enable bit and Multiple Message Enable. The rest
        // (64-bit capable, Multiple Message Capable) is fixed.
        MSI_CTRL => 0x0071_0000,
        MSI_ADDR_LO | MSI_ADDR_HI | MSI_DATA => 0xFFFF_FFFF,
        // PCIe DevCtl / LnkCtl / DevCtl2 / LnkCtl2, the low half of the word
        // after each capability word. The high halves are DevSta / LnkSta /
        // LnkSta2 (DevSta's error bits are write-1-to-clear on real silicon;
        // they never latch here) and DevCtl2's is reserved.
        PCIE_DEVCTL | PCIE_LNKCTL | PCIE_DEVCTL2 | PCIE_LNKCTL2 => 0x0000_FFFF,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// The xHCI register block behind BAR0 lives in [`super::xhci`]; this file keeps
// only the PCI-function half of the device.

pub use super::xhci::CAPLENGTH;

/// The VL805 endpoint's configuration space.
pub struct Vl805 {
    cfg: Vec<u8>,
    /// Sticky storage for the register file reached through the indirect
    /// index/data port at config `0x78`/`0x7C`. Both the EEPROM bootloader
    /// (around `0x000B6CE0`) and start4 (`0x3EDC5EF8`, `MCU FW: %x %x`) stream
    /// the VL805's firmware image through it and then read it back to verify —
    /// the bootloader byte by byte, bailing out with `HUB2.0 fail`
    /// (`0x000B6DF8`) on the first mismatch. Nothing here interprets the
    /// contents; storing and returning them is the whole job.
    vendor_regs: BTreeMap<u32, u32>,
    /// How many words have been pushed through the data port. Only an
    /// observable for tests.
    pub vendor_writes: u64,
    /// The xHCI controller behind BAR0 — registers, rings and the devices on
    /// the root ports. See [`super::xhci`].
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

    /// PERST#: the function comes back as it powered on — command register
    /// clear, BAR0 unassigned, MSI off — and so does the xHCI controller, so an
    /// interrupt it had pending is gone. Linux asserts PERST# before it starts
    /// the link (`brcm_pcie_setup()`); without this the firmware's last USB
    /// event left INTA asserted and nothing on the ARM ever cleared it.
    ///
    /// The vendor port's storage survives: nothing reads it back after a
    /// reset without writing it first.
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
        // A Pi 4B has a VIA Labs four-port hub soldered to xHCI root port 1 —
        // that is the USB2 half of all four type-A sockets, and it is there
        // whether or not anything is plugged in. Root ports 2 and 3 go to the
        // two blue sockets' SuperSpeed lanes; 4 and 5 go nowhere.
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
        // The data half of the vendor port reads back whatever the currently
        // selected index holds.
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

    /// Write `width` bytes of the xHCI register block behind BAR0. `mem` is
    /// host memory, because a doorbell write makes the controller fetch TRBs
    /// and post events.
    pub fn bar0_write(&mut self, off: u32, width: Width, value: u32, mem: &mut dyn HostMem) {
        self.xhci.write(off, width, value, mem);
    }

    /// The MSI the function sends when its interrupt fires, as `(address,
    /// data)`, if the host has enabled MSI in the capability at `0x90` (64-bit
    /// layout: address at `0x94`/`0x98`, data at `0x9C`). `lspci` on a
    /// Raspberry Pi 4B d03115: `MSI: Enable+ Count=4/4 Maskable- 64bit+`,
    /// `Address: 00000000fffffffc Data: 6540`. Only interrupter 0 is ever used,
    /// so the vector offset the function may OR into the data is always zero.
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

    /// Where BAR0 currently decodes on the PCI bus, if the firmware has both
    /// assigned it and enabled memory decoding. `None` = the window is off.
    ///
    /// The second-stage bootloader assigns `0x8200_0000` here and then writes
    /// `0x0146` to the command register — the same command word `lspci` reports
    /// on the running board.
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

    /// The seed holds what `specs/vl805.toml` says the PCIe capability's words
    /// reset to: the measured capability words, the control words at power-on.
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
