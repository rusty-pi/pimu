//! The always-on config / OTP engine at `0x7E20_F000`: the fuse array, one row
//! at a time. Registers and fields: `specs/otp.toml`.
//!
//! The key written to `KEY` is the **OTP row number**, so `ConfigOtp::table` is
//! the fuse array itself. It is not a real board's fuses and must never become
//! one — OTP holds device-unique and secret material (`CLAUDE.md`). Only rows
//! the boot depends on are modelled; the rest read 0, as unprogrammed fuses do.
//!
//! Which rows those are is not a matter of taste. start4's board check, which
//! `arm_loader` gates the ARM launch on, reads **rows 19..26** as two four-word
//! blocks and compares them against per-board-family constants in its own
//! `.rdata` — each block alone, then the two OR-ed together. Read them back as
//! 0 and start4 blinks LED error 4-4 ("unsupported board type") forever.
//! `vcgencmd otp_dump` is no help: from Linux those rows read `0xFFFF_FFFF`,
//! which is a redaction, not their contents. [`BOARD_IDENTITY`] is the value
//! that satisfies the check, taken from start4's own board-type table.
//!
//! The board serial (row 28, and its complement in row 29) is deliberately not
//! a real board's, only stable: the firmware derives the identity it publishes
//! from these rows, so `boot --scenario` pins values that a changed fuse moves.

use std::collections::{BTreeMap, BTreeSet};

use crate::bus::{BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};

// `PARAM_A` bit 0 starts a transaction; `STATUS` bit 1 reports completion.
use crate::spec::otp::{
    BOOTMODE as REG_BOOTMODE, CLKMUX as REG_CLKMUX, DATA as REG_DATA, KEY as REG_KEY,
    PARAM_A as REG_PARAM_A, PARAM_A_CMD_MASK as CMD, PARAM_A_CMD_SHIFT as CMD_SHIFT,
    PARAM_A_GO_MASK as GO, PARAM_B, STATUS as REG_STATUS, STATUS_DONE_MASK as DONE,
    STATUS_PROG_ENABLED_MASK as PROG_ENABLED,
};
use crate::spec::Coverage;

const CMD_READ: u32 = 0;
const CMD_PROG_ENABLE: u32 = 2;
const CMD_PROG_DISABLE: u32 = 3;
const CMD_PROGRAM: u32 = 10;

/// The four words start4 sends through `CMD_PROG_ENABLE` to unlock programming.
const PROG_ENABLE_KEY: [u32; 4] = [0xF, 0x4, 0x8, 0xD];

pub const COVERAGE: Coverage = Coverage {
    block: "otp",
    decoded: &[
        REG_BOOTMODE,
        REG_CLKMUX,
        REG_PARAM_A,
        PARAM_B,
        REG_STATUS,
        REG_DATA,
        REG_KEY,
    ],
};

const READY: u32 = (1 << 17) | (1 << 18) | (1 << 7);

/// The bootmode row, which `OTP_BOOTMODE_REG` presents without a transaction:
/// the boot ROM picks its boot source from it before anything else.
const BOOTMODE_ROW: u32 = 17;

/// The Hamming check bits over the identity block, as the boot ROM computes
/// them before deriving the bootcode's HMAC key: data bits fill the positions
/// of 1..=136 that are not powers of two, check bit `j` the parity of those
/// whose position has bit `j` set.
const fn identity_check(words: [u32; 4]) -> u32 {
    let mut check = 0;
    let mut j = 0;
    while j < 8 {
        let mut parity = 0;
        let mut d = 0;
        let mut p: u32 = 1;
        while p <= 136 {
            if !p.is_power_of_two() {
                if (p >> j) & 1 != 0 {
                    parity ^= (words[d >> 5] >> (d & 31)) & 1;
                }
                d += 1;
            }
            p += 1;
        }
        check |= parity << j;
        j += 1;
    }
    check
}

/// The board-identity block in OTP rows 19..22 (and again in 23..26).
///
/// These four words are start4's BCM2711 entry (4B, 400, CM4, CM4S), obfuscated
/// with `^ 0xB0BE_5AD5` in its `.rdata`. They come out of the firmware image,
/// not off any board, and identify the model rather than the unit. Stored whole
/// in both four-row blocks, which satisfies all three comparisons. The boot ROM
/// also folds them into the bootcode HMAC key ([`crate::firmware::bootrom`]).
pub(crate) const BOARD_IDENTITY: [u32; 4] = [0x8AA9_6D38, 0x9111_243F, 0x38E4_E488, 0x8E02_2082];

/// The invented 64-bit board serial. The firmware checks row 28 against its
/// complement in row 29, and the high half lives in row 35.
const SERIAL: u64 = 0xFA1E_0023_1AA2_BB31;

/// `0x8B0`: the bootmode row of a part with nothing fused into it. The bits set
/// are 4, 5, 7 and 11, and Raspberry Pi's `otp-bits` documents none of those.
///
/// It says nothing about where the board boots from. Three Raspberry Pi 4B
/// d03115 boards report `0x8B0` with `BOOT_ORDER=0xf14` and `BOOT_ORDER=0xf21`,
/// the second booting from the network first: on BCM2711 the boot source comes
/// from the EEPROM configuration.
///
/// It does carry secure boot, which none of those boards has. Fusing a
/// customer key sets bit 14 and bits 18:15 here, along with the key's SHA-256
/// in rows 47-54 and its count of 0 bits in the low byte of row 55, and the
/// EEPROM stages then take only signed files (`specs/otp.toml`). All of that is
/// blank in the model, so it is a part that has never been through
/// `rpi-eeprom-digest`, the same way rows 56-63 make it one that has never been
/// through `rpi-otp-private-key`.
const BOOTMODE: u32 = 0x0000_08B0;

/// The fuses that are neither derived nor part of a repeated block: row and
/// value. Rows 19-27 and 30 are worked out in [`ConfigOtp::new`] instead.
const DEFAULT_FUSES: &[(u32, u32)] = &[
    // OTP control; bits 26/27 would lock VC JTAG.
    (16, 0x0000_0001),
    // Bootmode and its copy.
    (17, BOOTMODE),
    (18, BOOTMODE),
    // Serial number, its complement, and its high 32 bits.
    (28, SERIAL as u32),
    (29, !(SERIAL as u32)),
    (35, (SERIAL >> 32) as u32),
    // Ethernet MAC `02:00:5E:00:53:01`, from RFC 7042's documentation range —
    // locally administered and unicast, which a tidier `01:…` would not be.
    // Row 65 holds the first four octets, most significant first, and bits
    // 31:16 of row 64 the last two. Programming them is optional, but they are
    // fused on the board this mirrors.
    (64, 0x5301_0000),
    (65, 0x0200_5E00),
];

/// What an OTP row is for, for the `io` and `otp` log lines. The meanings are
/// Raspberry Pi's own documented ones for pre-BCM2712 boards, apart from rows
/// 19-27 and 44, which are not public: 44 is a core-voltage trim start4 reads
/// with the identity rows.
pub fn row_meaning(row: u32) -> String {
    let word = |first: u32, words: u32| format!("word {} of {words}", row - first + 1);
    match row {
        16 => "OTP control: VideoCore JTAG lock".into(),
        17 => "boot mode".into(),
        18 => "boot mode, copy".into(),
        19..=22 => format!("board identity, {}", word(19, 4)),
        23..=26 => format!("board identity, second copy, {}", word(23, 4)),
        27 => "board identity check bits".into(),
        28 => "serial number".into(),
        29 => "serial number, bits inverted".into(),
        30 => "revision code: board model, RAM size, maker".into(),
        33 => "extended board revision".into(),
        35 => "serial number, high 32 bits".into(),
        36..=43 => format!("customer OTP, {}", word(36, 8)),
        44 => "core-voltage trim: 10 mV a set bit (not public)".into(),
        45 => "MPEG-2 codec licence key".into(),
        46 => "VC-1 codec licence key".into(),
        47..=54 => format!("secure-boot key hash, {}", word(47, 8)),
        55 => "secure-boot flags".into(),
        56..=63 => format!("device private key, {}", word(56, 8)),
        64 => "MAC address, bytes 5-6".into(),
        65 => "MAC address, bytes 1-4".into(),
        66 => "advanced boot, not used on BCM2711".into(),
        _ => "not documented".into(),
    }
}

/// The fuses of a board with signed boot for the RSA-2048 key `pubkey`, as
/// `rpi-eeprom-config`'s `pubkey.bin` has it (264 bytes): the boot mode bits
/// that make the EEPROM stages take only signed files (bit 14 and bit 15 of row
/// 17), the key's SHA-256 in rows 47 to 54, first word first and each word
/// little-endian, and its count of 0 bits in the low byte of row 55.
pub fn signed_boot_rows(pubkey: &[u8]) -> Vec<(u32, u32)> {
    let hash = sha256(pubkey);
    let words: Vec<u32> = hash
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect();
    let zero_bits: u32 = words.iter().map(|w| w.count_zeros()).sum();
    let mut rows = vec![(BOOTMODE_ROW, BOOTMODE | SIGNED_BOOT_BITS)];
    rows.extend(words.iter().enumerate().map(|(i, &w)| (47 + i as u32, w)));
    rows.push((55, zero_bits & 0xFF));
    rows
}

/// Bit 14, and a bit of 18:15: both set, and the EEPROM stages take only
/// signed files.
const SIGNED_BOOT_BITS: u32 = 1 << 14 | 1 << 15;

fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    let mut out = [0u8; 32];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *chunk = word.to_be_bytes();
    }
    out
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ConfigOtp {
    storage: BTreeMap<u32, u32>,
    key: u32,
    data: u32,
    /// `STATUS.DONE`: idle. A command clears it while it runs, and
    /// `ConfigOtp::busy` is the one `STATUS` read that sees it clear.
    done: bool,
    busy: bool,
    unlock: usize,
    prog_enabled: bool,
    table: BTreeMap<u32, u32>,
    /// Rows whose fuses do not blow: a program command on one completes and
    /// leaves it as it was, as it does on a part with a bad fuse.
    #[serde(default)]
    stuck: BTreeSet<u32>,
    pub log: Log,
}

impl Default for ConfigOtp {
    fn default() -> Self {
        ConfigOtp::new()
    }
}

impl ConfigOtp {
    pub fn new() -> ConfigOtp {
        let mut table = BTreeMap::new();
        // A complete but fictitious board identity. Which rows are *programmed*
        // mirrors a Raspberry Pi 4 Model B as it leaves the factory, because
        // firmware reading 0 from a row concludes the fuse is unprogrammed and
        // takes another path; the values are invented and deliberately look it
        // (`0xFA1E_00rr` where nothing reads them). Filling a control row with
        // a pattern is not harmless: `0xFA1E_0010` in row 16 would lock VC-JTAG.
        for row in 0..=5 {
            table.insert(row, 0xFA1E_0000 | row);
        }
        for &(row, word) in DEFAULT_FUSES {
            table.insert(row, word);
        }
        // 19-26: the identity block, which cannot be invented (module docs).
        for (i, word) in BOARD_IDENTITY.iter().enumerate() {
            table.insert(19 + i as u32, *word);
            table.insert(23 + i as u32, *word);
        }
        // 27: its Hamming check bits. Left blank, the boot ROM "corrects" a bit
        // that was right and the bootcode's signature stops matching. It ORs
        // bits 15:8 into 7:0, so one byte per copy, the same in both.
        let check = identity_check(BOARD_IDENTITY);
        table.insert(27, check | check << 8);
        // 30: revision code, the one value taken from real hardware — the
        // firmware decodes it into the board model it reports. It identifies a
        // model, not a unit.
        table.insert(30, crate::soc::Board::default().revision);
        // Left unprogrammed, as a board out of the factory has them: 36-43
        // customer OTP, 45/46 the codec licence keys, 47-54 the secure-boot key
        // hash, 55 its flags, and 56-63 the device private key, which nothing
        // but `rpi-otp-private-key` on a provisioned board ever writes.
        ConfigOtp {
            storage: BTreeMap::new(),
            key: 0,
            data: 0,
            done: true,
            busy: false,
            unlock: 0,
            prog_enabled: false,
            table,
            stuck: BTreeSet::new(),
            log: Log::default(),
        }
    }

    pub fn set(&mut self, key: u32, value: u32) {
        self.table.insert(key, value);
    }

    /// A row's fused value; unprogrammed rows read 0, as the hardware does.
    pub fn row(&self, key: u32) -> u32 {
        self.table.get(&key).copied().unwrap_or(0)
    }

    /// Make the fuses of `row` refuse to blow, so a program command on it
    /// changes nothing. For a scenario that needs a failed programming.
    pub fn stick(&mut self, row: u32) {
        self.stuck.insert(row);
    }

    /// The fuse array as it stands. A reset does not blank a fuse, so `boot`
    /// hands this to the next boot's machine.
    pub fn fuses(&self) -> &BTreeMap<u32, u32> {
        &self.table
    }

    /// Fuse every row of `fuses`, leaving the rows it says nothing about alone.
    /// A file cannot spell a blank row — an unprogrammed fuse reads 0 and is
    /// simply absent — so a row it does not carry keeps the value it had.
    pub fn fuse_rows(&mut self, fuses: &BTreeMap<u32, u32>) {
        self.table.extend(fuses);
    }

    /// `PARAM_A.GO`: run `cmd`, which completes at once.
    fn command(&mut self, cmd: u32) {
        match cmd {
            CMD_READ => self.read_row(),
            CMD_PROG_ENABLE => {
                // A wrong word starts the sequence over; a guess.
                self.unlock = if self.data == PROG_ENABLE_KEY[self.unlock] {
                    self.unlock + 1
                } else {
                    usize::from(self.data == PROG_ENABLE_KEY[0])
                };
                if self.unlock == PROG_ENABLE_KEY.len() {
                    self.prog_enabled = true;
                    self.unlock = 0;
                }
            }
            CMD_PROG_DISABLE => {
                self.prog_enabled = false;
                self.unlock = 0;
            }
            CMD_PROGRAM => self.program_row(),
            _ => crate::log!(
                self.log,
                Channel::Otp,
                "command {cmd} (unmodelled) on row {}",
                self.key
            ),
        }
        self.done = true;
        self.busy = true;
    }

    /// `CMD_PROGRAM`: OR the bits of `DATA` into row `KEY`, if unlocked.
    fn program_row(&mut self) {
        let (row, was) = (self.key, self.row(self.key));
        if !self.prog_enabled {
            crate::log!(
                self.log,
                Channel::Otp,
                "program row {row} ignored: programming is not enabled"
            );
            return;
        }
        if self.stuck.contains(&row) {
            crate::log!(
                self.log,
                Channel::Otp,
                "program row {row} (0x{row:x}) left as it was: the fuse does not blow"
            );
            return;
        }
        let value = was | self.data;
        if value != was {
            self.table.insert(row, value);
        }
        let meaning = row_meaning(row);
        self.log.otp_write(row, value, was, &meaning);
        crate::log!(
            self.log,
            Channel::Otp,
            "program row {row} (0x{row:x}): 0x{was:08x} -> 0x{value:08x}  {meaning}"
        );
    }

    fn read_row(&mut self) {
        let fused = self.table.contains_key(&self.key);
        self.data = self.table.get(&self.key).copied().unwrap_or(0);
        let meaning = row_meaning(self.key);
        self.log.otp_read(self.key, self.data, fused, &meaning);
        crate::log!(
            self.log,
            Channel::Otp,
            "key {} (0x{:x}) -> 0x{:08x}  {meaning}{}",
            self.key,
            self.key,
            self.data,
            if fused { "" } else { "  (UNMODELLED)" }
        );
    }
}

impl MmioDevice for ConfigOtp {
    fn name(&self) -> &'static str {
        "config-otp"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset & !3 {
            REG_STATUS => {
                // The model runs a command in no time, so the first `STATUS`
                // read after one stands for the interval `DONE` is clear.
                let done = self.done && !core::mem::take(&mut self.busy);
                (if done { DONE } else { 0 }) | if self.prog_enabled { PROG_ENABLED } else { 0 }
            }
            REG_BOOTMODE => self.row(BOOTMODE_ROW),
            REG_DATA => self.data,
            REG_KEY => self.key,
            off => self.storage.get(&off).copied().unwrap_or(READY),
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        match offset & !3 {
            REG_KEY => self.key = value,
            REG_PARAM_A => {
                self.storage.insert(REG_PARAM_A, value);
                if value & GO != 0 {
                    self.command((value & CMD) >> CMD_SHIFT);
                }
            }
            REG_DATA => self.data = value,
            // A write of 1 to `DONE` does nothing: on a Raspberry Pi 4B d03115
            // the flag reads back set straight after one.
            REG_STATUS => {}
            REG_CLKMUX => {
                self.storage.insert(REG_CLKMUX, value);
            }
            off => {
                self.storage.insert(off, value);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With row 27 in place the boot ROM's check finds nothing to correct; the
    /// syndrome is worked out independently here.
    #[test]
    fn identity_check_bits_leave_nothing_to_correct() {
        let otp = ConfigOtp::new();
        let words: Vec<u32> = (19..23).map(|r| otp.row(r) | otp.row(r + 4)).collect();
        let row27 = otp.row(27);
        let check = ((row27 >> 8) | row27) & 0xff;
        assert_eq!(check, 0x5d);
        let mut syndrome = 0;
        for j in 0..8 {
            let mut c = (check >> j) & 1;
            let mut d = 0usize;
            for p in 1u32..=136 {
                if p.is_power_of_two() {
                    continue;
                }
                if (p >> j) & 1 != 0 {
                    c ^= (words[d / 32] >> (d % 32)) & 1;
                }
                d += 1;
            }
            syndrome |= c << j;
        }
        assert_eq!(syndrome, 0);
    }

    /// A file holding one region says only that: the model is a board out of
    /// the factory, and a provisioned device private key goes over its rows.
    #[test]
    fn fusing_rows_leaves_the_ones_the_file_does_not_carry() {
        let mut otp = ConfigOtp::new();
        assert_eq!(otp.row(56), 0, "a factory-fresh board has no private key");
        otp.fuse_rows(&BTreeMap::from([(56, 0x5250_4956), (63, 1)]));
        assert_eq!(otp.row(56), 0x5250_4956);
        assert_eq!(otp.row(63), 1);
        assert_eq!(otp.row(19), BOARD_IDENTITY[0], "the identity block stays");
        assert_eq!(otp.row(17), 0x0000_08B0);
    }

    #[test]
    fn bootmode_reg_presents_the_bootmode_row() {
        let mut otp = ConfigOtp::new();
        assert_eq!(otp.read(REG_BOOTMODE, Width::Word).unwrap(), 0x0000_08B0);
        otp.set(BOOTMODE_ROW, 0x1234);
        assert_eq!(otp.read(REG_BOOTMODE, Width::Word).unwrap(), 0x1234);
    }

    /// `STATUS.DONE` is the block being idle. A poll that only watches for it
    /// set reads `DATA` before the row arrives — measured on a Raspberry Pi 4B
    /// d03115, where it made start4 read row 30 as 0.
    #[test]
    fn done_falls_for_the_command_and_a_write_of_one_does_nothing() {
        let mut otp = ConfigOtp::new();
        otp.set(30, 0x00d0_3115);
        assert_ne!(otp.read(REG_STATUS, Width::Word).unwrap() & DONE, 0);
        otp.write(REG_STATUS, Width::Word, DONE).unwrap();
        assert_ne!(otp.read(REG_STATUS, Width::Word).unwrap() & DONE, 0);
        otp.write(REG_KEY, Width::Word, 30).unwrap();
        otp.write(REG_PARAM_A, Width::Word, CMD_READ << CMD_SHIFT | GO)
            .unwrap();
        assert_eq!(otp.read(REG_STATUS, Width::Word).unwrap() & DONE, 0);
        assert_ne!(otp.read(REG_STATUS, Width::Word).unwrap() & DONE, 0);
        assert_eq!(otp.read(REG_DATA, Width::Word).unwrap(), 0x00d0_3115);
    }

    #[test]
    fn sha256_matches_the_standard_vectors() {
        let hex = |d: [u8; 32]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(
            hex(sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// The key's hash in rows 47-54 and its zero count in row 55 are what the
    /// bootloader prints as the customer key hash.
    #[test]
    fn signed_boot_rows_hold_the_keys_hash_and_its_zero_count() {
        let rows = signed_boot_rows(b"abc");
        assert_eq!(rows[0], (17, 0x0000_C8B0));
        assert_eq!(rows[1], (47, 0xbf16_78ba));
        assert_eq!(rows[8], (54, 0xad15_00f2));
        let zeros = rows[1..9].iter().map(|r| r.1.count_zeros()).sum::<u32>();
        assert_eq!(rows[9], (55, zeros & 0xff));
    }

    #[test]
    fn a_stuck_row_keeps_its_value_when_programmed() {
        let mut otp = ConfigOtp::new();
        otp.stick(60);
        send_key(&mut otp, PROG_ENABLE_KEY);
        program(&mut otp, 60, 0xFFFF_FFFF);
        program(&mut otp, 61, 0xFFFF_FFFF);
        assert_eq!(read_row(&mut otp, 60), 0);
        assert_eq!(read_row(&mut otp, 61), 0xFFFF_FFFF);
    }

    fn command(otp: &mut ConfigOtp, cmd: u32) {
        otp.write(REG_PARAM_A, Width::Word, cmd << CMD_SHIFT)
            .unwrap();
        otp.write(PARAM_B, Width::Word, 0).unwrap();
        otp.write(REG_PARAM_A, Width::Word, cmd << CMD_SHIFT | GO)
            .unwrap();
        assert_eq!(otp.read(REG_STATUS, Width::Word).unwrap() & DONE, 0);
        assert_ne!(otp.read(REG_STATUS, Width::Word).unwrap() & DONE, 0);
    }

    fn send_key(otp: &mut ConfigOtp, key: [u32; 4]) {
        for word in key {
            otp.write(REG_DATA, Width::Word, word).unwrap();
            command(otp, CMD_PROG_ENABLE);
        }
    }

    #[test]
    fn row_meanings_count_the_words_of_a_field_from_one() {
        assert_eq!(row_meaning(19), "board identity, word 1 of 4");
        assert_eq!(row_meaning(26), "board identity, second copy, word 4 of 4");
        assert_eq!(row_meaning(36), "customer OTP, word 1 of 8");
        assert_eq!(row_meaning(43), "customer OTP, word 8 of 8");
        assert_eq!(row_meaning(63), "device private key, word 8 of 8");
        assert_eq!(
            row_meaning(30),
            "revision code: board model, RAM size, maker"
        );
        assert_eq!(row_meaning(31), "not documented");
    }

    fn program(otp: &mut ConfigOtp, row: u32, bits: u32) {
        otp.write(REG_DATA, Width::Word, bits).unwrap();
        otp.write(REG_KEY, Width::Word, row).unwrap();
        command(otp, CMD_PROGRAM);
    }

    fn read_row(otp: &mut ConfigOtp, row: u32) -> u32 {
        otp.write(REG_KEY, Width::Word, row).unwrap();
        command(otp, CMD_READ);
        otp.read(REG_DATA, Width::Word).unwrap()
    }

    fn prog_enabled(otp: &mut ConfigOtp) -> bool {
        otp.read(REG_STATUS, Width::Word).unwrap() & PROG_ENABLED != 0
    }

    #[test]
    fn programming_ors_bits_into_rows_until_disabled() {
        let mut otp = ConfigOtp::new();
        send_key(&mut otp, PROG_ENABLE_KEY);
        assert!(prog_enabled(&mut otp));
        program(&mut otp, 36, 0x0000_00F0);
        program(&mut otp, 36, 0x0000_000F);
        program(&mut otp, 36, 0);
        assert_eq!(read_row(&mut otp, 36), 0x0000_00FF);
        command(&mut otp, CMD_PROG_DISABLE);
        assert!(!prog_enabled(&mut otp));
        program(&mut otp, 37, 1);
        assert_eq!(read_row(&mut otp, 37), 0);
        assert_eq!(otp.fuses().get(&36), Some(&0x0000_00FF));
    }

    /// Without the whole key, in order, programming does nothing.
    #[test]
    fn programming_needs_the_key_in_order() {
        let mut otp = ConfigOtp::new();
        program(&mut otp, 36, 1);
        send_key(&mut otp, [0xF, 0x4, 0xD, 0x8]);
        assert!(!prog_enabled(&mut otp));
        program(&mut otp, 36, 1);
        assert_eq!(otp.row(36), 0);
        send_key(&mut otp, [0x4, 0xF, 0x4, 0x8]);
        assert!(!prog_enabled(&mut otp));
        otp.write(REG_DATA, Width::Word, 0xD).unwrap();
        command(&mut otp, CMD_PROG_ENABLE);
        assert!(prog_enabled(&mut otp));
    }

    /// Only a read loads `DATA`.
    #[test]
    fn only_a_read_loads_data() {
        let mut otp = ConfigOtp::new();
        assert_eq!(read_row(&mut otp, 28), 0x1AA2_BB31);
        otp.write(REG_DATA, Width::Word, PROG_ENABLE_KEY[0])
            .unwrap();
        command(&mut otp, CMD_PROG_ENABLE);
        assert_eq!(otp.read(REG_DATA, Width::Word).unwrap(), PROG_ENABLE_KEY[0]);
    }
}
