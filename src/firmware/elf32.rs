//! Minimal ELF32 little-endian loader.
//!
//! `start4.elf` (and the vc4boot test programs) are ELF32-LE with a VideoCore
//! `e_machine`. We only need the program headers: iterate `PT_LOAD`, copy
//! `p_filesz` bytes to `p_paddr`, zero the rest up to `p_memsz`.

use crate::bail;
use crate::error::{Context, Result};
use alloc::vec::Vec;

const PT_LOAD: u32 = 1;

#[derive(Debug, Clone)]
pub struct Segment {
    pub paddr: u32,
    pub vaddr: u32,
    pub data: Vec<u8>,
    pub mem_size: u32,
}

#[derive(Debug, Clone)]
pub struct Elf32 {
    pub entry: u32,
    pub machine: u16,
    pub segments: Vec<Segment>,
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl Elf32 {
    pub fn parse(bytes: &[u8]) -> Result<Elf32> {
        if bytes.len() < 52 || &bytes[0..4] != b"\x7fELF" {
            bail!("not an ELF file");
        }
        if bytes[4] != 1 {
            bail!("not ELF32 (EI_CLASS = {})", bytes[4]);
        }
        if bytes[5] != 1 {
            bail!("not little-endian (EI_DATA = {})", bytes[5]);
        }

        let machine = u16le(bytes, 18);
        let entry = u32le(bytes, 24);
        let phoff = u32le(bytes, 28) as usize;
        let phentsize = u16le(bytes, 42) as usize;
        let phnum = u16le(bytes, 44) as usize;

        if phentsize < 32 {
            bail!("implausible e_phentsize {phentsize}");
        }

        let mut segments = Vec::new();
        for i in 0..phnum {
            let off = phoff + i * phentsize;
            let ph = bytes
                .get(off..off + 32)
                .with_context(|| format!("program header {i} out of range"))?;

            if u32le(ph, 0) != PT_LOAD {
                continue;
            }
            let p_offset = u32le(ph, 4) as usize;
            let p_vaddr = u32le(ph, 8);
            let p_paddr = u32le(ph, 12);
            let p_filesz = u32le(ph, 16) as usize;
            let p_memsz = u32le(ph, 20);

            let data = bytes
                .get(p_offset..p_offset + p_filesz)
                .with_context(|| {
                    format!("PT_LOAD {i} body [{p_offset:#x}..+{p_filesz:#x}] out of range")
                })?
                .to_vec();

            segments.push(Segment {
                paddr: p_paddr,
                vaddr: p_vaddr,
                data,
                mem_size: p_memsz,
            });
        }

        if segments.is_empty() {
            bail!("no PT_LOAD segments");
        }
        Ok(Elf32 {
            entry,
            machine,
            segments,
        })
    }
}
