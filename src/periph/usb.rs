//! USB devices behind the VL805's root hub — the half that is not xHCI.
//!
//! [`super::xhci`] is the host controller: rings, contexts, doorbells. A device
//! here answers control transfers with descriptors and class requests and moves
//! bytes on its bulk and interrupt endpoints; it knows nothing about TRBs. A
//! device may have downstream ports of its own, so attachment is a tree:
//! [`UsbDevice::child`] is how the controller walks an xHCI route string.
//!
//! Both devices are modelled from bytes measured on a Raspberry Pi 4B d03115
//! with a Samsung "Flash Drive FIT" plugged in (`lsusb -v`, `/sys/bus/usb`):
//!
//! * [`Hub`] — the VIA Labs `2109:3431` four-port hub soldered to xHCI root
//!   port 1. It is the only thing a stock board has on the bus with nothing
//!   plugged in.
//! * [`MassStorage`] — a Bulk-Only Transport / SCSI disk (`090c:1000`), what
//!   `--usb <img>` attaches and, as the high-speed device a USB 3 stick is in a
//!   USB 2.0 socket, what `--otg <img>` puts on the USB-C port.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub use super::disk::Disk;
use super::disk::BLOCK_SIZE;

/// The `PORTSC` / slot-context speed encoding (xHCI 4.19.7).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed {
    Full = 1,
    Low = 2,
    High = 3,
    Super = 4,
}

/// A USB SETUP packet, as it arrives in a Setup Stage TRB.
#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    pub fn from_bytes(b: [u8; 8]) -> Setup {
        Setup {
            request_type: b[0],
            request: b[1],
            value: u16::from_le_bytes([b[2], b[3]]),
            index: u16::from_le_bytes([b[4], b[5]]),
            length: u16::from_le_bytes([b[6], b[7]]),
        }
    }

    pub fn dir_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }
}

const REQ_GET_STATUS: u8 = 0;
const REQ_CLEAR_FEATURE: u8 = 1;
const REQ_SET_FEATURE: u8 = 3;
const REQ_SET_ADDRESS: u8 = 5;
const REQ_GET_DESCRIPTOR: u8 = 6;
const REQ_GET_CONFIGURATION: u8 = 8;
const REQ_SET_CONFIGURATION: u8 = 9;
const REQ_GET_INTERFACE: u8 = 10;
const REQ_SET_INTERFACE: u8 = 11;

const DESC_DEVICE: u8 = 1;
const DESC_CONFIG: u8 = 2;
const DESC_STRING: u8 = 3;
const DESC_BOS: u8 = 15;

/// What a control or data transfer did. `Stall` is the legitimate answer to an
/// unsupported request, and enumeration paths depend on getting it.
///
/// `Nak` is "nothing yet": the transfer stays outstanding and the host sees no
/// completion at all, which is **not** `Ok(Vec::new())`, a real completion with
/// zero bytes. An idle hub NAKs its status-change endpoint forever; answering
/// zero-length instead has Linux resubmit the URB thousands of times a second.
pub enum Xfer {
    Ok(Vec<u8>),
    Nak,
    Stall,
}

pub trait UsbDevice: 'static {
    fn speed(&self) -> Speed;

    /// A port reset happened above this device: address back to 0, and for a
    /// hub, its downstream ports back to powered-but-idle.
    fn reset(&mut self);

    /// A control transfer on endpoint 0; the caller truncates the reply to
    /// `setup.length`.
    fn control(&mut self, setup: &Setup, data_out: &[u8]) -> Xfer;

    /// An IN transfer on a non-zero endpoint; fewer bytes than `len` is a
    /// short packet.
    fn data_in(&mut self, _ep: u8, _len: usize) -> Xfer {
        Xfer::Stall
    }

    fn data_out(&mut self, _ep: u8, _data: &[u8]) -> Xfer {
        Xfer::Stall
    }

    /// The device on downstream port `port` (1-based), for a hub.
    fn child(&mut self, _port: u8) -> Option<&mut (dyn UsbDevice + 'static)> {
        None
    }

    /// The slot context's "Hub" bit.
    fn is_hub(&self) -> bool {
        false
    }
}

/// The requests every device answers the same way; `None` for anything
/// device-specific, which the caller handles or stalls.
fn standard_control(dev: &mut CommonState, desc: &Descriptors, setup: &Setup) -> Option<Xfer> {
    // Only standard device-directed requests are handled here.
    if setup.request_type & 0x60 != 0 {
        return None;
    }
    Some(match setup.request {
        REQ_GET_DESCRIPTOR => {
            let kind = (setup.value >> 8) as u8;
            let idx = setup.value as u8;
            match kind {
                DESC_DEVICE => Xfer::Ok(desc.device.clone()),
                DESC_CONFIG => Xfer::Ok(desc.config.clone()),
                DESC_BOS => match &desc.bos {
                    Some(b) => Xfer::Ok(b.clone()),
                    None => Xfer::Stall,
                },
                DESC_STRING => match desc.strings.get(&idx) {
                    Some(s) => Xfer::Ok(s.clone()),
                    None => Xfer::Stall,
                },
                _ => return None,
            }
        }
        // SET_ADDRESS never reaches a device on xHCI, but answering is free.
        REQ_SET_ADDRESS => {
            dev.address = setup.value as u8;
            Xfer::Ok(Vec::new())
        }
        REQ_SET_CONFIGURATION => {
            dev.configuration = setup.value as u8;
            Xfer::Ok(Vec::new())
        }
        REQ_GET_CONFIGURATION => Xfer::Ok(vec![dev.configuration]),
        REQ_GET_STATUS => Xfer::Ok(vec![desc.status, 0]),
        REQ_SET_FEATURE | REQ_CLEAR_FEATURE => Xfer::Ok(Vec::new()),
        REQ_GET_INTERFACE => Xfer::Ok(vec![0]),
        REQ_SET_INTERFACE => Xfer::Ok(Vec::new()),
        _ => return None,
    })
}

pub struct Descriptors {
    pub device: Vec<u8>,
    /// The configuration descriptor and everything after it as one blob, which
    /// is what `GET_DESCRIPTOR(CONFIG)` returns.
    pub config: Vec<u8>,
    pub bos: Option<Vec<u8>>,
    pub strings: HashMap<u8, Vec<u8>>,
    pub status: u8,
}

#[derive(Default)]
pub struct CommonState {
    pub address: u8,
    pub configuration: u8,
}

fn string_desc(s: &str) -> Vec<u8> {
    let utf16: Vec<u16> = s.encode_utf16().collect();
    let mut v = vec![(2 + utf16.len() * 2) as u8, DESC_STRING];
    for c in utf16 {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v
}

fn lang_desc() -> Vec<u8> {
    vec![4, DESC_STRING, 0x09, 0x04]
}

const HUB_FEAT_PORT_CONNECTION: u16 = 0;
const HUB_FEAT_PORT_ENABLE: u16 = 1;
const HUB_FEAT_PORT_RESET: u16 = 4;
const HUB_FEAT_PORT_POWER: u16 = 8;
const HUB_FEAT_C_PORT_CONNECTION: u16 = 16;
const HUB_FEAT_C_PORT_ENABLE: u16 = 17;
const HUB_FEAT_C_PORT_SUSPEND: u16 = 18;
const HUB_FEAT_C_PORT_OVER_CURRENT: u16 = 19;
const HUB_FEAT_C_PORT_RESET: u16 = 20;

const DESC_HUB: u8 = 0x29;

#[derive(Default)]
struct HubPort {
    device: Option<Box<dyn UsbDevice>>,
    status: u16,
    change: u16,
}

/// The VIA Labs `2109:3431` four-port hub soldered to xHCI root port 1 of every
/// Pi 4B. Every descriptor byte below is verbatim from a Raspberry Pi 4B
/// d03115 (`/sys/bus/usb/devices/1-1/descriptors`).
pub struct Hub {
    common: CommonState,
    desc: Descriptors,
    ports: [HubPort; 4],
}

impl Default for Hub {
    fn default() -> Self {
        Hub::new()
    }
}

impl Hub {
    pub fn new() -> Hub {
        let mut strings = HashMap::new();
        strings.insert(0, lang_desc());
        strings.insert(1, string_desc("USB2.0 Hub"));
        let mut ports: [HubPort; 4] = Default::default();
        for p in &mut ports {
            p.status = 1 << 8;
        }
        Hub {
            common: CommonState::default(),
            desc: Descriptors {
                device: vec![
                    0x12, 0x01, 0x10, 0x02, 0x09, 0x00, 0x01, 0x40, 0x09, 0x21, 0x31, 0x34, 0x21,
                    0x04, 0x00, 0x01, 0x00, 0x01,
                ],
                config: vec![
                    0x09, 0x02, 0x19, 0x00, 0x01, 0x01, 0x00, 0xe0, 0x32, // configuration
                    0x09, 0x04, 0x00, 0x00, 0x01, 0x09, 0x00, 0x00, 0x00, // interface
                    0x07, 0x05, 0x81, 0x03, 0x01, 0x00, 0x0c, // endpoint 0x81
                ],
                bos: Some(vec![
                    0x05, 0x0f, 0x2a, 0x00, 0x03, 0x07, 0x10, 0x02, 0x02, 0x00, 0x00, 0x00, 0x0a,
                    0x10, 0x03, 0x00, 0x0e, 0x00, 0x01, 0x04, 0xe7, 0x00, 0x14, 0x10, 0x04, 0x00,
                    0x5c, 0xf3, 0xee, 0x30, 0xd5, 0x07, 0x49, 0x25, 0xb0, 0x01, 0x80, 0x2d, 0x79,
                    0x43, 0x4c, 0x30,
                ]),
                strings,
                status: 0x01,
            },
            ports,
        }
    }

    pub fn attach(&mut self, port: u8, device: Box<dyn UsbDevice>) {
        let speed = device.speed();
        let p = &mut self.ports[(port - 1) as usize];
        p.status = 1
            | (1 << 8)
            | match speed {
                Speed::Low => 1 << 9,
                Speed::High | Speed::Super => 1 << 10,
                Speed::Full => 0,
            };
        p.change = 1; // C_PORT_CONNECTION
        p.device = Some(device);
    }

    /// The hub descriptor, measured: four ports, ganged power and over-current,
    /// port indicators, no removable ports.
    fn hub_descriptor() -> Vec<u8> {
        vec![0x09, DESC_HUB, 0x04, 0xe0, 0x00, 0x32, 0x64, 0x00, 0xff]
    }

    fn class_control(&mut self, setup: &Setup, _data_out: &[u8]) -> Xfer {
        let recipient = setup.request_type & 0x1F;
        let port = setup.index as usize;
        match (recipient, setup.request) {
            (0, REQ_GET_DESCRIPTOR) => Xfer::Ok(Hub::hub_descriptor()),
            (0, REQ_GET_STATUS) => Xfer::Ok(vec![0, 0, 0, 0]),
            (0, REQ_SET_FEATURE) | (0, REQ_CLEAR_FEATURE) => Xfer::Ok(Vec::new()),
            (3, REQ_GET_STATUS) => {
                let Some(p) = self.ports.get(port.wrapping_sub(1)) else {
                    return Xfer::Stall;
                };
                let mut v = p.status.to_le_bytes().to_vec();
                v.extend_from_slice(&p.change.to_le_bytes());
                Xfer::Ok(v)
            }
            (3, REQ_SET_FEATURE) => {
                let Some(p) = self.ports.get_mut(port.wrapping_sub(1)) else {
                    return Xfer::Stall;
                };
                match setup.value {
                    HUB_FEAT_PORT_POWER => p.status |= 1 << 8,
                    HUB_FEAT_PORT_RESET => {
                        // A reset on an occupied port completes at once and
                        // leaves the port enabled, the only state the
                        // firmware's poll loop progresses from.
                        if let Some(d) = p.device.as_mut() {
                            d.reset();
                            p.status |= 1 << 1; // PORT_ENABLE
                        }
                        p.change |= 1 << 4; // C_PORT_RESET
                    }
                    _ => {}
                }
                Xfer::Ok(Vec::new())
            }
            (3, REQ_CLEAR_FEATURE) => {
                let Some(p) = self.ports.get_mut(port.wrapping_sub(1)) else {
                    return Xfer::Stall;
                };
                match setup.value {
                    HUB_FEAT_PORT_ENABLE => p.status &= !(1 << 1),
                    HUB_FEAT_PORT_POWER => p.status &= !(1 << 8),
                    HUB_FEAT_PORT_CONNECTION => {}
                    HUB_FEAT_C_PORT_CONNECTION => p.change &= !(1 << 0),
                    HUB_FEAT_C_PORT_ENABLE => p.change &= !(1 << 1),
                    HUB_FEAT_C_PORT_SUSPEND => p.change &= !(1 << 2),
                    HUB_FEAT_C_PORT_OVER_CURRENT => p.change &= !(1 << 3),
                    HUB_FEAT_C_PORT_RESET => p.change &= !(1 << 4),
                    _ => {}
                }
                Xfer::Ok(Vec::new())
            }
            _ => Xfer::Stall,
        }
    }
}

impl UsbDevice for Hub {
    fn speed(&self) -> Speed {
        Speed::High
    }

    fn is_hub(&self) -> bool {
        true
    }

    fn reset(&mut self) {
        self.common = CommonState::default();
    }

    fn control(&mut self, setup: &Setup, data_out: &[u8]) -> Xfer {
        if setup.request_type & 0x60 == 0x20 {
            return self.class_control(setup, data_out);
        }
        match standard_control(&mut self.common, &self.desc, setup) {
            Some(x) => x,
            None => Xfer::Stall,
        }
    }

    /// Endpoint `0x81`: one bit per port plus bit 0 for the hub. Nothing to
    /// report is a NAK, so the host's poll stays outstanding.
    fn data_in(&mut self, ep: u8, _len: usize) -> Xfer {
        if ep != 1 {
            return Xfer::Stall;
        }
        let mut bits = 0u8;
        for (i, p) in self.ports.iter().enumerate() {
            if p.change != 0 {
                bits |= 1 << (i + 1);
            }
        }
        if bits == 0 {
            Xfer::Nak
        } else {
            Xfer::Ok(vec![bits])
        }
    }

    fn child(&mut self, port: u8) -> Option<&mut (dyn UsbDevice + 'static)> {
        self.ports
            .get_mut(port.wrapping_sub(1) as usize)?
            .device
            .as_deref_mut()
    }
}

const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;
const CBW_LEN: usize = 31;
const CSW_LEN: usize = 13;

enum BotPhase {
    /// Waiting for the next command block.
    Command,
    /// Data to hand back on bulk-IN, then a CSW.
    DataIn { data: Vec<u8>, tag: u32 },
    /// Bytes still expected on bulk-OUT before the CSW. A SCSI WRITE carries
    /// where the data lands; other OUT data is swallowed.
    DataOut {
        remaining: usize,
        tag: u32,
        write: Option<(u64, Vec<u8>)>,
        status: u8,
    },
    /// Command finished; the CSW is the next bulk-IN.
    Status { tag: u32, residue: u32, status: u8 },
}

/// A READ or WRITE command's LBA and block count, in any of its three forms.
fn lba_count(cdb: &[u8]) -> Option<(u64, u64)> {
    let be = |r: std::ops::Range<usize>| {
        cdb.get(r)
            .map(|b| b.iter().fold(0u64, |v, &x| v << 8 | u64::from(x)))
    };
    match cdb.first()? {
        0x28 | 0x2A => Some((be(2..6)?, be(7..9)?)),
        0xA8 | 0xAA => Some((be(2..6)?, be(6..10)?)),
        0x88 | 0x8A => Some((be(2..10)?, be(10..14)?)),
        _ => None,
    }
}

/// A USB mass-storage device: Bulk-Only Transport carrying SCSI over a
/// [`Disk`]. Identity is measured on a Raspberry Pi 4B d03115 — descriptors
/// from `lsusb -v`, `INQUIRY` fields from `/sys/block/sda/device/*` — but the
/// capacity is the [`Disk`]'s.
pub struct MassStorage {
    common: CommonState,
    desc: Descriptors,
    disk: Rc<RefCell<Disk>>,
    phase: BotPhase,
    /// SuperSpeed in a blue socket, high speed in a USB 2.0 one.
    speed: Speed,
}

impl MassStorage {
    pub fn new(image: Vec<u8>) -> MassStorage {
        MassStorage::with_disk(Rc::new(RefCell::new(Disk::from_vec(image))))
    }

    /// The stick on a [`Disk`] shared with the next boot after a reset.
    pub fn with_disk(disk: Rc<RefCell<Disk>>) -> MassStorage {
        let mut strings = HashMap::new();
        strings.insert(0, lang_desc());
        strings.insert(1, string_desc("Samsung"));
        strings.insert(2, string_desc("Flash Drive FIT"));
        strings.insert(3, string_desc("0374122050000640"));
        MassStorage {
            common: CommonState::default(),
            desc: Descriptors {
                device: vec![
                    0x12, 0x01, 0x10, 0x03, 0x00, 0x00, 0x00, 0x09, 0x0c, 0x09, 0x00, 0x10, 0x00,
                    0x11, 0x01, 0x02, 0x03, 0x01,
                ],
                config: vec![
                    0x09, 0x02, 0x2c, 0x00, 0x01, 0x01, 0x00, 0x80, 0x26, // configuration
                    0x09, 0x04, 0x00, 0x00, 0x02, 0x08, 0x06, 0x50, 0x00, // interface
                    0x07, 0x05, 0x01, 0x02, 0x00, 0x04, 0x00, // bulk OUT 0x01
                    0x06, 0x30, 0x08, 0x00, 0x00, 0x00, // SS companion
                    0x07, 0x05, 0x82, 0x02, 0x00, 0x04, 0x00, // bulk IN 0x82
                    0x06, 0x30, 0x08, 0x00, 0x00, 0x00, // SS companion
                ],
                bos: None,
                strings,
                status: 0x00,
            },
            disk,
            phase: BotPhase::Command,
            speed: Speed::Super,
        }
    }

    /// The same stick in a USB 2.0 socket (`--otg`). Not a second capture: a
    /// USB 3 device in a USB 2.0 port enumerates high-speed, so these follow
    /// from the measured SuperSpeed descriptors by the USB 2.0 rules —
    /// `bcdUSB` 2.00, 64-byte endpoint zero, 512-byte bulk endpoints, no
    /// SuperSpeed companions, and the same current in 2 mA units.
    pub fn with_disk_hs(disk: Rc<RefCell<Disk>>) -> MassStorage {
        let mut dev = MassStorage::with_disk(disk);
        dev.speed = Speed::High;
        dev.desc.device = vec![
            0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x0c, 0x09, 0x00, 0x10, 0x00, 0x11,
            0x01, 0x02, 0x03, 0x01,
        ];
        dev.desc.config = vec![
            0x09, 0x02, 0x20, 0x00, 0x01, 0x01, 0x00, 0x80, 0x98, // configuration
            0x09, 0x04, 0x00, 0x00, 0x02, 0x08, 0x06, 0x50, 0x00, // interface
            0x07, 0x05, 0x01, 0x02, 0x00, 0x02, 0x00, // bulk OUT 0x01
            0x07, 0x05, 0x82, 0x02, 0x00, 0x02, 0x00, // bulk IN 0x82
        ];
        dev
    }

    fn blocks(&self) -> u64 {
        self.disk.borrow().blocks()
    }

    fn scsi(&mut self, cdb: &[u8], alloc: usize) -> (Vec<u8>, u8) {
        match cdb.first().copied().unwrap_or(0) {
            0x00 => (Vec::new(), 0),
            0x03 => {
                let mut s = vec![0u8; 18];
                s[0] = 0x70;
                s[7] = 10;
                (s, 0)
            }
            // INQUIRY, fields measured: direct access, removable, SPC-5.
            0x12 => {
                let mut d = vec![0u8; 36];
                d[0] = 0x00;
                d[1] = 0x80;
                d[2] = 0x06;
                d[3] = 0x02;
                d[4] = 31;
                d[8..16].copy_from_slice(b"Samsung ");
                d[16..32].copy_from_slice(b"Flash Drive FIT ");
                d[32..36].copy_from_slice(b"1100");
                (d, 0)
            }
            0x25 => {
                let last = self.blocks().saturating_sub(1) as u32;
                let mut d = Vec::with_capacity(8);
                d.extend_from_slice(&last.to_be_bytes());
                d.extend_from_slice(&(BLOCK_SIZE as u32).to_be_bytes());
                (d, 0)
            }
            0x28 | 0xA8 | 0x88 => {
                match lba_count(cdb).and_then(|(lba, n)| self.disk.borrow().read(lba, n)) {
                    Some(data) => (data, 0),
                    None => (Vec::new(), 1),
                }
            }
            0x1A => (vec![3, 0, 0, 0], 0),
            0x1E | 0x1B | 0x35 => (Vec::new(), 0),
            _ => {
                let _ = alloc;
                (Vec::new(), 1)
            }
        }
    }

    fn handle_cbw(&mut self, cbw: &[u8]) -> Xfer {
        if cbw.len() < CBW_LEN
            || u32::from_le_bytes([cbw[0], cbw[1], cbw[2], cbw[3]]) != CBW_SIGNATURE
        {
            return Xfer::Stall;
        }
        let tag = u32::from_le_bytes([cbw[4], cbw[5], cbw[6], cbw[7]]);
        let len = u32::from_le_bytes([cbw[8], cbw[9], cbw[10], cbw[11]]) as usize;
        let dir_in = cbw[12] & 0x80 != 0;
        let cdb_len = (cbw[14] & 0x1F) as usize;
        let cdb = cbw[15..15 + cdb_len.min(16)].to_vec();
        if dir_in || len == 0 {
            let (mut data, status) = self.scsi(&cdb, len);
            data.truncate(len);
            let residue = (len - data.len()) as u32;
            self.phase = if data.is_empty() {
                BotPhase::Status {
                    tag,
                    residue,
                    status,
                }
            } else {
                BotPhase::DataIn { data, tag }
            };
            if status != 0 {
                self.phase = BotPhase::Status {
                    tag,
                    residue: len as u32,
                    status,
                };
            }
        } else {
            // WRITE: the data goes onto the disk as it arrives.
            let (write, status) = match cdb.first().copied() {
                Some(0x2A | 0xAA | 0x8A) => match lba_count(&cdb) {
                    Some((lba, n)) if lba.saturating_add(n) <= self.blocks() => {
                        (Some((lba, Vec::new())), 0)
                    }
                    _ => (None, 1),
                },
                _ => (None, 0),
            };
            self.phase = BotPhase::DataOut {
                remaining: len,
                tag,
                write,
                status,
            };
        }
        Xfer::Ok(Vec::new())
    }

    fn csw(tag: u32, residue: u32, status: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(CSW_LEN);
        v.extend_from_slice(&CSW_SIGNATURE.to_le_bytes());
        v.extend_from_slice(&tag.to_le_bytes());
        v.extend_from_slice(&residue.to_le_bytes());
        v.push(status);
        v
    }
}

impl UsbDevice for MassStorage {
    fn speed(&self) -> Speed {
        self.speed
    }

    fn reset(&mut self) {
        self.common = CommonState::default();
        self.phase = BotPhase::Command;
    }

    fn control(&mut self, setup: &Setup, _data_out: &[u8]) -> Xfer {
        if setup.request_type & 0x60 == 0x20 {
            return match setup.request {
                0xFE => Xfer::Ok(vec![0]),
                0xFF => {
                    self.phase = BotPhase::Command;
                    Xfer::Ok(Vec::new())
                }
                _ => Xfer::Stall,
            };
        }
        match standard_control(&mut self.common, &self.desc, setup) {
            Some(x) => x,
            None => Xfer::Stall,
        }
    }

    fn data_in(&mut self, ep: u8, len: usize) -> Xfer {
        if ep != 2 {
            return Xfer::Stall;
        }
        // A stalled data phase is over either way: the next IN gets the status,
        // which is what BOT 6.7.2 has the host read once it clears the halt.
        if crate::jitter::fault("a stalled bulk IN") {
            if let BotPhase::DataIn { data, tag } =
                std::mem::replace(&mut self.phase, BotPhase::Command)
            {
                self.phase = BotPhase::Status {
                    tag,
                    residue: data.len() as u32,
                    status: 1,
                };
            }
            return Xfer::Stall;
        }
        match std::mem::replace(&mut self.phase, BotPhase::Command) {
            BotPhase::DataIn { mut data, tag } => {
                let take = len.min(data.len());
                let out: Vec<u8> = data.drain(..take).collect();
                self.phase = if data.is_empty() {
                    BotPhase::Status {
                        tag,
                        residue: 0,
                        status: 0,
                    }
                } else {
                    BotPhase::DataIn { data, tag }
                };
                Xfer::Ok(out)
            }
            BotPhase::Status {
                tag,
                residue,
                status,
            } => Xfer::Ok(MassStorage::csw(tag, residue, status)),
            other => {
                self.phase = other;
                Xfer::Ok(Vec::new())
            }
        }
    }

    fn data_out(&mut self, ep: u8, data: &[u8]) -> Xfer {
        if ep != 1 {
            return Xfer::Stall;
        }
        match std::mem::replace(&mut self.phase, BotPhase::Command) {
            BotPhase::Command => self.handle_cbw(data),
            BotPhase::DataOut {
                remaining,
                tag,
                mut write,
                status,
            } => {
                let take = data.len().min(remaining);
                if let Some((lba, pending)) = &mut write {
                    pending.extend_from_slice(&data[..take]);
                    let whole = pending.len() / BLOCK_SIZE * BLOCK_SIZE;
                    if whole > 0 {
                        self.disk.borrow_mut().write(*lba, &pending[..whole]);
                        *lba += (whole / BLOCK_SIZE) as u64;
                        pending.drain(..whole);
                    }
                }
                let left = remaining - take;
                self.phase = if left == 0 {
                    BotPhase::Status {
                        tag,
                        residue: 0,
                        status,
                    }
                } else {
                    BotPhase::DataOut {
                        remaining: left,
                        tag,
                        write,
                        status,
                    }
                };
                Xfer::Ok(Vec::new())
            }
            other => {
                self.phase = other;
                Xfer::Ok(Vec::new())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl(dev: &mut dyn UsbDevice, rt: u8, req: u8, value: u16, index: u16, len: u16) -> Vec<u8> {
        let setup = Setup {
            request_type: rt,
            request: req,
            value,
            index,
            length: len,
        };
        match dev.control(&setup, &[]) {
            Xfer::Ok(v) => v,
            Xfer::Nak => panic!("naked"),
            Xfer::Stall => panic!("stalled"),
        }
    }

    #[test]
    fn hub_device_descriptor_matches_the_real_board() {
        let mut hub = Hub::new();
        let d = ctrl(&mut hub, 0x80, REQ_GET_DESCRIPTOR, 0x0100, 0, 18);
        assert_eq!(
            d,
            vec![
                0x12, 0x01, 0x10, 0x02, 0x09, 0x00, 0x01, 0x40, 0x09, 0x21, 0x31, 0x34, 0x21, 0x04,
                0x00, 0x01, 0x00, 0x01
            ]
        );
        assert_eq!(u16::from_le_bytes([d[8], d[9]]), 0x2109);
        assert_eq!(u16::from_le_bytes([d[10], d[11]]), 0x3431);
    }

    #[test]
    fn hub_reports_empty_powered_ports() {
        let mut hub = Hub::new();
        for port in 1..=4 {
            let st = ctrl(&mut hub, 0xA3, REQ_GET_STATUS, 0, port, 4);
            assert_eq!(st, vec![0x00, 0x01, 0x00, 0x00], "port {port}");
        }
    }

    #[test]
    fn hub_reports_a_connect_and_clears_the_change_bit() {
        let mut hub = Hub::new();
        hub.attach(3, Box::new(MassStorage::new(vec![0; 4096])));
        let st = ctrl(&mut hub, 0xA3, REQ_GET_STATUS, 0, 3, 4);
        assert_eq!(u16::from_le_bytes([st[0], st[1]]) & 1, 1, "connected");
        assert_eq!(u16::from_le_bytes([st[2], st[3]]), 1, "C_PORT_CONNECTION");
        match hub.data_in(1, 1) {
            Xfer::Ok(v) => assert_eq!(v, vec![0b1000]),
            Xfer::Nak => panic!("naked"),
            Xfer::Stall => panic!("stalled"),
        }
        ctrl(
            &mut hub,
            0x23,
            REQ_CLEAR_FEATURE,
            HUB_FEAT_C_PORT_CONNECTION,
            3,
            0,
        );
        let st = ctrl(&mut hub, 0xA3, REQ_GET_STATUS, 0, 3, 4);
        assert_eq!(u16::from_le_bytes([st[2], st[3]]), 0);
    }

    #[test]
    fn bot_inquiry_and_capacity() {
        let mut msd = MassStorage::new(vec![0u8; 16 * BLOCK_SIZE]);
        let mut cbw = vec![0u8; CBW_LEN];
        cbw[..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        cbw[4..8].copy_from_slice(&1u32.to_le_bytes());
        cbw[8..12].copy_from_slice(&36u32.to_le_bytes());
        cbw[12] = 0x80;
        cbw[14] = 6;
        cbw[15] = 0x12;
        cbw[19] = 36;
        assert!(matches!(msd.data_out(1, &cbw), Xfer::Ok(_)));
        let Xfer::Ok(inq) = msd.data_in(2, 36) else {
            panic!("stall")
        };
        assert_eq!(&inq[8..16], b"Samsung ");
        assert_eq!(&inq[16..32], b"Flash Drive FIT ");
        assert_eq!(&inq[32..36], b"1100");
        let Xfer::Ok(csw) = msd.data_in(2, 13) else {
            panic!("stall")
        };
        assert_eq!(
            u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]),
            CSW_SIGNATURE
        );
        assert_eq!(csw[12], 0, "command succeeded");

        let mut cbw = vec![0u8; CBW_LEN];
        cbw[..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        cbw[8..12].copy_from_slice(&8u32.to_le_bytes());
        cbw[12] = 0x80;
        cbw[14] = 10;
        cbw[15] = 0x25;
        assert!(matches!(msd.data_out(1, &cbw), Xfer::Ok(_)));
        let Xfer::Ok(cap) = msd.data_in(2, 8) else {
            panic!("stall")
        };
        assert_eq!(u32::from_be_bytes([cap[0], cap[1], cap[2], cap[3]]), 15);
        assert_eq!(u32::from_be_bytes([cap[4], cap[5], cap[6], cap[7]]), 512);
    }

    #[test]
    fn bot_read10_returns_image_bytes() {
        let mut image = vec![0u8; 4 * BLOCK_SIZE];
        image[BLOCK_SIZE] = 0xAA;
        image[BLOCK_SIZE + 511] = 0x55;
        let mut msd = MassStorage::new(image);
        let mut cbw = vec![0u8; CBW_LEN];
        cbw[..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        cbw[8..12].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
        cbw[12] = 0x80;
        cbw[14] = 10;
        cbw[15] = 0x28;
        cbw[15 + 5] = 1; // LBA 1
        cbw[15 + 8] = 1; // one block
        assert!(matches!(msd.data_out(1, &cbw), Xfer::Ok(_)));
        let Xfer::Ok(data) = msd.data_in(2, BLOCK_SIZE) else {
            panic!("stall")
        };
        assert_eq!(data.len(), BLOCK_SIZE);
        assert_eq!(data[0], 0xAA);
        assert_eq!(data[511], 0x55);
    }

    fn rw10(op: u8, lba: u32, count: u16, dir_in: bool) -> Vec<u8> {
        let mut cbw = vec![0u8; CBW_LEN];
        cbw[..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        cbw[8..12].copy_from_slice(&(u32::from(count) * BLOCK_SIZE as u32).to_le_bytes());
        cbw[12] = if dir_in { 0x80 } else { 0 };
        cbw[14] = 10;
        cbw[15] = op;
        cbw[17..21].copy_from_slice(&lba.to_be_bytes());
        cbw[22..24].copy_from_slice(&count.to_be_bytes());
        cbw
    }

    /// What an initrd's repart does: write past the end of the image, then read
    /// it back after a reset.
    #[test]
    fn writes_land_on_the_disk_and_outlive_the_device() {
        let disk = Rc::new(RefCell::new(Disk::from_vec(vec![0x11; 4 * BLOCK_SIZE])));
        disk.borrow_mut().set_blocks(16); // a stick bigger than its image
        let mut msd = MassStorage::with_disk(disk.clone());
        let mut data = vec![0xAB; BLOCK_SIZE];
        data.extend(vec![0xCD; BLOCK_SIZE]);
        assert!(matches!(
            msd.data_out(1, &rw10(0x2A, 9, 2, false)),
            Xfer::Ok(_)
        ));
        msd.data_out(1, &data[..700]);
        msd.data_out(1, &data[700..]);
        let Xfer::Ok(csw) = msd.data_in(2, CSW_LEN) else {
            panic!("stall")
        };
        assert_eq!(csw[12], 0, "write succeeded");
        assert_eq!(disk.borrow().written_blocks(), 2);
        let mut next = MassStorage::with_disk(disk.clone());
        next.data_out(1, &rw10(0x28, 8, 3, true));
        let Xfer::Ok(back) = next.data_in(2, 3 * BLOCK_SIZE) else {
            panic!("stall")
        };
        assert_eq!(back[..BLOCK_SIZE], [0u8; BLOCK_SIZE][..], "past the image");
        assert_eq!(back[BLOCK_SIZE..], data[..]);
        assert_eq!(disk.borrow().read(0, 1).unwrap(), vec![0x11; BLOCK_SIZE]);
    }

    #[test]
    fn a_write_past_the_end_fails() {
        let mut msd = MassStorage::new(vec![0; 4 * BLOCK_SIZE]);
        msd.data_out(1, &rw10(0x2A, 3, 2, false));
        msd.data_out(1, &[0; 2 * BLOCK_SIZE]);
        let Xfer::Ok(csw) = msd.data_in(2, CSW_LEN) else {
            panic!("stall")
        };
        assert_eq!(csw[12], 1);
    }
}
