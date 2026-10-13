//! A scripted Wake-on-LAN magic packet: the built-in peer sends the board one,
//! once the console has printed a given text, for the station address GENET has
//! programmed at that moment.

use std::time::Duration;

use pimu::bus::Bus;
use pimu::emulator::{Emulator, RunEnd, RunLimits};
use pimu::net::BuiltinPeer;
use pimu::soc::bcm2711::{GENET_BASE, GPIO_BASE, UART0_BASE};
use pimu::spec::{genet, gpio};
use pimu::Machine;

const CODE: u32 = 0x1000;

const fn st(rd: u16, rs: u16) -> u16 {
    0x0900 | (rs << 4) | rd
}

/// Print `text` on the UART, then spin.
fn print_then_spin(m: &mut Machine, text: &str) -> u32 {
    let mut code = vec![0xE800 | 6, UART0_BASE as u16, (UART0_BASE >> 16) as u16];
    for b in text.bytes() {
        code.extend([0xE800 | 7, u16::from(b), 0]);
        code.push(st(7, 6));
    }
    let spin = CODE + 2 * code.len() as u32;
    code.push(0x1800 | (0xE << 7));
    for (i, h) in code.iter().enumerate() {
        m.store16(CODE + 2 * i as u32, *h).unwrap();
    }
    spin
}

/// What `bcmgenet_wol_power_down_cfg` leaves behind, with the MAC set to `mac`.
fn arm_wake_on_lan(m: &mut Machine, mac: [u8; 6]) {
    let at = |off| GENET_BASE + off;
    m.store32(GPIO_BASE + gpio::PIN_MUX, gpio::PIN_MUX_ETHERNET_MASK)
        .unwrap();
    m.store32(
        at(genet::UMAC_MAC0),
        u32::from_be_bytes(mac[..4].try_into().unwrap()),
    )
    .unwrap();
    m.store32(
        at(genet::UMAC_MAC1),
        u32::from(mac[4]) << 8 | u32::from(mac[5]),
    )
    .unwrap();
    m.store32(at(genet::UMAC_CMD), genet::UMAC_CMD_RX_EN_MASK)
        .unwrap();
    m.store32(at(genet::UMAC_MPD_CTRL), genet::UMAC_MPD_CTRL_EN_MASK)
        .unwrap();
    m.store32(at(genet::HFB_CTRL), genet::HFB_CTRL_ACPI_EN_MASK)
        .unwrap();
}

fn run(mac: [u8; 6], print: &str, after: Option<&str>) -> Emulator {
    let mut m = Machine::new(1 << 20);
    let spin = print_then_spin(&mut m, print);
    arm_wake_on_lan(&mut m, mac);
    m.attach_net(Box::new(BuiltinPeer::new()));
    let mut emu = Emulator::new(m, CODE);
    emu.stream_console = false;
    emu.input.magic_packet_after = after.map(str::to_string);
    let report = emu.run(&RunLimits {
        max_steps: Some(100_000),
        max_wall: Some(Duration::from_secs(60)),
        stop_pc: None,
        idle_spin_limit: 0,
        silent_us: 0,
        until: None,
        speed: None,
    });
    assert_eq!(report.end, RunEnd::StepLimit);
    assert_eq!(report.pc, spin, "the payload is spinning at its end");
    emu
}

const MAC: [u8; 6] = [0x02, 0x00, 0x5e, 0x00, 0x53, 0x01];

#[test]
fn the_packet_goes_out_once_the_console_has_printed_the_text() {
    let mut emu = run(MAC, "WoL armed\r\n", Some("armed"));
    assert_eq!(emu.machine.genet.stats.rx_wake, 1);
    assert_eq!(emu.machine.genet.stats.rx, 0);
    let stat = emu
        .machine
        .load32(GENET_BASE + genet::INTRL2_CPU_STAT)
        .unwrap();
    assert_ne!(stat & genet::INTRL2_CPU_STAT_MPD_R_MASK, 0);
    assert!(emu.input.magic_packet_after.is_none(), "taken once sent");
}

#[test]
fn the_packet_is_for_the_address_the_mac_holds_at_that_moment() {
    let other = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
    let emu = run(other, "WoL armed\r\n", Some("armed"));
    assert_eq!(emu.machine.genet.stats.rx_wake, 1);
    assert_eq!(emu.machine.genet.mac_state().addr, other);
}

#[test]
fn nothing_is_sent_while_the_text_has_not_been_printed() {
    let emu = run(MAC, "WoL armed\r\n", Some("never printed"));
    assert_eq!(emu.machine.genet.stats.rx_wake, 0);
    assert_eq!(
        emu.input.magic_packet_after.as_deref(),
        Some("never printed")
    );
}

#[test]
fn without_the_option_nothing_is_sent() {
    let emu = run(MAC, "WoL armed\r\n", None);
    assert_eq!(emu.machine.genet.stats.rx_wake, 0);
}

#[test]
fn a_backend_that_is_no_peer_sends_nothing() {
    struct Cable;
    impl pimu::net::NetBackend for Cable {
        fn name(&self) -> &'static str {
            "a cable to nowhere"
        }
        fn send(&mut self, _frame: &[u8]) {}
        fn recv(&mut self) -> Option<Vec<u8>> {
            None
        }
    }
    let mut m = Machine::new(1 << 20);
    m.attach_net(Box::new(Cable));
    assert!(!m.send_magic_packet());
    let mut peer = Machine::new(1 << 20);
    peer.attach_net(Box::new(BuiltinPeer::new()));
    assert!(peer.send_magic_packet());
    assert!(
        !Machine::new(1 << 20).send_magic_packet(),
        "no cable at all"
    );
}
