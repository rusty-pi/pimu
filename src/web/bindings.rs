//! What a page calls.

use wasm_bindgen::prelude::*;

use crate::fat::{Entry, Kind, Source};
use crate::periph::disk::Disk;

use super::options::Options;
use super::session::{bootconf_entries, DiskFactory, Inputs, Session};
use super::source::JsSource;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn error(message: &str);
}

/// A panic is a line in the console instead of an `unreachable` with no reason.
#[wasm_bindgen(start)]
fn start() {
    std::panic::set_hook(Box::new(|info| error(&info.to_string())));
}

fn js_error(e: anyhow::Error) -> JsError {
    JsError::new(&format!("{e:#}"))
}

/// The `bootconf.txt` of an EEPROM image, as JSON `[[section, key, value], ...]`:
/// what a settings form starts from. `[]` for something that is not an image.
#[wasm_bindgen]
pub fn eeprom_bootconf(image: &[u8]) -> String {
    serde_json::to_string(&bootconf_entries(image)).unwrap_or_else(|_| "[]".into())
}

/// Files as the small FAT16 volume HTTP boot downloads as `boot.img`.
#[wasm_bindgen]
#[derive(Default)]
pub struct BootImage {
    root: Vec<Entry>,
}

#[wasm_bindgen]
impl BootImage {
    #[wasm_bindgen(constructor)]
    pub fn new() -> BootImage {
        BootImage::default()
    }

    /// A file of the volume; a `/` in `path` puts it in a directory.
    pub fn add_file(&mut self, path: &str, bytes: Vec<u8>) {
        insert(&mut self.root, path, bytes);
    }

    pub fn build(self) -> Result<Vec<u8>, JsError> {
        crate::bootimg::fat16_image(self.root).map_err(js_error)
    }
}

/// Everything the machine boots from, collected before it is built.
#[wasm_bindgen]
pub struct PiBuilder {
    options: Options,
    inputs: Inputs,
}

#[wasm_bindgen]
impl PiBuilder {
    /// `options` is the JSON of [`Options`]; an empty string takes the defaults.
    #[wasm_bindgen(constructor)]
    pub fn new(options: &str, eeprom: Vec<u8>) -> Result<PiBuilder, JsError> {
        let options = Options::parse(options).map_err(js_error)?;
        Ok(PiBuilder {
            options,
            inputs: Inputs {
                eeprom,
                ..Inputs::default()
            },
        })
    }

    /// A file of the boot partition; a `/` in `path` puts it in a directory.
    pub fn add_file(&mut self, path: &str, bytes: Vec<u8>) {
        insert(&mut self.inputs.boot_files, path, bytes);
    }

    /// A file the network peer serves over TFTP.
    pub fn add_tftp_file(&mut self, path: &str, bytes: Vec<u8>) {
        self.inputs.tftp_files.push((path.to_string(), bytes));
    }

    /// A file the network peer serves over HTTP.
    pub fn add_http_file(&mut self, path: &str, bytes: Vec<u8>) {
        self.inputs.http_files.push((path.to_string(), bytes));
    }

    /// The RSA-2048 public key for `pubkey.bin` in the EEPROM image.
    pub fn set_pubkey(&mut self, bytes: Vec<u8>) {
        self.inputs.pubkey = Some(bytes);
    }

    /// The ID EEPROM of a HAT on the 40-pin header.
    pub fn set_hat(&mut self, bytes: Vec<u8>) {
        self.inputs.hat = Some(bytes);
    }

    /// The EDID the monitor answers with, when `display` is on.
    pub fn set_edid(&mut self, bytes: Vec<u8>) {
        self.inputs.edid = Some(bytes);
    }

    /// A disk image in `slot` (`sd`, `emmc`, `usb` or `otg`): an object
    /// `{ size, read(offset, length) -> Uint8Array }`. It is read as the guest
    /// asks for blocks and never whole.
    pub fn attach_image(&mut self, slot: &str, image: JsValue) -> Result<(), JsError> {
        let source = JsSource::new(image).map_err(js_error)?;
        let factory: DiskFactory = Box::new(move || Disk::from_source(Box::new(source.clone()), 0));
        let slot = match slot {
            "sd" => &mut self.inputs.sd,
            "emmc" => &mut self.inputs.emmc,
            "usb" => &mut self.inputs.usb,
            "otg" => &mut self.inputs.otg,
            other => return Err(JsError::new(&format!("no slot {other:?}"))),
        };
        *slot = Some(factory);
        Ok(())
    }

    pub fn build(self) -> Result<Pi, JsError> {
        let session = Session::new(self.options, self.inputs).map_err(js_error)?;
        Ok(Pi { session })
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
    session: Session,
}

#[wasm_bindgen]
impl Pi {
    /// Run for about `millis` of wall time and return the serial output that
    /// came out meanwhile.
    pub fn run(&mut self, millis: u32) -> Vec<u8> {
        self.session.run(millis)
    }

    /// Bytes typed at the serial console.
    pub fn send(&mut self, bytes: &[u8]) {
        self.session.send(bytes);
    }

    /// Have the built-in network peer send the board a Wake-on-LAN packet.
    pub fn send_magic_packet(&mut self) -> bool {
        self.session.send_magic_packet()
    }

    /// The `log` channels' lines since the last call, as JSON a line.
    pub fn take_log(&mut self) -> String {
        self.session.take_log()
    }

    /// A JSON object: whether the run has ended and how, the reboots so far,
    /// whether the ARM has been started, and the model's clock.
    pub fn status(&mut self) -> String {
        self.session.status().to_string()
    }

    /// The device tree the firmware handed to the ARM, as source.
    pub fn device_tree(&mut self) -> Option<String> {
        self.session.device_tree().map(|(dts, _)| dts)
    }

    pub fn device_tree_blob(&mut self) -> Option<Vec<u8>> {
        self.session.device_tree().map(|(_, blob)| blob)
    }

    /// The SPI flash as the firmware has left it.
    pub fn eeprom(&self) -> Vec<u8> {
        self.session.eeprom()
    }

    /// The EEPROM's `bootconf.txt`, a `[section] KEY=VALUE` line each.
    pub fn bootconf(&self) -> Vec<String> {
        self.session.bootconf()
    }

    /// The fuses that are not blank, as JSON `[{row, value, meaning}]`.
    pub fn otp(&self) -> String {
        self.session.otp().to_string()
    }
}
