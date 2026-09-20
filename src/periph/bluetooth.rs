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
    /// The address the chip reports, which the firmware writes into it.
    bd_addr: [u8; 6],
}

impl BtModem {
    /// A modem whose address is `bd_addr` — a Pi's is its Ethernet MAC plus
    /// one (`e4:5f:01:83:fb:74` on the network and `…:75` on the air, on a
    /// Raspberry Pi 4B d03115).
    pub fn new(bd_addr: [u8; 6]) -> BtModem {
        BtModem {
            rx: Vec::new(),
            out: Vec::new(),
            baud: DEFAULT_BAUD,
            minidriver: false,
            firmware_bytes: 0,
            bd_addr,
        }
    }

    /// The rate the host last asked the chip's UART for.
    pub fn baud(&self) -> u32 {
        self.baud
    }

    /// The address the chip answers `Read_BD_ADDR` with.
    pub fn bd_addr(&self) -> [u8; 6] {
        self.bd_addr
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
            OP_READ_BD_ADDR => ret.extend_from_slice(&self.bd_addr),
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
                    self.bd_addr.copy_from_slice(addr);
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

    #[test]
    fn the_address_reads_back_and_the_firmware_can_change_it() {
        let mut m = modem();
        assert_eq!(
            command(&mut m, OP_READ_BD_ADDR, &[])[1..],
            [0x02, 0x00, 0x5e, 0x00, 0x53, 0x02]
        );
        command(&mut m, OP_BCM_WRITE_BD_ADDR, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(
            command(&mut m, OP_READ_BD_ADDR, &[])[1..],
            [1, 2, 3, 4, 5, 6]
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
