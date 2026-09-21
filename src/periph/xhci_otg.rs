//! The BCM2711's own xHCI controller at `0x7E9C_0000` — the USB-C port as a
//! USB 2.0 host (#113).
//!
//! The Pi 4's USB-C socket has two controllers behind it and they are
//! alternatives: the DWC2 OTG core at `0x7E98_0000` ([`super::dwc2`]), which is
//! what a stock boot uses, and this one. `config.txt`'s `otg_mode=1` picks this
//! one for Linux — the device tree ships `/scb/xhci@7e9c0000`
//! (`compatible = "generic-xhci"`) with `status = "disabled"` and the firmware
//! turns it on — and the bootloader boots from a mass-storage device on it as
//! `BCM-USB-MSD`, `BOOT_ORDER` digit `0x5`.
//!
//! ## Why this block and not the DWC2
//!
//! `boot --eeprom firmware/pieeprom.bin --boot-order 0x5 -v` prints
//! `Boot mode: BCM-USB-MSD (05) order 0` and then reads `0x7E9C_0000`
//! `+0x00`..`+0x1C`, the xHCI capability registers, and nothing else. Before
//! this file existed those reads fell through to the catch-all stub, so the
//! bootloader saw
//!
//! ```text
//!   xHC0 ver: 0 HCS: 00000000 00000000 00000000 HCC: 00000000
//!   xHC0 ports 0 slots 0 intrs 0
//!   USB xHC init failed
//! ```
//!
//! and went on to its next boot mode. The `dwc2` log channel is empty for that
//! whole run: the boot mode named after the Broadcom USB controller never
//! touches the DWC2 core.
//!
//! ## What it is
//!
//! A plain xHCI, so the ring engine is [`super::xhci`]'s — the same one the
//! VL805's controller runs on, told a different [`Caps`]: one USB2 root port,
//! one interrupter, no streams and no scratchpad buffers. Those values are
//! inferred rather than measured (`specs/xhci_otg.toml`); what a driver needs
//! of them is that they are self-consistent and that the register block is
//! where `CAPLENGTH`, `DBOFF` and `RTSOFF` say it is.
//!
//! Two things differ from the VL805's:
//!
//! * **Memory.** The endpoint behind PCIe reaches DRAM through the root
//!   complex's inbound window; this controller is on the SoC's own bus, where
//!   `/scb dma-ranges` is the identity over 16 GB. So a ring address is a CPU
//!   physical address handed straight to [`Ram`]'s own host-memory view: the
//!   firmware's rings sit in the low gigabyte, Linux's above 4 GB on a board
//!   that big.
//! * **Interrupt.** No MSI and no INTA: one level line, `GIC_SPI 176`
//!   (id [`crate::spec::xhci_otg::IRQ_GIC`]), driven by interrupter 0's
//!   `IMAN.IP`. The bootloader polls instead; Linux uses the line.
//!
//! A register write can make the controller fetch TRBs and post events, which
//! needs host memory, and [`crate::bus::MmioDevice`] has none to give. So a
//! write is parked here and [`Machine::store_device`] runs it with the
//! machine's DRAM, the way it does an EMMC2 DMA — one write per access, drained
//! before the access returns, so nothing observes a half-applied write.
//!
//! [`Machine::store_device`]: crate::machine::Machine

use crate::bus::{BusResult, MmioDevice, Width};
use crate::log::Log;
use crate::mem::Ram;
use crate::periph::usb::UsbDevice;
use crate::periph::xhci::{Caps, Xhci};
use crate::spec::xhci_otg as regs;
use crate::spec::Coverage;

/// Every register in `specs/xhci_otg.toml` is the shared engine's, and it
/// decodes all of them.
pub const COVERAGE: Coverage = Coverage {
    block: "xhci_otg",
    decoded: &[
        regs::CAPLENGTH,
        regs::HCIVERSION,
        regs::HCSPARAMS1,
        regs::HCSPARAMS2,
        regs::HCSPARAMS3,
        regs::HCCPARAMS1,
        regs::DBOFF,
        regs::RTSOFF,
        regs::HCCPARAMS2,
        regs::USBCMD,
        regs::USBSTS,
        regs::PAGESIZE,
        regs::DNCTRL,
        regs::CRCR_LO,
        regs::CRCR_HI,
        regs::DCBAAP_LO,
        regs::DCBAAP_HI,
        regs::CONFIG,
        regs::USBLEGSUP,
        regs::SUPPORTED_USB2,
        regs::SUPPORTED_USB2_NAME,
        regs::SUPPORTED_USB2_PORTS,
        regs::DOORBELL,
        regs::MFINDEX,
        regs::IMAN,
        regs::IMOD,
        regs::ERSTSZ,
        regs::ERSTBA_LO,
        regs::ERSTBA_HI,
        regs::ERDP_LO,
        regs::ERDP_HI,
        regs::PORTSC,
        regs::PORTPMSC,
        regs::PORTLI,
        regs::PORTHLPMC,
    ],
};

/// What this controller reports about itself (`specs/xhci_otg.toml`): one USB2
/// root port on the USB-C socket, 32 slots, one interrupter, and an
/// extended-capability list with only the USB 2.0 half of the VL805's.
pub const CAPS: Caps = Caps {
    words: &[
        (
            regs::CAPLENGTH,
            regs::CAPLENGTH_RESET | regs::HCIVERSION_RESET << 16,
        ),
        (regs::HCSPARAMS1, regs::HCSPARAMS1_RESET),
        (regs::HCSPARAMS2, regs::HCSPARAMS2_RESET),
        (regs::HCSPARAMS3, regs::HCSPARAMS3_RESET),
        (regs::HCCPARAMS1, regs::HCCPARAMS1_RESET),
        (regs::DBOFF, regs::DBOFF_RESET),
        (regs::RTSOFF, regs::RTSOFF_RESET),
        (regs::HCCPARAMS2, regs::HCCPARAMS2_RESET),
        (regs::USBLEGSUP, regs::USBLEGSUP_RESET),
        (regs::SUPPORTED_USB2, regs::SUPPORTED_USB2_RESET),
        (regs::SUPPORTED_USB2_NAME, regs::SUPPORTED_USB2_NAME_RESET),
        (regs::SUPPORTED_USB2_PORTS, regs::SUPPORTED_USB2_PORTS_RESET),
    ],
    usb2_ports: &[true],
    max_slots: (regs::HCSPARAMS1_RESET & 0xFF) as usize,
    tag: "otg ",
};
// The register block has to be where the capability values say, and the port
// and doorbell counts what `HCSPARAMS1` announces.
const _: () = assert!(
    regs::USBCMD == regs::CAPLENGTH_RESET
        && regs::IMAN == regs::RTSOFF_RESET + 0x20
        && regs::DOORBELL == regs::DBOFF_RESET
        && CAPS.usb2_ports.len() == (regs::HCSPARAMS1_RESET >> 24) as usize
        && CAPS.max_slots + 1 == regs::DOORBELL_COUNT as usize
);

/// The one root port: the USB-C socket.
pub const PORT: usize = 1;

/// The controller, and the one register write waiting for host memory.
pub struct XhciOtg {
    hc: Xhci,
    /// The write [`XhciOtg::run_pending`] still has to apply.
    pending: Option<(u32, Width, u32)>,
}

impl Default for XhciOtg {
    fn default() -> Self {
        XhciOtg::new()
    }
}

impl XhciOtg {
    pub fn new() -> XhciOtg {
        XhciOtg {
            hc: Xhci::with_caps(&CAPS),
            pending: None,
        }
    }

    /// Plug a device into the USB-C socket.
    pub fn attach(&mut self, device: Box<dyn UsbDevice>) {
        self.hc.attach(PORT, device);
    }

    /// Whether anything is plugged in, i.e. whether the port is worth a look.
    pub fn populated(&mut self) -> bool {
        self.hc.port_device(PORT).is_some()
    }

    /// Where [`crate::log::Channel::Xhci`] goes for this controller.
    pub fn set_log(&mut self, log: Log) {
        self.hc.log = log;
    }

    /// A register write is waiting for [`Self::run_pending`].
    pub fn write_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Apply the parked write, which may run a ring in `ram`.
    pub fn run_pending(&mut self, ram: &mut Ram) {
        if let Some((off, width, value)) = self.pending.take() {
            self.hc.write(off, width, value, ram);
        }
    }

    /// The level on `GIC_SPI 176`.
    pub fn irq_asserted(&self) -> bool {
        self.hc.interrupt_pending()
    }
}

impl MmioDevice for XhciOtg {
    fn name(&self) -> &'static str {
        "xhci_otg"
    }

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32> {
        Ok(self.hc.read(offset, width))
    }

    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()> {
        // Parked, not applied: the engine needs DRAM, which the caller has.
        self.pending = Some((offset, width, value));
        Ok(())
    }

    fn irq_pending(&self) -> bool {
        self.irq_asserted()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::periph::usb::MassStorage;

    fn ram() -> Ram {
        Ram::new(0, 64 * 1024)
    }

    /// What the bootloader reads first: the capability registers, which have to
    /// describe one USB2 port and the register block the engine implements.
    #[test]
    fn the_capability_registers_describe_a_one_port_controller() {
        let mut d = XhciOtg::new();
        let word = d.read(regs::CAPLENGTH, Width::Word).unwrap();
        assert_eq!(word & 0xFF, regs::CAPLENGTH_RESET, "CAPLENGTH");
        assert_eq!(word >> 16, regs::HCIVERSION_RESET, "HCIVERSION");
        let hcs1 = d.read(regs::HCSPARAMS1, Width::Word).unwrap();
        assert_eq!(hcs1 >> 24, 1, "one root port");
        assert_eq!(hcs1 & 0xFF, 32, "32 slots");
        assert_eq!((hcs1 >> 8) & 0x7FF, 1, "one interrupter");
        // The extended-capability list ends at the USB 2.0 entry: no
        // SuperSpeed protocol, so the host never looks for a USB3 port.
        assert_eq!(
            d.read(regs::SUPPORTED_USB2, Width::Word).unwrap() >> 8 & 0xFF,
            0
        );
    }

    /// An empty socket reads the same resting word the VL805's USB2 root port
    /// does, and a stick in it reports a connection the host has to reset.
    #[test]
    fn the_port_reports_what_is_plugged_in() {
        let mut d = XhciOtg::new();
        assert!(!d.populated());
        assert_eq!(d.read(regs::PORTSC, Width::Word).unwrap(), 0x4000_02A0);
        d.attach(Box::new(MassStorage::new(vec![0; 4096])));
        assert!(d.populated());
        assert_eq!(d.read(regs::PORTSC, Width::Word).unwrap(), 0x4002_02E1);
    }

    /// A write only takes effect once the machine hands the device its DRAM,
    /// and then it does: `RS` clears `USBSTS.HCH`.
    #[test]
    fn a_write_waits_for_host_memory() {
        let mut d = XhciOtg::new();
        let mut ram = ram();
        assert_eq!(d.read(regs::USBSTS, Width::Word).unwrap() & 1, 1, "halted");
        d.write(regs::USBCMD, Width::Word, 1).unwrap();
        assert!(d.write_pending());
        assert_eq!(
            d.read(regs::USBSTS, Width::Word).unwrap() & 1,
            1,
            "still halted"
        );
        d.run_pending(&mut ram);
        assert!(!d.write_pending());
        assert_eq!(d.read(regs::USBSTS, Width::Word).unwrap() & 1, 0, "running");
    }
}
