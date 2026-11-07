//! `disasm`: a flat binary, an ELF segment or an EEPROM image's bootcode through the VPU decoder.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};

use pimu::vpu::decode::decode;
use pimu::vpu::length::insn_len_bytes;

use crate::parse_u32;

/// Disassemble a flat binary, an ELF segment or an EEPROM image's bootcode
/// with the (partial) VPU decoder.
#[derive(clap::Args)]
pub struct DisasmArgs {
    /// The file to read.
    path: PathBuf,
    /// Where offset 0 of a flat binary is.
    #[arg(long, default_value = "0", value_parser = parse_u32)]
    base: u32,
    /// How many instructions to decode.
    #[arg(long, default_value_t = 64)]
    count: usize,
    /// Where to start: by default the ELF's entry, the bootcode's or --base.
    #[arg(long, value_parser = parse_u32)]
    vaddr: Option<u32>,
    /// Print each instruction's address, length and mnemonic only.
    #[arg(long = "lengths")]
    lengths_only: bool,
    /// The file is an EEPROM image: decode its bootcode.
    #[arg(long)]
    eeprom: bool,
}

pub fn cmd_disasm(args: DisasmArgs) -> Result<ExitCode> {
    let DisasmArgs {
        path,
        base,
        count,
        vaddr,
        lengths_only,
        eeprom,
    } = args;
    let raw = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;

    // ELF: the segment holding `vaddr` (or the entry). Flat: offset 0 is at `--base`.
    let (bytes, mut pc): (Vec<u8>, u32) = if eeprom {
        use pimu::firmware::eeprom::{EepromImage, BOOTCODE_ENTRY_OFFSET, BOOTCODE_LOAD_ADDR};
        let img = EepromImage::parse(&raw)?;
        let bc = img
            .bootcode()
            .context("EEPROM image has no bootcode section")?;
        let target = vaddr.unwrap_or(BOOTCODE_LOAD_ADDR + BOOTCODE_ENTRY_OFFSET);
        let skip = (target - BOOTCODE_LOAD_ADDR) as usize;
        (bc.body[skip..].to_vec(), target)
    } else if raw.starts_with(b"\x7fELF") {
        let elf = pimu::firmware::elf32::Elf32::parse(&raw)?;
        let target = vaddr.unwrap_or(elf.entry);
        let seg = elf
            .segments
            .iter()
            .find(|s| target >= s.vaddr && (target as u64) < s.vaddr as u64 + s.data.len() as u64)
            .with_context(|| format!("no loadable segment contains vaddr {target:#x}"))?;
        let skip = (target - seg.vaddr) as usize;
        (seg.data[skip..].to_vec(), target)
    } else {
        let target = vaddr.unwrap_or(base);
        let skip = target
            .checked_sub(base)
            .map(|s| s as usize)
            .filter(|&s| s < raw.len())
            .with_context(|| {
                format!("--vaddr {target:#x} is outside the file at --base {base:#x}")
            })?;
        (raw[skip..].to_vec(), target)
    };

    let mut off = 0usize;
    for _ in 0..count {
        if off + 2 > bytes.len() {
            break;
        }
        let p0 = u16::from_le_bytes([bytes[off], bytes[off + 1]]);
        let len = insn_len_bytes(p0) as usize;
        if off + len > bytes.len() {
            println!("{pc:#010x}:  (truncated {len}-byte insn)");
            break;
        }
        let insn = decode(&bytes[off..off + len], pc);
        if lengths_only {
            println!("{pc:#010x} {len} {}", insn.op.mnemonic());
        } else {
            let hex: String = bytes[off..off + len]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            println!("{pc:#010x}:  {hex:<20}  {:?}", insn.op);
        }
        pc = pc.wrapping_add(len as u32);
        off += len;
    }
    Ok(ExitCode::SUCCESS)
}
