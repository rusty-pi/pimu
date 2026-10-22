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
    snapshot: Option<Vec<u8>>,
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
            snapshot: None,
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

    /// Start from a snapshot the page kept from a boot with the same
    /// settings (`Pi::take_snapshot`), skipping the firmware's boot.
    pub fn set_snapshot(&mut self, bytes: Vec<u8>) {
        self.snapshot = Some(bytes);
    }

    pub fn build(self) -> Result<Pi, JsError> {
        let mut session = Session::new(self.options, self.inputs).map_err(js_error)?;
        if let Some(bytes) = self.snapshot {
            session.load_snapshot(bytes).map_err(js_error)?;
        }
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

    /// The snapshot taken when the ARM was released, if the options asked
    /// for one: once, as it is taken.
    pub fn take_snapshot(&mut self) -> Option<Vec<u8>> {
        self.session.take_snapshot()
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

/// Whether this build can run the ARM cores on threads: it was linked for
/// shared memory, and the page is cross-origin isolated enough to have given
/// it any.
#[wasm_bindgen]
pub fn threads_built() -> bool {
    cfg!(pimu_threads)
}

/// A worker thread's whole life: join the pool the cores run on and serve it
/// for good. The page calls this once in each worker it starts for the purpose,
/// after loading this module on the shared memory.
#[wasm_bindgen]
pub fn pool_thread() {
    #[cfg(pimu_threads)]
    crate::arm::pool::run_forever();
}

/// How many workers are serving the pool.
#[wasm_bindgen]
pub fn pool_threads() -> usize {
    #[cfg(pimu_threads)]
    return crate::arm::pool::threads();
    #[cfg(not(pimu_threads))]
    0
}

/// Run four cores side by side on the pool and report whether they counted
/// every increment. `false` where there are no threads.
#[wasm_bindgen]
pub fn parallel_probe() -> bool {
    #[cfg(pimu_threads)]
    return crate::arm::parallel_probe();
    #[cfg(not(pimu_threads))]
    false
}

/// The ARM micro-benchmark (`src/arm/bench.rs`): each core runs about
/// `per_core` instructions, on threads when `parallel` and this build has
/// them. `[threaded (0 or 1), instructions, seconds]`, or empty if the cores
/// did not finish.
#[wasm_bindgen]
pub fn arm_speed(parallel: bool, per_core: f64) -> Vec<f64> {
    crate::arm::bench::speed(parallel, per_core as u64).map_or(Vec::new(), |s| {
        vec![f64::from(u8::from(s.threaded)), s.insns as f64, s.secs]
    })
}

/// Compile the threaded cores' straight runs to wasm once they have been
/// entered `hot` times (`src/aarch64/wasmjit.rs`), or not at all for 0; for
/// the cores started after. False in a build without it.
#[wasm_bindgen]
pub fn set_wasm_jit(hot: u32) -> bool {
    #[cfg(pimu_wasm_jit)]
    {
        crate::aarch64::wasmjit::set_hot(hot);
        true
    }
    #[cfg(not(pimu_wasm_jit))]
    {
        let _ = hot;
        false
    }
}

/// What the wasm JIT did, every core's: runs and modules compiled,
/// microseconds compiling, entries into compiled code and the instructions
/// retired from them, instructions compiled inline and through their
/// handlers.
#[wasm_bindgen]
pub fn wasm_jit_stats() -> Vec<f64> {
    #[cfg(pimu_wasm_jit)]
    {
        use crate::aarch64::wasmjit::{STATS, STATS_CALLED, STATS_INLINE};
        use std::sync::atomic::Ordering::Relaxed;
        let mut v: Vec<f64> = STATS.iter().map(|a| a.load(Relaxed) as f64).collect();
        v.push(STATS_INLINE.load(Relaxed) as f64);
        v.push(STATS_CALLED.load(Relaxed) as f64);
        v
    }
    #[cfg(not(pimu_wasm_jit))]
    Vec::new()
}
