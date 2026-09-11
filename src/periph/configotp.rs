//! The always-on config / OTP engine at `0x7E20_F000`.
//!
//! The EEPROM bootloader's `getconfig(key)` path reads board identity through
//! this block:
//!
//! ```text
//!   write key            -> +0x1C
//!   write 0, 0           -> +0x0C, +0x08     (transaction params)
//!   set  +0x08 |= 1                          (trigger)
//!   poll +0x10 bit 0                         (done)
//!   read value           <- +0x18
//! ```
//!
//! The same block also takes some clock-mux pokes at `+0x04` (values 3/0/2)
//! which we just absorb. Anything we don't recognise keeps the old "always
//! ready" status bits so unrelated pollers still make progress.
//!
//! The key written to `+0x1C` is the **OTP row number**, so [`ConfigOtp::table`]
//! is the fuse array itself: row -> value. It is not a full dump of a real
//! board's fuses and must never become one — OTP holds device-unique and secret
//! material (see `CLAUDE.md`). Only rows whose contents the boot actually
//! depends on are modelled; every other row reads back 0.
//!
//! Which rows those are is not a matter of taste. `arm_loader` will not start
//! the ARM until `FUN_0EC78F70` says the board is genuine, and that check reads
//! **rows 19..26** as two four-word blocks and compares them against an
//! obfuscated per-board-family constant in `.text` — first the block on its own,
//! then the other block, then the two OR-ed together. With those rows reading
//! back 0 no comparison can match, the check fails, and start4 blinks LED error
//! code 4-4 ("unsupported board type") in a loop for the rest of the boot
//! instead of reaching `arm_loader`.
//!
//! `vcgencmd otp_dump` is no help in seeding them: from Linux those rows read
//! back `0xFFFF_FFFF`, which cannot be their fused value because the firmware
//! would reject it — the VPU locks the block before handing over, so what
//! userspace sees is a redaction, not the contents. The value that does satisfy
//! the check is [`BOARD_IDENTITY`], taken from start4's own board-type table.
//!
//! The board serial (row 28, and its complement in row 29) is deliberately
//! **not** a real board's — it is an arbitrary fixed value, so nothing here
//! carries the identity of a specific piece of hardware. It only has to stay
//! stable across firmware versions for the `rpi-machine-id` regression to mean
//! something.
//!
//! It does feed that derivation, which was worth checking rather than assuming:
//! flipping row 28 by one bit changes every byte of the `rpi-machine-id`
//! `arm_loader` publishes (`2928640898f6b5035da98885da0ac498` ->
//! `075d24bb620ac951d7b20db02366b59b`). So the regression `scripts/boot-check.sh`
//! pins is a real derivation being re-run, not a constant being copied — and
//! changing the serial here invalidates that milestone, which is the point.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

const REG_CLKMUX: u32 = 0x04;
const REG_PARAM_A: u32 = 0x08;
const REG_STATUS: u32 = 0x10;
const REG_DATA: u32 = 0x18;
const REG_KEY: u32 = 0x1C;

/// `+0x08` bit 0 kicks off a transaction; `+0x10` **bit 1** reports completion
/// (the poll at `0x8000760e` is `btest [+0x10], #1`).
const GO: u32 = 1 << 0;
const DONE: u32 = 1 << 1;

/// Status bits unrelated firmware paths poll for on this block.
const READY: u32 = (1 << 17) | (1 << 18) | (1 << 7);

/// The board-identity block in OTP rows 19..22 (and again in 23..26).
///
/// `FUN_0EC78F70`, the check `arm_loader` gates the ARM launch on, compares
/// those rows against a per-board-family constant obfuscated with
/// `^ 0xB0BE_5AD5` in start4's `.text`. It picks the constant by board type —
/// `0x3EDE_DB18` for type 17, Pi 4 Model B, which is what OTP row 30's revision
/// code says this machine is — so these four words are the value a genuine
/// Pi 4 Model B must have fused. They come out of the firmware image itself,
/// not off any particular board, and identify the model rather than the unit.
///
/// The firmware accepts the value in either four-row block, or spread across
/// both and OR-ed together (the redundancy real fuses need); storing it whole in
/// both blocks satisfies every one of those comparisons.
const BOARD_IDENTITY: [u32; 4] = [0x8AA9_6D38, 0x9111_243F, 0x38E4_E488, 0x8E02_2082];

pub struct ConfigOtp {
    storage: BTreeMap<u32, u32>,
    /// Key latched via `+0x1C`, resolved on the next triggered transaction.
    key: u32,
    /// Result presented at `+0x18`.
    data: u32,
    done: bool,
    /// key -> config value.
    table: BTreeMap<u32, u32>,
}

impl Default for ConfigOtp {
    fn default() -> Self {
        ConfigOtp::new()
    }
}

impl ConfigOtp {
    pub fn new() -> ConfigOtp {
        let mut table = BTreeMap::new();
        // A complete but entirely fictitious board identity.
        //
        // Which rows are *programmed* mirrors a real Raspberry Pi 4 Model B —
        // 0..5, 16..30, 35, 64 and 65 carry data, the rest are blank. That
        // shape matters: firmware reading 0 from a row concludes the fuse is
        // unprogrammed and takes a different path, so leaving a populated row
        // at zero is the same class of bug as a stub ID register reading 0
        // (#13). The *values* are invented and deliberately look it — see the
        // OTP rule in CLAUDE.md for why a real board's must never be committed.
        //
        // Row meanings are from Raspberry Pi's own documentation,
        // `documentation/asciidoc/computers/raspberry-pi/otp-bits.adoc`.
        // Rows marked "not public" there get `0xFA1E_00rr` ("fake", row in the
        // low byte); rows whose bits *mean* something get a value chosen so the
        // modelled board behaves like the reference one. Filling the documented
        // control rows with a pattern is not harmless: `0xFA1E_0010` in row 16
        // sets bit 26 and the boot flips to "VC-JTAG locked".
        for row in (0..=5).chain(std::iter::once(27)) {
            table.insert(row, 0xFA1E_0000 | row);
        }
        // 19-26: the board-identity block `arm_loader` verifies (see the module
        // docs). Unlike its neighbours this one cannot be invented — the value
        // is the Pi 4 Model B family constant start4 itself carries, so it
        // identifies the model, not the unit. Stored in both four-row blocks so
        // all three of the firmware's comparisons agree.
        for (i, word) in BOARD_IDENTITY.iter().enumerate() {
            table.insert(19 + i as u32, *word);
            table.insert(23 + i as u32, *word);
        }
        // 16: OTP control. Bits 26 and 27 disable VC JTAG; both stay clear so
        // the boot reports "VC-JTAG unlocked" as the reference log does.
        table.insert(16, 0x0000_0001);
        // 17: bootmode, 18: its copy. `0x8B0` is what every Pi 4 reports —
        // it is in the reference logs already ("OTP boardrev d03115 bootrom
        // 8b0 8b0") and is a model constant, not board-unique. None of the
        // documented bits are set: not bit 15, which would disable ROM RSA
        // key 0 and turn secure boot on, and none of 19-22/28/29, which would
        // redirect the boot to GPIO, SD or USB. The bits it does set are in
        // the "not public" range. On BCM2711 the bootmode comes from the
        // EEPROM configuration anyway, not from OTP.
        table.insert(17, 0x0000_08B0);
        table.insert(18, 0x0000_08B0);
        // 28: serial number, 29: its bitwise complement — the firmware can
        // check one against the other, so they are generated as a pair.
        const SERIAL: u32 = 0x1AA2_BB31;
        table.insert(28, SERIAL);
        table.insert(29, !SERIAL);
        // 30: revision code. The one value taken from real hardware, because
        // the firmware decodes it into the board model it reports ("board:
        // boardrev d03115 otp d03115"). It identifies a model, not a board:
        // Raspberry Pi 4 Model B, 8 GB, rev 1.5.
        table.insert(30, 0x00D0_3115);
        // 35: high 32 bits of the 64-bit serial.
        table.insert(35, 0xFA1E_0023);
        // 64/65: Ethernet MAC `02:00:5E:00:53:01` — locally administered
        // (bit 1 of the first octet set) and unicast (bit 0 clear), from the
        // documentation range in RFC 7042 section 2.1.2. A tidier-looking
        // `01:02:03:04:05:06` would be wrong: bit 0 of `01` marks it
        // multicast, which is not a legal source address.
        //
        // The split — low four bytes in 64, high two in 65 — is inferred from
        // Raspberry Pi's tooling, not verified against this firmware: the boot
        // reads both rows but nothing modelled so far consumes them.
        // `arm_loader`'s `rpi-machine-id` derivation is the consumer that will
        // pin it down (#5), and this is the place to correct if it disagrees.
        table.insert(64, 0x5E00_5301);
        table.insert(65, 0x0000_0200);
        // 56-63: the 256-bit customer-private key. The reference board has
        // these *fused* — `vcgencmd otp_dump` prints them as `00000000`, but it
        // hides this region the same way it hides rows 19-26, and Linux's
        // `nvmem_priv0` reads back 32 non-zero bytes. Modelling them as blank
        // would therefore be modelling the wrong board: firmware that reads 0
        // from a row concludes the fuse is unprogrammed and can take a
        // different path.
        //
        // The value is **invented for this model** and must stay that way. The
        // real rows are the secret behind `rpi-machine-id` and the root LUKS
        // passphrase, which is what `CLAUDE.md`'s "never commit an OTP dump"
        // rule protects; a made-up key exercises the same firmware path. It is
        // a valid NIST P-256 scalar — non-zero and far below the group order,
        // whose top word is `0xFFFFFFFF` — and deliberately ASCII, so a
        // hexdump of these rows reads as obviously fake.
        //
        // Fusing them does not by itself make the crypto property tags answer:
        // `GET_CRYPTO_KEY_STATUS` still reports `KEY_NOT_FOUND`, on this model
        // and on the reference board alike. The key also has to be *registered*
        // in customer OTP (rows 36-43), which is blank on both.
        const DEVICE_PRIVATE_KEY: [u32; 8] = [
            0x5250_4956, // "RPIV"
            0x4952_5446, // "IRTF"
            0x574D_4F44, // "WMOD"
            0x454C_4B45, // "ELKE"
            0x5930_3030, // "Y000"
            0x3030_3030, // "0000"
            0x3030_3030, // "0000"
            0x3030_3031, // "0001"
        ];
        for (i, word) in DEVICE_PRIVATE_KEY.iter().enumerate() {
            table.insert(56 + i as u32, *word);
        }
        // Deliberately left blank, exactly as the reference board has them:
        // 36-43 customer OTP (`nvmem_cust0` reads back all zero), 45/46 the
        // MPG2 and WVC1 decode keys, 47-54 the SHA256 of the secure-boot RSA
        // public key, and 55 the secure-boot flags.
        ConfigOtp {
            storage: BTreeMap::new(),
            key: 0,
            data: 0,
            done: false,
            table,
        }
    }

    /// Override / extend the config table (e.g. from a scenario spec).
    pub fn set(&mut self, key: u32, value: u32) {
        self.table.insert(key, value);
    }

    /// The fused value of a row, as the firmware would read it. Unprogrammed
    /// rows read back 0, which is what the hardware does too.
    ///
    /// Used by [`crate::identity`] to recompute `/chosen/rpi-machine-id`
    /// independently of the firmware.
    pub fn row(&self, key: u32) -> u32 {
        self.table.get(&key).copied().unwrap_or(0)
    }

    fn resolve(&mut self) {
        self.data = self.table.get(&self.key).copied().unwrap_or(0);
        if std::env::var_os("RVF_DBG_OTP").is_some() {
            eprintln!(
                "[otp] key {} (0x{:x}) -> 0x{:08x}{}",
                self.key,
                self.key,
                self.data,
                if self.table.contains_key(&self.key) {
                    ""
                } else {
                    "  (UNMODELLED)"
                }
            );
        }
        self.done = true;
    }
}

impl MmioDevice for ConfigOtp {
    fn name(&self) -> &'static str {
        "config-otp"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        Ok(match offset & !3 {
            REG_STATUS => {
                if self.done {
                    DONE
                } else {
                    0
                }
            }
            REG_DATA => self.data,
            REG_KEY => self.key,
            off => self.storage.get(&off).copied().unwrap_or(READY),
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        match offset & !3 {
            REG_KEY => {
                self.key = value;
                self.done = false;
            }
            REG_PARAM_A => {
                self.storage.insert(REG_PARAM_A, value);
                if value & GO != 0 {
                    self.resolve();
                }
            }
            REG_STATUS => {
                // write-1-to-clear the done latch
                if value & DONE != 0 {
                    self.done = false;
                }
            }
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
