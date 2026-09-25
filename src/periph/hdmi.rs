//! BCM2711 HDMI controller core registers — the `hdmi` window of each
//! connector (`0x7EF0_0700`, `0x7EF0_5700`) — with no monitor attached.
//!
//! Registers and fields: `specs/hdmi.toml` ([`crate::spec::hdmi`]).
//!
//! No encoder or PHY stands behind the window. What is modelled is what
//! firmware waits on; the rest of the window is plain sticky storage.
//!
//! **Packet RAM.** Rewriting a packet means turning its slot off in
//! `RAM_PACKET_CONFIG`, waiting for `RAM_PACKET_STATUS` to follow, writing it,
//! turning it back on and waiting again — and start4 does that with no timeout
//! when it stops its display, so a status that never moves parks the mailbox
//! thread serving `NOTIFY_DISPLAY_DONE` for good. So `RAM_PACKET_STATUS`
//! follows the enable bits at once.
//!
//! **FIFO recenter.** Software pulses `FIFO_CTL.RECENTER` and waits for
//! `RECENTER_DONE`; the 2020-era bootcode (`pieeprom-2020-09-03`) does it on
//! every boot with no timeout, and reads neither `HOTPLUG` nor the DDC bus
//! first, so the recenter cannot need a sink. Any write with `RECENTER` set
//! finishes at once and leaves `RECENTER_DONE` set.
//!
//! **Hotplug.** Neither connector has a monitor by default, so `HOTPLUG` reads
//! 0 and `CONNECTED` is clear; the DDC side of that is [`super::hdmi_ddc`].

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::spec::hdmi::{
    FIFO_CTL, FIFO_CTL_RECENTER_DONE_MASK, FIFO_CTL_RECENTER_MASK, HOTPLUG, HOTPLUG_CONNECTED_MASK,
    RAM_PACKET_CONFIG, RAM_PACKET_CONFIG_PACKETS_MASK, RAM_PACKET_STATUS,
};
use crate::spec::Coverage;

pub const COVERAGE: Coverage = Coverage {
    block: "hdmi",
    decoded: &[FIFO_CTL, RAM_PACKET_CONFIG, RAM_PACKET_STATUS, HOTPLUG],
};

pub struct Hdmi {
    name: &'static str,
    storage: BTreeMap<u32, u32>,
    recenter_done: bool,
    /// Whether a monitor is on this connector; `boot --display` sets it.
    connected: bool,
}

impl Hdmi {
    pub fn new(name: &'static str) -> Hdmi {
        Hdmi {
            name,
            storage: BTreeMap::new(),
            recenter_done: false,
            connected: false,
        }
    }

    /// Put a monitor on this connector: `HOTPLUG` then answers `CONNECTED`.
    /// Note this is the *register*, which is not the same lever as a board's
    /// `hdmi_force_hotplug=1`: measured on a Raspberry Pi 4B d03115, stock
    /// firmware with that set still gives up on EDID after one attempt, where a
    /// set `CONNECTED` bit makes it retry ten times.
    pub fn with_display(mut self) -> Hdmi {
        self.connected = true;
        self
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
            RAM_PACKET_STATUS => self.reg(RAM_PACKET_CONFIG) & RAM_PACKET_CONFIG_PACKETS_MASK,
            HOTPLUG if self.connected => HOTPLUG_CONNECTED_MASK,
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

    /// `--display` puts a monitor on the connector: `HOTPLUG` answers
    /// `CONNECTED`, and it stays read-only either way.
    #[test]
    fn hotplug_reports_a_monitor_only_when_one_is_attached() {
        let mut bare = Hdmi::new("hdmi0");
        assert_eq!(bare.read(HOTPLUG, Width::Word).unwrap(), 0);

        let mut plugged = Hdmi::new("hdmi0").with_display();
        assert_eq!(
            plugged.read(HOTPLUG, Width::Word).unwrap(),
            HOTPLUG_CONNECTED_MASK
        );
        plugged.write(HOTPLUG, Width::Word, 0).unwrap();
        bare.write(HOTPLUG, Width::Word, 0xFFFF_FFFF).unwrap();
        assert_eq!(
            plugged.read(HOTPLUG, Width::Word).unwrap(),
            HOTPLUG_CONNECTED_MASK
        );
        assert_eq!(bare.read(HOTPLUG, Width::Word).unwrap(), 0);
    }

    #[test]
    fn a_packets_status_follows_its_enable() {
        // start4's AV-mute write: slot 0 off, wait, write the packet, slot 0
        // on, wait.
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
