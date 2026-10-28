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
use std::collections::{HashMap, VecDeque};
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

    /// [`UsbDevice::data_in`] on a bulk endpoint set up with streams: `stream`
    /// is the Stream ID the host rang the doorbell with. A device without
    /// streams ignores it.
    fn data_in_stream(&mut self, ep: u8, _stream: u32, len: usize) -> Xfer {
        self.data_in(ep, len)
    }

    fn data_out_stream(&mut self, ep: u8, _stream: u32, data: &[u8]) -> Xfer {
        self.data_out(ep, data)
    }

    /// The device on downstream port `port` (1-based), for a hub.
    fn child(&mut self, _port: u8) -> Option<&mut (dyn UsbDevice + 'static)> {
        None
    }

    /// The slot context's "Hub" bit.
    fn is_hub(&self) -> bool {
        false
    }

    /// How long after `SET_ADDRESS` the device fails a request with a
    /// transaction error.
    fn recovery_us(&self) -> u64 {
        0
    }

    /// Bring the device's clock to modelled time `now_us`.
    fn advance(&mut self, _now_us: u64) {}

    /// The device has been given a bus address; one that has not answers at
    /// address 0, together with everything else in that state.
    fn addressed(&self) -> bool {
        true
    }

    fn set_address(&mut self, _address: u8) {}

    /// For a hub: another port than `port` has an enabled device still at
    /// address 0, which would answer the same `SET_ADDRESS`.
    fn sibling_at_default(&self, _port: u8) -> bool {
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
    reset_done_us: Option<u64>,
}

/// How long a port reset signals for: USB 2.0 `TDRST` is 10 ms at least, and
/// a hub holds `PORT_RESET` for that long before the port is enabled.
const PORT_RESET_US: u64 = 10_000;

const PORT_STATUS_ENABLE: u16 = 1 << 1;
const PORT_STATUS_RESET: u16 = 1 << 4;
const PORT_CHANGE_RESET: u16 = 1 << 4;

/// The VIA Labs `2109:3431` four-port hub soldered to xHCI root port 1 of every
/// Pi 4B. Every descriptor byte below is verbatim from a Raspberry Pi 4B
/// d03115 (`/sys/bus/usb/devices/1-1/descriptors`).
pub struct Hub {
    common: CommonState,
    desc: Descriptors,
    ports: [HubPort; 4],
    now_us: u64,
    recovery_us: u64,
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
            now_us: 0,
            recovery_us: 0,
        }
    }

    fn finish_resets(&mut self) {
        let now = self.now_us;
        for p in &mut self.ports {
            if p.reset_done_us.is_some_and(|t| t <= now) {
                p.reset_done_us = None;
                p.status &= !PORT_STATUS_RESET;
                if p.device.is_some() {
                    p.status |= PORT_STATUS_ENABLE;
                }
                p.change |= PORT_CHANGE_RESET;
            }
        }
    }

    /// A USB-C dock: a hub with a second hub and an Ethernet adapter behind
    /// it, and behind the second hub a card reader with no card and `stick`.
    /// Five devices that each need a controller slot. The topology is what a
    /// dock booted from on a Raspberry Pi 4B d03115;
    /// the device identities are not measured.
    pub fn dock(stick: MassStorage) -> Hub {
        let mut inner = Hub::dock_hub();
        inner.attach(1, Box::new(MassStorage::empty_card_reader()));
        inner.attach(2, Box::new(stick));
        let mut outer = Hub::dock_hub();
        outer.attach(1, Box::new(inner));
        outer.attach(2, Box::new(EthernetAdapter::new()));
        outer
    }

    /// A hub that fails the first request sent within 10 ms of `SET_ADDRESS`,
    /// as the dock's did on a Raspberry Pi 4B d03115.
    /// The soldered VIA hub has no such limit on record.
    fn dock_hub() -> Hub {
        Hub {
            recovery_us: 10_000,
            ..Hub::new()
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
                        if let Some(d) = p.device.as_mut() {
                            d.reset();
                        }
                        p.status &= !PORT_STATUS_ENABLE;
                        p.status |= PORT_STATUS_RESET;
                        p.reset_done_us = Some(self.now_us + PORT_RESET_US);
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

    fn advance(&mut self, now_us: u64) {
        self.now_us = now_us;
        self.finish_resets();
        for p in &mut self.ports {
            if let Some(d) = p.device.as_mut() {
                d.advance(now_us);
            }
        }
    }

    fn recovery_us(&self) -> u64 {
        self.recovery_us
    }

    fn addressed(&self) -> bool {
        self.common.address != 0
    }

    fn set_address(&mut self, address: u8) {
        self.common.address = address;
    }

    fn sibling_at_default(&self, port: u8) -> bool {
        self.ports.iter().enumerate().any(|(i, p)| {
            i + 1 != port as usize
                && p.status & PORT_STATUS_ENABLE != 0
                && p.device.as_ref().is_some_and(|d| !d.addressed())
        })
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

/// A USB Ethernet adapter that only enumerates: vendor-specific class, no
/// traffic. It holds a controller slot, which is all a dock needs of it.
pub struct EthernetAdapter {
    common: CommonState,
    desc: Descriptors,
}

impl Default for EthernetAdapter {
    fn default() -> Self {
        EthernetAdapter::new()
    }
}

impl EthernetAdapter {
    pub fn new() -> EthernetAdapter {
        let mut strings = HashMap::new();
        strings.insert(0, lang_desc());
        strings.insert(1, string_desc("Realtek"));
        strings.insert(2, string_desc("USB 10/100/1000 LAN"));
        EthernetAdapter {
            common: CommonState::default(),
            desc: Descriptors {
                device: vec![
                    0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0xda, 0x0b, 0x53, 0x81, 0x00,
                    0x30, 0x01, 0x02, 0x00, 0x01,
                ],
                config: vec![
                    0x09, 0x02, 0x19, 0x00, 0x01, 0x01, 0x00, 0xa0, 0x32, // configuration
                    0x09, 0x04, 0x00, 0x00, 0x01, 0xff, 0xff, 0x00, 0x00, // interface
                    0x07, 0x05, 0x81, 0x03, 0x02, 0x00, 0x08, // interrupt IN 0x81
                ],
                bos: None,
                strings,
                status: 0x00,
            },
        }
    }
}

impl UsbDevice for EthernetAdapter {
    fn speed(&self) -> Speed {
        Speed::High
    }

    fn addressed(&self) -> bool {
        self.common.address != 0
    }

    fn set_address(&mut self, address: u8) {
        self.common.address = address;
    }

    fn reset(&mut self) {
        self.common = CommonState::default();
    }

    fn control(&mut self, setup: &Setup, _data_out: &[u8]) -> Xfer {
        standard_control(&mut self.common, &self.desc, setup).unwrap_or(Xfer::Stall)
    }

    fn data_in(&mut self, _ep: u8, _len: usize) -> Xfer {
        Xfer::Nak
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
    /// A card reader with no card answers every medium command NOT READY.
    medium: bool,
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
            medium: true,
        }
    }

    /// The card reader of a dock with no card in it: it enumerates like a
    /// stick and answers TEST UNIT READY with NOT READY, MEDIUM NOT PRESENT.
    /// Identity is not measured.
    pub fn empty_card_reader() -> MassStorage {
        let mut dev = MassStorage::with_disk_hs(Rc::new(RefCell::new(Disk::from_vec(Vec::new()))));
        dev.medium = false;
        dev
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
        let op = cdb.first().copied().unwrap_or(0);
        if !self.medium && matches!(op, 0x00 | 0x25 | 0x28 | 0xA8 | 0x88) {
            return (Vec::new(), 1);
        }
        match op {
            0x00 => (Vec::new(), 0),
            0x03 => {
                let mut s = vec![0u8; 18];
                s[0] = 0x70;
                s[7] = 10;
                if !self.medium {
                    s[2] = 0x02;
                    s[12] = 0x3A;
                }
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

    fn addressed(&self) -> bool {
        self.common.address != 0
    }

    fn set_address(&mut self, address: u8) {
        self.common.address = address;
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

const IU_COMMAND: u8 = 0x01;
const IU_SENSE: u8 = 0x03;
const IU_READ_READY: u8 = 0x06;
const IU_WRITE_READY: u8 = 0x07;
const COMMAND_IU_LEN: usize = 32;
const SENSE_HEADER_LEN: usize = 16;
const SCSI_CHECK_CONDITION: u8 = 0x02;

/// `bInterfaceProtocol` of the Bulk-Only and the UAS alternate setting.
const PROTOCOL_BOT: u8 = 0x50;
const PROTOCOL_UAS: u8 = 0x62;
/// `bmAttributes` of the SuperSpeed companion on the pipes that use streams:
/// `MaxStreams` 4, so 16 stream IDs.
const UAS_MAX_STREAMS: u8 = 4;

/// One endpoint with its SuperSpeed companion, and for UAS its pipe usage
/// descriptor.
fn ss_endpoint(address: u8, max_streams: u8, pipe: Option<u8>) -> Vec<u8> {
    let mut d = vec![
        0x07,
        0x05,
        address,
        0x02,
        0x00,
        0x04,
        0x00, // endpoint, 1024 bytes
        0x06,
        0x30,
        0x0F,
        max_streams,
        0x00,
        0x00, // SS companion, burst 16
    ];
    if let Some(id) = pipe {
        d.extend_from_slice(&[0x04, 0x24, id, 0x00]);
    }
    d
}

/// Where a command is: what the data pipes still owe, and the sense IU that
/// ends it.
struct UasCommand {
    /// Bytes for the data-in pipe.
    data_in: Vec<u8>,
    /// A WRITE waiting for its data: next LBA, bytes still owed, and the
    /// bytes of a block not yet whole.
    write: Option<(u64, usize, Vec<u8>)>,
    status: u8,
}

/// A SuperSpeed mass-storage stick with two alternate settings on its one
/// interface, as every UAS stick has: setting 0 is Bulk-Only Transport
/// (`bInterfaceProtocol` `0x50`, [`MassStorage`] behind it), setting 1 is USB
/// Attached SCSI (`0x62`) with a command, a status, a data-in and a data-out
/// pipe. `SET_INTERFACE` switches.
///
/// UAS carries a command as a 32-byte Command IU on the command pipe. The
/// device answers on the status pipe with Read Ready or Write Ready IUs and a
/// Sense IU when the command is done; the data goes on the data pipes. Status
/// and data pipes use streams, and the device uses the command's tag as the
/// stream ID of everything that belongs to it.
///
/// The descriptors are not measured on a real UAS stick: they are written
/// from USB 3.2 section 9.6 and the UAS specification (INCITS 471) — endpoints
/// with a SuperSpeed companion carrying `MaxStreams` and a pipe usage
/// descriptor each (command 1, status 2, data-in 3, data-out 4) — with an
/// invented identity. The SCSI answers are [`MassStorage`]'s.
pub struct UasStorage {
    common: CommonState,
    desc: Descriptors,
    bot: MassStorage,
    alternate: u8,
    /// `SET_INTERFACE` to the UAS setting fails, as on a stick or hub whose UAS
    /// does not work: a host has to stay on Bulk-Only.
    refuse_uas: bool,
    commands: HashMap<u16, UasCommand>,
    /// IUs for the status pipe, by stream ID.
    status: HashMap<u32, VecDeque<Vec<u8>>>,
}

impl UasStorage {
    pub fn new(image: Vec<u8>) -> UasStorage {
        UasStorage::with_disk(Rc::new(RefCell::new(Disk::from_vec(image))))
    }

    pub fn with_disk(disk: Rc<RefCell<Disk>>) -> UasStorage {
        let mut strings = HashMap::new();
        strings.insert(0, lang_desc());
        strings.insert(1, string_desc("pimu"));
        strings.insert(2, string_desc("UAS Flash Drive"));
        strings.insert(3, string_desc("0000000000000UAS"));
        let interface = |alt: u8, endpoints: u8, protocol: u8| {
            vec![0x09, 0x04, 0x00, alt, endpoints, 0x08, 0x06, protocol, 0x00]
        };
        let mut config = vec![0x09, 0x02, 0, 0, 0x01, 0x01, 0x00, 0x80, 0x26];
        config.extend(interface(0, 2, PROTOCOL_BOT));
        config.extend(ss_endpoint(0x01, 0, None));
        config.extend(ss_endpoint(0x82, 0, None));
        config.extend(interface(1, 4, PROTOCOL_UAS));
        config.extend(ss_endpoint(0x01, 0, Some(1)));
        config.extend(ss_endpoint(0x82, UAS_MAX_STREAMS, Some(2)));
        config.extend(ss_endpoint(0x83, UAS_MAX_STREAMS, Some(3)));
        config.extend(ss_endpoint(0x04, UAS_MAX_STREAMS, Some(4)));
        let total = (config.len() as u16).to_le_bytes();
        config[2..4].copy_from_slice(&total);
        UasStorage {
            common: CommonState::default(),
            desc: Descriptors {
                device: vec![
                    0x12, 0x01, 0x10, 0x03, 0x00, 0x00, 0x00, 0x09, 0x09, 0x12, 0x01, 0x00, 0x00,
                    0x01, 0x01, 0x02, 0x03, 0x01,
                ],
                config,
                bos: None,
                strings,
                status: 0x00,
            },
            bot: MassStorage::with_disk(disk),
            alternate: 0,
            refuse_uas: false,
            commands: HashMap::new(),
            status: HashMap::new(),
        }
    }

    /// A stick whose UAS setting cannot be selected.
    pub fn refusing_uas(mut self) -> UasStorage {
        self.refuse_uas = true;
        self
    }

    fn select(&mut self, alternate: u8) {
        self.alternate = alternate;
        self.bot.reset();
        self.commands.clear();
        self.status.clear();
    }

    fn push_status(&mut self, tag: u16, iu: Vec<u8>) {
        self.status.entry(tag as u32).or_default().push_back(iu);
    }

    fn ready_iu(id: u8, tag: u16) -> Vec<u8> {
        let mut iu = vec![id, 0];
        iu.extend_from_slice(&tag.to_be_bytes());
        iu
    }

    fn sense_iu(tag: u16, status: u8) -> Vec<u8> {
        let sense = if status == 0 {
            Vec::new()
        } else {
            // ILLEGAL REQUEST, INVALID COMMAND OPERATION CODE.
            let mut s = vec![0u8; 18];
            s[0] = 0x70;
            s[2] = 0x05;
            s[7] = 10;
            s[12] = 0x20;
            s
        };
        let mut iu = vec![0u8; SENSE_HEADER_LEN];
        iu[0] = IU_SENSE;
        iu[2..4].copy_from_slice(&tag.to_be_bytes());
        iu[6] = status;
        iu[14..16].copy_from_slice(&(sense.len() as u16).to_be_bytes());
        iu.extend(sense);
        iu
    }

    fn command_iu(&mut self, iu: &[u8]) -> Xfer {
        if iu.len() < COMMAND_IU_LEN || iu[0] != IU_COMMAND {
            return Xfer::Stall;
        }
        let tag = u16::from_be_bytes([iu[2], iu[3]]);
        let cdb_len = 16 + usize::from(iu[6] >> 2) * 4;
        let cdb = &iu[16..iu.len().min(16 + cdb_len)];
        let op = cdb.first().copied().unwrap_or(0);
        if matches!(op, 0x2A | 0xAA | 0x8A) {
            self.write_command(tag, cdb);
            return Xfer::Ok(Vec::new());
        }
        let (data, status) = self.bot.scsi(cdb, 0);
        if status != 0 {
            self.push_status(tag, UasStorage::sense_iu(tag, SCSI_CHECK_CONDITION));
        } else if data.is_empty() {
            self.push_status(tag, UasStorage::sense_iu(tag, 0));
        } else {
            self.push_status(tag, UasStorage::ready_iu(IU_READ_READY, tag));
            self.commands.insert(
                tag,
                UasCommand {
                    data_in: data,
                    write: None,
                    status: 0,
                },
            );
        }
        Xfer::Ok(Vec::new())
    }

    fn write_command(&mut self, tag: u16, cdb: &[u8]) {
        match lba_count(cdb) {
            Some((lba, n)) if lba.saturating_add(n) <= self.bot.blocks() => {
                self.commands.insert(
                    tag,
                    UasCommand {
                        data_in: Vec::new(),
                        write: Some((lba, n as usize * BLOCK_SIZE, Vec::new())),
                        status: 0,
                    },
                );
                self.push_status(tag, UasStorage::ready_iu(IU_WRITE_READY, tag));
            }
            _ => self.push_status(tag, UasStorage::sense_iu(tag, SCSI_CHECK_CONDITION)),
        }
    }

    fn status_in(&mut self, stream: u32) -> Xfer {
        match self.status.get_mut(&stream).and_then(VecDeque::pop_front) {
            Some(iu) => Xfer::Ok(iu),
            None => Xfer::Nak,
        }
    }

    fn data_in_pipe(&mut self, stream: u32, len: usize) -> Xfer {
        let tag = stream as u16;
        let Some(cmd) = self.commands.get_mut(&tag) else {
            return Xfer::Nak;
        };
        if cmd.data_in.is_empty() {
            return Xfer::Nak;
        }
        let take = len.min(cmd.data_in.len());
        let out: Vec<u8> = cmd.data_in.drain(..take).collect();
        if cmd.data_in.is_empty() {
            let status = cmd.status;
            self.commands.remove(&tag);
            self.push_status(tag, UasStorage::sense_iu(tag, status));
        }
        Xfer::Ok(out)
    }

    fn data_out_pipe(&mut self, stream: u32, data: &[u8]) -> Xfer {
        let tag = stream as u16;
        let Some(UasCommand {
            write: Some((lba, owed, pending)),
            ..
        }) = self.commands.get_mut(&tag)
        else {
            return Xfer::Nak;
        };
        let take = data.len().min(*owed);
        pending.extend_from_slice(&data[..take]);
        *owed -= take;
        let whole = pending.len() / BLOCK_SIZE * BLOCK_SIZE;
        if whole > 0 {
            self.bot.disk.borrow_mut().write(*lba, &pending[..whole]);
            *lba += (whole / BLOCK_SIZE) as u64;
            pending.drain(..whole);
        }
        if *owed == 0 {
            self.commands.remove(&tag);
            self.push_status(tag, UasStorage::sense_iu(tag, 0));
        }
        Xfer::Ok(Vec::new())
    }
}

impl UsbDevice for UasStorage {
    fn speed(&self) -> Speed {
        Speed::Super
    }

    fn addressed(&self) -> bool {
        self.common.address != 0
    }

    fn set_address(&mut self, address: u8) {
        self.common.address = address;
    }

    fn reset(&mut self) {
        self.common = CommonState::default();
        self.select(0);
    }

    fn control(&mut self, setup: &Setup, data_out: &[u8]) -> Xfer {
        if setup.request_type & 0x60 == 0x20 {
            return self.bot.control(setup, data_out);
        }
        match (setup.request_type, setup.request) {
            (0x01, REQ_SET_INTERFACE) => {
                let alternate = setup.value as u8;
                if setup.index != 0 || alternate > 1 || (alternate == 1 && self.refuse_uas) {
                    return Xfer::Stall;
                }
                self.select(alternate);
                Xfer::Ok(Vec::new())
            }
            (0x81, REQ_GET_INTERFACE) => Xfer::Ok(vec![self.alternate]),
            (0x00, REQ_SET_CONFIGURATION) => {
                self.select(0);
                standard_control(&mut self.common, &self.desc, setup).unwrap_or(Xfer::Stall)
            }
            _ => standard_control(&mut self.common, &self.desc, setup).unwrap_or(Xfer::Stall),
        }
    }

    fn data_in(&mut self, ep: u8, len: usize) -> Xfer {
        self.data_in_stream(ep, 0, len)
    }

    fn data_out(&mut self, ep: u8, data: &[u8]) -> Xfer {
        self.data_out_stream(ep, 0, data)
    }

    fn data_in_stream(&mut self, ep: u8, stream: u32, len: usize) -> Xfer {
        if self.alternate == 0 {
            return self.bot.data_in(ep, len);
        }
        match ep {
            2 => self.status_in(stream),
            3 => self.data_in_pipe(stream, len),
            _ => Xfer::Stall,
        }
    }

    fn data_out_stream(&mut self, ep: u8, stream: u32, data: &[u8]) -> Xfer {
        if self.alternate == 0 {
            return self.bot.data_out(ep, data);
        }
        match ep {
            1 => self.command_iu(data),
            4 => self.data_out_pipe(stream, data),
            _ => Xfer::Stall,
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
    fn a_dock_puts_the_stick_two_hubs_deep_beside_an_empty_reader() {
        let mut dock = Hub::dock(MassStorage::new(vec![0; 4096]));
        assert!(dock.is_hub());
        assert!(dock.child(1).unwrap().is_hub(), "the second hub");
        assert_eq!(dock.child(2).unwrap().speed(), Speed::High, "the adapter");
        let inner = dock.child(1).unwrap();
        let reader = inner.child(1).unwrap();
        let ready = cbw(0x00);
        assert!(matches!(reader.data_out(1, &ready), Xfer::Ok(_)));
        let Xfer::Ok(csw) = reader.data_in(2, CSW_LEN) else {
            panic!("no status block");
        };
        assert_eq!(csw[12], 1, "no medium: the reader is not ready");
        let stick = inner.child(2).unwrap();
        assert!(matches!(stick.data_out(1, &ready), Xfer::Ok(_)));
        let Xfer::Ok(csw) = stick.data_in(2, CSW_LEN) else {
            panic!("no status block");
        };
        assert_eq!(csw[12], 0, "the stick is ready");
    }

    fn reset_port(hub: &mut Hub, port: u16) {
        ctrl(hub, 0x23, REQ_SET_FEATURE, HUB_FEAT_PORT_RESET, port, 0);
    }

    fn port_status(hub: &mut Hub, port: u16) -> (u16, u16) {
        let st = ctrl(hub, 0xA3, REQ_GET_STATUS, 0, port, 4);
        (
            u16::from_le_bytes([st[0], st[1]]),
            u16::from_le_bytes([st[2], st[3]]),
        )
    }

    #[test]
    fn a_port_reset_takes_ten_milliseconds() {
        let mut hub = Hub::new();
        hub.attach(1, Box::new(MassStorage::new(vec![0; 4096])));
        hub.advance(1_000);
        reset_port(&mut hub, 1);
        hub.advance(1_000 + PORT_RESET_US - 1);
        let (status, change) = port_status(&mut hub, 1);
        assert_ne!(status & PORT_STATUS_RESET, 0, "still resetting");
        assert_eq!(status & PORT_STATUS_ENABLE, 0, "not enabled yet");
        assert_eq!(change & PORT_CHANGE_RESET, 0);
        hub.advance(1_000 + PORT_RESET_US);
        let (status, change) = port_status(&mut hub, 1);
        assert_eq!(status & PORT_STATUS_RESET, 0);
        assert_ne!(status & PORT_STATUS_ENABLE, 0, "enabled");
        assert_ne!(change & PORT_CHANGE_RESET, 0, "C_PORT_RESET");
    }

    /// Two enabled devices at address 0 answer the same `SET_ADDRESS`.
    #[test]
    fn an_enabled_sibling_at_address_zero_is_seen() {
        let mut hub = Hub::new();
        hub.attach(1, Box::new(MassStorage::new(vec![0; 4096])));
        hub.attach(2, Box::new(MassStorage::new(vec![0; 4096])));
        reset_port(&mut hub, 1);
        reset_port(&mut hub, 2);
        hub.advance(PORT_RESET_US);
        assert!(hub.sibling_at_default(1));
        assert!(hub.sibling_at_default(2));
        hub.child(1).unwrap().set_address(1);
        assert!(hub.sibling_at_default(1), "port 2 is still at 0");
        assert!(!hub.sibling_at_default(2), "port 1 has its address");
    }

    #[test]
    fn a_device_waiting_for_its_reset_is_not_a_sibling_at_address_zero() {
        let mut hub = Hub::new();
        hub.attach(1, Box::new(MassStorage::new(vec![0; 4096])));
        hub.attach(2, Box::new(MassStorage::new(vec![0; 4096])));
        reset_port(&mut hub, 1);
        hub.advance(PORT_RESET_US);
        reset_port(&mut hub, 2);
        assert!(!hub.sibling_at_default(1));
    }

    fn cbw(op: u8) -> Vec<u8> {
        let mut v = vec![0u8; CBW_LEN];
        v[..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        v[14] = 6;
        v[15] = op;
        v
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

    fn uas_command(tag: u16, cdb: &[u8]) -> Vec<u8> {
        let mut iu = vec![0u8; COMMAND_IU_LEN];
        iu[0] = IU_COMMAND;
        iu[2..4].copy_from_slice(&tag.to_be_bytes());
        iu[16..16 + cdb.len()].copy_from_slice(cdb);
        iu
    }

    fn uas_stick() -> UasStorage {
        let mut dev = UasStorage::new(vec![0x11; 4 * BLOCK_SIZE]);
        ctrl(&mut dev, 0x01, REQ_SET_INTERFACE, 1, 0, 0);
        dev
    }

    fn status_iu(dev: &mut UasStorage, stream: u32) -> Vec<u8> {
        match dev.data_in_stream(2, stream, 128) {
            Xfer::Ok(iu) => iu,
            _ => panic!("no status IU"),
        }
    }

    #[test]
    fn a_uas_stick_describes_a_bot_and_a_uas_setting_on_one_interface() {
        let mut dev = UasStorage::new(Vec::new());
        let config = ctrl(&mut dev, 0x80, REQ_GET_DESCRIPTOR, 0x0200, 0, 512);
        assert_eq!(
            u16::from_le_bytes([config[2], config[3]]) as usize,
            config.len()
        );
        let mut at = 0;
        let (mut interfaces, mut pipes, mut streams) = (Vec::new(), Vec::new(), Vec::new());
        while at < config.len() {
            let d = &config[at..at + config[at] as usize];
            match d[1] {
                4 => interfaces.push((d[2], d[3], d[4], d[7])),
                0x24 => pipes.push(d[2]),
                0x30 => streams.push(d[3]),
                _ => {}
            }
            at += d.len();
        }
        assert_eq!(interfaces, [(0, 0, 2, 0x50), (0, 1, 4, 0x62)]);
        assert_eq!(pipes, [1, 2, 3, 4]);
        assert_eq!(streams, [0, 0, 0, 4, 4, 4]);
    }

    #[test]
    fn a_uas_read_gets_read_ready_data_and_a_sense_iu_on_the_tags_stream() {
        let mut dev = uas_stick();
        let mut cdb = [0u8; 10];
        cdb[0] = 0x28;
        cdb[5] = 2;
        cdb[8] = 1;
        assert!(matches!(
            dev.data_out_stream(1, 0, &uas_command(7, &cdb)),
            Xfer::Ok(_)
        ));
        assert!(
            matches!(dev.data_in_stream(2, 1, 128), Xfer::Nak),
            "other streams are idle"
        );
        assert_eq!(status_iu(&mut dev, 7), vec![IU_READ_READY, 0, 0, 7]);
        let Xfer::Ok(data) = dev.data_in_stream(3, 7, 512) else {
            panic!("no data")
        };
        assert_eq!(data, vec![0x11; BLOCK_SIZE]);
        let sense = status_iu(&mut dev, 7);
        assert_eq!(
            (sense[0], sense[2..4].to_vec(), sense[6]),
            (IU_SENSE, vec![0, 7], 0)
        );
    }

    #[test]
    fn a_uas_write_waits_for_its_write_ready_and_lands_on_the_disk() {
        let mut dev = uas_stick();
        let mut cdb = [0u8; 10];
        cdb[0] = 0x2A;
        cdb[5] = 1;
        cdb[8] = 1;
        dev.data_out_stream(1, 0, &uas_command(1, &cdb));
        assert_eq!(status_iu(&mut dev, 1), vec![IU_WRITE_READY, 0, 0, 1]);
        assert!(
            matches!(dev.data_in_stream(2, 1, 128), Xfer::Nak),
            "no sense before the data"
        );
        dev.data_out_stream(4, 1, &[0xAB; BLOCK_SIZE]);
        assert_eq!(status_iu(&mut dev, 1)[6], 0);
        assert_eq!(
            dev.bot.disk.borrow().read(1, 1).unwrap(),
            vec![0xAB; BLOCK_SIZE]
        );
    }

    #[test]
    fn uas_commands_without_data_end_in_a_sense_iu() {
        let mut dev = uas_stick();
        dev.data_out_stream(1, 0, &uas_command(1, &[0x00; 6]));
        assert_eq!(status_iu(&mut dev, 1)[6], 0, "test unit ready");

        let mut capacity = [0u8; 10];
        capacity[0] = 0x25;
        dev.data_out_stream(1, 0, &uas_command(1, &capacity));
        assert_eq!(status_iu(&mut dev, 1)[0], IU_READ_READY);
        let Xfer::Ok(data) = dev.data_in_stream(3, 1, 8) else {
            panic!("no data")
        };
        assert_eq!(
            data,
            [0, 0, 0, 3, 0, 0, 2, 0],
            "last LBA 3, 512-byte blocks"
        );
        status_iu(&mut dev, 1);

        dev.data_out_stream(1, 0, &uas_command(1, &[0xEE; 6]));
        let sense = status_iu(&mut dev, 1);
        assert_eq!(sense[6], SCSI_CHECK_CONDITION);
        assert_eq!(u16::from_be_bytes([sense[14], sense[15]]), 18);
    }

    #[test]
    fn selecting_setting_0_goes_back_to_bulk_only() {
        let mut dev = uas_stick();
        assert_eq!(ctrl(&mut dev, 0x81, REQ_GET_INTERFACE, 0, 0, 1), vec![1]);
        ctrl(&mut dev, 0x01, REQ_SET_INTERFACE, 0, 0, 0);
        assert_eq!(ctrl(&mut dev, 0x81, REQ_GET_INTERFACE, 0, 0, 1), vec![0]);
        assert!(matches!(dev.data_out(1, &cbw(0x00)), Xfer::Ok(_)));
        let Xfer::Ok(csw) = dev.data_in(2, CSW_LEN) else {
            panic!("stall")
        };
        assert_eq!(csw[..4], CSW_SIGNATURE.to_le_bytes());
    }

    #[test]
    fn a_stick_that_refuses_uas_stalls_the_request_and_stays_bulk_only() {
        let mut dev = UasStorage::new(vec![0; BLOCK_SIZE]).refusing_uas();
        let setup = Setup {
            request_type: 0x01,
            request: REQ_SET_INTERFACE,
            value: 1,
            index: 0,
            length: 0,
        };
        assert!(matches!(dev.control(&setup, &[]), Xfer::Stall));
        assert!(matches!(dev.data_out(1, &cbw(0x00)), Xfer::Ok(_)));
        assert!(matches!(dev.data_in(2, CSW_LEN), Xfer::Ok(_)));
    }
}
