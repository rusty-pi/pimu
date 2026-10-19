//! A booting machine a page drives a slice of wall time at a time, and the
//! reset loop around it that `pimu boot` runs in one go.
//!
//! This mirrors `Rig` and `run_boot` in `src/cli/boot.rs`, minus what a browser
//! has no use for (a VPU ELF as the payload, `--maskrom`, `--patch`, the
//! `--scenario` limits). What both share lives in [`crate::boot`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::boot::{boot_outcome, locate_fdt, CardEdits, FlashEdits};
use crate::emulator::{Emulator, RunEnd, RunLimits};
use crate::fat::Entry;
use crate::firmware::bootrom::BootRom;
use crate::log::{Log, Spec};
use crate::machine::Machine;
use crate::periph::disk::Disk;
use crate::periph::fxl6408::Fxl6408;
use crate::soc::Board;
use crate::stdio::{HostEvent, HostInput};
use crate::vpu::UnimplPolicy;

use super::options::Options;

/// Blue socket A: root port 1 is the USB2 port feeding the on-board VIA hub, so a
/// SuperSpeed fixture goes on port 2 (see `USB_ROOT_PORT` in `src/cli/boot.rs`).
const USB_ROOT_PORT: usize = 2;

/// A reset that comes back this many times is the firmware giving up.
const MAX_REBOOTS: u32 = 4;

/// The console kept for finding the device tree by its log line.
const CONSOLE_KEPT: usize = 1 << 20;

/// Log lines held between two calls to [`Session::take_log`].
const LOG_KEPT: usize = 8 << 20;

/// Builds a fresh disk for each boot: a card starts from its medium again after
/// a reset, with what the guest wrote forgotten.
pub type DiskFactory = Box<dyn Fn() -> Disk>;

type SharedDisk = Rc<RefCell<Disk>>;

/// What the page gives the machine to boot from.
#[derive(Default)]
pub struct Inputs {
    pub eeprom: Vec<u8>,
    pub pubkey: Option<Vec<u8>>,
    /// The files of the boot partition: the SD card, when there is no `sd` image.
    pub boot_files: Vec<Entry>,
    pub sd: Option<DiskFactory>,
    pub emmc: Option<DiskFactory>,
    pub usb: Option<DiskFactory>,
    pub otg: Option<DiskFactory>,
    pub hat: Option<Vec<u8>>,
    pub edid: Option<Vec<u8>>,
    /// Files the network peer serves over TFTP.
    pub tftp_files: Vec<(String, Vec<u8>)>,
    /// Files the network peer serves over HTTP.
    pub http_files: Vec<(String, Vec<u8>)>,
}

#[derive(Clone, Default)]
struct LogBuffer(Rc<RefCell<(Vec<u8>, u64)>>);

impl Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut state = self.0.borrow_mut();
        if state.0.len() + bytes.len() <= LOG_KEPT {
            state.0.extend_from_slice(bytes);
        } else {
            state.1 += 1;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuffer {
    fn take(&self) -> String {
        let mut state = self.0.borrow_mut();
        String::from_utf8_lossy(&std::mem::take(&mut state.0)).into_owned()
    }

    fn dropped(&self) -> u64 {
        self.0.borrow().1
    }

    fn note(&self, channel: &str, msg: &str) {
        let line = format!("{{\"us\":0,\"channel\":\"{channel}\",\"msg\":\"{msg}\"}}\n");
        let _ = self.clone().write(line.as_bytes());
    }
}

/// What the machine carries from one boot into the next: a reset leaves the
/// flash as the self-update wrote it, the fuses programmed, the expander's
/// pins, the PM partition bits and the DRAM's contents.
#[derive(Default)]
struct Carry {
    flash: Vec<u8>,
    fuses: Option<BTreeMap<u32, u32>>,
    partition: u32,
    expander: Option<Fxl6408>,
    ram: Option<crate::mem::Contents>,
}

struct Rig {
    options: Options,
    board: Board,
    log: Log,
    bootrom: BootRom,
    flash_edits: FlashEdits,
    inputs: Inputs,
    usb: Option<SharedDisk>,
    otg: Option<SharedDisk>,
}

impl Rig {
    fn machine(&self, carry: &mut Carry, reboots: u32) -> Result<Machine> {
        let options = &self.options;
        let mut machine = Machine::new(self.board.memory_bytes());
        machine.set_board(self.board);
        machine.set_log(self.log.clone());
        machine.spi0.attach_flash(carry.flash.clone())?;
        if let Some(emmc) = &self.inputs.emmc {
            machine
                .emmc2
                .insert_mmc_disk(emmc().with_log(self.log.clone(), "emmc"));
        } else if let Some(sd) = &self.inputs.sd {
            machine
                .emmc2
                .insert_disk(sd().with_log(self.log.clone(), "sd"));
        } else if !options.no_sd_card {
            let card = crate::fat::card_from_entries(self.inputs.boot_files.clone())?;
            let disk = Disk::from_card(card).context("building the boot partition")?;
            machine
                .emmc2
                .insert_disk(disk.with_log(self.log.clone(), "sd"));
        }
        if options.display {
            let edid = self
                .inputs
                .edid
                .clone()
                .unwrap_or_else(|| crate::periph::hdmi_ddc::DEFAULT_EDID.to_vec());
            machine.hdmi0 = crate::periph::Hdmi::new("hdmi0").with_display();
            machine.hdmi_ddc0 = crate::periph::HdmiDdc::new("hdmi-ddc0").with_edid(edid);
        }
        if options.check_coherency {
            machine.ram.coherency = crate::coherency::Coherency::on(self.log.clone());
        }
        if options.check_alignment {
            machine.alignment = crate::align::Alignment::on(self.log.clone());
        }
        if let Some(seed) = options.jitter {
            crate::jitter::arm(seed, self.log.clone());
            if let Some(one_in) = options.faults {
                crate::jitter::set_faults(one_in);
            }
        }
        if let Some(hat) = &self.inputs.hat {
            machine
                .bsc0
                .attach_eeprom(crate::periph::hat::HatEeprom::new(hat.clone()));
        }
        if let Some(disk) = &self.usb {
            machine.pcie.endpoint.attach(
                USB_ROOT_PORT,
                Box::new(crate::periph::usb::MassStorage::with_disk(disk.clone())),
            );
        }
        if let Some(disk) = &self.otg {
            let stick = crate::periph::usb::MassStorage::with_disk_hs(disk.clone());
            let device: Box<dyn crate::periph::usb::UsbDevice> = if options.otg_dock {
                Box::new(crate::periph::usb::Hub::dock(stick))
            } else {
                Box::new(stick)
            };
            machine.xhci_otg.attach(device);
        }
        if options.network {
            let mut peer = crate::net::BuiltinPeer::new().with_log(self.log.clone());
            for (name, bytes) in &self.inputs.tftp_files {
                peer.add_tftp_file(name, bytes.clone());
            }
            for (name, bytes) in &self.inputs.http_files {
                peer.add_http_file(name, bytes.clone());
            }
            machine.attach_net(Box::new(peer));
        }

        if let Some(ram) = carry.ram.take() {
            machine.ram.restore_contents(ram);
        }
        if let Some(fuses) = &carry.fuses {
            machine.config_otp.fuse_rows(fuses);
        }
        for &(row, value) in &options.otp_rows {
            machine.config_otp.set(row, value);
        }
        machine.pm.keep_partition_bits(carry.partition);
        if let Some(expander) = carry.expander.take() {
            machine.bsc_pmic.fit_expander(expander);
        }
        if reboots == 0 && options.tryboot {
            machine.pm.request_tryboot();
        }
        Ok(machine)
    }

    fn emulator(&self, carry: &mut Carry, reboots: u32) -> Result<Emulator> {
        let mut machine = self.machine(carry, reboots)?;
        let start = self.bootrom.boot(&mut machine)?.entry;
        let mut emu = Emulator::new(machine, start);
        emu.stream_console = false;
        emu.parallel_arm = self.options.parallel;
        emu.set_unimpl_policy(if self.options.skip_unimpl {
            UnimplPolicy::Skip
        } else {
            UnimplPolicy::ReconFault
        });
        Ok(emu)
    }
}

/// The `bootconf.txt` of an EEPROM image as `(section, KEY, VALUE)`, `all`
/// for the section that has no header; empty for an image without one.
pub fn bootconf_entries(flash: &[u8]) -> Vec<(String, String, String)> {
    let Ok(image) = crate::firmware::eeprom::EepromImage::parse(flash) else {
        return Vec::new();
    };
    let Some(conf) = image.bootconf() else {
        return Vec::new();
    };
    conf.entries
        .into_iter()
        .map(|(group, key, value)| {
            let group = if group.is_empty() {
                "all".into()
            } else {
                group
            };
            (group, key, value)
        })
        .collect()
}

/// [`bootconf_entries`] as `[section] KEY=VALUE` lines, then the boot order in words.
pub fn bootconf_lines(flash: &[u8]) -> Vec<String> {
    let mut lines: Vec<String> = bootconf_entries(flash)
        .into_iter()
        .map(|(group, key, value)| format!("[{group}] {key}={value}"))
        .collect();
    if let Some(order) = crate::firmware::eeprom::EepromImage::parse(flash)
        .ok()
        .and_then(|image| image.bootconf())
        .and_then(|conf| conf.boot_order_names())
    {
        lines.push(format!("BOOT_ORDER: {order}"));
    }
    lines
}

pub struct Session {
    rig: Rig,
    carry: Carry,
    emu: Emulator,
    keys: Sender<HostEvent>,
    reboots: u32,
    console: Vec<u8>,
    log: LogBuffer,
    ended: Option<(bool, String)>,
}

impl Session {
    pub fn new(options: Options, mut inputs: Inputs) -> Result<Session> {
        let board = options.board()?;
        let buffer = LogBuffer::default();
        let mut spec = Spec::parse("jsonl:io").map_err(anyhow::Error::msg)?;
        if !options.log.trim().is_empty() {
            spec.add(Spec::parse(options.log.trim()).map_err(anyhow::Error::msg)?)
                .map_err(anyhow::Error::msg)?;
        }
        let log = Log::new(spec, Box::new(buffer.clone()));

        let card = CardEdits {
            config: options.config_txt.clone(),
            cmdline: options.cmdline.clone(),
        };
        if inputs.sd.is_some() || inputs.emmc.is_some() {
            if !card.is_empty() {
                bail!("config.txt and cmdline.txt lines edit a card built from files, not a disk image");
            }
        } else {
            card.apply(&mut inputs.boot_files)?;
        }

        let flash_edits = FlashEdits::new(
            true,
            options.skip_signed_boot,
            options.boot_order.as_deref(),
            &options.bootconf,
            inputs.usb.is_some(),
            inputs.pubkey.take(),
        )?;
        let mut flash = std::mem::take(&mut inputs.eeprom);
        flash_edits.apply(&mut flash, false);

        let min_stick = options.usb_mb.unwrap_or(0) << 20;
        let usb = inputs.usb.take().map(|make| {
            Rc::new(RefCell::new(
                make().with_capacity(min_stick).with_log(log.clone(), "usb"),
            ))
        });
        let otg = inputs.otg.take().map(|make| {
            Rc::new(RefCell::new(
                make().with_capacity(min_stick).with_log(log.clone(), "otg"),
            ))
        });

        let rig = Rig {
            options,
            board,
            log,
            bootrom: BootRom::unkeyed(),
            flash_edits,
            inputs,
            usb,
            otg,
        };
        let mut carry = Carry {
            flash,
            ..Carry::default()
        };
        let mut emu = rig.emulator(&mut carry, 0)?;
        let (host, keys) = HostInput::channel();
        emu.input.host = Some(host);
        Ok(Session {
            rig,
            carry,
            emu,
            keys,
            reboots: 0,
            console: Vec::new(),
            log: buffer,
            ended: None,
        })
    }

    /// Run for about `millis` of wall time and return the serial output that
    /// came out meanwhile.
    pub fn run(&mut self, millis: u32) -> Vec<u8> {
        if self.ended.is_some() {
            return Vec::new();
        }
        let limits = RunLimits {
            max_steps: None,
            max_wall: Some(Duration::from_millis(millis.into())),
            ..RunLimits::default()
        };
        let report = self.emu.run(&limits);
        if self.console.len() < CONSOLE_KEPT {
            self.console.extend_from_slice(&report.console);
        }
        match &report.end {
            RunEnd::TimeLimit => {}
            RunEnd::Reset => {
                if let Err(e) = self.reboot() {
                    self.ended = Some((false, format!("{e:#}")));
                }
            }
            _ => {
                self.settle();
                self.ended = Some(boot_outcome(&report, &self.emu, true, None, self.reboots));
            }
        }
        report.console
    }

    /// The firmware asked for a reset: boot again from the flash it left, with
    /// the DRAM, the fuses and the pins as they were.
    fn reboot(&mut self) -> Result<()> {
        self.reboots += 1;
        if self.reboots > MAX_REBOOTS {
            self.ended = Some((
                false,
                format!("the firmware kept resetting ({} reboots)", self.reboots - 1),
            ));
            return Ok(());
        }
        let machine = &mut self.emu.machine;
        let mut flash = machine.spi0.flash_bytes().to_vec();
        self.rig.flash_edits.apply(&mut flash, false);
        self.carry = Carry {
            flash,
            fuses: Some(machine.config_otp.fuses().clone()),
            partition: machine.pm.partition_bits(),
            expander: machine.bsc_pmic.expander().cloned(),
            ram: Some(machine.ram.take_contents()),
        };
        let host = self.emu.input.host.take();
        self.log
            .note("bench", &format!("reset: reboot {}", self.reboots));
        self.emu = self.rig.emulator(&mut self.carry, self.reboots)?;
        self.emu.input.host = host;
        Ok(())
    }

    /// A core parked in a busy-wait loop is behind until it is brought up to date.
    fn settle(&mut self) {
        if let Some(arm) = &mut self.emu.arm {
            arm.settle(&self.emu.machine);
        }
    }

    pub fn send(&self, bytes: &[u8]) {
        let _ = self.keys.send(HostEvent::Bytes(bytes.to_vec()));
    }

    /// Have the network peer send the board a Wake-on-LAN magic packet.
    pub fn send_magic_packet(&mut self) -> bool {
        self.emu.machine.send_magic_packet()
    }

    /// The log lines (JSON, one a line) since the last call, the block run
    /// still being joined included, so a page sees the traffic as it happens.
    pub fn take_log(&self) -> String {
        self.rig.log.flush();
        self.log.take()
    }

    /// Each core as a page draws it: `off` until something starts it, `running`,
    /// or `sleeping` in a `wfi`/`wfe` (or a busy-wait the model has parked).
    fn cores(&self) -> serde_json::Value {
        let arm: Vec<_> = (0..crate::arm::CORES)
            .map(|id| {
                let core = self.emu.arm.as_ref().and_then(|a| a.cores.get(id));
                let (state, insns) = match core {
                    Some(core) if core.entered.is_none() => ("off", 0),
                    Some(core) if core.waiting => ("sleeping", core.insns),
                    Some(core) => ("running", core.insns),
                    None => ("off", 0),
                };
                json!({ "state": state, "insns": insns })
            })
            .collect();
        let vpu = [
            json!({ "state": "running", "insns": self.emu.cpu.retired }),
            match &self.emu.cpu1 {
                Some(core) => json!({ "state": "running", "insns": core.retired }),
                None => json!({ "state": "off", "insns": 0 }),
            },
        ];
        json!({ "arm": arm, "vpu": vpu })
    }

    pub fn status(&mut self) -> serde_json::Value {
        let cores = self.cores();
        json!({
            "cores": cores,
            "ended": self.ended.is_some(),
            "ok": self.ended.as_ref().map(|e| e.0),
            "why": self.ended.as_ref().map(|e| e.1.as_str()),
            "reboots": self.reboots,
            "armReleased": self.emu.machine.armctrl.released(),
            "parallel": self.emu.arm.as_ref().is_some_and(|a| a.is_parallel()),
            "modelUs": self.emu.machine.systimer.now_us(),
            "retired": self.emu.cpu.retired,
            "logDropped": self.log.dropped(),
        })
    }

    /// The device tree the firmware handed to the ARM, as source and as a blob.
    pub fn device_tree(&mut self) -> Option<(String, Vec<u8>)> {
        self.settle();
        let handoff = self.emu.arm.as_ref().and_then(|a| a.handoff);
        let (_, blob) = locate_fdt(&mut self.emu.machine, handoff, &self.console)?;
        let fdt = crate::fdt::Fdt::parse(&blob).ok()?;
        Some((fdt.to_dts(), blob))
    }

    /// The SPI flash as the firmware has left it.
    pub fn eeprom(&self) -> Vec<u8> {
        self.emu.machine.spi0.flash_bytes().to_vec()
    }

    /// The EEPROM's `bootconf.txt`, a `[section] KEY=VALUE` line each.
    pub fn bootconf(&self) -> Vec<String> {
        bootconf_lines(self.emu.machine.spi0.flash_bytes())
    }

    /// The fuses that are not blank, with what each row is for.
    pub fn otp(&self) -> serde_json::Value {
        let rows: Vec<_> = self
            .emu
            .machine
            .config_otp
            .fuses()
            .iter()
            .filter(|(_, &value)| value != 0)
            .map(|(&row, &value)| {
                json!({
                    "row": row,
                    "value": value,
                    "meaning": crate::periph::configotp::row_meaning(row),
                })
            })
            .collect();
        json!(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_eeprom_does_not_boot() {
        let inputs = Inputs {
            eeprom: vec![0xff; 512 << 10],
            ..Inputs::default()
        };
        assert!(Session::new(Options::default(), inputs).is_err());
    }

    #[test]
    fn card_lines_do_not_go_on_a_disk_image() {
        let options = Options {
            cmdline: Some("console=ttyAMA0".into()),
            ..Options::default()
        };
        let inputs = Inputs {
            eeprom: vec![0xff; 512 << 10],
            sd: Some(Box::new(|| Disk::from_vec(vec![0; 1024]))),
            ..Inputs::default()
        };
        let err = Session::new(options, inputs).err().unwrap();
        assert!(format!("{err:#}").contains("disk image"));
    }
}
