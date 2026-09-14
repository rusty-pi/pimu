//! A deterministic network-boot server inside the emulator.
//!
//! [`BuiltinPeer`] plays everything the Raspberry Pi network boot needs on
//! the other end of the cable, as one host on a `192.0.2.0/24` segment
//! (TEST-NET-1, RFC 5737):
//!
//! * **ARP** for its own address;
//! * **ICMP echo** replies;
//! * **DHCP** (RFC 2131), handing the one client `192.0.2.100` and naming
//!   itself as the TFTP server. For a client that identifies as a PXE client
//!   (option 60 `PXEClient...`) it answers with option 43 carrying the PXE
//!   boot menu entry `Raspberry Pi Boot`: the bootloader does not TFTP boot
//!   without it. The bytes are what `dnsmasq` sends for
//!   `pxe-service=0,"Raspberry Pi Boot"`, the configuration Raspberry Pi's
//!   network boot documentation gives;
//! * **DNS** (RFC 1035): every name resolves to the peer itself, so the
//!   bootloader's default `HTTP_HOST` (`fw-download-alias1.raspberrypi.com`)
//!   lands here without an EEPROM change;
//! * **TFTP** (RFC 1350) read requests, with the `blksize` (RFC 2348),
//!   `tsize` and `timeout` (RFC 2349) options;
//! * **HTTP/1.1** `GET` and `HEAD` on port 80 (plain HTTP, no TLS), with
//!   single `Range: bytes=` requests, over a minimal **TCP** (RFC 9293): one
//!   request per connection, answered with `Connection: close`.
//!
//! TFTP and HTTP serve the same files: a directory and/or files registered in
//! memory.
//!
//! Every reply is produced synchronously, while the request is handed over,
//! and nothing depends on time: the same guest traffic always gets the same
//! frames back. No loss is modelled, so there is no retransmission either: a
//! duplicate TFTP ACK is ignored rather than answered, and TCP never times
//! out. TCP sends as much as the client's window allows and no more, so a
//! slow guest paces the transfer.
//!
//! Limits: no IP options or fragments, no TCP window scaling, SACK or urgent
//! data; out-of-order TCP data is dropped and re-ACKed (it cannot happen on
//! this lossless link).

use std::collections::{BTreeMap, VecDeque};
use std::path::{Component, Path, PathBuf};

use super::*;

pub const SERVER_MAC: Mac = [0x02, 0x00, 0x5e, 0x00, 0x53, 0x02];
pub const SERVER_IP: [u8; 4] = [192, 0, 2, 1];
pub const CLIENT_IP: [u8; 4] = [192, 0, 2, 100];
pub const NETMASK: [u8; 4] = [255, 255, 255, 0];
const SUBNET_BROADCAST: [u8; 4] = [192, 0, 2, 255];
const LIMITED_BROADCAST: [u8; 4] = [255; 4];
const LEASE_SECS: u32 = 86_400;

const DNS_PORT: u16 = 53;
const DNS_TYPE_A: u16 = 1;
const DNS_CLASS_IN: u16 = 1;
const DNS_TTL_SECS: u32 = 60;

const DHCP_SERVER_PORT: u16 = 67;
const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_MAGIC: [u8; 4] = [0x63, 0x82, 0x53, 0x63];
const DHCPDISCOVER: u8 = 1;
const DHCPOFFER: u8 = 2;
const DHCPREQUEST: u8 = 3;
const DHCPACK: u8 = 5;
const DHCP_BROADCAST_FLAG: u16 = 0x8000;

/// Option 43 as `dnsmasq` builds it for `pxe-service=0,"Raspberry Pi Boot"`:
/// discovery control 3 (sub-option 6), an empty menu prompt with timeout 0
/// ("PXE", sub-option 10), and one boot menu item of type 0 named
/// `Raspberry Pi Boot` (sub-option 9).
const PXE_VENDOR_OPTIONS: &[u8] =
    b"\x06\x01\x03\x0a\x04\x00PXE\x09\x14\x00\x00\x11Raspberry Pi Boot\xff";

const TFTP_PORT: u16 = 69;
const TFTP_RRQ: u16 = 1;
const TFTP_WRQ: u16 = 2;
const TFTP_DATA: u16 = 3;
const TFTP_ACK: u16 = 4;
const TFTP_ERROR: u16 = 5;
const TFTP_OACK: u16 = 6;
const TFTP_DEFAULT_BLKSIZE: usize = 512;
/// The largest block that fits a 1500-byte MTU: 1500 - 20 (IPv4) - 8 (UDP)
/// - 4 (TFTP header).
const TFTP_MAX_BLKSIZE: usize = 1468;
/// Server transfer ports (TIDs) are handed out from here up.
const TFTP_FIRST_TID: u16 = 49152;

const HTTP_PORT: u16 = 80;
const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;
/// The segment size a 1500-byte MTU leaves room for, and what the SYN-ACK
/// advertises.
const TCP_MSS: usize = 1460;
/// The receive window the server advertises. Requests are small; this only
/// has to hold one.
const TCP_WINDOW: u16 = 65535;
/// Initial sequence numbers: fixed, one 16 MiB stride per connection, so runs
/// are reproducible and connections do not share sequence space.
const TCP_ISS_BASE: u32 = 0x5250_0000;
/// Largest request head accepted before the connection is reset.
const HTTP_MAX_REQUEST: usize = 16 * 1024;

/// The fields of a TCP header the peer sets.
#[derive(Clone, Copy)]
struct TcpHeader {
    sport: u16,
    dport: u16,
    seq: u32,
    ack: u32,
    flags: u8,
}

/// A connection, from the server's side.
struct TcpConn {
    client_mac: Mac,
    client_ip: [u8; 4],
    client_port: u16,
    server_port: u16,
    mss: usize,
    /// First sequence number of the response stream (ISS + 1).
    base: u32,
    snd_una: u32,
    snd_nxt: u32,
    snd_wnd: u32,
    rcv_nxt: u32,
    request: Vec<u8>,
    /// The whole response, once the request is complete.
    response: Option<Vec<u8>>,
    fin_sent: bool,
    fin_received: bool,
}

/// Wrapping sequence-number comparison: `a` is after `b`.
fn seq_gt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

/// One TFTP read in progress.
struct Transfer {
    name: String,
    client_mac: Mac,
    client_ip: [u8; 4],
    client_port: u16,
    data: Vec<u8>,
    blksize: usize,
    /// The block last sent; 0 while an OACK waits for its ACK.
    block: usize,
}

impl Transfer {
    /// Blocks in the file, counting the short (possibly empty) last one.
    fn blocks(&self) -> usize {
        self.data.len() / self.blksize + 1
    }

    fn block_data(&self, block: usize) -> &[u8] {
        let start = ((block - 1) * self.blksize).min(self.data.len());
        let end = (start + self.blksize).min(self.data.len());
        &self.data[start..end]
    }
}

pub struct BuiltinPeer {
    root: Option<PathBuf>,
    files: BTreeMap<String, Vec<u8>>,
    out: VecDeque<Vec<u8>>,
    log: Vec<String>,
    transfers: BTreeMap<u16, Transfer>,
    next_tid: u16,
    ip_id: u16,
    /// TCP connections by (client IP, client port, server port).
    conns: BTreeMap<([u8; 4], u16, u16), TcpConn>,
    conn_count: u32,
    /// Where each [`Self::note`] also goes, for `boot --io-log` (#35).
    io: Option<crate::iolog::IoLogRef>,
}

impl Default for BuiltinPeer {
    fn default() -> Self {
        BuiltinPeer::new()
    }
}

impl BuiltinPeer {
    /// A server with nothing to serve.
    pub fn new() -> BuiltinPeer {
        BuiltinPeer {
            root: None,
            files: BTreeMap::new(),
            out: VecDeque::new(),
            log: Vec::new(),
            transfers: BTreeMap::new(),
            next_tid: TFTP_FIRST_TID,
            ip_id: 0,
            conns: BTreeMap::new(),
            conn_count: 0,
            io: None,
        }
    }

    /// A server whose TFTP root is `dir`.
    pub fn with_root(dir: impl Into<PathBuf>) -> BuiltinPeer {
        BuiltinPeer {
            root: Some(dir.into()),
            ..BuiltinPeer::new()
        }
    }

    /// Also write what the peer does to the I/O log.
    pub fn with_io(mut self, io: crate::iolog::IoLogRef) -> BuiltinPeer {
        self.io = Some(io);
        self
    }

    /// Record one thing the peer did, for the run report and the I/O log.
    fn note(&mut self, what: impl Into<String>) {
        let what = what.into();
        if let Some(io) = &self.io {
            io.borrow_mut().net(&what);
        }
        self.log.push(what);
    }

    /// Serve `data` as `name` (a path relative to the TFTP root, `/`
    /// separated). Takes precedence over a file of that name under the root.
    pub fn add_file(&mut self, name: &str, data: Vec<u8>) {
        self.files
            .insert(name.trim_start_matches('/').to_string(), data);
    }

    fn lookup(&self, name: &str) -> Option<Vec<u8>> {
        let name = name.trim_start_matches('/');
        if let Some(d) = self.files.get(name) {
            return Some(d.clone());
        }
        let rel = Path::new(name);
        // Nothing outside the root: no `..`, no absolute paths.
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return None;
        }
        let path = self.root.as_ref()?.join(rel);
        path.is_file().then(|| std::fs::read(path).ok()).flatten()
    }

    fn eth(&mut self, dst: Mac, ethertype: u16, payload: &[u8]) {
        let mut f = Vec::with_capacity(14 + payload.len());
        f.extend_from_slice(&dst);
        f.extend_from_slice(&SERVER_MAC);
        f.extend_from_slice(&ethertype.to_be_bytes());
        f.extend_from_slice(payload);
        self.out.push_back(f);
    }

    fn ipv4(&mut self, dst_mac: Mac, dst: [u8; 4], proto: u8, payload: &[u8]) {
        let mut p = vec![0u8; 20];
        p[0] = 0x45;
        p[2..4].copy_from_slice(&((20 + payload.len()) as u16).to_be_bytes());
        p[4..6].copy_from_slice(&self.ip_id.to_be_bytes());
        self.ip_id = self.ip_id.wrapping_add(1);
        p[6] = 0x40; // don't fragment
        p[8] = 64;
        p[9] = proto;
        p[12..16].copy_from_slice(&SERVER_IP);
        p[16..20].copy_from_slice(&dst);
        let c = checksum(&p);
        p[10..12].copy_from_slice(&c.to_be_bytes());
        p.extend_from_slice(payload);
        self.eth(dst_mac, ETHERTYPE_IPV4, &p);
    }

    fn udp(&mut self, dst_mac: Mac, dst: [u8; 4], sport: u16, dport: u16, payload: &[u8]) {
        let mut u = Vec::with_capacity(8 + payload.len());
        u.extend_from_slice(&sport.to_be_bytes());
        u.extend_from_slice(&dport.to_be_bytes());
        u.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        u.extend_from_slice(&[0, 0]);
        u.extend_from_slice(payload);
        let c = udp_checksum(SERVER_IP, dst, &u);
        u[6..8].copy_from_slice(&c.to_be_bytes());
        self.ipv4(dst_mac, dst, IPPROTO_UDP, &u);
    }

    fn arp(&mut self, a: &[u8]) {
        // Ethernet / IPv4 requests for our address only.
        if a.len() < 28 || be16(a, 0) != 1 || be16(a, 2) != ETHERTYPE_IPV4 || be16(a, 6) != 1 {
            return;
        }
        if a[24..28] != SERVER_IP {
            return;
        }
        let sha: Mac = a[8..14].try_into().unwrap();
        let mut r = a[..28].to_vec();
        r[6..8].copy_from_slice(&2u16.to_be_bytes());
        r[8..14].copy_from_slice(&SERVER_MAC);
        r[14..18].copy_from_slice(&SERVER_IP);
        r[18..28].copy_from_slice(&a[8..18]);
        self.eth(sha, ETHERTYPE_ARP, &r);
    }

    fn ip(&mut self, src_mac: Mac, p: &[u8]) {
        if p.len() < 20 || p[0] >> 4 != 4 {
            return;
        }
        let ihl = usize::from(p[0] & 0xf) * 4;
        let total = usize::from(be16(p, 2)).min(p.len());
        // Fragments are not reassembled; nothing the boot sends is fragmented.
        if ihl < 20 || total < ihl || be16(p, 6) & 0x3fff != 0 {
            return;
        }
        let src: [u8; 4] = p[12..16].try_into().unwrap();
        let dst: [u8; 4] = p[16..20].try_into().unwrap();
        if ![SERVER_IP, SUBNET_BROADCAST, LIMITED_BROADCAST].contains(&dst) {
            return;
        }
        let body = &p[ihl..total];
        match p[9] {
            IPPROTO_ICMP if dst == SERVER_IP => self.icmp(src_mac, src, body),
            IPPROTO_TCP if dst == SERVER_IP && body.len() >= 20 => self.tcp(src_mac, src, body),
            IPPROTO_UDP if body.len() >= 8 => {
                let len = usize::from(be16(body, 4)).clamp(8, body.len());
                let (sport, dport) = (be16(body, 0), be16(body, 2));
                let data = &body[8..len];
                match dport {
                    DHCP_SERVER_PORT => self.dhcp(data),
                    DNS_PORT if dst == SERVER_IP => self.dns(src_mac, src, sport, data),
                    TFTP_PORT if dst == SERVER_IP => self.tftp_request(src_mac, src, sport, data),
                    tid if dst == SERVER_IP && self.transfers.contains_key(&tid) => {
                        self.tftp_session(tid, src, sport, data)
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn icmp(&mut self, src_mac: Mac, src: [u8; 4], m: &[u8]) {
        // Echo request -> echo reply, same identifier, sequence and data.
        if m.len() < 8 || m[0] != 8 {
            return;
        }
        let mut r = m.to_vec();
        r[0] = 0;
        r[2..4].copy_from_slice(&[0, 0]);
        let c = checksum(&r);
        r[2..4].copy_from_slice(&c.to_be_bytes());
        self.ipv4(src_mac, src, IPPROTO_ICMP, &r);
    }

    fn dhcp(&mut self, b: &[u8]) {
        if b.len() < 240 || b[0] != 1 || b[1] != 1 || b[2] != 6 || b[236..240] != DHCP_MAGIC {
            return;
        }
        let mut options: BTreeMap<u8, &[u8]> = BTreeMap::new();
        let mut i = 240;
        while i < b.len() {
            match b[i] {
                0 => i += 1,
                255 => break,
                code => {
                    let Some(&len) = b.get(i + 1) else { break };
                    let end = (i + 2 + usize::from(len)).min(b.len());
                    options.insert(code, &b[i + 2..end]);
                    i = end;
                }
            }
        }
        let chaddr: Mac = b[28..34].try_into().unwrap();
        let reply = match options.get(&53).and_then(|t| t.first()) {
            Some(&DHCPDISCOVER) => DHCPOFFER,
            Some(&DHCPREQUEST) => DHCPACK,
            _ => return,
        };
        let pxe = options
            .get(&60)
            .is_some_and(|v| v.starts_with(b"PXEClient"));
        self.note(format!(
            "dhcp: {} from {}{} -> {} {}",
            if reply == DHCPOFFER {
                "DISCOVER"
            } else {
                "REQUEST"
            },
            fmt_mac(&chaddr),
            if pxe { " (PXEClient)" } else { "" },
            if reply == DHCPOFFER { "OFFER" } else { "ACK" },
            fmt_ip(CLIENT_IP),
        ));

        let flags = be16(b, 10);
        let mut r = vec![0u8; 236];
        r[0] = 2;
        r[1] = 1;
        r[2] = 6;
        r[4..8].copy_from_slice(&b[4..8]); // xid
        r[10..12].copy_from_slice(&flags.to_be_bytes());
        r[16..20].copy_from_slice(&CLIENT_IP); // yiaddr
        r[20..24].copy_from_slice(&SERVER_IP); // siaddr: next server
        r[28..44].copy_from_slice(&b[28..44]); // chaddr
        r.extend_from_slice(&DHCP_MAGIC);
        let mut opt = |code: u8, v: &[u8]| {
            r.push(code);
            r.push(v.len() as u8);
            r.extend_from_slice(v);
        };
        opt(53, &[reply]);
        opt(54, &SERVER_IP);
        opt(51, &LEASE_SECS.to_be_bytes());
        opt(1, &NETMASK);
        opt(3, &SERVER_IP);
        opt(6, &SERVER_IP);
        opt(66, fmt_ip(SERVER_IP).as_bytes());
        if pxe {
            opt(60, b"PXEClient");
            opt(43, PXE_VENDOR_OPTIONS);
        }
        r.push(255);

        let (mac, ip) = if flags & DHCP_BROADCAST_FLAG != 0 {
            (BROADCAST, LIMITED_BROADCAST)
        } else {
            (chaddr, CLIENT_IP)
        };
        self.udp(mac, ip, DHCP_SERVER_PORT, DHCP_CLIENT_PORT, &r);
    }

    fn dns(&mut self, mac: Mac, ip: [u8; 4], port: u16, q: &[u8]) {
        // A standard query (QR = 0, opcode 0) with one question.
        if q.len() < 12 || q[2] & 0xf8 != 0 || be16(q, 4) != 1 {
            return;
        }
        let mut i = 12;
        let mut labels = Vec::new();
        while let Some(&n) = q.get(i) {
            if n == 0 || n & 0xc0 != 0 {
                break;
            }
            let end = i + 1 + usize::from(n);
            let Some(label) = q.get(i + 1..end) else {
                return;
            };
            labels.push(String::from_utf8_lossy(label).into_owned());
            i = end;
        }
        if q.get(i) != Some(&0) || q.len() < i + 5 {
            return;
        }
        let qend = i + 5;
        let (qtype, qclass) = (be16(q, i + 1), be16(q, i + 3));
        let name = labels.join(".");
        let answer = qtype == DNS_TYPE_A && qclass == DNS_CLASS_IN;
        self.note(format!(
            "dns: {name} type {qtype} -> {}",
            if answer {
                fmt_ip(SERVER_IP)
            } else {
                "no records".into()
            }
        ));
        let mut r = Vec::with_capacity(qend + 16);
        r.extend_from_slice(&q[..2]); // id
                                      // QR, the query's RD copied, RA; NOERROR.
        r.extend_from_slice(&[0x80 | (q[2] & 0x01), 0x80]);
        r.extend_from_slice(&1u16.to_be_bytes());
        r.extend_from_slice(&u16::from(answer).to_be_bytes());
        r.extend_from_slice(&[0, 0, 0, 0]);
        r.extend_from_slice(&q[12..qend]);
        if answer {
            r.extend_from_slice(&[0xc0, 12]); // the name in the question
            r.extend_from_slice(&DNS_TYPE_A.to_be_bytes());
            r.extend_from_slice(&DNS_CLASS_IN.to_be_bytes());
            r.extend_from_slice(&DNS_TTL_SECS.to_be_bytes());
            r.extend_from_slice(&4u16.to_be_bytes());
            r.extend_from_slice(&SERVER_IP);
        }
        self.udp(mac, ip, DNS_PORT, port, &r);
    }

    fn tcp_segment(&mut self, mac: Mac, ip: [u8; 4], h: TcpHeader, options: &[u8], data: &[u8]) {
        let hlen = 20 + options.len();
        let mut t = Vec::with_capacity(hlen + data.len());
        t.extend_from_slice(&h.sport.to_be_bytes());
        t.extend_from_slice(&h.dport.to_be_bytes());
        t.extend_from_slice(&h.seq.to_be_bytes());
        t.extend_from_slice(&h.ack.to_be_bytes());
        t.push(((hlen / 4) as u8) << 4);
        t.push(h.flags);
        t.extend_from_slice(&TCP_WINDOW.to_be_bytes());
        t.extend_from_slice(&[0, 0, 0, 0]); // checksum, urgent pointer
        t.extend_from_slice(options);
        t.extend_from_slice(data);
        let c = transport_checksum(SERVER_IP, ip, IPPROTO_TCP, &t);
        t[16..18].copy_from_slice(&c.to_be_bytes());
        self.ipv4(mac, ip, IPPROTO_TCP, &t);
    }

    /// Send a segment on `key`'s connection, from its current state.
    fn tcp_send(&mut self, key: ([u8; 4], u16, u16), flags: u8, data_from: Option<(usize, usize)>) {
        let c = &self.conns[&key];
        let (mac, ip, cport, sport, seq, ack) = (
            c.client_mac,
            c.client_ip,
            c.client_port,
            c.server_port,
            c.snd_nxt,
            c.rcv_nxt,
        );
        let data = match (data_from, &c.response) {
            (Some((from, to)), Some(r)) => r[from..to].to_vec(),
            _ => Vec::new(),
        };
        self.tcp_segment(
            mac,
            ip,
            TcpHeader {
                sport,
                dport: cport,
                seq,
                ack,
                flags,
            },
            &[],
            &data,
        );
    }

    /// Send as much of the response as the client's window takes, then FIN
    /// once all of it is out.
    fn tcp_pump(&mut self, key: ([u8; 4], u16, u16)) {
        loop {
            let c = &self.conns[&key];
            let Some(resp) = &c.response else { return };
            let sent = c.snd_nxt.wrapping_sub(c.base) as usize - usize::from(c.fin_sent);
            let in_flight = c.snd_nxt.wrapping_sub(c.snd_una);
            if sent < resp.len() {
                let room = c.snd_wnd.saturating_sub(in_flight) as usize;
                let n = c.mss.min(resp.len() - sent).min(room);
                if n == 0 {
                    return;
                }
                self.tcp_send(key, TCP_ACK | TCP_PSH, Some((sent, sent + n)));
                self.conns.get_mut(&key).unwrap().snd_nxt += n as u32;
            } else {
                if !c.fin_sent {
                    self.tcp_send(key, TCP_FIN | TCP_ACK, None);
                    let c = self.conns.get_mut(&key).unwrap();
                    c.snd_nxt += 1;
                    c.fin_sent = true;
                }
                return;
            }
        }
    }

    fn tcp(&mut self, mac: Mac, ip: [u8; 4], t: &[u8]) {
        let (sport, dport) = (be16(t, 0), be16(t, 2));
        let (seq, ack) = (be32(t, 4), be32(t, 8));
        let doff = usize::from(t[12] >> 4) * 4;
        let flags = t[13];
        if doff < 20 || doff > t.len() {
            return;
        }
        let data = &t[doff..];
        let key = (ip, sport, dport);
        if flags & TCP_RST != 0 {
            self.conns.remove(&key);
            return;
        }
        if !self.conns.contains_key(&key) {
            if flags & TCP_SYN == 0 || flags & TCP_ACK != 0 || dport != HTTP_PORT {
                // Nothing listening, or no such connection: reset.
                if flags & TCP_ACK != 0 {
                    self.tcp_segment(
                        mac,
                        ip,
                        TcpHeader {
                            sport: dport,
                            dport: sport,
                            seq: ack,
                            ack: 0,
                            flags: TCP_RST,
                        },
                        &[],
                        &[],
                    );
                } else {
                    let len = data.len() as u32 + u32::from(flags & (TCP_SYN | TCP_FIN) != 0);
                    self.tcp_segment(
                        mac,
                        ip,
                        TcpHeader {
                            sport: dport,
                            dport: sport,
                            seq: 0,
                            ack: seq.wrapping_add(len),
                            flags: TCP_RST | TCP_ACK,
                        },
                        &[],
                        &[],
                    );
                }
                return;
            }
            // Passive open: SYN -> SYN-ACK, advertising our MSS.
            let mut mss = TCP_MSS;
            let mut i = 20;
            while i < doff {
                match t[i] {
                    0 => break,
                    1 => i += 1,
                    kind => {
                        let len = usize::from(*t.get(i + 1).unwrap_or(&0)).max(2);
                        if kind == 2 && len == 4 && i + 4 <= doff {
                            mss = mss.min(usize::from(be16(t, i + 2)));
                        }
                        i += len;
                    }
                }
            }
            let iss = TCP_ISS_BASE.wrapping_add(self.conn_count << 24);
            self.conn_count += 1;
            self.conns.insert(
                key,
                TcpConn {
                    client_mac: mac,
                    client_ip: ip,
                    client_port: sport,
                    server_port: dport,
                    mss: mss.max(64),
                    base: iss.wrapping_add(1),
                    snd_una: iss,
                    snd_nxt: iss,
                    snd_wnd: u32::from(be16(t, 14)),
                    rcv_nxt: seq.wrapping_add(1),
                    request: Vec::new(),
                    response: None,
                    fin_sent: false,
                    fin_received: false,
                },
            );
            let mut opt = vec![2, 4];
            opt.extend_from_slice(&(TCP_MSS as u16).to_be_bytes());
            self.tcp_segment(
                mac,
                ip,
                TcpHeader {
                    sport: dport,
                    dport: sport,
                    seq: iss,
                    ack: seq.wrapping_add(1),
                    flags: TCP_SYN | TCP_ACK,
                },
                &opt,
                &[],
            );
            self.conns.get_mut(&key).unwrap().snd_nxt = iss.wrapping_add(1);
            return;
        }

        let c = self.conns.get_mut(&key).unwrap();
        if flags & TCP_ACK != 0 {
            // Acknowledges something we sent (and not more than that).
            if seq_gt(ack, c.snd_una) && !seq_gt(ack, c.snd_nxt) {
                c.snd_una = ack;
            }
            c.snd_wnd = u32::from(be16(t, 14));
        }
        let mut must_ack = false;
        if !data.is_empty() || flags & TCP_FIN != 0 {
            if seq != c.rcv_nxt {
                // Out of order (or a duplicate): re-ACK what we have.
                self.tcp_send(key, TCP_ACK, None);
                return;
            }
            if !data.is_empty() && c.response.is_none() {
                c.request.extend_from_slice(data);
            }
            c.rcv_nxt = c.rcv_nxt.wrapping_add(data.len() as u32);
            if flags & TCP_FIN != 0 {
                c.rcv_nxt = c.rcv_nxt.wrapping_add(1);
                c.fin_received = true;
            }
            must_ack = true;
        }
        if c.response.is_none() {
            if let Some(end) = find(&c.request, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&c.request[..end]).into_owned();
                let resp = self.http_response(&head);
                self.conns.get_mut(&key).unwrap().response = Some(resp);
            } else if c.request.len() > HTTP_MAX_REQUEST || c.fin_received {
                self.log
                    .push("http: incomplete request, connection reset".into());
                self.tcp_send(key, TCP_RST | TCP_ACK, None);
                self.conns.remove(&key);
                return;
            }
        }
        let before = self.out.len();
        self.tcp_pump(key);
        if must_ack && self.out.len() == before {
            self.tcp_send(key, TCP_ACK, None);
        }
        let c = &self.conns[&key];
        // Both FINs exchanged and ours acknowledged: done.
        if c.fin_sent && c.fin_received && c.snd_una == c.snd_nxt {
            self.conns.remove(&key);
        }
    }

    /// The whole response to the request whose head (request line and
    /// headers, without the blank line) is `head`.
    fn http_response(&mut self, head: &str) -> Vec<u8> {
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split(' ');
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let range = lines
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case("range"))
            .map(|(_, v)| v.trim().to_string());
        // An absolute-form target (`http://host/path`) carries the host too.
        let path = match target.split_once("://") {
            Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
            None => target,
        };
        let path = path.split(['?', '#']).next().unwrap_or("");

        let reply = |status: &str, headers: &[String], body: &[u8], head_only: bool| {
            let mut r = format!("HTTP/1.1 {status}\r\n").into_bytes();
            for h in headers {
                r.extend_from_slice(h.as_bytes());
                r.extend_from_slice(b"\r\n");
            }
            r.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
            r.extend_from_slice(b"Connection: close\r\n\r\n");
            if !head_only {
                r.extend_from_slice(body);
            }
            r
        };
        let head_only = method == "HEAD";
        if method != "GET" && !head_only {
            self.note(format!("http: {method} {target} -> 405"));
            return reply(
                "405 Method Not Allowed",
                &["Allow: GET, HEAD".into()],
                b"",
                false,
            );
        }
        let Some(data) = self.lookup(path) else {
            self.note(format!("http: {method} {path} -> 404"));
            return reply("404 Not Found", &[], b"not found\n", head_only);
        };
        let total = data.len();
        let octets = "Content-Type: application/octet-stream".to_string();
        match range.as_deref().map(|r| parse_range(r, total)) {
            None => {
                self.log
                    .push(format!("http: {method} {path} -> 200, {total} bytes"));
                reply("200 OK", &[octets], &data, head_only)
            }
            Some(Some((first, last))) => {
                self.note(format!(
                    "http: {method} {path} bytes {first}-{last}/{total} -> 206"
                ));
                let cr = format!("Content-Range: bytes {first}-{last}/{total}");
                reply(
                    "206 Partial Content",
                    &[octets, cr],
                    &data[first..=last],
                    head_only,
                )
            }
            Some(None) => {
                self.log
                    .push(format!("http: {method} {path} unsatisfiable range -> 416"));
                let cr = format!("Content-Range: bytes */{total}");
                reply("416 Range Not Satisfiable", &[cr], b"", head_only)
            }
        }
    }

    fn tftp_error(&mut self, mac: Mac, ip: [u8; 4], sport: u16, dport: u16, code: u16, msg: &str) {
        let mut p = Vec::new();
        p.extend_from_slice(&TFTP_ERROR.to_be_bytes());
        p.extend_from_slice(&code.to_be_bytes());
        p.extend_from_slice(msg.as_bytes());
        p.push(0);
        self.udp(mac, ip, sport, dport, &p);
    }

    fn tftp_send_block(&mut self, tid: u16) {
        let t = &self.transfers[&tid];
        let mut p = Vec::with_capacity(4 + t.blksize);
        p.extend_from_slice(&TFTP_DATA.to_be_bytes());
        p.extend_from_slice(&(t.block as u16).to_be_bytes());
        p.extend_from_slice(t.block_data(t.block));
        let (mac, ip, port) = (t.client_mac, t.client_ip, t.client_port);
        self.udp(mac, ip, tid, port, &p);
    }

    fn tftp_request(&mut self, mac: Mac, ip: [u8; 4], port: u16, p: &[u8]) {
        if p.len() < 2 {
            return;
        }
        let tid = self.next_tid;
        self.next_tid = self.next_tid.checked_add(1).unwrap_or(TFTP_FIRST_TID);
        let op = be16(p, 0);
        let fields: Vec<String> = p[2..]
            .split(|&b| b == 0)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        if op == TFTP_WRQ {
            self.note("tftp: WRQ refused");
            self.tftp_error(mac, ip, tid, port, 2, "read only");
            return;
        }
        if op != TFTP_RRQ || fields.len() < 2 {
            return;
        }
        let name = fields[0].clone();
        let Some(data) = self.lookup(&name) else {
            self.note(format!("tftp: RRQ {name} -> not found"));
            self.tftp_error(mac, ip, tid, port, 1, "File not found");
            return;
        };
        self.log
            .push(format!("tftp: RRQ {name} -> {} bytes", data.len()));

        // Options are name/value pairs after the mode. Unknown ones are left
        // out of the OACK, which tells the client they were not taken.
        let mut blksize = TFTP_DEFAULT_BLKSIZE;
        let mut oack: Vec<(String, String)> = Vec::new();
        for [k, v] in fields[2..].as_chunks::<2>().0 {
            let k = k.to_ascii_lowercase();
            match k.as_str() {
                "blksize" => {
                    if let Ok(n) = v.parse::<usize>() {
                        blksize = n.clamp(8, TFTP_MAX_BLKSIZE);
                        oack.push((k, blksize.to_string()));
                    }
                }
                "tsize" => oack.push((k, data.len().to_string())),
                "timeout" if v.parse::<u8>().is_ok_and(|t| t >= 1) => oack.push((k, v.clone())),
                _ => {}
            }
        }
        self.transfers.insert(
            tid,
            Transfer {
                name,
                client_mac: mac,
                client_ip: ip,
                client_port: port,
                data,
                blksize,
                block: 0,
            },
        );
        if oack.is_empty() {
            self.transfers.get_mut(&tid).unwrap().block = 1;
            self.tftp_send_block(tid);
        } else {
            let mut r = TFTP_OACK.to_be_bytes().to_vec();
            for (k, v) in oack {
                r.extend_from_slice(k.as_bytes());
                r.push(0);
                r.extend_from_slice(v.as_bytes());
                r.push(0);
            }
            self.udp(mac, ip, tid, port, &r);
        }
    }

    fn tftp_session(&mut self, tid: u16, ip: [u8; 4], port: u16, p: &[u8]) {
        let t = &self.transfers[&tid];
        let mac = t.client_mac;
        if ip != t.client_ip || port != t.client_port {
            self.tftp_error(mac, ip, tid, port, 5, "Unknown transfer ID");
            return;
        }
        if p.len() < 4 {
            return;
        }
        match be16(p, 0) {
            TFTP_ACK if be16(p, 2) == t.block as u16 => {
                if t.block == t.blocks() {
                    let t = self.transfers.remove(&tid).unwrap();
                    self.log
                        .push(format!("tftp: {} sent, {} bytes", t.name, t.data.len()));
                } else {
                    self.transfers.get_mut(&tid).unwrap().block += 1;
                    self.tftp_send_block(tid);
                }
            }
            TFTP_ERROR => {
                let t = self.transfers.remove(&tid).unwrap();
                let msg = String::from_utf8_lossy(&p[4..]);
                self.note(format!(
                    "tftp: {} aborted by client (error {}: {})",
                    t.name,
                    be16(p, 2),
                    msg.trim_end_matches('\0')
                ));
            }
            _ => {}
        }
    }
}

impl NetBackend for BuiltinPeer {
    fn name(&self) -> &'static str {
        "built-in network peer"
    }

    fn send(&mut self, frame: &[u8]) {
        if frame.len() < 14 {
            return;
        }
        let dst = &frame[..6];
        if dst != SERVER_MAC && dst != BROADCAST {
            return;
        }
        let src: Mac = frame[6..12].try_into().unwrap();
        match be16(frame, 12) {
            ETHERTYPE_ARP => self.arp(&frame[14..]),
            ETHERTYPE_IPV4 => self.ip(src, &frame[14..]),
            _ => {}
        }
    }

    fn recv(&mut self) -> Option<Vec<u8>> {
        self.out.pop_front()
    }

    fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// A single `bytes=` range against a `total`-byte body: the inclusive span to
/// send, or `None` when it cannot be satisfied (RFC 9110 section 14.1.2).
fn parse_range(spec: &str, total: usize) -> Option<(usize, usize)> {
    let (first, last) = spec.strip_prefix("bytes=")?.split_once('-')?;
    let (first, last) = (first.trim(), last.trim());
    if total == 0 {
        return None;
    }
    if first.is_empty() {
        // `bytes=-N`: the last N bytes.
        let n: usize = last.parse().ok()?;
        return (n > 0).then(|| (total.saturating_sub(n), total - 1));
    }
    let first: usize = first.parse().ok()?;
    let last = if last.is_empty() {
        total - 1
    } else {
        last.parse::<usize>().ok()?.min(total - 1)
    };
    (first <= last).then_some((first, last))
}

pub fn fmt_ip(ip: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT_MAC: Mac = [0x02, 0x00, 0x5e, 0x00, 0x53, 0x01];

    /// A client-side frame builder: what a guest's stack would put on the
    /// wire.
    fn udp_frame(
        dst_mac: Mac,
        src: [u8; 4],
        dst: [u8; 4],
        sport: u16,
        dport: u16,
        d: &[u8],
    ) -> Vec<u8> {
        let mut u = Vec::new();
        u.extend_from_slice(&sport.to_be_bytes());
        u.extend_from_slice(&dport.to_be_bytes());
        u.extend_from_slice(&((8 + d.len()) as u16).to_be_bytes());
        u.extend_from_slice(&[0, 0]);
        u.extend_from_slice(d);
        let c = udp_checksum(src, dst, &u);
        u[6..8].copy_from_slice(&c.to_be_bytes());
        let mut ip = vec![0x45, 0, 0, 0, 0, 0, 0, 0, 64, IPPROTO_UDP, 0, 0];
        ip[2..4].copy_from_slice(&((20 + u.len()) as u16).to_be_bytes());
        ip.extend_from_slice(&src);
        ip.extend_from_slice(&dst);
        let c = checksum(&ip);
        ip[10..12].copy_from_slice(&c.to_be_bytes());
        ip.extend_from_slice(&u);
        let mut f = dst_mac.to_vec();
        f.extend_from_slice(&CLIENT_MAC);
        f.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
        f.extend_from_slice(&ip);
        f
    }

    /// Check a frame from the peer and return its UDP (source port,
    /// destination port, payload), verifying both checksums.
    fn parse_udp(f: &[u8]) -> (Mac, [u8; 4], u16, u16, Vec<u8>) {
        assert_eq!(f[6..12], SERVER_MAC);
        assert_eq!(be16(f, 12), ETHERTYPE_IPV4);
        let ip = &f[14..];
        assert_eq!(checksum(&ip[..20]), 0, "IPv4 header checksum");
        assert_eq!(ip[9], IPPROTO_UDP);
        let dst: [u8; 4] = ip[16..20].try_into().unwrap();
        let u = &ip[20..usize::from(be16(ip, 2))];
        let mut z = u.to_vec();
        z[6..8].copy_from_slice(&[0, 0]);
        assert_eq!(udp_checksum(SERVER_IP, dst, &z), be16(u, 6), "UDP checksum");
        (
            f[..6].try_into().unwrap(),
            dst,
            be16(u, 0),
            be16(u, 2),
            u[8..].to_vec(),
        )
    }

    fn dhcp_msg(kind: u8, flags: u16) -> Vec<u8> {
        let mut b = vec![0u8; 236];
        b[0] = 1;
        b[1] = 1;
        b[2] = 6;
        b[4..8].copy_from_slice(&0x1234_5678u32.to_be_bytes());
        b[10..12].copy_from_slice(&flags.to_be_bytes());
        b[28..34].copy_from_slice(&CLIENT_MAC);
        b.extend_from_slice(&DHCP_MAGIC);
        b.extend_from_slice(&[53, 1, kind]);
        b.extend_from_slice(&[60, 32]);
        b.extend_from_slice(b"PXEClient:Arch:00000:UNDI:002001");
        b.push(255);
        b
    }

    fn options(b: &[u8]) -> BTreeMap<u8, Vec<u8>> {
        let mut m = BTreeMap::new();
        let mut i = 240;
        while b[i] != 255 {
            m.insert(b[i], b[i + 2..i + 2 + usize::from(b[i + 1])].to_vec());
            i += 2 + usize::from(b[i + 1]);
        }
        m
    }

    #[test]
    fn answers_arp_for_its_own_address_only() {
        let mut p = BuiltinPeer::new();
        let mut arp = vec![0, 1, 8, 0, 6, 4, 0, 1];
        arp.extend_from_slice(&CLIENT_MAC);
        arp.extend_from_slice(&CLIENT_IP);
        arp.extend_from_slice(&[0; 6]);
        arp.extend_from_slice(&SERVER_IP);
        let mut f = BROADCAST.to_vec();
        f.extend_from_slice(&CLIENT_MAC);
        f.extend_from_slice(&ETHERTYPE_ARP.to_be_bytes());
        f.extend_from_slice(&arp);
        p.send(&f);
        let r = p.recv().unwrap();
        assert_eq!(r[..6], CLIENT_MAC);
        assert_eq!(be16(&r, 14 + 6), 2, "reply");
        assert_eq!(r[14 + 8..14 + 14], SERVER_MAC);
        assert_eq!(r[14 + 14..14 + 18], SERVER_IP);
        assert_eq!(r[14 + 24..14 + 28], CLIENT_IP);

        f[14 + 24..14 + 28].copy_from_slice(&[192, 0, 2, 7]);
        p.send(&f);
        assert!(p.recv().is_none(), "someone else's address");
    }

    #[test]
    fn dhcp_discover_and_request_get_offer_and_ack_with_the_pxe_menu() {
        let mut p = BuiltinPeer::new();
        for (kind, want) in [(DHCPDISCOVER, DHCPOFFER), (DHCPREQUEST, DHCPACK)] {
            let f = udp_frame(
                BROADCAST,
                [0; 4],
                LIMITED_BROADCAST,
                68,
                67,
                &dhcp_msg(kind, 0),
            );
            p.send(&f);
            let (mac, ip, sport, dport, b) = parse_udp(&p.recv().unwrap());
            assert_eq!((mac, ip, sport, dport), (CLIENT_MAC, CLIENT_IP, 67, 68));
            assert_eq!(b[0], 2);
            assert_eq!(be32(&b, 4), 0x1234_5678, "xid echoed");
            assert_eq!(b[16..20], CLIENT_IP);
            assert_eq!(b[20..24], SERVER_IP);
            assert_eq!(b[28..34], CLIENT_MAC);
            let o = options(&b);
            assert_eq!(o[&53], [want]);
            assert_eq!(o[&54], SERVER_IP);
            assert_eq!(o[&1], NETMASK);
            assert_eq!(o[&66], b"192.0.2.1");
            assert_eq!(o[&60], b"PXEClient");
            let menu = &o[&43];
            assert!(menu.windows(17).any(|w| w == b"Raspberry Pi Boot"));
            assert_eq!(menu.len(), 32);
        }
        assert_eq!(p.take_log().len(), 2);
    }

    #[test]
    fn dhcp_honours_the_broadcast_flag() {
        let mut p = BuiltinPeer::new();
        let msg = dhcp_msg(DHCPDISCOVER, DHCP_BROADCAST_FLAG);
        p.send(&udp_frame(
            BROADCAST,
            [0; 4],
            LIMITED_BROADCAST,
            68,
            67,
            &msg,
        ));
        let (mac, ip, ..) = parse_udp(&p.recv().unwrap());
        assert_eq!((mac, ip), (BROADCAST, LIMITED_BROADCAST));
    }

    fn rrq(name: &str, opts: &[(&str, &str)]) -> Vec<u8> {
        let mut r = TFTP_RRQ.to_be_bytes().to_vec();
        for s in [name, "octet"]
            .into_iter()
            .chain(opts.iter().flat_map(|(k, v)| [*k, *v]))
        {
            r.extend_from_slice(s.as_bytes());
            r.push(0);
        }
        r
    }

    fn to_server(sport: u16, dport: u16, d: &[u8]) -> Vec<u8> {
        udp_frame(SERVER_MAC, CLIENT_IP, SERVER_IP, sport, dport, d)
    }

    fn ack(block: u16) -> Vec<u8> {
        let mut a = TFTP_ACK.to_be_bytes().to_vec();
        a.extend_from_slice(&block.to_be_bytes());
        a
    }

    /// Read `name` to the end in `blksize` blocks, ACKing every one; return
    /// the data and the block sizes.
    fn fetch(
        p: &mut BuiltinPeer,
        name: &str,
        opts: &[(&str, &str)],
        blksize: usize,
    ) -> (Vec<u8>, Vec<usize>) {
        p.send(&to_server(1000, 69, &rrq(name, opts)));
        let mut data = Vec::new();
        let mut sizes = Vec::new();
        let mut expect = 1u16;
        loop {
            let (_, _, tid, dport, d) = parse_udp(&p.recv().expect("a reply"));
            assert_eq!(dport, 1000);
            assert_ne!(tid, 69, "transfers run on their own port");
            match be16(&d, 0) {
                TFTP_OACK => p.send(&to_server(1000, tid, &ack(0))),
                TFTP_DATA => {
                    assert_eq!(be16(&d, 2), expect);
                    data.extend_from_slice(&d[4..]);
                    sizes.push(d.len() - 4);
                    p.send(&to_server(1000, tid, &ack(expect)));
                    expect = expect.wrapping_add(1);
                    if d.len() - 4 < blksize {
                        break;
                    }
                }
                op => panic!("unexpected TFTP opcode {op}"),
            }
        }
        assert!(p.recv().is_none());
        (data, sizes)
    }

    #[test]
    fn tftp_serves_a_file_in_512_byte_blocks() {
        let mut p = BuiltinPeer::new();
        let file: Vec<u8> = (0..1300u32).map(|i| i as u8).collect();
        p.add_file("start4.elf", file.clone());
        let (data, sizes) = fetch(&mut p, "start4.elf", &[], 512);
        assert_eq!(data, file);
        assert_eq!(sizes, [512, 512, 276]);
        assert!(p.take_log().last().unwrap().contains("sent, 1300 bytes"));
    }

    #[test]
    fn tftp_blksize_and_tsize_are_acknowledged() {
        let mut p = BuiltinPeer::new();
        let file = vec![0x5a; 3000];
        p.add_file("1aa2bb31/start4.elf", file.clone());
        p.send(&to_server(
            1000,
            69,
            &rrq(
                "/1aa2bb31/start4.elf",
                &[("blksize", "1482"), ("tsize", "0"), ("x", "y")],
            ),
        ));
        let (_, _, _, _, d) = parse_udp(&p.recv().unwrap());
        assert_eq!(be16(&d, 0), TFTP_OACK);
        assert_eq!(&d[2..], b"blksize\x001468\x00tsize\x003000\x00");

        let mut p = BuiltinPeer::new();
        p.add_file("1aa2bb31/start4.elf", file.clone());
        let (data, sizes) = fetch(&mut p, "1aa2bb31/start4.elf", &[("blksize", "1024")], 1024);
        assert_eq!(data, file);
        assert_eq!(sizes, [1024, 1024, 952]);
    }

    #[test]
    fn a_file_that_fills_its_last_block_ends_with_an_empty_one() {
        let mut p = BuiltinPeer::new();
        p.add_file("x", vec![1; 1024]);
        let (data, sizes) = fetch(&mut p, "x", &[], 512);
        assert_eq!(data.len(), 1024);
        assert_eq!(sizes, [512, 512, 0]);
    }

    #[test]
    fn tftp_missing_file_and_escapes_get_file_not_found() {
        let dir = std::env::temp_dir();
        let mut p = BuiltinPeer::with_root(&dir);
        for name in ["no-such-file", "../etc/passwd", "/../x"] {
            p.send(&to_server(1000, 69, &rrq(name, &[])));
            let (_, _, _, _, d) = parse_udp(&p.recv().unwrap());
            assert_eq!(be16(&d, 0), TFTP_ERROR);
            assert_eq!(be16(&d, 2), 1);
        }
    }

    #[test]
    fn tftp_reads_from_the_root_directory() {
        let dir = std::env::temp_dir().join(format!("rvf-tftp-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/config.txt"), b"arm_64bit=1\n").unwrap();
        let mut p = BuiltinPeer::with_root(&dir);
        let (data, _) = fetch(&mut p, "sub/config.txt", &[], 512);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(data, b"arm_64bit=1\n");
    }

    #[test]
    fn a_client_error_ends_the_transfer() {
        let mut p = BuiltinPeer::new();
        p.add_file("kernel8.img", vec![0; 10_000]);
        p.send(&to_server(1000, 69, &rrq("kernel8.img", &[("tsize", "0")])));
        let (_, _, tid, _, _) = parse_udp(&p.recv().unwrap());
        let mut e = TFTP_ERROR.to_be_bytes().to_vec();
        e.extend_from_slice(&[0, 0]);
        e.extend_from_slice(b"size probe\0");
        p.send(&to_server(1000, tid, &e));
        assert!(p.transfers.is_empty());
        p.send(&to_server(1000, tid, &ack(0)));
        assert!(p.recv().is_none(), "nothing left to answer");
    }

    #[test]
    fn dns_resolves_every_name_to_the_peer() {
        let mut p = BuiltinPeer::new();
        let mut q = vec![0xbe, 0xef, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for l in ["fw-download-alias1", "raspberrypi", "com"] {
            q.push(l.len() as u8);
            q.extend_from_slice(l.as_bytes());
        }
        q.extend_from_slice(&[0, 0, 1, 0, 1]);
        p.send(&to_server(5353, 53, &q));
        let (_, _, sport, dport, r) = parse_udp(&p.recv().unwrap());
        assert_eq!((sport, dport), (53, 5353));
        assert_eq!(r[..2], [0xbe, 0xef]);
        assert_eq!(r[2] & 0x80, 0x80, "a response");
        assert_eq!(r[3] & 0x0f, 0, "NOERROR");
        assert_eq!(be16(&r, 6), 1, "one answer");
        assert_eq!(r[r.len() - 4..], SERVER_IP);
        // AAAA: no records, still NOERROR.
        let n = q.len();
        q[n - 3] = 28;
        p.send(&to_server(5353, 53, &q));
        let (.., r) = parse_udp(&p.recv().unwrap());
        assert_eq!(be16(&r, 6), 0);
        assert!(p.take_log()[0].contains("-> 192.0.2.1"));
    }

    /// The client side of one TCP connection to the peer, for tests.
    struct Client {
        port: u16,
        seq: u32,
        ack: u32,
        window: u16,
    }

    impl Client {
        fn segment(&mut self, p: &mut BuiltinPeer, flags: u8, opts: &[u8], data: &[u8]) {
            let hlen = 20 + opts.len();
            let mut t = Vec::new();
            t.extend_from_slice(&self.port.to_be_bytes());
            t.extend_from_slice(&HTTP_PORT.to_be_bytes());
            t.extend_from_slice(&self.seq.to_be_bytes());
            t.extend_from_slice(&self.ack.to_be_bytes());
            t.push(((hlen / 4) as u8) << 4);
            t.push(flags);
            t.extend_from_slice(&self.window.to_be_bytes());
            t.extend_from_slice(&[0, 0, 0, 0]);
            t.extend_from_slice(opts);
            t.extend_from_slice(data);
            let c = transport_checksum(CLIENT_IP, SERVER_IP, IPPROTO_TCP, &t);
            t[16..18].copy_from_slice(&c.to_be_bytes());
            let mut ip = vec![0x45, 0, 0, 0, 0, 0, 0x40, 0, 64, IPPROTO_TCP, 0, 0];
            ip[2..4].copy_from_slice(&((20 + t.len()) as u16).to_be_bytes());
            ip.extend_from_slice(&CLIENT_IP);
            ip.extend_from_slice(&SERVER_IP);
            let c = checksum(&ip);
            ip[10..12].copy_from_slice(&c.to_be_bytes());
            ip.extend_from_slice(&t);
            let mut f = SERVER_MAC.to_vec();
            f.extend_from_slice(&CLIENT_MAC);
            f.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
            f.extend_from_slice(&ip);
            p.send(&f);
            self.seq = self
                .seq
                .wrapping_add(data.len() as u32 + u32::from(flags & (TCP_SYN | TCP_FIN) != 0));
        }
    }

    /// A segment from the peer: (seq, ack, flags, options, data), with both
    /// checksums verified.
    fn parse_tcp(f: &[u8]) -> (u32, u32, u8, Vec<u8>, Vec<u8>) {
        assert_eq!(f[..6], CLIENT_MAC);
        let ip = &f[14..];
        assert_eq!(checksum(&ip[..20]), 0);
        assert_eq!(ip[9], IPPROTO_TCP);
        let t = &ip[20..usize::from(be16(ip, 2))];
        assert_eq!(transport_checksum(SERVER_IP, CLIENT_IP, IPPROTO_TCP, t), 0);
        let doff = usize::from(t[12] >> 4) * 4;
        (
            be32(t, 4),
            be32(t, 8),
            t[13],
            t[20..doff].to_vec(),
            t[doff..].to_vec(),
        )
    }

    /// Open a connection, send `request`, read the response to the end with
    /// a receive window of `window` bytes, close. Returns the response and the
    /// largest number of bytes the peer ever had unacknowledged.
    fn http(p: &mut BuiltinPeer, request: &str, window: u16) -> (String, usize) {
        let mut c = Client {
            port: 40000,
            seq: 1000,
            ack: 0,
            window,
        };
        c.segment(p, TCP_SYN, &[2, 4, 0x05, 0xb4], &[]);
        let (iss, ack, flags, opts, _) = parse_tcp(&p.recv().unwrap());
        assert_eq!(flags, TCP_SYN | TCP_ACK);
        assert_eq!(ack, 1001);
        assert_eq!(opts, [2, 4, 0x05, 0xb4], "MSS 1460");
        c.ack = iss.wrapping_add(1);
        c.segment(p, TCP_ACK, &[], &[]);
        assert!(p.recv().is_none(), "nothing to say yet");
        c.segment(p, TCP_ACK | TCP_PSH, &[], request.as_bytes());

        let mut body = Vec::new();
        let mut max_in_flight = 0;
        loop {
            // Everything the peer sent in reply to our last segment.
            let mut got = Vec::new();
            while let Some(f) = p.recv() {
                got.push(parse_tcp(&f));
            }
            assert!(!got.is_empty(), "the peer always answers");
            let in_flight: usize = got.iter().map(|g| g.4.len()).sum();
            max_in_flight = max_in_flight.max(in_flight);
            let mut fin = false;
            for (seq, _, flags, _, data) in got {
                assert_eq!(seq, c.ack, "in order, no gaps");
                c.ack = c.ack.wrapping_add(data.len() as u32);
                body.extend_from_slice(&data);
                if flags & TCP_FIN != 0 {
                    c.ack = c.ack.wrapping_add(1);
                    fin = true;
                }
            }
            if fin {
                c.segment(p, TCP_FIN | TCP_ACK, &[], &[]);
                // The peer acknowledges our FIN, and is done.
                let (_, ack, flags, _, data) = parse_tcp(&p.recv().unwrap());
                assert_eq!((flags, ack, data.len()), (TCP_ACK, c.seq, 0));
                assert!(p.recv().is_none());
                break;
            }
            c.segment(p, TCP_ACK, &[], &[]);
        }
        assert!(p.conns.is_empty(), "closed");
        (String::from_utf8_lossy(&body).into_owned(), max_in_flight)
    }

    #[test]
    fn http_get_serves_the_file_paced_by_the_client_window() {
        let mut p = BuiltinPeer::new();
        let file: String = (0..10_000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        p.add_file("net_install/boot.img", file.clone().into_bytes());
        let req = "GET /net_install/boot.img HTTP/1.1\r\nHost: fw-download-alias1.raspberrypi.com\r\n\r\n";
        let (resp, in_flight) = http(&mut p, req, 3000);
        let (head, body) = resp.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(head.contains("Content-Length: 10000"));
        assert!(head.contains("Connection: close"));
        assert_eq!(body, file);
        assert!(in_flight <= 3000, "window respected: {in_flight}");
        assert!(p
            .take_log()
            .iter()
            .any(|l| l.contains("-> 200, 10000 bytes")));
    }

    #[test]
    fn http_range_head_and_missing_files() {
        let mut p = BuiltinPeer::new();
        p.add_file("f", b"0123456789".to_vec());
        let (r, _) = http(&mut p, "GET /f HTTP/1.1\r\nRange: bytes=2-5\r\n\r\n", 65535);
        assert!(r.starts_with("HTTP/1.1 206 Partial Content\r\n"));
        assert!(r.contains("Content-Range: bytes 2-5/10\r\n"));
        assert!(r.ends_with("\r\n\r\n2345"));
        let (r, _) = http(&mut p, "HEAD http://host/f?x=1 HTTP/1.1\r\n\r\n", 65535);
        assert!(r.contains("Content-Length: 10\r\n"));
        assert!(r.ends_with("\r\n\r\n"), "no body for HEAD");
        let (r, _) = http(&mut p, "GET /nope HTTP/1.1\r\n\r\n", 65535);
        assert!(r.starts_with("HTTP/1.1 404 Not Found"));
        let (r, _) = http(&mut p, "GET /f HTTP/1.1\r\nrange: bytes=20-\r\n\r\n", 65535);
        assert!(r.starts_with("HTTP/1.1 416"));
    }

    #[test]
    fn parse_range_forms() {
        assert_eq!(parse_range("bytes=0-0", 10), Some((0, 0)));
        assert_eq!(parse_range("bytes=4-", 10), Some((4, 9)));
        assert_eq!(parse_range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(parse_range("bytes=8-100", 10), Some((8, 9)));
        assert_eq!(parse_range("bytes=5-2", 10), None);
        assert_eq!(parse_range("bytes=10-", 10), None);
        assert_eq!(parse_range("items=0-1", 10), None);
    }

    #[test]
    fn tcp_to_a_closed_port_is_reset() {
        let mut p = BuiltinPeer::new();
        let mut c = Client {
            port: 40000,
            seq: 7,
            ack: 0,
            window: 1000,
        };
        let mut t = Vec::new();
        t.extend_from_slice(&c.port.to_be_bytes());
        t.extend_from_slice(&81u16.to_be_bytes());
        c.seq = 7;
        // Reuse the client builder for port 80, then check a stray ACK.
        c.segment(&mut p, TCP_ACK, &[], &[]);
        let (seq, _, flags, _, _) = parse_tcp(&p.recv().unwrap());
        assert_eq!(flags, TCP_RST, "no connection: RST with the ACK's number");
        assert_eq!(seq, 0);
    }

    #[test]
    fn ping() {
        let mut p = BuiltinPeer::new();
        let mut icmp = vec![8, 0, 0, 0, 0x12, 0x34, 0, 1, b'h', b'i'];
        let c = checksum(&icmp);
        icmp[2..4].copy_from_slice(&c.to_be_bytes());
        let mut ip = vec![0x45, 0, 0, 30, 0, 0, 0, 0, 64, IPPROTO_ICMP, 0, 0];
        ip.extend_from_slice(&CLIENT_IP);
        ip.extend_from_slice(&SERVER_IP);
        let c = checksum(&ip);
        ip[10..12].copy_from_slice(&c.to_be_bytes());
        ip.extend_from_slice(&icmp);
        let mut f = SERVER_MAC.to_vec();
        f.extend_from_slice(&CLIENT_MAC);
        f.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
        f.extend_from_slice(&ip);
        p.send(&f);
        let r = p.recv().unwrap();
        let reply = &r[14 + 20..];
        assert_eq!(reply[0], 0);
        assert_eq!(checksum(reply), 0);
        assert_eq!(reply[4..], icmp[4..]);
    }
}
