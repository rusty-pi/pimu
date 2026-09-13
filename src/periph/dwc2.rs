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
//! Read on `rpi-dev` through `/dev/mem` after `vcmailbox` asked the firmware
//! for USB power (the window was not read with USB off):
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
//! connection and a channel only ever halts. The latched `GINTSTS` interrupt
//! bits never set. Everything else is plain storage, and a core soft reset
//! does not return it to its reset values.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::dwc2::{
    GHWCFG1, GHWCFG1_RESET, GHWCFG2, GHWCFG2_RESET, GHWCFG3, GHWCFG3_RESET, GHWCFG4, GHWCFG4_RESET,
    GINTSTS, GINTSTS_CURMOD_MASK, GINTSTS_NPTXFEMP_MASK, GINTSTS_PTXFEMP_MASK, GRSTCTL,
    GRSTCTL_AHBIDLE_MASK, GRSTCTL_TXFNUM_MASK, GSNPSID, GSNPSID_RESET, GUSBCFG,
    GUSBCFG_FORCEDEVMODE_MASK, HCCHAR, HCCHAR_CHDIS_MASK, HCCHAR_CHENA_MASK, HCCHAR_COUNT,
    HCCHAR_STRIDE, HCINT, HCINT_CHHLTD_MASK, HPRT0, HPRT0_PRTCONNDET_MASK, HPRT0_PRTCONNSTS_MASK,
    HPRT0_PRTENA_MASK, HPRT0_PRTENCHNG_MASK, HPRT0_PRTLNSTS_MASK, HPRT0_PRTOVRCURRACT_MASK,
    HPRT0_PRTOVRCURRCHNG_MASK, HPRT0_PRTSPD_MASK,
};
use crate::spec::Coverage;

/// The reset handshake, the id and configuration words, host mode, channel
/// halts and an empty root port; the rest of the window is storage.
pub const COVERAGE: Coverage = Coverage {
    block: "dwc2",
    decoded: &[
        GRSTCTL, GINTSTS, GSNPSID, GHWCFG1, GHWCFG2, GHWCFG3, GHWCFG4, HPRT0, HCCHAR, HCINT,
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

#[derive(Default)]
pub struct Dwc2 {
    storage: BTreeMap<u32, u32>,
    /// `RVF_DBG_DWC2`: log every write, and every read that returns something
    /// other than the previous read of the same register, so a poll shows up
    /// once rather than once per iteration.
    dbg: bool,
    last_read: BTreeMap<u32, u32>,
}

impl Dwc2 {
    pub fn new() -> Dwc2 {
        Dwc2 {
            dbg: std::env::var_os("RVF_DBG_DWC2").is_some(),
            ..Dwc2::default()
        }
    }

    fn stored(&self, off: u32) -> u32 {
        self.storage.get(&off).copied().unwrap_or(0)
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
                let host = if self.stored(GUSBCFG) & GUSBCFG_FORCEDEVMODE_MASK == 0 {
                    GINTSTS_CURMOD_MASK
                } else {
                    0
                };
                host | GINTSTS_NPTXFEMP_MASK | GINTSTS_PTXFEMP_MASK
            }
            GSNPSID => GSNPSID_RESET,
            GHWCFG1 => GHWCFG1_RESET,
            GHWCFG2 => GHWCFG2_RESET,
            GHWCFG3 => GHWCFG3_RESET,
            GHWCFG4 => GHWCFG4_RESET,
            _ => stored,
        };
        if self.dbg && self.last_read.insert(off, value) != Some(value) {
            eprintln!("[dwc2] read  +{off:#05x} -> {value:#010x}");
        }
        Ok(value)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        if self.dbg {
            eprintln!("[dwc2] write +{off:#05x} <- {value:#010x}");
        }
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
        match off {
            // The latched interrupt bits never set, and the levels are derived.
            GINTSTS => {}
            HPRT0 => {
                self.storage.insert(off, value & !HPRT0_STATUS);
            }
            _ => {
                self.storage.insert(off, value);
            }
        }
        Ok(())
    }
}
