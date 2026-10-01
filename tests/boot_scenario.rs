//! The firmware regression, minus the boot itself: `pimu boot-check` runs that,
//! in its own CI job. What is testable here is everything around the run — that
//! the scenario parses, that every milestone says why it exists, and that a
//! changed transcript is actually *rejected*.

use std::path::{Path, PathBuf};

use pimu::harness::boot::{self, BootScenario, GoldenCheck, RetiredCounts};

fn scenario_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/boot/firmware.yaml")
}

fn scenario() -> BootScenario {
    BootScenario::load(&scenario_path()).expect("load the boot scenario")
}

/// A stand-in for the combined run log: the golden console plus the evidence
/// that never reaches a UART and the transcript alone cannot prove.
fn fake_log(console: &str) -> String {
    format!(
        "{console}\n\
         --- sdram controller (0x7e00_1000) ---\n\
         \x20 refresh interval 658 -> 1562 -> 3124  (7 mode-register reads)\n\
         --- device tree handed to the ARM ---\n\
         \x20 /chosen/rpi-serial64           \"fa1e00231aa2bb31\"\n\
         --- ARM property mailbox (0x7e00_b880) ---\n\
         \x20 mailbox: config1 0x1, 1 requests taken, 1 replies written\n\
         \x20 tag 0x00000001     answered    4 bytes  0x6a7a16af\n\
         \x20 tag 0x00028001     answered    8 bytes  0x00000003 0x00000001\n\
         \x20 tag 0x0003008f     answered    4 bytes  0x00000001\n\
         \x20 tag 0x00030090     answered    4 bytes  0x00000001\n\
         \x20 tag 0x0003009c     answered    4 bytes  0x00000000\n\
         \x20 tag 0x00030092     answered   40 bytes  0x00000000 0x00000020 0x6ff9b60e 0x7bb3973a 0x01eb65b0 0x48fd764f 0x8286f48d 0xf1b46aed 0xe8e0da4c 0x9d1e6ea2\n\
         {}",
        report(&scenario())
    )
}

fn report(scn: &BootScenario) -> String {
    let text = std::fs::read_to_string(scn.retired_path().unwrap())
        .expect("the retired counts are committed");
    let counts = RetiredCounts::parse(&text).expect("the retired counts parse");
    let mut out = format!(
        "end        Stuck {{ pc: 0x3ec40014, silent_us: 60001165, retired: 68775692 }}\n\
         retired    {}  (skipped 0, cycles 5678)\n",
        counts.get("vpu0").expect("VPU core 0 is pinned")
    );
    if let Some(n) = counts.get("vpu1") {
        out.push_str(&format!(
            "core1      pc 0x3ec40014  retired {n}  end None\n"
        ));
    }
    out.push_str("\n--- ARM cores ---\n");
    for i in 0..4 {
        if let Some(n) = counts.get(&format!("arm{i}")) {
            out.push_str(&format!(
                "  core {i}    still in the armstub\n            \
                 {n} instructions, 0 exceptions, 0 interrupts; now pc 0x80  EL2  sp 0x0  (wfi)\n"
            ));
        }
    }
    out
}

/// The fixture must pass, or the tests below, each breaking one thing in it, do
/// not prove anything.
#[test]
fn the_fixture_passes_every_assertion() {
    let scn = scenario();
    let golden = std::fs::read_to_string(scn.golden_path().unwrap()).expect("read golden");
    let failures = boot::check_run(&scn, &fake_log(&golden), &golden).expect("check");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_boot_scenario_parses_and_every_milestone_says_why() {
    let scn = scenario();
    assert!(
        scn.milestones.len() >= 30,
        "the boot scenario has only {} milestones; the bash check it replaced had ~30",
        scn.milestones.len()
    );
    for m in &scn.milestones {
        assert!(
            m.why.trim().len() > 20,
            "milestone {} has no usable reason: {:?}",
            m.line,
            m.why
        );
        assert!(
            !m.line.to_string().is_empty(),
            "milestone {} has an empty pattern",
            m.line
        );
        assert!(
            !(m.absent && (m.count.is_some() || m.max_count.is_some())),
            "milestone {} is both absent and counted",
            m.line
        );
    }
    assert_eq!(
        scn.boot.max_skipped, 0,
        "a clean boot skips no instructions"
    );
}

#[test]
fn the_run_plan_is_the_only_place_the_workload_is_written_down() {
    let scn = scenario();
    let args = scn.boot_args(Path::new("/tmp/console.bin"));
    let joined = args.join(" ");
    assert!(joined.starts_with("boot "), "{joined}");
    assert!(joined.contains("pieeprom.bin"), "{joined}");
    assert!(joined.contains("--eeprom"), "{joined}");
    assert!(joined.contains("--sd "), "{joined}");
    assert!(joined.contains("--max-wall 330"), "{joined}");
    assert!(
        joined.contains("--console-log /tmp/console.bin"),
        "{joined}"
    );
}

/// Feeding the golden back through the normaliser must be a no-op, or it holds
/// something that moves from run to run.
#[test]
fn the_recorded_golden_is_already_normalised() {
    let scn = scenario();
    let golden =
        std::fs::read(scn.golden_path().unwrap()).expect("the golden transcript is committed");
    let again = boot::normalise_console(&golden);
    assert_eq!(
        String::from_utf8_lossy(&golden),
        again,
        "the committed golden is not stable under normalisation"
    );
    assert!(
        again.lines().count() > 100,
        "the golden is only {} lines; that is not a boot",
        again.lines().count()
    );
}

/// The golden reaches the hand-off, so `--update` cannot record a truncated run.
#[test]
fn the_recorded_golden_reaches_the_arm_handover() {
    let scn = scenario();
    let golden = std::fs::read_to_string(scn.golden_path().unwrap()).expect("read golden");
    for needle in [
        "PM_RSTS 00000020",
        "*** Restart logging",
        "Loaded 'kernel8.img'",
        "arm_loader: Starting ARM with 948MB",
    ] {
        assert!(golden.contains(needle), "golden is missing {needle:?}");
    }
    for m in scn.milestones.iter().filter(|m| m.absent) {
        assert!(
            m.check(&golden).is_none(),
            "the golden contains a line the scenario forbids: {}",
            m.line
        );
    }
}

/// A transcript differing by one line is rejected, and the diff points at it.
#[test]
fn a_changed_transcript_fails_the_golden_check() {
    let scn = scenario();
    let golden = std::fs::read_to_string(scn.golden_path().unwrap()).expect("read golden");

    let broken = golden.replacen("948MB", "947MB", 1);
    assert_ne!(broken, golden, "the golden should mention the memory split");
    match boot::check_golden(&scn, &broken).expect("golden check") {
        GoldenCheck::Mismatch(diff) => {
            assert!(
                diff.contains("947MB"),
                "diff does not show the change:\n{diff}"
            );
            assert!(
                diff.contains("948MB"),
                "diff does not show the change:\n{diff}"
            );
            assert_eq!(
                diff.lines().filter(|l| l.starts_with("  -")).count(),
                1,
                "a one-line change produced a cascading diff:\n{diff}"
            );
        }
        GoldenCheck::Match => panic!("a changed transcript matched the golden"),
        GoldenCheck::Missing => panic!("no golden to compare against"),
    }

    let mut lines: Vec<&str> = golden.lines().collect();
    let i = lines
        .iter()
        .position(|l| l.contains("Watchdog stopped"))
        .expect("the golden stops the watchdog");
    let moved = lines.remove(i);
    lines.insert(0, moved);
    let reordered = format!("{}\n", lines.join("\n"));
    assert!(
        matches!(
            boot::check_golden(&scn, &reordered).expect("golden check"),
            GoldenCheck::Mismatch(_)
        ),
        "reordered output matched the golden"
    );
}

/// Milestones still fail on their own condition, not merely on a diff.
#[test]
fn a_milestone_fails_when_its_invariant_breaks() {
    let scn = scenario();
    let golden = std::fs::read_to_string(scn.golden_path().unwrap()).expect("read golden");

    let broken: String = fake_log(&golden)
        .lines()
        .filter(|l| !l.contains("arm_loader: Starting ARM"))
        .map(|l| format!("{l}\n"))
        .collect();
    let failures: Vec<String> = scn
        .milestones
        .iter()
        .filter_map(|m| m.check(&broken))
        .collect();
    assert_eq!(
        failures.len(),
        1,
        "expected exactly the hand-off milestone to fail, got:\n{}",
        failures.join("\n")
    );
    assert!(failures[0].contains("MISSING"), "{}", failures[0]);
    assert!(
        failures[0].contains("hands the board over to the ARM"),
        "the failure does not say what it proves:\n{}",
        failures[0]
    );

    // A derail is fatal even when the console reaches the hand-off.
    let derailed = format!("{}[derail] pc=0xfffffdda\n", fake_log(&golden));
    let hits: Vec<String> = scn
        .milestones
        .iter()
        .filter_map(|m| m.check(&derailed))
        .collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("PRESENT"), "{}", hits[0]);
}

/// A run that never printed its counters, or skipped instructions, is no pass.
#[test]
fn the_skipped_instruction_guard_still_bites() {
    let scn = scenario();
    let golden = std::fs::read_to_string(scn.golden_path().unwrap()).expect("read golden");

    let skipped = fake_log(&golden).replace("(skipped 0,", "(skipped 7,");
    let f = boot::check_run(&scn, &skipped, &golden).expect("check");
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].contains("skipped instructions is 7"), "{}", f[0]);

    let no_report: String = fake_log(&golden)
        .lines()
        .filter(|l| !l.starts_with("retired "))
        .map(|l| format!("{l}\n"))
        .collect();
    let f = boot::check_run(&scn, &no_report, &golden).expect("check");
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].contains("could not read the skipped"), "{}", f[0]);
}

/// Passing console and milestones are not enough: the retired counts are pinned.
#[test]
fn a_changed_retired_count_fails_the_check() {
    let scn = scenario();
    let golden = std::fs::read_to_string(scn.golden_path().unwrap()).expect("read golden");
    let log = fake_log(&golden);
    let vpu0 = RetiredCounts::from_log(&log)
        .and_then(|c| c.get("vpu0"))
        .expect("the fixture has the report");

    let moved = log.replace(
        &format!("retired    {vpu0} "),
        &format!("retired    {} ", vpu0 + 1),
    );
    let f = boot::check_run(&scn, &moved, &golden).expect("check");
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].starts_with("RETIRED:"), "{}", f[0]);
    assert!(f[0].contains("vpu0 ") && f[0].contains("(+1)"), "{}", f[0]);

    let (head, _) = log
        .split_once("\n--- ARM cores ---\n")
        .expect("the fixture releases the ARM");
    let f = boot::check_run(&scn, &format!("{head}\n"), &golden).expect("check");
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].contains("arm0  76 -> none"), "{}", f[0]);

    let dir = std::env::temp_dir().join(format!("pimu-retired-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut unpinned = scenario();
    let copy = dir.join("firmware.txt");
    std::fs::copy(scn.golden_path().unwrap(), &copy).unwrap();
    unpinned.golden.as_mut().unwrap().path = copy.display().to_string();
    let f = boot::check_run(&unpinned, &log, &golden).expect("check");
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].contains("MISSING: no retired counts"), "{}", f[0]);
}

/// Each boot medium's scenario (`testdata/boot/*.yaml`) attaches exactly the
/// media it names, and pins what its cores retired.
#[test]
fn every_boot_scenario_loads_and_plans_its_media() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/boot");
    let mut seen = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "yaml") {
            continue;
        }
        let scn = BootScenario::load(&path).expect("scenario parses");
        assert!(
            scn.milestones.iter().all(|m| !m.why.trim().is_empty()),
            "{}: every milestone says why",
            path.display()
        );
        let joined = scn.boot_args(Path::new("/tmp/c")).join(" ");
        for (flag, media) in [
            ("--sd ", &scn.boot.sd),
            ("--usb ", &scn.boot.usb),
            ("--otg ", &scn.boot.otg),
            ("--netboot ", &scn.boot.netboot),
        ] {
            assert_eq!(joined.contains(flag), media.is_some(), "{joined}");
        }
        if let Some(order) = &scn.boot.boot_order {
            assert!(
                joined.contains(&format!("--boot-order {order}")),
                "{joined}"
            );
        }
        for (flag, value) in [
            ("--stepping", &scn.boot.stepping),
            ("--board-rev", &scn.boot.board_rev),
        ] {
            assert_eq!(joined.contains(flag), value.is_some(), "{joined}");
            if let Some(v) = value {
                assert!(joined.contains(&format!("{flag} {v}")), "{joined}");
            }
        }
        let media = [
            &scn.boot.sd,
            &scn.boot.usb,
            &scn.boot.otg,
            &scn.boot.netboot,
            &scn.boot.eeprom_pubkey,
        ];
        assert_eq!(
            scn.inputs().len(),
            1 + media.iter().filter(|m| m.is_some()).count(),
            "{}",
            path.display()
        );
        let text = std::fs::read_to_string(scn.retired_path().unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", scn.retired_path().unwrap().display()));
        let counts = RetiredCounts::parse(&text).expect("the retired counts parse");
        assert!(counts.get("vpu0").is_some(), "{}", path.display());
        seen.push(scn.name);
    }
    seen.sort();
    for name in [
        "b0-stepping",
        "firmware-cd",
        "firmware",
        "tftp-boot",
        "usb-boot",
    ] {
        assert!(seen.iter().any(|s| s == name), "{name} missing: {seen:?}");
    }
}

/// A fresh checkout has no boot media, so `boot-check --plan` refuses, naming each
/// missing file and the command that makes it, rather than booting without a card.
#[test]
fn missing_boot_media_are_named_with_the_command_that_makes_them() {
    let root = std::env::temp_dir().join(format!("pimu-boot-inputs-{}", std::process::id()));
    let mut scn = scenario();
    scn.base_dir = root.join("testdata/boot");
    std::fs::create_dir_all(&scn.base_dir).unwrap();
    std::fs::create_dir_all(root.join("firmware")).unwrap();

    let missing = scn.missing_inputs();
    let names: Vec<String> = missing
        .iter()
        .map(|i| i.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["pieeprom.bin", "sd-halt.img"]);
    assert_eq!(missing[0].make, "scripts/fetch-firmware.sh");
    let make_sd = &missing[1].make;
    assert!(
        make_sd.starts_with("KERNEL=halt scripts/make-sd.sh /"),
        "{make_sd}"
    );
    assert!(
        make_sd.ends_with("/firmware/sd-halt.img") && !make_sd.contains(".."),
        "{make_sd}"
    );

    for i in &missing {
        std::fs::write(&i.path, b"").unwrap();
    }
    assert!(scn.missing_inputs().is_empty());
    std::fs::remove_dir_all(&root).unwrap();
}

/// A card's name says how it is built; built wrong, it boots something else.
#[test]
fn a_cards_name_decides_the_environment_that_builds_it() {
    for (img, prefix) in [
        ("../../firmware/sd.img", ""),
        ("../../firmware/sd-halt.img", "KERNEL=halt "),
        ("../../firmware/sd-wireless.img", "WIRELESS=1 "),
        ("../../firmware/sd-brcmfmac.img", "BRCMFMAC=1 "),
        (
            "../../firmware/sd-halt-start4cd.img",
            "START4=start4cd KERNEL=halt ",
        ),
    ] {
        let mut scn = scenario();
        scn.boot.sd = Some(img.into());
        let make = &scn.inputs()[1].make;
        assert!(
            make.starts_with(&format!("{prefix}scripts/make-sd.sh")),
            "{img}: {make}"
        );
    }
}
