//! BCM2711 HDMI controller core registers — the `hdmi` window of each
//! connector (`0x7EF0_0700`, `0x7EF0_5700`) — with no monitor attached.
//!
//! No encoder or PHY stands behind the window. What is modelled is what
//! firmware waits on; the rest of the window is plain sticky storage.
//!
//! ## Packet RAM (#61)
//!
//! The controller sends the packets in its packet RAM (infoframes, the general
//! control packet) with every frame, one enable bit per slot in
//! `RAM_PACKET_CONFIG`, and `RAM_PACKET_STATUS` reports which slots it is
//! sending. Rewriting a packet means turning its slot off, waiting for the
//! status to follow, writing it, turning it back on and waiting again. start4
//! does that with no timeout when it stops its display (`0x3ECDFD68`, the
//! AV-mute general control packet). On the catch-all stub the status never
//! moved, so the mailbox thread serving `NOTIFY_DISPLAY_DONE` spun there for
//! good. Linux's vc4 does the same with a 100 ms timeout.
//!
//! Model: `RAM_PACKET_STATUS` follows the enable bits at once.
//!
//! ## FIFO recenter (#63)
//!
//! After a mode set, software recentres the FIFO between the pixel valve and
//! the encoder: it pulses `FIFO_CTL.RECENTER` and waits for `RECENTER_DONE`.
//! The 2020-era bootcode (`pieeprom-2020-09-03`) brings HDMI up on every boot
//! and spins on that bit with no timeout (`0x8000777E`). It reads neither
//! `HOTPLUG` nor the DDC bus first, so headless boards take the same path, and
//! the recenter cannot need a sink. On plain storage the bit never set, and
//! those EEPROM images never got past it. Linux's vc4 waits 1 ms and warns.
//!
//! Model: any write with `RECENTER` set finishes a recenter at once and sets
//! `RECENTER_DONE`, which then stays set.
//!
//! ## Hotplug
//!
//! Neither connector has a monitor, as on the reference board: `HOTPLUG`
//! reads 0, so `CONNECTED` (the HPD line) is clear. start4 reads it on both
//! connectors during boot, and Linux's `vc5_hdmi_hp_detect` reports it. The
//! DDC side of "no monitor" is [`super::hdmi_ddc`]: no EDID EEPROM answers.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::spec::hdmi::{
    FIFO_CTL, FIFO_CTL_RECENTER_DONE_MASK, FIFO_CTL_RECENTER_MASK, HOTPLUG, RAM_PACKET_CONFIG,
    RAM_PACKET_CONFIG_PACKETS_MASK, RAM_PACKET_STATUS,
};
use crate::spec::Coverage;

/// The packet-RAM handshake, the FIFO recenter and the hotplug state are
/// modelled, on both connectors; the rest of the window is storage.
pub const COVERAGE: Coverage = Coverage {
    block: "hdmi",
    decoded: &[FIFO_CTL, RAM_PACKET_CONFIG, RAM_PACKET_STATUS, HOTPLUG],
};

pub struct Hdmi {
    name: &'static str,
    storage: BTreeMap<u32, u32>,
    /// `FIFO_CTL.RECENTER_DONE`.
    recenter_done: bool,
}

impl Hdmi {
    pub fn new(name: &'static str) -> Hdmi {
        Hdmi {
            name,
            storage: BTreeMap::new(),
            recenter_done: false,
        }
    }

    fn reg(&self, off: u32) -> u32 {
        self.storage.get(&off).copied().unwrap_or(0)
    }
}

impl MmioDevice for Hdmi {
    fn name(&self) -> &'static str {
        self.name
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        Ok(match off {
            FIFO_CTL if self.recenter_done => self.reg(off) | FIFO_CTL_RECENTER_DONE_MASK,
            // The encoder takes a slot on or off as soon as it is told to.
            RAM_PACKET_STATUS => self.reg(RAM_PACKET_CONFIG) & RAM_PACKET_CONFIG_PACKETS_MASK,
            // Nothing plugged in.
            HOTPLUG => 0,
            _ => self.reg(off),
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        match off {
            RAM_PACKET_STATUS | HOTPLUG => {}
            FIFO_CTL => {
                if value & FIFO_CTL_RECENTER_MASK != 0 {
                    self.recenter_done = true;
                }
                self.storage
                    .insert(off, value & !FIFO_CTL_RECENTER_DONE_MASK);
            }
            _ => {
                self.storage.insert(off, value);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(h: &mut Hdmi) -> u32 {
        h.read(RAM_PACKET_STATUS, Width::Word).unwrap()
    }

    fn fifo_ctl(h: &mut Hdmi) -> u32 {
        h.read(FIFO_CTL, Width::Word).unwrap()
    }

    #[test]
    fn a_packets_status_follows_its_enable() {
        // start4's AV-mute write (`0x3ECDFD68`): slot 0 off, wait, write the
        // packet, slot 0 on, wait.
        let mut h = Hdmi::new("hdmi0");
        h.write(RAM_PACKET_CONFIG, Width::Word, 1).unwrap();
        assert_eq!(status(&mut h) & 1, 1);
        h.write(RAM_PACKET_CONFIG, Width::Word, 0).unwrap();
        assert_eq!(status(&mut h) & 1, 0);
        h.write(RAM_PACKET_CONFIG, Width::Word, 1).unwrap();
        assert_eq!(status(&mut h) & 1, 1);
    }

    #[test]
    fn only_the_slot_bits_show_in_the_status() {
        let mut h = Hdmi::new("hdmi0");
        h.write(RAM_PACKET_CONFIG, Width::Word, 0x0001_0005)
            .unwrap();
        assert_eq!(status(&mut h), 0x5);
        h.write(RAM_PACKET_STATUS, Width::Word, 0xFFFF).unwrap();
        assert_eq!(status(&mut h), 0x5, "read-only");
        assert_eq!(h.read(RAM_PACKET_CONFIG, Width::Word).unwrap(), 0x0001_0005);
    }

    #[test]
    fn the_2020_bootcode_recenter_finishes() {
        // pieeprom-2020-09-03 bootcode: 0x5, then a 0x45 pulse, then it spins
        // on bit 14.
        let mut h = Hdmi::new("hdmi0");
        h.write(FIFO_CTL, Width::Word, 0x5).unwrap();
        assert_eq!(fifo_ctl(&mut h), 0x5, "no recenter asked for yet");
        h.write(FIFO_CTL, Width::Word, 0x45).unwrap();
        h.write(FIFO_CTL, Width::Word, 0x5).unwrap();
        assert_eq!(fifo_ctl(&mut h), 0x5 | FIFO_CTL_RECENTER_DONE_MASK);
    }

    #[test]
    fn linux_recenter_finishes_with_recenter_still_set() {
        // vc4_hdmi_recenter_fifo(): clear, set, then wait with RECENTER set.
        let mut h = Hdmi::new("hdmi1");
        let drift = 0x5;
        h.write(FIFO_CTL, Width::Word, drift).unwrap();
        h.write(FIFO_CTL, Width::Word, drift | FIFO_CTL_RECENTER_MASK)
            .unwrap();
        assert_ne!(fifo_ctl(&mut h) & FIFO_CTL_RECENTER_DONE_MASK, 0);
    }

    #[test]
    fn recenter_done_cannot_be_written() {
        let mut h = Hdmi::new("hdmi0");
        h.write(FIFO_CTL, Width::Word, FIFO_CTL_RECENTER_DONE_MASK | 0x5)
            .unwrap();
        assert_eq!(fifo_ctl(&mut h), 0x5);
    }

    #[test]
    fn no_monitor_is_plugged_in() {
        let mut h = Hdmi::new("hdmi0");
        assert_eq!(h.read(HOTPLUG, Width::Word).unwrap(), 0);
        h.write(HOTPLUG, Width::Word, 0xFFFF_FFFF).unwrap();
        assert_eq!(h.read(HOTPLUG, Width::Word).unwrap(), 0, "read-only");
    }

    #[test]
    fn the_rest_of_the_window_reads_back_what_was_written() {
        let mut h = Hdmi::new("hdmi1");
        h.write(0xE4, Width::Word, 0x0010_0280).unwrap();
        assert_eq!(h.read(0xE4, Width::Word).unwrap(), 0x0010_0280);
    }
}
