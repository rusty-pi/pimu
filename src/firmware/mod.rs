//! Loading firmware images into the machine.
//!
//! - [`elf32`] — `start4.elf` and the vc4boot test programs.
//! - `fixup4.dat` and `pieeprom.bin` parsing arrive with their milestones; for
//!   now [`Payload::RawBinary`] covers hand-written test payloads.

pub mod elf32;

use anyhow::{Context, Result};

use crate::bus::Bus;
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

    pub fn entry(&self) -> u32 {
        match self {
            Payload::RawBinary { entry, .. } => *entry,
            Payload::Elf(e) => e.entry,
        }
    }

    /// Copy the payload into the machine's RAM.
    pub fn load_into(&self, machine: &mut Machine) -> Result<()> {
        match self {
            Payload::RawBinary {
                load_addr, bytes, ..
            } => {
                for (i, chunk) in bytes.chunks(4).enumerate() {
                    let mut w = [0u8; 4];
                    w[..chunk.len()].copy_from_slice(chunk);
                    machine
                        .store32(load_addr + (i as u32) * 4, u32::from_le_bytes(w))
                        .map_err(|e| anyhow::anyhow!("loading raw payload: {e}"))?;
                }
            }
            Payload::Elf(elf) => {
                for (i, seg) in elf.segments.iter().enumerate() {
                    machine.ram.write_slice(seg.paddr, &seg.data).map_err(|e| {
                        anyhow::anyhow!("loading ELF segment {i} @ {:#x}: {e}", seg.paddr)
                    })?;
                    // Zero the .bss tail.
                    let bss = seg.mem_size.saturating_sub(seg.data.len() as u32);
                    if bss > 0 {
                        let start = seg.paddr + seg.data.len() as u32;
                        let zeros = vec![0u8; bss as usize];
                        machine
                            .ram
                            .write_slice(start, &zeros)
                            .map_err(|e| anyhow::anyhow!("zeroing ELF bss @ {start:#x}: {e}"))?;
                    }
                }
            }
        }
        Ok(())
    }
}
