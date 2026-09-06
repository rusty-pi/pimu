//! Loading firmware images into the machine.
//!
//! - [`elf32`] — `start4.elf` and the vc4boot test programs.
//! - [`eeprom`] — `pieeprom.bin` section table + bootcode extraction.
//! - `fixup4.dat` parsing arrives with M3.

pub mod eeprom;
pub mod elf32;

use anyhow::{Context, Result};

use crate::machine::Machine;

/// Something loadable into the machine, with a known entry point.
#[derive(Debug, Clone)]
pub enum Payload {
    /// A flat binary placed at `load_addr`; execution starts at `entry`.
    RawBinary {
        load_addr: u32,
        entry: u32,
        bytes: Vec<u8>,
    },
    /// An ELF32 image; segments go to their `p_paddr`, entry from the header.
    Elf(elf32::Elf32),
}

impl Payload {
    pub fn raw(load_addr: u32, bytes: impl Into<Vec<u8>>) -> Payload {
        Payload::RawBinary {
            load_addr,
            entry: load_addr,
            bytes: bytes.into(),
        }
    }

    pub fn from_elf_bytes(bytes: &[u8]) -> Result<Payload> {
        Ok(Payload::Elf(
            elf32::Elf32::parse(bytes).context("parsing ELF payload")?,
        ))
    }

    /// Extract the bootcode section from a `pieeprom.bin` image and stage it as
    /// the boot ROM would: body at `0x8000_0000`, entry at `+0x200`.
    pub fn from_eeprom_bytes(bytes: &[u8]) -> Result<Payload> {
        let img = eeprom::EepromImage::parse(bytes).context("parsing EEPROM image")?;
        let bc = img
            .bootcode()
            .context("EEPROM image has no bootcode section")?;
        Ok(Payload::RawBinary {
            load_addr: eeprom::BOOTCODE_LOAD_ADDR,
            entry: eeprom::BOOTCODE_LOAD_ADDR + eeprom::BOOTCODE_ENTRY_OFFSET,
            bytes: bc.body.clone(),
        })
    }

    pub fn entry(&self) -> u32 {
        match self {
            Payload::RawBinary { entry, .. } => *entry,
            Payload::Elf(e) => e.entry,
        }
    }

    /// Copy the payload into the machine's memory (address aliases folded).
    pub fn load_into(&self, machine: &mut Machine) -> Result<()> {
        match self {
            Payload::RawBinary {
                load_addr, bytes, ..
            } => {
                write_folded(machine, *load_addr, bytes)
                    .with_context(|| format!("loading raw payload @ {load_addr:#x}"))?;
            }
            Payload::Elf(elf) => {
                for (i, seg) in elf.segments.iter().enumerate() {
                    write_folded(machine, seg.paddr, &seg.data)
                        .with_context(|| format!("loading ELF segment {i} @ {:#x}", seg.paddr))?;
                    let bss = seg.mem_size.saturating_sub(seg.data.len() as u32);
                    if bss > 0 {
                        let start = seg.paddr + seg.data.len() as u32;
                        write_folded(machine, start, &vec![0u8; bss as usize])
                            .with_context(|| format!("zeroing ELF bss @ {start:#x}"))?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Write `bytes` to memory at `addr`, folding VC4 cache aliases and going
/// straight to the backing store (bypasses MMIO — loaders only ever target RAM).
fn write_folded(machine: &mut Machine, addr: u32, bytes: &[u8]) -> Result<()> {
    let phys = addr & 0x3FFF_FFFF;
    machine.ram.write_slice(phys, bytes).map_err(|e| {
        anyhow::anyhow!(
            "{e} (phys {phys:#x}, {} bytes, ram {} B)",
            bytes.len(),
            machine.ram.len()
        )
    })
}
