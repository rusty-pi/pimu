//! What `arm_loader` leaves for the ARM, and how to read it: the armstub at
//! ARM physical 0, the kernel image and the device tree (#40, milestone 2).
//!
//! ## The armstub
//!
//! `arm_loader` places an armstub at ARM physical 0 — the upstream `armstub8`
//! shape, embedded in `start4.elf` at file offset `0x1E40EC` — and fills in two
//! of its data words: `[0xf8]` (`dtb_ptr32`) and `[0xfc]` (`kernel_entry32`).
//! After the pinned firmware's boot they hold `0x2eff1e00` and `0x00200000`,
//! the same addresses its console reports (`Device tree loaded to 0x2eff1e00`,
//! `Loaded 'kernel8.img' to 0x200000`). The stub is what a real Pi 4's cores
//! run first, and it does work Linux depends on:
//!
//! ```text
//!   0x00  ARM_CONTROL = 0, core timer prescaler = 0x8000_0000   (ARM local)
//!   0x10  L2CTLR_EL1 |= 0x22           s3_1_c11_c0_2, IMPLEMENTATION DEFINED
//!   0x20  CNTFRQ_EL0 = 54_000_000,  CNTVOFF_EL2 = 0
//!   0x2c  CPTR_EL3 = 0x33ff,  SCR_EL3 = 0x5b1 (NS, HCE, RW, SMD),  ACTLR_EL3
//!   0x44  CPUECTLR_EL1 = 0x40 (SMPEN)  s3_1_c15_c2_1, IMPLEMENTATION DEFINED
//!   0x4c  bl setup_gic: GICC_CTLR = 0x1e7, GICC_PMR = 0xff, all IRQs group 1
//!   0x50  SCTLR_EL2 = 0x30c50830,  SPSR_EL3 = 0x3c9 (EL2h),  eret
//!   0x90  primary core: w4 = [0xfc], w0 = [0xf8], x1..x3 = 0, br x4
//! ```
//!
//! So a core should be started the way the SoC starts it — at physical 0, in
//! EL3 with `DAIF` masked, the architectural reset state — and the kernel is
//! then entered by the stub, at EL2 non-secure with `x0` = the dtb, which is
//! what the arm64 boot protocol asks for and what the reference board reports
//! (`CPU: All CPU(s) started at EL2`). Entering the kernel directly would skip
//! `SCR_EL3.HCE` (without it Linux's first `hvc` is UNDEFINED), `CNTFRQ` and
//! the GIC's group setup (see [`crate::periph::gic`]), each of which would then
//! have to be faked.
//!
//! The two IMPLEMENTATION DEFINED registers (`L2CTLR_EL1`, `CPUECTLR_EL1`) are
//! only written by the stub; plain storage is enough for them
//! ([`crate::aarch64::SysregMove::is_impdef`] recognises the encoding space).
//!
//! Secondary cores are released through the spin table in RAM: every `cpu@n`
//! in the handed-over tree has `enable-method = "spin-table"`,
//! `cpu-release-addr = <0 0xd8>` … `<0 0xf0>`. The stub parks them polling
//! those words.
//!
//! Found and exercised with an in-process Unicorn core on the `arm-unicorn`
//! branch (PR #36), which ran this stub and the kernel it enters up to
//! `Waiting for root device`.

use crate::bus::{BusResult, Width};
use crate::fdt::Fdt;
use crate::machine::Machine;

/// The armstub's `dtb_ptr32` and `kernel_entry32` words (module docs).
pub const STUB_DTB_PTR: u32 = 0xf8;
pub const STUB_KERNEL_ENTRY: u32 = 0xfc;

/// The arm64 `Image` header's magic, `"ARM\x64"` at `+0x38`
/// (`Documentation/arch/arm64/booting.rst`).
const IMAGE_MAGIC: u32 = 0x644d_5241;

/// What `arm_loader` left in the armstub for the primary core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handoff {
    pub kernel: u32,
    pub dtb: u32,
}

/// Read the armstub's data words out of RAM.
pub fn read_handoff(machine: &Machine) -> BusResult<Handoff> {
    Ok(Handoff {
        kernel: machine.ram.load(STUB_KERNEL_ENTRY, Width::Word)?,
        dtb: machine.ram.load(STUB_DTB_PTR, Width::Word)?,
    })
}

/// Is there an arm64 `Image` header at `addr`?
pub fn is_arm64_image(machine: &Machine, addr: u32) -> bool {
    machine.ram.load(addr + 0x38, Width::Word) == Ok(IMAGE_MAGIC)
}

/// The device tree blob at `addr`, trimmed to its header's `totalsize`.
pub fn read_dtb(machine: &Machine, addr: u32) -> Result<Vec<u8>, String> {
    let head = machine.ram.read_slice(addr, 8).map_err(|e| e.to_string())?;
    let total = u32::from_be_bytes([head[4], head[5], head[6], head[7]]) as usize;
    let blob = machine
        .ram
        .read_slice(addr, total)
        .map_err(|e| e.to_string())?
        .to_vec();
    Fdt::parse(&blob)?;
    Ok(blob)
}

/// Kernel arguments a harness wants in front of the firmware's command line:
///
/// * `earlycon` — resolves through `/chosen/stdout-path` = `"serial0:115200n8"`
///   and the `serial0` alias to the PL011 at `0x7e201000`, so the kernel
///   prints from its first line.
/// * `keep_bootcon` — otherwise the kernel drops `earlycon` the moment a real
///   console registers, which here is `tty1` (the `console=tty1` in the
///   firmware's `cmdline.txt`); everything after `printk: legacy bootconsole
///   [pl11] disabled` then goes to a framebuffer console nobody reads. The
///   price: once `ttyAMA0` registers, every line is printed twice.
/// * `kvm-arm.mode=none` — keeps KVM out of this boot. Its vgic probe used to
///   take an external abort on the GIC's virtualisation interface, which
///   [`crate::periph::gic`] models now (#58); a UEFI boot, which cannot add
///   the flag, runs KVM's bring-up, and this one keeps its transcript as it
///   was recorded.
pub const BOOTARGS: [&str; 3] = ["earlycon", "keep_bootcon", "kvm-arm.mode=none"];

/// Put `args` on the kernel command line, in the device tree the firmware
/// already placed at `dtb`, before the ARM is released. Arguments already
/// present (by name) are not added again.
///
/// Why here and not in `cmdline.txt`: the firmware echoes `cmdline.txt` into
/// its log (`Read command line from file 'cmdline.txt':`, the line itself, then
/// `brfs: File read: 97 bytes`), so a changed `cmdline.txt` moves the firmware
/// regression's golden transcript. Patching `/chosen/bootargs` after the
/// firmware is done leaves every byte it printed alone.
///
/// The blob grows by a few bytes and is rewritten in place, so the armstub's
/// `dtb_ptr32` stays what the firmware wrote. The bytes it grows into must be
/// zero — unused — or this refuses. Linux reserves the blob by the header's
/// new `totalsize`. Returns the old and new command lines.
pub fn add_bootargs(
    machine: &mut Machine,
    dtb: u32,
    args: &[&str],
) -> Result<(String, String), String> {
    let blob = read_dtb(machine, dtb)?;
    let fdt = Fdt::parse(&blob)?;
    let old = fdt
        .properties_of("/chosen")
        .and_then(|p| p.into_iter().find(|p| p.name == "bootargs"))
        .and_then(|p| p.as_str())
        .ok_or("no /chosen/bootargs to add to")?;
    let name = |a: &str| a.split('=').next().unwrap_or(a).to_string();
    let missing: Vec<&str> = args
        .iter()
        .copied()
        .filter(|a| !old.split_whitespace().any(|o| name(o) == name(a)))
        .collect();
    if missing.is_empty() {
        return Ok((old.clone(), old));
    }
    let new = format!("{} {old}", missing.join(" "));
    let mut value = new.clone().into_bytes();
    value.push(0);
    let patched = fdt.with_property("/chosen", "bootargs", &value)?;
    let grow = patched.len().saturating_sub(blob.len());
    let tail = machine
        .ram
        .read_slice(dtb + blob.len() as u32, grow)
        .map_err(|e| e.to_string())?;
    if tail.iter().any(|&b| b != 0) {
        return Err(format!(
            "the {grow} bytes after the device tree at {dtb:#x} are in use; not growing it in place"
        ));
    }
    machine
        .ram
        .write_slice(dtb, &patched)
        .map_err(|e| e.to_string())?;
    Ok((old, new))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DTB_AT: u32 = 0x10_0000;
    const RAM: usize = 32 << 20;

    /// A minimal blob: `/ { chosen { bootargs = "<args>"; }; }`.
    fn dtb(args: &str) -> Vec<u8> {
        fn tok(s: &mut Vec<u8>, v: u32) {
            s.extend_from_slice(&v.to_be_bytes());
        }
        fn pad(s: &mut Vec<u8>) {
            while !s.len().is_multiple_of(4) {
                s.push(0);
            }
        }
        let mut st = Vec::new();
        tok(&mut st, 1); // FDT_BEGIN_NODE ""
        tok(&mut st, 0);
        tok(&mut st, 1); // FDT_BEGIN_NODE "chosen"
        st.extend_from_slice(b"chosen\0");
        pad(&mut st);
        let mut v = args.as_bytes().to_vec();
        v.push(0);
        tok(&mut st, 3); // FDT_PROP
        tok(&mut st, v.len() as u32);
        tok(&mut st, 0); // nameoff of "bootargs"
        st.extend_from_slice(&v);
        pad(&mut st);
        tok(&mut st, 2); // FDT_END_NODE chosen
        tok(&mut st, 2); // FDT_END_NODE /
        tok(&mut st, 9); // FDT_END
        let strings = b"bootargs\0".to_vec();
        let (rsv_off, struct_off) = (40u32, 56u32);
        let strings_off = struct_off + st.len() as u32;
        let total = strings_off + strings.len() as u32;
        let mut b = Vec::new();
        for w in [
            0xd00d_feed,
            total,
            struct_off,
            strings_off,
            rsv_off,
            17,
            16,
            0,
            strings.len() as u32,
            st.len() as u32,
        ] {
            tok(&mut b, w);
        }
        b.extend_from_slice(&[0; 16]); // empty memory-reservation block
        b.extend_from_slice(&st);
        b.extend_from_slice(&strings);
        b
    }

    fn machine_with(blob: &[u8]) -> Machine {
        let mut m = Machine::new(RAM);
        m.ram.write_slice(DTB_AT, blob).unwrap();
        m
    }

    #[test]
    fn reads_the_handoff_words_and_the_image_magic() {
        let mut m = Machine::new(RAM);
        m.ram
            .store(STUB_KERNEL_ENTRY, Width::Word, 0x20_0000)
            .unwrap();
        m.ram.store(STUB_DTB_PTR, Width::Word, DTB_AT).unwrap();
        m.ram.store(0x20_0038, Width::Word, IMAGE_MAGIC).unwrap();
        let h = read_handoff(&m).unwrap();
        assert_eq!(
            h,
            Handoff {
                kernel: 0x20_0000,
                dtb: DTB_AT
            }
        );
        assert!(is_arm64_image(&m, h.kernel));
        assert!(!is_arm64_image(&m, DTB_AT));
    }

    #[test]
    fn prepends_missing_args_and_grows_the_blob_in_place() {
        let blob = dtb("console=ttyAMA0,115200 keep_bootcon");
        let mut m = machine_with(&blob);
        let (old, new) = add_bootargs(&mut m, DTB_AT, &BOOTARGS).unwrap();
        assert_eq!(old, "console=ttyAMA0,115200 keep_bootcon");
        assert_eq!(
            new,
            "earlycon kvm-arm.mode=none console=ttyAMA0,115200 keep_bootcon"
        );
        let after = read_dtb(&m, DTB_AT).unwrap();
        assert!(after.len() > blob.len());
        let fdt = Fdt::parse(&after).unwrap();
        let args = fdt
            .properties_of("/chosen")
            .and_then(|p| p.into_iter().find(|p| p.name == "bootargs"))
            .and_then(|p| p.as_str());
        assert_eq!(args.as_deref(), Some(new.as_str()));
        // A second pass has nothing left to add.
        let (o2, n2) = add_bootargs(&mut m, DTB_AT, &BOOTARGS).unwrap();
        assert_eq!(o2, n2);
    }

    #[test]
    fn refuses_to_grow_over_bytes_in_use() {
        let blob = dtb("console=ttyAMA0,115200");
        let mut m = machine_with(&blob);
        m.ram
            .store(DTB_AT + blob.len() as u32, Width::Word, 0xdead_beef)
            .unwrap();
        assert!(add_bootargs(&mut m, DTB_AT, &BOOTARGS).is_err());
    }
}
