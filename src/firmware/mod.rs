//! Loading firmware images into the machine.
//!
//! The model knows no `start4.elf` pcs, addresses or `gp` offsets, and must not
//! learn any: the bench runs different firmware builds through the same model,
//! and a baked-in address goes quietly wrong on a build where it moved. Model
//! the hardware behaviour the firmware relies on instead. Addresses the
//! diagnostics watch are fine — on another build they just print nothing.

pub mod bootrom;
pub mod eeprom;
pub mod elf32;

/// A `kernel8.img` that parks the ARM: an arm64 Image header, then
/// `msr daifset, #0xf; wfi; b .-4`. The card for a boot that ends at the
/// handover, since the ARM is always modelled and this gives it nothing to do.
pub const HALT_KERNEL: [u8; 76] = [
    0x10, 0x00, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, // b 0x40; code1
    0, 0, 0, 0, 0, 0, 0, 0, // text_offset 0
    0x4c, 0, 0, 0, 0, 0, 0, 0, // image_size
    0x0a, 0, 0, 0, 0, 0, 0, 0, // flags: LE, 4K pages, anywhere
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // res2..res4
    b'A', b'R', b'M', 0x64, 0, 0, 0, 0, // magic, res5
    0xdf, 0x4f, 0x03, 0xd5, // msr daifset, #0xf
    0x7f, 0x20, 0x03, 0xd5, // wfi
    0xff, 0xff, 0xff, 0x17, // b .-4
];

use anyhow::{Context, Result};

use crate::machine::Machine;

#[derive(Debug, Clone)]
pub enum Payload {
    RawBinary {
        load_addr: u32,
        entry: u32,
        bytes: Vec<u8>,
    },
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

/// Write `bytes` at `addr`, folding VC4 cache aliases and bypassing MMIO.
pub(crate) fn write_folded(machine: &mut Machine, addr: u32, bytes: &[u8]) -> Result<()> {
    let phys = addr & 0x3FFF_FFFF;
    machine.ram.write_slice(phys, bytes).map_err(|e| {
        anyhow::anyhow!(
            "{e} (phys {phys:#x}, {} bytes, ram {} B)",
            bytes.len(),
            machine.ram.len()
        )
    })
}
