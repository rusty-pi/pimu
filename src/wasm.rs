//! The emulator in a browser tab: `wasm-bindgen` glue over the same library the
//! command line drives.
//!
//! A page hands over the EEPROM image and the files of a boot partition, then
//! calls [`Pi::run`] for a slice of wall time at a time (from a Web Worker, so
//! the page stays responsive) and shows the serial bytes it returns.

use std::sync::mpsc::Sender;
use std::time::Duration;

use wasm_bindgen::prelude::*;

use crate::emulator::{Emulator, RunEnd, RunLimits};
use crate::fat::{Entry, Kind, Source};
use crate::firmware::bootrom::BootRom;
use crate::log::{Log, Spec};
use crate::machine::Machine;
use crate::periph::disk::Disk;
use crate::soc::Board;
use crate::stdio::{HostEvent, HostInput};
use crate::vpu::UnimplPolicy;

/// A Raspberry Pi 4B with 1 GiB, rev 1.4: the biggest board whose RAM a 32-bit
/// address space holds with room to spare (the default 8 GiB board's size does
/// not even fit a `usize` here).
const BROWSER_BOARD_REVISION: u32 = 0x00A0_3114;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(line: &str);
}

/// The `--log` channels, a line each in the browser's console.
struct ConsoleLog(Vec<u8>);

impl std::io::Write for ConsoleLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(bytes);
        while let Some(end) = self.0.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.0.drain(..=end).collect();
            log(String::from_utf8_lossy(&line[..end]).as_ref());
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn js_error(e: anyhow::Error) -> JsError {
    JsError::new(&format!("{e:#}"))
}

/// The boot partition's files, collected before the machine is built.
#[wasm_bindgen]
pub struct PiBuilder {
    eeprom: Vec<u8>,
    root: Vec<Entry>,
    log: Log,
}

#[wasm_bindgen]
impl PiBuilder {
    #[wasm_bindgen(constructor)]
    pub fn new(eeprom: Vec<u8>) -> PiBuilder {
        PiBuilder {
            eeprom,
            root: Vec::new(),
            log: Log::default(),
        }
    }

    /// Send these `--log` channels (`arm-exc,mbox`) to the browser's console.
    pub fn log(&mut self, channels: &str) -> Result<(), JsError> {
        let spec = Spec::parse(channels).map_err(|e| JsError::new(&e))?;
        self.log = Log::new(spec, Box::new(ConsoleLog(Vec::new())));
        Ok(())
    }

    /// A file of the boot partition; a `/` in `path` puts it in a directory.
    pub fn add_file(&mut self, path: &str, bytes: Vec<u8>) {
        insert(&mut self.root, path, bytes);
    }

    pub fn build(self) -> Result<Pi, JsError> {
        Pi::assemble(self.eeprom, self.root, self.log).map_err(js_error)
    }
}

fn insert(dir: &mut Vec<Entry>, path: &str, bytes: Vec<u8>) {
    match path.split_once('/') {
        Some((name, rest)) => {
            let at = match dir.iter().position(|e| e.name == name) {
                Some(at) => at,
                None => {
                    dir.push(Entry {
                        name: name.to_string(),
                        kind: Kind::Dir(Vec::new()),
                    });
                    dir.len() - 1
                }
            };
            if let Kind::Dir(children) = &mut dir[at].kind {
                insert(children, rest, bytes);
            }
        }
        None => dir.push(Entry {
            name: path.to_string(),
            kind: Kind::File {
                len: bytes.len() as u64,
                source: Source::Bytes(bytes),
            },
        }),
    }
}

#[wasm_bindgen]
pub struct Pi {
    emu: Emulator,
    keys: Sender<HostEvent>,
    ended: Option<String>,
}

#[wasm_bindgen]
impl Pi {
    fn assemble(eeprom: Vec<u8>, root: Vec<Entry>, log: Log) -> anyhow::Result<Pi> {
        let board = Board {
            revision: BROWSER_BOARD_REVISION,
            ..Board::default()
        };
        let mut machine = Machine::new(board.memory_bytes() as usize);
        machine.set_board(board);
        machine.set_log(log);
        machine.spi0.attach_flash(eeprom)?;
        let card = crate::fat::card_from_entries(root)?;
        machine.emmc2.insert_disk(Disk::from_card(card)?);
        let start = BootRom::unkeyed().boot(&mut machine)?.entry;

        let mut emu = Emulator::new(machine, start);
        emu.stream_console = false;
        emu.set_unimpl_policy(UnimplPolicy::ReconFault);
        let (host, keys) = HostInput::channel();
        emu.input.host = Some(host);
        Ok(Pi {
            emu,
            keys,
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
        if report.end != RunEnd::TimeLimit {
            self.ended = Some(format!("{:?}", report.end));
        }
        report.console
    }

    /// Bytes typed at the serial console.
    pub fn send(&mut self, bytes: &[u8]) {
        let _ = self.keys.send(HostEvent::Bytes(bytes.to_vec()));
    }

    /// Why the run stopped, once it has.
    pub fn ended(&self) -> Option<String> {
        self.ended.clone()
    }
}
