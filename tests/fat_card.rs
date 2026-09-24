//! The card `pimu::fat` builds out of a directory, read back by something that
//! is not this model: `mtools`, which is also what `scripts/make-sd.sh` writes
//! the real card with. Skipped where `mtools` is not installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use pimu::fat::{card_from_dir, Card};
use pimu::periph::disk::BLOCK_SIZE;

/// The volume starts 1 MiB in, which is what `-i <image>@@<offset>` needs.
const PART_OFFSET: u64 = 2048 * BLOCK_SIZE as u64;

#[test]
fn mtools_reads_back_what_the_card_was_built_from() {
    let Some(mdir) = tool("mdir") else {
        eprintln!("mtools not installed; skipping");
        return;
    };
    let dir = tempdir("fat-card");
    // A plain 8.3 name, one too long for it, two that collide once shortened,
    // a file longer than a cluster, and a subdirectory: what a firmware
    // checkout has.
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

    // The contents, not just the entries: a file's clusters are the host
    // file's bytes, and the last one is short.
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

/// The whole card as a file: the metadata, then every file where its clusters
/// are. Sparse — the volume is padded out to the cluster count that makes it
/// FAT32, which is most of its size.
fn write_image(card: &Card, path: &Path) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(&card.meta).unwrap();
    for extent in &card.extents {
        f.seek(SeekFrom::Start(extent.lba * BLOCK_SIZE as u64))
            .unwrap();
        f.write_all(&std::fs::read(&extent.path).unwrap()).unwrap();
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
        // mtools warns about a boot sector it did not write itself otherwise.
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
