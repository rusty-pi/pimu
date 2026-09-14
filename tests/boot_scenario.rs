//! The firmware-boot regression, minus the boot.
//!
//! `testdata/boot/firmware-boot.toml` describes a run that takes minutes and
//! needs firmware blobs that are never committed, so `cargo test` cannot boot
//! it — `scripts/boot-check.sh` does that, and CI runs it in its own job. What
//! is testable here is everything around the run, and it is the part that has
//! silently rotted before: that the scenario still parses, that every milestone
//! carries the reason it exists, and — the point of the whole exercise — that a
//! transcript which has actually changed is *rejected*. A regression guard
//! nobody has seen fail is not yet a guard.

use std::path::{Path, PathBuf};

use rpi_virt_fw::harness::boot::{self, BootScenario, GoldenCheck};

fn scenario_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/boot/firmware-boot.toml")
}

fn scenario() -> BootScenario {
    BootScenario::load(&scenario_path()).expect("load the boot scenario")
}

/// A stand-in for the combined run log: the console the golden recorded, plus
/// the parts of the evidence that never reach a UART and only exist in the
/// `boot` report. Spelling them out here is the point — it documents which
/// milestones are *not* provable from the transcript alone.
fn fake_log(console: &str) -> String {
    format!(
        "{console}\n\
         --- sdram controller (0x7e00_1000) ---\n\
         \x20 refresh interval 658 -> 1562 -> 3124  (7 mode-register reads)\n\
         --- device tree handed to the ARM ---\n\
         \x20 /chosen/rpi-serial64           \"fa1e00231aa2bb31\"\n\
         \x20 /chosen/rpi-machine-id         \"ed96a9bc626d9d0869ce37ee4aea025d\"\n\
         --- rpi-machine-id derivation (#22) ---\n\
         \x20 ed96a9bc626d9d0869ce37ee4aea025d  matches the value the firmware published\n\
         --- ARM property mailbox (0x7e00_b880) ---\n\
         \x20 mailbox: config1 0x1, 1 requests taken, 1 replies written\n\
         \x20 tag 0x00000001     answered    4 bytes  0x6a7a16af\n\
         \x20 tag 0x00028001     answered    8 bytes  0x00000003 0x00000001\n\
         \x20 tag 0x0003008f     answered    4 bytes  0x00000001\n\
         \x20 tag 0x00030090     answered    4 bytes  0x00000001\n\
         \x20 tag 0x0003009c     answered    4 bytes  0x00000000\n\
         \x20 tag 0x00030092     answered   40 bytes  0x00000000 0x00000020 0x6ff9b60e 0x7bb3973a 0x01eb65b0 0x48fd764f 0x8286f48d 0xf1b46aed 0xe8e0da4c 0x9d1e6ea2\n\
         retired 1234  (skipped 0, cycles 5678)\n"
    )
}

/// The fixture has to be a passing run, or none of the failure tests below
/// prove anything: they all work by breaking exactly one thing in it.
#[test]
fn the_fixture_passes_every_assertion() {
    let scn = scenario();
    let golden = std::fs::read_to_string(scn.golden_path()).expect("read golden");
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
        // `absent` and a count are contradictory; catch the typo here rather
        // than having it silently weaken the check.
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

/// The golden is recorded through the normaliser, so feeding it back through
/// must be a no-op. If it is not, the golden holds something that moves from
/// run to run and the check could never be stable.
#[test]
fn the_recorded_golden_is_already_normalised() {
    let scn = scenario();
    let golden = std::fs::read(scn.golden_path()).expect("the golden transcript is committed");
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

/// The golden must actually contain the boot it claims to, all the way to the
/// hand-off. This is what stops `--update` from quietly recording a truncated
/// run as the new truth.
#[test]
fn the_recorded_golden_reaches_the_arm_handover() {
    let scn = scenario();
    let golden = std::fs::read_to_string(scn.golden_path()).expect("read golden");
    for needle in [
        "PM_RSTS 00000020",
        "*** Restart logging",
        "Loaded 'kernel8.img'",
        "arm_loader: Starting ARM with 948MB",
    ] {
        assert!(golden.contains(needle), "golden is missing {needle:?}");
    }
    // Nothing the milestones forbid may be sitting in the golden either.
    for m in scn.milestones.iter().filter(|m| m.absent) {
        assert!(
            m.check(&golden).is_none(),
            "the golden contains a line the scenario forbids: {}",
            m.line
        );
    }
}

/// The guard, demonstrated: a transcript that differs by one line is rejected,
/// and the diff points at that line instead of at everything after it.
#[test]
fn a_changed_transcript_fails_the_golden_check() {
    let scn = scenario();
    let golden = std::fs::read_to_string(scn.golden_path()).expect("read golden");

    // A value that shifted: the kind of change every grep in the old bash
    // check would have missed, because the line still matches.
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
            // Not a cascade: one line changed, one line reported either way.
            assert_eq!(
                diff.lines().filter(|l| l.starts_with("  -")).count(),
                1,
                "a one-line change produced a cascading diff:\n{diff}"
            );
        }
        GoldenCheck::Match => panic!("a changed transcript matched the golden"),
        GoldenCheck::Missing => panic!("no golden to compare against"),
    }

    // Output that merely *moved* must fail too — that is the whole reason the
    // golden exists next to the milestones.
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

/// And the other half: the milestones still fail on the condition they were
/// written for, not merely on a diff.
#[test]
fn a_milestone_fails_when_its_invariant_breaks() {
    let scn = scenario();
    let golden = std::fs::read_to_string(scn.golden_path()).expect("read golden");

    // A log with the hand-off line removed: the milestone that names it must
    // fail, and it must be the one that names it.
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

    // A derail anywhere in the log is fatal even though the console still
    // reaches the hand-off — the `[derail]` marker never appears on the UART.
    let derailed = format!("{}[derail] pc=0xfffffdda\n", fake_log(&golden));
    let hits: Vec<String> = scn
        .milestones
        .iter()
        .filter_map(|m| m.check(&derailed))
        .collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("PRESENT"), "{}", hits[0]);
}

/// The run report is part of the evidence: a run that never printed its
/// counters, or that skipped instructions, is not a pass.
#[test]
fn the_skipped_instruction_guard_still_bites() {
    let scn = scenario();
    let golden = std::fs::read_to_string(scn.golden_path()).expect("read golden");

    let skipped = fake_log(&golden).replace("(skipped 0,", "(skipped 7,");
    let f = boot::check_run(&scn, &skipped, &golden).expect("check");
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].contains("skipped instructions is 7"), "{}", f[0]);

    // A run that stopped before printing its counters proves nothing below it.
    let no_report: String = fake_log(&golden)
        .lines()
        .filter(|l| !l.starts_with("retired "))
        .map(|l| format!("{l}\n"))
        .collect();
    let f = boot::check_run(&scn, &no_report, &golden).expect("check");
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].contains("could not read the skipped"), "{}", f[0]);
}

/// Every boot medium has its own scenario (`testdata/boot/*.toml`), and each
/// one's run plan attaches exactly the media it names.
#[test]
fn every_boot_scenario_loads_and_plans_its_media() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/boot");
    let mut seen = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "toml") {
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
        seen.push(scn.name);
    }
    seen.sort();
    for name in ["firmware-boot", "tftp-boot", "usb-boot"] {
        assert!(seen.iter().any(|s| s == name), "{name} missing: {seen:?}");
    }
}
