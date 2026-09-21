//! The board identity the EEPROM bootloader derives, and `start4` republishes.
//!
//! `/chosen/rpi-machine-id` is the string a `rpi-mkosi` image turns into its
//! root-LUKS passphrase (rpi-mkosi#37), so what produces it is the whole point
//! of this bench. It is *not* derived by `start4.elf`: `0x3ECC5190` looks up the
//! EEPROM bootloader's `"BVER"` handoff block and, when it is present, simply
//! does `memcpy(out, BVER + 0x8c, 16)` — everything after that is hex encoding.
//! (`start4` does carry its own fallback for a board with no such block, and
//! that one is a different function: `SHA-256(otp[28] ‖ otp[35] ‖ otp[30])`.
//! On this bench it is never reached.)
//!
//! The real derivation runs in `pieeprom.bin`'s first stage, at `0x80008030`,
//! and was traced rather than guessed (#22):
//!
//! ```text
//!   0x80008030  r0 = 28 ; bl otp_read  -> [gp+156]
//!   0x8000803c  r0 = 64 ; bl otp_read  -> [gp+864]
//!   0x8000804a  r0 = 65 ; bl otp_read  -> [gp+868]
//!   0x80008074  r0 = 35 ; bl otp_read  -> [sp+0]
//!   ...          five memcpy(4) calls assemble a 20-byte buffer on the stack:
//!                  row 28, row 35, row 30, row 64, row 65
//!   0x800080bc  bl sha256(buf, 20, digest)
//!   0x800080ca  memcpy(BVER + 0x8c, digest, 16)
//! ```
//!
//! So the identity is
//!
//! ```text
//!   SHA-256( le32(otp[28]) ‖ le32(otp[35]) ‖ le32(otp[30])
//!            ‖ le32(otp[64]) ‖ le32(otp[65]) )[..16]
//! ```
//!
//! Each row goes in as a native little-endian 32-bit word, in that order — the
//! order the bootloader assembles them, not the numeric row order — and the
//! 32-byte digest is truncated to its first 16 bytes. The rows are the
//! documented public identity ones: 28 and 35 are the low and high halves of the
//! 64-bit board serial, 30 is the revision code, and 64/65 hold the Ethernet
//! MAC. **No secret row takes part**: neither the secure-boot key hash (47-54)
//! nor the device private key (56-63) is read anywhere on this path, which is
//! what makes the derivation safe to write down (see the OTP rule in
//! `CLAUDE.md`).
//!
//! That last point is the useful one for rpi-mkosi#37. The passphrase depends on
//! five public fuses and on this function staying put across firmware versions;
//! [`expected_machine_id`] recomputes it independently of the firmware so a
//! `boot` run can say whether the value the firmware published is still the one
//! the algorithm predicts, instead of only noticing after the fact that it
//! moved.
//! # This is a diagnostic, not an assertion
//!
//! Nothing regresses against this reimplementation, and nothing should. The
//! guard that matters is the **pinned output**: given the fixed OTP rows in
//! `src/periph/configotp.rs`, `/chosen/rpi-machine-id` must be exactly
//! `ed96a9bc626d9d0869ce37ee4aea025d`, which `testdata/boot/firmware.toml`
//! asserts directly against the transcript.
//!
//! Checking "our recomputation agrees with the firmware" instead would be
//! circular, and worse than useless: it recomputes from the *same* fuses, so a
//! change to those fuses moves the published id and the check still passes —
//! masking exactly the event the bench exists to catch (rpi-mkosi#37). A
//! firmware that changed its algorithm would fail this file's tests rather than
//! the boot, which is the wrong place to find out.
//!
//! What it is for: when the pinned value does fail, this says *why* — which
//! inputs went in and what they hash to — so "the fuses changed" is
//! distinguishable from "the algorithm changed" by reading the report.

/// The OTP rows the derivation consumes, in the order the bootloader feeds them
/// to SHA-256.
pub const MACHINE_ID_ROWS: [u32; 5] = [28, 35, 30, 64, 65];

/// Recompute `/chosen/rpi-machine-id` from the board's OTP rows.
///
/// `rows` are the values of [`MACHINE_ID_ROWS`], in that order.
pub fn expected_machine_id(rows: &[u32; 5]) -> [u8; 16] {
    let mut buf = [0u8; 20];
    for (i, row) in rows.iter().enumerate() {
        buf[i * 4..i * 4 + 4].copy_from_slice(&row.to_le_bytes());
    }
    let digest = sha256(&buf);
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// The same, hex encoded the way `start4` writes it into `/chosen`.
pub fn expected_machine_id_hex(rows: &[u32; 5]) -> String {
    expected_machine_id(rows)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Plain SHA-256. Small enough to carry rather than take a dependency for, and
/// this is the only thing in the bench that hashes anything.
fn sha256(msg: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut padded = msg.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&((msg.len() as u64) * 8).to_be_bytes());

    for block in padded.as_chunks::<64>().0 {
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

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (dst, src) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *dst = dst.wrapping_add(src);
        }
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha256_matches_the_published_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Two blocks, to exercise the message schedule across a chunk boundary.
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// The identity this bench's invented OTP produces, and the one
    /// `boot-check` pins on the firmware's own output. The two
    /// agreeing is the whole claim of #22: the model can predict the value
    /// rather than only observe it.
    #[test]
    fn machine_id_matches_the_modelled_board() {
        let rows = [
            0x1AA2_BB31,
            0xFA1E_0023,
            0x00D0_3115,
            0x5301_0000,
            0x0200_5E00,
        ];
        assert_eq!(
            expected_machine_id_hex(&rows),
            "ed96a9bc626d9d0869ce37ee4aea025d"
        );
    }

    /// The avalanche that showed the value is a derivation and not a constant:
    /// one bit of the serial changes every byte of the identity.
    #[test]
    fn one_serial_bit_changes_the_whole_identity() {
        let rows = [
            0x1AA2_BB31,
            0xFA1E_0023,
            0x00D0_3115,
            0x5301_0000,
            0x0200_5E00,
        ];
        let mut flipped = rows;
        flipped[0] = 0x1AA2_BB32;
        assert_eq!(
            expected_machine_id_hex(&flipped),
            "4118472de608104951e495ece64b75f0"
        );
    }
}
