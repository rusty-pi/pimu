//! `boot --scenario --on <device>`: run a scenario on a real board instead of
//! the model, and judge what its UART prints by the scenario's milestones.
//!
//! Opt-in and never part of CI: the board, its serial line and the command that
//! restarts it are described in a file on the user's own machine
//! (`devices.yaml`). pimu knows no power switch; `power_cycle` is a command.

use std::io::{Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use pimu::harness::boot::MediumSpec;
use pimu::harness::BootScenario;
use pimu::soc::board::Board;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    /// `Revision` in `/proc/cpuinfo`, as a scenario's `board_rev` writes it.
    pub board_rev: String,
    /// `b0` or `c0`, for a scenario that names a `stepping`.
    #[serde(default)]
    pub stepping: Option<String>,
    /// The serial line the board's UART is on.
    pub serial: PathBuf,
    #[serde(default = "default_baud")]
    pub baud: u32,
    /// The directory the board's bootloader reads over TFTP.
    pub tftp_dir: PathBuf,
    /// The command (argv, no shell) that restarts the board into what is in
    /// `tftp_dir`: a smart plug, or `ssh <board> sudo reboot "0 tryboot"`.
    pub power_cycle: Vec<String>,
}

fn default_baud() -> u32 {
    115200
}

fn home_relative(p: &Path) -> PathBuf {
    match (p.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => p.to_path_buf(),
    }
}

/// `--devices`, or `$XDG_CONFIG_HOME/pimu/devices.yaml`.
fn devices_file(given: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = given {
        return Ok(p.to_path_buf());
    }
    Ok(std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("no XDG_CONFIG_HOME and no HOME to find devices.yaml in")?
        .join("pimu")
        .join("devices.yaml"))
}

pub fn load(given: Option<&Path>, name: &str) -> Result<Device> {
    let file = devices_file(given)?;
    let text = std::fs::read_to_string(&file)
        .with_context(|| format!("reading the device list {}", file.display()))?;
    let mut all: std::collections::BTreeMap<String, Device> = yaml_serde::from_str(&text)
        .with_context(|| format!("parsing the device list {}", file.display()))?;
    let Some(mut device) = all.remove(name) else {
        let known: Vec<&str> = all.keys().map(String::as_str).collect();
        bail!(
            "{}: no device `{name}` (it lists: {})",
            file.display(),
            known.join(", ")
        );
    };
    if device.power_cycle.is_empty() {
        bail!("{name}: `power_cycle` is empty: it is the command that restarts the board");
    }
    device.serial = home_relative(&device.serial);
    device.tftp_dir = home_relative(&device.tftp_dir);
    Ok(device)
}

/// Why `scn` cannot run on `device`, one reason each. Nothing here is a failed
/// check: the scenario asks for something a real board cannot be given.
pub fn unsuitable(scn: &BootScenario, device: &Device) -> Vec<String> {
    let b = &scn.boot;
    let mut why = Vec::new();
    if !b.otp_row.is_empty() {
        why.push("`otp_row` fuses OTP, and fuses survive a reset on a real board".into());
    }
    if !b.mbox_property.is_empty() {
        why.push("`mbox_property` asks the firmware through a mailbox only the model has".into());
    }
    if b.eeprom_pubkey.is_some() {
        why.push("`eeprom_pubkey` needs the bootloader EEPROM rewritten".into());
    }
    if !b.input.is_empty() || b.magic_packet_after.is_some() {
        why.push("`input` and `magic_packet_after` are not carried to a real board yet".into());
    }
    if b.sd.is_some() || b.usb.is_some() || b.otg.is_some() || b.http.is_some() {
        why.push("only a `tftp` medium can be staged on a real board".into());
    } else if b.tftp.is_none() {
        why.push("it has no `tftp` medium to stage".into());
    }
    if let Some(MediumSpec::Path(p)) = &b.tftp {
        if pimu::remote::is_url(p) {
            why.push("a `tftp` URL is not staged: list the files or name a directory".into());
        }
    }
    if let Some(want) = &b.board_rev {
        match (
            Board::parse_revision(want),
            Board::parse_revision(&device.board_rev),
        ) {
            (Ok(w), Ok(d)) if w == d => {}
            _ => why.push(format!(
                "it wants board_rev {want}, the device is {}",
                device.board_rev
            )),
        }
    }
    if let Some(want) = &b.stepping {
        if device.stepping.as_deref().map(str::to_lowercase) != Some(want.to_lowercase()) {
            why.push(format!(
                "it wants stepping {want}, the device says {}",
                device.stepping.as_deref().unwrap_or("nothing")
            ));
        }
    }
    why
}

/// Copy what the scenario serves into the device's TFTP directory. Files are
/// overwritten and nothing is deleted: that directory is the user's.
pub fn stage(scn: &BootScenario, device: &Device, work: &Path) -> Result<usize> {
    let mut staged = scn.clone();
    staged.stage(work)?;
    let Some(MediumSpec::Path(root)) = &staged.boot.tftp else {
        bail!("{}: no `tftp` medium to stage", scn.name);
    };
    let from = staged.base_dir.join(root);
    copy_tree(&from, &device.tftp_dir)
}

fn copy_tree(from: &Path, to: &Path) -> Result<usize> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    let mut copied = 0;
    for entry in std::fs::read_dir(from).with_context(|| format!("listing {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copied += copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).with_context(|| {
                format!("copying {} to {}", entry.path().display(), target.display())
            })?;
            copied += 1;
        }
    }
    Ok(copied)
}

fn configure_line(device: &Device) -> Result<()> {
    if !std::fs::metadata(&device.serial)
        .with_context(|| format!("opening {}", device.serial.display()))?
        .file_type()
        .is_char_device()
    {
        return Ok(());
    }
    let status = Command::new("stty")
        .arg("-F")
        .arg(&device.serial)
        .args([
            &device.baud.to_string(),
            "raw",
            "-echo",
            "min",
            "0",
            "time",
            "1",
        ])
        .status()
        .context("running stty to set the serial line")?;
    if !status.success() {
        bail!("stty could not set {}: {status}", device.serial.display());
    }
    Ok(())
}

/// Restart the board and write its UART to `log` until `until` prints or the
/// wall budget is spent. The line is open before the restart, so the first
/// bytes of the boot are not lost.
pub fn capture(
    device: &Device,
    until: Option<&str>,
    wall: Duration,
    log: &Path,
) -> Result<Vec<u8>> {
    configure_line(device)?;
    let mut line = std::fs::File::open(&device.serial)
        .with_context(|| format!("opening {}", device.serial.display()))?;
    // Whatever the line held before the restart is not this boot's.
    if std::fs::metadata(&device.serial)?
        .file_type()
        .is_char_device()
    {
        let mut stale = [0u8; 4096];
        while line.read(&mut stale)? > 0 {}
    }
    let (program, args) = device
        .power_cycle
        .split_first()
        .context("`power_cycle` is empty")?;
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running power_cycle `{program}`"))?;
    if !status.success() {
        bail!("power_cycle `{program}` failed: {status}");
    }

    let mut file =
        std::fs::File::create(log).with_context(|| format!("creating {}", log.display()))?;
    let started = Instant::now();
    let mut seen = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut out = std::io::stdout();
    while started.elapsed() < wall {
        let n = line.read(&mut chunk).context("reading the serial line")?;
        if n == 0 {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        file.write_all(&chunk[..n])?;
        out.write_all(&chunk[..n])?;
        out.flush()?;
        seen.extend_from_slice(&chunk[..n]);
        if until.is_some_and(|u| String::from_utf8_lossy(&seen).contains(u)) {
            break;
        }
    }
    Ok(seen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario(boot: &str) -> BootScenario {
        yaml_serde::from_str(&format!(
            "name: x\nboot:\n  eeprom: e\n  wall_secs: 1\n  tftp: t\n{boot}"
        ))
        .unwrap()
    }

    fn device(extra: &str) -> Device {
        yaml_serde::from_str(&format!(
            "board_rev: d03115\nserial: s\ntftp_dir: t\npower_cycle: [true]\n{extra}"
        ))
        .unwrap()
    }

    #[test]
    fn a_scenario_that_fuses_otp_is_refused() {
        let why = unsuitable(&scenario("  otp_row: [\"56=0x1\"]\n"), &device(""));
        assert_eq!(why.len(), 1, "{why:?}");
        assert!(why[0].contains("fuses"), "{why:?}");
    }

    #[test]
    fn the_board_has_to_be_the_one_the_scenario_names() {
        let scn = scenario("  board_rev: c03112\n  stepping: b0\n");
        assert_eq!(unsuitable(&scn, &device("")).len(), 2);
        let b0 = device("stepping: B0\n").board_rev_as("c03112");
        assert!(unsuitable(&scn, &b0).is_empty());
    }

    #[test]
    fn a_scenario_that_boots_from_a_card_has_nothing_to_stage() {
        let why = unsuitable(&scenario("  sd: card.img\n"), &device(""));
        assert!(why.iter().any(|w| w.contains("only a `tftp`")), "{why:?}");
    }

    impl Device {
        fn board_rev_as(mut self, rev: &str) -> Device {
            self.board_rev = rev.into();
            self
        }
    }
}
