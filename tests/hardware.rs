//! `boot --scenario --on`: a scenario run on a real board. The board here is a
//! plain file standing in for its serial line, and the power cycle is a shell
//! command that appends the boot to it, so nothing needs hardware.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCENARIO: &str = r#"
name: net
boot:
  eeprom: missing-eeprom.bin
  tftp:
    files:
      start4.elf: { inline: "elf" }
      config.txt: { inline: "uart_2ndstage=1" }
  wall_secs: 5
  until: "Starting ARM"
milestones:
  - why: "The bootloader went to the network."
    line: "Boot mode: NETWORK"
  - why: "The model's invented MAC."
    line: "NETWORK: 02:00:5e:00:53:01"
    model_only: true
  - why: "Nothing derailed."
    line: "[derail]"
    absent: true
"#;

fn setup(name: &str, uart: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("hardware-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("net.yaml"), SCENARIO).unwrap();
    std::fs::write(dir.join("uart"), "").unwrap();
    std::fs::write(dir.join("boot.txt"), uart).unwrap();
    std::fs::write(
        dir.join("devices.yaml"),
        format!(
            "pi:\n  board_rev: d03115\n  serial: {0}/uart\n  tftp_dir: {0}/tftp\n  \
             power_cycle: [\"sh\", \"-c\", \"cat {0}/boot.txt >> {0}/uart\"]\n",
            dir.display()
        ),
    )
    .unwrap();
    dir
}

fn pimu(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pimu"))
        .current_dir(dir)
        .arg("boot")
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn a_board_that_prints_the_milestones_passes_and_gets_the_files() {
    let dir = setup(
        "pass",
        "Boot mode: NETWORK (02)\r\narm_loader: Starting ARM\r\n",
    );
    let o = pimu(
        &dir,
        &[
            "--scenario",
            "net.yaml",
            "--on",
            "pi",
            "--devices",
            "devices.yaml",
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("board check passed"), "{}", text(&o));
    assert_eq!(std::fs::read(dir.join("tftp/start4.elf")).unwrap(), b"elf");
    assert!(dir.join("boot-net.log").exists());
}

#[test]
fn a_missing_milestone_fails_the_run() {
    let dir = setup("fail", "Boot mode: SD (01)\r\narm_loader: Starting ARM\r\n");
    let o = pimu(
        &dir,
        &[
            "--scenario",
            "net.yaml",
            "--on",
            "pi",
            "--devices",
            "devices.yaml",
        ],
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("MISSING"), "{}", text(&o));
}

#[test]
fn a_scenario_for_another_board_is_skipped_and_not_run() {
    let dir = setup("skip", "");
    let scn = SCENARIO.replace("boot:\n", "boot:\n  board_rev: c03112\n");
    std::fs::write(dir.join("net.yaml"), scn).unwrap();
    let o = pimu(
        &dir,
        &[
            "--scenario",
            "net.yaml",
            "--on",
            "pi",
            "--devices",
            "devices.yaml",
        ],
    );
    assert!(!o.status.success());
    assert!(text(&o).contains("wants board_rev c03112"), "{}", text(&o));
    assert!(!dir.join("tftp").exists());
}

#[test]
fn a_capture_is_checked_without_booting() {
    let dir = setup("capture", "");
    std::fs::write(
        dir.join("uart.log"),
        "Boot mode: NETWORK (02)\r\narm_loader: Starting ARM\r\n",
    )
    .unwrap();
    let o = pimu(
        &dir,
        &["--scenario", "net.yaml", "--from-capture", "uart.log"],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("1 model-only left out"), "{}", text(&o));
}
