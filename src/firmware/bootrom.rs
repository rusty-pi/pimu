//! Model of the BCM2711 on-chip boot ROM (the maskROM first stage).
//!
//! On real hardware the VPU comes out of reset executing the 32 KiB on-chip ROM
//! at `0x6000_0000`. That ROM — not `start4.elf`, not the EEPROM bootloader — is
//! what actually *starts* the machine: it splits the two VPU cores, brings up
//! enough clocking to reach SPI, reads the `pieeprom.bin` bootloader image off
//! the SPI flash, **verifies the second-stage bootcode's signature**, stages it
//! into L2-as-SRAM, and only then hands control to it. Every later stage runs
//! because the ROM decided it should.
//!
//! The model cannot execute the ROM itself: the image is Broadcom's mask (not
//! redistributable) and its signing salt is a per-silicon secret. So this models
//! the ROM's *behaviour*, transcribed from a disassembly of the ROM on a
//! BCM2711C0. Like every other stage, though, it is a participant on the modelled
//! bus — it reads the image from the [`Spi0`](crate::periph::spi0) flash, reads
//! the key OTP rows through the [`configotp`](crate::periph::configotp) MMIO
//! transaction, and stages the bootcode into the machine's RAM. It does not reach
//! around the peripherals for the data the real ROM fetches through them.
//!
//! # What the ROM does with the bootcode, from the disassembly
//!
//! Reset vector `0x6000_0000` is `mov r0,256; version r0; btest r0,16; bne …` —
//! the classic VC4 core split (bit 16 of `version` is the core id); core 0 then
//! runs a fixed sequence of `bl`s and, if it ever returns, `Sleep`s at
//! `0x6000_008e`.
//!
//! The signature check is the routine at **`0x6000_0608`**. Reconstructed:
//!
//! 1. `r8 = image_base + length; r8 -= 20` (`0x6000_0648`, `0x6000_0650`) — the
//!    stored signature is the **last 20 bytes** of the image.
//! 2. `Lea r1,[pc+26550]` at `0x6000_0662` points at the 20-byte salt descriptor
//!    at `0x6000_6e18`, and the loop at `0x6000_066e..0x6000_067a` computes
//!    `key[i] = salt[i] ^ otp[i]` a word at a time (5 words = 20 bytes), the OTP
//!    words having been staged into the boot-info buffer at `r24+0xDC`.
//! 3. The call at `0x6000_0692` runs HMAC-SHA1 with that key over
//!    `image[0 .. length-20]` (`r2 = length-20` at `0x6000_068c`), writing the
//!    digest to the scratch buffer at `r24+0xC8`.
//! 4. The loop at `0x6000_069c..0x6000_06b8` compares the 20 computed bytes with
//!    the 20 stored bytes and returns 0 only when every byte matches.
//!
//! The SHA-1 core lives at `0x6000_675e` (the H0..H4 init constants) with the
//! round constants K0..K3 at `0x6000_652e`; it is a stock SHA-1, so we compute
//! the same digest with the `sha1`/`hmac` crates rather than re-deriving it. The
//! derivation `key = salt ^ otp`, message `= image[..-20]`, signature
//! `= image[-20..]` also reproduces the byte-exact HMAC in a stock signed
//! `pieeprom.bin` footer, which is the independent check that this reading is
//! right. It is the same scheme `raspberrypi/rpi-tools`' `signing-tool/sign.js`
//! documents (old-style HMAC checksum, enforced whenever OTP secure-boot is off).
//!
//! The OTP half of the key is the `r24+0xDC` buffer, which the ROM fills from OTP
//! rows 19..=22 (the board-identity fuses). We read those same rows through the
//! config/OTP block at `0x7E20_F000`, so the key follows the modelled fuses.
//!
//! # The salt is never in this repository
//!
//! The 20-byte salt at `0x6000_6e18` is a maskROM signing secret. It is **not**
//! embedded here and never should be. Signature checking is therefore off unless
//! the operator supplies the key at run time (see [`BootRom::from_env`]); without
//! it the model stages the bootcode unverified, exactly as before this stage
//! existed. The public half of the derivation — the board-identity OTP words — is
//! [`crate::periph::configotp::BOARD_IDENTITY`], which the model's OTP block
//! already serves.
//!
//! # The ROM's OTP helpers
//!
//! 2020-04-16 bootcode calls three ROM routines directly (#71); see
//! [`ROM_HELPERS`] for the stand-ins this stage places.

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use sha1::Sha1;

use crate::bus::Bus;
use crate::firmware::eeprom::{EepromImage, BOOTCODE_ENTRY_OFFSET, BOOTCODE_LOAD_ADDR};
use crate::firmware::write_folded;
use crate::machine::Machine;
use crate::periph::configotp::BOARD_IDENTITY;
use crate::spec::otp::{
    DATA as OTP_DATA, KEY as OTP_KEY, PARAM_A as OTP_PARAM_A, PARAM_A_GO_MASK as OTP_GO,
    STATUS as OTP_STATUS, STATUS_DONE_MASK as OTP_DONE,
};

/// Length of the HMAC-SHA1 signature the ROM appends and checks.
pub const HMAC_LEN: usize = 20;

/// The full signed-image footer, for reference: `length:u32`, `keyindex:u32`,
/// `rsa[256]`, `hmac_sha1[20]`. Only the trailing HMAC matters to the old-style
/// check the ROM enforces; the RSA block is verified only under secure boot.
pub const SIG_FOOTER_LEN: usize = 4 + 4 + 256 + HMAC_LEN;

/// The config/OTP block base (`0x7E20_F000`, `specs/otp.toml`), where the ROM
/// reads the board-identity fuses that form the OTP half of the HMAC key.
const OTP_BASE: u32 = 0x7E20_F000;

/// OTP rows the HMAC key is built from: rows 19..=22, the board-identity block.
const OTP_KEY_ROWS: std::ops::RangeInclusive<u32> = 19..=22;

/// The C0 ROM's OTP helpers that 2020-04-16 bootcode calls directly: through
/// its trampoline at `0x80001f68` (`version r2; eor r1, r2; bl r1`) it opens
/// the OTP block (`0x6000_647a`), reads row 28 into `*r0` (`0x6000_09d0`) and
/// closes the block again (`0x6000_1d50`, #71). Later bootcode reads OTP
/// itself. The model has no ROM at `0x6000_0000` — the address folds onto
/// DRAM — so the stage puts routines of its own there. They are not the ROM's
/// code: opening and closing only set the block's clock mux, which the model
/// absorbs, and the read skips the `STATUS` poll because the model's
/// transaction completes on `GO`.
const ROM_HELPERS: [(u32, &[u8]); 3] = [
    (0x6000_647A, &[0x5A, 0x00]), // b lr
    (0x6000_1D50, &[0x5A, 0x00]), // b lr
    (
        0x6000_09D0,
        &[
            0x01, 0xE8, 0x00, 0xF0, 0x20, 0x7E, // mov r1, 0x7E20F000
            0xC2, 0x61, // mov r2, 28
            0x12, 0x37, // st r2, (r1+0x1C)    KEY
            0x12, 0x60, // mov r2, 1
            0x12, 0x32, // st r2, (r1+0x08)    PARAM_A.GO
            0x12, 0x26, // ld r2, (r1+0x18)    DATA
            0x02, 0x09, // st r2, (r0)
            0x5A, 0x00, // b lr
        ],
    ),
];

// The row read above encodes these offsets.
const _: () = assert!(
    OTP_BASE == 0x7E20_F000
        && OTP_KEY == 0x1C
        && OTP_PARAM_A == 0x08
        && OTP_DATA == 0x18
        && OTP_GO == 1
);

/// The 20-byte OTP contribution to the HMAC key derived from a constant, for
/// tests and documentation: OTP rows 19..=22 written little-endian into a 20-byte
/// buffer, so the last four bytes stay zero. This is what the ROM builds in its
/// `r24+0xDC` buffer, and what `read_otp_key_words` reads back from the modelled
/// OTP block; the two must agree while the fuses hold [`BOARD_IDENTITY`].
pub fn otp_key_words() -> [u8; HMAC_LEN] {
    let mut otp = [0u8; HMAC_LEN];
    for (i, word) in BOARD_IDENTITY.iter().enumerate() {
        otp[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    otp
}

/// Read one OTP row through the config/OTP block, the way the EEPROM bootloader's
/// `getconfig` does: write the row number to `KEY`, start the transaction with
/// `PARAM_A.GO`, wait for `STATUS.DONE`, take the value from `DATA`.
fn read_otp_row(machine: &mut Machine, row: u32) -> u32 {
    let _ = machine.store32(OTP_BASE + OTP_KEY, row);
    let _ = machine.store32(OTP_BASE + OTP_PARAM_A, OTP_GO);
    // The transaction resolves on the GO write in the model; poll anyway, the way
    // the ROM does, so the read exercises the STATUS path.
    for _ in 0..8 {
        if machine.load32(OTP_BASE + OTP_STATUS).unwrap_or(0) & OTP_DONE != 0 {
            break;
        }
    }
    machine.load32(OTP_BASE + OTP_DATA).unwrap_or(0)
}

/// The OTP half of the HMAC key, read from the modelled fuses over MMIO: rows
/// 19..=22 written little-endian into a 20-byte buffer (last four bytes zero).
pub fn read_otp_key_words(machine: &mut Machine) -> [u8; HMAC_LEN] {
    let mut otp = [0u8; HMAC_LEN];
    for (i, row) in OTP_KEY_ROWS.enumerate() {
        otp[i * 4..i * 4 + 4].copy_from_slice(&read_otp_row(machine, row).to_le_bytes());
    }
    otp
}

/// `key[i] = salt[i] ^ otp[i]`, the ROM's `0x6000_066e` loop.
fn xor20(salt: &[u8; HMAC_LEN], otp: &[u8; HMAC_LEN]) -> [u8; HMAC_LEN] {
    let mut key = [0u8; HMAC_LEN];
    for i in 0..HMAC_LEN {
        key[i] = salt[i] ^ otp[i];
    }
    key
}

/// HMAC-SHA1 over `image[..-20]`, compared with the trailing 20 bytes.
fn hmac_ok(key: &[u8; HMAC_LEN], image: &[u8]) -> bool {
    if image.len() <= HMAC_LEN {
        return false;
    }
    let (message, signature) = image.split_at(image.len() - HMAC_LEN);
    let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC key length is unrestricted");
    mac.update(message);
    mac.verify_slice(signature).is_ok()
}

/// Outcome of the ROM's bootcode signature check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigCheck {
    /// No key was supplied, so the check is not modelled this run.
    Skipped,
    /// HMAC-SHA1 matched the footer — the ROM would proceed.
    Ok,
    /// HMAC-SHA1 mismatch — the real ROM halts here and the board stays silent.
    Failed,
}

/// What the ROM produced for the next stage: the entry it hands control to, and
/// a short human-readable log of the steps it took.
pub struct BootOutcome {
    pub entry: u32,
    pub log: Vec<String>,
}

/// The modelled boot ROM. Holds the (optional) signing secret; everything else
/// about the first stage is behaviour driven through the bus.
///
/// The HMAC key is either supplied whole (`key`) or derived at boot time from a
/// salt XORed with the OTP rows read out of the machine (`salt`). Both are
/// run-time secrets and never live in the repository.
#[derive(Debug, Clone, Default)]
pub struct BootRom {
    key: Option<[u8; HMAC_LEN]>,
    salt: Option<[u8; HMAC_LEN]>,
}

impl BootRom {
    /// A ROM with no signing key: it stages bootcode but does not check it, the
    /// same behaviour the model had before this stage existed.
    pub fn unkeyed() -> BootRom {
        BootRom::default()
    }

    /// A ROM with an explicit HMAC key (already `salt ^ otp`).
    pub fn with_key(key: [u8; HMAC_LEN]) -> BootRom {
        BootRom {
            key: Some(key),
            salt: None,
        }
    }

    /// A ROM with the maskROM salt; the key is `salt ^ otp`, with the OTP words
    /// read from the machine at boot time.
    pub fn with_salt(salt: [u8; HMAC_LEN]) -> BootRom {
        BootRom {
            key: None,
            salt: Some(salt),
        }
    }

    /// Build from the environment. The secret is a run-time value and never lives
    /// in the repository:
    ///
    /// * `RVF_BOOT_KEY=<40 hex>` — the 20-byte HMAC key directly.
    /// * `RVF_BOOT_SALT=<40 hex>` — the 20-byte maskROM salt; XORed with the OTP
    ///   rows the model serves to form the key.
    ///
    /// Neither set means an unkeyed ROM (signature check skipped). `RVF_BOOT_KEY`
    /// wins if both are set.
    pub fn from_env() -> Result<BootRom> {
        if let Some(key) = env_hex20("RVF_BOOT_KEY")? {
            return Ok(BootRom::with_key(key));
        }
        if let Some(salt) = env_hex20("RVF_BOOT_SALT")? {
            return Ok(BootRom::with_salt(salt));
        }
        Ok(BootRom::unkeyed())
    }

    /// Is the signature check armed (a secret is available)?
    pub fn checks_signature(&self) -> bool {
        self.key.is_some() || self.salt.is_some()
    }

    /// The effective HMAC key for this run, deriving it from the salt and the
    /// machine's OTP rows when only a salt was supplied. `None` = unkeyed.
    fn effective_key(&self, machine: &mut Machine) -> Option<[u8; HMAC_LEN]> {
        match (&self.key, &self.salt) {
            (Some(key), _) => Some(*key),
            (None, Some(salt)) => Some(xor20(salt, &read_otp_key_words(machine))),
            (None, None) => None,
        }
    }

    /// Model the ROM's first stage: read the `pieeprom.bin` image out of the SPI
    /// flash, locate the second-stage bootcode, verify its signature if a key is
    /// available (reading the OTP key rows over MMIO), stage it into RAM at
    /// [`BOOTCODE_LOAD_ADDR`], and return the entry hand-off (`+0x200`).
    ///
    /// Returns an error when the ROM would refuse to boot — no flash, a malformed
    /// image, or a signature mismatch with a key present (on hardware the ROM
    /// simply halts and the board is silent; the model says so instead of
    /// hanging).
    pub fn boot(&self, machine: &mut Machine) -> Result<BootOutcome> {
        // The image comes off the SPI-NOR flash the ROM reads, not a side channel.
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
                "boot ROM: signature check skipped (set RVF_BOOT_KEY or RVF_BOOT_SALT to enable)"
                    .into(),
            ),
            SigCheck::Failed => bail!(
                "boot ROM: bootcode HMAC-SHA1 signature mismatch — hardware would halt here, \
                 refusing to boot"
            ),
        }

        // Stage the second stage into L2-as-SRAM, then hand off at its entry.
        write_folded(machine, BOOTCODE_LOAD_ADDR, &body).context("boot ROM: staging bootcode")?;
        machine
            .l2
            .hold(BOOTCODE_LOAD_ADDR & 0x3FFF_FFFF, body.len());
        // Where 0x6000_0000 folds to (512 MiB in); a smaller RAM goes without.
        for (addr, code) in ROM_HELPERS {
            let _ = write_folded(machine, addr, code);
        }
        Ok(BootOutcome {
            entry: BOOTCODE_LOAD_ADDR + BOOTCODE_ENTRY_OFFSET,
            log,
        })
    }
}

/// Parse a `NAME=<hex>` environment variable into 20 bytes, tolerating a `0x`
/// prefix and surrounding whitespace. Absent variable → `Ok(None)`.
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

    /// RFC 2202 HMAC-SHA1 test case 1 — proves the crate wiring computes a
    /// standard HMAC-SHA1, the same primitive the ROM's SHA-1 core implements.
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

    /// Build a fake signed image for an arbitrary (non-secret) test key.
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

    /// The OTP read over MMIO returns the same words as the constant derivation
    /// while the fuses hold `BOARD_IDENTITY`.
    #[test]
    fn otp_read_over_mmio_matches_the_constant_words() {
        let mut machine = Machine::new(1 << 20);
        assert_eq!(read_otp_key_words(&mut machine), otp_key_words());
        // Last four bytes are zero (only rows 19..=22 populate 16 bytes).
        assert_eq!(&otp_key_words()[16..20], &[0, 0, 0, 0]);
    }

    /// The row-28 helper 2020-04-16 calls at `0x6000_09d0` (#71) does the OTP
    /// transaction and stores the row where `r0` points.
    #[test]
    fn the_rom_s_row_28_helper_stores_the_row() {
        use crate::vpu::{Step, Vpu};
        const CODE: u32 = 0x1000;
        const RET: u32 = 0x2000;
        const BUF: u32 = 0x3000;
        let mut machine = Machine::new(1 << 20);
        let (_, code) = ROM_HELPERS.iter().find(|(a, _)| *a == 0x6000_09D0).unwrap();
        write_folded(&mut machine, CODE, code).unwrap();
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

    /// Wrap a signed bootcode body in a minimal EEPROM image.
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

    /// End to end through the bus: the ROM reads the image from the SPI flash,
    /// reads the OTP key rows, derives `salt ^ otp`, verifies, and stages the
    /// bootcode at the load address with entry `+0x200`. A tampered image is
    /// refused; an unkeyed ROM stages without checking.
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
        // The staged bytes landed in RAM (0x8000_0000 folds to phys 0).
        assert_eq!(machine.ram.read_slice(0, 4).unwrap(), &[0xAB; 4]);

        // Tampered image: refuse to boot.
        let mut bad_body = body.clone();
        bad_body[0] ^= 1;
        let mut m2 = Machine::new(1 << 20);
        m2.spi0.attach_flash(eeprom_with_bootcode(&bad_body));
        assert!(rom.boot(&mut m2).is_err(), "bad signature must refuse");

        // Unkeyed ROM stages the good image without checking.
        let mut m3 = Machine::new(1 << 20);
        m3.spi0.attach_flash(image);
        assert!(BootRom::unkeyed().boot(&mut m3).is_ok());

        // No flash at all: refuse.
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
