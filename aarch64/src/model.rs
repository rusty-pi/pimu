//! Running the VPU model inside the image — stage 3 of #32.
//!
//! Same crate, same models, same run loop as the hosted `recon`; only the four
//! things `src/` leaves to a frontend are different, and they are all set up
//! in `main.rs` before this is called:
//!
//!  1. a heap over real physical RAM (`heap.rs`),
//!  2. a microsecond clock, so `max_wall` is not a no-op (`clock.rs`),
//!  3. the diagnostic and console sinks, pointed at the PL011 (`main.rs`),
//!  4. a `DiagConfig`, since `from_env` is hosted-only and there is no
//!     environment to read.
//!
//! What it does *not* do is assert a golden transcript. `cargo test` and
//! `scripts/boot-check.sh` stay hosted — they are the primary test path and
//! nothing about stage 3 changes that. The image checks an ordered list of
//! console landmarks instead, which is enough to answer the one question this
//! stage asks: does the firmware get as far here as it does there?

use crate::bundle::{Bundle, BundleBlocks};
use crate::uart::Uart;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt::Write;
use core::time::Duration;
use rpi_virt_fw::diag::DiagConfig;
use rpi_virt_fw::emulator::{Emulator, RunEnd, RunLimits};
use rpi_virt_fw::firmware::Payload;
use rpi_virt_fw::vpu::UnimplPolicy;
use rpi_virt_fw::Machine;

/// Blob names inside the `-initrd` bundle, i.e. the bare-metal spelling of
/// `recon firmware/pieeprom.bin --eeprom --sd firmware/sd.img`.
pub const EEPROM_BLOB: &str = "pieeprom.bin";
pub const SD_BLOB: &str = "sd.img";

/// RAM handed to the model, in MiB.
///
/// 1 GiB, and not a round number picked for comfort: `Machine::fold_ram_addr`
/// masks every RAM address with `0x3FFF_FFFF` to collapse the four VC4 cache
/// aliases (`0x0`, `0x4000_0000`, `0x8000_0000`, `0xC000_0000`) onto one
/// backing store, so 1 GiB *is* the whole of the address space the model can
/// reach. The hosted `recon --eeprom` default of 2048 MiB is slack above that,
/// and slack is what this image does not have: `raspi4b` is fixed at 2 GiB
/// total (`-m 1G` → "Invalid RAM size, should be 2 GiB") and the heap window
/// is 1.5 GiB of it.
///
/// Checked, not reasoned about: a hosted `recon --ram-mb 1024` run of the boot
/// scenario reaches `arm_loader: Starting ARM with 948MB` exactly as the
/// 2048 MiB default does.
const RAM_MB: usize = 1024;

/// Console landmarks, in the order a healthy boot produces them.
///
/// Deliberately few and far apart. The hosted scenario's 46 milestones and its
/// 141-line golden transcript are the real assertions and they stay hosted;
/// what this list is for is locating a bare-metal run on the same path — and,
/// when the run falls short, saying where it stopped instead of leaving a wall
/// of UART output to read.
const MILESTONES: &[(&str, &str)] = &[
    ("PM_RSTS 00000020", "EEPROM bootloader running"),
    ("PCIe scan 00001106:00003483", "PCIe up, VL805 found"),
    ("MESS:", "start4.elf loaded and logging"),
    ("*** Restart logging", "config.txt parsed, second-stage log"),
    (
        "arm_loader: Starting ARM with 948MB",
        "ARM hand-off — the whole boot",
    ),
];

/// How far a run got.
pub struct Outcome {
    /// Index into [`MILESTONES`] of the last one seen, `None` for none at all.
    pub reached: Option<usize>,
    /// Every milestone was seen, in order.
    pub complete: bool,
}

/// Build the machine the bundle describes and run it to a stop condition.
///
/// Mirrors `cmd_recon`'s `--eeprom` path in `src/main.rs`, reboot loop
/// included: a `pieeprom.upd` image self-updates its own flash and then asks
/// the SoC to reset, and the caller is expected to re-run from the updated
/// bytes rather than treat the reset as the end.
///
/// Known limit: each reboot builds a fresh `Machine`, and the previous one's
/// 1 GiB of RAM is freed but not reclaimed — the bump allocator only gives
/// back the most recent block (`heap.rs`), and by then it is buried. One
/// reboot fits in the 1.5 GiB window, a second does not. That is enough for an
/// already-provisioned `pieeprom.bin`, which is what this is run with and
/// which never resets; a `pieeprom.upd` boot would exhaust the heap and say
/// so, rather than fail quietly. Reclaiming it needs an allocator that
/// coalesces, or a `Machine` that can be reset in place.
pub fn run(bundle: &Bundle<'static>, con: &mut Uart) -> Outcome {
    let Some(eeprom) = bundle.get(EEPROM_BLOB) else {
        con.puts("FAILED: no ");
        con.puts(EEPROM_BLOB);
        con.puts(" in the -initrd bundle\n");
        return Outcome {
            reached: None,
            complete: false,
        };
    };
    let sd = bundle.get(SD_BLOB);

    // The wall budget is the only stop condition a healthy boot reaches, so it
    // has to outlast the interpreter running under TCG rather than natively.
    // `main.rs` derives it from how long QEMU is willing to let us run.
    let limits = RunLimits {
        max_steps: None,
        max_wall: Some(Duration::from_secs(crate::run_budget_secs())),
        stop_pc: None,
        idle_spin_limit: 200_000,
        // Same 60 s of *modelled* silence the hosted scenario uses: the
        // model's worst legitimate gap is the thirteen-second kernel load, so
        // this reports a wedge in seconds instead of burning the wall clock.
        silent_us: 60_000_000,
    };

    // `flash` is the EEPROM's working copy: a self-update rewrites it, and the
    // reboot below has to start from the rewritten bytes.
    let mut flash: Vec<u8> = eeprom.bytes().to_vec();
    let mut reboots = 0u32;
    let mut seen: usize = 0;

    loop {
        let payload = match Payload::from_eeprom_bytes(&flash) {
            Ok(p) => p,
            Err(e) => {
                let _ = writeln!(con, "FAILED: {EEPROM_BLOB}: {e}");
                return Outcome {
                    reached: None,
                    complete: false,
                };
            }
        };
        let mut machine = Machine::new(RAM_MB * 1024 * 1024);
        machine.spi0.attach_flash(flash.clone());
        if let Some(sd) = &sd {
            machine
                .emmc2
                .insert_card_medium(Box::new(BundleBlocks::new(sd.bytes())));
        }
        if let Err(e) = payload.load_into(&mut machine) {
            let _ = writeln!(con, "FAILED: loading the bootcode: {e}");
            return Outcome {
                reached: None,
                complete: false,
            };
        }
        let entry = payload.entry();

        let mut emu = Emulator::new(machine, entry);
        // The same policy `recon` runs with: an instruction the decoder does
        // not know is a fault, not a silent skip, because silently not doing
        // what the firmware asked is the failure mode this project exists to
        // avoid.
        emu.set_unimpl_policy(UnimplPolicy::ReconFault);
        // `from_env` is hosted-only and there is nothing to read here anyway.
        // `quiet()` leaves the live console on, which is what streams the
        // firmware's UART bytes out of ours as they are produced.
        emu.diag = Some(DiagConfig::quiet());

        let _ = writeln!(
            con,
            "\n--- boot {} : entry {:#010x}, {} MiB model RAM, wall {} s ---",
            reboots + 1,
            entry,
            RAM_MB,
            crate::run_budget_secs(),
        );
        let report = emu.run(&limits);

        seen = seen.max(milestones_in(&report.console));
        let _ = writeln!(
            con,
            "\n--- end {:?}  pc {:#010x}  retired {}  skipped {}  console {} bytes ---",
            report.end,
            report.pc,
            report.retired,
            report.skipped,
            report.console.len(),
        );

        if report.end == RunEnd::Reset && reboots < 4 {
            reboots += 1;
            // The self-update wrote through `spi0`; carry those bytes into the
            // next boot the way `cmd_recon` does, or the firmware provisions
            // itself again on every reboot and never gets past it.
            flash = emu.machine.spi0.flash_bytes().to_vec();
            let _ = writeln!(con, "=== RESET (reboot {reboots}) — re-running ===");
            continue;
        }

        return Outcome {
            reached: seen.checked_sub(1),
            complete: seen == MILESTONES.len(),
        };
    }
}

/// How many leading [`MILESTONES`] the console contains, in order.
fn milestones_in(console: &[u8]) -> usize {
    let mut from = 0usize;
    let mut n = 0usize;
    for (needle, _) in MILESTONES {
        match find(&console[from..], needle.as_bytes()) {
            Some(at) => {
                from += at + needle.len();
                n += 1;
            }
            None => break,
        }
    }
    n
}

/// Substring search. `[u8]` has no `find`, and pulling in a crate for a needle
/// that is never more than thirty bytes would be the wrong trade.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// Print the milestone list with what was reached ticked off.
pub fn report(outcome: &Outcome, con: &mut Uart) {
    let _ = writeln!(con, "\nmilestones:");
    for (i, (needle, why)) in MILESTONES.iter().enumerate() {
        let hit = outcome.reached.is_some_and(|r| i <= r);
        let _ = writeln!(
            con,
            "  [{}] {why}\n      \"{needle}\"",
            if hit { "ok" } else { "--" }
        );
    }
}
