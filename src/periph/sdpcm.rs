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
//! ### What the event channel carries
//!
//! Everything the firmware says without being asked, on channel 1. A frame
//! there is a BCDC *data* header — four bytes (`bcdc.c:73`), not the sixteen
//! a command uses — and then an Ethernet frame the chip addresses to itself:
//! `struct brcmf_event` (`fweh.h:251`), an ether header of protocol
//! `ETH_P_LINK_CTL`, Broadcom's own header with its OUI in it, and the event
//! message. `brcmf_fweh_process_skb` (`fweh.h:394`) checks the OUI and the
//! subtype and hands what is left to `brcmf_fweh_process_event`, which
//! queues it for a worker.
//!
//! Which events reach the host at all is the host's choice, and it says so
//! twice: `event_msgs` is a bit per firmware event code
//! (`brcmf_c_preinit_dcmds`, `common.c:419`), and on this chip
//! `brcmf_cyw_activate_events` (`cyw/core.c:82`) then sets the same mask
//! again through `event_msgs_ext`, which wraps it in a version, a command and
//! a length. Both are answered from the one mask the chip keeps, and a get
//! reports it.
//! <https://github.com/raspberrypi/linux/blob/16f1da3c4e94437449d6aa151589ca0ad4b388bb/drivers/net/wireless/broadcom/brcm80211/brcmfmac/cyw/core.c>
//!
//! ### What a scan gets back
//!
//! The one thing here that is not an answer to a question. `escan`
//! (`brcmf_run_escan`, `cfg80211.c:1443`) is a set, acknowledged like any
//! other, and what the host is really waiting for arrives afterwards on the
//! event channel: a `BRCMF_E_ESCAN_RESULT` per network with
//! `BRCMF_E_STATUS_PARTIAL`, each carrying a `struct brcmf_escan_result_le`
//! around one `struct brcmf_bss_info_le` and its information elements, and
//! then one more with `BRCMF_E_STATUS_SUCCESS` to say the scan is over.
//! `brcmf_cfg80211_escan_handler` (`cfg80211.c:3642`) collects the first
//! kind and `brcmf_inform_bss` (`cfg80211.c:3432`) hands them to cfg80211.
//!
//! It takes its time over it, a dwell per channel, which is the one place
//! in this module where being slower is being righter: see
//! [`SCAN_DWELL_US`].
//!
//! The networks it reports are **invented** — see [`NETWORKS`]. Nothing here
//! listens to anything.
//!
//! ### What this is not
//!
//! A radio. The responder below answers what `brcmf_bus_started`
//! (`core.c`) asks on the way to registering a network interface, and its
//! answers are the model's own — a firmware version that says so, one band,
//! and a chip address that is invented rather than fused into a part.
//! Nothing here associates or carries a packet, so there is nothing for the
//! data channel to carry either: a frame the host sends on it is taken and
//! goes nowhere.
//!
//! Nothing raises an event during a bring-up, and that is not a gap: the
//! whole of `brcmf_cfg80211_up` (`cfg80211.c:7878`) is a run of BCDC
//! commands — `BRCMF_C_UP`, `BRCMF_C_SET_PM`, the roaming settings,
//! `BRCMF_C_SET_INFRA`, `BRCMF_C_SET_FAKEFRAG` — each answered on the
//! control channel, and it waits for nothing else. The one event the model
//! does raise there is the interface it comes up with, and the host is not
//! listening that early; see `Firmware::start` below.

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

/// `SDPCM_CONTROL_CHANNEL` (`sdio.c:1354`): a command and its answer.
const CHANNEL_CONTROL: u8 = 0;
/// `SDPCM_EVENT_CHANNEL` (`sdio.c:1355`): everything the chip says without
/// being asked. Only the chip sends on it — `brcmf_sdio_hdparse`
/// (`sdio.c:1448`) will take a frame there and `brcmf_sdio_readframes`
/// (`sdio.c:2065`) hands it to `brcmf_rx_event`, while the host's own frames
/// go out on the control and data channels.
const CHANNEL_EVENT: u8 = 1;
/// `SDPCM_DATA_CHANNEL` (`sdio.c:1356`): what `brcmf_sdio_txpkt`
/// (`sdio.c:2382`) sends a network packet on. There is no radio behind this,
/// so a frame arriving here is taken and dropped.
const CHANNEL_DATA: u8 = 2;

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

/// `BCDC_HEADER_LEN` (`bcdc.c:46`): the *other* BCDC header, the one on a
/// frame that is not a command. `struct brcmf_proto_bcdc_header`
/// (`bcdc.c:73`) is four bytes — flags, priority, more flags, and how much
/// firmware signalling sits between it and the packet.
const BCDC_DATA_HDRLEN: usize = 4;
/// `BCDC_PROTO_VER` (`bcdc.c:47`) in `[7:4]` of the flags
/// (`BCDC_FLAG_VER_SHIFT`, `bcdc.c:49`). `brcmf_proto_bcdc_hdrpull`
/// (`bcdc.c:290`) refuses a frame carrying anything else as "non-BCDC".
const BCDC_DATA_FLAGS: u8 = 2 << 4;

/// `ETH_P_LINK_CTL` (`if_ether.h`): the ether type an event frame carries,
/// and the first thing `brcmf_fweh_process_skb` (`fweh.h:394`) checks.
const ETH_P_LINK_CTL: u16 = 0x886C;
/// `BRCM_OUI` (`fweh.h:209`), in `struct brcm_ethhdr`. A frame whose OUI is
/// not this one is not an event and `brcmf_fweh_process_skb` drops it.
const BRCM_OUI: [u8; 3] = [0x00, 0x10, 0x18];
/// `BCMILCP_BCM_SUBTYPE_EVENT` (`fweh.h:210`), the header's `usr_subtype`:
/// the last of the three things that make a frame an event.
const BCM_SUBTYPE_EVENT: u16 = 1;
/// `BCMILCP_SUBTYPE_VENDOR_LONG` (`fweh.h:211`), the header's `subtype`.
/// `brcmf_rx_event` (`core.c:528`) asks for no particular subtype, so this
/// one is checked only on the data path (`brcmf_rx_frame`, `core.c:500`) —
/// but a firmware writes it either way.
const SUBTYPE_VENDOR_LONG: u16 = 32769;
/// `struct brcmf_event_msg_be.version` (`fweh.h:231`). The driver prints it
/// and acts on nothing in it.
const EVENT_MSG_VERSION: u16 = 2;
/// `sizeof(struct brcmf_event)` (`fweh.h:251`): the ether header, Broadcom's
/// header and the message, which is the least
/// `brcmf_fweh_process_skb` will look at.
const EVENT_HDRLEN: usize = 14 + 10 + 48;
/// `IFNAMSIZ`, the fixed-width name in the message.
const EVENT_IFNAME_LEN: usize = 16;

/// `BRCMF_E_IF` (`fweh.h:77`): a bsscfg came or went. It is the one event
/// `brcmf_fweh_process_event` (`fweh.c:498`) lets through with no handler
/// registered for it, and the one both `brcmf_c_preinit_dcmds`
/// (`common.c:425`) and `brcmf_fweh_activate_events` (`fweh.c:456`) force
/// into the mask whatever else the host wants — "old cruft that all vendors
/// have", as the driver puts it.
const E_IF: u32 = 54;
/// `BRCMF_E_IF_ADD` (`fweh.h:192`), the `action` of a `struct
/// brcmf_if_event` (`fweh.h:286`).
const E_IF_ADD: u8 = 1;
/// `BRCMF_E_IF_ROLE_STA` (`fweh.h:200`).
const E_IF_ROLE_STA: u8 = 0;

/// `BRCMF_E_ESCAN_RESULT` (`fweh.h:89`): one network a scan found, or — with
/// a status that is not `PARTIAL` — the scan finishing.
/// `brcmf_init_escan` (`cfg80211.c:3754`) registers the handler for it, which
/// is what puts the code in the mask the host sets.
/// <https://github.com/raspberrypi/linux/blob/16f1da3c4e94437449d6aa151589ca0ad4b388bb/drivers/net/wireless/broadcom/brcm80211/brcmfmac/fweh.h>
const E_ESCAN_RESULT: u32 = 69;
/// `BRCMF_E_STATUS_SUCCESS` (`fweh.h:125`) and `BRCMF_E_STATUS_PARTIAL`
/// (`fweh.h:133`). The handler reads the event's data only on a `PARTIAL`;
/// anything else is the scan ending, and `aborted` is "not `SUCCESS`"
/// (`cfg80211.c:3744`).
const E_STATUS_SUCCESS: u32 = 0;
const E_STATUS_PARTIAL: u32 = 8;

/// `struct eventmsgs_ext` (`cyw/fwil_types.h:31`): version, command, mask
/// length, the most a get may answer with, and then the mask.
const EVENTMSGS_EXT_HDRLEN: usize = 4;
/// `EVENTMSGS_VER` (`cyw/fwil_types.h:18`).
const EVENTMSGS_VER: u8 = 1;
/// `enum brcmf_event_msgs_ext_command` (`cyw/fwil_types.h:11`). Only
/// `SET_MASK` is ever sent — `brcmf_cyw_activate_events` (`cyw/core.c:94`)
/// hands the whole mask over at once — but the other two are the iovar's
/// own, and a firmware answers them.
const EVENTMSGS_SET_BIT: u8 = 1;
const EVENTMSGS_RESET_BIT: u8 = 2;
const EVENTMSGS_SET_MASK: u8 = 3;

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
/// `BRCMU_CHSPEC_CH_MASK` (`include/brcmu_d11.h:73`): the channel number in a
/// channel spec, which is all this reads out of the ones a scan request
/// names.
const CHSPEC_CH_MASK: u16 = 0x00FF;

/// `WL_ESCAN_ACTION_START` (`cfg80211.h:51`). The other two — `CONTINUE` and
/// `ABORT` — are a scan the model never has running to continue or abort:
/// the results are all there by the time the `escan` set is acknowledged.
const ESCAN_ACTION_START: u16 = 1;
/// `BRCMF_ESCAN_REQ_VERSION_V2` (`fwil_types.h:74`), the version
/// `brcmf_run_escan` (`cfg80211.c:1470`) stamps unless `BRCMF_FEAT_SCAN_V2`
/// is off, in which case it converts the parameters down and stamps
/// `BRCMF_ESCAN_REQ_VERSION` (1). The two shapes put `channel_num` in
/// different places, so the version is what says where to look.
const ESCAN_REQ_VERSION_V2: u32 = 2;
/// `offsetof(struct brcmf_escan_params_le, params_le)` (`fwil_types.h:460`):
/// the version, the action and the sync id in front of the scan parameters.
const ESCAN_PARAMS_HDRLEN: usize = 8;
/// `BRCMF_SCAN_PARAMS_FIXED_SIZE` and `BRCMF_SCAN_PARAMS_V2_FIXED_SIZE`
/// (`fwil_types.h:50`): where `channel_list` starts in each shape.
const SCAN_PARAMS_FIXED: usize = 64;
const SCAN_PARAMS_V2_FIXED: usize = 72;
/// Where `channel_num` sits in each — the word before `channel_list`.
const SCAN_PARAMS_CHANNEL_NUM: usize = SCAN_PARAMS_FIXED - 4;
const SCAN_PARAMS_V2_CHANNEL_NUM: usize = SCAN_PARAMS_V2_FIXED - 4;
/// `BRCMF_SCAN_PARAMS_COUNT_MASK` (`fwil_types.h:57`): the low half of
/// `channel_num` is how many channel specs follow, and zero means all of
/// them.
const SCAN_PARAMS_COUNT_MASK: u32 = 0x0000_FFFF;

/// `BRCMF_BSS_INFO_VERSION` (`fwil_types.h:21`). `brcmf_inform_bss`
/// (`cfg80211.c:3442`) refuses a whole scan whose results carry any other
/// number.
const BSS_INFO_VERSION: u32 = 109;
/// `sizeof(struct brcmf_bss_info_le)` (`fwil_types.h:314`) as a C compiler
/// lays it out: the struct is not `__packed`, so it is 128 bytes with
/// alignment padding inside it, not the 121 its fields add up to. It is also
/// the offset the information elements start at, which is what `ie_offset`
/// carries.
const BSS_INFO_LEN: usize = 128;
/// `WL_ESCAN_RESULTS_FIXED_SIZE` (`fwil_types.h:478`): `buflen`, `version`,
/// `sync_id` and `bss_count` in front of the one BSS.
/// `brcmf_cfg80211_escan_handler` (`cfg80211.c:3702`) checks the BSS's own
/// length against `buflen` less exactly this.
const ESCAN_RESULTS_FIXED: usize = 12;

/// `WLAN_CAPABILITY_ESS` (`include/linux/ieee80211.h`): an infrastructure
/// network. `brcmf_cfg80211_escan_handler` (`cfg80211.c:3712`) throws away a
/// result with `WLAN_CAPABILITY_IBSS` set unless the wiphy has ad-hoc among
/// its interface modes, and the bit next to it is `PRIVACY` — off, so what
/// the model reports is an open network. Nothing can associate with it
/// either way.
const BSS_CAPABILITY: u16 = 0x0001;
/// The beacon interval, in TU, which is what every access point uses and
/// what `iw` prints as `beacon interval: 100 TUs`.
const BSS_BEACON_PERIOD: u16 = 100;
/// One beacon per DTIM, the simplest thing an access point can say.
const BSS_DTIM_PERIOD: u8 = 1;
/// The noise floor the model reports beside the signal. Read nowhere in the
/// driver; a firmware measures it, and this one has nothing to measure, so
/// it is a plausible number and no more.
const BSS_PHY_NOISE: i8 = -92;

/// Supported rates, in 500 kbit/s units with the high bit on the ones that
/// are basic — the encoding `struct brcmf_bss_info_le.rateset`
/// (`fwil_types.h:324`) and the Supported Rates element share. These are
/// 802.11b's four as basic and 802.11g's eight on top, which is what a
/// 2.4 GHz access point with no HT offers.
const BSS_RATES: [u8; 12] = [
    0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6C,
];
/// How many rates fit in a Supported Rates element before the rest have to
/// go in an Extended Supported Rates one (802.11, 9.4.2.3).
const SUPP_RATES_MAX: usize = 8;

/// Information-element ids (`enum ieee80211_eid`,
/// `include/linux/ieee80211.h`). The SSID is the one that matters: cfg80211
/// takes the network's name out of the elements and not out of
/// `brcmf_bss_info_le.SSID`, so a result whose elements have no SSID in them
/// is a network `iw` prints with an empty name.
const EID_SSID: u8 = 0;
const EID_SUPP_RATES: u8 = 1;
const EID_DS_PARAMS: u8 = 3;
const EID_EXT_SUPP_RATES: u8 = 50;

/// How long the chip spends on a channel before it moves to the next one.
///
/// `brcmf_escan_prep` (`cfg80211.c:1119`) asks for the firmware's own
/// defaults — `active_time` and `passive_time` are both `-1` — and
/// Broadcom's active dwell is about this. Thirteen channels of it is the
/// second or so a scan on a real board takes, which is also why the driver
/// gives one ten seconds before it calls it lost
/// (`BRCMF_ESCAN_TIMER_INTERVAL_MS`, `cfg80211.h:49`).
///
/// **A scan has to take time.** Answering one the instant it is asked looks
/// like a faster model and is a wrong one: `iw dev wlan0 scan` sends the
/// trigger, waits for it to be acknowledged, and only then opens the socket
/// it listens for the completion on (`__listen_events`, `iw/event.c`) — so a
/// completion that beat it to that socket is an event nothing was subscribed
/// to, and `iw` waits in `recvmsg` for one that has already been and gone.
/// A real scan loses that race by a thousandfold; this one has to lose it
/// too.
const SCAN_DWELL_US: u64 = 40_000;

/// One network the model's firmware reports when the host scans.
struct Bss {
    ssid: &'static str,
    bssid: [u8; 6],
    /// A 2.4 GHz channel, which has to be one [`CHANNELS_2G`] offers:
    /// cfg80211 knows only the channels `brcmf_construct_chaninfo`
    /// (`cfg80211.c:7008`) enabled, and a result on any other one is a BSS
    /// with no channel to put it on.
    channel: u32,
    /// The signal, in dBm. `brcmf_inform_single_bss` (`cfg80211.c:3396`)
    /// reads the field as a signed 16-bit number and hands cfg80211 a
    /// hundred times it, which is what `iw` prints as `signal: -60.00 dBm`.
    rssi: i16,
}

/// What a scan finds, which is **invented**: there is no radio behind this
/// module and nothing was measured to produce it.
///
/// The SSID names the model rather than a place, so that a scan result can
/// never be mistaken for one taken off the air. The BSSID is the next
/// address along from the three the model already has — the OTP Ethernet
/// MAC `02:00:5e:00:53:01`, the Bluetooth modem's `02:00:5e:00:53:02` and
/// the WiFi chip's own [`CHIP_MAC`] — so it is locally administered, unicast
/// and from the documentation range in RFC 7042 section 2.1.2. Nothing
/// derives it and it is not any network's.
///
/// One entry because one is what the goal needs; the code below sends a
/// `PARTIAL` per entry and the completion after them, so a second is data
/// and nothing else. Two entries on the same channel would also exercise
/// `brcmf_compare_update_same_bss` (`cfg80211.c:3311`).
const NETWORKS: [Bss; 1] = [Bss {
    ssid: "pimu-model-ap",
    bssid: [0x02, 0x00, 0x5E, 0x00, 0x53, 0x04],
    channel: 1,
    rssi: -60,
}];

/// The address the chip came out of its own OTP with, which is what it
/// answers `cur_etheraddr` with until something overrides it.
///
/// A real 43455 has a unique one fused into the part — the comment above
/// `brcmf_default_mac_address` (`common.c:236`) is about exactly that, and
/// about the boards where the nvram's address gets used instead and collides.
/// So this is **invented**: locally administered (bit 1 of the first octet),
/// unicast (bit 0 clear), from the documentation range in RFC 7042 section
/// 2.1.2, and the next one along from the two the model already has — the OTP
/// Ethernet MAC `02:00:5e:00:53:01` (`crate::periph::configotp`) and the
/// Bluetooth modem's `02:00:5e:00:53:02` (`crate::machine`). Nothing derives
/// it and it is not any board's.
///
/// Deliberately not the GENET's, either: on real hardware the WLAN address
/// comes from a different part than the Ethernet one, and the run report
/// prints both so that they are seen rather than assumed equal.
///
/// It must also not be `brcmf_default_mac_address` itself (`common.c:247`,
/// `00:90:4c:c5:12:38`), which `brcmf_c_preinit_dcmds` (`common.c:296`)
/// throws away for a random address — which would be a different address
/// every run.
const CHIP_MAC: [u8; 6] = [0x02, 0x00, 0x5E, 0x00, 0x53, 0x03];

/// What the model answers the `ver` iovar with. `brcmf_c_preinit_dcmds`
/// (`common.c:268`) prints it as the firmware version and keeps whatever
/// follows the last space as the number ethtool reports, so it has to have
/// one. It says what it is: no firmware ran to produce it.
const VERSION: &str = "pimu model firmware (no radio) version 0";

/// Where the address the chip answers `cur_etheraddr` with came from, for the
/// run report. Which of the three won is not visible anywhere else: the
/// address reaches the console only through `ip link`, and that prints one
/// address whichever it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacSource {
    /// [`CHIP_MAC`]: the chip's own, and nothing overrode it.
    Otp,
    /// The card's nvram carried a `macaddr=` line and the firmware took it.
    Nvram,
    /// The host wrote one with `cur_etheraddr`.
    Host,
}

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

    /// Bring the firmware to model time `now_us`, and say whether that put a
    /// frame in front of the host.
    ///
    /// Everything else the chip sends is an answer, so the host is already
    /// writing when it is queued and the frame indication goes out with the
    /// end of that write. A scan's results are the one thing the firmware
    /// has without being asked *again*: it was asked once, and then it goes
    /// and listens. So they need a clock, and the caller has to raise the
    /// indication for them.
    #[inline]
    pub fn advance_to(&mut self, now_us: u64) -> bool {
        self.fw.now_us = now_us;
        !self.fw.pending.is_empty() && self.flush_events()
    }

    /// The firmware starting: read the nvram the host left in the chip's
    /// memory and take the address out of it, which is the one thing in the
    /// file the model has anything to do with, and then announce the
    /// interface it came up with.
    pub fn start(&mut self, nvram: &[u8]) {
        if let Some(mac) = nvram_macaddr(nvram) {
            self.fw.mac = mac;
            self.fw.mac_source = MacSource::Nvram;
        }
        self.fw.start();
        self.flush_events();
    }

    /// The event mask the host last set, one bit per firmware event code.
    pub fn event_mask(&self) -> &[u8] {
        &self.fw.event_mask
    }

    /// Which iovar the host last set that mask with.
    pub fn event_mask_source(&self) -> EventMaskSource {
        self.fw.event_mask_source
    }

    /// The firmware event codes the host has asked to hear about.
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

    /// How many events the chip has sent, and how many it raised and threw
    /// away because the host had not asked for them.
    pub fn events_sent(&self) -> u32 {
        self.fw.events_sent
    }

    pub fn events_dropped(&self) -> u32 {
        self.fw.events_dropped
    }

    /// Frames the host sent on the data channel, which is every packet it
    /// tried to transmit through the interface.
    pub fn data_frames_in(&self) -> u32 {
        self.fw.data_frames_in
    }

    /// Scans the host asked for, and how many networks the chip reported
    /// across them. Nothing about a scan reaches the console: the request is
    /// an iovar and the results are events.
    pub fn escans(&self) -> u32 {
        self.fw.escans
    }

    pub fn escan_results(&self) -> u32 {
        self.fw.escan_results
    }

    /// The address the chip answers `cur_etheraddr` with, for the run report.
    pub fn mac(&self) -> [u8; 6] {
        self.fw.mac
    }

    /// Where that address came from.
    pub fn mac_source(&self) -> MacSource {
        self.fw.mac_source
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
        match sw.channel {
            CHANNEL_CONTROL => {
                let reply = self.fw.control(&frame[sw.doffset..sw.len]);
                // Turning receive glomming on is the one thing an answer
                // changes about the protocol itself: from the next frame the
                // host sends, the hardware header has the glom extension on
                // it.
                self.glom = self.fw.rxglom;
                self.push(CHANNEL_CONTROL, &reply);
                // A command may leave the firmware with something to say.
                // It goes out behind the answer, never in front of it: the
                // driver is waiting on the control channel and matches the
                // request id, and an event that overtook the answer would be
                // one more frame to read before it got there.
                self.flush_events();
            }
            // A packet the host wants transmitted. There is no radio, so it
            // is taken — a frame the chip left unread would stall the
            // protocol — and goes nowhere. A real firmware would answer with
            // a `BRCMF_E_TXSTATUS` once flow control is on; nothing here
            // turns that on, and `brcmf_proto_bcdc_txcomplete` (`bcdc.c`)
            // frees the packet on its own.
            CHANNEL_DATA => self.fw.data_frames_in += 1,
            // Nothing else is a channel the host sends on.
            _ => {}
        }
    }

    /// Hand whatever the firmware has ready to the frame queue, and say
    /// whether anything went.
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

/// Which iovar last wrote the event mask, for the run report. The two say
/// the same thing in different shapes and the chip keeps one mask, so which
/// of them the host used is visible nowhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventMaskSource {
    /// Nothing has set it: the chip's own, which is empty.
    Firmware,
    /// `event_msgs`, the plain mask (`common.c:433`).
    EventMsgs,
    /// `event_msgs_ext`, which is what this chip's vendor half of the driver
    /// uses (`brcmf_cyw_activate_events`, `cyw/core.c:82`).
    EventMsgsExt,
}

/// The firmware behind the control channel: what the model answers the
/// driver's commands with.
struct Firmware {
    mac: [u8; 6],
    mac_source: MacSource,
    /// The host asked for receive glomming and was told yes, which changes
    /// the shape of everything it sends from then on.
    rxglom: bool,
    /// One bit per firmware event code: what the host has asked to be told
    /// about. It starts empty, which is the conservative reading of a
    /// register the driver bothers to read before it writes
    /// (`brcmf_c_preinit_dcmds`, `common.c:420`): a firmware that came up
    /// announcing everything would have the host's first `event_msgs` get
    /// turn it all back on again, and nothing in the driver says what the
    /// default is.
    ///
    /// Its length is the host's to choose — `fweh->event_mask_len` is
    /// `DIV_ROUND_UP(fweh->num_event_codes, 8)`, and `num_event_codes` comes
    /// from the vendor half (`BRCMF_CYW_E_LAST` = 197, so 25 bytes, for this
    /// chip) — so the chip keeps whatever it was handed.
    event_mask: Vec<u8>,
    event_mask_source: EventMaskSource,
    /// Model time, as [`Sdpcm::advance_to`] last brought the chip to. What
    /// the firmware does on its own rather than in answer to a command is
    /// timed against it.
    now_us: u64,
    /// Events raised and not yet framed, each with the model time the chip
    /// has it ready at, earliest first. [`Sdpcm::flush_events`] takes the
    /// ones whose time has come — immediately for an event a command raised,
    /// and a scan's channel dwells later for a scan's.
    pending: VecDeque<(u64, Event)>,
    events_sent: u32,
    events_dropped: u32,
    data_frames_in: u32,
    /// Scans the host asked for, and networks reported across them. The
    /// whole exchange is events, so none of it reaches the console.
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

    /// The firmware has its interface, and says so.
    ///
    /// A bsscfg coming into existence is what `BRCMF_E_IF` with
    /// `BRCMF_E_IF_ADD` reports, and bsscfg 0 comes into existence when the
    /// firmware starts. The host is not listening yet — it turns events on
    /// with `event_msgs` much later, in `brcmf_c_preinit_dcmds`
    /// (`common.c:419`) — so the mask throws this one away, which is why
    /// `brcmfmac` never sees an `IF` event for `wlan0` and creates the
    /// interface itself (`brcmf_bus_started`, `core.c:1237`). Should one
    /// arrive anyway, the driver is ready for it: `brcmf_add_if`
    /// (`core.c:244`) logs "ignore IF event" and leaves the interface alone.
    fn start(&mut self) {
        let mut event = Event::new(E_IF);
        event.addr = self.mac;
        // `struct brcmf_if_event` (`fweh.h:286`): the interface index, what
        // happened to it, flags, the bsscfg index and the role it has. Index
        // and bsscfg are both 0 — this is the primary — and the role is a
        // station, which is what `brcmf_cfg80211_attach` then puts it in.
        event.data = vec![0, E_IF_ADD, 0, 0, E_IF_ROLE_STA];
        self.raise(event);
    }

    /// Raise an event now, if the host asked for this one.
    fn raise(&mut self, event: Event) {
        self.raise_at(event, self.now_us);
    }

    /// Raise one the chip will only have at model time `due_us` — what a
    /// scan's results are, because a scan takes as long as it takes to
    /// listen. Raised in order, so the queue stays sorted by the time each
    /// event comes due.
    fn raise_at(&mut self, event: Event, due_us: u64) {
        if self.wants(event.code) {
            self.events_sent += 1;
            self.pending.push_back((due_us, event));
        } else {
            self.events_dropped += 1;
        }
    }

    /// Is `code`'s bit set in the mask? `setbit` (`brcmu_utils.h`) and
    /// `brcmf_fweh_activate_events` (`fweh.c:450`) number the bits the way
    /// they are laid out: byte `code / 8`, bit `code % 8`.
    fn wants(&self, code: u32) -> bool {
        let (byte, bit) = (code as usize / 8, code % 8);
        self.event_mask
            .get(byte)
            .is_some_and(|b| b & (1 << bit) != 0)
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
            b"cur_etheraddr" if value.len() >= 6 => {
                self.mac.copy_from_slice(&value[..6]);
                self.mac_source = MacSource::Host;
            }
            // `brcmf_sdio_bus_preinit` (`sdio.c:3724`) takes a yes here as
            // permission to use the glom header on everything it sends.
            b"bus:rxglom" => self.rxglom = word() != 0,
            // The plain mask, byte for byte
            // (`brcmf_fweh_activate_events`, `fweh.c:462`).
            b"event_msgs" => {
                self.event_mask = value.to_vec();
                self.event_mask_source = EventMaskSource::EventMsgs;
            }
            // The same mask inside `struct eventmsgs_ext`, which is how the
            // `cyw` half of the driver sets it on this chip.
            b"event_msgs_ext" => self.event_msgs_ext_set(value),
            // The one set whose answer is not the whole of what the host is
            // waiting for.
            b"escan" => self.escan(value),
            _ => {}
        }
    }

    /// A scan, answered with what the model's firmware has to report.
    ///
    /// `struct brcmf_escan_params_le` (`fwil_types.h:460`): the version, what
    /// to do, an id to tell concurrent scans apart, and then the scan
    /// parameters. The model has its results already, but it does not hand
    /// them over yet: each comes due a channel dwell at a time, because a
    /// scan that answered inside its own acknowledgement is one `iw` never
    /// hears the end of — see [`SCAN_DWELL_US`].
    ///
    /// Waiting is safe: `brcmf_cfg80211_scan` (`cfg80211.c:1568`) sets
    /// `BRCMF_SCAN_STATUS_BUSY` *before* it sends the request, so a result
    /// arriving any time after this is one the handler will take, and the
    /// driver's own timeout is ten seconds away.
    fn escan(&mut self, value: &[u8]) {
        let Some(head) = value.get(..ESCAN_PARAMS_HDRLEN) else {
            return;
        };
        let version = u32::from_le_bytes(head[..4].try_into().unwrap());
        let action = u16::from_le_bytes(head[4..6].try_into().unwrap());
        let sync_id = u16::from_le_bytes(head[6..8].try_into().unwrap());
        // `CONTINUE` and `ABORT` are for a scan that is still running, and
        // one never is: by the time the host could send either, the results
        // and the completion are already queued.
        if action != ESCAN_ACTION_START {
            return;
        }
        self.escans += 1;

        // One channel at a time, in the order the request named them, and a
        // network is heard while the chip is on its channel — so a result
        // comes due at the end of that channel's dwell and the completion at
        // the end of the last.
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

        // And then the scan ending, which is the event `iw` is really
        // waiting on: `brcmf_notify_escan_complete` (`cfg80211.c:1184`) is
        // what tells cfg80211 the results are all in, and nothing else in
        // the driver calls it — bar the ten-second timeout
        // (`BRCMF_ESCAN_TIMER_INTERVAL_MS`, `cfg80211.h:49`), which would
        // report the same networks ten seconds later and an aborted scan
        // with them. The completion carries no data: the handler reads the
        // event's payload only on a `PARTIAL`.
        let mut done = Event::new(E_ESCAN_RESULT);
        done.status = E_STATUS_SUCCESS;
        done.addr = self.mac;
        self.raise_at(done, due);
    }

    /// One `event_msgs_ext` write. `len` says how much of `mask` is meant,
    /// and `command` what to do with it: replace the mask, or turn the bits
    /// it names on or off. `brcmf_cyw_activate_events` (`cyw/core.c:94`)
    /// only ever replaces.
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
            // `EVENTMSGS_NONE`, and anything else: nothing to do.
            _ => return,
        }
        self.event_mask_source = EventMaskSource::EventMsgsExt;
    }

    /// What an `event_msgs_ext` get answers with: the header the host sent
    /// back, with the mask the chip holds under it and the length of it in
    /// both `len` and `maxgetsize`.
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
                    // What the host has asked for so far, which is what
                    // `brcmf_c_preinit_dcmds` (`common.c:420`) reads before
                    // it sets `BRCMF_E_IF` and writes the mask back. An
                    // answer of zeros would be a lie only once the host has
                    // set something; from here the chip reports what it
                    // holds.
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

/// One firmware event, as `struct brcmf_event_msg_be` (`fweh.h:230`) carries
/// it, plus the event-specific bytes that follow.
///
/// Everything in the message is big-endian, which is the one place in this
/// protocol that is: the control channel, the SDPCM headers and the nvram are
/// all little-endian, and `brcmf_fweh_event_worker` (`fweh.c:288`) byte-swaps
/// every field of this one on the way in.
struct Event {
    /// The firmware event code, which is the index the driver looks its
    /// handler up by (`brcmf_fweh_process_event`, `fweh.c:495`).
    code: u32,
    /// `BRCMF_EVENT_MSG_*` (`fweh.h:120`). `BRCMF_EVENT_MSG_LINK` is the one
    /// that means anything on its own: `brcmf_is_linkdown` (`cfg80211.c:6143`)
    /// reads a `BRCMF_E_LINK` without it as the link having gone down.
    flags: u16,
    /// `BRCMF_E_STATUS_*` (`fweh.h:125`) and `BRCMF_E_REASON_*`
    /// (`fweh.h:153`): what happened and why.
    status: u32,
    reason: u32,
    /// Whose address the event is about — a peer's, or the interface's own.
    addr: [u8; 6],
    /// The interface and bsscfg it belongs to. `brcmf_fweh_event_worker`
    /// (`fweh.c:280`) throws away anything whose bsscfg is past
    /// `BRCMF_MAX_IFS`.
    ifidx: u8,
    bsscfgidx: u8,
    /// The event-specific bytes, `datalen` of them.
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

    /// The payload of an event frame: the four-byte BCDC data header, then
    /// the Ethernet frame `brcmf_fweh_process_skb` (`fweh.h:394`) picks
    /// apart. `mac` is the interface's address, which the frame is both from
    /// and to — the chip is talking to its own host, and `eth_type_trans`
    /// has to see a packet addressed to the interface for
    /// `brcmf_rx_hdrpull` (`core.c:483`) to keep it.
    fn encode(&self, mac: [u8; 6]) -> Vec<u8> {
        let mut out = Vec::with_capacity(BCDC_DATA_HDRLEN + EVENT_HDRLEN + self.data.len());
        // flags, priority, the interface index, and no firmware signalling
        // between this header and the packet — which is what lets
        // `brcmf_fws_hdrpull` (`fwsignal.c`) return without looking.
        out.extend_from_slice(&[BCDC_DATA_FLAGS, 0, self.ifidx, 0]);

        // `struct ethhdr`.
        out.extend_from_slice(&mac);
        out.extend_from_slice(&mac);
        out.extend_from_slice(&ETH_P_LINK_CTL.to_be_bytes());

        // `struct brcm_ethhdr` (`fweh.h:222`). Its `length` is the driver's
        // own "TODO", read nowhere; a firmware puts the rest of the frame in
        // it.
        let rest = (48 + self.data.len()) as u16;
        out.extend_from_slice(&SUBTYPE_VENDOR_LONG.to_be_bytes());
        out.extend_from_slice(&rest.to_be_bytes());
        out.push(0);
        out.extend_from_slice(&BRCM_OUI);
        out.extend_from_slice(&BCM_SUBTYPE_EVENT.to_be_bytes());

        // `struct brcmf_event_msg_be`.
        out.extend_from_slice(&EVENT_MSG_VERSION.to_be_bytes());
        out.extend_from_slice(&self.flags.to_be_bytes());
        out.extend_from_slice(&self.code.to_be_bytes());
        out.extend_from_slice(&self.status.to_be_bytes());
        out.extend_from_slice(&self.reason.to_be_bytes());
        // `auth_type`, which only the authentication events use.
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(self.data.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.addr);
        // The interface's name, fixed width and NUL-padded. The model's
        // firmware has no name of its own for it, and the driver reads it
        // only when it is the one creating the interface
        // (`brcmf_fweh_handle_if_event`, `fweh.c:167`) — which is never for
        // an interface that already exists.
        out.extend_from_slice(&[0u8; EVENT_IFNAME_LEN]);
        out.push(self.ifidx);
        out.push(self.bsscfgidx);

        out.extend_from_slice(&self.data);
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

/// The `macaddr=` line of the nvram the host downloaded into the chip, if it
/// has one.
///
/// The file on the card is `key=value` lines, but what reaches the chip is
/// what `brcmf_fw_nvram_strip` (`firmware.c:398`) made of it: comments and
/// blank lines gone, every newline a NUL, and the whole padded out with more
/// NULs (`firmware.c:438`) — so an entry is the text between two of them.
/// <https://github.com/raspberrypi/linux/blob/16f1da3c4e94437449d6aa151589ca0ad4b388bb/drivers/net/wireless/broadcom/brcm80211/brcmfmac/firmware.c>
///
/// The driver drops the file's `macaddr` only when the platform itself hands
/// one down (`nvp->strip_mac`, `firmware.c:131`, from
/// `eth_platform_get_mac_address`), and then writes its own in its place
/// (`brcmf_fw_add_macaddr`, `firmware.c:383`). Nothing in the model supplies
/// one, so the line the card carries is the line the chip gets.
fn nvram_macaddr(nvram: &[u8]) -> Option<[u8; 6]> {
    nvram
        .split(|b| *b == 0)
        .filter_map(|entry| entry.strip_prefix(b"macaddr=".as_slice()))
        .find_map(parse_mac)
}

/// `02:00:5e:00:57:01` as six bytes, which is how an nvram file writes one.
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

/// The information elements the model's firmware reports for a network, as
/// they would have come out of its beacon: an id, a length and the body,
/// one after another (802.11, 9.4.2).
///
/// cfg80211 is handed these and nothing else about the network's name —
/// `brcmf_inform_single_bss` (`cfg80211.c:3399`) points `notify_ie` at
/// `ie_offset` and leaves `brcmf_bss_info_le.SSID` alone — so the SSID
/// element is what `iw` prints. The rest are what an access point that
/// offers nothing but 802.11g would carry, and what `iw` prints under it.
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

/// One `struct brcmf_bss_info_le` (`fwil_types.h:314`) with its information
/// elements behind it.
///
/// The fields are written in the order the struct declares them and the
/// padding a C compiler puts between them is written out with them: the
/// struct is not `__packed`, so `RSSI` lands at 78 rather than 77 and
/// `nbss_cap` at 84 rather than 82, and the whole comes to 128 bytes rather
/// than 121. Getting that wrong shifts `ie_offset` and `ie_length`, and the
/// driver reads the elements from wherever they say.
fn bss_info(bss: &Bss) -> Vec<u8> {
    let ies = beacon_ies(bss);
    let mut out = Vec::with_capacity(BSS_INFO_LEN + ies.len());
    let mut ssid = [0u8; 32];
    ssid[..bss.ssid.len()].copy_from_slice(bss.ssid.as_bytes());
    let mut rates = [0u8; 16];
    rates[..BSS_RATES.len()].copy_from_slice(&BSS_RATES);

    out.extend_from_slice(&BSS_INFO_VERSION.to_le_bytes());
    // `length` is the whole record, elements included — which is what the
    // handler checks against the event's own `buflen` and what
    // `next_bss_le` (`cfg80211.c:3423`) walks the collected results by.
    out.extend_from_slice(&((BSS_INFO_LEN + ies.len()) as u32).to_le_bytes());
    out.extend_from_slice(&bss.bssid);
    out.extend_from_slice(&BSS_BEACON_PERIOD.to_le_bytes());
    out.extend_from_slice(&BSS_CAPABILITY.to_le_bytes());
    out.push(bss.ssid.len() as u8);
    out.extend_from_slice(&ssid);
    // `rateset`, whose `count` is four-byte aligned and so starts a byte
    // past the SSID rather than on it.
    out.push(0);
    out.extend_from_slice(&(BSS_RATES.len() as u32).to_le_bytes());
    out.extend_from_slice(&rates);
    out.extend_from_slice(&((CHSPEC_BW_20 | bss.channel) as u16).to_le_bytes());
    // `atim_window`, which is an ad-hoc network's.
    out.extend_from_slice(&0u16.to_le_bytes());
    out.push(BSS_DTIM_PERIOD);
    // `RSSI` is two-byte aligned, so a pad byte comes before it.
    out.push(0);
    out.extend_from_slice(&bss.rssi.to_le_bytes());
    out.push(BSS_PHY_NOISE as u8);
    // `n_cap`: not 802.11n, so the MCS set and the HT capabilities below
    // stay zero and cfg80211 sees a plain 802.11g network.
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
    // `SNR`, the signal over the noise floor the model reports beside it.
    let snr = bss.rssi - i16::from(BSS_PHY_NOISE);
    out.extend_from_slice(&snr.to_le_bytes());
    out.extend_from_slice(&[0, 0]);

    debug_assert_eq!(out.len(), BSS_INFO_LEN, "not the struct's own length");
    out.extend_from_slice(&ies);
    out
}

/// One scan result as the event carries it: `struct brcmf_escan_result_le`
/// (`fwil_types.h:470`) wrapped around a single BSS.
///
/// `brcmf_cfg80211_escan_handler` (`cfg80211.c:3679`) insists on all three
/// of these agreeing — `buflen` no larger than the event's own `datalen`,
/// exactly one BSS, and that BSS's `length` exactly `buflen` less
/// [`ESCAN_RESULTS_FIXED`] — and throws the result away with a message if
/// they do not.
fn escan_result(bss: &Bss, sync_id: u16) -> Vec<u8> {
    let info = bss_info(bss);
    let mut out = Vec::with_capacity(ESCAN_RESULTS_FIXED + info.len());
    out.extend_from_slice(&((ESCAN_RESULTS_FIXED + info.len()) as u32).to_le_bytes());
    out.extend_from_slice(&BSS_INFO_VERSION.to_le_bytes());
    // The id the request came with, handed back. The driver matches nothing
    // against it — a firmware that runs several scans at once would.
    out.extend_from_slice(&sync_id.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&info);
    out
}

/// The channels a scan request asks for, or `None` for all of them.
///
/// `brcmf_escan_prep` (`cfg80211.c:1176`) puts the count in the low half of
/// `channel_num` and the specs themselves in `channel_list` behind the fixed
/// part of the parameters — which is 64 bytes in the first shape and 72 in
/// the second, so `version` is what says where to look. A count of zero is
/// "use all available channels", which is the comment on the field.
///
/// The SSID list beyond the channels is not read: it is what a real firmware
/// would put in its probe requests, and a network answers a directed probe
/// or not, but its beacons arrive either way. So a scan for one name still
/// finds what is there.
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

    /// The nvram as it reaches the chip: one NUL-terminated `key=value`
    /// entry per line of the file, padded out to a word with more NULs
    /// (`brcmf_fw_nvram_strip`, `firmware.c:438`). The token past the end of
    /// it is the bus's business, not this module's.
    fn nvram(lines: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for line in lines {
            out.extend_from_slice(line.as_bytes());
            out.push(0);
        }
        out.resize(out.len().next_multiple_of(4), 0);
        out
    }

    /// Read `cur_etheraddr` back the way `brcmf_c_preinit_dcmds`
    /// (`common.c:289`) does.
    fn read_mac(chip: &mut Sdpcm, seq: u8) -> [u8; 6] {
        let mut iovar = named("cur_etheraddr", &[]);
        iovar.resize(iovar.len() + 6, 0);
        chip.write(&request(seq, 1, C_GET_VAR, false, &iovar));
        chip.write_end();
        let (_, payload) = read_frame(chip).expect("a frame");
        payload[BCDC_HDRLEN..BCDC_HDRLEN + 6].try_into().unwrap()
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
        assert_eq!(&payload[BCDC_HDRLEN..BCDC_HDRLEN + 6], CHIP_MAC);
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
        // Taken and counted, not left half-read: the bytes are gone and the
        // next frame starts where it should.
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

        // Valid as `is_valid_ether_addr` has it — unicast and not all zero —
        // and not the address `brcmf_c_preinit_dcmds` (`common.c:296`)
        // throws away for a random one, which would be a different address
        // every run.
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
            "macaddr=02:00:5e:00:57:01",
            "boardtype=0x6e4",
        ]));
        let wanted = [0x02, 0x00, 0x5E, 0x00, 0x57, 0x01];
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

    /// A mask with those firmware event codes turned on, the way `setbit`
    /// lays them out.
    fn mask_of(codes: &[u32]) -> Vec<u8> {
        let mut mask = vec![0u8; CYW_MASK_LEN];
        for code in codes {
            mask[*code as usize / 8] |= 1 << (code % 8);
        }
        mask
    }

    /// Set `event_msgs` the way `brcmf_fweh_activate_events` (`fweh.c:462`)
    /// does, and take the acknowledgement.
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

    /// Set `event_msgs_ext` the way `brcmf_cyw_activate_events`
    /// (`cyw/core.c:82`) does: the four-byte header and then the mask.
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

    /// Read an iovar back, as `brcmf_fil_iovar_data_get` (`fwil.c`) does:
    /// the name and a buffer of the size wanted, and the answer's first
    /// `room` bytes are what it keeps.
    fn get_iovar(chip: &mut Sdpcm, seq: u8, name: &str, room: usize) -> Vec<u8> {
        let mut iovar = named(name, &[]);
        iovar.resize(iovar.len() + room, 0);
        chip.write(&request(seq, 1, C_GET_VAR, false, &iovar));
        chip.write_end();
        let (_, payload) = read_frame(chip).expect("a frame");
        payload[BCDC_HDRLEN..BCDC_HDRLEN + room].to_vec()
    }

    /// An event frame taken apart the way the driver takes one apart:
    /// `brcmf_proto_bcdc_hdrpull` (`bcdc.c:290`) pops the BCDC header,
    /// `eth_type_trans` the ether header, `brcmf_fweh_process_skb`
    /// (`fweh.h:394`) checks Broadcom's, and `brcmf_fweh_event_worker`
    /// (`fweh.c:288`) byte-swaps the message. Panics wherever the driver
    /// would have dropped the frame.
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

    /// An `escan` request the way `brcmf_run_escan` (`cfg80211.c:1443`)
    /// builds one: `struct brcmf_escan_params_le` with the version, the
    /// action and the sync id, then the scan parameters `brcmf_escan_prep`
    /// (`cfg80211.c:1096`) filled in. `channels` empty is the abort shape's
    /// count of zero, which means every channel.
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

    /// Ask for a scan and take the acknowledgement. Nothing else is in the
    /// FIFO afterwards: a scan takes as long as it takes to listen.
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
    /// `brcmf_cfg80211_escan_handler` (`cfg80211.c:3668`) checks one and
    /// then taken apart the way `brcmf_inform_single_bss` (`cfg80211.c:3360`)
    /// takes it apart. Panics wherever the driver would have dropped it.
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
