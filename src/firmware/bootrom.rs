//! Model of the BCM2711 on-chip boot ROM (the maskROM first stage).
//!
//! On real hardware the VPU comes out of reset in the 32 KiB ROM at
//! `0x6000_0000`, and that ROM — not `start4.elf`, not the EEPROM bootloader —
//! starts the machine: it splits the two VPU cores, brings up enough clocking to
//! reach SPI, reads `pieeprom.bin` off the flash, verifies the second-stage
//! bootcode's signature, stages it into L2-as-SRAM and hands control to it.
//!
//! The ROM image is Broadcom's mask and not redistributable, so this models its
//! *behaviour*, transcribed from a disassembly of a BCM2711C0 ROM. Like every
//! other stage it is a participant on the modelled bus: it reads the image
//! through [`Spi0`](crate::periph::spi0) and the key rows through
//! [`configotp`](crate::periph::configotp).
//!
//! The check at ROM `0x6000_0608` takes the last 20 bytes of the image as the
//! signature and compares HMAC-SHA1 of the rest, under `key = salt ^ otp` from
//! a 20-byte salt and OTP rows 19..=22, against it — the scheme
//! `raspberrypi/rpi-tools`' `signing-tool/sign.js` documents, confirmed by
//! reproducing the byte-exact HMAC in a stock signed footer.
//!
//! **The salt is a maskROM signing secret and is not in this repository.**
//! Checking is off unless the operator supplies the key at run time
//! ([`BootRom::from_env`]); without it the bootcode is staged unverified.
//!
//! Bootcode up to 2020-06-15 calls ROM routines directly, at addresses that
//! depend on the stepping; see [`RomHelper`].

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use sha1::Sha1;

use crate::bus::Bus;
use crate::firmware::eeprom::{EepromImage, BOOTCODE_ENTRY_OFFSET, BOOTCODE_LOAD_ADDR};
use crate::firmware::write_folded;
use crate::machine::Machine;
use crate::periph::configotp::BOARD_IDENTITY;
use crate::soc::Stepping;
use crate::spec::otp::{
    DATA as OTP_DATA, KEY as OTP_KEY, PARAM_A as OTP_PARAM_A, PARAM_A_GO_MASK as OTP_GO,
    STATUS as OTP_STATUS, STATUS_DONE_MASK as OTP_DONE,
};

/// Length of the HMAC-SHA1 signature the ROM appends and checks.
pub const HMAC_LEN: usize = 20;

/// The config/OTP block (`specs/otp.toml`), where the key rows are read.
const OTP_BASE: u32 = 0x7E20_F000;

/// OTP rows the HMAC key is built from: rows 19..=22, the board-identity block.
const OTP_KEY_ROWS: std::ops::RangeInclusive<u32> = 19..=22;

/// The mask ROM's OTP routines that bootcode up to 2020-06-15 calls directly,
/// through a trampoline that keys the pointers by `version`. The model has no
/// ROM at `0x6000_0000` — the address folds onto DRAM — so the stage puts
/// routines of its own where the machine's stepping keeps the ROM's. They are
/// not the ROM's code: open and close only set a clock mux the model absorbs,
/// and the reads skip the `STATUS` poll, since the transaction completes on
/// `GO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RomHelper {
    OtpOpen,
    OtpClose,
    ReadRow28,
    /// Returns the row named in `r0` in `r0`. Without it 2020-01-17 and
    /// 2020-06-15 print their board revision as junk.
    ReadRow,
}

impl RomHelper {
    const ALL: [RomHelper; 4] = [
        RomHelper::OtpOpen,
        RomHelper::OtpClose,
        RomHelper::ReadRow28,
        RomHelper::ReadRow,
    ];

    /// Where `stepping`'s ROM keeps it. The bootcode picks the table by
    /// `version`, except that a B0 whose halfwords at
    /// `0x6000_0798:0x6000_0796` read `0x1F1A_3364` takes C0's addresses. Same
    /// table and fingerprint check in every build from 2019-07-15 to 2020-06-15.
    const fn addr(self, stepping: Stepping) -> u32 {
        match (stepping, self) {
            (Stepping::C0, RomHelper::OtpOpen) => 0x6000_647A,
            (Stepping::C0, RomHelper::OtpClose) => 0x6000_1D50,
            (Stepping::C0, RomHelper::ReadRow28) => 0x6000_09D0,
            (Stepping::C0, RomHelper::ReadRow) => 0x6000_6278,
            (Stepping::B0, RomHelper::OtpOpen) => 0x6000_2D2A,
            (Stepping::B0, RomHelper::OtpClose) => 0x6000_193A,
            (Stepping::B0, RomHelper::ReadRow28) => 0x6000_0796,
            (Stepping::B0, RomHelper::ReadRow) => 0x6000_2CC0,
        }
    }

    const fn code(self) -> &'static [u8] {
        match self {
            RomHelper::OtpOpen | RomHelper::OtpClose => RETURN,
            RomHelper::ReadRow28 => READ_ROW_28,
            RomHelper::ReadRow => READ_ROW,
        }
    }
}

const RETURN: &[u8] = &[0x5A, 0x00]; // b lr

const READ_ROW_28: &[u8] = &[
    0x01, 0xE8, 0x00, 0xF0, 0x20, 0x7E, // mov r1, 0x7E20F000
    0xC2, 0x61, // mov r2, 28
    0x12, 0x37, // st r2, (r1+0x1C)    KEY
    0x12, 0x60, // mov r2, 1
    0x12, 0x32, // st r2, (r1+0x08)    PARAM_A.GO
    0x12, 0x26, // ld r2, (r1+0x18)    DATA
    0x02, 0x09, // st r2, (r0)
    0x5A, 0x00, // b lr
];

const READ_ROW: &[u8] = &[
    0x01, 0xE8, 0x00, 0xF0, 0x20, 0x7E, // mov r1, 0x7E20F000
    0x10, 0x37, // st r0, (r1+0x1C)    KEY
    0x12, 0x60, // mov r2, 1
    0x12, 0x32, // st r2, (r1+0x08)    PARAM_A.GO
    0x10, 0x26, // ld r0, (r1+0x18)    DATA
    0x5A, 0x00, // b lr
];

// On a B0 the row-28 stand-in sits where the bootcode fingerprints the ROM, so
// it must not read as the fingerprint, or the bootcode calls C0's addresses.
const _: () = {
    let at = RomHelper::ReadRow28.addr(Stepping::B0);
    let c = READ_ROW_28;
    let lo = c[0] as u32 | (c[1] as u32) << 8;
    let hi = c[2] as u32 | (c[3] as u32) << 8;
    assert!(at == 0x6000_0796 && (hi << 16 | lo) != 0x1F1A_3364);
};

const _: () = assert!(
    OTP_BASE == 0x7E20_F000
        && OTP_KEY == 0x1C
        && OTP_PARAM_A == 0x08
        && OTP_DATA == 0x18
        && OTP_GO == 1
);

/// The OTP half of the HMAC key derived from a constant, for tests: rows
/// 19..=22 little-endian, last four bytes zero.
pub fn otp_key_words() -> [u8; HMAC_LEN] {
    let mut otp = [0u8; HMAC_LEN];
    for (i, word) in BOARD_IDENTITY.iter().enumerate() {
        otp[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    otp
}

/// Read one OTP row: row number to `KEY`, start on `PARAM_A.GO`, wait for
/// `STATUS.DONE`, value from `DATA`.
fn read_otp_row(machine: &mut Machine, row: u32) -> u32 {
    let _ = machine.store32(OTP_BASE + OTP_KEY, row);
    let _ = machine.store32(OTP_BASE + OTP_PARAM_A, OTP_GO);
    // The transaction resolves on the `GO` write here; poll anyway, as the ROM
    // does, so the read exercises the `STATUS` path.
    for _ in 0..8 {
        if machine.load32(OTP_BASE + OTP_STATUS).unwrap_or(0) & OTP_DONE != 0 {
            break;
        }
    }
    machine.load32(OTP_BASE + OTP_DATA).unwrap_or(0)
}

/// The same, read from the modelled fuses over MMIO.
pub fn read_otp_key_words(machine: &mut Machine) -> [u8; HMAC_LEN] {
    let mut otp = [0u8; HMAC_LEN];
    for (i, row) in OTP_KEY_ROWS.enumerate() {
        otp[i * 4..i * 4 + 4].copy_from_slice(&read_otp_row(machine, row).to_le_bytes());
    }
    otp
}

fn xor20(salt: &[u8; HMAC_LEN], otp: &[u8; HMAC_LEN]) -> [u8; HMAC_LEN] {
    let mut key = [0u8; HMAC_LEN];
    for i in 0..HMAC_LEN {
        key[i] = salt[i] ^ otp[i];
    }
    key
}

fn hmac_ok(key: &[u8; HMAC_LEN], image: &[u8]) -> bool {
    if image.len() <= HMAC_LEN {
        return false;
    }
    let (message, signature) = image.split_at(image.len() - HMAC_LEN);
    let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC key length is unrestricted");
    mac.update(message);
    mac.verify_slice(signature).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigCheck {
    Skipped,
    Ok,
    /// HMAC-SHA1 mismatch — the real ROM halts here and the board stays silent.
    Failed,
}

/// What the ROM produced: the entry it hands control to, and a log of its steps.
pub struct BootOutcome {
    pub entry: u32,
    pub log: Vec<String>,
}

/// The modelled boot ROM. Holds the optional signing secret — supplied whole
/// (`key`) or as a `salt` XORed with the machine's OTP rows — which is a
/// run-time value and never lives in the repository.
#[derive(Debug, Clone, Default)]
pub struct BootRom {
    key: Option<[u8; HMAC_LEN]>,
    salt: Option<[u8; HMAC_LEN]>,
}

impl BootRom {
    /// A ROM with no signing key: it stages bootcode but does not check it.
    pub fn unkeyed() -> BootRom {
        BootRom::default()
    }

    pub fn with_key(key: [u8; HMAC_LEN]) -> BootRom {
        BootRom {
            key: Some(key),
            salt: None,
        }
    }

    /// A ROM with the maskROM salt; the key is `salt ^ otp`.
    pub fn with_salt(salt: [u8; HMAC_LEN]) -> BootRom {
        BootRom {
            key: None,
            salt: Some(salt),
        }
    }

    /// Build from the environment: `PIMU_BOOT_KEY=<40 hex>` is the key itself,
    /// `PIMU_BOOT_SALT=<40 hex>` the maskROM salt to XOR with the OTP rows.
    /// Neither set means an unkeyed ROM; `PIMU_BOOT_KEY` wins if both are.
    pub fn from_env() -> Result<BootRom> {
        if let Some(key) = env_hex20("PIMU_BOOT_KEY")? {
            return Ok(BootRom::with_key(key));
        }
        if let Some(salt) = env_hex20("PIMU_BOOT_SALT")? {
            return Ok(BootRom::with_salt(salt));
        }
        Ok(BootRom::unkeyed())
    }

    fn effective_key(&self, machine: &mut Machine) -> Option<[u8; HMAC_LEN]> {
        match (&self.key, &self.salt) {
            (Some(key), _) => Some(*key),
            (None, Some(salt)) => Some(xor20(salt, &read_otp_key_words(machine))),
            (None, None) => None,
        }
    }

    /// Model the ROM's first stage: read `pieeprom.bin` off the SPI flash,
    /// locate the bootcode, verify its signature if a key is available, stage it
    /// and return the entry hand-off. An error is where hardware would simply
    /// halt with the board silent.
    pub fn boot(&self, machine: &mut Machine) -> Result<BootOutcome> {
        let image = machine.spi0.flash_bytes().to_vec();
        if image.is_empty() {
            bail!("boot ROM: no SPI flash attached to read the bootloader image from");
        }
        let img = EepromImage::parse(&image).context("boot ROM: parsing pieeprom image")?;
        let bc = img
            .bootcode()
            .context("boot ROM: image has no bootcode section")?;
        let body = bc.body.clone();

        let mut log = vec![format!(
            "boot ROM: read {} bytes bootcode from SPI flash, staging at {:#010x}",
            body.len(),
            BOOTCODE_LOAD_ADDR
        )];

        let check = match self.effective_key(machine) {
            Some(key) => {
                if self.salt.is_some() {
                    log.push("boot ROM: read HMAC key OTP rows 19-22 over 0x7E20F000".into());
                }
                if hmac_ok(&key, &body) {
                    SigCheck::Ok
                } else {
                    SigCheck::Failed
                }
            }
            None => SigCheck::Skipped,
        };
        match check {
            SigCheck::Ok => log.push("boot ROM: bootcode HMAC-SHA1 signature verified".into()),
            SigCheck::Skipped => log.push(
                "boot ROM: signature check skipped (set PIMU_BOOT_KEY or PIMU_BOOT_SALT to enable)"
                    .into(),
            ),
            SigCheck::Failed => bail!(
                "boot ROM: bootcode HMAC-SHA1 signature mismatch — hardware would halt here, \
                 refusing to boot"
            ),
        }

        write_folded(machine, BOOTCODE_LOAD_ADDR, &body).context("boot ROM: staging bootcode")?;
        machine
            .l2
            .hold(BOOTCODE_LOAD_ADDR & 0x3FFF_FFFF, body.len());
        // Where `0x6000_0000` folds to (512 MiB in); a smaller RAM goes without.
        let stepping = machine.board().stepping;
        for helper in RomHelper::ALL {
            let _ = write_folded(machine, helper.addr(stepping), helper.code());
        }
        Ok(BootOutcome {
            entry: BOOTCODE_LOAD_ADDR + BOOTCODE_ENTRY_OFFSET,
            log,
        })
    }
}

fn env_hex20(name: &str) -> Result<Option<[u8; HMAC_LEN]>> {
    match std::env::var(name) {
        Ok(text) => {
            let bytes = decode_hex20(text.trim().trim_start_matches("0x"))
                .with_context(|| format!("{name}: expected {} hex bytes", HMAC_LEN))?;
            Ok(Some(bytes))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("{name}: {e}")),
    }
}

fn decode_hex20(s: &str) -> Result<[u8; HMAC_LEN]> {
    if s.len() != HMAC_LEN * 2 {
        bail!("expected {} hex chars, got {}", HMAC_LEN * 2, s.len());
    }
    let mut out = [0u8; HMAC_LEN];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .with_context(|| format!("non-hex at byte {i}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::firmware::eeprom::MAGIC_BOOTCODE;

    /// RFC 2202 HMAC-SHA1 test case 1: the same primitive the ROM's core is.
    #[test]
    fn hmac_sha1_matches_the_rfc_2202_vector() {
        let mut mac = Hmac::<Sha1>::new_from_slice(&[0x0b; 20]).unwrap();
        mac.update(b"Hi There");
        let got = mac.finalize().into_bytes();
        assert_eq!(
            got.as_slice(),
            hex("b617318655057264e28bc0b6fb378c8ef146be00").as_slice()
        );
    }

    fn sign(payload: &[u8], key: &[u8; HMAC_LEN]) -> Vec<u8> {
        let mut mac = Hmac::<Sha1>::new_from_slice(key).unwrap();
        mac.update(payload);
        let sig = mac.finalize().into_bytes();
        let mut image = payload.to_vec();
        image.extend_from_slice(&sig);
        image
    }

    #[test]
    fn hmac_ok_accepts_a_good_signature_and_rejects_tampering() {
        let key = [0x42u8; HMAC_LEN];
        let good = sign(b"second-stage bootcode payload", &key);
        assert!(hmac_ok(&key, &good));

        let mut tampered = good.clone();
        tampered[0] ^= 1;
        assert!(!hmac_ok(&key, &tampered));

        assert!(!hmac_ok(&[0x43u8; HMAC_LEN], &good), "wrong key");
        assert!(!hmac_ok(&key, &good[..10]), "too short to hold a signature");
    }

    /// The OTP read over MMIO matches the constant derivation while the fuses
    /// hold `BOARD_IDENTITY`.
    #[test]
    fn otp_read_over_mmio_matches_the_constant_words() {
        let mut machine = Machine::new(1 << 20);
        assert_eq!(read_otp_key_words(&mut machine), otp_key_words());
        assert_eq!(&otp_key_words()[16..20], &[0, 0, 0, 0]);
    }

    /// The row-28 helper 2020-04-16 calls at `0x6000_09d0` does the OTP
    /// transaction and stores the row where `r0` points.
    #[test]
    fn the_rom_s_row_28_helper_stores_the_row() {
        use crate::vpu::{Step, Vpu};
        const CODE: u32 = 0x1000;
        const RET: u32 = 0x2000;
        const BUF: u32 = 0x3000;
        let mut machine = Machine::new(1 << 20);
        write_folded(&mut machine, CODE, RomHelper::ReadRow28.code()).unwrap();
        machine.store32(BUF, 0xDEAD_BEEF).unwrap();
        let mut cpu = Vpu::new(CODE);
        cpu.regs.set(0, BUF);
        cpu.regs.set(26, RET);
        for _ in 0..16 {
            if cpu.regs.pc == RET {
                break;
            }
            assert_eq!(cpu.step(&mut machine), Step::Ran);
        }
        assert_eq!(cpu.regs.pc, RET);
        let want = read_otp_row(&mut machine, 28);
        assert_eq!(machine.load32(BUF).unwrap(), want);
    }

    /// The row reader 2020-01-17 and 2020-06-15 call at `0x6000_6278` takes the
    /// row in `r0` and returns its value there.
    #[test]
    fn the_rom_s_row_reader_returns_the_row_named_in_r0() {
        use crate::vpu::{Step, Vpu};
        const CODE: u32 = 0x1000;
        const RET: u32 = 0x2000;
        let mut machine = Machine::new(1 << 20);
        write_folded(&mut machine, CODE, RomHelper::ReadRow.code()).unwrap();
        for row in [17, 28, 30] {
            let mut cpu = Vpu::new(CODE);
            cpu.regs.set(0, row);
            cpu.regs.set(26, RET);
            for _ in 0..16 {
                if cpu.regs.pc == RET {
                    break;
                }
                assert_eq!(cpu.step(&mut machine), Step::Ran);
            }
            assert_eq!(cpu.regs.pc, RET);
            let want = read_otp_row(&mut machine, row);
            assert_eq!(cpu.regs.get(0), want, "row {row}");
        }
    }

    /// Each stepping's stand-ins fit where that stepping's ROM keeps them.
    #[test]
    fn each_stepping_s_helpers_have_room_of_their_own() {
        for stepping in [Stepping::B0, Stepping::C0] {
            let mut spans: Vec<(u32, u32)> = RomHelper::ALL
                .iter()
                .map(|h| (h.addr(stepping), h.addr(stepping) + h.code().len() as u32))
                .collect();
            spans.sort();
            for pair in spans.windows(2) {
                assert!(pair[0].1 <= pair[1].0, "{stepping}: {pair:x?} overlap");
            }
        }
        for h in RomHelper::ALL {
            assert_ne!(h.addr(Stepping::B0), h.addr(Stepping::C0), "{h:?}");
        }
    }

    #[test]
    fn the_row_reader_reports_the_board_the_machine_is() {
        use crate::soc::Board;
        let mut machine = Machine::new(1 << 20);
        assert_eq!(read_otp_row(&mut machine, 30), 0x00D0_3115);
        machine.set_board(Board::for_stepping(Stepping::B0));
        assert_eq!(read_otp_row(&mut machine, 30), 0x00C0_3112);
    }

    fn eeprom_with_bootcode(body: &[u8]) -> Vec<u8> {
        let mut img = Vec::new();
        img.extend_from_slice(&MAGIC_BOOTCODE.to_be_bytes());
        img.extend_from_slice(&(body.len() as u32).to_be_bytes());
        img.extend_from_slice(body);
        while !img.len().is_multiple_of(8) {
            img.push(0);
        }
        img.extend_from_slice(&[0, 0, 0, 0]); // EOF
        img
    }

    /// End to end through the bus: read the image, derive `salt ^ otp`, verify,
    /// stage the bootcode. A tampered image is refused, an unkeyed ROM is not.
    #[test]
    fn boot_reads_flash_and_otp_then_stages_a_verified_image() {
        let salt = [0x5au8; HMAC_LEN];
        let key = xor20(&salt, &otp_key_words());
        let body = sign(&vec![0xABu8; 0x400], &key);
        let image = eeprom_with_bootcode(&body);

        let mut machine = Machine::new(1 << 20);
        machine.spi0.attach_flash(image.clone());
        let rom = BootRom::with_salt(salt);
        let out = rom.boot(&mut machine).expect("verified image boots");
        assert_eq!(out.entry, BOOTCODE_LOAD_ADDR + BOOTCODE_ENTRY_OFFSET);
        assert_eq!(machine.ram.read_slice(0, 4).unwrap(), &[0xAB; 4]);

        let mut bad_body = body.clone();
        bad_body[0] ^= 1;
        let mut m2 = Machine::new(1 << 20);
        m2.spi0.attach_flash(eeprom_with_bootcode(&bad_body));
        assert!(rom.boot(&mut m2).is_err(), "bad signature must refuse");

        let mut m3 = Machine::new(1 << 20);
        m3.spi0.attach_flash(image);
        assert!(BootRom::unkeyed().boot(&mut m3).is_ok());

        let mut m4 = Machine::new(1 << 20);
        assert!(rom.boot(&mut m4).is_err(), "no flash must refuse");
    }

    #[test]
    fn decode_hex20_validates_length_and_digits() {
        assert!(decode_hex20("00").is_err());
        assert!(decode_hex20(&"zz".repeat(20)).is_err());
        assert_eq!(decode_hex20(&"a1".repeat(20)).unwrap(), [0xa1u8; 20]);
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }
}
