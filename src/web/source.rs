//! An image the page holds, read a range at a time.

use anyhow::{anyhow, bail, Result};
use js_sys::{Function, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};

use crate::periph::disk::ByteSource;

/// A JavaScript object `{ size, read(offset, length) }` whose `read` returns a
/// `Uint8Array` — a `Blob` read with `FileReaderSync`, or a URL fetched with a
/// ranged request, both from the worker the machine runs in.
#[derive(Clone)]
pub struct JsSource {
    object: JsValue,
    read: Function,
    len: u64,
}

fn describe(e: JsValue) -> anyhow::Error {
    anyhow!("{}", e.as_string().unwrap_or_else(|| format!("{e:?}")))
}

impl JsSource {
    pub fn new(object: JsValue) -> Result<JsSource> {
        let size = Reflect::get(&object, &"size".into())
            .map_err(describe)?
            .as_f64()
            .ok_or_else(|| anyhow!("the image has no size"))?;
        let read = Reflect::get(&object, &"read".into())
            .map_err(describe)?
            .dyn_into::<Function>()
            .map_err(|_| anyhow!("the image has no read()"))?;
        Ok(JsSource {
            object,
            read,
            len: size as u64,
        })
    }
}

impl ByteSource for JsSource {
    fn size(&self) -> u64 {
        self.len
    }

    fn read_at(&self, at: u64, out: &mut [u8]) -> Result<()> {
        let got = self
            .read
            .call2(
                &self.object,
                &(at as f64).into(),
                &(out.len() as f64).into(),
            )
            .map_err(describe)?;
        let bytes = Uint8Array::new(&got);
        if bytes.length() as usize != out.len() {
            bail!(
                "asked for {} bytes at {at}, got {}",
                out.len(),
                bytes.length()
            );
        }
        bytes.copy_to(out);
        Ok(())
    }
}
