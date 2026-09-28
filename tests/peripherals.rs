//! Peripheral-model tests: the register values the boot gates on. A register
//! that reads back 0 is "this chip is absent" to the firmware, which then skips
//! a whole subsystem and fails hundreds of millions of instructions later. The
//! registers themselves are described in `specs/*.toml` and `docs/periph/`.

use pimu::bus::{Bus, MmioDevice, Width};
use pimu::periph::Spi0;
use pimu::soc::bcm2711 as map;
use pimu::soc::{Board, Stepping};
use pimu::Machine;

fn machine() -> Machine {
    Machine::new(1024 * 1024)
}

/// `SCALER_DISPID`: start4 skips `hdmi_init` entirely unless it reads
/// `0x64647276`, the value a real Raspberry Pi 4B's `hvs_regs` gives.
#[test]
fn scaler_dispid_identifies_the_hvs() {
    let mut m = machine();
    assert_eq!(m.load32(map::HVS_BASE + 0x08).unwrap(), 0x6464_7276);

    m.store32(map::HVS_BASE + 0x08, 0).unwrap();
    assert_eq!(
        m.load32(map::HVS_BASE + 0x08).unwrap(),
        0x6464_7276,
        "SCALER_DISPID must not be clobberable"
    );
}

/// The HVS frame-swap wait polls `current` until it matches `requested`.
#[test]
fn hvs_frame_swap_completes_immediately() {
    let mut m = machine();
    for chan in 0..4u32 {
        m.store32(map::HVS_BASE + 0x20 + 4 * chan, 0x1234 + chan)
            .unwrap();
        assert_eq!(
            m.load32(map::HVS_BASE + 0x30 + 4 * chan).unwrap(),
            0x1234 + chan,
            "channel {chan} swap never reports complete"
        );
    }
}

/// start4's USB power-on, none of whose spins has a timeout. The values are what
/// a Raspberry Pi 4B d03115 reads before and after the same request.
#[test]
fn usb_power_on_handshake_completes() {
    let mut m = machine();
    let (ctrl, status) = (map::USBR_BASE + 0x08, map::USBR_BASE + 0x20);
    assert_eq!(m.load32(ctrl).unwrap(), 0x3);
    assert_eq!(m.load32(status).unwrap(), 0x0);

    let idle = m.load32(ctrl).unwrap();
    m.store32(ctrl, idle | 4).unwrap();
    assert_eq!(m.load32(status).unwrap(), 0x3, "power never acknowledged");
    m.store32(ctrl, idle).unwrap();
    assert_eq!(m.load32(status).unwrap(), 0x0);

    let grstctl = map::DWC2_BASE + 0x10;
    for (write, bit) in [(0x1, 0x1), (0x2, 0x2), (0x420, 0x20), (0x10, 0x10)] {
        m.store32(grstctl, write).unwrap();
        assert_eq!(
            m.load32(grstctl).unwrap() & bit,
            0,
            "GRSTCTL {write:#x} never completes"
        );
    }
    assert_eq!(m.load32(grstctl).unwrap(), 0x8000_0000);
}

/// `GSNPSID` names the core: OTG 2.80a on a Raspberry Pi 4B.
#[test]
fn dwc2_identifies_itself() {
    let mut m = machine();
    assert_eq!(m.load32(map::DWC2_BASE + 0x40).unwrap(), 0x4F54_280A);
    let hwcfg2 = m.load32(map::DWC2_BASE + 0x48).unwrap();
    assert_eq!((hwcfg2 >> 14 & 0xF) + 1, 8);
}

/// What UEFI's `DwUsbHostDxe` does once USB has power. A `CHENA` that never
/// clears costs ten guest seconds per channel.
#[test]
fn dwc2_host_channels_halt_and_the_port_is_empty() {
    let mut m = machine();
    let base = map::DWC2_BASE;
    m.store32(base + 0x0C, 0x2040_2700).unwrap();
    assert_eq!(m.load32(base + 0x14).unwrap() & 1, 1, "not in host mode");

    for ch in 0..8 {
        let (hcchar, hcint) = (base + 0x500 + ch * 0x20, base + 0x508 + ch * 0x20);
        m.store32(hcchar, 0x4000_0000).unwrap();
        m.store32(hcchar, 0xC000_0000).unwrap();
        assert_eq!(
            m.load32(hcchar).unwrap() & 0x8000_0000,
            0,
            "channel {ch} never halts"
        );
        assert_eq!(m.load32(hcint).unwrap(), 0x2, "no CHHLTD on channel {ch}");
        m.store32(hcint, 0x2).unwrap();
        assert_eq!(m.load32(hcint).unwrap(), 0);
    }

    // Port power sticks, and the change bits do not make it look connected.
    m.store32(base + 0x440, 0x1000 | 0x2F).unwrap();
    assert_eq!(m.load32(base + 0x440).unwrap(), 0x1000);

    m.store32(base + 0x0C, 0x4040_2700).unwrap();
    assert_eq!(m.load32(base + 0x14).unwrap() & 1, 0, "forced device mode");
}

const CS: u32 = 0x00;
const FIFO: u32 = 0x04;
const CS_TA: u32 = 1 << 7;
const CS_CLEAR_RX: u32 = 1 << 5;
const CS_CLEAR_TX: u32 = 1 << 4;
const CS_DONE: u32 = 1 << 16;
const CS_RXD: u32 = 1 << 17;
const CS_DMAEN: u32 = 1 << 8;
const CS_INTD: u32 = 1 << 9;
const CS_INTR: u32 = 1 << 10;
/// `CS.CS = 0b11` is the invalid native select Linux's `spi-bcm2835` parks the
/// hardware one at, because it works the select as a GPIO instead.
const CS_NO_NATIVE: u32 = 0x3;

/// SPI0 `CS.DONE` reflects the TX side only: start4's EEPROM scanner checks it
/// with received bytes still queued, and reads `DONE = 0` as a transfer error.
#[test]
fn spi0_done_is_tx_side_only() {
    let mut spi = Spi0::new();
    spi.attach_flash(vec![0xAA; 0x1000]);

    spi.write(CS, Width::Word, CS_TA | CS_CLEAR_RX | CS_CLEAR_TX)
        .unwrap();
    for byte in [0x03, 0x00, 0x00, 0x10, 0xFF] {
        spi.write(FIFO, Width::Word, byte).unwrap();
    }

    let cs = spi.read(CS, Width::Word).unwrap();
    assert_ne!(cs & CS_RXD, 0, "received bytes are waiting");
    assert_ne!(
        cs & CS_DONE,
        0,
        "DONE must be set with the transfer active, RX FIFO or not"
    );

    for _ in 0..4 {
        spi.read(FIFO, Width::Word).unwrap(); // command + address echoes
    }
    assert_eq!(spi.read(FIFO, Width::Word).unwrap(), 0xAA);
}

/// `RDID` names the part `flashrom` finds over `/dev/spidev0.0`: a Winbond
/// W25X40, the 512 KiB boot flash of a Raspberry Pi 4B d03115.
#[test]
fn spi0_flash_reports_the_parts_jedec_id() {
    let mut spi = Spi0::new();
    spi.attach_flash(vec![0xFF; 0x8_0000]);

    spi.write(CS, Width::Word, CS_TA | CS_CLEAR_RX | CS_CLEAR_TX)
        .unwrap();
    for byte in [0x9F, 0, 0, 0] {
        spi.write(FIFO, Width::Word, byte).unwrap();
    }
    spi.read(FIFO, Width::Word).unwrap(); // the command's own beat
    let id: Vec<u32> = (0..3)
        .map(|_| spi.read(FIFO, Width::Word).unwrap())
        .collect();
    assert_eq!(id, vec![0xEF, 0x30, 0x13]);
}

/// SPI0's interrupt, which `spi-bcm2835` needs for every transfer between the
/// polling limit and the DMA one: `DONE` under `CS.INTD`, the receive FIFO's
/// three-quarter mark under `CS.INTR`, and `PACTL_CS` bit 0 naming the master.
#[test]
fn spi0_drives_its_interrupt_and_pactl_names_it() {
    let mut m = machine();
    let pactl = map::PACTL_BASE;
    assert_eq!(m.load32(pactl).unwrap(), 0, "idle: nothing pending");

    m.store32(map::SPI0_BASE + CS, CS_TA).unwrap();
    assert_eq!(m.load32(pactl).unwrap(), 0, "no interrupt is enabled yet");

    m.store32(map::SPI0_BASE + CS, CS_TA | CS_INTD).unwrap();
    assert_eq!(m.load32(pactl).unwrap(), 1, "DONE under INTD");

    // `INTR` alone needs the receive FIFO three-quarters full, which is 48 of
    // its 64 bytes.
    m.store32(map::SPI0_BASE + CS, CS_TA | CS_INTR | CS_CLEAR_RX)
        .unwrap();
    assert_eq!(m.load32(pactl).unwrap(), 0, "an empty RX FIFO is not RXR");
    for _ in 0..FIFO_THREE_QUARTERS {
        m.store32(map::SPI0_BASE + FIFO, 0).unwrap();
    }
    assert_eq!(m.load32(pactl).unwrap(), 1, "RXR under INTR");
}

const FIFO_THREE_QUARTERS: usize = 48;

/// Put GPIO 40..42 on ALT4 and 43 on the output the `spi-gpio40-45` overlay
/// asks for, which is what reaches the boot flash from Linux.
fn route_flash_pins(m: &mut Machine) {
    let gpfsel4 = map::GPIO_BASE + 0x10;
    let alt4 = 0b011;
    let output = 0b001;
    m.store32(gpfsel4, alt4 | alt4 << 3 | alt4 << 6 | output << 9)
        .unwrap();
}

const GPSET1: u32 = map::GPIO_BASE + 0x1C + 4;
const GPCLR1: u32 = map::GPIO_BASE + 0x28 + 4;
/// GPIO 43 in bank 1.
const CS0_PIN: u32 = 1 << 11;

/// The flash follows its chip-select pin, not `CS.TA`: `spi-bcm2835` holds the
/// GPIO select down for a whole message and raises and drops `TA` once per
/// transfer inside it, so a command whose address and data are separate
/// transfers has to survive `TA` going away in between.
#[test]
fn spi0_gpio_chip_select_spans_several_transfers() {
    let mut m = machine();
    let mut image = vec![0xFF; 0x8_0000];
    image[0x1234] = 0x5A;
    m.spi0.attach_flash(image);
    route_flash_pins(&mut m);

    // Nothing is selected while the GPIO select is up, whatever `TA` says.
    m.store32(GPSET1, CS0_PIN).unwrap();
    m.store32(map::SPI0_BASE + CS, CS_TA | CS_NO_NATIVE)
        .unwrap();
    for byte in [0x03, 0x00, 0x12, 0x34] {
        m.store32(map::SPI0_BASE + FIFO, byte).unwrap();
    }
    m.store32(map::SPI0_BASE + FIFO, 0).unwrap();
    for _ in 0..4 {
        m.load32(map::SPI0_BASE + FIFO).unwrap();
    }
    assert_eq!(
        m.load32(map::SPI0_BASE + FIFO).unwrap(),
        0xFF,
        "a deselected flash drives nothing"
    );

    // Select, then the command and address as one transfer and the data as the
    // next, `TA` dropped in between.
    m.store32(GPCLR1, CS0_PIN).unwrap();
    m.store32(map::SPI0_BASE + CS, CS_TA | CS_NO_NATIVE)
        .unwrap();
    for byte in [0x03, 0x00, 0x12, 0x34] {
        m.store32(map::SPI0_BASE + FIFO, byte).unwrap();
    }
    for _ in 0..4 {
        m.load32(map::SPI0_BASE + FIFO).unwrap();
    }
    m.store32(
        map::SPI0_BASE + CS,
        CS_NO_NATIVE | CS_CLEAR_RX | CS_CLEAR_TX,
    )
    .unwrap();

    m.store32(map::SPI0_BASE + CS, CS_TA | CS_NO_NATIVE)
        .unwrap();
    m.store32(map::SPI0_BASE + FIFO, 0).unwrap();
    assert_eq!(
        m.load32(map::SPI0_BASE + FIFO).unwrap(),
        0x5A,
        "the command must survive TA dropping mid-message"
    );
    m.store32(GPSET1, CS0_PIN).unwrap();
}

/// `CS.DMAEN` makes `FIFO` 32 bits wide — four bytes a word, low byte first —
/// and `DLEN` is what ends the transfer: the driver's filler descriptor keeps
/// writing words long past it, and none of those may reach the flash.
#[test]
fn spi0_dma_fifo_is_four_bytes_wide_and_stops_at_dlen() {
    let mut m = machine();
    let mut image = vec![0xFF; 0x8_0000];
    image[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    m.spi0.attach_flash(image);
    route_flash_pins(&mut m);
    m.store32(GPCLR1, CS0_PIN).unwrap();

    // One four-byte word carries `READ 0x000000`, low byte first.
    m.store32(map::SPI0_BASE + 0x0C, 4).unwrap(); // DLEN
    m.store32(map::SPI0_BASE + CS, CS_TA | CS_NO_NATIVE | CS_DMAEN)
        .unwrap();
    m.store32(map::SPI0_BASE + FIFO, 0x0000_0003).unwrap();
    // Everything past `DLEN` is the filler, and shifts nothing.
    for _ in 0..4 {
        m.store32(map::SPI0_BASE + FIFO, 0).unwrap();
    }
    assert_eq!(
        m.load32(map::SPI0_BASE + FIFO).unwrap(),
        0xFFFF_FFFF,
        "four idle beats, one a word"
    );
    assert_eq!(
        m.load32(map::SPI0_BASE + FIFO).unwrap(),
        0xFFFF_FFFF,
        "nothing was clocked after DLEN"
    );

    // The read's data then comes with `DLEN` set to its own length.
    m.store32(
        map::SPI0_BASE + CS,
        CS_NO_NATIVE | CS_CLEAR_RX | CS_CLEAR_TX,
    )
    .unwrap();
    m.store32(map::SPI0_BASE + 0x0C, 8).unwrap();
    m.store32(map::SPI0_BASE + CS, CS_TA | CS_NO_NATIVE | CS_DMAEN)
        .unwrap();
    for _ in 0..4 {
        m.store32(map::SPI0_BASE + FIFO, 0).unwrap();
    }
    assert_eq!(m.load32(map::SPI0_BASE + FIFO).unwrap(), 0x0403_0201);
    assert_eq!(m.load32(map::SPI0_BASE + FIFO).unwrap(), 0x0807_0605);
    m.store32(GPSET1, CS0_PIN).unwrap();
}

/// Send one command over the flash's GPIO chip select, as a `spi_message` of
/// its own, and give back what came in.
fn flash_command(m: &mut Machine, bytes: &[u8]) -> Vec<u8> {
    m.store32(GPCLR1, CS0_PIN).unwrap();
    m.store32(map::SPI0_BASE + CS, CS_TA | CS_NO_NATIVE)
        .unwrap();
    for &b in bytes {
        m.store32(map::SPI0_BASE + FIFO, b as u32).unwrap();
    }
    let got = bytes
        .iter()
        .map(|_| m.load32(map::SPI0_BASE + FIFO).unwrap() as u8)
        .collect();
    m.store32(
        map::SPI0_BASE + CS,
        CS_NO_NATIVE | CS_CLEAR_RX | CS_CLEAR_TX,
    )
    .unwrap();
    m.store32(GPSET1, CS0_PIN).unwrap();
    got
}

/// The write `flashrom -w` makes: read the status register, unlock nothing,
/// then per block `WREN` + erase and per page `WREN` + `PP`, polling `WIP`
/// between. Each is a message of its own, so the write-enable latch has to
/// survive one and be spent by the next.
#[test]
fn spi0_flash_erases_and_programs_the_way_flashrom_writes() {
    let mut m = machine();
    let mut image = vec![0x00; 0x8_0000];
    image[0x2_0000] = 0x11;
    m.spi0.attach_flash(image);
    route_flash_pins(&mut m);

    let status = flash_command(&mut m, &[0x05, 0]);
    assert_eq!(status[1], 0, "no WIP and no block protection out of reset");

    // A program with no `WREN` in front of it does nothing.
    flash_command(&mut m, &[0x02, 0x00, 0x00, 0x00, 0xAB]);
    assert_eq!(m.spi0.flash_bytes()[0], 0x00, "PP needs the latch");

    // 64 KiB block erase, then the page program the erase makes possible.
    flash_command(&mut m, &[0x06]);
    assert_eq!(
        flash_command(&mut m, &[0x05, 0])[1] & 2,
        2,
        "WEL is up between the two messages"
    );
    flash_command(&mut m, &[0xD8, 0x02, 0x00, 0x00]);
    assert_eq!(m.spi0.flash_bytes()[0x2_0000], 0xFF, "the block is erased");
    assert_eq!(m.spi0.flash_bytes()[0x1_FFFF], 0x00, "and only that block");
    assert_eq!(
        flash_command(&mut m, &[0x05, 0])[1] & 2,
        0,
        "the erase spent the latch"
    );

    flash_command(&mut m, &[0x06]);
    flash_command(&mut m, &[0x02, 0x02, 0x00, 0x00, 0xA5, 0x5A]);
    assert_eq!(&m.spi0.flash_bytes()[0x2_0000..0x2_0002], &[0xA5, 0x5A]);
    assert!(m.spi0.dirty, "the run loop is told the image moved");
}

/// Core 1's copies of the core-control registers live at `+0x800`, so too small
/// a window drops them all on the stub and core 1 enables no interrupts.
#[test]
fn core1_registers_are_inside_the_corectl_window() {
    const _: () = assert!(map::CORECTL_SIZE > 0x844);

    let mut m = machine();
    let before = m.stub_hits;
    for off in [0x800, 0x810, 0x81C, 0x830, 0x840, 0x844] {
        m.store32(map::CORECTL_BASE + off, 0x0001_4000).unwrap();
        m.load32(map::CORECTL_BASE + off).unwrap();
        assert_eq!(
            m.stub_hits, before,
            "core 1's register +{off:#x} fell through to the peripheral stub"
        );
    }

    m.store32(map::CORECTL_BASE + 0x30, 0x0002_8000).unwrap();
    assert_eq!(m.corectl.vbase[0], 0x0002_8000);
}

/// Core 1's vector base is one 0x800 stride above core 0's.
#[test]
fn core1_vector_base_is_recorded_from_the_strided_offset() {
    let mut m = machine();

    m.store32(map::CORECTL_BASE + 0x830, 0xFEC0_1E00).unwrap();
    assert_eq!(m.corectl.vbase[1], 0xFEC0_1E00);

    let mut m = machine();
    m.store32(map::CORECTL_BASE + 0x38, 0xFEC0_1E00).unwrap();
    assert_eq!(m.corectl.vbase[1], 0);
}

/// The firmware raises an interrupt by setting its bit in that core's pending
/// word; dropping the write starves every software-posted interrupt.
#[test]
fn software_posted_interrupts_are_queued_per_core() {
    let mut m = machine();

    m.store32(map::CORECTL_BASE + 0x40, 1 << 2).unwrap(); // source 64 + 2
    m.store32(map::CORECTL_BASE + 0x840, 1 << 15).unwrap(); // core 1, source 79

    assert_eq!(m.corectl.take_sw_raised(), Some((0, 66)));
    assert_eq!(m.corectl.take_sw_raised(), Some((1, 79)));
    assert_eq!(m.corectl.take_sw_raised(), None);

    m.store32(map::CORECTL_BASE + 0x40, 0).unwrap();
    assert_eq!(m.corectl.take_sw_raised(), None);
}

/// Each bank has its own read-to-clear `IRQ_PENDING` (`+0x04` / `+0x804`).
#[test]
fn irq_pending_is_per_core_and_read_to_clear() {
    let mut m = machine();

    m.store32(map::CORECTL_BASE + 0x814, 1 << 28).unwrap();
    m.store32(map::CORECTL_BASE + 0x010, 1 << 8).unwrap();

    m.corectl.raise_source(1, 79);
    assert_eq!(
        m.load32(map::CORECTL_BASE + 0x04).unwrap(),
        0,
        "core 0's copy"
    );
    assert_eq!(m.load32(map::CORECTL_BASE + 0x804).unwrap(), 0x014F_014F);
    assert_eq!(
        m.load32(map::CORECTL_BASE + 0x804).unwrap(),
        0,
        "read-to-clear"
    );

    m.corectl.raise_source(0, 66);
    assert_eq!(
        m.load32(map::CORECTL_BASE + 0x804).unwrap(),
        0,
        "core 1's copy"
    );
    assert_eq!(m.load32(map::CORECTL_BASE + 0x04).unwrap(), 0x0142_0142);
}

/// `enable_irq_source(src, prio)`: `word = (src >> 3) & 3`, `field = (src & 7) * 4`.
#[test]
fn interrupt_priority_fields_round_trip() {
    let mut m = machine();

    m.store32(map::CORECTL_BASE + 0x10, 1 | (3 << 8)).unwrap();

    assert_eq!(m.corectl.irq_priority(0, 64), 1);
    assert_eq!(m.corectl.irq_priority(0, 66), 3);
    assert_eq!(m.corectl.irq_priority(0, 65), 0, "unenabled source");
    assert_eq!(m.corectl.irq_priority(0, 72), 0, "next word along");

    m.store32(map::CORECTL_BASE + 0x814, 1 << 28).unwrap();
    assert_eq!(m.corectl.irq_priority(1, 79), 1);
    assert_eq!(m.corectl.irq_priority(0, 79), 0, "core 0's copy");
}

// --- board PMICs on the BSC at 0x7E20_5E00 ---------------------------------

const BSC_C: u32 = 0x00;
const BSC_S: u32 = 0x04;
const BSC_DLEN: u32 = 0x08;
const BSC_A: u32 = 0x0C;
const BSC_FIFO: u32 = 0x10;

const C_READ: u32 = 1 << 0;
const C_ST: u32 = 1 << 7;
const C_I2CEN: u32 = 1 << 15;
const S_DONE: u32 = 1 << 1;
const S_ERR: u32 = 1 << 8;

/// Let a transfer settle: `S.DONE` lands only once the bytes have clocked out.
fn settle(m: &mut Machine) {
    m.tick(10_000 * 54); // 54 VPU cycles per microsecond
}

/// `read(reg)` the way the `0x1B` transport path does it: FIFO fed after `ST`.
fn pmic_read(m: &mut Machine, addr: u8, reg: u8) -> u8 {
    let base = map::BSC_PMIC_BASE;
    m.store32(base + BSC_A, addr as u32).unwrap();
    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST).unwrap();
    m.store32(base + BSC_FIFO, reg as u32).unwrap();
    settle(m);
    m.store32(base + BSC_S, S_DONE | S_ERR).unwrap();

    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST | C_READ).unwrap();
    settle(m);
    let v = m.load32(base + BSC_FIFO).unwrap() as u8;
    m.store32(base + BSC_S, S_DONE | S_ERR).unwrap();
    v
}

/// The same read on the `0x1E` path, which programs the read phase *before*
/// pushing the register byte and relies on the write phase stalling.
fn pmic_read_late_fifo(m: &mut Machine, addr: u8, reg: u8) -> u8 {
    let base = map::BSC_PMIC_BASE;
    m.store32(base + BSC_A, addr as u32).unwrap();
    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST).unwrap();
    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST | C_READ).unwrap();
    m.store32(base + BSC_FIFO, reg as u32).unwrap();
    settle(m);
    let v = m.load32(base + BSC_FIFO).unwrap() as u8;
    m.store32(base + BSC_S, S_DONE | S_ERR).unwrap();
    v
}

fn pmic_write(m: &mut Machine, addr: u8, reg: u8, value: u8) {
    let base = map::BSC_PMIC_BASE;
    m.store32(base + BSC_A, addr as u32).unwrap();
    m.store32(base + BSC_DLEN, 2).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST).unwrap();
    m.store32(base + BSC_FIFO, reg as u32).unwrap();
    m.store32(base + BSC_FIFO, value as u32).unwrap();
    settle(m);
    m.store32(base + BSC_S, S_DONE | S_ERR).unwrap();
}

/// The register asked for is the register got: the transport writes `C.ST` before
/// it feeds the FIFO, so running the transfer at `ST` selects nothing.
#[test]
fn pmic_register_pointer_follows_the_late_fifo_byte() {
    let mut m = machine();
    // 0x1B reg 0x09 is the SDRAM rail setpoint, shared by rails 2, 3 and 4.
    assert_eq!(pmic_read(&mut m, 0x1B, 0x09), 40);
    assert_eq!(pmic_read_late_fifo(&mut m, 0x1B, 0x09), 40);
    // 0x1E reg 0x25 is the SoC core rail setpoint.
    assert_eq!(pmic_read_late_fifo(&mut m, 0x1E, 0x25), 85);
    assert_eq!(pmic_read(&mut m, 0x1E, 0x25), 85);
}

/// Seeded setpoints decode to the voltages a Raspberry Pi 4B d03115 reports.
#[test]
fn pmic_setpoints_decode_to_the_real_boards_voltages() {
    let mut m = machine();

    // 0x1B rails 2..4 (`0x3EC8C710`): raw * 5_000 + 900_000 µV.
    let sdram = pmic_read(&mut m, 0x1B, 0x09) as u32 * 5_000 + 900_000;
    assert_eq!(
        sdram, 1_100_000,
        "vcgencmd measure_volts sdram_c on a Raspberry Pi 4B d03115"
    );

    // 0x1E rail 1 (`0x3EC8C9F6`): raw * 10_000 µV, within the descriptor's
    // 0.3 V..1.9 V range.
    let core = pmic_read(&mut m, 0x1E, 0x25) as u32 * 10_000;
    assert!(
        (300_000..=1_900_000).contains(&core),
        "core setpoint {core} outside the descriptor range"
    );
}

/// A setpoint write is read back and latches the "voltage settled" bit each
/// part's post-set callback polls, which has no timeout.
#[test]
fn pmic_setpoint_write_latches_the_settled_bit() {
    let mut m = machine();

    // 0x6E = 110 -> 1.10 V, the top of the band this boot's DVFS actually uses.
    pmic_write(&mut m, 0x1E, 0x25, 0x6E);
    assert_eq!(pmic_read(&mut m, 0x1E, 0x25), 0x6E);
    assert_ne!(pmic_read(&mut m, 0x1E, 0x02) & 0x08, 0, "0x1E settled bit");

    pmic_write(&mut m, 0x1B, 0x09, 40);
    assert_ne!(pmic_read(&mut m, 0x1B, 0x00) & 0x10, 0, "0x1B settled bit");
}

/// The two parts are separate register files even though they share a bus.
#[test]
fn pmic_addresses_are_separate_register_files() {
    let mut m = machine();
    pmic_write(&mut m, 0x1E, 0x40, 0x5A);
    assert_eq!(pmic_read(&mut m, 0x1E, 0x40), 0x5A);
    assert_eq!(pmic_read(&mut m, 0x1B, 0x40), 0x00);
}

/// A 4B up to rev 1.4 has one PMIC, at `0x1D`, in place of rev 1.5's pair.
#[test]
fn a_rev_1_2_board_has_the_one_0x1d_pmic() {
    let mut m = machine();
    m.set_board(Board::for_stepping(Stepping::B0));
    let pmic = m.bsc_pmic.slave().unwrap();
    assert!(pmic.responds_to(0x1D));
    assert!(!pmic.responds_to(0x1B) && !pmic.responds_to(0x1E));

    assert_eq!(
        pmic_read(&mut m, 0x1D, 0x1A) & 0x60,
        0x20,
        "0x1D power good"
    );
    assert_eq!(pmic_read(&mut m, 0x1D, 0x13) as u32 * 6_250, 1_100_000);
    let core = pmic_read(&mut m, 0x1D, 0x14);
    assert_eq!(pmic_read(&mut m, 0x1D, 0x0F), core ^ 0xAD);

    pmic_write(&mut m, 0x1D, 0x14, 0x90);
    assert_eq!(pmic_read(&mut m, 0x1D, 0x14), 0x90);
    assert_ne!(pmic_read(&mut m, 0x1D, 0x1A) & 0x10, 0, "0x1D settled bit");
    assert_eq!(
        m.bsc_pmic.slave().unwrap().core_rail_uv(),
        Some(0x90 * 6_250)
    );
}

/// The FXL6408 expander shares the bus at `0x43`; register `0x01` is its
/// Fairchild manufacturer id on the real part.
#[test]
fn gpio_expander_answers_on_the_pmic_bus() {
    let mut m = machine();
    assert_eq!(pmic_read(&mut m, 0x43, 0x01) >> 5, 0b101);
}

/// Nothing else is on this bus, and an unACKed transfer has to complete
/// `DONE | ERR` rather than spin.
#[test]
fn pmic_bus_nacks_every_other_address() {
    let mut m = machine();
    let base = map::BSC_PMIC_BASE;
    m.store32(base + BSC_A, 0x10).unwrap();
    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST | C_READ).unwrap();
    settle(&mut m);
    let s = m.load32(base + BSC_S).unwrap();
    assert_ne!(s & S_DONE, 0, "transfer must complete");
    assert_ne!(s & S_ERR, 0, "unACKed address must raise ERR");
}

/// `S.TA` spans the transfer and `S.DONE` lands only at its end, neither a
/// function of instructions retired: the transport's spin on it has no timeout.
#[test]
fn bsc_transfer_active_spans_the_whole_transfer() {
    let mut m = machine();
    let base = map::BSC_PMIC_BASE;
    const S_TA: u32 = 1 << 0;

    m.store32(base + BSC_A, 0x1E).unwrap();
    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST).unwrap();

    // Stalled on an empty FIFO: active, and staying active however long the
    // firmware takes to get round to it — 10 ms here, ~540 000 instructions.
    for _ in 0..1000 {
        m.tick(10 * 54);
        let s = m.load32(base + BSC_S).unwrap();
        assert_ne!(s & S_TA, 0, "TA must be held while the FIFO is empty");
        assert_eq!(s & S_DONE, 0, "nothing has been transferred yet");
    }

    m.store32(base + BSC_FIFO, 0x25).unwrap();
    let s = m.load32(base + BSC_S).unwrap();
    assert_ne!(s & S_TA, 0, "the last byte is still clocking out");
    assert_eq!(s & S_DONE, 0, "DONE must not land inside the FIFO write");

    // At 100 kHz (DIV defaults to 5000 against the 500 MHz core clock) that
    // byte takes 90 µs. Well short of it, nothing has changed.
    m.tick(50 * 54);
    assert_eq!(
        m.load32(base + BSC_S).unwrap() & S_DONE,
        0,
        "DONE cannot precede the bits going out"
    );

    settle(&mut m);
    let s = m.load32(base + BSC_S).unwrap();
    assert_eq!(s & S_TA, 0, "transfer over");
    assert_ne!(s & S_DONE, 0, "DONE latches at the end");
    assert_eq!(s & S_ERR, 0, "the PMIC acknowledged its address");
    assert_eq!(m.load32(base + BSC_DLEN).unwrap(), 0, "all bytes sent");
}

/// `DONE` under `C.INTD` holds source 117 up until `S` is cleared, and a
/// `sleep` stops the counter where the transfer lands; without `INTD` the
/// same transfer raises nothing.
#[test]
fn bsc_done_under_intd_raises_source_117() {
    const C_INTD: u32 = 1 << 8;
    let mut m = machine();
    let base = map::BSC_PMIC_BASE;
    m.store32(map::CORECTL_BASE + 0x10 + 4 * 6, 1 << 20)
        .unwrap();

    m.store32(base + BSC_A, 0x1E).unwrap();
    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST).unwrap();
    m.store32(base + BSC_FIFO, 0x25).unwrap();
    assert_eq!(
        m.bsc_pmic.irq_deadline(),
        None,
        "no INTD, nothing to wake for"
    );
    settle(&mut m);
    assert!(!m.bsc_pmic.irq_line() && !m.irq_queued());
    m.store32(base + BSC_S, S_DONE | S_ERR).unwrap();

    m.store32(base + BSC_C, C_I2CEN | C_ST | C_INTD).unwrap();
    m.store32(base + BSC_FIFO, 0x25).unwrap();
    let due = m.bsc_pmic.irq_deadline().expect("a transfer on the wire");
    assert_eq!(
        pimu::sched::next_due(&m).map(|(_, which)| which),
        Some(pimu::sched::Timed::I2c)
    );
    assert!(!m.bsc_pmic.irq_line(), "not before the byte is out");
    settle(&mut m);
    assert!(m.systimer.now_us() >= due);
    assert!(m.bsc_pmic.irq_line() && m.irq_queued());
    assert_eq!(m.load32(map::PACTL_BASE).unwrap() & 1 << 8, 0, "not I2C0");

    m.store32(base + BSC_S, S_DONE | S_ERR).unwrap();
    m.tick(54);
    assert!(!m.bsc_pmic.irq_line());
    assert!(
        !m.irq_queued(),
        "a line that dropped is not delivered later"
    );
}

/// The VCE launch handshake (`vcfw/drivers/chip/vciv/2708/vce.c`).
#[test]
fn vce_launch_completes_and_raises_its_interrupt() {
    use pimu::periph::vce;

    let mut m = machine();
    let ctrl = map::VCE_CTRL_BASE;

    // Idle: the two bits `vce_obtain_semaphore` asserts are clear read clear.
    assert_eq!(m.load32(ctrl).unwrap(), 0);
    assert!(!m.vce.irq_asserted());

    // A licence-check launch: flags 0xC0000000, so no ENDCODE_ENABLE is written.
    m.store32(ctrl + 0x08, 0x1234).unwrap(); // start pc
    m.store32(ctrl + 0x24, 0xFF).unwrap(); // INTCLR
    m.store32(ctrl + 0x20, 1).unwrap(); // RUN

    let status = m.load32(ctrl).unwrap();
    assert_eq!(
        status >> 16 & 0x1F,
        0,
        "vce_run_complete requires the endcode it asked for"
    );
    assert_ne!(status & (1 << 31), 0, "completion must flag an interrupt");
    assert_eq!(
        m.load32(ctrl + 0x30).unwrap(),
        0,
        "BAD_ADDR must stay clear"
    );
    assert_eq!(m.load32(ctrl + 0x08).unwrap(), 0x1234, "PC0 reads back");
    assert!(
        m.vce.irq_asserted(),
        "source {} has to be driven or the event flag is never set",
        vce::IRQ_SRC
    );

    m.store32(ctrl + 0x24, 0x8000_0000).unwrap();
    assert_eq!(m.load32(ctrl).unwrap() & (1 << 31), 0);
    assert!(!m.vce.irq_asserted());
    assert_eq!(m.load32(ctrl).unwrap() >> 16 & 0x1F, 0);
}

/// A non-zero endcode has to come back, or `vce_run_complete` retries forever.
#[test]
fn vce_reports_the_endcode_the_launch_armed() {
    let mut m = machine();
    let ctrl = map::VCE_CTRL_BASE;

    m.store32(ctrl + 0x24, 0xFF).unwrap();
    m.store32(ctrl + 0x28, (1 << 3) | 0x20).unwrap();
    m.store32(ctrl + 0x20, 1).unwrap();
    assert_eq!(m.load32(ctrl).unwrap() >> 16 & 0x1F, 3);

    // The next launch does not re-arm the mask: it must not leak in.
    m.store32(ctrl + 0x24, 0xFF).unwrap();
    m.store32(ctrl + 0x20, 1).unwrap();
    assert_eq!(m.load32(ctrl).unwrap() >> 16 & 0x1F, 0);
}

/// The codec licence check reads VCE register 2; the compute core is not emulated,
/// so a run leaves it zeroed — "no match", as a real Raspberry Pi 4B reports.
#[test]
fn vce_register_file_round_trips_but_a_run_consumes_it() {
    let mut m = machine();
    let regs = map::VCE_BASE + 0x2_0000;

    m.store32(regs + 4 * 2, 0x137A_FEDA ^ 0x4D50_4732).unwrap();
    assert_eq!(m.load32(regs + 4 * 2).unwrap(), 0x137A_FEDA ^ 0x4D50_4732);

    m.store32(map::VCE_CTRL_BASE + 0x20, 1).unwrap();
    assert_eq!(
        m.load32(regs + 4 * 2).unwrap(),
        0,
        "getreg(2) == 0 is 'licence key does not match', which is the measured \
         answer for a board with blank OTP rows 45/46"
    );
}

/// Program (`0x7F11_0000`) and data (`0x7F10_0000`) memory are real storage,
/// written and read a byte at a time when the host buffer is unaligned.
#[test]
fn vce_program_and_data_memory_are_writable() {
    let mut m = machine();
    m.store32(map::VCE_BASE + 0x1_0000, 0xDEAD_BEEF).unwrap();
    assert_eq!(m.load32(map::VCE_BASE + 0x1_0000).unwrap(), 0xDEAD_BEEF);

    m.store(map::VCE_BASE + 0x0104, Width::Byte, 0xA5).unwrap();
    assert_eq!(
        m.load(map::VCE_BASE + 0x0104, Width::Byte).unwrap(),
        0xA5,
        "data memory must take byte stores"
    );
    assert_eq!(m.load32(map::VCE_BASE).unwrap(), 0);
}

/// The ASB bridge handshake at `0x7E00_A000`: the power-domain switch spins
/// until `ASB_ACK` follows `ASB_REQ_STOP`, and until it drops again on release.
#[test]
fn asb_ack_follows_the_stop_request() {
    let mut m = machine();

    for off in [0x08, 0x0C, 0x10, 0x14, 0x18, 0x1C] {
        let reg = map::ASB_BASE + off;

        let v = m.load32(reg).unwrap();
        assert_eq!(v & 0b11, 0, "{reg:#x} must come up running");

        let v = m.load32(reg).unwrap() | 1;
        m.store32(reg, v).unwrap();
        assert_ne!(
            m.load32(reg).unwrap() & 0b10,
            0,
            "{reg:#x} never acknowledged the stop request"
        );

        let v = m.load32(reg).unwrap() & !1;
        m.store32(reg, v).unwrap();
        assert_eq!(
            m.load32(reg).unwrap() & 0b10,
            0,
            "{reg:#x} never dropped its acknowledge"
        );
    }
}

/// The bridges are independent, or a release spins on the wrong one.
#[test]
fn asb_bridges_are_independent() {
    let mut m = machine();

    m.store32(map::ASB_BASE + 0x18, 1).unwrap();
    assert_eq!(m.load32(map::ASB_BASE + 0x18).unwrap() & 0b11, 0b11);
    for off in [0x08, 0x0C, 0x10, 0x14, 0x1C] {
        assert_eq!(
            m.load32(map::ASB_BASE + off).unwrap() & 0b11,
            0,
            "bridge +{off:#x} moved with the H264 slave bridge"
        );
    }
}

/// `ASB_AXI_BRDG_ID` (`+0x20`) reads `"brdg"`.
#[test]
fn asb_identifies_itself_as_a_bridge() {
    let mut m = machine();
    assert_eq!(m.load32(map::ASB_BASE + 0x20).unwrap(), 0x6272_6467);
}

/// The PCIe root complex is MMIO, not folded onto DRAM at `0x3D50_xxxx`.
#[test]
fn pcie_window_is_mmio_not_dram() {
    // Big enough that the DRAM alias `addr & 0x3FFF_FFFF` is backed memory.
    let mut m = Machine::new(0x3D51_0000);
    m.store32(0x3D50_9210, 0xDEAD_BEEF).unwrap();
    m.store32(map::PCIE_BASE + 0x9210, 0x3).unwrap();
    assert_eq!(m.load32(0x3D50_9210).unwrap(), 0xDEAD_BEEF);
    assert_eq!(m.load32(map::PCIE_BASE + 0x9210).unwrap(), 0x3);
}

/// The VL805 is soldered to every 4B, so the link trains as soon as the
/// bootloader releases PERST#.
#[test]
fn pcie_link_trains_and_finds_the_vl805() {
    let mut m = machine();
    // bootcode parks the block in reset; the bootloader releases bridge, PERST#.
    m.store32(map::PCIE_BASE + 0x9210, 0x3).unwrap();
    m.store32(map::PCIE_BASE + 0x9210, 0x1).unwrap();
    m.store32(map::PCIE_BASE + 0x9210, 0x0).unwrap();
    assert_eq!(m.load32(map::PCIE_BASE + 0x4068).unwrap(), 0xB0);
    m.store32(map::PCIE_BASE + 0x9000, 1 << 20).unwrap();
    assert_eq!(m.load32(map::PCIE_BASE + 0x8000).unwrap(), 0x3483_1106);
}

/// The whole path the bootloader's `xHC0 ver:` line comes out of: enumerate,
/// program the outbound window, read BAR0 by 40-bit DMA4.
#[test]
fn xhci_capability_registers_arrive_by_forty_bit_dma() {
    let mut m = machine();
    m.store32(map::PCIE_BASE + 0x9210, 0x3).unwrap();
    m.store32(map::PCIE_BASE + 0x9210, 0x0).unwrap();
    m.store32(map::PCIE_BASE + 0x9000, 1 << 20).unwrap();
    m.store32(map::PCIE_BASE + 0x8010, 0x8200_0000).unwrap();
    m.store32(map::PCIE_BASE + 0x8014, 0).unwrap();
    m.store32(map::PCIE_BASE + 0x8004, 0x0146).unwrap();
    m.store32(map::PCIE_BASE + 0x400C, 0x8000_0000).unwrap();
    m.store32(map::PCIE_BASE + 0x4010, 0).unwrap();
    m.store32(map::PCIE_BASE + 0x4070, 0x3FF0_0000).unwrap();
    m.store32(map::PCIE_BASE + 0x4080, 6).unwrap();
    m.store32(map::PCIE_BASE + 0x4084, 6).unwrap();

    let cb = 0x2_0000u32;
    let dst = 0x3_0000u32;
    for (off, w) in [
        (0x00, 0),           // TI
        (0x04, 0x0200_0004), // SRC low
        (0x08, 0x0000_1006), // SRC info: INC | address bits [39:32] = 6
        (0x0C, dst),         // DEST low
        (0x10, 0x0000_1000), // DEST info: INC, high bits 0
        (0x14, 4),           // LEN
        (0x18, 0),           // NEXT
    ] {
        m.store32(cb + off, w).unwrap();
    }
    m.store32(map::DMA4_BASE + 0x04, cb >> 5).unwrap();
    m.store32(map::DMA4_BASE, 1).unwrap(); // ACTIVE

    assert_eq!(
        m.load32(dst).unwrap(),
        0x0500_0420,
        "HCSPARAMS1 as measured on a Raspberry Pi 4B d03115: MaxSlots 32, MaxIntrs 4, MaxPorts 5"
    );
    let cs = m.load32(map::DMA4_BASE).unwrap();
    assert_eq!(cs & 0x403, 0x2);
}

/// The PVT magic at `0x7D5D_8010 + ch*0x40`, which `FUN_0ec300fa` demands before
/// it reads anything. All eighteen channels carry it on a real Raspberry Pi 4B.
#[test]
fn pvt_channels_all_carry_the_magic() {
    let mut m = machine();
    for ch in 0..18 {
        let base = map::PVT_BASE + ch * 0x40;
        assert_eq!(
            m.load32(base + 0x10).unwrap(),
            0x7FFF_50CF,
            "channel {ch} magic"
        );
        m.store32(base + 0x10, 0).unwrap();
        assert_eq!(m.load32(base + 0x10).unwrap(), 0x7FFF_50CF);
        assert_eq!(m.load32(base).unwrap(), ch, "channel {ch} index");
    }
}

/// `FUN_0ec300fa` zeroes either half of `+0x1C` that reads below 10, which skips
/// the adaptive correction; the measured values clear that floor.
#[test]
fn pvt_readings_clear_the_firmwares_floor() {
    let mut m = machine();
    for ch in 0..18 {
        let v = m.load32(map::PVT_BASE + ch * 0x40 + 0x1C).unwrap();
        assert!(v >> 16 >= 10, "channel {ch} high half {:#x}", v >> 16);
        assert!(v & 0xFFFF >= 10, "channel {ch} low half {:#x}", v & 0xFFFF);
    }
}

/// `+0x14` / `+0x18` are writable, but read before anything writes them, so they
/// are seeded from hardware.
#[test]
fn pvt_thresholds_are_seeded_then_writable() {
    let mut m = machine();
    assert_eq!(m.load32(map::PVT_BASE + 0x14).unwrap(), 0x0364_0340);
    assert_eq!(m.load32(map::PVT_BASE + 0x18).unwrap(), 0x0648_0624);

    m.store32(map::PVT_BASE + 0x14, 0x1234_5678).unwrap();
    assert_eq!(m.load32(map::PVT_BASE + 0x14).unwrap(), 0x1234_5678);
    assert_eq!(m.load32(map::PVT_BASE + 0x40 + 0x14).unwrap(), 0x0364_0340);
}

/// The six AVS channels each report their own count, or a rail read would be
/// indistinguishable from a temperature read.
#[test]
fn avs_channels_report_distinct_counts() {
    let mut m = machine();
    let counts: Vec<u32> = (0..6)
        .map(|ch| m.load32(map::AVS_BASE + 0x200 + ch * 4).unwrap() & 0x3FF)
        .collect();
    // Channel 3 follows the PMIC (692 is the seeded 0.85 V); the rest measured.
    assert_eq!(counts, vec![752, 2, 669, 692, 2, 841]);

    // `FUN_0ed603e2` accepts a sample only with both bit 10 and bit 16 set.
    for ch in 0..6 {
        let v = m.load32(map::AVS_BASE + 0x200 + ch * 4).unwrap();
        assert!(v & (1 << 10) != 0 && v & (1 << 16) != 0, "channel {ch}");
    }
}

/// The AVS block is 36 channels wide at `+0x220`, not 24. Channels 0x20..0x23
/// settle with a count of 0 on hardware, which is not the same as never.
#[test]
fn avs_rail_monitors_cover_every_channel_the_sensor_api_accepts() {
    let mut m = machine();
    for ch in 0..0x24u32 {
        let v = m.load32(map::AVS_BASE + 0x220 + ch * 4).unwrap();
        assert_ne!(v & (1 << 16), 0, "channel {ch:#x} must report settled");
    }
    assert_eq!(m.load32(map::AVS_BASE + 0x220).unwrap() & 0x7FFF, 0x07FD);
    assert_eq!(
        m.load32(map::AVS_BASE + 0x220 + 0x23 * 4).unwrap() & 0x7FFF,
        0
    );
}

/// Channel 3 has to follow the core rail: the DVFS calibration gives up on the
/// whole scan unless the sensor reports at least 10 mV between two voltages.
#[test]
fn avs_core_channel_follows_the_pmic_setpoint() {
    let mut m = machine();
    let count = |m: &mut Machine| m.load32(map::AVS_BASE + 0x200 + 3 * 4).unwrap() & 0x3FF;
    // start4 decodes the count as `(count * 100571) >> 13` tenths of a mV.
    let tenths = |c: u32| (c * 100_571) >> 13;

    pmic_write(&mut m, 0x1E, 0x25, 0x54); // 0.84 V
    let low = count(&mut m);
    pmic_write(&mut m, 0x1E, 0x25, 0x6E); // 1.10 V
    let high = count(&mut m);

    // One count is ~1.2 mV, so the round trip is exact to within a count.
    assert!(
        tenths(low).abs_diff(8400) <= 12,
        "0.84 V decoded back as {}",
        tenths(low)
    );
    assert!(
        tenths(high).abs_diff(11000) <= 12,
        "1.10 V decoded back as {}",
        tenths(high)
    );
    assert!(tenths(high) - tenths(low) >= 100, "rail must move >= 10 mV");
}

/// `+0x03C` is an active-high disable mask; a masked channel must not report a
/// valid, settled sample.
#[test]
fn avs_disable_mask_gates_the_other_channels() {
    let mut m = machine();
    m.store32(map::AVS_BASE + 0x03C, !(1u32 << 3) & 0x7F)
        .unwrap();
    assert_eq!(
        m.load32(map::AVS_BASE + 0x200 + 3 * 4).unwrap() & 0x3FF,
        692
    );
    for ch in [0, 1, 2, 4, 5] {
        assert_eq!(
            m.load32(map::AVS_BASE + 0x200 + ch * 4).unwrap(),
            0,
            "channel {ch} should be masked off"
        );
    }

    // Real hardware idles at 0: all six channels were read live that way.
    m.store32(map::AVS_BASE + 0x03C, 0).unwrap();
    for ch in 0..6 {
        assert!(m.load32(map::AVS_BASE + 0x200 + ch * 4).unwrap() & (1 << 10) != 0);
    }
}

// --- HDMI DDC I²C masters (`0x7EF0_4500` / `0x7EF0_9500`) ------------------

const DDC_CHIP_ADDRESS: u32 = 0x00;
const DDC_DATA_IN: u32 = 0x04;
const DDC_CNT: u32 = 0x24;
const DDC_CTL: u32 = 0x28;
const DDC_IIC_ENABLE: u32 = 0x2C;
const DDC_DATA_OUT: u32 = 0x30;
const DDC_CTLHI: u32 = 0x50;

const DDC_EN_ENABLE: u32 = 1 << 0;
const DDC_EN_INTRP: u32 = 1 << 1;
const DDC_EN_NOACK: u32 = 1 << 2;
const DDC_EN_NOSTOP: u32 = 1 << 4;
const DDC_EN_NOSTART: u32 = 1 << 5;
const DDC_EN_RESTART: u32 = 1 << 6;

/// Start a transfer the way start4's DDC driver does.
fn ddc_start(m: &mut Machine, base: u32, addr: u32, read: bool, count: u32, flags: u32) {
    m.store32(base + DDC_CHIP_ADDRESS, addr << 1 | u32::from(read))
        .unwrap();
    let ctlhi = m.load32(base + DDC_CTLHI).unwrap();
    m.store32(base + DDC_CTLHI, ctlhi & !2).unwrap();
    m.store32(base + DDC_CNT, count & 0x3F).unwrap();
    let ctl = m.load32(base + DDC_CTL).unwrap();
    m.store32(base + DDC_CTL, (ctl & !3) | u32::from(read))
        .unwrap();
    m.store32(base + DDC_IIC_ENABLE, flags | DDC_EN_ENABLE | DDC_EN_INTRP)
        .unwrap();
}

/// The driver's completion wait: the final `IIC_ENABLE`, or `None` on timeout.
fn ddc_wait(m: &mut Machine, base: u32) -> Option<u32> {
    for _ in 0..20 {
        let v = m.load32(base + DDC_IIC_ENABLE).unwrap();
        if v & DDC_EN_INTRP != 0 {
            return Some(v);
        }
        m.tick(5000 * 54); // the driver's 5 ms between polls
    }
    None
}

fn ddc_stop(m: &mut Machine, base: u32) {
    m.store32(base + DDC_CNT, 0).unwrap();
    m.store32(base + DDC_IIC_ENABLE, 0).unwrap();
}

/// Nothing is plugged into either HDMI connector (a Raspberry Pi 4B d03115 reports
/// both `disconnected`), so the EDID address NACKs and the transfer completes.
#[test]
fn hdmi_ddc_nacks_when_no_monitor_answers() {
    for base in [map::HDMI_DDC0_BASE, map::HDMI_DDC1_BASE] {
        let mut m = machine();

        m.store32(base + DDC_DATA_IN, 0).unwrap();
        ddc_start(&mut m, base, 0x50, false, 1, DDC_EN_NOSTOP | DDC_EN_RESTART);
        let v = ddc_wait(&mut m, base).expect("the transfer must complete");
        assert_ne!(v & DDC_EN_NOACK, 0, "an empty bus cannot acknowledge");
        ddc_stop(&mut m, base);

        ddc_start(&mut m, base, 0x50, true, 32, DDC_EN_NOSTOP);
        let v = ddc_wait(&mut m, base).expect("the transfer must complete");
        assert_ne!(v & DDC_EN_NOACK, 0, "an empty bus cannot acknowledge");
        for i in 0..8 {
            assert_eq!(
                m.load32(base + DDC_DATA_OUT + 4 * i).unwrap(),
                0,
                "a NAKed read returns nothing"
            );
        }
        ddc_stop(&mut m, base);
    }
}

/// `INTRP` means "the bytes have been clocked out", so it cannot be set inside
/// the write that starts the transfer: 32 bytes at 97.5 kHz take ~3 ms.
#[test]
fn hdmi_ddc_completion_waits_for_the_wire() {
    let base = map::HDMI_DDC0_BASE;
    let mut m = machine();

    ddc_start(&mut m, base, 0x50, true, 32, DDC_EN_NOSTOP);
    assert_eq!(
        m.load32(base + DDC_IIC_ENABLE).unwrap() & DDC_EN_INTRP,
        0,
        "INTRP must not land inside the write that started the transfer"
    );
    m.tick(1000 * 54); // 1 ms — a third of the way through
    assert_eq!(
        m.load32(base + DDC_IIC_ENABLE).unwrap() & DDC_EN_INTRP,
        0,
        "INTRP cannot precede the bits going out"
    );
    m.tick(3000 * 54);
    assert_ne!(
        m.load32(base + DDC_IIC_ENABLE).unwrap() & DDC_EN_INTRP,
        0,
        "INTRP must latch once the transfer is over"
    );

    ddc_stop(&mut m, base);
    assert_eq!(m.load32(base + DDC_IIC_ENABLE).unwrap(), 0);
}

/// With a monitor on the bus the same sequence reads its EDID back, four bytes
/// per `DATA_OUT` register, little-endian. Nothing on the boot path attaches one.
#[test]
fn hdmi_ddc_reads_an_attached_edid() {
    use pimu::periph::HdmiDdc;

    let edid: Vec<u8> = (0..128u32).map(|i| (i * 7 + 1) as u8).collect();
    let base = map::HDMI_DDC0_BASE;
    let mut m = machine();
    m.hdmi_ddc0 = HdmiDdc::new("hdmi-ddc0").with_edid(edid.clone());

    m.store32(base + DDC_DATA_IN, 0).unwrap();
    ddc_start(&mut m, base, 0x50, false, 1, DDC_EN_NOSTOP | DDC_EN_RESTART);
    let v = ddc_wait(&mut m, base).expect("the transfer must complete");
    assert_eq!(v & DDC_EN_NOACK, 0, "a monitor acknowledges its address");
    ddc_stop(&mut m, base);

    let mut got = Vec::new();
    for chunk in 0..4 {
        let flags = if chunk == 0 {
            DDC_EN_NOSTOP
        } else {
            DDC_EN_NOSTOP | DDC_EN_NOSTART
        };
        ddc_start(&mut m, base, 0x50, true, 32, flags);
        let v = ddc_wait(&mut m, base).expect("the transfer must complete");
        assert_eq!(v & DDC_EN_NOACK, 0, "a monitor acknowledges its address");
        for i in 0..8 {
            got.extend_from_slice(&m.load32(base + DDC_DATA_OUT + 4 * i).unwrap().to_le_bytes());
        }
        ddc_stop(&mut m, base);
    }
    assert_eq!(got, edid, "the whole block, in order");

    let base1 = map::HDMI_DDC1_BASE;
    ddc_start(&mut m, base1, 0x50, true, 32, DDC_EN_NOSTOP);
    let v = ddc_wait(&mut m, base1).expect("the transfer must complete");
    assert_ne!(v & DDC_EN_NOACK, 0, "HDMI1 has no monitor");
}

// --- GPIO: a master reaches only what its pads are muxed to ----------------

const GPFSEL0: u32 = map::GPIO_BASE;
const GPFSEL4: u32 = map::GPIO_BASE + 0x10;

/// GPIO 0 and 1 on ALT0 — I²C 0 out to the 40-pin header.
const HEADER_I2C: u32 = 0b100 | 0b100 << 3;
/// GPIO 40..43 on ALT4 — SPI0 on the boot flash.
const FLASH_SPI: u32 = 0b011 | 0b011 << 3 | 0b011 << 6 | 0b011 << 9;

/// Read `len` bytes from `addr`, and say whether the address was acknowledged.
fn i2c_read(m: &mut Machine, base: u32, addr: u8, len: u32) -> Option<Vec<u8>> {
    m.store32(base + BSC_A, addr as u32).unwrap();
    m.store32(base + BSC_DLEN, len).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST | C_READ).unwrap();
    settle(m);
    let s = m.load32(base + BSC_S).unwrap();
    let mut got = Vec::new();
    for _ in 0..len {
        got.push(m.load32(base + BSC_FIFO).unwrap() as u8);
    }
    m.store32(base + BSC_S, S_DONE | S_ERR).unwrap();
    (s & S_ERR == 0).then_some(got)
}

/// A HAT's ID EEPROM is on the header pins, so it answers only while I²C 0 is
/// muxed there — the same master on GPIO 44/45 must find nothing.
#[test]
fn the_hat_eeprom_answers_only_on_the_header_pins() {
    let mut m = machine();
    m.bsc0.attach_eeprom(pimu::periph::hat::HatEeprom::new(
        b"R-Pi\x01\x00\x02\x00".to_vec(),
    ));

    assert_eq!(i2c_read(&mut m, map::BSC0_BASE, 0x50, 4), None);

    m.store32(GPFSEL0, HEADER_I2C).unwrap();
    assert_eq!(
        i2c_read(&mut m, map::BSC0_BASE, 0x50, 4).as_deref(),
        Some(&b"R-Pi"[..]),
        "the header bus reaches the HAT"
    );

    m.store32(GPFSEL0, 0).unwrap();
    assert_eq!(i2c_read(&mut m, map::BSC0_BASE, 0x50, 4), None);
}

/// The PMIC bus is not on the header: its instance is unaffected by the pins.
#[test]
fn the_pmic_bus_does_not_care_what_the_pins_do() {
    let mut m = machine();
    assert_eq!(pmic_read(&mut m, 0x43, 0x01) >> 5, 0b101);
    m.store32(GPFSEL0, HEADER_I2C).unwrap();
    assert_eq!(pmic_read(&mut m, 0x43, 0x01) >> 5, 0b101);
}

/// GPIO 40..43 are the boot flash only on ALT4, so a flash session reads the
/// image only while the four pins are moved there (`specs/spi0.toml`).
#[test]
fn spi0_reads_the_flash_only_while_its_pins_are_on_alt4() {
    const CS_TA: u32 = 1 << 7;
    let mut m = machine();
    m.spi0.attach_flash(vec![0xAA; 0x1000]);

    let read_byte = |m: &mut Machine| {
        let base = map::SPI0_BASE;
        m.store32(base, CS_TA).unwrap();
        for byte in [0x03, 0x00, 0x00, 0x00, 0xFF] {
            m.store32(base + 0x04, byte).unwrap();
        }
        for _ in 0..4 {
            m.load32(base + 0x04).unwrap(); // command + address echoes
        }
        let v = m.load32(base + 0x04).unwrap() as u8;
        m.store32(base, 0).unwrap();
        v
    };

    // Pins still inputs: the bytes go nowhere and MISO idles high.
    assert_eq!(read_byte(&mut m), 0xFF);

    m.store32(GPFSEL4, FLASH_SPI).unwrap();
    assert_eq!(read_byte(&mut m), 0xAA, "ALT4 puts the pads on the flash");

    m.store32(GPFSEL4, FLASH_SPI & !(0b111 << 6) | 0b001 << 6)
        .unwrap();
    assert_eq!(read_byte(&mut m), 0xFF);
}

/// The interrupt lines follow `GPEDS`, and a detector watches the pad whatever
/// drives it.
#[test]
fn a_gpio_edge_raises_the_banks_line() {
    const GPSET1: u32 = map::GPIO_BASE + 0x20;
    const GPCLR1: u32 = map::GPIO_BASE + 0x2C;
    const GPEDS1: u32 = map::GPIO_BASE + 0x44;
    const GPREN1: u32 = map::GPIO_BASE + 0x50;
    const LED: u32 = 1 << 10; // GPIO 42

    let mut m = machine();
    m.store32(GPFSEL4, 0b001 << 6).unwrap(); // GPIO 42 an output
    assert_eq!(m.gpio.irq_lines(), [false, false]);

    m.store32(GPSET1, LED).unwrap();
    assert_eq!(m.load32(GPEDS1).unwrap(), 0);
    m.store32(GPCLR1, LED).unwrap();

    m.store32(GPREN1, LED).unwrap();
    m.store32(GPSET1, LED).unwrap();
    assert_eq!(m.load32(GPEDS1).unwrap(), LED, "the rising edge latched");
    assert_eq!(m.gpio.irq_lines(), [false, true]);

    m.store32(GPEDS1, LED).unwrap();
    assert_eq!(m.load32(GPEDS1).unwrap(), 0, "write 1 clears it");
    assert_eq!(m.gpio.irq_lines(), [false, false]);
}
