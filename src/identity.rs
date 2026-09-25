//! The board identity the EEPROM bootloader derives, and `start4` republishes.
//!
//! `/chosen/rpi-machine-id` is the string an image can turn into its root-LUKS
//! passphrase, so a firmware bump that moves it locks the disk out.
//!
//! `start4.elf` only copies it out of the EEPROM bootloader's `"BVER"` handoff
//! block. The derivation runs in `pieeprom.bin`'s first stage, traced rather
//! than guessed, and is
//!
//! ```text
//!   SHA-256( le32(otp[28]) ‖ le32(otp[35]) ‖ le32(otp[30])
//!            ‖ le32(otp[64]) ‖ le32(otp[65]) )[..16]
//! ```
//!
//! — the rows in the order the bootloader assembles them, not numeric order.
//! They are the public identity ones (board serial, revision code, Ethernet
//! MAC): **no secret row takes part**, neither the secure-boot key hash nor the
//! device private key, which is what makes the derivation safe to write down.
//!
//! So the passphrase depends on five public fuses and on this function staying
//! put across firmware versions, and [`expected_machine_id`] recomputes it
//! independently. That recomputation is a diagnostic, not the assertion: the
//! guard is the pinned output in `testdata/boot/firmware.toml`, since checking
//! the model against the firmware would be circular — both read the same fuses.
//! When the pinned value fails, the recomputation says whether the fuses or the
//! algorithm moved.

/// The OTP rows the derivation consumes, in the order they are hashed.
pub const MACHINE_ID_ROWS: [u32; 5] = [28, 35, 30, 64, 65];

/// Recompute `/chosen/rpi-machine-id` from [`MACHINE_ID_ROWS`], in that order.
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

/// Plain SHA-256: the only thing in the bench that hashes anything.
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

    /// The identity the invented OTP produces is the one `boot-check` pins on
    /// the firmware's output: the model predicts the value, not just observes.
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

    /// The value is a derivation, not a constant: one flipped bit of the serial
    /// changes every byte of the identity.
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
