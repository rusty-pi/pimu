//! The DesignWare USB 2.0 OTG controller at `0x7E98_0000`, the Pi 4's USB-C
//! port: the reset start4 runs when USB power comes on, and a host port with
//! nothing plugged in for UEFI's driver to find (#49).
//!
//! ## start4
//!
//! Once [`super::hd`] acknowledges the power request, the `SET_POWER_STATE`
//! handler resets the core and flushes its FIFOs, spinning on each
//! self-clearing bit of `GRSTCTL` with no timeout (`0x3ED8958A..0x3ED895B2`):
//!
//! ```c
//! GRSTCTL = CSFTRST;                 while (GRSTCTL & CSFTRST) { }
//! GRSTCTL = HSFTRST;                 while (GRSTCTL & HSFTRST) { }
//! GRSTCTL = TXFNUM(0x10) | TXFFLSH;  while (GRSTCTL & TXFFLSH) { }
//! GRSTCTL = RXFFLSH;                 while (GRSTCTL & RXFFLSH) { }
//! ```
//!
//! The catch-all stub reads back what was written, so the first of those loops
//! would never end. Here the reset and flush bits are done by the time anyone
//! reads them, since the core has nothing to drain, and `AHBIDLE` always reads 1.
//!
//! ## UEFI
//!
//! It is edk2's `DwUsbHostDxe` that asks for USB power, and once it has it the
//! driver brings the controller up as a host. `DwHcInit` takes the number of
//! host channels from `GHWCFG2`, then halts each channel and waits for
//! `HCCHAR.CHENA` to clear. It gives each wait ten seconds, polled on the ARM,
//! so a model that keeps `CHENA` set costs ten seconds of guest time per
//! channel. Then it powers the root port if `GINTSTS.CURMOD` says host, and
//! UEFI's USB bus driver reads the port status from `HPRT0`.
//!
//! ## Ground truth
//!
//! Read on a Raspberry Pi 4B d03115 through `/dev/mem` after `vcmailbox` asked
//! the firmware for USB power (the window was not read with USB off):
//!
//! ```text
//!   GOTGCTL  0x001C0000   GSNPSID  0x4F54280A   (OTG 2.80a)
//!   GUSBCFG  0x20402700   GHWCFG1  0x00000000
//!   GRSTCTL  0x80000000   GHWCFG2  0x228DDD50   (8 host channels)
//!   GINTSTS  0x5400002B   GHWCFG3  0x0FF000E8
//!   HPRT0    0x00000000   GHWCFG4  0x1FF00020
//!   HCCHAR0  0x00000000   HCINT0   0x00000000
//! ```
//!
//! `GRSTCTL` has `TXFNUM` 0 because the firmware's last write was a bare
//! `RXFFLSH`. The id and configuration words are modelled because a word that
//! reads 0 there means "no such core" or "one channel" to whoever checks it.
//! `GOTGCTL.CONIDSTS` is 0 (an A-device), which is why the core is a host
//! unless `GUSBCFG` forces device mode.
//!
//! ## Not modelled
//!
//! Transfers: nothing is attached to the port, so `HPRT0` reports no
//! connection and a channel only ever halts. Everything else is plain
//! storage, and a core soft reset does not return it to its reset values.
//!
//! ## Device mode
//!
//! The boot ROM forces device mode for rpiboot (`GUSBCFG` `0x40402700`) and
//! then polls `GINTSTS` for a host's bus reset. With no host the bus stays
//! idle, and after 3 ms a connected device core reports a suspend
//! (`ERLYSUSP` and `USBSUSP`, `DSTS.SUSPSTS`); the ROM's poll takes `USBSUSP`
//! as "nobody there" and moves on (#68). The model latches the pair as the
//! core connects – forced into device mode with `DCTL.SFTDISCON` clear, or
//! `SFTDISCON` cleared in device mode – without the 3 ms. Those are the only
//! latched `GINTSTS` bits that ever set; writing 1 clears them. The global NAK
//! set / clear bits in `DCTL` take effect at once, since nothing is in flight.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};

use crate::spec::dwc2::{
    DCTL, DCTL_CGNPINNAK_MASK, DCTL_CGOUTNAK_MASK, DCTL_GNPINNAKSTS_MASK, DCTL_GOUTNAKSTS_MASK,
    DCTL_SFTDISCON_MASK, DCTL_SGNPINNAK_MASK, DCTL_SGOUTNAK_MASK, DSTS, DSTS_SUSPSTS_MASK, GHWCFG1,
    GHWCFG1_RESET, GHWCFG2, GHWCFG2_RESET, GHWCFG3, GHWCFG3_RESET, GHWCFG4, GHWCFG4_RESET, GINTSTS,
    GINTSTS_CURMOD_MASK, GINTSTS_ERLYSUSP_MASK, GINTSTS_NPTXFEMP_MASK, GINTSTS_PTXFEMP_MASK,
    GINTSTS_USBSUSP_MASK, GRSTCTL, GRSTCTL_AHBIDLE_MASK, GRSTCTL_TXFNUM_MASK, GSNPSID,
    GSNPSID_RESET, GUSBCFG, GUSBCFG_FORCEDEVMODE_MASK, HCCHAR, HCCHAR_CHDIS_MASK,
    HCCHAR_CHENA_MASK, HCCHAR_COUNT, HCCHAR_STRIDE, HCINT, HCINT_CHHLTD_MASK, HPRT0,
    HPRT0_PRTCONNDET_MASK, HPRT0_PRTCONNSTS_MASK, HPRT0_PRTENA_MASK, HPRT0_PRTENCHNG_MASK,
    HPRT0_PRTLNSTS_MASK, HPRT0_PRTOVRCURRACT_MASK, HPRT0_PRTOVRCURRCHNG_MASK, HPRT0_PRTSPD_MASK,
};
use crate::spec::Coverage;

/// The reset handshake, the id and configuration words, host mode, channel
/// halts and an empty root port; the rest of the window is storage.
pub const COVERAGE: Coverage = Coverage {
    block: "dwc2",
    decoded: &[
        GRSTCTL, GINTSTS, GSNPSID, GHWCFG1, GHWCFG2, GHWCFG3, GHWCFG4, HPRT0, HCCHAR, HCINT, DCTL,
        DSTS,
    ],
};

/// The `HPRT0` bits that report on an attached device. With nothing attached
/// they all read 0, whatever was written.
const HPRT0_STATUS: u32 = HPRT0_PRTCONNSTS_MASK
    | HPRT0_PRTCONNDET_MASK
    | HPRT0_PRTENA_MASK
    | HPRT0_PRTENCHNG_MASK
    | HPRT0_PRTOVRCURRACT_MASK
    | HPRT0_PRTOVRCURRCHNG_MASK
    | HPRT0_PRTLNSTS_MASK
    | HPRT0_PRTSPD_MASK;

const HALT: u32 = HCCHAR_CHENA_MASK | HCCHAR_CHDIS_MASK;

/// What a connected device core with no host reports ([`Dwc2::connected`]).
const SUSPEND: u32 = GINTSTS_ERLYSUSP_MASK | GINTSTS_USBSUSP_MASK;

/// `DCTL`'s global NAK status bits, and the write-only bits that set and
/// clear them.
const NAK_STATUS: u32 = DCTL_GNPINNAKSTS_MASK | DCTL_GOUTNAKSTS_MASK;
const NAK_SET_CLEAR: u32 =
    DCTL_SGNPINNAK_MASK | DCTL_CGNPINNAK_MASK | DCTL_SGOUTNAK_MASK | DCTL_CGOUTNAK_MASK;

#[derive(Default)]
pub struct Dwc2 {
    storage: BTreeMap<u32, u32>,
    /// Where [`Channel::Dwc2`] goes: every write, and every read that returns
    /// something other than the previous read of the same register, so a poll
    /// shows up once rather than once per iteration.
    pub log: Log,
    last_read: BTreeMap<u32, u32>,
    /// The latched `GINTSTS` bits: only [`SUSPEND`] ever sets.
    latched: u32,
}

impl Dwc2 {
    pub fn new() -> Dwc2 {
        Dwc2::default()
    }

    fn stored(&self, off: u32) -> u32 {
        self.storage.get(&off).copied().unwrap_or(0)
    }

    fn device_mode(&self) -> bool {
        self.stored(GUSBCFG) & GUSBCFG_FORCEDEVMODE_MASK != 0
    }

    /// A device-mode core with its pull-up on. Nothing is ever attached, so
    /// the bus it is on is idle.
    fn connected(&self) -> bool {
        self.device_mode() && self.stored(DCTL) & DCTL_SFTDISCON_MASK == 0
    }

    /// The channel `n` for which `off` is `base + n * HCCHAR_STRIDE`, if any.
    fn channel(off: u32, base: u32) -> Option<u32> {
        let rel = off.checked_sub(base)?;
        (rel % HCCHAR_STRIDE == 0 && rel / HCCHAR_STRIDE < HCCHAR_COUNT)
            .then_some(rel / HCCHAR_STRIDE)
    }
}

impl MmioDevice for Dwc2 {
    fn name(&self) -> &'static str {
        "dwc2"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        let stored = self.stored(off);
        let value = match off {
            GRSTCTL => GRSTCTL_AHBIDLE_MASK | (stored & GRSTCTL_TXFNUM_MASK),
            GINTSTS => {
                let host = if self.device_mode() {
                    0
                } else {
                    GINTSTS_CURMOD_MASK
                };
                host | GINTSTS_NPTXFEMP_MASK | GINTSTS_PTXFEMP_MASK | self.latched
            }
            DSTS => {
                if self.connected() {
                    DSTS_SUSPSTS_MASK
                } else {
                    0
                }
            }
            GSNPSID => GSNPSID_RESET,
            GHWCFG1 => GHWCFG1_RESET,
            GHWCFG2 => GHWCFG2_RESET,
            GHWCFG3 => GHWCFG3_RESET,
            GHWCFG4 => GHWCFG4_RESET,
            _ => stored,
        };
        if self.log.on(Channel::Dwc2) && self.last_read.insert(off, value) != Some(value) {
            crate::log!(
                self.log,
                Channel::Dwc2,
                "read  +{off:#05x} -> {value:#010x}"
            );
        }
        Ok(value)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        crate::log!(
            self.log,
            Channel::Dwc2,
            "write +{off:#05x} <- {value:#010x}"
        );
        if let Some(ch) = Dwc2::channel(off, HCCHAR) {
            if value & HALT == HALT {
                // No channel ever has a transfer in flight, so a halt request
                // completes at once.
                self.storage.insert(off, value & !HALT);
                *self.storage.entry(HCINT + ch * HCCHAR_STRIDE).or_default() |= HCINT_CHHLTD_MASK;
                return Ok(());
            }
        } else if Dwc2::channel(off, HCINT).is_some() {
            let left = self.stored(off) & !value;
            self.storage.insert(off, left);
            return Ok(());
        }
        let was_connected = self.connected();
        let dctl_before = self.stored(DCTL);
        match off {
            // Write 1 to clear; the levels are derived.
            GINTSTS => self.latched &= !value,
            HPRT0 => {
                self.storage.insert(off, value & !HPRT0_STATUS);
            }
            _ => {
                self.storage.insert(off, value);
            }
        }
        if self.connected() && !was_connected {
            self.latched |= SUSPEND;
        }
        if off == DCTL {
            // The set / clear bits are write-only; they act on the status bits.
            let mut v = value & !(NAK_SET_CLEAR | NAK_STATUS);
            v |= dctl_before & NAK_STATUS;
            for (set, clear, status) in [
                (
                    DCTL_SGNPINNAK_MASK,
                    DCTL_CGNPINNAK_MASK,
                    DCTL_GNPINNAKSTS_MASK,
                ),
                (DCTL_SGOUTNAK_MASK, DCTL_CGOUTNAK_MASK, DCTL_GOUTNAKSTS_MASK),
            ] {
                if value & set != 0 {
                    v |= status;
                }
                if value & clear != 0 {
                    v &= !status;
                }
            }
            self.storage.insert(DCTL, v);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The boot ROM's rpiboot attempt (#68): device mode, no host, so the
    /// core reports a suspend, which the ROM clears by writing it back.
    #[test]
    fn a_device_with_no_host_reports_a_suspend() {
        let mut d = Dwc2::new();
        assert_eq!(d.read(GINTSTS, Width::Word).unwrap() & SUSPEND, 0);
        d.write(GUSBCFG, Width::Word, 0x4040_2700).unwrap();
        let sts = d.read(GINTSTS, Width::Word).unwrap();
        assert_eq!(sts & (SUSPEND | GINTSTS_CURMOD_MASK), SUSPEND);
        assert_eq!(d.read(DSTS, Width::Word).unwrap(), DSTS_SUSPSTS_MASK);
        d.write(GINTSTS, Width::Word, sts).unwrap();
        assert_eq!(d.read(GINTSTS, Width::Word).unwrap() & SUSPEND, 0);
    }

    /// start4's host mode, and a device core that is soft-disconnected, see
    /// no suspend.
    #[test]
    fn a_host_or_a_disconnected_device_reports_none() {
        let mut d = Dwc2::new();
        d.write(GUSBCFG, Width::Word, 0x2040_2700).unwrap();
        assert_eq!(d.read(GINTSTS, Width::Word).unwrap() & SUSPEND, 0);

        let mut d = Dwc2::new();
        d.write(DCTL, Width::Word, DCTL_SFTDISCON_MASK).unwrap();
        d.write(GUSBCFG, Width::Word, 0x4040_2700).unwrap();
        assert_eq!(d.read(GINTSTS, Width::Word).unwrap() & SUSPEND, 0);
        assert_eq!(d.read(DSTS, Width::Word).unwrap(), 0);
        // Connecting later reports it then.
        d.write(DCTL, Width::Word, 0).unwrap();
        assert_eq!(d.read(GINTSTS, Width::Word).unwrap() & SUSPEND, SUSPEND);
    }

    /// What the boot ROM does on that suspend (0x60001bf6, #68): set the
    /// global OUT NAK, wait for its status, clear it again.
    #[test]
    fn the_global_nak_bits_take_effect_at_once() {
        let mut d = Dwc2::new();
        let dctl = d.read(DCTL, Width::Word).unwrap();
        d.write(DCTL, Width::Word, dctl | DCTL_SGOUTNAK_MASK)
            .unwrap();
        let dctl = d.read(DCTL, Width::Word).unwrap();
        assert_eq!(dctl, DCTL_GOUTNAKSTS_MASK);
        d.write(DCTL, Width::Word, dctl | DCTL_SGNPINNAK_MASK)
            .unwrap();
        assert_eq!(d.read(DCTL, Width::Word).unwrap(), NAK_STATUS);
        d.write(DCTL, Width::Word, DCTL_CGOUTNAK_MASK | DCTL_CGNPINNAK_MASK)
            .unwrap();
        assert_eq!(d.read(DCTL, Width::Word).unwrap(), 0);
    }
}
