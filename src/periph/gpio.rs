//! The GPIO block (`0x7E20_0000`): 58 pins, their functions, levels and pulls.
//!
//! ## What the firmware does with it
//!
//! A `firmware-boot` run touches `GPFSEL0`..`GPFSEL4`, `GPSET1`, `GPCLR1`, the
//! two undocumented words at `+0xD0` / `+0xD4` and all four `PUP_PDN`
//! registers — and nothing else in the window. There is no `GPLEV` read, no
//! edge detect, and no write to the BCM2835 pull registers: start4 has both
//! pull paths and a Pi 4 takes the BCM2711 one (`FUN_0ecc9762` ->
//! `FUN_0ecc83e4(&DAT_7e2000e4, pin, pull)`).
//!
//! `GPFSEL4` is written hundreds of times because GPIO 40..43 carry two things
//! at once: the SPI NOR flash the EEPROM stages read on ALT4
//! ([`super::spi0`]), and — on a 4B — PWM audio on 40/41 with the activity LED
//! an output on 42. Every flash session moves the four pins to ALT4 and back,
//! and drives the LED again afterwards.
//!
//! ## Reads of a write-only register
//!
//! `GPSET` and `GPCLR` answer `0x6770696f` — `"gpio"` big-endian, the block's
//! own tag — not zero and not the last value written, which is what the
//! catch-all stub used to answer. It matters because the bootloader's activity
//! LED is a read-modify-write:
//!
//! ```text
//!   0x8000A6FC  mov    r1, 0x7e200000
//!   0x8000A706  ld     r3, [r1+32]      ; GPSET1
//!   0x8000A708  bitset r3, #10          ; GPIO 42
//!   0x8000A70A  st     r3, [r1+32]
//! ```
//!
//! so a real board is told to set every pin in the tag as well. That is
//! harmless there — `GPSET` only drives pins whose function is `output` — and
//! it is harmless here for the same reason: the latch keeps the bits, and
//! [`Gpio::level`] ignores them for a pin that is not an output.
//!
//! ## Pin levels, and what watches them
//!
//! Nothing outside the model drives a pin. A `GPLEV` bit is therefore the
//! output latch for a pin whose function is `output`, and the pin's
//! termination otherwise: pull-up reads 1, pull-down and no pulling read 0.
//!
//! Pins do still move — the firmware drives the activity LED, and a write to
//! `PUP_PDN` moves what holds an input — so the six detect enables work:
//! an edge or a level latches `GPEDS`, and a bank with a latched bit raises
//! its interrupt lines ([`Gpio::irq_lines`]): `GIC_SPI` 113 for pins 0..31,
//! 114 for 32..57, 115 the mirror of the second bank's line the block's
//! third-bank output is, and 116 the 'any bank' line either raises. No
//! firmware in a boot enables a detector, so this has yet to fire in a run.
//!
//! ## What the pins carry
//!
//! Two masters reach what they reach only while their pins are muxed to them,
//! which [`crate::machine::Machine`] follows: SPI0 the boot flash while GPIO
//! 40..43 are on ALT4 ([`super::spi0`]), and I²C 0 a HAT's ID EEPROM while
//! GPIO 0/1 are on ALT0 ([`super::hat`]) — the same master runs on GPIO 44/45
//! for the camera and display probes, where no HAT is.
//!
//! ## Ground truth
//!
//! The whole window read through `/dev/gpiomem` on a Raspberry Pi 4B d03115
//! running Linux:
//!
//! ```text
//!   000 00000000   01c 6770696f   034 1000c1ff   0d0 00000001   0e4 4aa95555
//!   004 00000000   020 6770696f   038 000038fb   0d4 00000000   0e8 19aaaaaa
//!   008 12000000   024 6770696f   040 00000000   0a4 ffffffff   0ec 55505544
//!   00c 3fffffff   028 6770696f   044 00000000   0a8 03ffffff   0f0 000aaaaa
//!   010 00000064   02c 6770696f
//! ```
//!
//! `GPFSEL2` `0x12000000` is GPIO 28/29 on ALT5 (the RGMII MDIO bus), `GPFSEL3`
//! `0x3fffffff` is 30..39 on ALT3 (Bluetooth and the WiFi SDIO), and `GPFSEL4`
//! `0x64` is 40/41 on ALT0 with 42 an output — the same three words the model
//! ends a boot with.
//!
//! ## Which board
//!
//! The block is the chip's, but what a pin is wired to is the board's. The
//! names come from the 4B's own device tree (`gpio-line-names` of
//! `gpio@7e200000`), and the boards that differ say so in the firmware's
//! `dt-blob` (`pins_4b`, `pins_cm4`, `pins_400`): a Compute Module 4 puts its
//! SD interface on 48..53 and the SMPS I²C on 46/47, where a 4B has RGMII, and
//! a Pi 400 lights GPIO 42 as a power LED where a 4B blinks it for disk
//! activity. [`Gpio::fit_board`] picks the map, and the `gpio` log channel
//! names the pin it reports.

use crate::bus::{BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};
use crate::soc::board::{TYPE_CM4, TYPE_PI400};
use crate::soc::Board;

use crate::spec::gpio::{
    GPAFEN, GPAREN, GPCLR, GPCLR_RESET, GPEDS, GPFEN, GPFSEL, GPFSEL_COUNT, GPHEN, GPLEN, GPLEV,
    GPPUD, GPPUDCLK, GPREN, GPSET, GPSET_RESET, PAD_CFG, PIN_MUX, PIN_MUX_SD_LEGACY_MASK, PUP_PDN,
    PUP_PDN_COUNT,
};
use crate::spec::Coverage;

/// Every register in `specs/gpio.toml` is modelled.
pub const COVERAGE: Coverage = Coverage {
    block: "gpio",
    decoded: &[
        GPFSEL, GPSET, GPCLR, GPLEV, GPEDS, GPREN, GPFEN, GPHEN, GPLEN, GPAREN, GPAFEN, GPPUD,
        GPPUDCLK, PIN_MUX, PAD_CFG, PUP_PDN,
    ],
};

/// Pins the BCM2711 brings out: 0..57, in two banks of 32.
pub const PINS: usize = 58;

/// Registers with one bit a pin come in two banks.
const BANKS: usize = 2;

/// The six edge / level detect enables, in the order they sit in the window,
/// and their places in it.
const DETECTS: [u32; 6] = [GPREN, GPFEN, GPHEN, GPLEN, GPAREN, GPAFEN];
const REN: usize = 0;
const FEN: usize = 1;
const HEN: usize = 2;
const LEN: usize = 3;
const AREN: usize = 4;
const AFEN: usize = 5;

/// What a pin is doing, as its three `GPFSEL` bits say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    Input,
    Output,
    /// `ALT0`..`ALT5`.
    Alt(u8),
}

impl Function {
    /// The function the `GPFSEL` encoding names: 0 input, 1 output, then
    /// ALT5, ALT4, ALT0, ALT1, ALT2, ALT3.
    fn from_bits(bits: u32) -> Function {
        match bits & 7 {
            0 => Function::Input,
            1 => Function::Output,
            2 => Function::Alt(5),
            3 => Function::Alt(4),
            n => Function::Alt(n as u8 - 4),
        }
    }
}

impl std::fmt::Display for Function {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Function::Input => f.write_str("input"),
            Function::Output => f.write_str("output"),
            Function::Alt(n) => write!(f, "ALT{n}"),
        }
    }
}

/// What holds a pin when nothing drives it, as its two `PUP_PDN` bits say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pull {
    None,
    Up,
    Down,
    /// The encoding the datasheet reserves; kept as it was written.
    Reserved,
}

impl Pull {
    fn from_bits(bits: u32) -> Pull {
        match bits & 3 {
            0 => Pull::None,
            1 => Pull::Up,
            2 => Pull::Down,
            _ => Pull::Reserved,
        }
    }

    /// What a pin nothing drives reads as.
    fn level(self) -> bool {
        self == Pull::Up
    }
}

impl std::fmt::Display for Pull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Pull::None => "no pull",
            Pull::Up => "pull-up",
            Pull::Down => "pull-down",
            Pull::Reserved => "pull reserved",
        })
    }
}

/// What each pin is wired to on a Pi 4B, as the board's device tree names them
/// (`gpio-line-names` of `gpio@7e200000` in `bcm2711-rpi-4-b.dtb`). 2..27 are
/// the header pins a user owns, so they carry no name beyond their number.
const PI4B_LINES: [&str; PINS] = [
    "ID_SDA",
    "ID_SCL",
    "GPIO2",
    "GPIO3",
    "GPIO4",
    "GPIO5",
    "GPIO6",
    "GPIO7",
    "GPIO8",
    "GPIO9",
    "GPIO10",
    "GPIO11",
    "GPIO12",
    "GPIO13",
    "GPIO14",
    "GPIO15",
    "GPIO16",
    "GPIO17",
    "GPIO18",
    "GPIO19",
    "GPIO20",
    "GPIO21",
    "GPIO22",
    "GPIO23",
    "GPIO24",
    "GPIO25",
    "GPIO26",
    "GPIO27",
    "RGMII_MDIO",
    "RGMII_MDC",
    "CTS0",
    "RTS0",
    "TXD0",
    "RXD0",
    "SD1_CLK",
    "SD1_CMD",
    "SD1_DATA0",
    "SD1_DATA1",
    "SD1_DATA2",
    "SD1_DATA3",
    "PWM0_MISO",
    "PWM1_MOSI",
    "STATUS_LED_G_CLK",
    "SPIFLASH_CE_N",
    "SDA0",
    "SCL0",
    "RGMII_RXCLK",
    "RGMII_RXCTL",
    "RGMII_RXD0",
    "RGMII_RXD1",
    "RGMII_RXD2",
    "RGMII_RXD3",
    "RGMII_TXCLK",
    "RGMII_TXCTL",
    "RGMII_TXD0",
    "RGMII_TXD1",
    "RGMII_TXD2",
    "RGMII_TXD3",
];

/// What a Compute Module 4 does differently: its own SD interface where a 4B
/// has RGMII, and the SMPS I²C on the two pins below it (`pins_cm4` of the
/// firmware's `dt-blob`, which gives 48..53 `function = "sdcard"` and calls
/// 46/47 `SMPS_SCL` / `SMPS_SDA`). GPIO 42 is nobody's there: the module has no
/// activity LED, and `LEDS_DISK_ACTIVITY` is `absent`.
const CM4_LINES: &[(usize, &str)] = &[
    (42, "GPIO42"),
    (46, "SMPS_SCL"),
    (47, "SMPS_SDA"),
    (48, "SD0_CLK"),
    (49, "SD0_CMD"),
    (50, "SD0_DATA0"),
    (51, "SD0_DATA1"),
    (52, "SD0_DATA2"),
    (53, "SD0_DATA3"),
];

/// What a Pi 400 does differently: GPIO 42 drives the power LED, and the
/// firmware starts it *on* (`pin@p42 { function = "output"; polarity =
/// "active_high"; startup_state = "active"; }` in `pins_400`, against
/// `startup_state = "inactive"` for the 4B's activity LED). It is the same pin
/// as the SPI flash clock either way.
const PI400_LINES: &[(usize, &str)] = &[(42, "PWR_LED_CLK")];

/// What `GPIO_PUP_PDN_CNTRL_REG0`..`REG3` hold out of reset: pins 0 to 8 pulled
/// up, 9 to 27 down, 28 and 29 with no pull, 30 to 33 down, 34 to 36 up, 37 to
/// 43 down, 44 and 45 with no pull, and 46 to 57 up. A pin nothing drives reads
/// its termination, so a firmware that looks at a pin before it sets a pull
/// sees these.
const PUP_PDN_RESET: [u32; PUP_PDN_COUNT as usize] =
    [0xAAA9_5555, 0xA0AA_AAAA, 0x50AA_A95A, 0x0005_5555];

pub struct Gpio {
    /// `GPFSEL0`..`GPFSEL5`, as written.
    fsel: [u32; GPFSEL_COUNT as usize],
    /// The output latch `GPSET` sets and `GPCLR` clears, whatever the pin's
    /// function is.
    out: [u32; BANKS],
    /// `GPEDS`: never set, since nothing drives a pin from outside.
    eds: [u32; BANKS],
    /// The six detect enables, in [`DETECTS`] order.
    detect: [[u32; BANKS]; DETECTS.len()],
    /// The BCM2835 pull registers. Kept, and no pull changes with them.
    pud: u32,
    pudclk: [u32; BANKS],
    /// `GPIO_PUP_PDN_CNTRL_REG0`..`REG3`, two bits a pin.
    pup_pdn: [u32; PUP_PDN_COUNT as usize],
    /// The undocumented words at `+0xD0` and `+0xD4`.
    pin_mux: u32,
    pad_cfg: u32,
    /// What the board wires each pin to, for the log.
    lines: [&'static str; PINS],
    /// Where [`Channel::Gpio`] goes.
    pub log: Log,
}

impl Default for Gpio {
    fn default() -> Gpio {
        Gpio {
            fsel: [0; GPFSEL_COUNT as usize],
            out: [0; BANKS],
            eds: [0; BANKS],
            detect: [[0; BANKS]; DETECTS.len()],
            pud: 0,
            pudclk: [0; BANKS],
            pup_pdn: PUP_PDN_RESET,
            pin_mux: 0,
            pad_cfg: 0,
            lines: PI4B_LINES,
            log: Log::default(),
        }
    }
}

impl Gpio {
    pub fn new() -> Gpio {
        Gpio::default()
    }

    /// Wire the pins the way `board` has them.
    pub fn fit_board(&mut self, board: Board) {
        self.lines = PI4B_LINES;
        let changes: &[(usize, &'static str)] = match board.board_type() {
            TYPE_CM4 => CM4_LINES,
            TYPE_PI400 => PI400_LINES,
            _ => &[],
        };
        for &(pin, name) in changes {
            self.lines[pin] = name;
        }
    }

    /// What the board calls this pin.
    pub fn line(&self, pin: usize) -> &'static str {
        self.lines.get(pin).copied().unwrap_or("?")
    }

    /// The function the three `GPFSEL` bits of `pin` name. A pin the chip
    /// does not bring out is an input: nothing drives the bits above 57.
    pub fn function(&self, pin: usize) -> Function {
        if pin >= PINS {
            return Function::Input;
        }
        let (reg, shift) = (pin / 10, (pin % 10) * 3);
        Function::from_bits(self.fsel[reg] >> shift)
    }

    /// What holds `pin` when nothing drives it.
    pub fn pull(&self, pin: usize) -> Pull {
        if pin >= PINS {
            return Pull::None;
        }
        let (reg, shift) = (pin / 16, (pin % 16) * 2);
        Pull::from_bits(self.pup_pdn[reg] >> shift)
    }

    /// What a `GPLEV` bit reads: an output's own latch, and otherwise the
    /// pin's termination, because nothing outside the model drives a pin.
    pub fn level(&self, pin: usize) -> bool {
        if self.function(pin) == Function::Output {
            self.out[pin / 32] >> (pin % 32) & 1 != 0
        } else {
            self.pull(pin).level()
        }
    }

    /// Bit 1 of the undocumented word at `+0xD0`: the SD card slot is on the
    /// legacy EMMC controller rather than EMMC2 (#66). The machine routes the
    /// card; the block only holds the bit.
    pub fn sd_legacy(&self) -> bool {
        self.pin_mux & PIN_MUX_SD_LEGACY_MASK != 0
    }

    /// The pins of `bank` whose level is high.
    fn levels(&self, bank: usize) -> u32 {
        let mut v = 0;
        for pin in bank * 32..PINS.min((bank + 1) * 32) {
            if self.level(pin) {
                v |= 1 << (pin % 32);
            }
        }
        v
    }

    /// The level of every pin, a word a bank.
    fn levels_all(&self) -> [u32; BANKS] {
        [self.levels(0), self.levels(1)]
    }

    /// The bits of `bank` that are a pin: 32 in bank 0, 26 in bank 1.
    fn pins_of(bank: usize) -> u32 {
        let n = PINS.min((bank + 1) * 32) - bank * 32;
        u32::MAX >> (32 - n)
    }

    /// What the six detect enables make of the levels moving from `before` to
    /// where they are now: an edge latches `GPEDS` for the pin whose rising /
    /// falling enable is set, and a level detect latches it for as long as the
    /// pin sits at that level — so a `GPEDS` write clears a level detect's bit
    /// only until the next access.
    ///
    /// A detector watches the pad, so a pin the firmware drives itself is
    /// detected the same as one something outside drives. Nothing in a boot
    /// enables one, so the model has never seen it happen on hardware.
    fn detect_edges(&mut self, before: [u32; BANKS]) {
        let now = self.levels_all();
        for bank in 0..BANKS {
            let (rose, fell) = (now[bank] & !before[bank], before[bank] & !now[bank]);
            let d = &self.detect;
            let mut eds = self.eds[bank];
            eds |= rose & (d[REN][bank] | d[AREN][bank]);
            eds |= fell & (d[FEN][bank] | d[AFEN][bank]);
            eds |= now[bank] & d[HEN][bank];
            eds |= !now[bank] & d[LEN][bank];
            self.eds[bank] = eds & Gpio::pins_of(bank);
        }
    }

    /// The block's two bank interrupt lines: up while any pin of that bank has
    /// its `GPEDS` bit latched. The other two lines the block drives follow
    /// from these — see [`crate::arm`].
    pub fn irq_lines(&self) -> [bool; BANKS] {
        [self.eds[0] != 0, self.eds[1] != 0]
    }

    /// A word in the window as one of `count` elements `4` apart from `base`,
    /// if it is one.
    fn element(off: u32, base: u32, count: u32) -> Option<usize> {
        let i = off.checked_sub(base)? / 4;
        (i < count).then_some(i as usize)
    }

    /// One of a pair, one register a bank.
    fn bank(off: u32, base: u32) -> Option<usize> {
        Gpio::element(off, base, BANKS as u32)
    }

    /// A `GPFSEL` write: say what changed, pin by pin.
    fn log_fsel(&self, reg: usize, was: u32, now: u32) {
        for pin in reg * 10..PINS.min(reg * 10 + 10) {
            let shift = (pin % 10) * 3;
            let (a, b) = (was >> shift & 7, now >> shift & 7);
            if a == b {
                continue;
            }
            // A pin that becomes an output starts driving the latch it
            // already had, which is how the LED comes on: the bootloader
            // writes `GPSET` while the pin is still on ALT4 for the flash,
            // and only then gives it back to the LED.
            let func = Function::from_bits(b);
            let level = match (func, self.level(pin)) {
                (Function::Output, true) => ", high",
                (Function::Output, false) => ", low",
                _ => "",
            };
            crate::log!(
                self.log,
                Channel::Gpio,
                "{pin} ({}) {} -> {func}{level}",
                self.line(pin),
                Function::from_bits(a),
            );
        }
    }

    /// A `GPSET` / `GPCLR` write: report the pins it really drives, which are
    /// the ones whose function is `output`. The firmware reads these
    /// registers before it writes them and gets the block's tag back, so the
    /// other bits are noise (module docs).
    fn log_drive(&self, bank: usize, bits: u32, high: bool) {
        let mut bits = bits;
        while bits != 0 {
            let bit = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let pin = bank * 32 + bit;
            if pin >= PINS || self.function(pin) != Function::Output {
                continue;
            }
            crate::log!(
                self.log,
                Channel::Gpio,
                "{pin} ({}) {}",
                self.line(pin),
                if high { "high" } else { "low" }
            );
        }
    }

    /// A `PUP_PDN` write: say which pin's termination moved.
    fn log_pull(&self, reg: usize, was: u32, now: u32) {
        for pin in reg * 16..PINS.min(reg * 16 + 16) {
            let shift = (pin % 16) * 2;
            let (a, b) = (was >> shift & 3, now >> shift & 3);
            if a == b {
                continue;
            }
            crate::log!(
                self.log,
                Channel::Gpio,
                "{pin} ({}) {} -> {}",
                self.line(pin),
                Pull::from_bits(a),
                Pull::from_bits(b)
            );
        }
    }
}

impl MmioDevice for Gpio {
    fn name(&self) -> &'static str {
        "gpio"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        if let Some(reg) = Gpio::element(off, GPFSEL, GPFSEL_COUNT) {
            return Ok(self.fsel[reg]);
        }
        if Gpio::bank(off, GPSET).is_some() {
            return Ok(GPSET_RESET);
        }
        if Gpio::bank(off, GPCLR).is_some() {
            return Ok(GPCLR_RESET);
        }
        if let Some(bank) = Gpio::bank(off, GPLEV) {
            return Ok(self.levels(bank));
        }
        if let Some(bank) = Gpio::bank(off, GPEDS) {
            return Ok(self.eds[bank]);
        }
        for (i, &base) in DETECTS.iter().enumerate() {
            if let Some(bank) = Gpio::bank(off, base) {
                return Ok(self.detect[i][bank]);
            }
        }
        if off == GPPUD {
            return Ok(self.pud);
        }
        if let Some(bank) = Gpio::bank(off, GPPUDCLK) {
            return Ok(self.pudclk[bank]);
        }
        if off == PIN_MUX {
            return Ok(self.pin_mux);
        }
        if off == PAD_CFG {
            return Ok(self.pad_cfg);
        }
        if let Some(reg) = Gpio::element(off, PUP_PDN, PUP_PDN_COUNT) {
            return Ok(self.pup_pdn[reg]);
        }
        Ok(0)
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        // Every register here can move a pin's level or what watches it, so
        // the detectors run over the whole block after the write.
        let before = self.levels_all();
        self.store(offset & !3, value);
        self.detect_edges(before);
        Ok(())
    }
}

impl Gpio {
    /// One word into the block.
    fn store(&mut self, off: u32, value: u32) {
        if let Some(reg) = Gpio::element(off, GPFSEL, GPFSEL_COUNT) {
            let was = std::mem::replace(&mut self.fsel[reg], value);
            if was != value {
                self.log_fsel(reg, was, value);
            }
            return;
        }
        if let Some(bank) = Gpio::bank(off, GPSET) {
            self.log_drive(bank, value & !self.out[bank], true);
            self.out[bank] |= value;
            return;
        }
        if let Some(bank) = Gpio::bank(off, GPCLR) {
            self.log_drive(bank, value & self.out[bank], false);
            self.out[bank] &= !value;
            return;
        }
        if let Some(bank) = Gpio::bank(off, GPEDS) {
            self.eds[bank] &= !value;
            return;
        }
        for (i, &base) in DETECTS.iter().enumerate() {
            if let Some(bank) = Gpio::bank(off, base) {
                self.detect[i][bank] = value;
                return;
            }
        }
        // The BCM2835 pull registers: the BCM2711 pads do not listen to them,
        // so the words are kept and no termination moves.
        if off == GPPUD {
            self.pud = value;
            return;
        }
        if let Some(bank) = Gpio::bank(off, GPPUDCLK) {
            self.pudclk[bank] = value;
            return;
        }
        if off == PIN_MUX {
            self.pin_mux = value;
            return;
        }
        if off == PAD_CFG {
            self.pad_cfg = value;
            return;
        }
        if let Some(reg) = Gpio::element(off, PUP_PDN, PUP_PDN_COUNT) {
            let was = std::mem::replace(&mut self.pup_pdn[reg], value);
            if was != value {
                self.log_pull(reg, was, value);
            }
        }
        // GPLEV, and everything the block does not decode: dropped.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soc::board::{PI4B_8GB_REV_1_5, TYPE_PI4B};

    fn gpio() -> Gpio {
        Gpio::new()
    }

    fn wr(g: &mut Gpio, off: u32, value: u32) {
        g.write(off, Width::Word, value).unwrap();
    }

    fn rd(g: &mut Gpio, off: u32) -> u32 {
        g.read(off, Width::Word).unwrap()
    }

    /// The encoding the three `GPFSEL` bits use is not in numeric order past
    /// `output`, which is the thing to get wrong.
    #[test]
    fn function_select_decodes_the_alternates() {
        let mut g = gpio();
        // GPIO 14 and 15 on ALT0, as the firmware leaves the console pins.
        wr(&mut g, GPFSEL + 4, 0x0002_4000);
        assert_eq!(g.function(14), Function::Alt(0));
        assert_eq!(g.function(15), Function::Alt(0));
        // GPIO 40/41 on ALT0, 42 an output: `GPFSEL4` as a boot ends.
        wr(&mut g, GPFSEL + 16, 0x64);
        assert_eq!(g.function(40), Function::Alt(0));
        assert_eq!(g.function(42), Function::Output);
        // GPIO 28/29 on ALT5 (the RGMII MDIO bus), read back on a real board.
        wr(&mut g, GPFSEL + 8, 0x1200_0000);
        assert_eq!(g.function(28), Function::Alt(5));
        assert_eq!(g.function(29), Function::Alt(5));
        // The four pins of a flash session, ALT4.
        wr(&mut g, GPFSEL + 16, 0x6DB);
        for pin in 40..44 {
            assert_eq!(g.function(pin), Function::Alt(4), "{pin}");
        }
    }

    /// A read of `GPSET` / `GPCLR` answers the block's tag, which is what the
    /// bootloader's read-modify-write of the activity LED reads (module docs).
    #[test]
    fn the_write_only_registers_read_back_the_block_tag() {
        let mut g = gpio();
        for off in [GPSET, GPSET + 4, GPCLR, GPCLR + 4] {
            assert_eq!(rd(&mut g, off), 0x6770_696F, "{off:#x}");
        }
        assert_eq!(&GPSET_RESET.to_be_bytes(), b"gpio");
    }

    /// The activity LED: GPIO 42 an output, `GPSET1` / `GPCLR1` bit 10.
    #[test]
    fn an_output_pin_reads_its_own_latch() {
        let mut g = gpio();
        wr(&mut g, GPFSEL + 16, 0x40);
        assert_eq!(g.function(42), Function::Output);
        assert!(!g.level(42));
        wr(&mut g, GPSET + 4, 0x400);
        assert!(g.level(42));
        assert_eq!(rd(&mut g, GPLEV + 4) & 0x400, 0x400);
        wr(&mut g, GPCLR + 4, 0x400);
        assert!(!g.level(42));
        assert_eq!(rd(&mut g, GPLEV + 4) & 0x400, 0);
    }

    /// The bootloader sets the bits of the tag it read along with the one it
    /// means. They land in the latch, as they do on the real block, and change
    /// nothing: those pins are not outputs.
    #[test]
    fn a_set_of_pins_that_are_not_outputs_changes_no_level() {
        let mut g = gpio();
        // Bank 1 without the pulls it powers up with, so the latch is all
        // `GPLEV` has to report.
        wr(&mut g, PUP_PDN + 8, 0);
        wr(&mut g, PUP_PDN + 12, 0);
        wr(&mut g, GPFSEL + 16, 0x40); // GPIO 42 an output
        wr(&mut g, GPSET + 4, 0x6770_6D6F);
        assert!(g.level(42));
        assert_eq!(rd(&mut g, GPLEV + 4), 0x400);
    }

    /// An input reads its termination: `01` is a pull-up, measured on a real
    /// board on the pins whose lines need one.
    #[test]
    fn an_input_pin_reads_its_termination() {
        let mut g = gpio();
        // `PUP_PDN0` as start4 leaves it: GPIO 0 and 15 pulled up, 14 not.
        wr(&mut g, PUP_PDN, 0x4000_0005);
        assert_eq!(g.pull(0), Pull::Up);
        assert_eq!(g.pull(14), Pull::None);
        assert_eq!(g.pull(15), Pull::Up);
        assert!(g.level(0));
        assert!(!g.level(14));
        assert_eq!(rd(&mut g, GPLEV) & 0xC001, 0x8001);
        // A pull-down reads 0.
        wr(&mut g, PUP_PDN, 0x8000_000A);
        assert_eq!(g.pull(0), Pull::Down);
        assert_eq!(g.pull(15), Pull::Down);
        assert_eq!(rd(&mut g, GPLEV) & 0x8001, 0);
    }

    /// The BCM2835 pull registers are kept and do nothing: the BCM2711 pads
    /// only listen to `PUP_PDN`.
    #[test]
    fn the_legacy_pull_registers_move_no_termination() {
        let mut g = gpio();
        wr(&mut g, PUP_PDN, 0); // GPIO 4 off its reset pull-up
        wr(&mut g, GPPUD, 2); // "pull up" in the old encoding
        wr(&mut g, GPPUDCLK, 1 << 4);
        wr(&mut g, GPPUDCLK, 0);
        assert_eq!(g.pull(4), Pull::None);
        assert!(!g.level(4));
        assert_eq!(rd(&mut g, GPPUD), 2);
    }

    /// An edge detector latches `GPEDS` and raises its bank's line; `GPEDS`
    /// is write-1-to-clear, not storage. The pin here is the activity LED,
    /// which the firmware drives itself.
    #[test]
    fn an_edge_latches_gpeds_and_raises_the_line() {
        let mut g = gpio();
        wr(&mut g, GPFSEL + 16, 0x40); // GPIO 42 an output
        wr(&mut g, GPSET + 4, 0x400);
        wr(&mut g, GPCLR + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0, "no detector is enabled");
        assert_eq!(g.irq_lines(), [false, false]);

        wr(&mut g, GPREN + 4, 0x400);
        wr(&mut g, GPSET + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0x400);
        assert_eq!(g.irq_lines(), [false, true]);

        wr(&mut g, GPEDS + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0);
        assert_eq!(g.irq_lines(), [false, false]);

        // The falling edge needs its own enable.
        wr(&mut g, GPCLR + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0);
        wr(&mut g, GPFEN + 4, 0x400);
        wr(&mut g, GPSET + 4, 0x400);
        wr(&mut g, GPEDS + 4, 0x400);
        wr(&mut g, GPCLR + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0x400);
    }

    /// The asynchronous enables detect the same edges here: the model has no
    /// sampling clock to miss a pulse between.
    #[test]
    fn the_asynchronous_enables_detect_the_same_edges() {
        let mut g = gpio();
        wr(&mut g, GPFSEL + 16, 0x40);
        wr(&mut g, GPAREN + 4, 0x400);
        wr(&mut g, GPSET + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0x400);
        wr(&mut g, GPEDS + 4, 0x400);
        wr(&mut g, GPAFEN + 4, 0x400);
        wr(&mut g, GPCLR + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0x400);
    }

    /// A level detector holds its bit: clearing it while the pin is still at
    /// that level latches it again.
    #[test]
    fn a_level_detector_latches_again_while_the_level_lasts() {
        let mut g = gpio();
        wr(&mut g, GPFSEL + 16, 0x40);
        wr(&mut g, GPHEN + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0, "the pin is low");
        wr(&mut g, GPSET + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0x400);
        wr(&mut g, GPEDS + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0x400, "still high");
        wr(&mut g, GPCLR + 4, 0x400);
        wr(&mut g, GPEDS + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0);

        // And a low-level detector on an input nothing holds up.
        wr(&mut g, PUP_PDN, 0); // GPIO 4 off its reset pull-up
        wr(&mut g, GPLEN, 1 << 4);
        assert_eq!(rd(&mut g, GPEDS) & (1 << 4), 1 << 4);
        assert_eq!(g.irq_lines(), [true, false]);
        wr(&mut g, PUP_PDN, 1 << 8); // GPIO 4 pulled up
        wr(&mut g, GPEDS, 1 << 4);
        assert_eq!(rd(&mut g, GPEDS) & (1 << 4), 0);
    }

    /// A pin that does not exist is in no `GPEDS` bit, whatever a detector
    /// enable says about the bits above 57.
    #[test]
    fn the_bits_above_pin_57_latch_nothing() {
        let mut g = gpio();
        wr(&mut g, PUP_PDN + 8, 0); // bank 1 off its reset pulls, so every
        wr(&mut g, PUP_PDN + 12, 0); // pin in it reads low
        wr(&mut g, GPLEN + 4, 0xFFFF_FFFF);
        assert_eq!(rd(&mut g, GPEDS + 4), 0x03FF_FFFF);
        assert_eq!(g.irq_lines(), [false, true]);
    }

    /// Bit 1 of the undocumented word routes the card; the rest is storage.
    #[test]
    fn the_mux_word_says_which_sd_host_has_the_card() {
        let mut g = gpio();
        assert!(!g.sd_legacy());
        wr(&mut g, PIN_MUX, 0x2); // 2020-era bootcode, before its SD init
        assert!(g.sd_legacy());
        wr(&mut g, PIN_MUX, 0x1); // start4, which boots from EMMC2
        assert!(!g.sd_legacy());
        assert_eq!(rd(&mut g, PIN_MUX), 1);
    }

    /// The pin map is the board's, not the chip's.
    #[test]
    fn each_board_wires_the_pins_its_own_way() {
        let mut g = gpio();
        let board = |revision| Board {
            revision,
            ..Board::default()
        };
        g.fit_board(board(PI4B_8GB_REV_1_5));
        assert_eq!(board(PI4B_8GB_REV_1_5).board_type(), TYPE_PI4B);
        assert_eq!(g.line(42), "STATUS_LED_G_CLK");
        assert_eq!(g.line(48), "RGMII_RXD0");
        // A CM4 (`0x00B0_3141`): its own SD interface where the 4B has RGMII.
        g.fit_board(board(0x00B0_3141));
        assert_eq!(g.line(48), "SD0_CLK");
        assert_eq!(g.line(46), "SMPS_SCL");
        assert_eq!(g.line(42), "GPIO42");
        // A Pi 400 (`0x00C0_3130`): GPIO 42 is the power LED.
        g.fit_board(board(0x00C0_3130));
        assert_eq!(g.line(42), "PWR_LED_CLK");
        assert_eq!(g.line(48), "RGMII_RXD0");
        // And back to a 4B, with nothing left of the others.
        g.fit_board(board(PI4B_8GB_REV_1_5));
        assert_eq!(g.line(42), "STATUS_LED_G_CLK");
        assert_eq!(g.line(46), "RGMII_RXCLK");
    }

    /// Pins 58..63 have no pad: `GPFSEL5` keeps what is written there, and
    /// they are in no level.
    #[test]
    fn the_bits_above_pin_57_are_no_pin() {
        let mut g = gpio();
        wr(&mut g, GPFSEL + 20, 0xFFFF_FFFF);
        wr(&mut g, GPSET + 4, 0xFFFF_FFFF);
        assert_eq!(rd(&mut g, GPFSEL + 20), 0xFFFF_FFFF);
        assert_eq!(rd(&mut g, GPLEV + 4) >> 26, 0);
    }
}
