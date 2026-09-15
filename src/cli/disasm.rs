//! `disasm`: a flat binary, an ELF segment or an EEPROM image's bootcode
//! through the VPU decoder.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

use rpi_virt_fw::vpu::decode::decode;
use rpi_virt_fw::vpu::length::insn_len_bytes;

use crate::parse_u32;

pub fn cmd_disasm(args: &[String]) -> Result<ExitCode> {
    let mut path: Option<PathBuf> = None;
    let mut base: u32 = 0;
    let mut count: usize = 64;
    let mut vaddr: Option<u32> = None;
    let mut lengths_only = false;
    let mut eeprom = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--base" => base = parse_u32(it.next().context("--base needs a value")?)?,
            "--count" => count = it.next().context("--count needs a value")?.parse()?,
            "--vaddr" => vaddr = Some(parse_u32(it.next().context("--vaddr needs a value")?)?),
            "--lengths" => lengths_only = true,
            "--eeprom" => eeprom = true,
            s if !s.starts_with('-') => path = Some(PathBuf::from(s)),
            s => bail!("unexpected argument '{s}'"),
        }
    }
    let path = path.context("disasm: missing <file>")?;
    let raw = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;

    // ELF: locate the segment containing `vaddr` (or the entry) and disassemble
    // from there. Flat binary: file offset 0 sits at `--base`, and `--vaddr`
    // starts that far into it.
    let (bytes, mut pc): (Vec<u8>, u32) = if eeprom {
        use rpi_virt_fw::firmware::eeprom::{
            EepromImage, BOOTCODE_ENTRY_OFFSET, BOOTCODE_LOAD_ADDR,
        };
        let img = EepromImage::parse(&raw)?;
        let bc = img
            .bootcode()
            .context("EEPROM image has no bootcode section")?;
        let target = vaddr.unwrap_or(BOOTCODE_LOAD_ADDR + BOOTCODE_ENTRY_OFFSET);
        let skip = (target - BOOTCODE_LOAD_ADDR) as usize;
        (bc.body[skip..].to_vec(), target)
    } else if raw.starts_with(b"\x7fELF") {
        let elf = rpi_virt_fw::firmware::elf32::Elf32::parse(&raw)?;
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
