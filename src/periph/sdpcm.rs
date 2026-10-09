//! What the CYW43455's own firmware does, as far as the host can see it: the
//! **SDPCM** frame protocol over SDIO function 2 and the **BCDC** control
//! channel inside it. [`Cyw43455`](super::cyw43455::Cyw43455) is the chip's
//! bus; the image the driver downloads over it lands in memory nothing
//! executes, and this is what would have run there.
//!
//! ```text
//!   host -> chip   CMD53 write, incrementing, one frame per command
//!   chip -> host   CMD53 read, fixed address, whatever the FIFO holds
//!
//!   0    2    4             12                            len
//!   +----+----+-------------+------------------------------+
//!   |len |~len| sw header   | payload (BCDC on channel 0)  |
//!   +----+----+-------------+------------------------------+
//!
//!   ...and once the chip has said it can glom, everything the host sends has
//!   eight more bytes (the glom extension) between the two headers.
//! ```
//!
//! `brcmf_sdio_hdparse` is the specification for what the chip may send, and
//! three of its rules shape this module:
//!
//! 1. An all-zero hardware header means "nothing more to read"; halves that do
//!    not complement are a checksum error.
//! 2. The software header carries **the highest sequence number the host may
//!    still send**, and the driver stops dead once it has used that window up —
//!    so every frame has to open it again.
//! 3. The driver reads the first 64 bytes of a frame and the rest after,
//!    rounded up to the function's block size, so a read past the end of a
//!    frame has to answer, and answer zeros.
//!
//! Channel 0 carries BCDC commands and their answers; channel 1 everything the
//! firmware says unasked, as an Ethernet frame the chip addresses to itself
//! whose OUI and subtype the driver checks; channel 2 is data, which goes
//! nowhere here. Which events reach the host is the host's choice, set twice —
//! `event_msgs` and, on this chip, `event_msgs_ext` — and both are answered
//! from the one mask the chip keeps.
//!
//! A scan is the one thing that is not an answer to a question: the `escan` set
//! is acknowledged like any other and the results arrive afterwards on the
//! event channel, one `BRCMF_E_ESCAN_RESULT` per network and one more to end
//! it. It takes a dwell per channel, which is the one place here where being
//! slower is being righter (see [`SCAN_DWELL_US`]), and the networks it reports
//! are **invented** ([`NETWORKS`]).
//!
//! This is not a radio. The responder answers what `brcmf_bus_started` asks on
//! the way to registering a network interface, with answers of the model's own —
//! a firmware version that says so, one band, an invented chip address — and
//! nothing here associates or carries a packet. Nothing raises an event during
//! a bring-up either, and that is not a gap: `brcmf_cfg80211_up` is a run of
//! BCDC commands and waits for nothing else.
//!
//! Line references are to raspberrypi/linux
//! `16f1da3c4e94437449d6aa151589ca0ad4b388bb`, under
//! `drivers/net/wireless/broadcom/brcm80211/brcmfmac/`.

use std::collections::VecDeque;

/// `SDPCM_HWHDR_LEN` (`sdio.c:1345`): the frame length and its complement.
const HWHDR_LEN: usize = 4;
/// The glom extension the host adds between the two headers once the chip has
/// said frames may be glommed: the frame's real length in `[23:0]`, "last of
/// the glom" in `[24]`, and the host's padding in the second word.
const HWEXT_LEN: usize = 8;
const HWEXT_LEN_MASK: u32 = 0x00FF_FFFF;
const SWHDR_LEN: usize = 8;
/// `SDPCM_HDRLEN`, and the offset a payload starts at when the chip adds no
/// padding of its own.
const HDRLEN: usize = HWHDR_LEN + SWHDR_LEN;

/// Software-header fields (`sdio.c:1350`).
const CHANNEL_SHIFT: u32 = 8;
const CHANNEL_MASK: u32 = 0x0000_0F00;
const DOFFSET_SHIFT: u32 = 24;
/// `SDPCM_WINDOW_MASK`, in the second word: the highest sequence number the
/// host may send.
const WINDOW_SHIFT: u32 = 8;

const CHANNEL_CONTROL: u8 = 0;
/// `SDPCM_EVENT_CHANNEL`: everything the chip says without being asked.
const CHANNEL_EVENT: u8 = 1;
/// `SDPCM_DATA_CHANNEL`: what `brcmf_sdio_txpkt` sends a network packet on.
const CHANNEL_DATA: u8 = 2;

/// How far ahead of what it has already sent the chip lets the host run.
const TX_WINDOW: u8 = 4;

/// The most the chip will hold of a frame the host is still writing.
const MAX_FRAME: usize = 64 * 1024;

/// BCDC's header (`bcdc.c:25`): command, buffer length, flags, status.
const BCDC_HDRLEN: usize = 16;
/// `BCDC_DCMD_ERROR`: the answer's `status` is a firmware error code.
const BCDC_ERROR: u32 = 0x01;
const BCDC_SET: u32 = 0x02;

/// `BCDC_HEADER_LEN`: the *other* BCDC header, the one on a frame that is not a
/// command.
const BCDC_DATA_HDRLEN: usize = 4;
/// `BCDC_PROTO_VER` in `[7:4]` of the flags (`BCDC_FLAG_VER_SHIFT`,
/// `bcdc.c:49`).
const BCDC_DATA_FLAGS: u8 = 2 << 4;

/// `ETH_P_LINK_CTL`: the ether type an event frame carries, and the first thing
/// `brcmf_fweh_process_skb` checks.
const ETH_P_LINK_CTL: u16 = 0x886C;
/// `BRCM_OUI` (`fweh.h:209`), in `struct brcm_ethhdr`.
const BRCM_OUI: [u8; 3] = [0x00, 0x10, 0x18];
/// `BCMILCP_BCM_SUBTYPE_EVENT`, the header's `usr_subtype`: the last of the
/// three things that make a frame an event.
const BCM_SUBTYPE_EVENT: u16 = 1;
const SUBTYPE_VENDOR_LONG: u16 = 32769;
const EVENT_MSG_VERSION: u16 = 2;
/// `sizeof(struct brcmf_event)`: the ether header, Broadcom's header and the
/// message, which is the least `brcmf_fweh_process_skb` will look at.
const EVENT_HDRLEN: usize = 14 + 10 + 48;
const EVENT_IFNAME_LEN: usize = 16;

/// `BRCMF_E_IF` (`fweh.h:77`): a bsscfg came or went.
const E_IF: u32 = 54;
const E_IF_ADD: u8 = 1;
const E_IF_ROLE_STA: u8 = 0;

/// `BRCMF_E_ESCAN_RESULT`: one network a scan found, or — with a status that is
/// not `PARTIAL` — the scan finishing.
const E_ESCAN_RESULT: u32 = 69;
const E_STATUS_SUCCESS: u32 = 0;
const E_STATUS_PARTIAL: u32 = 8;

/// `struct eventmsgs_ext`: version, command, mask length, the most a get may
/// answer with, and then the mask.
const EVENTMSGS_EXT_HDRLEN: usize = 4;
const EVENTMSGS_VER: u8 = 1;
const EVENTMSGS_SET_BIT: u8 = 1;
const EVENTMSGS_RESET_BIT: u8 = 2;
const EVENTMSGS_SET_MASK: u8 = 3;

/// The commands this answers with something other than zeros (`fwil.h`).
const C_GET_VERSION: u32 = 1;
const C_GET_BANDLIST: u32 = 140;
const C_GET_VAR: u32 = 262;
const C_SET_VAR: u32 = 263;

/// `BRCMU_D11AC_IOTYPE`, which is what `BRCMF_C_GET_VERSION` answers and what
/// `brcmu_d11_attach` needs to see before it will fit the channel-spec encoders
/// — a zero there leaves them null and the next channel walk dereferences them.
const D11AC_IOTYPE: u32 = 2;

const WLC_BAND_2G: u32 = 2;

/// A 20 MHz channel spec in the 802.11ac encoding: `BRCMU_CHSPEC_D11AC_BW_20`
/// with `BRCMU_CHSPEC_D11AC_BND_2G` (zero) and the channel number in the low
/// byte.
const CHSPEC_BW_20: u32 = 0x1000;
const CHSPEC_BW_MASK: u32 = 0x3800;
const CHSPEC_BAND_MASK: u32 = 0xC000;
const CHSPEC_BAND_2G: u32 = 0x0000;
/// The channels the model's firmware offers, which have to be ones
/// `__wl_2ghz_channels` knows: anything else is an "unexpected firmware
/// channel" the driver drops.
const CHANNELS_2G: std::ops::RangeInclusive<u32> = 1..=13;
/// The channel the model's firmware says it is on, which is the first one it
/// offers.
const CHANSPEC_HOME: u32 = CHSPEC_BW_20 | 1;
/// `BRCMU_CHSPEC_CH_MASK`: the channel number in a channel spec, which is all
/// this reads out of the ones a scan request names.
const CHSPEC_CH_MASK: u16 = 0x00FF;

/// `WL_ESCAN_ACTION_START` (`cfg80211.h:51`).
const ESCAN_ACTION_START: u16 = 1;
const ESCAN_REQ_VERSION_V2: u32 = 2;
/// `offsetof(struct brcmf_escan_params_le, params_le)`: the version, the action
/// and the sync id in front of the scan parameters.
const ESCAN_PARAMS_HDRLEN: usize = 8;
/// `BRCMF_SCAN_PARAMS_FIXED_SIZE` and `BRCMF_SCAN_PARAMS_V2_FIXED_SIZE`: where
/// `channel_list` starts in each shape.
const SCAN_PARAMS_FIXED: usize = 64;
const SCAN_PARAMS_V2_FIXED: usize = 72;
/// Where `channel_num` sits in each — the word before `channel_list`.
const SCAN_PARAMS_CHANNEL_NUM: usize = SCAN_PARAMS_FIXED - 4;
const SCAN_PARAMS_V2_CHANNEL_NUM: usize = SCAN_PARAMS_V2_FIXED - 4;
/// `BRCMF_SCAN_PARAMS_COUNT_MASK`: the low half of `channel_num` is how many
/// channel specs follow, and zero means all of them.
const SCAN_PARAMS_COUNT_MASK: u32 = 0x0000_FFFF;

/// `BRCMF_BSS_INFO_VERSION` (`fwil_types.h:21`).
const BSS_INFO_VERSION: u32 = 109;
/// `sizeof(struct brcmf_bss_info_le)` as a C compiler lays it out: the struct
/// is not `__packed`, so it is 128 bytes with alignment padding inside it, not
/// the 121 its fields add up to.
const BSS_INFO_LEN: usize = 128;
/// `WL_ESCAN_RESULTS_FIXED_SIZE`: `buflen`, `version`, `sync_id` and
/// `bss_count` in front of the one BSS.
const ESCAN_RESULTS_FIXED: usize = 12;

const BSS_CAPABILITY: u16 = 0x0001;
/// The beacon interval, in TU, which is what every access point uses and what
/// `iw` prints as `beacon interval: 100 TUs`.
const BSS_BEACON_PERIOD: u16 = 100;
const BSS_DTIM_PERIOD: u8 = 1;
/// The noise floor the model reports beside the signal.
const BSS_PHY_NOISE: i8 = -92;

/// Supported rates, in 500 kbit/s units with the high bit on the ones that are
/// basic — the encoding `struct brcmf_bss_info_le.rateset` and the Supported
/// Rates element share.
const BSS_RATES: [u8; 12] = [
    0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6C,
];
/// How many rates fit in a Supported Rates element before the rest have to go
/// in an Extended Supported Rates one (802.11, 9.4.2.3).
const SUPP_RATES_MAX: usize = 8;

const EID_SSID: u8 = 0;
const EID_SUPP_RATES: u8 = 1;
const EID_DS_PARAMS: u8 = 3;
const EID_EXT_SUPP_RATES: u8 = 50;

/// How long the chip spends on a channel before it moves to the next one.
const SCAN_DWELL_US: u64 = 40_000;

/// One network the model's firmware reports when the host scans.
struct Bss {
    ssid: &'static str,
    bssid: [u8; 6],
    /// A 2.4 GHz channel, which has to be one [`CHANNELS_2G`] offers: a result
    /// on any other is a BSS cfg80211 has no channel to put it on.
    channel: u32,
    rssi: i16,
}

/// What a scan finds, which is **invented**: there is no radio behind this
/// module and nothing was measured to produce it.
const NETWORKS: [Bss; 1] = [Bss {
    ssid: "pimu-model-ap",
    bssid: [0x02, 0x00, 0x5E, 0x00, 0x53, 0x04],
    channel: 1,
    rssi: -60,
}];

/// The address the chip came out of its own OTP with, which is what it answers
/// `cur_etheraddr` with until something overrides it.
const CHIP_MAC: [u8; 6] = [0x02, 0x00, 0x5E, 0x00, 0x53, 0x03];

const VERSION: &str = "pimu model firmware (no radio) version 0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacSource {
    Otp,
    Nvram,
    Host,
}

pub struct Sdpcm {
    rx: VecDeque<Vec<u8>>,
    rx_pos: usize,
    tx: Vec<u8>,
    seq: u8,
    /// The sequence number of the last frame in, which is what the window the
    /// chip advertises is measured from.
    host_seq: u8,
    /// The host has been told the chip can glom frames on the way up, and puts
    /// the extra hardware header on everything it sends from then on.
    glom: bool,
    fw: Firmware,
}

impl Default for Sdpcm {
    fn default() -> Self {
        Sdpcm::new()
    }
}

impl Sdpcm {
    pub fn new() -> Sdpcm {
        Sdpcm {
            rx: VecDeque::new(),
            rx_pos: 0,
            tx: Vec::new(),
            // The host starts at 255 and the chip at 0; neither side checks
            // the other's first number, only that it keeps counting.
            seq: 0,
            host_seq: 0,
            glom: false,
            fw: Firmware::new(),
        }
    }

    pub fn frame_waiting(&self) -> bool {
        !self.rx.is_empty()
    }

    #[inline]
    pub fn advance_to(&mut self, now_us: u64) -> bool {
        self.fw.now_us = now_us;
        !self.fw.pending.is_empty() && self.flush_events()
    }

    /// The firmware starting: take the address out of the nvram the host left
    /// in memory, then announce the interface it came up with.
    pub fn start(&mut self, nvram: &[u8]) {
        if let Some(mac) = nvram_macaddr(nvram) {
            self.fw.mac = mac;
            self.fw.mac_source = MacSource::Nvram;
        }
        self.fw.start();
        self.flush_events();
    }

    pub fn event_mask(&self) -> &[u8] {
        &self.fw.event_mask
    }

    pub fn event_mask_source(&self) -> EventMaskSource {
        self.fw.event_mask_source
    }

    pub fn events_wanted(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for (i, byte) in self.fw.event_mask.iter().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) != 0 {
                    out.push((i * 8 + bit) as u32);
                }
            }
        }
        out
    }

    pub fn events_sent(&self) -> u32 {
        self.fw.events_sent
    }

    pub fn events_dropped(&self) -> u32 {
        self.fw.events_dropped
    }

    pub fn data_frames_in(&self) -> u32 {
        self.fw.data_frames_in
    }

    pub fn escans(&self) -> u32 {
        self.fw.escans
    }

    pub fn escan_results(&self) -> u32 {
        self.fw.escan_results
    }

    pub fn mac(&self) -> [u8; 6] {
        self.fw.mac
    }

    pub fn mac_source(&self) -> MacSource {
        self.fw.mac_source
    }

    pub fn read(&mut self, out: &mut [u8]) {
        out.fill(0);
        let Some(frame) = self.rx.front() else {
            return;
        };
        let n = frame.len().saturating_sub(self.rx_pos).min(out.len());
        out[..n].copy_from_slice(&frame[self.rx_pos..self.rx_pos + n]);
        self.rx_pos += out.len();
        if self.rx_pos >= frame.len() {
            self.rx.pop_front();
            self.rx_pos = 0;
        }
    }

    pub fn write(&mut self, data: &[u8]) {
        // Nothing the driver sends comes near this, so a buffer that grows
        // past it is bytes the chip never made sense of.
        if self.tx.len() + data.len() > MAX_FRAME {
            self.tx.clear();
        }
        self.tx.extend_from_slice(data);
    }

    pub fn write_end(&mut self) {
        let min = if self.glom {
            HDRLEN + HWEXT_LEN
        } else {
            HDRLEN
        };
        match parse_header(&self.tx, self.glom) {
            _ if self.tx.len() < min => {}
            Some(sw) if sw.total > self.tx.len() => {}
            Some(sw) => {
                let frame = std::mem::take(&mut self.tx);
                self.frame_in(&sw, &frame);
            }
            // A header that is not one: a real chip answers with a
            // write-out-of-sync interrupt, which nothing here can raise.
            None => self.tx.clear(),
        }
    }

    fn frame_in(&mut self, sw: &Header, frame: &[u8]) {
        self.host_seq = sw.seq;
        match sw.channel {
            CHANNEL_CONTROL => {
                let reply = self.fw.control(&frame[sw.doffset..sw.len]);
                // Glomming is the one thing an answer changes about the
                // protocol itself.
                self.glom = self.fw.rxglom;
                self.push(CHANNEL_CONTROL, &reply);
                // Anything the command leaves to say goes out *behind* the
                // answer: the driver is waiting on the control channel, and an
                // event in front of it is one more frame to read first.
                self.flush_events();
            }
            // A packet to transmit. There is no radio, so it is taken — an
            // unread frame would stall the protocol — and goes nowhere.
            CHANNEL_DATA => self.fw.data_frames_in += 1,
            _ => {}
        }
    }

    fn flush_events(&mut self) -> bool {
        let mut sent = false;
        while self
            .fw
            .pending
            .front()
            .is_some_and(|(due, _)| *due <= self.fw.now_us)
        {
            let (_, event) = self.fw.pending.pop_front().expect("the front frame");
            let frame = event.encode(self.fw.mac);
            self.push(CHANNEL_EVENT, &frame);
            sent = true;
        }
        sent
    }

    fn push(&mut self, channel: u8, payload: &[u8]) {
        let len = HDRLEN + payload.len();
        let mut frame = Vec::with_capacity(len);
        let hw = len as u16;
        frame.extend_from_slice(&hw.to_le_bytes());
        frame.extend_from_slice(&(!hw).to_le_bytes());
        let first = u32::from(self.seq)
            | (u32::from(channel) << CHANNEL_SHIFT)
            | ((HDRLEN as u32) << DOFFSET_SHIFT);
        frame.extend_from_slice(&first.to_le_bytes());
        let second = u32::from(self.host_seq.wrapping_add(TX_WINDOW)) << WINDOW_SHIFT;
        frame.extend_from_slice(&second.to_le_bytes());
        frame.extend_from_slice(payload);
        self.seq = self.seq.wrapping_add(1);
        self.rx.push_back(frame);
    }
}

struct Header {
    seq: u8,
    channel: u8,
    len: usize,
    doffset: usize,
    /// How many bytes of the command belong to this frame, padding included —
    /// which is what says whether the rest of it has arrived yet.
    total: usize,
}

/// Parse the twelve bytes at the head of a frame, the way `brcmf_sdio_hdparse`
/// parses one the chip wrote.
fn parse_header(frame: &[u8], glom: bool) -> Option<Header> {
    let ext = if glom { HWEXT_LEN } else { 0 };
    if frame.len() < HDRLEN + ext {
        return None;
    }
    let hw = u16::from_le_bytes([frame[0], frame[1]]);
    let check = u16::from_le_bytes([frame[2], frame[3]]);
    if hw ^ check != u16::MAX {
        return None;
    }
    let total = usize::from(hw);
    let len = if glom {
        let word = u32::from_le_bytes(frame[4..8].try_into().ok()?);
        HWHDR_LEN + (word & HWEXT_LEN_MASK) as usize
    } else {
        total
    };
    if len < HDRLEN + ext || len > total {
        return None;
    }
    let at = HWHDR_LEN + ext;
    let first = u32::from_le_bytes(frame[at..at + 4].try_into().ok()?);
    let doffset = (first >> DOFFSET_SHIFT) as usize;
    if doffset < at + SWHDR_LEN || doffset > len {
        return None;
    }
    Some(Header {
        seq: first as u8,
        channel: ((first & CHANNEL_MASK) >> CHANNEL_SHIFT) as u8,
        len,
        doffset,
        total,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventMaskSource {
    Firmware,
    EventMsgs,
    EventMsgsExt,
}

struct Firmware {
    mac: [u8; 6],
    mac_source: MacSource,
    /// The host asked for receive glomming and was told yes, which changes the
    /// shape of everything it sends from then on.
    rxglom: bool,
    event_mask: Vec<u8>,
    event_mask_source: EventMaskSource,
    now_us: u64,
    pending: VecDeque<(u64, Event)>,
    events_sent: u32,
    events_dropped: u32,
    data_frames_in: u32,
    escans: u32,
    escan_results: u32,
}

impl Firmware {
    fn new() -> Firmware {
        Firmware {
            mac: CHIP_MAC,
            mac_source: MacSource::Otp,
            rxglom: false,
            event_mask: Vec::new(),
            event_mask_source: EventMaskSource::Firmware,
            now_us: 0,
            pending: VecDeque::new(),
            events_sent: 0,
            events_dropped: 0,
            data_frames_in: 0,
            escans: 0,
            escan_results: 0,
        }
    }

    fn start(&mut self) {
        let mut event = Event::new(E_IF);
        event.addr = self.mac;
        // `struct brcmf_if_event`: index and bsscfg 0 (the primary), role
        // station.
        event.data = vec![0, E_IF_ADD, 0, 0, E_IF_ROLE_STA];
        self.raise(event);
    }

    fn raise(&mut self, event: Event) {
        self.raise_at(event, self.now_us);
    }

    /// Raise one the chip only has at model time `due_us`, as a scan's results
    /// are.
    fn raise_at(&mut self, event: Event, due_us: u64) {
        if self.wants(event.code) {
            self.events_sent += 1;
            self.pending.push_back((due_us, event));
        } else {
            self.events_dropped += 1;
        }
    }

    /// Is `code`'s bit set? The driver numbers them byte `code / 8`, bit
    /// `code % 8`.
    fn wants(&self, code: u32) -> bool {
        let (byte, bit) = (code as usize / 8, code % 8);
        self.event_mask
            .get(byte)
            .is_some_and(|b| b & (1 << bit) != 0)
    }

    fn control(&mut self, req: &[u8]) -> Vec<u8> {
        if req.len() < BCDC_HDRLEN {
            return Vec::new();
        }
        let word = |i: usize| u32::from_le_bytes(req[i..i + 4].try_into().unwrap());
        let cmd = word(0);
        let len = word(4) as usize;
        let flags = word(8);
        let data = &req[BCDC_HDRLEN..];

        // A get carries back exactly the buffer the driver offered, so what it
        // copies out is defined whatever it asked for.
        let body = if flags & BCDC_SET != 0 {
            self.set(cmd, data);
            Vec::new()
        } else {
            self.get(cmd, len, data)
        };

        let mut out = Vec::with_capacity(BCDC_HDRLEN + body.len());
        out.extend_from_slice(&cmd.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(flags & !BCDC_ERROR).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn set(&mut self, cmd: u32, data: &[u8]) {
        if cmd != C_SET_VAR {
            return;
        }
        let Some((name, value)) = iovar(data) else {
            return;
        };
        let word = || {
            value
                .get(..4)
                .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
                .unwrap_or(0)
        };
        match name {
            b"cur_etheraddr" if value.len() >= 6 => {
                self.mac.copy_from_slice(&value[..6]);
                self.mac_source = MacSource::Host;
            }
            // A yes here is permission to use the glom header from now on.
            b"bus:rxglom" => self.rxglom = word() != 0,
            b"event_msgs" => {
                self.event_mask = value.to_vec();
                self.event_mask_source = EventMaskSource::EventMsgs;
            }
            b"event_msgs_ext" => self.event_msgs_ext_set(value),
            b"escan" => self.escan(value),
            _ => {}
        }
    }

    fn escan(&mut self, value: &[u8]) {
        let Some(head) = value.get(..ESCAN_PARAMS_HDRLEN) else {
            return;
        };
        let version = u32::from_le_bytes(head[..4].try_into().unwrap());
        let action = u16::from_le_bytes(head[4..6].try_into().unwrap());
        let sync_id = u16::from_le_bytes(head[6..8].try_into().unwrap());
        // `CONTINUE` and `ABORT` need a scan still running, and one never is:
        // the results are queued by the time the set is acknowledged.
        if action != ESCAN_ACTION_START {
            return;
        }
        self.escans += 1;

        // One channel at a time: a result comes due at the end of that
        // channel's dwell, the completion at the end of the last.
        let channels = escan_channels(version, &value[ESCAN_PARAMS_HDRLEN..])
            .unwrap_or_else(|| CHANNELS_2G.collect());
        let mut due = self.now_us;
        for channel in &channels {
            due += SCAN_DWELL_US;
            for bss in NETWORKS.iter().filter(|bss| bss.channel == *channel) {
                let mut event = Event::new(E_ESCAN_RESULT);
                event.status = E_STATUS_PARTIAL;
                event.addr = self.mac;
                event.data = escan_result(bss, sync_id);
                self.raise_at(event, due);
                self.escan_results += 1;
            }
        }

        // Then the scan ending, which is what tells cfg80211 the results are
        // all in; without it the driver's ten-second timeout reports the same
        // networks late and aborted. It carries no data.
        let mut done = Event::new(E_ESCAN_RESULT);
        done.status = E_STATUS_SUCCESS;
        done.addr = self.mac;
        self.raise_at(done, due);
    }

    fn event_msgs_ext_set(&mut self, value: &[u8]) {
        if value.len() < EVENTMSGS_EXT_HDRLEN {
            return;
        }
        let (command, len) = (value[1], usize::from(value[2]));
        let mask = &value[EVENTMSGS_EXT_HDRLEN..];
        let Some(mask) = mask.get(..len.min(mask.len())) else {
            return;
        };
        match command {
            EVENTMSGS_SET_MASK => self.event_mask = mask.to_vec(),
            EVENTMSGS_SET_BIT | EVENTMSGS_RESET_BIT => {
                if self.event_mask.len() < mask.len() {
                    self.event_mask.resize(mask.len(), 0);
                }
                for (have, want) in self.event_mask.iter_mut().zip(mask) {
                    if command == EVENTMSGS_SET_BIT {
                        *have |= want;
                    } else {
                        *have &= !want;
                    }
                }
            }
            _ => return,
        }
        self.event_mask_source = EventMaskSource::EventMsgsExt;
    }

    /// An `event_msgs_ext` get: the host's own header with the chip's mask
    /// under it.
    fn event_msgs_ext_get(&self, req: &[u8], room: usize) -> Vec<u8> {
        let mask_room = room.saturating_sub(EVENTMSGS_EXT_HDRLEN);
        let len = self.event_mask.len().min(mask_room).min(0xFF);
        let mut out = vec![
            EVENTMSGS_VER,
            req.get(1).copied().unwrap_or(0),
            len as u8,
            len as u8,
        ];
        out.extend_from_slice(&self.event_mask[..len]);
        out
    }

    fn get(&mut self, cmd: u32, len: usize, data: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; len.min(64 * 1024)];
        match cmd {
            C_GET_VERSION => put(&mut out, 0, &D11AC_IOTYPE.to_le_bytes()),
            // The first word is how many bands follow
            // (`brcmf_setup_wiphy`, `cfg80211.c`).
            C_GET_BANDLIST => {
                put(&mut out, 0, &1u32.to_le_bytes());
                put(&mut out, 4, &WLC_BAND_2G.to_le_bytes());
            }
            C_GET_VAR => {
                let Some((name, value)) = iovar(data) else {
                    return out;
                };
                match name {
                    b"cur_etheraddr" => put(&mut out, 0, &self.mac),
                    b"ver" => {
                        put(&mut out, 0, VERSION.as_bytes());
                        put(&mut out, VERSION.len(), &[0]);
                    }
                    b"chanspecs" => {
                        let filter = value
                            .get(..2)
                            .map(|v| u16::from_le_bytes(v.try_into().unwrap()))
                            .unwrap_or(0);
                        put(&mut out, 0, &chanspecs(filter));
                    }
                    b"chanspec" => put(&mut out, 0, &CHANSPEC_HOME.to_le_bytes()),
                    // What the host has asked for so far: the driver reads
                    // this before adding `BRCMF_E_IF` and writing it back.
                    b"event_msgs" => put(&mut out, 0, &self.event_mask),
                    b"event_msgs_ext" => {
                        let answer = self.event_msgs_ext_get(value, out.len());
                        put(&mut out, 0, &answer);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        out
    }
}

struct Event {
    code: u32,
    flags: u16,
    status: u32,
    reason: u32,
    addr: [u8; 6],
    ifidx: u8,
    bsscfgidx: u8,
    data: Vec<u8>,
}

impl Event {
    fn new(code: u32) -> Event {
        Event {
            code,
            flags: 0,
            status: 0,
            reason: 0,
            addr: [0; 6],
            ifidx: 0,
            bsscfgidx: 0,
            data: Vec::new(),
        }
    }

    fn encode(&self, mac: [u8; 6]) -> Vec<u8> {
        let mut out = Vec::with_capacity(BCDC_DATA_HDRLEN + EVENT_HDRLEN + self.data.len());
        out.extend_from_slice(&[BCDC_DATA_FLAGS, 0, self.ifidx, 0]);

        out.extend_from_slice(&mac);
        out.extend_from_slice(&mac);
        out.extend_from_slice(&ETH_P_LINK_CTL.to_be_bytes());

        let rest = (48 + self.data.len()) as u16;
        out.extend_from_slice(&SUBTYPE_VENDOR_LONG.to_be_bytes());
        out.extend_from_slice(&rest.to_be_bytes());
        out.push(0);
        out.extend_from_slice(&BRCM_OUI);
        out.extend_from_slice(&BCM_SUBTYPE_EVENT.to_be_bytes());

        out.extend_from_slice(&EVENT_MSG_VERSION.to_be_bytes());
        out.extend_from_slice(&self.flags.to_be_bytes());
        out.extend_from_slice(&self.code.to_be_bytes());
        out.extend_from_slice(&self.status.to_be_bytes());
        out.extend_from_slice(&self.reason.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(self.data.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.addr);
        // The interface's name, fixed width and NUL-padded. The driver reads
        // it only when it is the one creating the interface.
        out.extend_from_slice(&[0u8; EVENT_IFNAME_LEN]);
        out.push(self.ifidx);
        out.push(self.bsscfgidx);

        out.extend_from_slice(&self.data);
        out
    }
}

fn iovar(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let end = data.iter().position(|b| *b == 0)?;
    Some((&data[..end], &data[end + 1..]))
}

fn nvram_macaddr(nvram: &[u8]) -> Option<[u8; 6]> {
    nvram
        .split(|b| *b == 0)
        .filter_map(|entry| entry.strip_prefix(b"macaddr=".as_slice()))
        .find_map(parse_mac)
}

fn parse_mac(text: &[u8]) -> Option<[u8; 6]> {
    let text = std::str::from_utf8(text).ok()?;
    let mut parts = text.split(':');
    let mut out = [0u8; 6];
    for slot in &mut out {
        let part = parts.next()?;
        if part.len() != 2 {
            return None;
        }
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// Copy `src` into `out` at `at`, dropping whatever does not fit: the driver
/// decides how big its buffer is, and a firmware that overran it would be
/// writing past the end of the answer.
fn put(out: &mut [u8], at: usize, src: &[u8]) {
    let Some(room) = out.len().checked_sub(at) else {
        return;
    };
    let n = room.min(src.len());
    out[at..at + n].copy_from_slice(&src[..n]);
}

/// The information elements the model's firmware reports for a network, as they
/// would have come out of its beacon: an id, a length and the body, one after
/// another (802.11, 9.4.2).
fn beacon_ies(bss: &Bss) -> Vec<u8> {
    let mut out = Vec::new();
    let mut element = |id: u8, body: &[u8]| {
        out.push(id);
        out.push(body.len() as u8);
        out.extend_from_slice(body);
    };
    element(EID_SSID, bss.ssid.as_bytes());
    element(EID_SUPP_RATES, &BSS_RATES[..SUPP_RATES_MAX]);
    element(EID_DS_PARAMS, &[bss.channel as u8]);
    element(EID_EXT_SUPP_RATES, &BSS_RATES[SUPP_RATES_MAX..]);
    out
}

fn bss_info(bss: &Bss) -> Vec<u8> {
    let ies = beacon_ies(bss);
    let mut out = Vec::with_capacity(BSS_INFO_LEN + ies.len());
    let mut ssid = [0u8; 32];
    ssid[..bss.ssid.len()].copy_from_slice(bss.ssid.as_bytes());
    let mut rates = [0u8; 16];
    rates[..BSS_RATES.len()].copy_from_slice(&BSS_RATES);

    out.extend_from_slice(&BSS_INFO_VERSION.to_le_bytes());
    // `length` is the whole record, elements included: the handler checks it
    // against the event's `buflen` and walks results by it.
    out.extend_from_slice(&((BSS_INFO_LEN + ies.len()) as u32).to_le_bytes());
    out.extend_from_slice(&bss.bssid);
    out.extend_from_slice(&BSS_BEACON_PERIOD.to_le_bytes());
    out.extend_from_slice(&BSS_CAPABILITY.to_le_bytes());
    out.push(bss.ssid.len() as u8);
    out.extend_from_slice(&ssid);
    out.push(0);
    out.extend_from_slice(&(BSS_RATES.len() as u32).to_le_bytes());
    out.extend_from_slice(&rates);
    out.extend_from_slice(&((CHSPEC_BW_20 | bss.channel) as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.push(BSS_DTIM_PERIOD);
    out.push(0);
    out.extend_from_slice(&bss.rssi.to_le_bytes());
    out.push(BSS_PHY_NOISE as u8);
    out.push(0);
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&0u32.to_le_bytes()); // nbss_cap
                                                // `ctl_ch`, which is the channel `brcmf_inform_single_bss`
                                                // (`cfg80211.c:3381`) uses; it decodes `chanspec` for it only when this
                                                // is zero.
    out.push(bss.channel as u8);
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved32
    out.push(0); // flags
    out.extend_from_slice(&[0, 0, 0]); // reserved
    out.extend_from_slice(&[0u8; 16]); // basic_mcs
    out.extend_from_slice(&(BSS_INFO_LEN as u16).to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&(ies.len() as u32).to_le_bytes());
    let snr = bss.rssi - i16::from(BSS_PHY_NOISE);
    out.extend_from_slice(&snr.to_le_bytes());
    out.extend_from_slice(&[0, 0]);

    debug_assert_eq!(out.len(), BSS_INFO_LEN, "not the struct's own length");
    out.extend_from_slice(&ies);
    out
}

fn escan_result(bss: &Bss, sync_id: u16) -> Vec<u8> {
    let info = bss_info(bss);
    let mut out = Vec::with_capacity(ESCAN_RESULTS_FIXED + info.len());
    out.extend_from_slice(&((ESCAN_RESULTS_FIXED + info.len()) as u32).to_le_bytes());
    out.extend_from_slice(&BSS_INFO_VERSION.to_le_bytes());
    out.extend_from_slice(&sync_id.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&info);
    out
}

fn escan_channels(version: u32, params: &[u8]) -> Option<Vec<u32>> {
    let (at, list) = if version == ESCAN_REQ_VERSION_V2 {
        (SCAN_PARAMS_V2_CHANNEL_NUM, SCAN_PARAMS_V2_FIXED)
    } else {
        (SCAN_PARAMS_CHANNEL_NUM, SCAN_PARAMS_FIXED)
    };
    let word = params.get(at..at + 4)?;
    let count = (u32::from_le_bytes(word.try_into().ok()?) & SCAN_PARAMS_COUNT_MASK) as usize;
    if count == 0 {
        return None;
    }
    let specs = params.get(list..list + count * 2)?;
    Some(
        specs
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u32::from(u16::from_le_bytes(*c) & CHSPEC_CH_MASK))
            .collect(),
    )
}

fn chanspecs(filter: u16) -> Vec<u8> {
    let filter = u32::from(filter);
    let offered = filter & CHSPEC_BAND_MASK == CHSPEC_BAND_2G
        && matches!(filter & CHSPEC_BW_MASK, 0 | CHSPEC_BW_20);

    let mut out = Vec::new();
    let n = if offered {
        CHANNELS_2G.count() as u32
    } else {
        0
    };
    out.extend_from_slice(&n.to_le_bytes());
    if offered {
        for ch in CHANNELS_2G {
            out.extend_from_slice(&(CHSPEC_BW_20 | ch).to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(seq: u8, id: u16, cmd: u32, set: bool, payload: &[u8]) -> Vec<u8> {
        let mut bcdc = Vec::new();
        bcdc.extend_from_slice(&cmd.to_le_bytes());
        bcdc.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        let flags = u32::from(id) << 16 | if set { BCDC_SET } else { 0 };
        bcdc.extend_from_slice(&flags.to_le_bytes());
        bcdc.extend_from_slice(&0u32.to_le_bytes());
        bcdc.extend_from_slice(payload);

        let len = (HDRLEN + bcdc.len()) as u16;
        let mut frame = Vec::new();
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&(!len).to_le_bytes());
        let first = u32::from(seq) | (HDRLEN as u32) << DOFFSET_SHIFT;
        frame.extend_from_slice(&first.to_le_bytes());
        frame.extend_from_slice(&0u32.to_le_bytes());
        frame.extend_from_slice(&bcdc);
        frame.resize(frame.len().next_multiple_of(64), 0);
        frame
    }

    /// The nvram as it reaches the chip: NUL-terminated `key=value` entries,
    /// padded out to a word.
    fn nvram(lines: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for line in lines {
            out.extend_from_slice(line.as_bytes());
            out.push(0);
        }
        out.resize(out.len().next_multiple_of(4), 0);
        out
    }

    fn read_mac(chip: &mut Sdpcm, seq: u8) -> [u8; 6] {
        let mut iovar = named("cur_etheraddr", &[]);
        iovar.resize(iovar.len() + 6, 0);
        chip.write(&request(seq, 1, C_GET_VAR, false, &iovar));
        chip.write_end();
        let (_, payload) = read_frame(chip).expect("a frame");
        payload[BCDC_HDRLEN..BCDC_HDRLEN + 6].try_into().unwrap()
    }

    fn named(name: &str, value: &[u8]) -> Vec<u8> {
        let mut v = name.as_bytes().to_vec();
        v.push(0);
        v.extend_from_slice(value);
        v
    }

    fn read_frame(chip: &mut Sdpcm) -> Option<(Header, Vec<u8>)> {
        let mut first = [0u8; 64];
        chip.read(&mut first);
        let hd = parse_header(&first, false)?;
        let mut body = first.to_vec();
        if hd.len > first.len() {
            let mut rest = vec![0u8; (hd.len - first.len()).next_multiple_of(4)];
            chip.read(&mut rest);
            body.extend_from_slice(&rest);
        }
        body.truncate(hd.len);
        let payload = body[hd.doffset..].to_vec();
        Some((hd, payload))
    }

    fn bcdc_parts(payload: &[u8]) -> (u32, u32, u32, u32) {
        let w = |i: usize| u32::from_le_bytes(payload[i..i + 4].try_into().unwrap());
        (w(0), w(4), w(8), w(12))
    }

    #[test]
    fn nothing_is_waiting_until_the_host_asks_for_something() {
        let mut chip = Sdpcm::new();
        assert!(!chip.frame_waiting());
        let mut buf = [0xAAu8; 64];
        chip.read(&mut buf);
        assert!(buf.iter().all(|b| *b == 0));
    }

    #[test]
    fn a_control_request_is_answered_with_its_own_id() {
        let mut chip = Sdpcm::new();
        let iovar = named("bus:txglomalign", &4u32.to_le_bytes());
        chip.write(&request(255, 7, C_SET_VAR, true, &iovar));
        chip.write_end();
        assert!(chip.frame_waiting());

        let (hd, payload) = read_frame(&mut chip).expect("a frame");
        assert_eq!(hd.channel, CHANNEL_CONTROL);
        assert_eq!(hd.doffset, HDRLEN);
        let (cmd, len, flags, status) = bcdc_parts(&payload);
        assert_eq!(cmd, C_SET_VAR);
        assert_eq!(flags >> 16, 7, "the request id comes back");
        assert_eq!(flags & BCDC_ERROR, 0, "and nothing failed");
        assert_eq!(status, 0);
        assert_eq!(len, 0, "a set carries nothing back");
        assert!(!chip.frame_waiting(), "and the FIFO is empty again");
    }

    #[test]
    fn a_get_carries_back_the_buffer_the_driver_offered() {
        let mut chip = Sdpcm::new();
        let mut iovar = named("cur_etheraddr", &[]);
        iovar.resize(iovar.len() + 6, 0);
        chip.write(&request(0, 1, C_GET_VAR, false, &iovar));
        chip.write_end();

        let (_, payload) = read_frame(&mut chip).expect("a frame");
        let (_, len, _, _) = bcdc_parts(&payload);
        assert_eq!(len as usize, iovar.len());
        assert_eq!(&payload[BCDC_HDRLEN..BCDC_HDRLEN + 6], CHIP_MAC);
    }

    #[test]
    fn the_version_string_ends_in_a_number_after_a_space() {
        let mut chip = Sdpcm::new();
        let mut iovar = named("ver", &[]);
        iovar.resize(iovar.len() + 128, 0);
        chip.write(&request(0, 2, C_GET_VAR, false, &iovar));
        chip.write_end();

        let (_, payload) = read_frame(&mut chip).expect("a frame");
        let body = &payload[BCDC_HDRLEN..];
        let text =
            std::str::from_utf8(&body[..body.iter().position(|b| *b == 0).unwrap()]).unwrap();
        let (_, number) = text.rsplit_once(' ').expect("a space");
        assert!(!number.is_empty());
    }

    #[test]
    fn every_frame_opens_the_window_the_host_sends_in() {
        let mut chip = Sdpcm::new();
        let mut tx_seq: u8 = 255;
        for i in 0..8 {
            let iovar = named("mpc", &1u32.to_le_bytes());
            chip.write(&request(tx_seq, i + 1, C_SET_VAR, true, &iovar));
            chip.write_end();
            tx_seq = tx_seq.wrapping_add(1);

            let (_, payload) = read_frame(&mut chip).expect("a frame");
            assert_eq!(bcdc_parts(&payload).2 >> 16, u32::from(i) + 1);
            let tx_max = chip.host_seq.wrapping_add(TX_WINDOW);
            let room = tx_max.wrapping_sub(tx_seq);
            assert_ne!(room, 0, "the window closed at frame {i}");
            assert_eq!(room & 0x80, 0);
            assert!(room <= 0x40, "wider than the driver accepts");
        }
    }

    #[test]
    fn the_chips_sequence_number_counts_every_frame() {
        let mut chip = Sdpcm::new();
        let mut seen = Vec::new();
        for i in 0..4u8 {
            chip.write(&request(i, 1, C_GET_VERSION, false, &4u32.to_le_bytes()));
            chip.write_end();
            seen.push(read_frame(&mut chip).expect("a frame").0.seq);
        }
        assert_eq!(seen, vec![0, 1, 2, 3]);
    }

    #[test]
    fn a_malformed_frame_is_dropped_rather_than_answered() {
        let mut chip = Sdpcm::new();
        let mut bad = request(0, 1, C_GET_VERSION, false, &[]);
        bad[2] ^= 0xFF;
        chip.write(&bad);
        chip.write_end();
        assert!(!chip.frame_waiting());

        let mut bad = request(0, 1, C_GET_VERSION, false, &[]);
        bad[7] = 2;
        chip.write(&bad);
        chip.write_end();
        assert!(!chip.frame_waiting());

        chip.write(&[0u8; 6]);
        chip.write_end();
        assert!(!chip.frame_waiting());
    }

    /// The same request with the glom extension on it, as `brcmf_sdio_hdpack`
    /// builds one with `bus->txglom` set: the hardware header carries the
    /// padded length, the extension the real one, and the software header moves
    /// eight bytes along.
    fn glom_request(seq: u8, id: u16, cmd: u32, set: bool, payload: &[u8], pad: u16) -> Vec<u8> {
        let plain = request(seq, id, cmd, set, payload);
        let real = HDRLEN + HWEXT_LEN + BCDC_HDRLEN + payload.len();
        let mut frame = Vec::new();
        let total = (real + usize::from(pad)) as u16;
        frame.extend_from_slice(&total.to_le_bytes());
        frame.extend_from_slice(&(!total).to_le_bytes());
        frame.extend_from_slice(&(((real - HWHDR_LEN) as u32) | 1 << 24).to_le_bytes());
        frame.extend_from_slice(&(u32::from(pad) << 16).to_le_bytes());
        let mut sw = u32::from_le_bytes(plain[4..8].try_into().unwrap());
        sw = (sw & !0xFF00_0000) | ((HDRLEN + HWEXT_LEN) as u32) << DOFFSET_SHIFT;
        frame.extend_from_slice(&sw.to_le_bytes());
        frame.extend_from_slice(&plain[8..12]);
        frame.extend_from_slice(&plain[HDRLEN..real - HWEXT_LEN]);
        frame.resize(usize::from(total), 0);
        frame
    }

    #[test]
    fn saying_yes_to_glomming_moves_the_header_the_host_sends() {
        let mut chip = Sdpcm::new();
        chip.write(&glom_request(0, 1, C_GET_VERSION, false, &[0; 4], 0));
        chip.write_end();
        assert!(!chip.frame_waiting(), "not glomming yet");

        chip.write(&request(
            1,
            2,
            C_SET_VAR,
            true,
            &named("bus:rxglom", &1u32.to_le_bytes()),
        ));
        chip.write_end();
        read_frame(&mut chip).expect("the acknowledgement");

        let mut iovar = named("cur_etheraddr", &[]);
        iovar.resize(iovar.len() + 6, 0);
        chip.write(&glom_request(2, 3, C_GET_VAR, false, &iovar, 20));
        chip.write_end();
        let (_, payload) = read_frame(&mut chip).expect("a frame");
        assert_eq!(bcdc_parts(&payload).2 >> 16, 3);
        assert_eq!(&payload[BCDC_HDRLEN..BCDC_HDRLEN + 6], CHIP_MAC);
        let mut plain = [0u8; 64];
        chip.read(&mut plain);
        assert!(plain.iter().all(|b| *b == 0), "and nothing is left over");
    }

    #[test]
    fn a_frame_split_across_two_commands_waits_for_the_rest() {
        let mut chip = Sdpcm::new();
        let mut iovar = named("clmload", &[]);
        iovar.resize(iovar.len() + 1400, 0);
        let frame = request(0, 1, C_SET_VAR, true, &iovar);
        let cut = frame.len() / 512 * 512;
        chip.write(&frame[..cut]);
        chip.write_end();
        assert!(!chip.frame_waiting(), "half a frame answers nothing");
        chip.write(&frame[cut..]);
        chip.write_end();

        let (_, payload) = read_frame(&mut chip).expect("a frame");
        assert_eq!(bcdc_parts(&payload).2 >> 16, 1);
    }

    #[test]
    fn a_frame_on_a_channel_with_no_radio_behind_it_goes_nowhere() {
        let mut chip = Sdpcm::new();
        let mut frame = request(0, 1, C_GET_VERSION, false, &[]);
        frame[5] = 0x02;
        chip.write(&frame);
        chip.write_end();
        assert!(!chip.frame_waiting());
        assert_eq!(chip.data_frames_in(), 1);
        chip.write(&request(1, 1, C_GET_VERSION, false, &[0; 4]));
        chip.write_end();
        assert_eq!(
            read_frame(&mut chip).expect("a frame").0.channel,
            CHANNEL_CONTROL
        );
    }

    #[test]
    fn reading_past_a_frame_gets_zeros_and_then_the_next_frame() {
        let mut chip = Sdpcm::new();
        for i in 0..2u8 {
            chip.write(&request(i, u16::from(i) + 1, C_GET_VERSION, false, &[0; 4]));
            chip.write_end();
        }
        let mut first = [0u8; 64];
        chip.read(&mut first);
        let hd = parse_header(&first, false).unwrap();
        assert!(hd.len <= 64, "this answer fits in the first read");
        let (hd, payload) = read_frame(&mut chip).expect("the second frame");
        assert_eq!(hd.seq, 1);
        assert_eq!(bcdc_parts(&payload).2 >> 16, 2);
        let mut after = [0xFFu8; 64];
        chip.read(&mut after);
        assert!(after.iter().all(|b| *b == 0));
    }

    #[test]
    fn a_long_answer_comes_back_over_two_reads() {
        let mut chip = Sdpcm::new();
        let mut iovar = named("chanspecs", &[]);
        iovar.resize(iovar.len() + 1024, 0);
        chip.write(&request(0, 1, C_GET_VAR, false, &iovar));
        chip.write_end();

        let (hd, payload) = read_frame(&mut chip).expect("a frame");
        assert!(hd.len > 64, "longer than the driver's first read");
        let body = &payload[BCDC_HDRLEN..];
        let count = u32::from_le_bytes(body[..4].try_into().unwrap());
        assert_eq!(count as usize, CHANNELS_2G.count());
        for i in 0..count as usize {
            let at = 4 + i * 4;
            let spec = u32::from_le_bytes(body[at..at + 4].try_into().unwrap());
            assert_eq!(spec & 0x3800, CHSPEC_BW_20, "20 MHz");
            assert_eq!(spec & 0xC000, 0, "2.4 GHz");
            assert!(CHANNELS_2G.contains(&(spec & 0xFF)));
        }
        assert!(!chip.frame_waiting());
    }

    #[test]
    fn a_channel_list_the_chip_has_nothing_for_comes_back_empty() {
        let mut chip = Sdpcm::new();
        // A 40 MHz query must answer only 40 MHz channels, or the driver
        // `WARN_ON`s each one.
        let mut iovar = named("chanspecs", &0x1800u16.to_le_bytes());
        iovar.resize(iovar.len() + 256, 0);
        chip.write(&request(0, 1, C_GET_VAR, false, &iovar));
        chip.write_end();
        let (_, payload) = read_frame(&mut chip).expect("a frame");
        let body = &payload[BCDC_HDRLEN..];
        assert_eq!(u32::from_le_bytes(body[..4].try_into().unwrap()), 0);
    }

    #[test]
    fn the_channel_the_chip_says_it_is_on_is_one_it_offers() {
        let mut chip = Sdpcm::new();
        let mut iovar = named("chanspec", &[]);
        iovar.resize(iovar.len() + 4, 0);
        chip.write(&request(0, 1, C_GET_VAR, false, &iovar));
        chip.write_end();
        let (_, payload) = read_frame(&mut chip).expect("a frame");
        let spec = u32::from_le_bytes(payload[BCDC_HDRLEN..BCDC_HDRLEN + 4].try_into().unwrap());
        assert_eq!(spec & CHSPEC_BW_MASK, CHSPEC_BW_20);
        assert_eq!(spec & CHSPEC_BAND_MASK, CHSPEC_BAND_2G);
        assert!(
            CHANNELS_2G.contains(&(spec & 0xFF)),
            "not a channel it offers"
        );
    }

    #[test]
    fn the_band_list_names_the_band_the_channels_are_in() {
        let mut chip = Sdpcm::new();
        chip.write(&request(0, 1, C_GET_BANDLIST, false, &[0u8; 12]));
        chip.write_end();
        let (_, payload) = read_frame(&mut chip).expect("a frame");
        let body = &payload[BCDC_HDRLEN..];
        let word = |i: usize| u32::from_le_bytes(body[i..i + 4].try_into().unwrap());
        assert_eq!(word(0), 1, "one band");
        assert_eq!(word(4), WLC_BAND_2G);
    }

    #[test]
    fn an_address_the_driver_sets_is_the_one_it_reads_back() {
        let mut chip = Sdpcm::new();
        let wanted = [0x02, 0xAB, 0xCD, 0xEF, 0x00, 0x11];
        chip.write(&request(
            0,
            1,
            C_SET_VAR,
            true,
            &named("cur_etheraddr", &wanted),
        ));
        chip.write_end();
        read_frame(&mut chip).expect("the acknowledgement");

        assert_eq!(read_mac(&mut chip, 1), wanted);
        assert_eq!(chip.mac(), wanted);
        assert_eq!(chip.mac_source(), MacSource::Host);
    }

    #[test]
    fn the_chip_answers_its_own_address_until_something_overrides_it() {
        let mut chip = Sdpcm::new();
        assert_eq!(chip.mac(), CHIP_MAC);
        assert_eq!(chip.mac_source(), MacSource::Otp);
        assert_eq!(read_mac(&mut chip, 0), CHIP_MAC);

        // Unicast, not all zero, and not the address the driver throws away
        // for a random one, which would differ every run.
        assert_eq!(CHIP_MAC[0] & 1, 0);
        assert!(CHIP_MAC.iter().any(|b| *b != 0));
        assert_ne!(CHIP_MAC, [0x00, 0x90, 0x4C, 0xC5, 0x12, 0x38]);
    }

    #[test]
    fn the_cards_nvram_overrides_the_address_the_chip_came_up_with() {
        let mut chip = Sdpcm::new();
        // The Pi 4B nvram the card carries has this line
        // (`brcmfmac43455-sdio.txt`, RPi-Distro/firmware-nonfree), a couple
        // of hundred entries in.
        chip.start(&nvram(&[
            "sromrev=11",
            "macaddr=b8:27:eb:74:f2:6c",
            "boardtype=0x6e4",
        ]));
        let wanted = [0xB8, 0x27, 0xEB, 0x74, 0xF2, 0x6C];
        assert_eq!(chip.mac(), wanted);
        assert_eq!(chip.mac_source(), MacSource::Nvram);
        assert_eq!(read_mac(&mut chip, 0), wanted);

        // ...and the host may still write over it, which is what
        // `brcmf_c_set_cur_etheraddr` (`common.c:230`) does with an address
        // the platform handed down.
        let host = [0x02, 0x00, 0x5E, 0xAA, 0xF9, 0xAB];
        chip.write(&request(
            1,
            1,
            C_SET_VAR,
            true,
            &named("cur_etheraddr", &host),
        ));
        chip.write_end();
        read_frame(&mut chip).expect("the acknowledgement");
        assert_eq!(chip.mac_source(), MacSource::Host);
        assert_eq!(read_mac(&mut chip, 2), host);
    }

    #[test]
    fn nvram_with_no_address_in_it_leaves_the_chips_own() {
        // No `macaddr` line at all: the chip keeps what it came up with.
        let mut chip = Sdpcm::new();
        chip.start(&nvram(&["sromrev=11", "boardtype=0x6e4", "nocrc=1"]));
        assert_eq!(chip.mac(), CHIP_MAC);
        assert_eq!(chip.mac_source(), MacSource::Otp);
        assert_eq!(read_mac(&mut chip, 0), CHIP_MAC);

        // Nor is a key that merely starts the same way one, and nor is a
        // value that is not six octets.
        for line in [
            "macaddress=02:00:5e:00:57:01",
            "macaddr=b8:27:eb:74:f2",
            "macaddr=02:00:5e:00:57:01:00",
            "macaddr=",
            "macaddr=not an address",
        ] {
            let mut chip = Sdpcm::new();
            chip.start(&nvram(&[line]));
            assert_eq!(chip.mac(), CHIP_MAC, "took {line:?}");
            assert_eq!(chip.mac_source(), MacSource::Otp);
        }

        // And nothing downloaded at all is nothing to read.
        let mut chip = Sdpcm::new();
        chip.start(&[]);
        assert_eq!(chip.mac_source(), MacSource::Otp);
    }

    #[test]
    fn an_unknown_command_is_answered_with_zeros_rather_than_an_error() {
        let mut chip = Sdpcm::new();
        let mut iovar = named("no_such_iovar", &[]);
        iovar.resize(iovar.len() + 32, 0);
        chip.write(&request(0, 1, C_GET_VAR, false, &iovar));
        chip.write_end();
        let (_, payload) = read_frame(&mut chip).expect("a frame");
        let (_, len, flags, status) = bcdc_parts(&payload);
        assert_eq!(flags & BCDC_ERROR, 0);
        assert_eq!(status, 0);
        assert_eq!(len as usize, iovar.len());
        assert!(payload[BCDC_HDRLEN..].iter().all(|b| *b == 0));
    }

    // --- the event channel ---------------------------------------------

    /// How long the mask the driver hands this chip is:
    /// `DIV_ROUND_UP(fweh->num_event_codes, 8)` (`brcmf_fweh_attach`,
    /// `fweh.c:356`) with `num_event_codes` = `BRCMF_CYW_E_LAST` = 197
    /// (`brcmf_cyw_alloc_fweh_info`, `cyw/core.c:77`).
    const CYW_MASK_LEN: usize = 25;

    /// A mask with those firmware event codes turned on, the way `setbit` lays
    /// them out.
    fn mask_of(codes: &[u32]) -> Vec<u8> {
        let mut mask = vec![0u8; CYW_MASK_LEN];
        for code in codes {
            mask[*code as usize / 8] |= 1 << (code % 8);
        }
        mask
    }

    /// Set `event_msgs` the way `brcmf_fweh_activate_events` does, and take the
    /// acknowledgement.
    fn set_event_msgs(chip: &mut Sdpcm, seq: u8, mask: &[u8]) {
        chip.write(&request(
            seq,
            1,
            C_SET_VAR,
            true,
            &named("event_msgs", mask),
        ));
        chip.write_end();
        read_frame(chip).expect("the acknowledgement");
    }

    /// Set `event_msgs_ext` the way `brcmf_cyw_activate_events` does: the four-
    /// byte header and then the mask.
    fn set_event_msgs_ext(chip: &mut Sdpcm, seq: u8, command: u8, mask: &[u8]) {
        let mut value = vec![EVENTMSGS_VER, command, mask.len() as u8, 0];
        value.extend_from_slice(mask);
        chip.write(&request(
            seq,
            1,
            C_SET_VAR,
            true,
            &named("event_msgs_ext", &value),
        ));
        chip.write_end();
        read_frame(chip).expect("the acknowledgement");
    }

    /// Read an iovar back, as `brcmf_fil_iovar_data_get` does: the name and a
    /// buffer of the size wanted, and the answer's first `room` bytes are what
    /// it keeps.
    fn get_iovar(chip: &mut Sdpcm, seq: u8, name: &str, room: usize) -> Vec<u8> {
        let mut iovar = named(name, &[]);
        iovar.resize(iovar.len() + room, 0);
        chip.write(&request(seq, 1, C_GET_VAR, false, &iovar));
        chip.write_end();
        let (_, payload) = read_frame(chip).expect("a frame");
        payload[BCDC_HDRLEN..BCDC_HDRLEN + room].to_vec()
    }

    /// An event frame taken apart the way the driver takes one apart:
    /// `brcmf_proto_bcdc_hdrpull` pops the BCDC header, `eth_type_trans` the
    /// ether header, `brcmf_fweh_process_skb` checks Broadcom's, and
    /// `brcmf_fweh_event_worker` byte-swaps the message.
    fn parse_event(payload: &[u8]) -> Event {
        // The BCDC data header.
        assert!(payload.len() > BCDC_DATA_HDRLEN, "rx data too short");
        assert_eq!(
            payload[0] & 0xF0,
            BCDC_DATA_FLAGS,
            "non-BCDC packet received"
        );
        assert_eq!(payload[3], 0, "firmware signalling nothing can parse");
        let packet = &payload[BCDC_DATA_HDRLEN..];

        // The ether header: addressed to the interface, and the protocol
        // `brcmf_fweh_process_skb` insists on.
        assert!(packet.len() >= EVENT_HDRLEN, "shorter than a brcmf_event");
        assert_eq!(
            u16::from_be_bytes(packet[12..14].try_into().unwrap()),
            ETH_P_LINK_CTL
        );

        // Broadcom's header: the OUI and the subtype are both checked.
        assert_eq!(packet[19..22], BRCM_OUI);
        assert_eq!(
            u16::from_be_bytes(packet[22..24].try_into().unwrap()),
            BCM_SUBTYPE_EVENT
        );

        let msg = &packet[24..];
        let be16 = |i: usize| u16::from_be_bytes(msg[i..i + 2].try_into().unwrap());
        let be32 = |i: usize| u32::from_be_bytes(msg[i..i + 4].try_into().unwrap());
        let datalen = be32(20) as usize;
        assert!(
            datalen <= msg.len() - 48,
            "datalen runs past the end of the frame"
        );
        Event {
            code: be32(4),
            flags: be16(2),
            status: be32(8),
            reason: be32(12),
            addr: msg[24..30].try_into().unwrap(),
            ifidx: msg[46],
            bsscfgidx: msg[47],
            data: msg[48..48 + datalen].to_vec(),
        }
    }

    #[test]
    fn the_chip_answers_with_the_event_mask_the_host_set() {
        let mut chip = Sdpcm::new();
        // Nothing yet: the chip came up quiet, which is what
        // `brcmf_c_preinit_dcmds` (`common.c:420`) reads first.
        assert_eq!(chip.event_mask_source(), EventMaskSource::Firmware);
        assert!(get_iovar(&mut chip, 0, "event_msgs", CYW_MASK_LEN)
            .iter()
            .all(|b| *b == 0));

        // ...then it sets the one bit it always sets.
        let wanted = mask_of(&[E_IF]);
        set_event_msgs(&mut chip, 1, &wanted);
        assert_eq!(chip.event_mask_source(), EventMaskSource::EventMsgs);
        assert_eq!(get_iovar(&mut chip, 2, "event_msgs", CYW_MASK_LEN), wanted);
        assert_eq!(chip.events_wanted(), vec![E_IF]);

        // On this chip the live mask comes through `event_msgs_ext`
        // instead, and it replaces what was there.
        let wanted = mask_of(&[0, 16, 54, 69, 189]);
        set_event_msgs_ext(&mut chip, 3, EVENTMSGS_SET_MASK, &wanted);
        assert_eq!(chip.event_mask_source(), EventMaskSource::EventMsgsExt);
        assert_eq!(chip.events_wanted(), vec![0, 16, 54, 69, 189]);
        // ...and either iovar reads it back.
        assert_eq!(get_iovar(&mut chip, 4, "event_msgs", CYW_MASK_LEN), wanted);
        let ext = get_iovar(
            &mut chip,
            5,
            "event_msgs_ext",
            EVENTMSGS_EXT_HDRLEN + CYW_MASK_LEN,
        );
        assert_eq!(ext[0], EVENTMSGS_VER);
        assert_eq!(usize::from(ext[2]), CYW_MASK_LEN, "the mask it holds");
        assert_eq!(ext[3], ext[2], "and as much of it as a get may have");
        assert_eq!(ext[EVENTMSGS_EXT_HDRLEN..], wanted);
    }

    #[test]
    fn event_msgs_ext_can_turn_single_bits_on_and_off() {
        let mut chip = Sdpcm::new();
        set_event_msgs_ext(&mut chip, 0, EVENTMSGS_SET_MASK, &mask_of(&[16, 54]));
        set_event_msgs_ext(&mut chip, 1, EVENTMSGS_SET_BIT, &mask_of(&[69]));
        assert_eq!(chip.events_wanted(), vec![16, 54, 69]);
        set_event_msgs_ext(&mut chip, 2, EVENTMSGS_RESET_BIT, &mask_of(&[16, 69]));
        assert_eq!(chip.events_wanted(), vec![54]);
        // A command the iovar does not have leaves the mask alone.
        set_event_msgs_ext(&mut chip, 3, 0, &mask_of(&[1, 2, 3]));
        assert_eq!(chip.events_wanted(), vec![54]);
    }

    #[test]
    fn the_interface_the_firmware_comes_up_with_is_announced_to_nobody() {
        // `Sdpcm::start` is the firmware starting, and a bsscfg coming into
        // existence is a `BRCMF_E_IF`. The host has set no mask at that
        // point — it is still downloading — so the event is raised and
        // thrown away, which is why `brcmfmac` never sees one for `wlan0`.
        let mut chip = Sdpcm::new();
        chip.start(&[]);
        assert!(!chip.frame_waiting());
        assert_eq!(chip.events_sent(), 0);
        assert_eq!(chip.events_dropped(), 1);
    }

    #[test]
    fn an_event_is_the_ethernet_frame_the_driver_takes_apart() {
        let mut chip = Sdpcm::new();
        chip.fw.event_mask = mask_of(&[E_IF]);
        chip.fw.start();
        chip.flush_events();
        assert!(chip.frame_waiting());
        assert_eq!(chip.events_sent(), 1);
        assert_eq!(chip.events_dropped(), 0);

        let (hd, payload) = read_frame(&mut chip).expect("a frame");
        assert_eq!(hd.channel, CHANNEL_EVENT);
        assert_eq!(hd.doffset, HDRLEN, "no padding in front of the payload");
        // Longer than the driver's first read, so the frame comes back over
        // two of them — which is the path `brcmf_sdio_readframes`
        // (`sdio.c:1872`) takes for anything but a control frame.
        assert!(hd.len > 64);

        let event = parse_event(&payload);
        assert_eq!(event.code, E_IF);
        assert_eq!(event.addr, CHIP_MAC, "the interface's own address");
        assert_eq!(event.ifidx, 0);
        assert_eq!(event.bsscfgidx, 0);
        // `struct brcmf_if_event` (`fweh.h:286`).
        assert_eq!(event.data, vec![0, E_IF_ADD, 0, 0, E_IF_ROLE_STA]);
        // `brcmf_fweh_handle_if_event` (`fweh.c:148`) ignores an event whose
        // `flags` carry `BRCMF_E_IF_FLAG_NOIF`; this one is a real
        // interface.
        assert_eq!(event.data[2] & 1, 0);
        assert!(!chip.frame_waiting());
    }

    #[test]
    fn an_event_waits_behind_whatever_the_chip_was_already_saying() {
        // The driver is blocked in `brcmf_sdio_bus_rxctl` (`sdio.c`) waiting
        // for an answer it matches by request id. An event that overtook the
        // answer would be one more frame to get through first, and on a
        // chip that raised one per command it would never catch up — so a
        // frame the firmware raises goes behind what it is already sending.
        let mut chip = Sdpcm::new();
        chip.fw.event_mask = mask_of(&[E_IF]);
        chip.write(&request(0, 9, C_GET_VERSION, false, &[0; 4]));
        chip.write_end();
        chip.fw.start();
        chip.flush_events();

        let (hd, payload) = read_frame(&mut chip).expect("the answer");
        assert_eq!(hd.channel, CHANNEL_CONTROL);
        assert_eq!(bcdc_parts(&payload).2 >> 16, 9);
        let (hd, payload) = read_frame(&mut chip).expect("the event");
        assert_eq!(hd.channel, CHANNEL_EVENT);
        assert_eq!(parse_event(&payload).code, E_IF);
        // One sequence of frames, whatever channel they are on: the driver
        // counts every frame the chip sends with the same counter
        // (`brcmf_sdio_hdparse`, `sdio.c:1462`).
        assert_eq!(hd.seq, 1);
    }

    // --- the scan -------------------------------------------------------

    /// An `escan` request the way `brcmf_run_escan` builds one: `struct
    /// brcmf_escan_params_le` with the version, the action and the sync id,
    /// then the scan parameters `brcmf_escan_prep` filled in.
    fn escan_request(version: u32, action: u16, sync_id: u16, channels: &[u32]) -> Vec<u8> {
        let fixed = if version == ESCAN_REQ_VERSION_V2 {
            SCAN_PARAMS_V2_FIXED
        } else {
            SCAN_PARAMS_FIXED
        };
        let mut value = Vec::new();
        value.extend_from_slice(&version.to_le_bytes());
        value.extend_from_slice(&action.to_le_bytes());
        value.extend_from_slice(&sync_id.to_le_bytes());
        value.resize(ESCAN_PARAMS_HDRLEN + fixed, 0);
        // `channel_num`: the count in the low half, the SSID count in the
        // high one. `iw` scans with one wildcard SSID.
        let at = ESCAN_PARAMS_HDRLEN + fixed - 4;
        let channel_num = (1u32 << 16) | channels.len() as u32;
        value[at..at + 4].copy_from_slice(&channel_num.to_le_bytes());
        for ch in channels {
            value.extend_from_slice(&((CHSPEC_BW_20 | ch) as u16).to_le_bytes());
        }
        // The wildcard `struct brcmf_ssid_le`, four-byte aligned behind the
        // channel list.
        value.resize(value.len().next_multiple_of(4) + 36, 0);
        value
    }

    /// Ask for a scan and take the acknowledgement.
    fn scan(chip: &mut Sdpcm, seq: u8, channels: &[u32]) {
        let value = escan_request(ESCAN_REQ_VERSION_V2, ESCAN_ACTION_START, 0x1234, channels);
        chip.write(&request(seq, 1, C_SET_VAR, true, &named("escan", &value)));
        chip.write_end();
        let (hd, payload) = read_frame(chip).expect("the acknowledgement");
        assert_eq!(hd.channel, CHANNEL_CONTROL);
        assert_eq!(bcdc_parts(&payload).3, 0, "the set failed");
        assert!(!chip.frame_waiting(), "a scan that answered instantly");
    }

    /// Let the whole of a scan of every channel go by.
    fn scan_finishes(chip: &mut Sdpcm) {
        let at = chip.fw.now_us + CHANNELS_2G.count() as u64 * SCAN_DWELL_US;
        chip.advance_to(at);
    }

    /// The information elements of a result, as `(id, body)` pairs — the way
    /// cfg80211 walks them, and the only place the network's name is.
    fn elements(ies: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        let mut at = 0;
        while at + 2 <= ies.len() {
            let (id, len) = (ies[at], usize::from(ies[at + 1]));
            assert!(at + 2 + len <= ies.len(), "element runs past the end");
            out.push((id, ies[at + 2..at + 2 + len].to_vec()));
            at += 2 + len;
        }
        assert_eq!(at, ies.len(), "a trailing byte that is not an element");
        out
    }

    /// One `struct brcmf_escan_result_le`, checked the way
    /// `brcmf_cfg80211_escan_handler` checks one and then taken apart the way
    /// `brcmf_inform_single_bss` takes it apart.
    struct Result {
        bssid: [u8; 6],
        capability: u16,
        beacon_period: u16,
        channel: u8,
        rssi: i16,
        ies: Vec<(u8, Vec<u8>)>,
    }

    fn parse_result(data: &[u8]) -> Result {
        let le16 = |i: usize| u16::from_le_bytes(data[i..i + 2].try_into().unwrap());
        let le32 = |i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap());

        // `e->datalen < sizeof(*escan_result_le)` is the first thing the
        // handler refuses.
        assert!(
            data.len() >= ESCAN_RESULTS_FIXED + BSS_INFO_LEN,
            "invalid event data length"
        );
        let buflen = le32(0) as usize;
        assert!(buflen <= 65000, "invalid escan buffer length");
        assert!(buflen <= data.len(), "buflen past the end of the event");
        assert!(buflen >= ESCAN_RESULTS_FIXED + BSS_INFO_LEN);
        assert_eq!(le16(10), 1, "invalid bss_count");

        let bi = &data[ESCAN_RESULTS_FIXED..];
        let w16 = |i: usize| u16::from_le_bytes(bi[i..i + 2].try_into().unwrap());
        let w32 = |i: usize| u32::from_le_bytes(bi[i..i + 4].try_into().unwrap());
        // `brcmf_inform_bss` (`cfg80211.c:3442`) refuses the whole scan over
        // this one.
        assert_eq!(w32(0), BSS_INFO_VERSION, "!= WL_BSS_INFO_VERSION");
        let bi_length = w32(4) as usize;
        assert_eq!(
            bi_length,
            buflen - ESCAN_RESULTS_FIXED,
            "ignoring invalid bss_info length"
        );
        assert!(bi_length <= 2048, "bss info is larger than buffer");
        let capability = w16(16);
        assert_eq!(capability & 0x0002, 0, "ignoring IBSS result");

        let ie_offset = usize::from(w16(116));
        let ie_length = w32(120) as usize;
        assert!(ie_offset + ie_length <= bi_length, "IEs past the record");

        Result {
            bssid: bi[8..14].try_into().unwrap(),
            capability,
            beacon_period: w16(14),
            channel: bi[88],
            rssi: w16(78) as i16,
            ies: elements(&bi[ie_offset..ie_offset + ie_length]),
        }
    }

    /// The events a scan left in the FIFO, as `(status, data)`.
    fn scan_events(chip: &mut Sdpcm) -> Vec<(u32, Vec<u8>)> {
        let mut out = Vec::new();
        while chip.frame_waiting() {
            let (hd, payload) = read_frame(chip).expect("a frame");
            assert_eq!(hd.channel, CHANNEL_EVENT);
            let event = parse_event(&payload);
            assert_eq!(event.code, E_ESCAN_RESULT);
            assert_eq!(event.bsscfgidx, 0, "not the primary interface");
            out.push((event.status, event.data));
        }
        out
    }

    /// A chip with the mask the driver really sets on this part, so that the
    /// scan's events are wanted.
    fn scanning_chip() -> Sdpcm {
        let mut chip = Sdpcm::new();
        chip.fw.event_mask = mask_of(&[E_IF, E_ESCAN_RESULT]);
        chip
    }

    #[test]
    fn a_scan_is_answered_with_a_result_for_each_network_and_then_a_completion() {
        let mut chip = scanning_chip();
        assert_eq!(chip.escans(), 0);
        scan(&mut chip, 0, &[]);
        assert_eq!(chip.escans(), 1);
        assert_eq!(chip.escan_results(), NETWORKS.len() as u32);
        scan_finishes(&mut chip);

        let events = scan_events(&mut chip);
        assert_eq!(events.len(), NETWORKS.len() + 1, "a result each and an end");
        // Every network first, as a partial result...
        for (event, bss) in events.iter().zip(NETWORKS.iter()) {
            assert_eq!(event.0, E_STATUS_PARTIAL);
            assert_eq!(parse_result(&event.1).bssid, bss.bssid);
        }
        // ...and then the scan ending, which is what
        // `brcmf_notify_escan_complete` is reached through. Anything but
        // `SUCCESS` would be an aborted scan (`cfg80211.c:3744`), and the
        // handler reads no data off it.
        let (status, data) = events.last().unwrap();
        assert_eq!(*status, E_STATUS_SUCCESS);
        assert!(data.is_empty());
        assert_eq!(chip.events_sent(), NETWORKS.len() as u32 + 1);
    }

    #[test]
    fn a_result_carries_the_network_the_model_invented() {
        let mut chip = scanning_chip();
        scan(&mut chip, 0, &[]);
        scan_finishes(&mut chip);
        let events = scan_events(&mut chip);
        let bss = &NETWORKS[0];
        let result = parse_result(&events[0].1);

        assert_eq!(result.bssid, bss.bssid);
        assert_eq!(result.channel, bss.channel as u8);
        assert_eq!(result.rssi, bss.rssi);
        assert_eq!(result.beacon_period, BSS_BEACON_PERIOD);
        // An infrastructure network, and an open one: the `PRIVACY` bit next
        // to `ESS` is clear.
        assert_eq!(result.capability, BSS_CAPABILITY);
        assert_eq!(result.capability & 0x0010, 0, "not an open network");

        // The address is invented the way the chip's own is: locally
        // administered, unicast, and not any network's.
        assert_eq!(bss.bssid[0] & 0x03, 0x02);
        assert_ne!(bss.bssid, CHIP_MAC);
        // And on a channel the chip told cfg80211 about, which is the only
        // kind it can place.
        assert!(CHANNELS_2G.contains(&bss.channel));
    }

    #[test]
    fn the_name_is_in_the_elements_where_cfg80211_looks_for_it() {
        let mut chip = scanning_chip();
        scan(&mut chip, 0, &[]);
        scan_finishes(&mut chip);
        let events = scan_events(&mut chip);
        let result = parse_result(&events[0].1);
        let bss = &NETWORKS[0];

        let ids: Vec<u8> = result.ies.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            vec![EID_SSID, EID_SUPP_RATES, EID_DS_PARAMS, EID_EXT_SUPP_RATES]
        );
        let ssid = &result.ies[0].1;
        assert_eq!(
            std::str::from_utf8(ssid).unwrap(),
            bss.ssid,
            "the name `iw` prints"
        );
        assert!(
            !ssid.is_empty(),
            "an empty SSID is what a wrong offset gives"
        );
        assert!(ssid.len() <= 32, "longer than IEEE80211_MAX_SSID_LEN");
        // The DS Parameter Set has to agree with `ctl_ch`, or `iw` prints
        // one channel and cfg80211 files the network under another.
        assert_eq!(result.ies[2].1, vec![bss.channel as u8]);
        // Every rate, split the way 802.11 splits them.
        let mut rates = result.ies[1].1.clone();
        rates.extend_from_slice(&result.ies[3].1);
        assert_eq!(rates, BSS_RATES);
        assert!(result.ies[1].1.len() <= SUPP_RATES_MAX);
    }

    #[test]
    fn a_scan_of_channels_the_network_is_not_on_finds_nothing() {
        // `iw dev wlan0 scan freq ...` names channels, and
        // `brcmf_escan_prep` (`cfg80211.c:1136`) puts them in the request.
        let mut chip = scanning_chip();
        let elsewhere: Vec<u32> = CHANNELS_2G
            .filter(|ch| !NETWORKS.iter().any(|bss| bss.channel == *ch))
            .collect();
        scan(&mut chip, 0, &elsewhere);
        assert_eq!(chip.escans(), 1, "the request was still a scan");
        assert_eq!(chip.escan_results(), 0);

        // ...but the completion still comes, or the host waits out the
        // ten-second timeout for nothing — and it comes when the chip has
        // been round every channel it was given, not before.
        let end = chip.fw.now_us + elsewhere.len() as u64 * SCAN_DWELL_US;
        chip.advance_to(end - 1);
        assert!(!chip.frame_waiting(), "the scan is not over yet");
        chip.advance_to(end);
        let events = scan_events(&mut chip);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, E_STATUS_SUCCESS);

        // And the channel the network is on finds it again.
        scan(&mut chip, 1, &[NETWORKS[0].channel]);
        assert_eq!(chip.escan_results(), 1);
        chip.advance_to(chip.fw.now_us + SCAN_DWELL_US);
        assert_eq!(scan_events(&mut chip).len(), 2);
    }

    #[test]
    fn the_channel_list_is_read_out_of_whichever_shape_the_request_has() {
        // `brcmf_run_escan` (`cfg80211.c:1472`) converts the parameters down
        // to the first shape when `BRCMF_FEAT_SCAN_V2` is off, and the two
        // put `channel_num` eight bytes apart.
        for version in [1, ESCAN_REQ_VERSION_V2] {
            let wanted = vec![NETWORKS[0].channel];
            let params = escan_request(version, ESCAN_ACTION_START, 0, &wanted);
            assert_eq!(
                escan_channels(version, &params[ESCAN_PARAMS_HDRLEN..]),
                Some(wanted),
                "version {version}"
            );
            // A count of zero is every channel, which is the shape an abort
            // and a whole-band scan both have.
            let all = escan_request(version, ESCAN_ACTION_START, 0, &[]);
            assert_eq!(escan_channels(version, &all[ESCAN_PARAMS_HDRLEN..]), None);
        }
        // Nothing to read is not a channel list either.
        assert_eq!(escan_channels(ESCAN_REQ_VERSION_V2, &[]), None);
    }

    #[test]
    fn a_scan_the_host_is_not_listening_for_is_raised_and_dropped() {
        // The event channel is still the mask's to allow: a host that never
        // registered the handler never set the bit, and a firmware that sent
        // the results anyway would be sending frames nothing reads.
        let mut chip = Sdpcm::new();
        chip.fw.event_mask = mask_of(&[E_IF]);
        scan(&mut chip, 0, &[]);
        scan_finishes(&mut chip);
        assert!(!chip.frame_waiting());
        assert_eq!(chip.escans(), 1);
        assert_eq!(chip.events_sent(), 0);
        assert_eq!(chip.events_dropped(), NETWORKS.len() as u32 + 1);
    }

    #[test]
    fn only_a_start_is_a_scan() {
        // `WL_ESCAN_ACTION_ABORT` is what `brcmf_notify_escan_complete`
        // (`cfg80211.c:1208`) sends to call a running scan off, and nothing
        // is ever running.
        let mut chip = scanning_chip();
        for action in [2u16, 3] {
            let value = escan_request(ESCAN_REQ_VERSION_V2, action, 0x1234, &[]);
            chip.write(&request(0, 1, C_SET_VAR, true, &named("escan", &value)));
            chip.write_end();
            read_frame(&mut chip).expect("the acknowledgement");
            assert!(!chip.frame_waiting(), "action {action} answered");
        }
        assert_eq!(chip.escans(), 0);
        // And a request too short to have a header in it is not one either.
        chip.write(&request(0, 1, C_SET_VAR, true, &named("escan", &[0, 0])));
        chip.write_end();
        read_frame(&mut chip).expect("the acknowledgement");
        assert_eq!(chip.escans(), 0);
    }

    #[test]
    fn a_scan_takes_a_dwell_on_every_channel_it_was_given() {
        // Answering the instant the request lands is what makes `iw dev
        // wlan0 scan` hang: it opens the socket it waits for the completion
        // on only after the trigger is acknowledged, so the completion has
        // to come later than that. It also has to come at all, or the ten
        // second timeout in the driver is what ends the scan.
        let mut chip = scanning_chip();
        chip.advance_to(5_000_000);
        scan(&mut chip, 0, &[]);
        let start = 5_000_000;

        // The network is on the first channel, so its result comes after one
        // dwell and the completion only after the last.
        let bss = &NETWORKS[0];
        let at = CHANNELS_2G
            .clone()
            .position(|ch| ch == bss.channel)
            .expect("a channel the chip offers");
        chip.advance_to(start + (at as u64 + 1) * SCAN_DWELL_US - 1);
        assert!(!chip.frame_waiting(), "the channel's dwell is not over");
        chip.advance_to(start + (at as u64 + 1) * SCAN_DWELL_US);
        let (hd, payload) = read_frame(&mut chip).expect("the result");
        assert_eq!(hd.channel, CHANNEL_EVENT);
        assert_eq!(parse_event(&payload).status, E_STATUS_PARTIAL);

        let end = start + CHANNELS_2G.count() as u64 * SCAN_DWELL_US;
        chip.advance_to(end - 1);
        assert!(!chip.frame_waiting(), "the scan is not over");
        assert!(chip.advance_to(end), "and then it is");
        let (_, payload) = read_frame(&mut chip).expect("the completion");
        assert_eq!(parse_event(&payload).status, E_STATUS_SUCCESS);
        // Nothing left to deliver, however far time runs on.
        assert!(!chip.advance_to(end + 60_000_000));
    }

    #[test]
    fn a_result_fits_in_one_frame_the_driver_will_read() {
        // `brcmf_sdio_hdparse` (`sdio.c:1435`) drops a frame longer than
        // `MAX_RX_DATASZ` on any channel but the control one, and an event
        // frame is what a scan result travels in.
        let mut chip = scanning_chip();
        scan(&mut chip, 0, &[]);
        scan_finishes(&mut chip);
        while chip.frame_waiting() {
            let mut first = [0u8; 64];
            chip.read(&mut first);
            let hd = parse_header(&first, false).expect("an event frame");
            assert!(hd.len <= 2048, "longer than MAX_RX_DATASZ");
            // ...and long enough to need the second read, which is the path
            // every frame but a control one takes.
            let mut rest = vec![0u8; (hd.len - first.len()).next_multiple_of(4)];
            chip.read(&mut rest);
        }
    }

    #[test]
    fn the_window_an_event_carries_is_the_one_the_host_is_sending_in() {
        // An event is not an answer, so it arrives without the host having
        // sent anything — and it still has to open the window, because
        // `data_ok` / `txctl_ok` (`sdio.c:690`) stop the driver the moment
        // the last frame it saw closed it.
        let mut chip = Sdpcm::new();
        chip.fw.event_mask = mask_of(&[E_IF]);
        let mut tx_seq: u8 = 255;
        for i in 0..4u8 {
            chip.write(&request(
                tx_seq,
                u16::from(i) + 1,
                C_SET_VAR,
                true,
                &named("mpc", &[1]),
            ));
            chip.write_end();
            tx_seq = tx_seq.wrapping_add(1);
            read_frame(&mut chip).expect("the acknowledgement");

            chip.fw.start();
            chip.flush_events();
            let (hd, _) = read_frame(&mut chip).expect("the event");
            assert_eq!(hd.channel, CHANNEL_EVENT);
            let room = chip.host_seq.wrapping_add(TX_WINDOW).wrapping_sub(tx_seq);
            assert_ne!(room, 0, "the window closed at event {i}");
            assert_eq!(room & 0x80, 0);
        }
    }
}
