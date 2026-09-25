//! What `arm_loader` leaves for the ARM, and how to read it: the armstub at
//! ARM physical 0, the kernel image and the device tree.
//!
//! `arm_loader` places the upstream `armstub8` at ARM physical 0 and fills in
//! two of its data words, `[0xf8]` (`dtb_ptr32`) and `[0xfc]`
//! (`kernel_entry32`). The stub is what a real Pi 4's cores run first, and it
//! does work Linux depends on:
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
//! So a core is started the way the SoC starts it — at physical 0, in EL3 with
//! `DAIF` masked — and the stub enters the kernel at EL2 non-secure with `x0` =
//! the dtb, as the arm64 boot protocol asks. Entering the kernel directly would
//! skip `SCR_EL3.HCE` (without which Linux's first `hvc` is UNDEFINED),
//! `CNTFRQ` and the GIC's group setup, each of which would have to be faked.
//! The two IMPLEMENTATION DEFINED registers are only ever written, so plain
//! storage is enough for them.
//!
//! Secondary cores are released through the spin table in RAM: every `cpu@n` in
//! the handed-over tree has `enable-method = "spin-table"` and a
//! `cpu-release-addr` the stub parks it polling.
use crate::bus::{BusResult, Width};
use crate::fdt::Fdt;
use crate::machine::Machine;

pub const STUB_DTB_PTR: u32 = 0xf8;
pub const STUB_KERNEL_ENTRY: u32 = 0xfc;

const IMAGE_MAGIC: u32 = 0x644d_5241;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handoff {
    pub kernel: u32,
    pub dtb: u32,
}

pub fn read_handoff(machine: &Machine) -> BusResult<Handoff> {
    Ok(Handoff {
        kernel: machine.ram.load(STUB_KERNEL_ENTRY, Width::Word)?,
        dtb: machine.ram.load(STUB_DTB_PTR, Width::Word)?,
    })
}

pub fn is_arm64_image(machine: &Machine, addr: u32) -> bool {
    machine.ram.load(addr + 0x38, Width::Word) == Ok(IMAGE_MAGIC)
}

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
/// `earlycon` so the kernel prints from its first line, `keep_bootcon` so it
/// does not drop that console once `tty1` registers (at the price of every line
/// appearing twice), and `kvm-arm.mode=none` to keep the transcript as recorded
/// — a UEFI boot cannot add the flag and runs KVM's bring-up over the GIC's
/// virtualisation interface instead.
pub const BOOTARGS: [&str; 3] = ["earlycon", "keep_bootcon", "kvm-arm.mode=none"];

/// Put `args` on the kernel command line in the device tree at `dtb`, before
/// the ARM is released. Not via `cmdline.txt`, which the firmware echoes into
/// its log, where a change would move the golden transcript. The blob grows in
/// place, so `dtb_ptr32` stays what the firmware wrote and the bytes grown into
/// must be zero.
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
