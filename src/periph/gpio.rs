//! The GPIO block (`0x7E20_0000`): 58 pins, their functions, levels and pulls.
//! Registers and fields: `specs/gpio.toml`.
//!
//! * `GPSET` and `GPCLR` answer `0x6770696f` — `"gpio"`, the block's own tag —
//!   on a read, not zero and not the last value written. The bootloader's
//!   activity LED is a read-modify-write of `GPSET1`, so a real board is told
//!   to set every pin in the tag as well; harmless, because `GPSET` only drives
//!   pins whose function is `output`.
//! * Nothing outside the model drives a pin, so a `GPLEV` bit is the output
//!   latch for an output and the pin's termination otherwise: pull-up reads 1,
//!   pull-down and no pull read 0. Pins still move (the firmware drives the
//!   LED, a `PUP_PDN` write moves what holds an input), so the detect enables
//!   work — no firmware in a boot enables one.
//! * The BCM2711 pads ignore the BCM2835 pull registers; only `PUP_PDN` moves
//!   a termination.
//! * Two masters reach what they reach only while their pins are muxed to them,
//!   which [`crate::machine::Machine`] follows: SPI0 the boot flash on GPIO
//!   40..43 ALT4, and I²C 0 a HAT's ID EEPROM on GPIO 0/1 ALT0.
//!
//! The block is the chip's, but what a pin is wired to is the board's: the
//! names come from the 4B's device tree and the boards that differ say so in
//! the firmware's `dt-blob` ([`Gpio::fit_board`]).

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

pub const COVERAGE: Coverage = Coverage {
    block: "gpio",
    decoded: &[
        GPFSEL, GPSET, GPCLR, GPLEV, GPEDS, GPREN, GPFEN, GPHEN, GPLEN, GPAREN, GPAFEN, GPPUD,
        GPPUDCLK, PIN_MUX, PAD_CFG, PUP_PDN,
    ],
};

pub const PINS: usize = 58;

const BANKS: usize = 2;

/// The six edge / level detect enables, in window order.
const DETECTS: [u32; 6] = [GPREN, GPFEN, GPHEN, GPLEN, GPAREN, GPAFEN];
const REN: usize = 0;
const FEN: usize = 1;
const HEN: usize = 2;
const LEN: usize = 3;
const AREN: usize = 4;
const AFEN: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    Input,
    Output,
    Alt(u8),
}

impl Function {
    /// The `GPFSEL` encoding: 0 input, 1 output, then ALT5, ALT4, ALT0..ALT3.
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

/// Each pin's six alternate functions, `ALT0` first, from BCM2711 ARM
/// Peripherals §5.3 Table 94; an empty string is a reserved cell, and pins
/// 46..57 are internal. Reported alongside the board line on a `GPFSEL` write,
/// so a log names the peripheral rather than only `ALT4`.
const ALT_FUNCTIONS: [[&str; 6]; PINS] = [
    ["SDA0", "SA5", "PCLK", "SPI3_CE0_N", "TXD2", "SDA6"], // GPIO0
    ["SCL0", "SA4", "DE", "SPI3_MISO", "RXD2", "SCL6"],    // GPIO1
    ["SDA1", "SA3", "LCD_VSYNC", "SPI3_MOSI", "CTS2", "SDA3"], // GPIO2
    ["SCL1", "SA2", "LCD_HSYNC", "SPI3_SCLK", "RTS2", "SCL3"], // GPIO3
    ["GPCLK0", "SA1", "DPI_D0", "SPI4_CE0_N", "TXD3", "SDA3"], // GPIO4
    ["GPCLK1", "SA0", "DPI_D1", "SPI4_MISO", "RXD3", "SCL3"], // GPIO5
    [
        "GPCLK2",
        "SOE_N / SE",
        "DPI_D2",
        "SPI4_MOSI",
        "CTS3",
        "SDA4",
    ], // GPIO6
    [
        "SPI0_CE1_N",
        "SWE_N / SRW_N",
        "DPI_D3",
        "SPI4_SCLK",
        "RTS3",
        "SCL4",
    ], // GPIO7
    [
        "SPI0_CE0_N",
        "SD0",
        "DPI_D4",
        "BSCSL / CE_N",
        "TXD4",
        "SDA4",
    ], // GPIO8
    ["SPI0_MISO", "SD1", "DPI_D5", "BSCSL / MISO", "RXD4", "SCL4"], // GPIO9
    [
        "SPI0_MOSI",
        "SD2",
        "DPI_D6",
        "BSCSL SDA / MOSI",
        "CTS4",
        "SDA5",
    ], // GPIO10
    [
        "SPI0_SCLK",
        "SD3",
        "DPI_D7",
        "BSCSL SCL / SCLK",
        "RTS4",
        "SCL5",
    ], // GPIO11
    ["PWM0_0", "SD4", "DPI_D8", "SPI5_CE0_N", "TXD5", "SDA5"], // GPIO12
    ["PWM0_1", "SD5", "DPI_D9", "SPI5_MISO", "RXD5", "SCL5"], // GPIO13
    ["TXD0", "SD6", "DPI_D10", "SPI5_MOSI", "CTS5", "TXD1"], // GPIO14
    ["RXD0", "SD7", "DPI_D11", "SPI5_SCLK", "RTS5", "RXD1"], // GPIO15
    ["", "SD8", "DPI_D12", "CTS0", "SPI1_CE2_N", "CTS1"],  // GPIO16
    ["", "SD9", "DPI_D13", "RTS0", "SPI1_CE1_N", "RTS1"],  // GPIO17
    [
        "PCM_CLK",
        "SD10",
        "DPI_D14",
        "SPI6_CE0_N",
        "SPI1_CE0_N",
        "PWM0_0",
    ], // GPIO18
    [
        "PCM_FS",
        "SD11",
        "DPI_D15",
        "SPI6_MISO",
        "SPI1_MISO",
        "PWM0_1",
    ], // GPIO19
    [
        "PCM_DIN",
        "SD12",
        "DPI_D16",
        "SPI6_MOSI",
        "SPI1_MOSI",
        "GPCLK0",
    ], // GPIO20
    [
        "PCM_DOUT",
        "SD13",
        "DPI_D17",
        "SPI6_SCLK",
        "SPI1_SCLK",
        "GPCLK1",
    ], // GPIO21
    ["SD0_CLK", "SD14", "DPI_D18", "SD1_CLK", "ARM_TRST", "SDA6"], // GPIO22
    ["SD0_CMD", "SD15", "DPI_D19", "SD1_CMD", "ARM_RTCK", "SCL6"], // GPIO23
    [
        "SD0_DAT0",
        "SD16",
        "DPI_D20",
        "SD1_DAT0",
        "ARM_TDO",
        "SPI3_CE1_N",
    ], // GPIO24
    [
        "SD0_DAT1",
        "SD17",
        "DPI_D21",
        "SD1_DAT1",
        "ARM_TCK",
        "SPI4_CE1_N",
    ], // GPIO25
    [
        "SD0_DAT2",
        "",
        "DPI_D22",
        "SD1_DAT2",
        "ARM_TDI",
        "SPI5_CE1_N",
    ], // GPIO26
    [
        "SD0_DAT3",
        "",
        "DPI_D23",
        "SD1_DAT3",
        "ARM_TMS",
        "SPI6_CE1_N",
    ], // GPIO27
    ["SDA0", "SA5", "PCM_CLK", "", "MII_A_RX_ERR", "RGMII_MDIO"], // GPIO28
    ["SCL0", "SA4", "PCM_FS", "", "MII_A_TX_ERR", "RGMII_MDC"], // GPIO29
    ["", "SA3", "PCM_DIN", "CTS0", "MII_A_CRS", "CTS1"],   // GPIO30
    ["", "SA2", "PCM_DOUT", "RTS0", "MII_A_COL", "RTS1"],  // GPIO31
    ["GPCLK0", "SA1", "", "TXD0", "SD_CARD_PRESENT", "TXD1"], // GPIO32
    ["", "SA0", "", "RXD0", "SD_CARD_WRPROT", "RXD1"],     // GPIO33
    [
        "GPCLK0",
        "SOE_N / SE",
        "",
        "SD1_CLK",
        "SD_CARD_LED",
        "RGMII_IRQ",
    ], // GPIO34
    [
        "SPI0_CE1_N",
        "SWE_N / SRW_N",
        "",
        "SD1_CMD",
        "RGMII_START_STOP",
        "",
    ], // GPIO35
    [
        "SPI0_CE0_N",
        "SD0",
        "TXD0",
        "SD1_DAT0",
        "RGMII_RX_OK",
        "MII_A_RX_ERR",
    ], // GPIO36
    [
        "SPI0_MISO",
        "SD1",
        "RXD0",
        "SD1_DAT1",
        "RGMII_MDIO",
        "MII_A_TX_ERR",
    ], // GPIO37
    [
        "SPI0_MOSI",
        "SD2",
        "RTS0",
        "SD1_DAT2",
        "RGMII_MDC",
        "MII_A_CRS",
    ], // GPIO38
    [
        "SPI0_SCLK",
        "SD3",
        "CTS0",
        "SD1_DAT3",
        "RGMII_IRQ",
        "MII_A_COL",
    ], // GPIO39
    ["PWM1_0", "SD4", "", "SD1_DAT4", "SPI0_MISO", "TXD1"], // GPIO40
    ["PWM1_1", "SD5", "", "SD1_DAT5", "SPI0_MOSI", "RXD1"], // GPIO41
    ["GPCLK1", "SD6", "", "SD1_DAT6", "SPI0_SCLK", "RTS1"], // GPIO42
    ["GPCLK2", "SD7", "", "SD1_DAT7", "SPI0_CE0_N", "CTS1"], // GPIO43
    ["GPCLK1", "SDA0", "SDA1", "", "SPI0_CE1_N", "SD_CARD_VOLT"], // GPIO44
    ["PWM0_1", "SCL0", "SCL1", "", "SPI0_CE2_N", "SD_CARD_PWR0"], // GPIO45
    ["", "", "", "", "", ""],                              // GPIO46
    ["", "", "", "", "", ""],                              // GPIO47
    ["", "", "", "", "", ""],                              // GPIO48
    ["", "", "", "", "", ""],                              // GPIO49
    ["", "", "", "", "", ""],                              // GPIO50
    ["", "", "", "", "", ""],                              // GPIO51
    ["", "", "", "", "", ""],                              // GPIO52
    ["", "", "", "", "", ""],                              // GPIO53
    ["", "", "", "", "", ""],                              // GPIO54
    ["", "", "", "", "", ""],                              // GPIO55
    ["", "", "", "", "", ""],                              // GPIO56
    ["", "", "", "", "", ""],                              // GPIO57
];

/// What each pin is wired to on a Pi 4B (`gpio-line-names` of
/// `gpio@7e200000`); 2..27 are header pins a user owns.
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

/// What a Compute Module 4 does differently (`pins_cm4` of the `dt-blob`): its
/// own SD interface where a 4B has RGMII, the SMPS I²C below it, and no
/// activity LED on GPIO 42.
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

/// What a Pi 400 does differently (`pins_400`): GPIO 42 drives the power LED
/// and the firmware starts it *on*. Same pin as the SPI flash clock either
/// way.
const PI400_LINES: &[(usize, &str)] = &[(42, "PWR_LED_CLK")];

/// `PUP_PDN` out of reset. A pin nothing drives reads its termination, so a
/// firmware that looks before it sets a pull sees these.
const PUP_PDN_RESET: [u32; PUP_PDN_COUNT as usize] =
    [0xAAA9_5555, 0xA0AA_AAAA, 0x50AA_A95A, 0x0005_5555];

pub struct Gpio {
    fsel: [u32; GPFSEL_COUNT as usize],
    /// The output latch, whatever the pin's function is.
    out: [u32; BANKS],
    eds: [u32; BANKS],
    detect: [[u32; BANKS]; DETECTS.len()],
    /// The BCM2835 pull registers: kept, and inert.
    pud: u32,
    pudclk: [u32; BANKS],
    pup_pdn: [u32; PUP_PDN_COUNT as usize],
    pin_mux: u32,
    pad_cfg: u32,
    lines: [&'static str; PINS],
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

    /// What `ALT0`..`ALT5` means on this pin, or `""` where there is none.
    pub fn alt_function(pin: usize, func: Function) -> &'static str {
        let Function::Alt(n) = func else {
            return "";
        };
        ALT_FUNCTIONS
            .get(pin)
            .and_then(|alts| alts.get(n as usize))
            .copied()
            .unwrap_or("")
    }

    pub fn line(&self, pin: usize) -> &'static str {
        self.lines.get(pin).copied().unwrap_or("?")
    }

    /// The function `pin`'s three `GPFSEL` bits name; pins the chip does not
    /// bring out are inputs.
    pub fn function(&self, pin: usize) -> Function {
        if pin >= PINS {
            return Function::Input;
        }
        let (reg, shift) = (pin / 10, (pin % 10) * 3);
        Function::from_bits(self.fsel[reg] >> shift)
    }

    pub fn pull(&self, pin: usize) -> Pull {
        if pin >= PINS {
            return Pull::None;
        }
        let (reg, shift) = (pin / 16, (pin % 16) * 2);
        Pull::from_bits(self.pup_pdn[reg] >> shift)
    }

    /// A `GPLEV` bit: an output's latch, otherwise the pin's termination.
    pub fn level(&self, pin: usize) -> bool {
        if self.function(pin) == Function::Output {
            self.out[pin / 32] >> (pin % 32) & 1 != 0
        } else {
            self.pull(pin).level()
        }
    }

    /// Bit 1 of `+0xD0`: the SD slot is on the legacy EMMC controller rather
    /// than EMMC2. The machine routes the card; the block holds the bit.
    pub fn sd_legacy(&self) -> bool {
        self.pin_mux & PIN_MUX_SD_LEGACY_MASK != 0
    }

    fn levels(&self, bank: usize) -> u32 {
        let mut v = 0;
        for pin in bank * 32..PINS.min((bank + 1) * 32) {
            if self.level(pin) {
                v |= 1 << (pin % 32);
            }
        }
        v
    }

    fn levels_all(&self) -> [u32; BANKS] {
        [self.levels(0), self.levels(1)]
    }

    fn pins_of(bank: usize) -> u32 {
        let n = PINS.min((bank + 1) * 32) - bank * 32;
        u32::MAX >> (32 - n)
    }

    /// What the six detect enables make of the levels moving from `before`.
    /// A level detect re-latches while the pin stays at that level, so a
    /// `GPEDS` write clears its bit only until the next access; and a detector
    /// watches the pad, so a pin the firmware drives itself counts too.
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

    /// The two bank interrupt lines: up while any pin of that bank has its
    /// `GPEDS` bit latched. The other two follow from these ([`crate::arm`]).
    pub fn irq_lines(&self) -> [bool; BANKS] {
        [self.eds[0] != 0, self.eds[1] != 0]
    }

    fn element(off: u32, base: u32, count: u32) -> Option<usize> {
        let i = off.checked_sub(base)? / 4;
        (i < count).then_some(i as usize)
    }

    fn bank(off: u32, base: u32) -> Option<usize> {
        Gpio::element(off, base, BANKS as u32)
    }

    fn log_fsel(&self, reg: usize, was: u32, now: u32) {
        for pin in reg * 10..PINS.min(reg * 10 + 10) {
            let shift = (pin % 10) * 3;
            let (a, b) = (was >> shift & 7, now >> shift & 7);
            if a == b {
                continue;
            }
            // A pin that becomes an output starts driving the latch it
            // already had — which is how the LED comes on.
            let func = Function::from_bits(b);
            let level = match (func, self.level(pin)) {
                (Function::Output, true) => ", high",
                (Function::Output, false) => ", low",
                _ => "",
            };
            crate::log!(
                self.log,
                Channel::Gpio,
                "{pin} ({}) {} -> {func}{}{level}",
                self.line(pin),
                Function::from_bits(a),
                match Gpio::alt_function(pin, func) {
                    "" => String::new(),
                    name => format!(" ({name})"),
                },
            );
        }
    }

    /// A `GPSET` / `GPCLR` write: report only the pins it really drives. The
    /// rest is the tag the firmware read back (module docs).
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
        // Any of these can move a level or what watches it.
        let before = self.levels_all();
        self.store(offset & !3, value);
        self.detect_edges(before);
        Ok(())
    }
}

impl Gpio {
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
        // The BCM2711 pads ignore these; keep the words, move nothing.
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

    /// The `GPFSEL` encoding is not in numeric order past `output`.
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
        // GPIO 28/29 on ALT5 (the RGMII MDIO bus), read back on a
        // Raspberry Pi 4B d03115.
        wr(&mut g, GPFSEL + 8, 0x1200_0000);
        assert_eq!(g.function(28), Function::Alt(5));
        assert_eq!(g.function(29), Function::Alt(5));
        // The four pins of a flash session, ALT4.
        wr(&mut g, GPFSEL + 16, 0x6DB);
        for pin in 40..44 {
            assert_eq!(g.function(pin), Function::Alt(4), "{pin}");
        }
    }

    /// A read of `GPSET` / `GPCLR` answers the block's tag (module docs).
    #[test]
    fn the_write_only_registers_read_back_the_block_tag() {
        let mut g = gpio();
        for off in [GPSET, GPSET + 4, GPCLR, GPCLR + 4] {
            assert_eq!(rd(&mut g, off), 0x6770_696F, "{off:#x}");
        }
        assert_eq!(&GPSET_RESET.to_be_bytes(), b"gpio");
    }

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

    /// The tag's bits land in the latch, as on the real block, and change
    /// nothing: those pins are not outputs.
    #[test]
    fn a_set_of_pins_that_are_not_outputs_changes_no_level() {
        let mut g = gpio();
        wr(&mut g, PUP_PDN + 8, 0);
        wr(&mut g, PUP_PDN + 12, 0);
        wr(&mut g, GPFSEL + 16, 0x40); // GPIO 42 an output
        wr(&mut g, GPSET + 4, 0x6770_6D6F);
        assert!(g.level(42));
        assert_eq!(rd(&mut g, GPLEV + 4), 0x400);
    }

    /// An input reads its termination: `01` is a pull-up, as measured on a
    /// Raspberry Pi 4B d03115 on the pins whose lines need one.
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
        wr(&mut g, PUP_PDN, 0x8000_000A);
        assert_eq!(g.pull(0), Pull::Down);
        assert_eq!(g.pull(15), Pull::Down);
        assert_eq!(rd(&mut g, GPLEV) & 0x8001, 0);
    }

    /// The alternate-function table is what makes a pin-mux log readable.
    #[test]
    fn a_pin_says_which_peripheral_an_alt_gives_it_to() {
        assert_eq!(Gpio::alt_function(40, Function::Alt(4)), "SPI0_MISO");
        assert_eq!(Gpio::alt_function(43, Function::Alt(4)), "SPI0_CE0_N");
        assert_eq!(Gpio::alt_function(14, Function::Alt(0)), "TXD0");
        assert_eq!(Gpio::alt_function(14, Function::Alt(5)), "TXD1");
        assert_eq!(Gpio::alt_function(28, Function::Alt(5)), "RGMII_MDIO");
        assert_eq!(Gpio::alt_function(40, Function::Alt(0)), "PWM1_0");
        assert_eq!(Gpio::alt_function(18, Function::Alt(0)), "PCM_CLK");
        assert_eq!(Gpio::alt_function(16, Function::Alt(0)), "");
        assert_eq!(Gpio::alt_function(48, Function::Alt(0)), "");
        assert_eq!(Gpio::alt_function(58, Function::Alt(0)), "");
        assert_eq!(Gpio::alt_function(14, Function::Output), "");
    }

    /// The BCM2835 pull registers are kept and do nothing.
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
    /// is write-1-to-clear, not storage.
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

        wr(&mut g, GPCLR + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0);
        wr(&mut g, GPFEN + 4, 0x400);
        wr(&mut g, GPSET + 4, 0x400);
        wr(&mut g, GPEDS + 4, 0x400);
        wr(&mut g, GPCLR + 4, 0x400);
        assert_eq!(rd(&mut g, GPEDS + 4), 0x400);
    }

    /// The asynchronous enables detect the same edges: no sampling clock.
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

    /// A level detector re-latches while the pin stays at that level.
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

        wr(&mut g, PUP_PDN, 0); // GPIO 4 off its reset pull-up
        wr(&mut g, GPLEN, 1 << 4);
        assert_eq!(rd(&mut g, GPEDS) & (1 << 4), 1 << 4);
        assert_eq!(g.irq_lines(), [true, false]);
        wr(&mut g, PUP_PDN, 1 << 8); // GPIO 4 pulled up
        wr(&mut g, GPEDS, 1 << 4);
        assert_eq!(rd(&mut g, GPEDS) & (1 << 4), 0);
    }

    /// A pin that does not exist is in no `GPEDS` bit.
    #[test]
    fn the_bits_above_pin_57_latch_nothing() {
        let mut g = gpio();
        wr(&mut g, PUP_PDN + 8, 0); // bank 1 off its reset pulls, so every
        wr(&mut g, PUP_PDN + 12, 0); // pin in it reads low
        wr(&mut g, GPLEN + 4, 0xFFFF_FFFF);
        assert_eq!(rd(&mut g, GPEDS + 4), 0x03FF_FFFF);
        assert_eq!(g.irq_lines(), [false, true]);
    }

    /// Bit 1 of the undocumented word routes the card.
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
        g.fit_board(board(PI4B_8GB_REV_1_5));
        assert_eq!(g.line(42), "STATUS_LED_G_CLK");
        assert_eq!(g.line(46), "RGMII_RXCLK");
    }

    /// Pins 58..63 have no pad.
    #[test]
    fn the_bits_above_pin_57_are_no_pin() {
        let mut g = gpio();
        wr(&mut g, GPFSEL + 20, 0xFFFF_FFFF);
        wr(&mut g, GPSET + 4, 0xFFFF_FFFF);
        assert_eq!(rd(&mut g, GPFSEL + 20), 0xFFFF_FFFF);
        assert_eq!(rd(&mut g, GPLEV + 4) >> 26, 0);
    }
}
