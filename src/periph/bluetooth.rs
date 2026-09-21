//! The Bluetooth modem on `UART0`: the CYW43455's BT side, which a Pi 4B has
//! wired to the PL011 on GPIO 30..33 whenever `config.txt` does not carry
//! `dtoverlay=disable-bt` (#124).
//!
//! It speaks H4 — one type byte, then the packet — and answers the commands
//! `hci_uart`'s Broadcom protocol sends while it brings the chip up
//! (`drivers/bluetooth/btbcm.c`, `hci_bcm.c`): a reset, the local version,
//! the vendor commands around the firmware download, and the addresses and
//! names the core reads afterwards. The firmware download itself is accepted
//! and thrown away: the chip runs no patch RAM here.
//!
//! The identity it reports is measured on a Raspberry Pi 4B d03115:
//!
//! ```text
//!   HCI Version: 5.0 (0x9)  Revision: 0x17e
//!   LMP Version: 5.0 (0x9)  Subversion: 0x6119
//!   Manufacturer: Cypress Semiconductor (305)
//!   Features: 0xbf 0xfe 0xcf 0xfe 0xdb 0xff 0x7b 0x87
//! ```
//!
//! — `hciconfig -a`, with `Bluetooth: hci0: BCM: chip id 107` in `dmesg`.
//! Subversion `0x6119` is what `btbcm`'s table calls `BCM4345C0`, the
//! firmware file a Pi 4B loads.
//!
//! Nothing in the model's own boots drives it yet: the `kernel8.img`
//! `scripts/fetch-firmware.sh` puts on the card has no Bluetooth stack built
//! in (no `hci_uart`, no `Bluetooth: Core ver`), so the modem sees the
//! device-tree node come up as `ttyAMA1` and nothing more. A stock rootfs
//! loads the modules and talks to it.

/// H4 packet types (Bluetooth core specification, Vol 4 Part A).
const H4_COMMAND: u8 = 0x01;
const H4_ACL: u8 = 0x02;
const H4_SCO: u8 = 0x03;
const H4_EVENT: u8 = 0x04;

/// `HCI_Command_Complete` (Vol 4 Part E, 7.7.14).
const EVT_COMMAND_COMPLETE: u8 = 0x0E;

/// Status codes: success, and "unknown HCI command".
const STATUS_OK: u8 = 0x00;
const STATUS_UNKNOWN_COMMAND: u8 = 0x01;

/// The opcodes the modem answers.
const OP_RESET: u16 = 0x0C03;
const OP_READ_LOCAL_NAME: u16 = 0x0C14;
const OP_READ_LOCAL_VERSION: u16 = 0x1001;
const OP_READ_LOCAL_COMMANDS: u16 = 0x1002;
const OP_READ_LOCAL_FEATURES: u16 = 0x1003;
const OP_READ_BUFFER_SIZE: u16 = 0x1005;
const OP_READ_BD_ADDR: u16 = 0x1009;
/// Broadcom vendor commands (`btbcm.c`).
const OP_BCM_WRITE_BD_ADDR: u16 = 0xFC01;
const OP_BCM_SET_BAUDRATE: u16 = 0xFC18;
const OP_BCM_DOWNLOAD_MINIDRIVER: u16 = 0xFC2E;
const OP_BCM_WRITE_UART_CLOCK: u16 = 0xFC45;
const OP_BCM_WRITE_RAM: u16 = 0xFC4C;
const OP_BCM_LAUNCH_RAM: u16 = 0xFC4E;
const OP_BCM_READ_VERBOSE_CONFIG: u16 = 0xFC79;

/// What `Read_Local_Version_Information` answers with, measured (module
/// docs): HCI 5.0, revision `0x017e`, LMP 5.0, Cypress (305), `BCM4345C0`.
const HCI_VERSION: u8 = 0x09;
const HCI_REVISION: u16 = 0x017E;
const LMP_VERSION: u8 = 0x09;
const MANUFACTURER: u16 = 305;
const LMP_SUBVERSION: u16 = 0x6119;

/// `Read_Local_Supported_Features`, measured: `hciconfig -a`'s `Features`.
const LOCAL_FEATURES: [u8; 8] = [0xbf, 0xfe, 0xcf, 0xfe, 0xdb, 0xff, 0x7b, 0x87];

/// `Read_Verbose_Config_Version_Info`'s chip id, which `btbcm` prints as
/// `BCM: chip id 107` on the reference board. The other five bytes are the
/// firmware build it reports, which is the patch RAM's; this chip has none.
const CHIP_ID: u8 = 107;

/// The rate the chip's UART comes up at, before `Set_Baudrate` moves it.
const DEFAULT_BAUD: u32 = 115_200;

/// The modem's side of the line.
pub struct BtModem {
    /// Bytes from the host that do not make a whole packet yet.
    rx: Vec<u8>,
    /// Events waiting to go back to the host.
    out: Vec<u8>,
    /// What the last `Set_Baudrate` asked for. The model's UART does not
    /// change rate with it — both ends are the model's — but the value is
    /// worth having for a log.
    baud: u32,
    /// The chip is in the minidriver, i.e. between `Download_Minidriver` and
    /// `Launch_RAM`, where it takes firmware chunks.
    minidriver: bool,
    /// Firmware bytes accepted and dropped since the minidriver started.
    firmware_bytes: usize,
    /// The address the chip answers `Read_BD_ADDR` with, most significant
    /// octet first — the order an address is written in. HCI carries it the
    /// other way round: `Read_BD_ADDR`'s return parameter and
    /// `BCM_WRITE_BD_ADDR`'s parameter are little-endian like every other
    /// multi-octet HCI field (Vol 4 Part E, 5.2), so it is reversed on the way
    /// out and on the way in.
    bd_addr: [u8; 6],
    /// The host has written an address with `BCM_WRITE_BD_ADDR`, so
    /// [`Self::bd_addr`] is no longer the one the chip came up with. That is
    /// what a Linux host does at attach with the device tree's
    /// `local-bd-address` ([`published_bd_address`]), and it is the difference
    /// between "the chip's own" and "the board's" in the run report.
    bd_addr_written: bool,
}

impl BtModem {
    /// A modem that comes up holding `bd_addr`, most significant octet first.
    ///
    /// This is the chip's own address, not the board's: a Pi's Bluetooth
    /// address is its Ethernet MAC plus one (`e4:5f:01:83:fb:74` on the
    /// network and `…:75` on the air, on a Raspberry Pi 4B d03115), but that
    /// is a value the *firmware* derives and publishes in the device tree, and
    /// the host programs it into the chip at attach. Until then the chip
    /// answers whatever it was built with.
    pub fn new(bd_addr: [u8; 6]) -> BtModem {
        BtModem {
            rx: Vec::new(),
            out: Vec::new(),
            baud: DEFAULT_BAUD,
            minidriver: false,
            firmware_bytes: 0,
            bd_addr,
            bd_addr_written: false,
        }
    }

    /// The rate the host last asked the chip's UART for.
    pub fn baud(&self) -> u32 {
        self.baud
    }

    /// The address the chip answers `Read_BD_ADDR` with, most significant
    /// octet first.
    pub fn bd_addr(&self) -> [u8; 6] {
        self.bd_addr
    }

    /// Whether [`Self::bd_addr`] is one the host wrote rather than the one the
    /// chip came up with.
    pub fn bd_addr_written(&self) -> bool {
        self.bd_addr_written
    }

    /// Bytes the host sent down the line.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.rx.extend_from_slice(bytes);
        while self.take_packet() {}
    }

    /// Whatever the modem has to say, and nothing once it is taken.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// Something is waiting to go back to the host.
    pub fn has_output(&self) -> bool {
        !self.out.is_empty()
    }

    /// Consume one whole H4 packet, if there is one. Returns false when what
    /// is buffered is still short of a packet.
    fn take_packet(&mut self) -> bool {
        let Some(&kind) = self.rx.first() else {
            return false;
        };
        // Every type this chip is sent has its length in a fixed place: one
        // byte after a three-byte command header, two after a four-byte ACL
        // one (Vol 4 Part A, 2).
        let (header, length) = match kind {
            H4_COMMAND => (4, self.rx.get(3).map(|&n| usize::from(n))),
            H4_ACL => (
                5,
                self.rx
                    .get(3..5)
                    .map(|n| usize::from(u16::from_le_bytes([n[0], n[1]]))),
            ),
            H4_SCO => (4, self.rx.get(3).map(|&n| usize::from(n))),
            // Nothing else is legal from the host; drop the byte rather than
            // stalling on it for ever.
            _ => {
                self.rx.remove(0);
                return !self.rx.is_empty();
            }
        };
        let Some(length) = length else {
            return false;
        };
        if self.rx.len() < header + length {
            return false;
        }
        let packet: Vec<u8> = self.rx.drain(..header + length).collect();
        if kind == H4_COMMAND {
            let opcode = u16::from_le_bytes([packet[1], packet[2]]);
            self.command(opcode, &packet[4..]);
        }
        // ACL and SCO go nowhere: there is no link to carry them.
        !self.rx.is_empty()
    }

    /// Answer one command with a `Command_Complete`.
    fn command(&mut self, opcode: u16, params: &[u8]) {
        let mut ret = vec![STATUS_OK];
        match opcode {
            OP_RESET => {
                // A reset puts the UART back to the rate the chip starts at.
                self.baud = DEFAULT_BAUD;
                self.minidriver = false;
                self.firmware_bytes = 0;
            }
            OP_READ_LOCAL_VERSION => {
                ret.push(HCI_VERSION);
                ret.extend_from_slice(&HCI_REVISION.to_le_bytes());
                ret.push(LMP_VERSION);
                ret.extend_from_slice(&MANUFACTURER.to_le_bytes());
                ret.extend_from_slice(&LMP_SUBVERSION.to_le_bytes());
            }
            OP_READ_LOCAL_FEATURES => ret.extend_from_slice(&LOCAL_FEATURES),
            OP_READ_LOCAL_COMMANDS => {
                // The 64-byte bitmap. Every command this modem answers is in
                // it; the bits are octet `n`, bit `m` per Vol 4 Part E, 6.27.
                let mut commands = [0u8; 64];
                for (octet, bit) in [
                    (5, 0),  // Reset
                    (14, 3), // Read_Local_Version_Information
                    (14, 5), // Read_Local_Supported_Features
                    (14, 6), // Read_Buffer_Size
                    (15, 1), // Read_BD_ADDR
                    (13, 1), // Read_Local_Name
                ] {
                    commands[octet] |= 1 << bit;
                }
                ret.extend_from_slice(&commands);
            }
            OP_READ_BUFFER_SIZE => {
                // `ACL MTU: 1021:8  SCO MTU: 64:1` (`hciconfig -a`).
                ret.extend_from_slice(&1021u16.to_le_bytes());
                ret.push(64);
                ret.extend_from_slice(&8u16.to_le_bytes());
                ret.extend_from_slice(&1u16.to_le_bytes());
            }
            OP_READ_BD_ADDR => ret.extend(self.bd_addr.iter().rev()),
            OP_READ_LOCAL_NAME => {
                // 248 bytes, NUL-padded. A chip with no patch RAM loaded
                // reports the part it is.
                let mut name = [0u8; 248];
                let text = b"BCM4345C0";
                name[..text.len()].copy_from_slice(text);
                ret.extend_from_slice(&name);
            }
            OP_BCM_READ_VERBOSE_CONFIG => {
                // `btbcm` reads the chip id out of the second byte.
                ret.push(CHIP_ID);
                ret.extend_from_slice(&[0; 4]);
            }
            OP_BCM_SET_BAUDRATE => {
                // Two reserved bytes, then the rate, little-endian.
                if let Some(rate) = params.get(2..6) {
                    self.baud = u32::from_le_bytes([rate[0], rate[1], rate[2], rate[3]]);
                }
            }
            OP_BCM_WRITE_BD_ADDR => {
                if let Some(addr) = params.get(..6) {
                    for (slot, &b) in self.bd_addr.iter_mut().zip(addr.iter().rev()) {
                        *slot = b;
                    }
                    self.bd_addr_written = true;
                }
            }
            OP_BCM_DOWNLOAD_MINIDRIVER => {
                self.minidriver = true;
                self.firmware_bytes = 0;
            }
            OP_BCM_WRITE_RAM => self.firmware_bytes += params.len().saturating_sub(4),
            OP_BCM_LAUNCH_RAM => self.minidriver = false,
            OP_BCM_WRITE_UART_CLOCK => {}
            _ => ret[0] = STATUS_UNKNOWN_COMMAND,
        }
        self.command_complete(opcode, &ret);
    }

    /// `HCI_Command_Complete`: one command credit, the opcode, then the
    /// return parameters (status first).
    fn command_complete(&mut self, opcode: u16, ret: &[u8]) {
        let plen = 3 + ret.len();
        debug_assert!(plen <= usize::from(u8::MAX), "event too long for H4");
        self.out.push(H4_EVENT);
        self.out.push(EVT_COMMAND_COMPLETE);
        self.out.push(plen as u8);
        self.out.push(1); // Num_HCI_Command_Packets
        self.out.extend_from_slice(&opcode.to_le_bytes());
        self.out.extend_from_slice(ret);
    }
}

/// What the device tree the firmware handed the ARM says the board's
/// Bluetooth address is, from [`published_bd_address`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedBdAddr {
    /// The path of the node the firmware left enabled, and the address in it,
    /// most significant octet first. `None` when every node in the tree is
    /// disabled — which is what `dtoverlay=disable-bt` leaves behind.
    pub enabled: Option<(String, [u8; 6])>,
    /// How many nodes carry a `local-bd-address` at all. A Pi 4B's base tree
    /// has two, one under each UART, and the firmware enables the one the
    /// overlays leave the modem on.
    pub nodes: usize,
}

/// The Bluetooth address the firmware derived and published, read out of the
/// device tree it handed the ARM.
///
/// Nothing else in a run reports it: it never reaches the console, and the
/// chip itself answers its own address until a host writes this one into it
/// with `BCM_WRITE_BD_ADDR`. A firmware bump that changes the derivation would
/// otherwise pass every scenario unnoticed.
///
/// A node is taken by its `local-bd-address` property rather than by its
/// `compatible` (`brcm,bcm43438-bt` on a Pi 4B), because the property is what
/// is being read. Which node is *reported* is decided by `status`, not by
/// order: the tree carries one node per UART, the firmware enables whichever
/// the config.txt overlays leave the modem on, and the other keeps a
/// `local-bd-address` of all zeroes. Reporting the first match would report
/// those zeroes on a board whose modem is on the mini-UART.
pub fn published_bd_address(fdt: &crate::fdt::Fdt) -> PublishedBdAddr {
    let nodes = fdt.nodes();
    bd_address_of(
        nodes
            .iter()
            .map(|(_, path, props)| (path.as_str(), props.as_slice())),
    )
}

/// [`published_bd_address`] over an already-walked tree, so the choice between
/// the nodes can be tested without building a blob.
fn bd_address_of<'a>(
    nodes: impl Iterator<Item = (&'a str, &'a [crate::fdt::Property])>,
) -> PublishedBdAddr {
    let mut out = PublishedBdAddr {
        enabled: None,
        nodes: 0,
    };
    for (path, props) in nodes {
        let Some(addr) = props
            .iter()
            .find(|p| p.name == "local-bd-address")
            .and_then(|p| decode_bd_address(&p.value))
        else {
            continue;
        };
        out.nodes += 1;
        // No `status` at all means enabled (Devicetree Specification v0.4,
        // 2.3.4), and the firmware writes "okay" on the one it kept.
        let enabled = props
            .iter()
            .find(|p| p.name == "status")
            .and_then(|p| p.as_str())
            .is_none_or(|s| s == "okay" || s == "ok");
        if enabled && out.enabled.is_none() {
            out.enabled = Some((path.to_string(), addr));
        }
    }
    out
}

/// A `local-bd-address` property as an address, most significant octet first.
///
/// The property holds a `bdaddr_t`, which is least significant octet first
/// (`hci_dev_get_bd_addr_from_property()` reads it straight into one), so the
/// six bytes `ab f9 aa 5e 00 02` are the address `02:00:5e:aa:f9:ab` — the one
/// the host then writes into the chip with `BCM_WRITE_BD_ADDR`.
pub fn decode_bd_address(value: &[u8]) -> Option<[u8; 6]> {
    let mut addr = [0u8; 6];
    for (slot, &b) in addr.iter_mut().zip(value.get(..6)?.iter().rev()) {
        *slot = b;
    }
    Some(addr)
}

/// An address as it is written: `02:00:5e:aa:f9:ab`.
pub fn format_bd_address(addr: [u8; 6]) -> String {
    let [a, b, c, d, e, f] = addr;
    format!("{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modem() -> BtModem {
        BtModem::new([0x02, 0x00, 0x5e, 0x00, 0x53, 0x02])
    }

    /// One HCI command down the line, and the return parameters of the
    /// `Command_Complete` that comes back (the status byte included).
    fn command(m: &mut BtModem, opcode: u16, params: &[u8]) -> Vec<u8> {
        let mut packet = vec![H4_COMMAND];
        packet.extend_from_slice(&opcode.to_le_bytes());
        packet.push(params.len() as u8);
        packet.extend_from_slice(params);
        m.feed(&packet);
        let event = m.take_output();
        assert_eq!(event[0], H4_EVENT);
        assert_eq!(event[1], EVT_COMMAND_COMPLETE);
        assert_eq!(usize::from(event[2]), event.len() - 3);
        assert_eq!(event[3], 1, "one command credit");
        assert_eq!(u16::from_le_bytes([event[4], event[5]]), opcode);
        event[6..].to_vec()
    }

    #[test]
    fn it_reports_the_identity_the_reference_board_does() {
        let mut m = modem();
        assert_eq!(command(&mut m, OP_RESET, &[]), [STATUS_OK]);
        let v = command(&mut m, OP_READ_LOCAL_VERSION, &[]);
        assert_eq!(v[0], STATUS_OK);
        assert_eq!(v[1], 0x09); // HCI 5.0
        assert_eq!(u16::from_le_bytes([v[2], v[3]]), 0x017e);
        assert_eq!(v[4], 0x09); // LMP 5.0
        assert_eq!(u16::from_le_bytes([v[5], v[6]]), 305); // Cypress
        assert_eq!(u16::from_le_bytes([v[7], v[8]]), 0x6119); // BCM4345C0
        assert_eq!(
            command(&mut m, OP_READ_LOCAL_FEATURES, &[])[1..],
            LOCAL_FEATURES
        );
        assert_eq!(command(&mut m, OP_BCM_READ_VERBOSE_CONFIG, &[])[1], CHIP_ID);
    }

    #[test]
    fn an_unknown_command_is_refused_rather_than_ignored() {
        let mut m = modem();
        assert_eq!(command(&mut m, 0xFCFF, &[]), [STATUS_UNKNOWN_COMMAND]);
    }

    /// What `btbcm_setup_patchram` does: minidriver, chunks, launch, reset.
    #[test]
    fn the_firmware_download_is_accepted_and_dropped() {
        let mut m = modem();
        command(&mut m, OP_BCM_DOWNLOAD_MINIDRIVER, &[]);
        assert!(m.minidriver);
        // Each chunk is a four-byte address plus its bytes.
        for chunk in 0..3u32 {
            let mut params = chunk.to_le_bytes().to_vec();
            params.extend_from_slice(&[0xaa; 16]);
            assert_eq!(command(&mut m, OP_BCM_WRITE_RAM, &params), [STATUS_OK]);
        }
        assert_eq!(m.firmware_bytes, 48);
        command(&mut m, OP_BCM_LAUNCH_RAM, &0u32.to_le_bytes());
        assert!(!m.minidriver);
    }

    #[test]
    fn set_baudrate_is_remembered_and_a_reset_undoes_it() {
        let mut m = modem();
        let mut params = vec![0, 0];
        params.extend_from_slice(&3_000_000u32.to_le_bytes());
        command(&mut m, OP_BCM_SET_BAUDRATE, &params);
        assert_eq!(m.baud(), 3_000_000);
        command(&mut m, OP_RESET, &[]);
        assert_eq!(m.baud(), DEFAULT_BAUD);
    }

    /// HCI carries the address least significant octet first, both ways, so
    /// what goes on the wire is the reverse of what is written down. A host
    /// that writes the device tree's `abf9aa5e0002` sends exactly those bytes
    /// and the chip then holds `02:00:5e:aa:f9:ab`.
    #[test]
    fn the_address_reads_back_and_the_host_can_change_it() {
        let mut m = modem();
        assert_eq!(
            command(&mut m, OP_READ_BD_ADDR, &[])[1..],
            [0x02, 0x53, 0x00, 0x5e, 0x00, 0x02]
        );
        assert_eq!(m.bd_addr(), [0x02, 0x00, 0x5e, 0x00, 0x53, 0x02]);
        assert!(!m.bd_addr_written());

        command(
            &mut m,
            OP_BCM_WRITE_BD_ADDR,
            &[0xab, 0xf9, 0xaa, 0x5e, 0x00, 0x02],
        );
        assert_eq!(m.bd_addr(), [0x02, 0x00, 0x5e, 0xaa, 0xf9, 0xab]);
        assert!(m.bd_addr_written());
        assert_eq!(
            command(&mut m, OP_READ_BD_ADDR, &[])[1..],
            [0xab, 0xf9, 0xaa, 0x5e, 0x00, 0x02]
        );
    }

    fn prop(name: &str, value: &[u8]) -> crate::fdt::Property {
        crate::fdt::Property {
            name: name.to_string(),
            value: value.to_vec(),
        }
    }

    /// The property is least significant octet first, and the node to report
    /// is the one the firmware enabled — not the first one in the tree, which
    /// on a stock-config card is the disabled mini-UART node holding zeroes.
    #[test]
    fn the_published_address_comes_from_the_node_the_firmware_enabled() {
        let bt = prop("compatible", b"brcm,bcm43438-bt\0");
        let mini = [
            bt.clone(),
            prop("local-bd-address", &[0, 0, 0, 0, 0, 0]),
            prop("status", b"disabled\0"),
        ];
        let pl011 = [
            bt,
            prop("local-bd-address", &[0xab, 0xf9, 0xaa, 0x5e, 0x00, 0x02]),
            prop("status", b"okay\0"),
        ];
        let found = bd_address_of(
            [
                ("/soc/serial@7e215040/bluetooth", mini.as_slice()),
                ("/soc/serial@7e201000/bluetooth", pl011.as_slice()),
            ]
            .into_iter(),
        );
        assert_eq!(found.nodes, 2);
        let (path, addr) = found.enabled.expect("one node is enabled");
        assert_eq!(path, "/soc/serial@7e201000/bluetooth");
        assert_eq!(format_bd_address(addr), "02:00:5e:aa:f9:ab");
    }

    /// `dtoverlay=disable-bt` leaves both nodes disabled. The firmware still
    /// fills the address in, so there is a value to be misreported here.
    #[test]
    fn a_tree_with_no_enabled_node_publishes_nothing() {
        let disabled = |addr: [u8; 6]| {
            [
                prop("local-bd-address", &addr),
                prop("status", b"disabled\0"),
            ]
        };
        let mini = disabled([0, 0, 0, 0, 0, 0]);
        let pl011 = disabled([0xab, 0xf9, 0xaa, 0x5e, 0x00, 0x02]);
        let found = bd_address_of(
            [
                ("/soc/serial@7e215040/bluetooth", mini.as_slice()),
                ("/soc/serial@7e201000/bluetooth", pl011.as_slice()),
            ]
            .into_iter(),
        );
        assert_eq!(found.nodes, 2);
        assert_eq!(found.enabled, None);
    }

    /// A node with no `status` is enabled (Devicetree Specification v0.4,
    /// 2.3.4), and a node without the property is not a Bluetooth node.
    #[test]
    fn a_node_without_a_status_counts_as_enabled() {
        let bt = [prop("local-bd-address", &[6, 5, 4, 3, 2, 1])];
        let other = [prop("compatible", b"brcm,bcm2835-aux-uart\0")];
        let found = bd_address_of(
            [
                ("/soc/serial@7e215040", other.as_slice()),
                ("/soc/serial@7e201000/bluetooth", bt.as_slice()),
            ]
            .into_iter(),
        );
        assert_eq!(found.nodes, 1);
        assert_eq!(
            found.enabled,
            Some((
                "/soc/serial@7e201000/bluetooth".to_string(),
                [1, 2, 3, 4, 5, 6]
            ))
        );
    }

    #[test]
    fn a_command_split_across_writes_is_still_one_command() {
        let mut m = modem();
        m.feed(&[H4_COMMAND, 0x03]);
        assert!(!m.has_output());
        m.feed(&[0x0c]);
        assert!(!m.has_output());
        m.feed(&[0x00]);
        assert!(m.has_output());
        assert_eq!(
            m.take_output(),
            [H4_EVENT, EVT_COMMAND_COMPLETE, 4, 1, 0x03, 0x0c, STATUS_OK]
        );
    }

    #[test]
    fn two_commands_in_one_write_are_both_answered() {
        let mut m = modem();
        m.feed(&[
            H4_COMMAND, 0x03, 0x0c, 0x00, // Reset
            H4_COMMAND, 0x09, 0x10, 0x00, // Read_BD_ADDR
        ]);
        let out = m.take_output();
        assert_eq!(
            out[..7],
            [H4_EVENT, EVT_COMMAND_COMPLETE, 4, 1, 0x03, 0x0c, STATUS_OK]
        );
        assert_eq!(out[7..11], [H4_EVENT, EVT_COMMAND_COMPLETE, 10, 1]);
    }

    /// ACL data has nowhere to go, and must not be mistaken for a command.
    #[test]
    fn acl_data_is_swallowed_whole() {
        let mut m = modem();
        m.feed(&[H4_ACL, 0x01, 0x20, 0x02, 0x00, 0xde, 0xad]);
        assert!(!m.has_output());
        assert_eq!(command(&mut m, OP_RESET, &[]), [STATUS_OK]);
    }
}
