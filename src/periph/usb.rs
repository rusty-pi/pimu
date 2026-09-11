//! USB devices behind the VL805's root hub — the half of issue #18 stage 3
//! that is not xHCI.
//!
//! [`super::xhci`] is the host controller: rings, contexts, doorbells. This
//! file is what those rings eventually talk to. A device here answers control
//! transfers with descriptors and class requests and moves bytes on its bulk
//! and interrupt endpoints; it knows nothing about TRBs.
//!
//! Two devices are modelled, both from bytes measured on `rpi-dev`
//! (`docs/usb-xhci.md` §5.2, and the `lsusb -v` capture that produced the
//! tables below):
//!
//! * [`Hub`] — the VIA Labs `2109:3431` four-port hub that a Pi 4B has soldered
//!   to xHCI root port 1. It is the *only* thing a stock board has on the bus
//!   with nothing plugged in, and it is why the reference log's second
//!   `XHCI-STOP` prints `USBSTS 18` instead of `USBSTS 0`.
//! * [`MassStorage`] — a Bulk-Only Transport / SCSI disk, modelled on the
//!   Samsung "Flash Drive FIT" (`090c:1000`) the ground-truth capture used.
//!   This is what `--usb <img>` attaches.
//!
//! ## Topology
//!
//! A device may itself have downstream ports, so the attachment is a tree, not
//! a list: [`UsbDevice::child`] is how the controller walks an xHCI route
//! string down to the device a slot addresses.

use std::collections::HashMap;

/// The `PORTSC` / slot-context speed encoding (xHCI 4.19.7 "Protocol Speed
/// ID"), for the default speed IDs the VL805 reports in its supported-protocol
/// extended capabilities.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed {
    Full = 1,
    Low = 2,
    High = 3,
    Super = 4,
}

/// A USB SETUP packet, as it arrives in the immediate data of a Setup Stage
/// TRB.
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

// Standard request codes (USB 2.0 §9.4).
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

/// What a control or data transfer did. `Stall` is a protocol error the host
/// reports as a Stall Error completion code; it is a legitimate answer to an
/// unsupported request and several enumeration paths depend on getting it.
pub enum Xfer {
    Ok(Vec<u8>),
    Stall,
}

/// One USB device on the bus.
pub trait UsbDevice: 'static {
    /// The speed the port reports when this device is attached.
    fn speed(&self) -> Speed;

    /// A port reset happened above this device: address back to 0, and for a
    /// hub, its downstream ports back to powered-but-idle.
    fn reset(&mut self);

    /// A control transfer on endpoint 0. `data_out` is the OUT payload, empty
    /// for an IN request; the returned bytes are the IN payload, truncated by
    /// the caller to `setup.length`.
    fn control(&mut self, setup: &Setup, data_out: &[u8]) -> Xfer;

    /// An IN transfer on a non-zero endpoint. `len` is how much the host asked
    /// for; returning fewer bytes is a short packet, which the host reports as
    /// a Short Packet completion with the residue.
    fn data_in(&mut self, _ep: u8, _len: usize) -> Xfer {
        Xfer::Stall
    }

    /// An OUT transfer on a non-zero endpoint.
    fn data_out(&mut self, _ep: u8, _data: &[u8]) -> Xfer {
        Xfer::Stall
    }

    /// The device on downstream port `port` (1-based), for a hub. The
    /// `'static` bound is what lets the controller hand the borrow back up
    /// through several hub tiers without the lifetime collapsing.
    fn child(&mut self, _port: u8) -> Option<&mut (dyn UsbDevice + 'static)> {
        None
    }

    /// Whether this device has downstream ports at all — the slot context's
    /// "Hub" bit, which the host sets when it configures a hub.
    fn is_hub(&self) -> bool {
        false
    }
}

/// Answer the descriptor and configuration requests every device answers the
/// same way, given its descriptor bytes. Returns `None` for anything
/// device-specific, which the caller then handles or stalls.
fn standard_control(
    dev: &mut CommonState,
    desc: &Descriptors,
    setup: &Setup,
) -> Option<Xfer> {
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
        // SET_ADDRESS never reaches a device on xHCI — the controller does
        // addressing itself — but answering it costs nothing.
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

/// The descriptor bytes a device answers `GET_DESCRIPTOR` with.
pub struct Descriptors {
    pub device: Vec<u8>,
    /// The configuration descriptor *and* everything that follows it —
    /// interface, endpoints, companions — as one blob, which is what a
    /// `wLength`-sized `GET_DESCRIPTOR(CONFIG)` returns.
    pub config: Vec<u8>,
    pub bos: Option<Vec<u8>>,
    pub strings: HashMap<u8, Vec<u8>>,
    /// The `GET_STATUS` low byte: bit 0 self-powered, bit 1 remote wakeup.
    pub status: u8,
}

/// State every device keeps regardless of class.
#[derive(Default)]
pub struct CommonState {
    pub address: u8,
    pub configuration: u8,
}

/// A UTF-16LE string descriptor.
fn string_desc(s: &str) -> Vec<u8> {
    let utf16: Vec<u16> = s.encode_utf16().collect();
    let mut v = vec![(2 + utf16.len() * 2) as u8, DESC_STRING];
    for c in utf16 {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v
}

/// String descriptor 0: the supported language list, US English.
fn lang_desc() -> Vec<u8> {
    vec![4, DESC_STRING, 0x09, 0x04]
}

// ---------------------------------------------------------------------------
// The VIA Labs hub

/// Hub class requests (USB 2.0 §11.24.2).
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

/// One downstream port of [`Hub`], in `GET_PORT_STATUS` terms.
#[derive(Default)]
struct HubPort {
    device: Option<Box<dyn UsbDevice>>,
    /// `wPortStatus`, low half of the `GET_PORT_STATUS` reply.
    status: u16,
    /// `wPortChange`, high half — write-1-to-clear through
    /// `CLEAR_FEATURE(C_PORT_*)`.
    change: u16,
}

/// The VIA Labs `2109:3431` four-port hub soldered to xHCI root port 1 of every
/// Pi 4B. Descriptor bytes below are verbatim from `rpi-dev`:
///
/// ```text
/// $ od -An -tx1 -v /sys/bus/usb/devices/1-1/descriptors
///  12 01 10 02 09 00 01 40 09 21 31 34 21 04 00 01
///  00 01 09 02 19 00 01 01 00 e0 32 09 04 00 00 01
///  09 00 00 00 07 05 81 03 01 00 0c
/// ```
///
/// i.e. `bcdUSB 2.10`, class 9 / protocol 1 (single TT), `bMaxPacketSize0 64`,
/// `bcdDevice 4.21`, `iProduct 1` = "USB2.0 Hub"; one configuration
/// (`wTotalLength 0x19`, self-powered + remote wakeup, 100 mA), one interface,
/// one interrupt-IN endpoint `0x81` with `wMaxPacketSize 1`, `bInterval 12`.
/// The hub descriptor and BOS blob were read the same way.
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
        // Power is on from the start: `lsusb -v` reports every port
        // `0000.0100 power` with nothing plugged in.
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
                // Self-powered, remote wakeup off: `Device Status 0x0001`.
                status: 0x01,
            },
            ports,
        }
    }

    /// Plug `device` into downstream port `port` (1-based).
    pub fn attach(&mut self, port: u8, device: Box<dyn UsbDevice>) {
        let speed = device.speed();
        let p = &mut self.ports[(port - 1) as usize];
        // Connected, powered, and at the speed the device negotiated. Bit 9 is
        // low-speed, bit 10 high-speed (USB 2.0 table 11-21).
        p.status = 1 | (1 << 8)
            | match speed {
                Speed::Low => 1 << 9,
                Speed::High | Speed::Super => 1 << 10,
                Speed::Full => 0,
            };
        p.change = 1; // C_PORT_CONNECTION
        p.device = Some(device);
    }

    /// `09 29 04 e0 00 32 64 00 ff` — four ports, ganged power and
    /// over-current, 32 FS-bit TT think time, port indicators, `bPwrOn2PwrGood`
    /// 50 (× 2 ms), `bHubContrCurrent` 100 mA, no removable ports, all-ones
    /// power-control mask. Measured; `lsusb -v` decodes it field for field.
    fn hub_descriptor() -> Vec<u8> {
        vec![0x09, DESC_HUB, 0x04, 0xe0, 0x00, 0x32, 0x64, 0x00, 0xff]
    }

    fn class_control(&mut self, setup: &Setup, _data_out: &[u8]) -> Xfer {
        let recipient = setup.request_type & 0x1F;
        let port = setup.index as usize;
        match (recipient, setup.request) {
            // GET_DESCRIPTOR(HUB) — class-specific, so it does not go through
            // `standard_control`.
            (0, REQ_GET_DESCRIPTOR) => Xfer::Ok(Hub::hub_descriptor()),
            // Hub status: no local power change, no over-current.
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
                        // A reset on an occupied port completes immediately and
                        // leaves the port enabled, which is the only state the
                        // firmware's poll loop can make progress from.
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

    /// Endpoint `0x81`, the status-change endpoint. One byte, one bit per port
    /// plus bit 0 for the hub itself; nothing to report is a NAK, which the
    /// host sees as a zero-length transfer.
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
            Xfer::Ok(Vec::new())
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

// ---------------------------------------------------------------------------
// Bulk-Only Transport mass storage

/// `CBW` signature `USBC`, `CSW` signature `USBS` (Bulk-Only Transport §5.1).
const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;
const CBW_LEN: usize = 31;
const CSW_LEN: usize = 13;

const BLOCK_SIZE: usize = 512;

/// What the device is in the middle of, between the CBW and the CSW.
enum BotPhase {
    /// Waiting for the next command block.
    Command,
    /// Data to hand back on the bulk-IN endpoint, then a CSW.
    DataIn { data: Vec<u8>, tag: u32 },
    /// Bytes still expected on the bulk-OUT endpoint before the CSW.
    DataOut { remaining: usize, tag: u32 },
    /// Command finished; the CSW is the next bulk-IN.
    Status { tag: u32, residue: u32, status: u8 },
}

/// A USB mass-storage device: Bulk-Only Transport carrying SCSI, backed by a
/// disk image.
///
/// Identity is the Samsung "Flash Drive FIT" (`090c:1000`) the stage-3 ground
/// truth was captured from — descriptors verbatim from `docs/usb-xhci.md` §5.2,
/// `INQUIRY` fields from `/sys/block/sda/device/*`. The capacity comes from the
/// image rather than the stick, because that is the one field a fixture cannot
/// borrow.
pub struct MassStorage {
    common: CommonState,
    desc: Descriptors,
    image: Vec<u8>,
    phase: BotPhase,
}

impl MassStorage {
    pub fn new(image: Vec<u8>) -> MassStorage {
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
                // Bus powered, no remote wakeup.
                status: 0x00,
            },
            image,
            phase: BotPhase::Command,
        }
    }

    fn blocks(&self) -> u64 {
        (self.image.len() / BLOCK_SIZE) as u64
    }

    /// Run one SCSI command block, returning the IN payload (for a read) and
    /// the CSW status byte.
    fn scsi(&mut self, cdb: &[u8], alloc: usize) -> (Vec<u8>, u8) {
        match cdb.first().copied().unwrap_or(0) {
            // TEST UNIT READY
            0x00 => (Vec::new(), 0),
            // REQUEST SENSE — always "no sense", fixed format.
            0x03 => {
                let mut s = vec![0u8; 18];
                s[0] = 0x70;
                s[7] = 10;
                (s, 0)
            }
            // INQUIRY. Fields measured off the reference stick: peripheral type
            // 0 (direct access), removable, SPC-5 (`scsi_level 7` means the
            // version byte is 6), vendor/model/rev padded to 8/16/4.
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
            // READ CAPACITY(10): last LBA, then block length.
            0x25 => {
                let last = self.blocks().saturating_sub(1) as u32;
                let mut d = Vec::with_capacity(8);
                d.extend_from_slice(&last.to_be_bytes());
                d.extend_from_slice(&(BLOCK_SIZE as u32).to_be_bytes());
                (d, 0)
            }
            // READ(10) / READ(12) / READ(16)
            0x28 | 0xA8 | 0x88 => {
                let (lba, count) = match cdb[0] {
                    0x28 => (
                        u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]) as u64,
                        u16::from_be_bytes([cdb[7], cdb[8]]) as u64,
                    ),
                    0xA8 => (
                        u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]) as u64,
                        u32::from_be_bytes([cdb[6], cdb[7], cdb[8], cdb[9]]) as u64,
                    ),
                    _ => (
                        u64::from_be_bytes([
                            cdb[2], cdb[3], cdb[4], cdb[5], cdb[6], cdb[7], cdb[8], cdb[9],
                        ]),
                        u32::from_be_bytes([cdb[10], cdb[11], cdb[12], cdb[13]]) as u64,
                    ),
                };
                let start = lba as usize * BLOCK_SIZE;
                let len = count as usize * BLOCK_SIZE;
                if start + len > self.image.len() {
                    return (Vec::new(), 1);
                }
                (self.image[start..start + len].to_vec(), 0)
            }
            // MODE SENSE(6): one header, no pages, not write protected.
            0x1A => (vec![3, 0, 0, 0], 0),
            // PREVENT/ALLOW MEDIUM REMOVAL, START STOP UNIT, SYNCHRONIZE CACHE
            0x1E | 0x1B | 0x35 => (Vec::new(), 0),
            // Anything else fails the command; the firmware only issues the
            // handful above.
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
            // A write. Nothing the bootloader does writes, so the data is
            // swallowed and the command reported good.
            self.phase = BotPhase::DataOut {
                remaining: len,
                tag,
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
        Speed::Super
    }

    fn reset(&mut self) {
        self.common = CommonState::default();
        self.phase = BotPhase::Command;
    }

    fn control(&mut self, setup: &Setup, _data_out: &[u8]) -> Xfer {
        // Bulk-Only Transport class requests (§3).
        if setup.request_type & 0x60 == 0x20 {
            return match setup.request {
                // GET MAX LUN — one logical unit, so zero.
                0xFE => Xfer::Ok(vec![0]),
                // BULK-ONLY MASS STORAGE RESET
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
            BotPhase::DataOut { remaining, tag } => {
                let left = remaining.saturating_sub(data.len());
                self.phase = if left == 0 {
                    BotPhase::Status {
                        tag,
                        residue: 0,
                        status: 0,
                    }
                } else {
                    BotPhase::DataOut {
                        remaining: left,
                        tag,
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
            Xfer::Stall => panic!("stalled"),
        }
    }

    #[test]
    fn hub_device_descriptor_matches_rpi_dev() {
        let mut hub = Hub::new();
        let d = ctrl(&mut hub, 0x80, REQ_GET_DESCRIPTOR, 0x0100, 0, 18);
        assert_eq!(
            d,
            vec![
                0x12, 0x01, 0x10, 0x02, 0x09, 0x00, 0x01, 0x40, 0x09, 0x21, 0x31, 0x34, 0x21, 0x04,
                0x00, 0x01, 0x00, 0x01
            ]
        );
        // VID 2109 PID 3431, the pair the bootloader's `DEV` line prints.
        assert_eq!(u16::from_le_bytes([d[8], d[9]]), 0x2109);
        assert_eq!(u16::from_le_bytes([d[10], d[11]]), 0x3431);
    }

    #[test]
    fn hub_reports_empty_powered_ports() {
        let mut hub = Hub::new();
        for port in 1..=4 {
            let st = ctrl(&mut hub, 0xA3, REQ_GET_STATUS, 0, port, 4);
            // `lsusb -v`: "Port n: 0000.0100 power".
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
        // The status-change endpoint flags the port that changed.
        match hub.data_in(1, 1) {
            Xfer::Ok(v) => assert_eq!(v, vec![0b1000]),
            Xfer::Stall => panic!("stalled"),
        }
        ctrl(&mut hub, 0x23, REQ_CLEAR_FEATURE, HUB_FEAT_C_PORT_CONNECTION, 3, 0);
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
        assert_eq!(u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]), CSW_SIGNATURE);
        assert_eq!(csw[12], 0, "command succeeded");

        // READ CAPACITY(10) reports the image's own geometry.
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
}
