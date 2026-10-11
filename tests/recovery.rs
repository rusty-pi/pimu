//! The EEPROM image on an SD card is flashed by `recovery.bin`, which the boot
//! ROM runs from the card in preference to the bootcode in the flash. The run is
//! silent, so the flash is what says it worked: all of `pieeprom.upd` but its
//! last 8 bytes, which become the `ts:` of `pieeprom.sig` as a little-endian
//! word and the word's complement before it. Skipped without the blobs
//! `scripts/fetch-firmware.sh` fetches.

use std::path::{Path, PathBuf};
use std::process::Command;

const CARD_FILES: [&str; 3] = ["recovery.bin", "pieeprom.upd", "pieeprom.sig"];

fn firmware(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("firmware")
        .join(name)
}

#[test]
fn recovery_bin_flashes_the_update_image_and_stamps_its_timestamp_at_the_end() {
    let blobs = [CARD_FILES.as_slice(), &["pieeprom.bin"]].concat();
    if let Some(missing) = blobs.iter().find(|n| !firmware(n).exists()) {
        eprintln!("firmware/{missing} not fetched; skipping");
        return;
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("recovery-card");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in CARD_FILES {
        std::fs::copy(firmware(name), dir.join(name)).unwrap();
    }
    let flashed = dir.join("flash-after.bin");

    // The firmware keeps resetting into recovery.bin, which is still on the
    // card, so the run ends in failure; the flash is what is looked at.
    Command::new(env!("CARGO_BIN_EXE_pimu"))
        .arg("boot")
        .arg("--eeprom")
        .arg(firmware("pieeprom.bin"))
        .arg("--sd")
        .arg(&dir)
        .args(["--max-wall", "120", "-q", "--dump-eeprom"])
        .arg(&flashed)
        .output()
        .expect("run pimu boot");

    let after = std::fs::read(&flashed).expect("the run wrote the flash");
    let image = std::fs::read(dir.join("pieeprom.upd")).unwrap();
    assert_eq!(after.len(), image.len());
    let trailer = image.len() - 8;
    assert!(
        after[..trailer] == image[..trailer],
        "the flash differs from pieeprom.upd before its last 8 bytes"
    );
    let sig = std::fs::read_to_string(dir.join("pieeprom.sig")).unwrap();
    let ts: u32 = sig
        .lines()
        .find_map(|l| l.strip_prefix("ts:"))
        .expect("pieeprom.sig has a ts line")
        .trim()
        .parse()
        .unwrap();
    assert_eq!(after[trailer..trailer + 4], (!ts).to_le_bytes());
    assert_eq!(after[trailer + 4..], ts.to_le_bytes());
}
