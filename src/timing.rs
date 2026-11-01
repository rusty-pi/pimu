//! What the model charges modelled time for, and the measurements it comes from.
//!
//! The VPU's time is the sum of three things: how long its instructions take at
//! the clock the firmware has programmed ([`INSNS_PER_CYCLE`],
//! [`TAKEN_BRANCH_CYCLES`], the clock itself from
//! [`ClockManager::vpu_hz`](crate::periph::clockman::ClockManager::vpu_hz)), how
//! long the hardware it polls takes to answer where the model answers at once
//! (the rest of this module), and the waits the firmware asks for with the
//! system timer, which are exact.
//!
//! Every value was measured or fitted against a Raspberry Pi 4B d03115
//! (`Revision` of `/proc/cpuinfo`) booting the EEPROM release of 2026-08-04
//! (`0a7ef05f`) and `start4.elf` of 2026-09-14 (`a8698392`) from an SD card, with
//! `SIGNED_BOOT=1` and a signed `boot.img` of 16 MiB and of 30 MiB. The board's
//! own seconds-since-reset stamps in the bootloader's UART log are the time base:
//! host timestamps of the log run late by the backlog of a 115200 baud console.
//! None of it is read from a board at build or test time.

/// Instructions the VPU retires in a core clock cycle between taken control
/// transfers. The scalar pipeline dual-issues; 2 is the fit of the 1148 M
/// instructions of the bootloader's SHA-256 over a 16 MiB `boot.img` to the
/// 1.2 s to 1.7 s the board takes for them, together with
/// [`TAKEN_BRANCH_CYCLES`].
pub const INSNS_PER_CYCLE: u64 = 2;

/// Cycles a taken branch, jump, call or return costs on top of its own
/// instruction. Fitted to the instruction mix of three spans of the board's
/// boot: SHA-256 (3 % of instructions are taken transfers), the `start4.elf`
/// kernel inflate and CRC (11 %, 1.44 s on the board for 406 M instructions) and
/// the bootloader's load from SPI flash (15 %, 0.31 s).
pub const TAKEN_BRANCH_CYCLES: u64 = 10;

/// The core clock the peripheral blocks (SPI0, the I²C masters) divide down, in
/// MHz: `vcgencmd measure_clock core` of the board in the idle desktop, and what
/// the bootcode programs (PLLC at 3000 MHz over 6) before it prints anything.
pub const CORE_MHZ: u64 = 500;

/// The boot ROM's time before the bootcode's first line: it reads the bootcode
/// from the flash and checks it. Linear in the bootcode's size over the five
/// releases of 2025-05-08 to 2026-10-22 (55278 to 75062 bytes, 0.53 s to 0.64 s
/// to the first line).
pub const ROM_BASE_US: u64 = 189_000;
/// Tenths of a microsecond a byte of the bootcode adds to the ROM's time.
pub const ROM_TENTHS_US_PER_BYTE: u64 = 62;

/// How long one calibration handshake of the LPDDR4 PHY takes (a write of 1 to
/// `PHY_CAL_TRIGGER`). The bootloader runs 27 of them, 1.47 s on the board
/// between the `DDR 3200 Mbps` line (0.73 s) and `SD_SB` (2.20 s).
pub const PHY_CAL_US: u64 = 54_000;

/// How long an SD card takes to answer `ACMD41` with its power-up finished,
/// counted from the first one. The bootloader polls it 229 to 287 times on the
/// board, a pair of commands every 0.8 ms to 1 ms.
pub const SD_POWER_UP_US: u64 = 196_000;

/// Clocks the card and the host leave between a command and its response and
/// before the next command, on top of the 48 of the command and the 48 or 136 of
/// the response.
pub const SD_COMMAND_GAP_CLOCKS: u64 = 64;

/// A VPU instruction costs one unit of the system timer's cycle count; a clock
/// cycle is this many of them.
pub const UNITS_PER_CYCLE: u64 = INSNS_PER_CYCLE;

/// Units a taken control transfer adds to a step.
pub const TAKEN_BRANCH_UNITS: u64 = TAKEN_BRANCH_CYCLES * UNITS_PER_CYCLE;
