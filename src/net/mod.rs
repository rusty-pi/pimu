//! What is on the other end of the Ethernet cable.
//!
//! The GENET model ([`crate::periph::genet`]) moves frames between its DMA
//! rings and a [`NetBackend`]. A backend sees whole Ethernet frames without the
//! FCS — the unit every user-mode network stack exchanges — so a host-facing
//! backend is a thin adapter over one of those. [`BuiltinPeer`] needs nothing
//! outside the emulator; [`StreamBackend`] is the host's network, via passt.

pub mod peer;
pub mod stream;

pub use peer::{BuiltinPeer, PeerRoot};
pub use stream::StreamBackend;

pub trait NetBackend {
    fn name(&self) -> &'static str;

    fn send(&mut self, frame: &[u8]);

    fn recv(&mut self) -> Option<Vec<u8>>;

    fn take_log(&mut self) -> Vec<String> {
        Vec::new()
    }
}

pub type Mac = [u8; 6];

pub const BROADCAST: Mac = [0xff; 6];
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const IPPROTO_ICMP: u8 = 1;
pub const IPPROTO_TCP: u8 = 6;
pub const IPPROTO_UDP: u8 = 17;

/// The one's-complement sum of `data` as big-endian words, not yet inverted.
fn sum16(data: &[u8], mut acc: u32) -> u32 {
    let (words, rest) = data.as_chunks::<2>();
    for w in words {
        acc += u32::from(u16::from_be_bytes(*w));
    }
    if let [b] = rest {
        acc += u32::from(*b) << 8;
    }
    acc
}

fn fold(mut acc: u32) -> u16 {
    while acc > 0xffff {
        acc = (acc & 0xffff) + (acc >> 16);
    }
    !(acc as u16)
}

pub fn checksum(data: &[u8]) -> u16 {
    fold(sum16(data, 0))
}

/// The TCP or UDP checksum of `segment`, over the IPv4 pseudo-header too.
pub fn transport_checksum(src: [u8; 4], dst: [u8; 4], proto: u8, segment: &[u8]) -> u16 {
    let mut pseudo = [0u8; 12];
    pseudo[..4].copy_from_slice(&src);
    pseudo[4..8].copy_from_slice(&dst);
    pseudo[9] = proto;
    pseudo[10..12].copy_from_slice(&(segment.len() as u16).to_be_bytes());
    fold(sum16(segment, sum16(&pseudo, 0)))
}

pub fn udp_checksum(src: [u8; 4], dst: [u8; 4], udp: &[u8]) -> u16 {
    match transport_checksum(src, dst, IPPROTO_UDP, udp) {
        0 => 0xffff,
        c => c,
    }
}

pub fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

pub fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

pub fn fmt_mac(m: &[u8]) -> String {
    m.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_rfc1071_example() {
        let data = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        assert_eq!(checksum(&data), !0xddf2);
    }

    #[test]
    fn a_header_with_its_checksum_sums_to_zero() {
        let mut ip = [
            0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8,
            0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];
        let c = checksum(&ip);
        ip[10..12].copy_from_slice(&c.to_be_bytes());
        assert_eq!(c, 0xb861);
        assert_eq!(checksum(&ip), 0);
    }
}
