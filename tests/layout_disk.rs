//! The disk `pimu::layout` builds out of a list of partitions, read back with
//! the tools that make such disks for real: `sfdisk` for the GPT, `e2fsck` and
//! `debugfs` for the ext4 partition, `mtools` for the FAT32 one. Each is
//! skipped where it is not installed.

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use pimu::fat::{Card, Source};
use pimu::layout::PartitionsSpec;
use pimu::periph::disk::BLOCK_SIZE;

fn write_image(card: &Card, path: &Path) {
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(&card.meta).unwrap();
    for extent in &card.extents {
        f.seek(SeekFrom::Start(extent.lba * BLOCK_SIZE as u64))
            .unwrap();
        let bytes = match &extent.source {
            Source::Path(path) => std::fs::read(path).unwrap(),
            Source::Bytes(bytes) => bytes.clone(),
            Source::Url(url) => panic!("{url}: no network in a test"),
        };
        f.write_all(&bytes).unwrap();
    }
    f.set_len(card.blocks * BLOCK_SIZE as u64).unwrap();
}

fn tool(name: &str) -> Option<PathBuf> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("PATH=$PATH:/usr/sbin:/sbin command -v {name}"))
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
        "{} {args:?}: {}{}",
        tool.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pimu-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A partition's bytes, cut out of the disk into a file of their own.
fn cut(image: &Path, start: u64, sectors: u64, to: &Path) {
    use std::io::Read;
    let mut f = std::fs::File::open(image).unwrap();
    f.seek(SeekFrom::Start(start * BLOCK_SIZE as u64)).unwrap();
    let mut bytes = vec![0u8; sectors as usize * BLOCK_SIZE];
    f.read_exact(&mut bytes).unwrap();
    std::fs::write(to, bytes).unwrap();
}

const DISK: &str = "partitions:
  - type: esp
    format: vfat
    label: boot
    files:
      config.txt: { inline: \"arm_64bit=1\\n\" }
      start4.elf: big.bin
      overlays/x.dtbo: { inline: dtbo }
  - type: root
    format: ext4
    label: rootfs
    files:
      etc/hostname: { inline: \"pimu\\n\" }
      usr/bin/tool: tool.sh
      usr/share/big.bin: big.bin
      usr/share/empty: { inline: \"\" }
  - type: usr-verity
    size: 1M
";

fn disk(dir: &Path) -> (PathBuf, Card) {
    let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7 + i / 251) as u8).collect();
    std::fs::write(dir.join("big.bin"), &big).unwrap();
    let tool = dir.join("tool.sh");
    std::fs::write(&tool, b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let spec: PartitionsSpec = yaml_serde::from_str(DISK).unwrap();
    let card = spec.card(dir).unwrap();
    let image = dir.join("disk.img");
    write_image(&card, &image);
    (image, card)
}

#[test]
fn sfdisk_reads_the_gpt() {
    let Some(sfdisk) = tool("sfdisk") else {
        eprintln!("sfdisk not installed; skipping");
        return;
    };
    let dir = tempdir("layout-gpt");
    let (image, _) = disk(&dir);
    let out = run(&sfdisk, &["--json", image.to_str().unwrap()]);
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    let table = &json["partitiontable"];
    assert_eq!(table["label"], "gpt");
    let parts = table["partitions"].as_array().unwrap();
    let types: Vec<&str> = parts.iter().map(|p| p["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        [
            "C12A7328-F81F-11D2-BA4B-00A0C93EC93B",
            "B921B045-1DF0-41C3-AF44-4C6F280D3FAE",
            "6E11A4E7-FBCA-4DED-B9E9-E1A512BB664E",
        ]
    );
    assert_eq!(parts[0]["name"], "boot");
    assert_eq!(parts[1]["name"], "rootfs");
    let verify = run(&sfdisk, &["--verify", image.to_str().unwrap()]);
    assert!(verify.contains("No errors"), "{verify}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_type_guids_are_the_ones_sfdisk_names() {
    let Some(sfdisk) = tool("sfdisk") else {
        eprintln!("sfdisk not installed; skipping");
        return;
    };
    let types = run(&sfdisk, &["--list-types", "--label", "gpt"]);
    for (guid, name) in [
        ("C12A7328-F81F-11D2-BA4B-00A0C93EC93B", "EFI System"),
        ("B921B045-1DF0-41C3-AF44-4C6F280D3FAE", "root (ARM-64)"),
        ("B0E01050-EE5F-4390-949A-9101B17104E9", "/usr (ARM-64)"),
        (
            "6E11A4E7-FBCA-4DED-B9E9-E1A512BB664E",
            "/usr verity (ARM-64)",
        ),
        (
            "C23CE4FF-44BD-4B00-B2D4-B41B3419E02A",
            "/usr verity sign. (ARM-64)",
        ),
    ] {
        let line = types
            .lines()
            .find(|l| l.contains(guid))
            .unwrap_or_else(|| panic!("{guid} is no GPT type of sfdisk's"));
        assert!(line.contains(name), "{guid}: {line} is not {name}");
    }
}

#[test]
fn e2fsck_finds_the_ext4_partition_clean_and_debugfs_reads_its_files() {
    let (Some(e2fsck), Some(debugfs)) = (tool("e2fsck"), tool("debugfs")) else {
        eprintln!("e2fsprogs not installed; skipping");
        return;
    };
    let dir = tempdir("layout-ext4");
    let (image, card) = disk(&dir);
    let entries = &card.meta[2 * BLOCK_SIZE..];
    let start = u64::from_le_bytes(entries[128 + 32..128 + 40].try_into().unwrap());
    let end = u64::from_le_bytes(entries[128 + 40..128 + 48].try_into().unwrap());
    let part = dir.join("root.img");
    cut(&image, start, end - start + 1, &part);

    let fsck = Command::new(&e2fsck)
        .args(["-fn", part.to_str().unwrap()])
        .output()
        .unwrap();
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&fsck.stdout),
        String::from_utf8_lossy(&fsck.stderr)
    );
    assert!(fsck.status.success(), "e2fsck:\n{report}");

    let p = part.to_str().unwrap();
    assert_eq!(run(&debugfs, &["-R", "cat /etc/hostname", p]), "pimu\n");
    let big = Command::new(&debugfs)
        .args(["-R", "cat /usr/share/big.bin", p])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(big, std::fs::read(dir.join("big.bin")).unwrap());
    let stat = run(&debugfs, &["-R", "stat /usr/bin/tool", p]);
    assert!(stat.contains("Mode:  0755"), "{stat}");
    let ls = run(&debugfs, &["-R", "ls -l /", p]);
    assert!(ls.contains("lost+found"), "{ls}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn mtools_reads_the_vfat_partition_of_the_gpt_disk() {
    let Some(mdir) = tool("mdir") else {
        eprintln!("mtools not installed; skipping");
        return;
    };
    let mcopy = tool("mcopy").expect("mcopy beside mdir");
    let dir = tempdir("layout-vfat");
    let (image, _) = disk(&dir);
    let at = format!("{}@@{}", image.display(), 2048 * BLOCK_SIZE);
    let root = run(&mdir, &["-i", &at, "-b", "::/"]);
    for name in ["::/config.txt", "::/start4.elf", "::/overlays"] {
        assert!(root.contains(name), "{name} missing from\n{root}");
    }
    let out = dir.join("out");
    std::fs::create_dir(&out).unwrap();
    run(&mcopy, &["-i", &at, "::/start4.elf", out.to_str().unwrap()]);
    assert_eq!(
        std::fs::read(out.join("start4.elf")).unwrap(),
        std::fs::read(dir.join("big.bin")).unwrap()
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
