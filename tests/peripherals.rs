//! Peripheral-model tests.
//!
//! The recurring failure mode in this project is not a peripheral that behaves
//! subtly wrong — it is a register that reads back 0. Firmware reads 0 from an
//! identification or status register as "this chip is absent" and silently
//! skips a whole subsystem, and the symptom surfaces hundreds of millions of
//! instructions later as a `bl <null>`. These tests pin the register values
//! that the boot is known to gate on.

use rpi_virt_fw::bus::{Bus, MmioDevice, Width};
use rpi_virt_fw::periph::Spi0;
use rpi_virt_fw::soc::bcm2711 as map;
use rpi_virt_fw::Machine;

fn machine() -> Machine {
    Machine::new(1024 * 1024)
}

/// `SCALER_DISPID` at `0x7E40_0008`. start4's display bring-up compares it
/// against `0x64647276` and bails out immediately otherwise, which skips
/// `hdmi_init` and leaves the HDMI provider array null (commit `b21bc55`, #13).
/// The value is read off a running Pi 4's `hvs_regs`, not guessed.
#[test]
fn scaler_dispid_identifies_the_hvs() {
    let mut m = machine();
    assert_eq!(m.load32(map::HVS_BASE + 0x08).unwrap(), 0x6464_7276);

    // Read-only: the firmware writes all over this block.
    m.store32(map::HVS_BASE + 0x08, 0).unwrap();
    assert_eq!(
        m.load32(map::HVS_BASE + 0x08).unwrap(),
        0x6464_7276,
        "SCALER_DISPID must not be clobberable"
    );
}

/// The HVS frame-swap wait polls `current` (`+0x30 + 4*chan`) until it matches
/// `requested` (`+0x20 + 4*chan`). With no display behind it, `current` has to
/// mirror `requested` or every swap burns its full 100 x 1 ms timeout.
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

const CS: u32 = 0x00;
const FIFO: u32 = 0x04;
const CS_TA: u32 = 1 << 7;
const CS_CLEAR_RX: u32 = 1 << 5;
const CS_CLEAR_TX: u32 = 1 << 4;
const CS_DONE: u32 = 1 << 16;
const CS_RXD: u32 = 1 << 17;

/// SPI0 `CS.DONE` reflects the TX side only. Gating it on an empty RX FIFO
/// broke start4's EEPROM section scanner, which clocks a block and then checks
/// `DONE` while the received bytes are still queued — reading `DONE = 0` there
/// is a transfer error to the firmware, and the whole scan failed.
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

    // And the read command actually returns flash contents.
    for _ in 0..4 {
        spi.read(FIFO, Width::Word).unwrap(); // command + address echoes
    }
    assert_eq!(spi.read(FIFO, Width::Word).unwrap(), 0xAA);
}

/// Core 1's copies of the core-control registers live at `+0x800`. The window
/// was mapped 0x100 bytes wide, so every one of them fell through to the
/// catch-all peripheral stub and core 1 looked like it enabled no interrupts at
/// all (commit `7bd21a3`).
#[test]
fn core1_registers_are_inside_the_corectl_window() {
    // The window must cover core 1's block at +0x800.
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

    // Core 0's vector base is the one register whose value the model keeps.
    m.store32(map::CORECTL_BASE + 0x30, 0x0002_8000).unwrap();
    assert_eq!(m.corectl.vbase[0], 0x0002_8000);
}

/// Core 1's vector base is one 0x800 stride above core 0's, like every other
/// register in this block. A boot trace of the core-control writes shows the
/// pair `+0x30` and `+0x830` taking the same base (`0xFEC01E00`) and nothing
/// ever writing `+0x38`; the old `+0x38` guess left `vbase[1]` at 0, so core 1
/// could never be vectored (commit `06a8447`).
#[test]
fn core1_vector_base_is_recorded_from_the_strided_offset() {
    let mut m = machine();

    m.store32(map::CORECTL_BASE + 0x830, 0xFEC0_1E00).unwrap();
    assert_eq!(m.corectl.vbase[1], 0xFEC0_1E00);

    // `+0x38` is not the vector base and must not be mistaken for it.
    let mut m = machine();
    m.store32(map::CORECTL_BASE + 0x38, 0xFEC0_1E00).unwrap();
    assert_eq!(m.corectl.vbase[1], 0);
}

/// start4 raises an interrupt on a core by setting its bit in that core's
/// pending word (`+0x40` for sources 64..95, `+0x840` for core 1). Dropping
/// those writes silently starves every software-posted interrupt — the clock
/// service re-posts its own source 66 that way on every run.
#[test]
fn software_posted_interrupts_are_queued_per_core() {
    let mut m = machine();

    m.store32(map::CORECTL_BASE + 0x40, 1 << 2).unwrap(); // source 64 + 2
    m.store32(map::CORECTL_BASE + 0x840, 1 << 15).unwrap(); // core 1, source 79

    assert_eq!(m.corectl.take_sw_raised(), Some((0, 66)));
    assert_eq!(m.corectl.take_sw_raised(), Some((1, 79)));
    assert_eq!(m.corectl.take_sw_raised(), None);

    // Clearing a bit is the ISR's acknowledge, not a new interrupt.
    m.store32(map::CORECTL_BASE + 0x40, 0).unwrap();
    assert_eq!(m.corectl.take_sw_raised(), None);
}

/// `enable_irq_source(src, prio)` packs a 4-bit field per source into the words
/// at `+0x10`: `word = (src >> 3) & 3`, `field = (src & 7) * 4`. The tick only
/// gets delivered if that decode round-trips.
#[test]
fn interrupt_priority_fields_round_trip() {
    let mut m = machine();

    // enable_irq_source(64, 1) and enable_irq_source(66, 3).
    m.store32(map::CORECTL_BASE + 0x10, 1 | (3 << 8)).unwrap();

    assert_eq!(m.corectl.irq_priority(64), 1);
    assert_eq!(m.corectl.irq_priority(66), 3);
    assert_eq!(m.corectl.irq_priority(65), 0, "unenabled source");
    assert_eq!(m.corectl.irq_priority(72), 0, "next word along");
}

// ---------------------------------------------------------------------------
// Board PMICs on the BSC at 0x7E20_5E00 (#4).
// ---------------------------------------------------------------------------

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

/// Let a transfer settle: `S.DONE` is deliberately held off for a few ticks so
/// the driver's submit call unwinds before the completion is observed.
fn settle(m: &mut Machine) {
    for _ in 0..256 {
        m.bsc_pmic.tick(1);
    }
}

/// `read(reg)` the way start4's BSC transport does it when `cfg[8] & 2` is set
/// (the `0x1B` path): one-byte write phase, FIFO fed straight after `ST`, then
/// a separate read phase.
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

/// The same read, in the order the transport uses when `cfg[8] & 2` is clear
/// (the `0x1E` path): it programs the read phase *before* pushing the register
/// byte, relying on the write phase stalling with `S.TA` asserted until the
/// FIFO has data. Both orders have to select the same register.
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

/// The register the firmware asks for is the register it gets. start4's
/// transport (`0x3ECF0ED0`) writes `C.ST` before it feeds the FIFO, so a model
/// that runs the transfer at `ST` and takes the FIFO byte afterwards selects
/// nothing, and the auto-incrementing pointer walks the whole 0..0xFF space
/// instead of answering the register that was asked for.
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

/// Seeded setpoints must decode, through the firmware's own conversion, to the
/// voltages a real d03115 reports.
#[test]
fn pmic_setpoints_decode_to_the_real_boards_voltages() {
    let mut m = machine();

    // 0x1B rails 2..4 (`0x3EC8C710`): raw * 5_000 + 900_000 µV.
    let sdram = pmic_read(&mut m, 0x1B, 0x09) as u32 * 5_000 + 900_000;
    assert_eq!(sdram, 1_100_000, "vcgencmd measure_volts sdram_c on rpi-dev");

    // 0x1E rail 1 (`0x3EC8C9F6`): raw * 10_000 µV, within the descriptor's
    // 0.3 V..1.9 V range.
    let core = pmic_read(&mut m, 0x1E, 0x25) as u32 * 10_000;
    assert!(
        (300_000..=1_900_000).contains(&core),
        "core setpoint {core} outside the descriptor range"
    );
}

/// A setpoint write is read back, and latches the "voltage settled" bit each
/// part's post-set callback polls: `0x1B` reg 0x00 bit 4 (`0x3EC8C746`) and
/// `0x1E` reg 0x02 bit 3 (`0x3EC8C9FC`). Those loops have no timeout, so a bit
/// that never sets hangs the boot outright.
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

/// The two parts are separate register files even though they share a bus: a
/// write to one must not show up in the other.
#[test]
fn pmic_addresses_are_separate_register_files() {
    let mut m = machine();
    pmic_write(&mut m, 0x1E, 0x40, 0x5A);
    assert_eq!(pmic_read(&mut m, 0x1E, 0x40), 0x5A);
    assert_eq!(pmic_read(&mut m, 0x1B, 0x40), 0x00);
}

/// Nothing else is on this bus. start4 probes a handful of other addresses on
/// it; an unACKed transfer has to complete `DONE | ERR`, not spin.
#[test]
fn pmic_bus_nacks_every_other_address() {
    let mut m = machine();
    let base = map::BSC_PMIC_BASE;
    m.store32(base + BSC_A, 0x43).unwrap();
    m.store32(base + BSC_DLEN, 1).unwrap();
    m.store32(base + BSC_C, C_I2CEN | C_ST | C_READ).unwrap();
    settle(&mut m);
    let s = m.load32(base + BSC_S).unwrap();
    assert_ne!(s & S_DONE, 0, "transfer must complete");
    assert_ne!(s & S_ERR, 0, "unACKed address must raise ERR");
}
