//! What the CYW43455's own firmware does, as far as the host can see it: the
//! frame protocol over SDIO function 2 and the control channel inside it.
//!
//! [`Cyw43455`](super::cyw43455::Cyw43455) is the chip's bus, and the image
//! the driver downloads over it lands in memory nothing in the model executes.
//! This is the other half: the ARM the driver releases would run a firmware
//! that talks **SDPCM** to the host — a frame protocol with its own header on
//! function 2 — and answers **BCDC** commands on SDPCM's control channel.
//! Neither protocol has anything to do with the backplane; a frame is a CMD53
//! on function 2, and function 2 is a FIFO, not an address space.
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
//!   ...and once the chip has said it can glom, everything the host sends
//!   has eight more bytes between the two headers:
//!
//!   0    2    4             12            20
//!   +----+----+-------------+-------------+-------------- ...
//!   |len |~len| glom ext    | sw header   | payload
//!   +----+----+-------------+-------------+-------------- ...
//! ```
//!
//! Line references are to **raspberrypi/linux**
//! `16f1da3c4e94437449d6aa151589ca0ad4b388bb`, the kernel
//! `scripts/fetch-firmware.sh` pins, under
//! `drivers/net/wireless/broadcom/brcm80211/brcmfmac/`:
//! <https://github.com/raspberrypi/linux/blob/16f1da3c4e94437449d6aa151589ca0ad4b388bb/drivers/net/wireless/broadcom/brcm80211/brcmfmac/sdio.c>
//!
//! ### What the driver demands of a frame
//!
//! `brcmf_sdio_hdparse` (`sdio.c:1386`) is the specification for everything
//! the chip sends:
//!
//! 1. The four-byte hardware header is the frame length and its complement.
//!    All zero means "nothing more to read", which is how the driver stops
//!    reading (`rxpending`); anything whose halves do not complement is a
//!    checksum error.
//! 2. The eight-byte software header carries the sequence number the chip
//!    stamps, the channel, the offset the payload starts at, and — the part
//!    that matters most — the highest sequence number the host may still
//!    send. `data_ok` / `txctl_ok` (`sdio.c:690`) stop the driver dead once
//!    it has used its window up, so every frame has to open it again.
//! 3. The driver reads `BRCMF_FIRSTREAD` = 64 bytes first (`sdio.c:139`) and
//!    the rest of the frame after, rounded up to the function's block size.
//!    So a read past the end of a frame has to answer, and answer zeros.
//!
//! ### What the control channel carries
//!
//! BCDC (`bcdc.c:25`): a 16-byte header — command, buffer length, flags,
//! status — and the buffer. `[1]` of the flags is set for a set and clear for
//! a get, `[31:16]` is a request id the driver matches the answer against, and
//! `[0]` of the answer says the command failed. Commands 262 and 263
//! (`fwil.h:80`) are "get" and "set a named variable", whose buffer starts
//! with the name as a NUL-terminated string.
//!
//! ### What this is not
//!
//! A radio. The responder below answers what `brcmf_bus_started`
//! (`core.c`) asks on the way to registering a network interface, and its
//! answers are the model's own — a firmware version that says so, one band,
//! and a MAC address that is a placeholder. Nothing here scans, associates or
//! carries a packet; a frame on the data or event channels is dropped.

use std::collections::VecDeque;

/// `SDPCM_HWHDR_LEN` (`sdio.c:1345`): the frame length and its complement.
const HWHDR_LEN: usize = 4;
/// `SDPCM_HWEXT_LEN` (`sdio.c:1346`): the glom extension, which the host adds
/// between the hardware and the software header once the chip has told it
/// frames may be glommed on the way up. Its first word carries the frame's
/// real length (minus the hardware header) in `[23:0]` and "last frame of the
/// glom" in `[24]`; its second, the padding the host added.
const HWEXT_LEN: usize = 8;
const HWEXT_LEN_MASK: u32 = 0x00FF_FFFF;
/// `SDPCM_SWHDR_LEN` (`sdio.c:1347`).
const SWHDR_LEN: usize = 8;
/// `SDPCM_HDRLEN` (`sdio.c:1348`), and the offset a payload starts at when
/// the chip adds no padding of its own.
const HDRLEN: usize = HWHDR_LEN + SWHDR_LEN;

/// Software-header fields (`sdio.c:1350`). The sequence number is the low
/// byte of the first word; the rest are masks into it and into the second.
const CHANNEL_SHIFT: u32 = 8;
const CHANNEL_MASK: u32 = 0x0000_0F00;
const DOFFSET_SHIFT: u32 = 24;
/// `SDPCM_WINDOW_MASK` (`sdio.c:1365`), in the second word: the highest
/// sequence number the host may send.
const WINDOW_SHIFT: u32 = 8;

/// `SDPCM_CONTROL_CHANNEL` (`sdio.c:1354`). The event and data channels
/// (1 and 2) exist, and nothing here sends or accepts one.
const CHANNEL_CONTROL: u8 = 0;

/// How far ahead of what it has already sent the chip lets the host run.
/// `brcmf_sdio_hdparse` (`sdio.c:1488`) rejects a window more than 0x40
/// frames wide as an error and clamps it to two; four is well inside that and
/// more than a synchronous request/answer exchange can use.
const TX_WINDOW: u8 = 4;

/// The most the chip will hold of a frame the host is still writing. The
/// driver's own ceiling is `MAX_DATA_BUF` = 32 KiB (`sdio.c:136`) and a
/// control frame is capped well below that by `bus_if->maxctl`.
const MAX_FRAME: usize = 64 * 1024;

/// BCDC's header (`bcdc.c:25`): command, buffer length, flags, status.
const BCDC_HDRLEN: usize = 16;
/// `BCDC_DCMD_ERROR` (`bcdc.c:34`): the answer's `status` is a firmware error
/// code.
const BCDC_ERROR: u32 = 0x01;
/// `BCDC_DCMD_SET` (`bcdc.c:35`): the request writes rather than reads.
const BCDC_SET: u32 = 0x02;

/// The commands this answers with something other than zeros (`fwil.h`).
const C_GET_VERSION: u32 = 1;
const C_GET_BANDLIST: u32 = 140;
const C_GET_VAR: u32 = 262;
const C_SET_VAR: u32 = 263;

/// `BRCMU_D11AC_IOTYPE` (`include/brcmu_d11.h:11`), which is what
/// `BRCMF_C_GET_VERSION` answers and what `brcmu_d11_attach`
/// (`brcmutil/d11.c`) needs to see before it will fit the channel-spec
/// encoders — a zero there leaves them null and the next channel walk
/// dereferences them.
const D11AC_IOTYPE: u32 = 2;

/// `WLC_BAND_2G` (`include/brcmu_wifi.h:94`), as `BRCMF_C_GET_BANDLIST`
/// reports it.
const WLC_BAND_2G: u32 = 2;

/// A 20 MHz channel spec in the 802.11ac encoding:
/// `BRCMU_CHSPEC_D11AC_BW_20` with `BRCMU_CHSPEC_D11AC_BND_2G` (zero) and the
/// channel number in the low byte (`include/brcmu_d11.h:76`).
const CHSPEC_BW_20: u32 = 0x1000;
/// The bandwidth and band fields a `chanspecs` query filters on.
const CHSPEC_BW_MASK: u32 = 0x3800;
const CHSPEC_BAND_MASK: u32 = 0xC000;
const CHSPEC_BAND_2G: u32 = 0x0000;
/// The channels the model's firmware offers, which have to be ones
/// `__wl_2ghz_channels` (`cfg80211.c`) knows: anything else is an
/// "unexpected firmware channel" the driver drops.
const CHANNELS_2G: std::ops::RangeInclusive<u32> = 1..=13;
/// The channel the model's firmware says it is on, which is the first one it
/// offers. `brcmf_cfg80211_get_channel` (`cfg80211.c:5728`) asks for it as
/// soon as the interface is registered and hands what comes back to
/// cfg80211, which warns loudly about a channel it cannot place — and a zero
/// is one.
const CHANSPEC_HOME: u32 = CHSPEC_BW_20 | 1;

/// The MAC address the model answers `cur_etheraddr` with.
///
/// A **placeholder**: locally administered, unicast, and not
/// `brcmf_default_mac_address` (`common.c`), which the driver replaces with a
/// random one. The real address comes out of the chip's OTP, with the card's
/// nvram `macaddr=` line overriding it, and neither is modelled yet.
const PLACEHOLDER_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];

/// What the model answers the `ver` iovar with. `brcmf_c_preinit_dcmds`
/// (`common.c:268`) prints it as the firmware version and keeps whatever
/// follows the last space as the number ethtool reports, so it has to have
/// one. It says what it is: no firmware ran to produce it.
const VERSION: &str = "rpi-virt-fw model firmware (no radio) version 0";

/// One end of the frame protocol: the FIFO behind function 2 in both
/// directions, and the firmware that answers on it.
pub struct Sdpcm {
    /// Frames the chip has for the host, oldest first.
    rx: VecDeque<Vec<u8>>,
    /// How much of the front frame the host has read.
    rx_pos: usize,
    /// The frame the host is writing, as the CMD53 blocks arrive.
    tx: Vec<u8>,
    /// The sequence number the next frame out of the chip carries.
    seq: u8,
    /// The sequence number of the last frame in, which is what the window the
    /// chip advertises is measured from.
    host_seq: u8,
    /// The host has been told the chip can glom frames on the way up, and
    /// puts the extra hardware header on everything it sends from then on.
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
            // The host starts at 255 (`brcmf_sdio_probe`, `sdio.c:4684`) and
            // the chip at 0; neither side checks the other's first number,
            // only that it keeps counting.
            seq: 0,
            host_seq: 0,
            glom: false,
            fw: Firmware::new(),
        }
    }

    /// A frame is waiting for the host, which is what the SDIO core raises
    /// `I_HMB_FRAME_IND` (`sdio.c:272`) to say.
    pub fn frame_waiting(&self) -> bool {
        !self.rx.is_empty()
    }

    /// The data phase of a CMD53 read on function 2. Function 2 is a FIFO, so
    /// the address the command named says nothing: the bytes come from where
    /// the last read left off, and a read past the end of a frame gets zeros
    /// and moves on to the next one.
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

    /// One block of a CMD53 write on function 2.
    pub fn write(&mut self, data: &[u8]) {
        // Nothing the driver sends comes near this — `MAX_DATA_BUF`
        // (`sdio.c:136`) is 32 KiB — so a buffer that grows past it is
        // bytes the chip never made sense of, and keeping them would only
        // corrupt the next frame.
        if self.tx.len() + data.len() > MAX_FRAME {
            self.tx.clear();
        }
        self.tx.extend_from_slice(data);
    }

    /// The CMD53 write ended.
    ///
    /// A frame is usually one command's worth: `brcmf_sdio_tx_ctrlframe`
    /// (`sdio.c:2412`) pads anything over the block size up to a whole number
    /// of blocks, and the MMC core sends that as a single block-mode command.
    /// But a length it cannot pad goes out as a block-mode command and a
    /// byte-mode remainder (`sdio_io_rw_ext_helper`), so what is here may be
    /// half a frame — in which case the rest is in the next command, and the
    /// bytes wait. What is past the length in the header is padding and goes
    /// nowhere.
    pub fn write_end(&mut self) {
        let min = if self.glom {
            HDRLEN + HWEXT_LEN
        } else {
            HDRLEN
        };
        match parse_header(&self.tx, self.glom) {
            // Not a header yet, or a frame the rest of which has not
            // arrived: wait for the next command.
            _ if self.tx.len() < min => {}
            Some(sw) if sw.total > self.tx.len() => {}
            Some(sw) => {
                let frame = std::mem::take(&mut self.tx);
                self.frame_in(&sw, &frame);
            }
            // A header that is not one. A real chip answers with a
            // write-out-of-sync interrupt; nothing in the model can produce
            // one, so the bytes are simply dropped.
            None => self.tx.clear(),
        }
    }

    /// Take a frame the host wrote, and answer it if it is one the model's
    /// firmware answers.
    fn frame_in(&mut self, sw: &Header, frame: &[u8]) {
        self.host_seq = sw.seq;
        if sw.channel != CHANNEL_CONTROL {
            return;
        }
        let reply = self.fw.control(&frame[sw.doffset..sw.len]);
        // Turning receive glomming on is the one thing an answer changes
        // about the protocol itself: from the next frame the host sends, the
        // hardware header has the glom extension on it.
        self.glom = self.fw.rxglom;
        self.push(CHANNEL_CONTROL, &reply);
    }

    /// Queue one frame for the host.
    fn push(&mut self, channel: u8, payload: &[u8]) {
        let len = HDRLEN + payload.len();
        let mut frame = Vec::with_capacity(len);
        let hw = len as u16;
        frame.extend_from_slice(&hw.to_le_bytes());
        frame.extend_from_slice(&(!hw).to_le_bytes());
        let first = u32::from(self.seq)
            | (u32::from(channel) << CHANNEL_SHIFT)
            // No read-ahead: the next frame's length is 0, so the driver reads
            // a fresh header for it.
            | ((HDRLEN as u32) << DOFFSET_SHIFT);
        frame.extend_from_slice(&first.to_le_bytes());
        // No flow control, and the window the host may send in.
        let second = u32::from(self.host_seq.wrapping_add(TX_WINDOW)) << WINDOW_SHIFT;
        frame.extend_from_slice(&second.to_le_bytes());
        frame.extend_from_slice(payload);
        self.seq = self.seq.wrapping_add(1);
        self.rx.push_back(frame);
    }
}

/// A frame's headers as [`Sdpcm::frame_in`] reads them.
struct Header {
    seq: u8,
    channel: u8,
    /// Where the payload ends.
    len: usize,
    /// Where it starts.
    doffset: usize,
    /// How many bytes of the command belong to this frame, padding included
    /// — which is what says whether the rest of it has arrived yet.
    total: usize,
}

/// Parse the twelve bytes at the head of a frame, the way
/// `brcmf_sdio_hdparse` (`sdio.c:1386`) parses one the chip wrote. `None` for
/// anything malformed — a real chip answers that with an out-of-sync
/// interrupt, and nothing in the model can produce one.
///
/// Only the headers are looked at: the caller has as much of the frame as it
/// has, which for the driver's first read is 64 bytes of a longer one.
///
/// With `glom` the host puts the glom extension between the hardware and the
/// software header (`brcmf_sdio_hdpack`, `sdio.c:1503`), and the hardware
/// header then carries the padded length while the extension carries the
/// real one.
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

/// The firmware behind the control channel: what the model answers the
/// driver's commands with.
struct Firmware {
    mac: [u8; 6],
    /// The host asked for receive glomming and was told yes, which changes
    /// the shape of everything it sends from then on.
    rxglom: bool,
}

impl Firmware {
    fn new() -> Firmware {
        Firmware {
            mac: PLACEHOLDER_MAC,
            rxglom: false,
        }
    }

    /// One BCDC request in, one BCDC answer out.
    ///
    /// The answer echoes the command and the flags — the driver matches the
    /// request id it put in the top half of the flags
    /// (`brcmf_proto_bcdc_cmplt`, `bcdc.c:140`) and gives up on an answer
    /// that does not carry it — and carries the data a get asked for.
    fn control(&mut self, req: &[u8]) -> Vec<u8> {
        if req.len() < BCDC_HDRLEN {
            return Vec::new();
        }
        let word = |i: usize| u32::from_le_bytes(req[i..i + 4].try_into().unwrap());
        let cmd = word(0);
        let len = word(4) as usize;
        let flags = word(8);
        let data = &req[BCDC_HDRLEN..];

        // A set is acknowledged with nothing; a get carries back exactly the
        // buffer the driver offered, so that what it copies out is defined
        // whatever it asked for.
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
        // status: no firmware error. Nothing the model answers can fail, so
        // the error flag above is never set either.
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// A command that writes. Every one of them is taken and none is acted
    /// on: there is no radio for them to configure. The one exception is the
    /// address the driver may hand down before it asks for it back.
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
            b"cur_etheraddr" if value.len() >= 6 => self.mac.copy_from_slice(&value[..6]),
            // `brcmf_sdio_bus_preinit` (`sdio.c:3724`) takes a yes here as
            // permission to use the glom header on everything it sends.
            b"bus:rxglom" => self.rxglom = word() != 0,
            _ => {}
        }
    }

    /// A command that reads, answering in the `len` bytes the driver offered.
    /// Anything not named here reads as zeros, which every caller in the
    /// driver treats as "the feature is off" rather than as a failure — the
    /// alternative, the error flag, makes `brcmf_bus_started` give up.
    fn get(&mut self, cmd: u32, len: usize, data: &[u8]) -> Vec<u8> {
        // A buffer the driver never asks more than a few kilobytes for; the
        // cap keeps a malformed length from allocating the machine's memory.
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
                        // The driver reads it as a C string.
                        put(&mut out, VERSION.len(), &[0]);
                    }
                    // The query carries a channel spec in the first
                    // halfword of its buffer, saying which channels to list.
                    b"chanspecs" => {
                        let filter = value
                            .get(..2)
                            .map(|v| u16::from_le_bytes(v.try_into().unwrap()))
                            .unwrap_or(0);
                        put(&mut out, 0, &chanspecs(filter));
                    }
                    b"chanspec" => put(&mut out, 0, &CHANSPEC_HOME.to_le_bytes()),
                    _ => {}
                }
            }
            _ => {}
        }
        out
    }
}

/// The name and value of an iovar request: the name is a NUL-terminated
/// string and the value is whatever follows it (`brcmf_create_iovar`,
/// `fwil.c`).
fn iovar(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let end = data.iter().position(|b| *b == 0)?;
    Some((&data[..end], &data[end + 1..]))
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

/// The channel list `brcmf_construct_chaninfo` (`cfg80211.c:7008`) walks: a
/// count and then one 32-bit channel spec each.
///
/// `filter` is the halfword the query arrives with. Zero asks for everything,
/// which is what `brcmf_construct_chaninfo` sends; anything else is a channel
/// spec whose band and bandwidth fields say which ones to list, which is how
/// `brcmf_enable_bw40_2g` (`cfg80211.c:7143`) asks for the 40 MHz channels.
/// Answering that one with 20 MHz channels earns a `WARN_ON` apiece, so a
/// firmware with nothing to offer has to answer with an empty list.
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

    /// A BCDC request the way `brcmf_proto_bcdc_msg` (`bcdc.c:109`) builds
    /// one, wrapped in the SDPCM header `brcmf_sdio_tx_ctrlframe`
    /// (`sdio.c:2412`) puts on it.
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
        // The host pads to the block size; the padding is not the frame.
        frame.resize(frame.len().next_multiple_of(64), 0);
        frame
    }

    /// An iovar request's payload: the name, a NUL, then the value.
    fn named(name: &str, value: &[u8]) -> Vec<u8> {
        let mut v = name.as_bytes().to_vec();
        v.push(0);
        v.extend_from_slice(value);
        v
    }

    /// Read one whole frame back the way `brcmf_sdio_readframes`
    /// (`sdio.c:1872`) does: 64 bytes for the header, then the rest of what
    /// the header said, rounded up. Hands back the parsed header and the
    /// payload.
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

    /// The header of a BCDC answer: command, length, flags, status.
    fn bcdc_parts(payload: &[u8]) -> (u32, u32, u32, u32) {
        let w = |i: usize| u32::from_le_bytes(payload[i..i + 4].try_into().unwrap());
        (w(0), w(4), w(8), w(12))
    }

    #[test]
    fn nothing_is_waiting_until_the_host_asks_for_something() {
        let mut chip = Sdpcm::new();
        assert!(!chip.frame_waiting());
        // An all-zero read is what tells the driver there is nothing there.
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
        // `brcmf_fil_iovar_data_get` asks with a buffer of the size it wants
        // back, the name in the front of it.
        let mut iovar = named("cur_etheraddr", &[]);
        iovar.resize(iovar.len() + 6, 0);
        chip.write(&request(0, 1, C_GET_VAR, false, &iovar));
        chip.write_end();

        let (_, payload) = read_frame(&mut chip).expect("a frame");
        let (_, len, _, _) = bcdc_parts(&payload);
        assert_eq!(len as usize, iovar.len());
        let mac = &payload[BCDC_HDRLEN..BCDC_HDRLEN + 6];
        assert_eq!(mac, PLACEHOLDER_MAC);
        // Valid as `is_valid_ether_addr` has it: a unicast address that is
        // not all zero.
        assert_eq!(mac[0] & 1, 0);
        assert!(mac.iter().any(|b| *b != 0));
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
        // `brcmf_c_preinit_dcmds` takes everything after the last space as
        // the version number and gives up if there is no space at all.
        let (_, number) = text.rsplit_once(' ').expect("a space");
        assert!(!number.is_empty());
    }

    #[test]
    fn every_frame_opens_the_window_the_host_sends_in() {
        let mut chip = Sdpcm::new();
        // The driver's own arithmetic: it may send while
        // `(tx_max - tx_seq)` is non-zero and its top bit is clear.
        let mut tx_seq: u8 = 255;
        for i in 0..8 {
            let iovar = named("mpc", &1u32.to_le_bytes());
            chip.write(&request(tx_seq, i + 1, C_SET_VAR, true, &iovar));
            chip.write_end();
            tx_seq = tx_seq.wrapping_add(1);

            let (_, payload) = read_frame(&mut chip).expect("a frame");
            assert_eq!(bcdc_parts(&payload).2 >> 16, u32::from(i) + 1);
            // The window the frame carried, out of the second header word.
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
        // A length that does not complement its checksum.
        let mut bad = request(0, 1, C_GET_VERSION, false, &[]);
        bad[2] ^= 0xFF;
        chip.write(&bad);
        chip.write_end();
        assert!(!chip.frame_waiting());

        // A data offset inside the header.
        let mut bad = request(0, 1, C_GET_VERSION, false, &[]);
        bad[7] = 2;
        chip.write(&bad);
        chip.write_end();
        assert!(!chip.frame_waiting());

        // Half a header is not malformed, only unfinished; it waits.
        chip.write(&[0u8; 6]);
        chip.write_end();
        assert!(!chip.frame_waiting());
    }

    /// The same request with the glom extension on it, as
    /// `brcmf_sdio_hdpack` (`sdio.c:1503`) builds one with `bus->txglom` set:
    /// the hardware header carries the padded length, the extension the real
    /// one, and the software header moves eight bytes along.
    fn glom_request(seq: u8, id: u16, cmd: u32, set: bool, payload: &[u8], pad: u16) -> Vec<u8> {
        let plain = request(seq, id, cmd, set, payload);
        // The host backs its buffer up by the whole header it now uses, so
        // the frame is the extension longer than the plain one.
        let real = HDRLEN + HWEXT_LEN + BCDC_HDRLEN + payload.len();
        let mut frame = Vec::new();
        let total = (real + usize::from(pad)) as u16;
        frame.extend_from_slice(&total.to_le_bytes());
        frame.extend_from_slice(&(!total).to_le_bytes());
        // The real length without the hardware header, and "last frame".
        frame.extend_from_slice(&(((real - HWHDR_LEN) as u32) | 1 << 24).to_le_bytes());
        frame.extend_from_slice(&(u32::from(pad) << 16).to_le_bytes());
        // The software header, with the payload offset moved along with it.
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
        // Before the answer, a glommed frame is not what the chip expects.
        chip.write(&glom_request(0, 1, C_GET_VERSION, false, &[0; 4], 0));
        chip.write_end();
        assert!(!chip.frame_waiting(), "not glomming yet");

        // `brcmf_sdio_bus_preinit` asks, and the chip says yes.
        chip.write(&request(
            1,
            2,
            C_SET_VAR,
            true,
            &named("bus:rxglom", &1u32.to_le_bytes()),
        ));
        chip.write_end();
        read_frame(&mut chip).expect("the acknowledgement");

        // From here every request the host sends has the extension, padding
        // and all.
        let mut iovar = named("cur_etheraddr", &[]);
        iovar.resize(iovar.len() + 6, 0);
        chip.write(&glom_request(2, 3, C_GET_VAR, false, &iovar, 20));
        chip.write_end();
        let (_, payload) = read_frame(&mut chip).expect("a frame");
        assert_eq!(bcdc_parts(&payload).2 >> 16, 3);
        assert_eq!(&payload[BCDC_HDRLEN..BCDC_HDRLEN + 6], PLACEHOLDER_MAC);
        // ...and the answer still comes back in the plain shape, because
        // nothing changed about the direction the chip sends in.
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
        // What the MMC core does with a length that is not a whole number of
        // blocks: the blocks in one command, the remainder in another.
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
        // Channel 2, the data channel.
        frame[5] = 0x02;
        chip.write(&frame);
        chip.write_end();
        assert!(!chip.frame_waiting());
    }

    #[test]
    fn reading_past_a_frame_gets_zeros_and_then_the_next_frame() {
        let mut chip = Sdpcm::new();
        for i in 0..2u8 {
            chip.write(&request(i, u16::from(i) + 1, C_GET_VERSION, false, &[0; 4]));
            chip.write_end();
        }
        // The driver rounds its second read up to the block size, well past
        // the end of the frame.
        let mut first = [0u8; 64];
        chip.read(&mut first);
        let hd = parse_header(&first, false).unwrap();
        assert!(hd.len <= 64, "this answer fits in the first read");
        // ...so the next read is the next frame, not the tail of this one.
        let (hd, payload) = read_frame(&mut chip).expect("the second frame");
        assert_eq!(hd.seq, 1);
        assert_eq!(bcdc_parts(&payload).2 >> 16, 2);
        // And past the last frame: zeros, which is "no more to read".
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
        // Each entry has to decode as a 20 MHz channel in the 2.4 GHz band.
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
        // `brcmf_enable_bw40_2g` (`cfg80211.c:7143`) puts a 40 MHz 2.4 GHz
        // channel spec in the front of its buffer and expects only 40 MHz
        // channels back; each one that is not earns a `WARN_ON`.
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

        let mut iovar = named("cur_etheraddr", &[]);
        iovar.resize(iovar.len() + 6, 0);
        chip.write(&request(1, 2, C_GET_VAR, false, &iovar));
        chip.write_end();
        let (_, payload) = read_frame(&mut chip).expect("a frame");
        assert_eq!(&payload[BCDC_HDRLEN..BCDC_HDRLEN + 6], wanted);
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
}
