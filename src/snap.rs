//! A machine saved at the moment the VPU releases the ARM, and loaded again,
//! so a run that is about the ARM side skips the firmware's boot.
//!
//! Loading goes *into* a machine built from the same command line
//! ([`serde::Deserialize::deserialize_in_place`]): what a snapshot does not
//! hold — open files, the network backend, the log, constant tables — stays
//! as that build made it. DRAM is saved as its non-zero pages.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The value of a field a snapshot leaves out, for the whole-value
/// deserializer a derive also generates: a snapshot is only ever loaded in
/// place, which keeps those fields as they are.
pub fn unset<T>() -> T {
    panic!(
        "a snapshot is loaded into a machine, never as a new one: {}",
        std::any::type_name::<T>()
    )
}

/// A field a snapshot leaves out: the machine it is loaded into keeps its
/// own (an open file, a backend, a constant table). serde's in-place
/// deserializer would reset a `skip` field to its default, so this is a type
/// of its own rather than an attribute.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keep<T>(pub T);

impl<T> std::ops::Deref for Keep<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> std::ops::DerefMut for Keep<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T> Serialize for Keep<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_unit()
    }
}

impl<'de, T> Deserialize<'de> for Keep<T> {
    fn deserialize<D: Deserializer<'de>>(_: D) -> Result<Self, D::Error> {
        unset()
    }

    fn deserialize_in_place<D: Deserializer<'de>>(d: D, _: &mut Self) -> Result<(), D::Error> {
        <()>::deserialize(d)
    }
}

/// `[T; N]` for any `N`, which serde implements only up to 32.
pub mod array {
    use super::*;

    pub fn serialize<S: Serializer, T: Serialize, const N: usize>(
        v: &[T; N],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.collect_seq(v.iter())
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>, const N: usize>(
        d: D,
    ) -> Result<[T; N], D::Error> {
        let v = Vec::<T>::deserialize(d)?;
        let n = v.len();
        v.try_into()
            .map_err(|_| serde::de::Error::invalid_length(n, &"the array's length"))
    }
}

/// A disk's written blocks, by LBA.
pub mod blocks {
    use super::*;
    use std::collections::HashMap;

    pub fn serialize<S: Serializer, const N: usize>(
        v: &HashMap<u64, Box<[u8; N]>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.collect_map(v.iter().map(|(k, b)| (k, &b[..])))
    }

    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<HashMap<u64, Box<[u8; N]>>, D::Error> {
        let m = HashMap::<u64, Vec<u8>>::deserialize(d)?;
        m.into_iter()
            .map(|(k, v)| {
                let n = v.len();
                let b: Box<[u8; N]> = v
                    .into_boxed_slice()
                    .try_into()
                    .map_err(|_| serde::de::Error::invalid_length(n, &"a block"))?;
                Ok((k, b))
            })
            .collect()
    }
}

/// `Box<[u8; N]>` for any `N`.
pub mod boxed {
    use super::*;

    pub fn serialize<S: Serializer, const N: usize>(v: &[u8; N], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<Box<[u8; N]>, D::Error> {
        let v = Vec::<u8>::deserialize(d)?;
        let n = v.len();
        v.into_boxed_slice()
            .try_into()
            .map_err(|_| serde::de::Error::invalid_length(n, &"the array's length"))
    }
}

/// A small [`crate::mem::Ram`] beside DRAM — the bootcode's L2 — whole.
pub mod small_ram {
    use super::*;
    use crate::mem::Ram;

    pub fn serialize<S: Serializer>(v: &Option<Ram>, s: S) -> Result<S::Ok, S::Error> {
        let whole = v.as_ref().map(|r| {
            let bytes = r
                .read_slice(r.base(), r.len() as usize)
                .map(|b| b.into_owned())
                .unwrap_or_default();
            (r.base(), r.len(), bytes)
        });
        whole.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Ram>, D::Error> {
        let whole = Option::<(u32, u64, Vec<u8>)>::deserialize(d)?;
        Ok(whole.map(|(base, len, bytes)| {
            let mut r = Ram::new(base, len);
            let _ = r.write_slice(base, &bytes);
            r
        }))
    }
}

use crate::emulator::Emulator;
use crate::periph::sdcard::SdCard;
use anyhow::{bail, ensure, Context, Result};
use std::io::{Read, Write};

const MAGIC: &[u8; 8] = b"PIMUSNAP";
const VERSION: u32 = 1;
const PAGE: usize = 4096;

/// The cards in the machine's three slots — EMMC2, the legacy EMMC, and the
/// WiFi chip's parking place — which `Option` keeps out of an in-place load.
fn slots(emu: &mut Emulator) -> [&mut Option<SdCard>; 3] {
    let m = &mut emu.machine;
    let (emmc2, emmc, wifi) = m.card_slots();
    [emmc2, emmc, wifi]
}

fn section(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Save `emu`, which stopped with the ARM's release pending
/// ([`crate::emulator::RunEnd::Released`]), with the console bytes printed
/// so far, to `path`.
pub fn save(path: &std::path::Path, emu: &mut Emulator, console: &[u8]) -> Result<()> {
    let out = to_bytes(emu, console)?;
    let mut f =
        std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    f.write_all(&out)?;
    Ok(())
}

/// [`save`], into memory.
pub fn to_bytes(emu: &mut Emulator, console: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    section(&mut out, &encode(&emu.machine)?);
    section(&mut out, &encode(&emu.cpu)?);
    section(&mut out, &encode(&emu.cpu1)?);
    let cards: Vec<Option<(crate::periph::sdcard::CardKind, Vec<u8>)>> = slots(emu)
        .into_iter()
        .map(|c| {
            c.as_ref()
                .map(|c| Ok::<_, bincode::Error>((c.kind(), encode(c)?)))
                .transpose()
        })
        .collect::<std::result::Result<_, _>>()?;
    section(&mut out, &encode(&cards)?);
    section(&mut out, console);
    let mut pages = Vec::new();
    let mut count = 0u64;
    for (off, region) in emu.machine.ram.regions() {
        let crate::mem::Region::Bytes(bytes) = region else {
            continue;
        };
        for (i, page) in bytes.chunks(PAGE).enumerate() {
            if page.iter().any(|&b| b != 0) {
                pages.extend_from_slice(&(off + (i * PAGE) as u64).to_le_bytes());
                pages.extend_from_slice(&(page.len() as u32).to_le_bytes());
                pages.extend_from_slice(page);
                count += 1;
            }
        }
    }
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&pages);
    Ok(out)
}

struct Reader {
    bytes: Vec<u8>,
    at: usize,
}

impl Reader {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        ensure!(self.at + n <= self.bytes.len(), "the snapshot ends early");
        let s = &self.bytes[self.at..self.at + n];
        self.at += n;
        Ok(s)
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }

    fn section(&mut self) -> Result<&[u8]> {
        let n = self.u64()? as usize;
        self.take(n)
    }
}

fn encode<T: Serialize + ?Sized>(v: &T) -> bincode::Result<Vec<u8>> {
    bincode::serialize(v)
}

fn decode<'de, T: Deserialize<'de>>(bytes: &'de [u8]) -> bincode::Result<T> {
    bincode::deserialize(bytes)
}

fn in_place<'de, T: Deserialize<'de>>(bytes: &'de [u8], place: &mut T) -> Result<()> {
    use bincode::Options;
    let options = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .allow_trailing_bytes();
    let mut de = bincode::Deserializer::from_slice(bytes, options);
    T::deserialize_in_place(&mut de, place)?;
    Ok(())
}

/// Load the snapshot at `path` into `emu`, which the same command line built
/// and nothing has run yet: the console bytes it had printed. The next
/// [`Emulator::run`] goes on from the ARM's release.
pub fn load(path: &std::path::Path, emu: &mut Emulator) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .read_to_end(&mut bytes)?;
    from_bytes(bytes, emu).with_context(|| format!("loading {}", path.display()))
}

/// [`load`], from memory.
pub fn from_bytes(bytes: Vec<u8>, emu: &mut Emulator) -> Result<Vec<u8>> {
    let mut r = Reader { bytes, at: 0 };
    ensure!(r.take(8)? == MAGIC, "not a pimu snapshot");
    let version = u32::from_le_bytes(r.take(4)?.try_into()?);
    ensure!(
        version == VERSION,
        "snapshot version {version}, this build reads {VERSION}"
    );
    let machine = r.section()?.to_vec();
    in_place(&machine, &mut emu.machine).context("the machine")?;
    let cpu = r.section()?.to_vec();
    in_place(&cpu, &mut emu.cpu).context("VPU core 0")?;
    emu.cpu1 = decode(r.section()?).context("VPU core 1")?;
    let cards: Vec<Option<(crate::periph::sdcard::CardKind, Vec<u8>)>> = decode(r.section()?)?;
    let mut fresh: Vec<SdCard> = slots(emu).into_iter().filter_map(|c| c.take()).collect();
    for (slot, saved) in slots(emu).into_iter().zip(cards) {
        let Some((kind, state)) = saved else { continue };
        let Some(i) = fresh.iter().position(|c| c.kind() == kind) else {
            bail!("the snapshot has a {kind:?} card this command line does not");
        };
        let mut card = fresh.swap_remove(i);
        in_place(&state, &mut card).context("a card")?;
        *slot = Some(card);
    }
    let console = r.section()?.to_vec();
    let count = r.u64()?;
    for _ in 0..count {
        let off = r.u64()?;
        let len = u32::from_le_bytes(r.take(4)?.try_into()?) as usize;
        let page = r.take(len)?;
        for (i, w) in page.chunks(8).enumerate() {
            let mut b = [0u8; 8];
            b[..w.len()].copy_from_slice(w);
            let v = u64::from_le_bytes(b);
            if v != 0 {
                let at = off + 8 * i as u64;
                ensure!(
                    emu.machine.ram.write_le(at, w.len() as u32, v),
                    "DRAM at {at:#x} is outside this board's"
                );
            }
        }
    }
    emu.resume_at_release = true;
    Ok(console)
}
