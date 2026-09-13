//! BCM2711 HDMI controller core registers — the `hdmi` window of each
//! connector (`0x7EF0_0700`, `0x7EF0_5700`).
//!
//! Only the packet-RAM handshake is modelled. The controller sends the
//! packets in its packet RAM (infoframes, the general control packet) with
//! every frame, one enable bit per slot in `RAM_PACKET_CONFIG`, and
//! `RAM_PACKET_STATUS` reports which slots it is sending. Rewriting a packet
//! means turning its slot off, waiting for the status to follow, writing it,
//! turning it back on and waiting again. start4 does that with no timeout
//! when it stops its display (`0x3ECDFD68`, the AV-mute general control
//! packet). On the catch-all stub the status never moved, so the mailbox
//! thread serving `NOTIFY_DISPLAY_DONE` spun there for good (#61). Linux's vc4
//! does the same with a 100 ms timeout.
//!
//! Model: `RAM_PACKET_STATUS` follows the enable bits at once. Everything
//! else in the window is plain sticky storage.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::spec::hdmi::{RAM_PACKET_CONFIG, RAM_PACKET_CONFIG_PACKETS_MASK, RAM_PACKET_STATUS};
use crate::spec::Coverage;

/// The packet-RAM handshake is modelled, on both connectors; the rest of the
/// window is storage.
pub const COVERAGE: Coverage = Coverage {
    block: "hdmi",
    decoded: &[RAM_PACKET_CONFIG, RAM_PACKET_STATUS],
};

pub struct Hdmi {
    name: &'static str,
    storage: BTreeMap<u32, u32>,
}

impl Hdmi {
    pub fn new(name: &'static str) -> Hdmi {
        Hdmi {
            name,
            storage: BTreeMap::new(),
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
            // The encoder takes a slot on or off as soon as it is told to.
            RAM_PACKET_STATUS => self.reg(RAM_PACKET_CONFIG) & RAM_PACKET_CONFIG_PACKETS_MASK,
            _ => self.reg(off),
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        if off != RAM_PACKET_STATUS {
            self.storage.insert(off, value);
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
    fn the_rest_of_the_window_reads_back_what_was_written() {
        let mut h = Hdmi::new("hdmi1");
        h.write(0x74, Width::Word, 0x1234).unwrap();
        assert_eq!(h.read(0x74, Width::Word).unwrap(), 0x1234);
    }
}
