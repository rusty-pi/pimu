//! What a boot needs apart from the machine itself, shared by the command line
//! and the browser build: the edits made to the EEPROM image and the card's
//! files, finding the device tree the firmware handed to the ARM, and judging
//! whether the run got where it was meant to.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

use crate::armstub::Handoff;
use crate::machine::Machine;
use crate::periph::fxl6408::Fxl6408;
use crate::periph::sdcard::Kept;

/// What a boot leaves for the one after a reset: the flash as the self-update
/// wrote it, the fuses programmed, the pins of the expander, the PM partition
/// bits, the DRAM's contents and the card with what the guest wrote to it.
/// The command line and the browser build both carry a machine over a reset
/// with this, so that they keep the same things.
#[derive(Default)]
pub struct Carry {
    pub flash: Vec<u8>,
    pub fuses: Option<BTreeMap<u32, u32>>,
    partition: u32,
    expander: Option<Fxl6408>,
    ram: Option<crate::mem::Contents>,
    card: Option<Kept>,
}

impl Carry {
    /// What the first boot starts from: the flash, and the fuses a run before
    /// left, if any.
    pub fn first(flash: Vec<u8>, fuses: Option<BTreeMap<u32, u32>>) -> Carry {
        Carry { flash, fuses, ..Carry::default() }
    }

    /// What `machine` leaves: `flash` is its flash with `edits` made again,
    /// which the self-update undid.
    pub fn leave(machine: &mut Machine, edits: &FlashEdits) -> Carry {
        let mut flash = machine.spi0.flash_bytes().to_vec();
        edits.apply(&mut flash, false);
        Carry {
            flash,
            fuses: Some(machine.config_otp.fuses().clone()),
            partition: machine.pm.partition_bits(),
            expander: machine.bsc_pmic.expander().cloned(),
            ram: Some(machine.ram.take_contents()),
            card: machine.emmc2.keep_card(),
        }
    }

    /// The pins of the expander as the reset left them, which tell whether the
    /// card had power across it.
    pub fn expander(&self) -> Option<&Fxl6408> {
        self.expander.as_ref()
    }

    /// Give a machine built for the next boot what the last one left. The
    /// fuses are written first and `otp_rows` over them, so a row the run was
    /// told to fuse stays fused whatever an earlier boot programmed. The card
    /// built with the machine is replaced by the one the last boot used, if it
    /// had one.
    pub fn restore(&mut self, machine: &mut Machine, otp_rows: &[(u32, u32)]) {
        if let Some(ram) = self.ram.take() {
            machine.ram.restore_contents(ram);
        }
        if let Some(fuses) = &self.fuses {
            machine.config_otp.fuse_rows(fuses);
        }
        for &(row, value) in otp_rows {
            machine.config_otp.set(row, value);
        }
        machine.pm.keep_partition_bits(self.partition);
        if let Some(expander) = self.expander.take() {
            machine.bsc_pmic.fit_expander(expander);
        }
        if let Some(card) = self.card.take() {
            machine.emmc2.restore_card(card);
        }
    }
}

/// What the command line adds to a card built out of files: `config.txt` lines
/// and a `cmdline.txt`. The files themselves are left alone — the edited ones
/// are held in memory, so a read-only directory or a URL takes them too.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct CardEdits {
    pub config: Vec<String>,
    pub cmdline: Option<String>,
}

impl CardEdits {
    pub fn is_empty(&self) -> bool {
        self.config.is_empty() && self.cmdline.is_none()
    }

    pub fn apply(&self, entries: &mut Vec<crate::fat::Entry>) -> Result<()> {
        if !self.config.is_empty() {
            let mut text = match read_card_file(entries, CONFIG_TXT)? {
                Some(bytes) => String::from_utf8(bytes)
                    .with_context(|| format!("{CONFIG_TXT} on the card is not text"))?,
                None => String::new(),
            };
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            // Under `[all]`, so a conditional section the file ends in does not
            // swallow the lines — which is what the firmware itself would do.
            text.push_str("[all]\n");
            for line in &self.config {
                text.push_str(line);
                text.push('\n');
            }
            put_card_file(entries, CONFIG_TXT, text.into_bytes());
        }
        if let Some(cmdline) = &self.cmdline {
            // One line is the whole command line, so this replaces the card's.
            put_card_file(entries, CMDLINE_TXT, format!("{cmdline}\n").into_bytes());
        }
        Ok(())
    }
}

const CONFIG_TXT: &str = "config.txt";
const CMDLINE_TXT: &str = "cmdline.txt";

/// The bytes of a file in the card's root, wherever they come from.
pub fn read_card_file(entries: &[crate::fat::Entry], name: &str) -> Result<Option<Vec<u8>>> {
    let Some(entry) = entries.iter().find(|e| e.name == name) else {
        return Ok(None);
    };
    let crate::fat::Kind::File { source, len } = &entry.kind else {
        bail!("{name} on the card is a directory");
    };
    let bytes = match source {
        crate::fat::Source::Path(path) => {
            std::fs::read(path).with_context(|| format!("reading {}", path.display()))?
        }
        crate::fat::Source::Url(url) => crate::remote::fetch(url, *len)?,
        crate::fat::Source::Bytes(bytes) => bytes.clone(),
    };
    Ok(Some(bytes))
}

/// `name` in the card's root holds `bytes`, in place of whatever it held.
pub fn put_card_file(entries: &mut Vec<crate::fat::Entry>, name: &str, bytes: Vec<u8>) {
    let kind = crate::fat::Kind::File {
        len: bytes.len() as u64,
        source: crate::fat::Source::Bytes(bytes),
    };
    match entries.iter().position(|e| e.name == name) {
        Some(i) => entries[i].kind = kind,
        // The order is the order the firmware finds them in, so keep it by name.
        None => {
            let at = entries.partition_point(|e| e.name.as_str() < name);
            entries.insert(
                at,
                crate::fat::Entry {
                    name: name.to_string(),
                    kind,
                },
            );
        }
    }
}

/// SD card, then the USB stick, then restart: what `--usb` boots with when
/// nothing else says how, since the bootloader's own order never tries USB.
pub const USB_BOOT_ORDER: &str = "0xf41";

/// The `bootconf.txt` edits the options ask for. Re-applied after every
/// self-update reset, which brings back the image's own settings.
pub struct FlashEdits {
    eeprom: bool,
    skip_signed_boot: bool,
    conf_lines: Vec<String>,
    pubkey: Option<Vec<u8>>,
}

impl FlashEdits {
    /// `boot_order` and `conf_lines` become `bootconf.txt` lines; with `usb`
    /// and no order of its own, the order is [`USB_BOOT_ORDER`].
    pub fn new(
        eeprom: bool,
        skip_signed_boot: bool,
        boot_order: Option<&str>,
        bootconf: &[String],
        usb: bool,
        pubkey: Option<Vec<u8>>,
    ) -> Result<Self> {
        let ordered = boot_order.is_some()
            || bootconf
                .iter()
                .any(|l| l.trim_start().starts_with("BOOT_ORDER="));
        let usb_default = (usb && !ordered).then_some(USB_BOOT_ORDER);
        let conf_lines = boot_order
            .into_iter()
            .chain(usb_default)
            .map(|o| format!("BOOT_ORDER={o}"))
            .chain(bootconf.iter().cloned())
            .collect();
        if let Some(k) = &pubkey {
            if k.len() != 264 {
                bail!(
                    "the public key is {} bytes, want 264 (RSA-2048 n + e)",
                    k.len()
                );
            }
        }
        Ok(Self {
            eeprom,
            skip_signed_boot,
            conf_lines,
            pubkey,
        })
    }

    pub fn apply(&self, flash: &mut Vec<u8>, announce: bool) {
        self.unsign(flash, announce);
        self.append_conf(flash, announce);
        self.set_pubkey(flash, announce);
    }

    /// `--skip-signed-boot`: flip `SIGNED_BOOT=1` to `=0`, skipping the very slow
    /// SHA-256 + RSA-2048 verify of `boot.img`. Same length, so the byte layout is
    /// preserved, and the stale `bootconf.sig` is not checked once the flag is 0.
    fn unsign(&self, flash: &mut [u8], announce: bool) {
        if !(self.skip_signed_boot && self.eeprom) {
            return;
        }
        let needle = b"SIGNED_BOOT=1";
        if let Some(i) = flash.windows(needle.len()).position(|w| w == needle) {
            flash[i + needle.len() - 1] = b'0';
            if announce {
                println!("skip-signed-boot: patched bootconf SIGNED_BOOT=0 @ {i:#x}");
            }
        } else if announce {
            eprintln!("skip-signed-boot: 'SIGNED_BOOT=1' not found in EEPROM image");
        }
    }

    /// Append `BOOT_ORDER=` and every `--bootconf KEY=VALUE` line to the EEPROM's
    /// `bootconf.txt`; a later line overrides an earlier one with the same key.
    /// The section is last in the image and followed by erased flash, so growing
    /// it is a length-field bump and an append — nothing moves.
    fn append_conf(&self, flash: &mut Vec<u8>, announce: bool) {
        if self.conf_lines.is_empty() || !self.eeprom {
            return;
        }
        let Some(hdr) = find_bootconf_header(flash) else {
            if announce {
                eprintln!("boot-order: no bootconf.txt section in the EEPROM image");
            }
            return;
        };
        let len = u32::from_be_bytes([
            flash[hdr + 4],
            flash[hdr + 5],
            flash[hdr + 6],
            flash[hdr + 7],
        ]) as usize;
        let text: String = self.conf_lines.iter().map(|l| format!("{l}\n")).collect();
        let end = hdr + 8 + len;
        let new_len = len + text.len();
        flash.splice(end..end + text.len(), text.bytes());
        flash[hdr + 4..hdr + 8].copy_from_slice(&(new_len as u32).to_be_bytes());
        if announce {
            for l in &self.conf_lines {
                println!("bootconf: appended {l} @ {:#x}", end);
            }
        }
    }

    /// An RSA-2048 public key in the EEPROM's `pubkey.bin` slot, as
    /// `rpi-eeprom-config --pubkey` writes it (n then e, little-endian, 256 + 8
    /// bytes). Signed images are verified against it; an all-zero slot verifies nothing.
    fn set_pubkey(&self, flash: &mut [u8], announce: bool) {
        let Some(k) = &self.pubkey else { return };
        match crate::firmware::eeprom::replace_file(flash, "pubkey.bin", k) {
            Ok(()) if announce => println!("eeprom-pubkey: pubkey.bin replaced"),
            Ok(()) => {}
            Err(e) => eprintln!("eeprom-pubkey: {e:#}"),
        }
    }
}

/// Did the boot do what it was run for, and in a word, what happened. An EEPROM
/// boot succeeds once the firmware has started the ARM — where a Pi's boot firmware
/// is done — and nothing after that undoes it; a VPU ELF succeeds by halting.
/// Anything the model could not do fails the run whenever it happens.
pub fn boot_outcome(
    report: &crate::emulator::RunReport,
    emu: &crate::emulator::Emulator,
    eeprom: bool,
    until: Option<&str>,
    reboots: u32,
) -> (bool, String) {
    use crate::emulator::RunEnd;
    use crate::vpu::exec::Stop;

    let end = match &report.end {
        RunEnd::Until => {
            return (
                true,
                format!("the console printed {:?}", until.unwrap_or("")),
            )
        }
        RunEnd::Quit => return (true, "the session was ended".into()),
        RunEnd::Released => return (true, "the ARM was released; snapshot saved".into()),
        RunEnd::Halted(Stop::Fault(f)) => return (false, format!("the VPU faulted: {f:x?}")),
        RunEnd::Core1Halted(Stop::Fault(f)) => {
            return (false, format!("VPU core 1 faulted: {f:x?}"))
        }
        RunEnd::ArmStopped(s) => {
            return (
                false,
                format!("an ARM core hit something not modelled: {s:x?}"),
            )
        }
        RunEnd::Reset => {
            return (
                false,
                format!("the firmware kept resetting ({reboots} reboots)"),
            )
        }
        RunEnd::Halted(Stop::Halt(h)) | RunEnd::Core1Halted(Stop::Halt(h)) => {
            if !eeprom && until.is_none() {
                return (true, format!("the program halted ({h:?})"));
            }
            format!("the VPU halted ({h:?})")
        }
        RunEnd::StopPc(pc) => format!("reached pc {pc:#010x}"),
        RunEnd::StepLimit => "the instruction limit was reached".into(),
        RunEnd::TimeLimit => "the wall-clock limit was reached".into(),
        RunEnd::IdleSpin(pc) => format!("it spun at {pc:#010x} with no output"),
        RunEnd::Stuck { pc, silent_us, .. } => format!(
            "no console output for {:.1} s of modelled time, at pc {pc:?}",
            *silent_us as f64 / 1e6
        ),
    };
    if let Some(text) = until {
        return (false, format!("the console never printed {text:?}: {end}"));
    }
    if !eeprom {
        return (false, end);
    }
    if emu.machine.armctrl.released() {
        (true, "the firmware started the ARM".into())
    } else {
        (false, format!("the firmware never started the ARM: {end}"))
    }
}

/// Found through the section walk, not by searching for the name: the bootcode
/// carries a string table with the same names in it.
fn find_bootconf_header(flash: &[u8]) -> Option<usize> {
    let img = crate::firmware::eeprom::EepromImage::parse(flash).ok()?;
    img.sections
        .iter()
        .find(|s| s.filename.as_deref() == Some("bootconf.txt"))
        .map(|s| s.header_offset)
}

/// Find the device tree blob `arm_loader` left for the ARM: the armstub's
/// `dtb_ptr32`, falling back to the firmware's own `Device tree loaded to` console
/// line only when the ARM was never released (`start4cd.elf` prints nothing there).
/// Neither way hard-codes an address, which is what keeps this working across
/// firmware versions. The header's `totalsize` is trusted over the logged length.
pub fn locate_fdt(
    machine: &mut Machine,
    handoff: Option<Handoff>,
    console: &[u8],
) -> Option<(u32, Vec<u8>)> {
    use crate::bus::{Bus, Width};

    let text = String::from_utf8_lossy(console);
    // Last one wins: a `tryboot` retry would load the tree more than once.
    let logged = text
        .rsplit_once("Device tree loaded to 0x")
        .and_then(|(_, tail)| {
            let (addr_hex, rest) = tail.split_once(" (size 0x")?;
            let addr = u32::from_str_radix(addr_hex.trim(), 16).ok()?;
            let len = rest
                .split_once(')')
                .and_then(|(l, _)| u32::from_str_radix(l.trim(), 16).ok())
                .unwrap_or(0);
            Some((addr, len))
        });
    // The armstub itself lives at 0, so a zero pointer is no tree at all.
    let (addr, logged_len) = match handoff.map(|h| h.dtb).filter(|&a| a != 0) {
        Some(addr) => (addr, logged.filter(|l| l.0 == addr).map_or(0, |l| l.1)),
        None => logged?,
    };

    let byte = |m: &mut Machine, a: u32| m.load(a, Width::Byte).unwrap_or(0) as u8;
    let read = |m: &mut Machine, a: u32, n: u32| -> Vec<u8> {
        (0..n).map(|i| byte(m, a.wrapping_add(i))).collect()
    };
    let head = read(machine, addr, 8);
    let totalsize = u32::from_be_bytes([head[4], head[5], head[6], head[7]]);
    // Fall back to the logged length so `Fdt::parse` can report what is there.
    let len = if u32::from_be_bytes([head[0], head[1], head[2], head[3]]) == crate::fdt::FDT_MAGIC
        && (40..=8 << 20).contains(&totalsize)
    {
        totalsize
    } else {
        logged_len.max(40)
    };
    Some((addr, read(machine, addr, len)))
}
