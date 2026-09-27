//! The card `pimu::fat` builds out of a directory, read back with `mtools` —
//! what `scripts/make-sd.sh` writes the real card with. Skipped without it.

use std::path::{Path, PathBuf};
use std::process::Command;

use pimu::fat::{card_from_dir, Card, Source};
use pimu::periph::disk::BLOCK_SIZE;

const PART_OFFSET: u64 = 2048 * BLOCK_SIZE as u64;

#[test]
fn mtools_reads_back_what_the_card_was_built_from() {
    let Some(mdir) = tool("mdir") else {
        eprintln!("mtools not installed; skipping");
        return;
    };
    let dir = tempdir("fat-card");
    // 8.3, too long, two colliding once shortened, one over a cluster, a dir.
    write(&dir.join("start4.elf"), &vec![0xa5; 40 * 1024]);
    write(&dir.join("bcm2711-rpi-4-b.dtb"), b"dtb");
    write(&dir.join("config.txt"), b"arm_64bit=1\n");
    write(&dir.join("empty.txt"), b"");
    std::fs::create_dir(dir.join("overlays")).unwrap();
    write(&dir.join("overlays/vc4-kms-v3d.dtbo"), b"one");
    write(&dir.join("overlays/vc4-kms-v3d-pi4.dtbo"), b"two");

    let image = dir.join("card.img");
    write_image(&card_from_dir(&dir).unwrap(), &image);

    let root = run(&mdir, &["-i", &at(&image), "-b", "::/"]);
    for name in [
        "::/start4.elf",
        "::/bcm2711-rpi-4-b.dtb",
        "::/config.txt",
        "::/overlays",
        "::/empty.txt",
    ] {
        assert!(root.contains(name), "{name} missing from\n{root}");
    }
    let overlays = run(&mdir, &["-i", &at(&image), "-b", "::/overlays"]);
    for name in ["vc4-kms-v3d.dtbo", "vc4-kms-v3d-pi4.dtbo"] {
        assert!(overlays.contains(name), "{name} missing from\n{overlays}");
    }

    let mcopy = tool("mcopy").expect("mcopy beside mdir");
    let out = dir.join("out");
    std::fs::create_dir(&out).unwrap();
    run(
        &mcopy,
        &[
            "-i",
            &at(&image),
            "::/start4.elf",
            "::/overlays/vc4-kms-v3d-pi4.dtbo",
            out.to_str().unwrap(),
        ],
    );
    assert_eq!(
        std::fs::read(out.join("start4.elf")).unwrap(),
        vec![0xa5; 40 * 1024]
    );
    assert_eq!(
        std::fs::read(out.join("vc4-kms-v3d-pi4.dtbo")).unwrap(),
        b"two"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The whole card as a sparse file, padded to the cluster count FAT32 needs.
fn write_image(card: &Card, path: &Path) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(&card.meta).unwrap();
    for extent in &card.extents {
        f.seek(SeekFrom::Start(extent.lba * BLOCK_SIZE as u64))
            .unwrap();
        let Source::Path(path) = &extent.source else {
            panic!("a card out of a directory maps host files")
        };
        f.write_all(&std::fs::read(path).unwrap()).unwrap();
    }
    f.set_len(card.blocks * BLOCK_SIZE as u64).unwrap();
}

fn at(image: &Path) -> String {
    format!("{}@@{PART_OFFSET}", image.display())
}

fn tool(name: &str) -> Option<PathBuf> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name}"))
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn run(tool: &Path, args: &[&str]) -> String {
    let out = Command::new(tool)
        .args(args)
        .env("MTOOLS_SKIP_CHECK", "1")
        .output()
        .unwrap_or_else(|e| panic!("running {}: {e}", tool.display()));
    assert!(
        out.status.success(),
        "{} {args:?}: {}",
        tool.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pimu-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
