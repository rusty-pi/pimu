//! The in-process ARM core: a Unicorn 2 (QEMU TCG) Cortex-A72 that runs the
//! kernel `start4.elf` hands over to, against the model's own RAM and
//! peripherals (`docs/arm-unicorn.md`, `recon --arm`). Behind the `arm` cargo
//! feature, because Unicorn is a C library with a cmake + libclang build.
//!
//! ## Entry: the firmware's own armstub, at EL3
//!
//! `arm_loader` leaves an armstub at ARM physical 0 — the upstream `armstub8`
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
//! So the core is started the way the SoC starts it — at physical 0, in EL3
//! with `DAIF` masked, the architectural reset state — and the kernel is
//! entered by the stub, at EL2 non-secure with `x0` = the dtb, which is what
//! the arm64 boot protocol asks for and what the reference board reports
//! (`CPU: All CPU(s) started at EL2`). Entering the kernel directly would skip
//! `SCR_EL3.HCE` (without it Linux's first `hvc` is UNDEFINED), `CNTFRQ` and
//! the GIC's group setup, each of which would then have to be faked here.
//!
//! Unicorn's A72 has EL2 and EL3 (`ID_AA64PFR0 = 0x2222`), but Unicorn forces
//! `PSTATE` to EL1h at the end of CPU init for backward compatibility
//! (`qemu/target/arm/cpu64.c`, `env->pstate = PSTATE_MODE_EL1h`). Writing
//! `PSTATE` back to EL3h works only once QEMU's cached `hflags` are rebuilt,
//! which Unicorn does after a coprocessor-register write and not after a
//! `PSTATE` write — see [`rebuild_hflags`].
//!
//! Its QEMU also has none of the Cortex-A72's IMPLEMENTATION DEFINED
//! registers, so the stub's `L2CTLR_EL1` / `CPUECTLR_EL1` accesses are
//! UNDEFINED there. They are emulated as plain storage on the
//! undefined-instruction path ([`ArmSide::impdef`]).
//!
//! ## System registers: the `MRS`/`MSR` hook, registered by hand
//!
//! The generic timer has to be answered here (see [`GenericTimer`] for why),
//! and the way to see a system-register access is Unicorn's `UC_HOOK_INSN`
//! for `UC_ARM64_INS_MRS` / `MSR`: the translator emits a call to the hook
//! before the access and branches over the access when the hook returns
//! non-zero (`qemu/target/arm/translate-a64.c`, `gen_hook_sys` /
//! `handle_sys`).
//!
//! The Rust binding's wrapper for it cannot be used. The C callback type
//! returns `uint32_t` (`uc_cb_insn_sys_t`, `include/unicorn/arm64.h`), but the
//! binding's trampoline (`insn_sys_hook_proxy_arm64`) returns a Rust `bool`,
//! which on x86-64 sets only `al`: the C side reads whatever the rest of
//! `eax` held, and a hook that meant "let it through" skips the access
//! whenever that garbage is non-zero. That is what an earlier probe here saw
//! as "an MSR the hook lets through is dropped" (`msr cntfrq_el0` then read
//! back QEMU's 62.5 MHz default). [`ffi`] registers [`on_mrs`] / [`on_msr`]
//! directly with the right return type, and a test pins that a passed-through
//! `MSR` now lands.
//!
//! ## Exceptions and interrupts are taken by hand
//!
//! Unicorn never takes an exception into the guest. `cpu_handle_exception`
//! (`qemu/accel/tcg/cpu-exec.c`) hands every one to the `UC_HOOK_INTR` hooks
//! and then discards it; with no hook it halts with `UC_ERR_EXCEPTION`. Linux
//! needs them before `start_kernel` — `finalise_el2` issues `hvc #0` to its
//! own EL2 stub — so [`on_exception`] does the architectural entry itself
//! (`ELR`/`SPSR`/`ESR` of the target EL, `PSTATE` = ELxh with `DAIF` masked,
//! `PC` = `VBAR` + the vector offset; ARM ARM D1.10). The syndrome is rebuilt
//! from the instruction, because Unicorn does not expose QEMU's: that covers
//! SVC, HVC, SMC, BRK and UNDEFINED. An abort cannot be rebuilt that way (the
//! fault address and status are QEMU-internal), so one stops the core and is
//! reported instead of guessed at. `eret` needs nothing: QEMU executes it.
//!
//! Unicorn has no interrupt input either, so IRQs are entered the same way,
//! from outside: [`ArmCore::run`] asks the [`Gic`] what it is signalling to
//! CPU 0 and, if the core would take it now ([`Route::target`], ARM ARM
//! D1.13.4 — routing by `SCR_EL3.IRQ` / `HCR_EL2.{IMO,TGE}`, masking by
//! `PSTATE.I` only when the target is the current EL), enters the IRQ vector
//! (`+0x80` from the synchronous one). The GIC keeps signalling until Linux
//! reads `GICC_IAR`; the entry masks `PSTATE.I`, so it is not entered twice.
//!
//! "Now" has to be soon after it becomes true, or the kernel livelocks: its
//! idle loop runs `wfi` with IRQs masked and unmasks for a few instructions
//! after waking (`default_idle_call`: `cpu_do_idle()` then
//! `raw_local_irq_enable()`). QEMU ends the translation block at every
//! `msr daifclr` ("exit the cpu loop to re-evaluate pending IRQs"), so the
//! block hook [`on_block`] sees the unmask at the next block and stops the
//! slice there when an interrupt is waiting.
//!
//! ## Memory and MMIO
//!
//! * RAM: the `memory` node of the device tree the firmware hands over, mapped
//!   with `mem_map_ptr` straight onto the model's [`crate::mem::Ram`], so a
//!   store from either core is the other's next load. See [`ArmCore::new`] for
//!   the invariant that keeps that sound.
//! * `0xFC00_0000..0xFF80_0000`: the dtb's `/soc` `ranges` put the bus's
//!   `0x7C00_0000` and `0x7E00_0000` windows here, one constant offset away
//!   from the VPU's view. One `mmio_map` translates and dispatches into the
//!   same [`Machine`], so the PL011, the mailbox and the rest answer both cores
//!   from one state. An address no VPU-side device decodes is refused rather
//!   than folded into RAM the way the VPU's own bus would.
//! * `0xFF80_0000..0x1_0000_0000` (`ranges` `<0x40000000 … 0xff800000
//!   0x800000>`): the ARM-local block ([`ArmLocal`]) and the GIC-400 ([`Gic`]).
//!   The VPU never sees either, so they live here, not in [`Machine`]. GIC
//!   accesses carry the security state (EL3 = secure), because the armstub
//!   programs the secure view and Linux the non-secure one.
//!
//! No model can signal a bus error back into the guest (Unicorn has no way to
//! raise an external abort from an MMIO callback), so an access a model
//! refuses reads as zero and is recorded in [`ArmSide::mmio_faults`].
//!
//! ## Time and scheduling
//!
//! Everything runs on one modelled clock, [`ArmSide::cycles`]: an instruction
//! is a cycle (at [`gentimer::ARM_HZ`]), and a `wfi` adds the cycles it slept.
//! Nothing reads the host clock, so two runs with the same inputs are the
//! same run, timestamps included.
//!
//! Instructions are counted per translation block, by [`on_block`] at block
//! entry (every A64 instruction is 4 bytes). That hook runs before the block's
//! own exit check (`qemu/accel/tcg/translator.c`: `gen_uc_tracecode` for
//! `UC_HOOK_BLOCK` precedes `gen_tb_start`), so when it calls `emu_stop` the
//! block does not run and is not counted. This replaces `emu_start`'s
//! instruction count, which Unicorn implements as a code hook on every
//! instruction (`uc.c`, `hook_count_cb`). A system-register read in the middle
//! of a block sees the whole block counted — fine, since only monotonicity and
//! reproducibility matter.
//!
//! [`ArmCore::run`] runs the ARM until the next point anything can change:
//! the VPU's turn (every [`Schedule::arm_slice`] cycles it gets
//! [`Schedule::vpu_slice`] steps, so the firmware keeps servicing the
//! mailbox), a generic-timer compare firing, or an interrupt becoming
//! takeable. Between those it drives the interrupt lines and takes an IRQ.
//!
//! `wfi` makes Unicorn return from `emu_start` (QEMU's `helper_wfi` halts,
//! since nothing ever gives it pending work). The run loop then advances the
//! clock straight to whatever can wake the core — the next timer compare, or
//! the VPU's turn, since the mailbox is the other interrupt source — unless an
//! interrupt is already pending, which wakes a `wfi` whether or not it is
//! masked (ARM ARM D1.16.2). `emu_start` clears QEMU's halted state
//! (`resume_all_vcpus`), so the core resumes after the `wfi`.
//!
//! ## Known gaps
//!
//! * One core. Linux's secondaries wait on the spin table at `0xd8` and are
//!   never released; it gives up on them after its own timeout.
//! * The PL011's interrupt is not wired: the model's `RIS`/`MIS` read zero and
//!   the firmware reads the same registers, so modelling them belongs with the
//!   firmware regression. Linux's console writes poll `FR` and do not need it.
//! * The generic timer's EL0/EL1 access traps (`CNTKCTL_EL1`, `CNTHCTL_EL2`)
//!   are not modelled: every EL can read the counter.
//! * Code the VPU writes into RAM the ARM has already translated is not seen
//!   by the ARM: QEMU tracks self-modifying code only for its own guest's
//!   stores. Nothing does that today.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;
use std::io::Write as _;
use std::time::{Duration, Instant};

use unicorn_engine::unicorn_const::{Arch, Mode, Prot};
use unicorn_engine::{
    uc_error, Arm64CpuModel, HookType, MemType, RegisterARM64, RegisterARM64CP, Unicorn,
};

use crate::bus::{Bus, BusError, BusResult, MmioDevice, Width};
use crate::emulator::{Emulator, RunEnd, RunLimits};
use crate::fdt::Fdt;
use crate::machine::Machine;
use crate::periph::gentimer::{self, GenericTimer, Which};
use crate::periph::gic::{self, Accessor, Signal};
use crate::periph::{armlocal, mbox, ArmLocal, Gic};
use crate::soc::bcm2711::{EMMC2_BASE, EMMC2_SIZE};

pub use crate::periph::gentimer::ARM_HZ;

/// The armstub's `dtb_ptr32` and `kernel_entry32` words (module docs).
pub const STUB_DTB_PTR: u32 = 0xf8;
pub const STUB_KERNEL_ENTRY: u32 = 0xfc;

/// The arm64 `Image` header's magic, `"ARM\x64"` at `+0x38`
/// (`Documentation/arch/arm64/booting.rst`).
const IMAGE_MAGIC: u32 = 0x644d_5241;

/// ARM view of the peripheral windows, and the bus address it starts at.
const PERIPH_ARM: u64 = 0xFC00_0000;
const PERIPH_SIZE: u64 = 0x0380_0000;
const PERIPH_BUS: u32 = 0x7C00_0000;
/// ARM-local block + GIC-400.
const LOCAL_ARM: u64 = armlocal::BASE as u64;
const LOCAL_SIZE: u64 = 0x0080_0000;
const GIC_OFF: u64 = (gic::BASE - armlocal::BASE) as u64;

/// QEMU's exception numbers (`qemu/target/arm/cpu.h`), as `UC_HOOK_INTR`
/// reports them.
const EXCP_UDEF: u32 = 1;
const EXCP_SWI: u32 = 2;
const EXCP_BKPT: u32 = 7;
const EXCP_HVC: u32 = 11;
const EXCP_SMC: u32 = 13;

/// ESR_ELx exception classes (ARM ARM D17.2.37) and the IL bit (32-bit insn).
const EC_UNKNOWN: u64 = 0x00;
const EC_SVC64: u64 = 0x15;
const EC_HVC64: u64 = 0x16;
const EC_SMC64: u64 = 0x17;
const EC_BRK64: u64 = 0x3C;
const ESR_IL: u64 = 1 << 25;

/// Offsets of the exception kinds within each group of four vectors
/// (ARM ARM D1.10.2).
const VECTOR_SYNC: u64 = 0x000;
const VECTOR_IRQ: u64 = 0x080;
const VECTOR_FIQ: u64 = 0x100;

/// `PSTATE.{D,A,I,F}`, and `M` for ELx with `SP_ELx`.
const PSTATE_DAIF: u64 = 0xF << 6;
const PSTATE_I: u64 = 1 << 7;
const PSTATE_F: u64 = 1 << 6;
fn pstate_elh(el: u32) -> u64 {
    (u64::from(el) << 2) | 1
}

/// Interrupt routing bits (ARM ARM D17.2.117 `SCR_EL3`, D17.2.48 `HCR_EL2`).
const SCR_NS: u64 = 1 << 0;
const SCR_IRQ: u64 = 1 << 1;
const SCR_FIQ: u64 = 1 << 2;
const HCR_FMO: u64 = 1 << 3;
const HCR_IMO: u64 = 1 << 4;
const HCR_TGE: u64 = 1 << 27;

/// `wfi`.
const INSN_WFI: u32 = 0xd503_207f;

/// How many recent MMIO accesses to keep for the "where did it stop" report.
const RECENT_MMIO: usize = 32;
/// Exceptions kept for the report: the first few (the boot's `hvc`s), and the
/// most recent ones (where it ended up).
const FIRST_EXCEPTIONS: usize = 8;
const RECENT_EXCEPTIONS: usize = 24;

const XREGS: [RegisterARM64; 31] = [
    RegisterARM64::X0,
    RegisterARM64::X1,
    RegisterARM64::X2,
    RegisterARM64::X3,
    RegisterARM64::X4,
    RegisterARM64::X5,
    RegisterARM64::X6,
    RegisterARM64::X7,
    RegisterARM64::X8,
    RegisterARM64::X9,
    RegisterARM64::X10,
    RegisterARM64::X11,
    RegisterARM64::X12,
    RegisterARM64::X13,
    RegisterARM64::X14,
    RegisterARM64::X15,
    RegisterARM64::X16,
    RegisterARM64::X17,
    RegisterARM64::X18,
    RegisterARM64::X19,
    RegisterARM64::X20,
    RegisterARM64::X21,
    RegisterARM64::X22,
    RegisterARM64::X23,
    RegisterARM64::X24,
    RegisterARM64::X25,
    RegisterARM64::X26,
    RegisterARM64::X27,
    RegisterARM64::X28,
    RegisterARM64::X29,
    RegisterARM64::X30,
];

/// The slice of Unicorn's C API the `MRS`/`MSR` hook needs, declared here
/// because the binding's own wrapper has the wrong return type (module docs).
/// The library itself is linked by `unicorn-engine-sys`.
mod ffi {
    use std::ffi::c_void;

    /// `include/unicorn/unicorn.h`: `UC_HOOK_INSN = 1 << 1`.
    pub const UC_HOOK_INSN: i32 = 1 << 1;
    /// `include/unicorn/arm64.h`, `uc_arm64_insn`.
    pub const UC_ARM64_INS_MRS: i32 = 1;
    pub const UC_ARM64_INS_MSR: i32 = 2;
    /// `uc_arm64_reg`: `INVALID, X29, X30, NZCV, SP, WSP, WZR, XZR`. The hook
    /// passes it as `Rt` for register 31.
    pub const UC_ARM64_REG_XZR: i32 = 7;

    /// `uc_arm64_cp_reg`.
    #[repr(C)]
    pub struct CpReg {
        pub crn: u32,
        pub crm: u32,
        pub op0: u32,
        pub op1: u32,
        pub op2: u32,
        pub val: u64,
    }

    /// `uc_cb_insn_sys_t`: note the `uint32_t` return.
    pub type SysHook = extern "C" fn(*mut c_void, i32, *const CpReg, *mut c_void) -> u32;

    extern "C" {
        pub fn uc_hook_add(
            uc: *mut c_void,
            hh: *mut usize,
            kind: i32,
            callback: *mut c_void,
            user_data: *mut c_void,
            begin: u64,
            end: u64,
            ...
        ) -> i32;
        pub fn uc_reg_write(uc: *mut c_void, regid: i32, value: *const c_void) -> i32;
    }
}

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

/// What the harness puts in front of the firmware's kernel command line:
///
/// * `earlycon` — resolves through `/chosen/stdout-path` = `"serial0:115200n8"`
///   and the `serial0` alias to the PL011 at `0x7e201000`, so the kernel
///   prints from its first line.
/// * `keep_bootcon` — otherwise the kernel drops `earlycon` the moment a real
///   console registers, which here is `tty1` (the `console=tty1` in the
///   firmware's `cmdline.txt`): milestone 1's output ended at `printk: legacy
///   bootconsole [pl11] disabled`, with everything after it going to a
///   framebuffer console nobody reads.
/// * `kvm-arm.mode=none` — KVM's vgic probe reads the GIC's virtualisation
///   interface, which [`Gic`] deliberately faults rather than invent.
pub const BOOTARGS: [&str; 3] = ["earlycon", "keep_bootcon", "kvm-arm.mode=none"];

/// Put [`BOOTARGS`] on the kernel command line, in the device tree the
/// firmware already placed, before the ARM is released. Arguments already
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

/// One ARM-side MMIO access, for the report.
#[derive(Debug, Clone, Copy)]
pub struct MmioAccess {
    pub addr: u64,
    pub size: usize,
    pub value: u64,
    pub write: bool,
}

/// An exception or interrupt the core took (by hand, see the module docs).
#[derive(Debug, Clone)]
pub struct ExceptionTaken {
    pub kind: &'static str,
    pub pc: u64,
    pub from_el: u32,
    pub to_el: u32,
    /// The syndrome written; 0 for an interrupt, which writes none.
    pub esr: u64,
    /// The interrupt's INTID, for an IRQ or FIQ.
    pub intid: Option<u32>,
    pub vector: u64,
    /// [`ArmSide::cycles`] when it was taken.
    pub cycle: u64,
}

/// Why the ARM stopped.
#[derive(Debug, Clone)]
pub enum ArmStop {
    /// An exception whose syndrome cannot be rebuilt here — an abort, or an
    /// interrupt Unicorn raised on its own. `intno` is QEMU's `EXCP_*`
    /// (3 = prefetch abort, 4 = data abort).
    Unhandled { intno: u32, pc: u64, el: u32 },
    /// Unicorn gave up: an unmapped physical access, a failed register write.
    Engine {
        error: String,
        pc: u64,
        unmapped: Option<(MemType, u64, usize)>,
    },
}

/// Where the GIC's signal goes, from the routing controls (module docs).
#[derive(Debug, Clone, Copy, Default)]
struct Route {
    scr: u64,
    hcr: u64,
}

impl Route {
    fn read(uc: &Unicorn<'_, ArmSide>) -> Result<Route, uc_error> {
        // SCR_EL3 = S3_6_C1_C1_0, HCR_EL2 = S3_4_C1_C1_0.
        let rd = |op1| {
            let mut cp = RegisterARM64CP {
                crn: 1,
                crm: 1,
                op0: 3,
                op1,
                op2: 0,
                val: 0,
            };
            uc.reg_read_arm64_coproc(&mut cp).map(|()| cp.val)
        };
        Ok(Route {
            scr: rd(6)?,
            hcr: rd(4)?,
        })
    }

    /// The EL an IRQ (or FIQ) is taken to from a core in `pstate`, or `None`
    /// if it stays pending there (ARM ARM D1.13.4). `SCR_EL3.IRQ` sends it to
    /// EL3; from non-secure EL0/EL1, `HCR_EL2.IMO` or `TGE` send it to EL2;
    /// otherwise it goes to the current EL, but never below EL1. Only an
    /// interrupt to the current EL is masked by `PSTATE.I`; one to a higher EL
    /// is taken regardless, and one to a lower EL waits.
    fn target(&self, pstate: u64, fiq: bool) -> Option<u32> {
        let el = ((pstate >> 2) & 3) as u32;
        let (scr_bit, hcr_bit, mask) = if fiq {
            (SCR_FIQ, HCR_FMO, PSTATE_F)
        } else {
            (SCR_IRQ, HCR_IMO, PSTATE_I)
        };
        let to = if self.scr & scr_bit != 0 {
            3
        } else if el < 2 && self.scr & SCR_NS != 0 && self.hcr & (hcr_bit | HCR_TGE) != 0 {
            2
        } else {
            el.max(1)
        };
        match to.cmp(&el) {
            std::cmp::Ordering::Less => None,
            std::cmp::Ordering::Equal => (pstate & mask == 0).then_some(to),
            std::cmp::Ordering::Greater => Some(to),
        }
    }
}

/// Everything the ARM's callbacks reach: the VPU model and its bus, and the
/// devices only the ARM sees. It is Unicorn's user data, so `mmio_map` and hook
/// callbacks get it as `uc.get_data_mut()`.
pub struct ArmSide {
    pub emu: Emulator,
    pub gic: Gic,
    pub local: ArmLocal,
    pub timer: GenericTimer,
    /// The modelled clock: instructions executed plus cycles slept in `wfi`
    /// (module docs, "Time and scheduling").
    pub cycles: u64,
    /// The part of [`Self::cycles`] slept in `wfi`.
    pub slept: u64,
    /// `wfi`s the core halted in.
    pub wfis: u64,
    /// Translation-cache flushes after eMMC2 DMA wrote RAM ([`Self::code_dirty`]).
    pub tb_flushes: u64,
    /// Interrupts taken, by INTID.
    pub irqs: BTreeMap<u32, u64>,
    /// Generic-timer register accesses answered by [`on_mrs`] / [`on_msr`].
    pub timer_accesses: u64,
    /// The A72's IMPLEMENTATION DEFINED system registers Unicorn lacks
    /// (`op0 = 3`, `CRn` 11 or 15), as plain storage keyed by encoding. Reset
    /// values are not modelled: the armstub writes the two it touches before
    /// anything reads them, and nothing after it reads them at all.
    pub impdef: BTreeMap<String, u64>,
    pub mmio_faults: Vec<String>,
    pub recent_mmio: VecDeque<MmioAccess>,
    pub first_exceptions: Vec<ExceptionTaken>,
    pub recent_exceptions: VecDeque<ExceptionTaken>,
    pub exceptions_taken: u64,
    /// What the GIC is signalling to CPU 0, refreshed whenever something that
    /// can change it happens (a line moves, a GIC register is touched), and
    /// the routing that decides whether the core takes it.
    signal: Option<Signal>,
    route: Route,
    /// The levels last driven onto the four timer PPIs ([`Which::ALL`] order),
    /// the mailbox SPI and the eMMC2 SPI.
    lines: [bool; 6],
    /// The eMMC2 DMA engine wrote RAM since the last slice. It writes the
    /// mapped host memory directly, which QEMU does not see, so code it had
    /// translated from those pages would outlive them; the next slice starts
    /// with the translation cache flushed.
    code_dirty: bool,
    /// [`on_block`] stops the slice at the first block that starts at or
    /// after this cycle.
    stop_at: u64,
    hit_stop: bool,
    stop: Option<ArmStop>,
    unmapped: Option<(MemType, u64, usize)>,
}

impl ArmSide {
    fn record(&mut self, addr: u64, size: usize, value: u64, write: bool) {
        if self.recent_mmio.len() == RECENT_MMIO {
            self.recent_mmio.pop_front();
        }
        self.recent_mmio.push_back(MmioAccess {
            addr,
            size,
            value,
            write,
        });
    }

    fn record_exception(&mut self, e: ExceptionTaken) {
        self.exceptions_taken += 1;
        if self.first_exceptions.len() < FIRST_EXCEPTIONS {
            self.first_exceptions.push(e.clone());
        }
        if self.recent_exceptions.len() == RECENT_EXCEPTIONS {
            self.recent_exceptions.pop_front();
        }
        self.recent_exceptions.push_back(e);
    }

    /// Drive every interrupt line the ARM sees from its source's current
    /// state — the four generic timers at [`Self::cycles`], the mailbox — and
    /// refresh what the GIC signals.
    fn sync_lines(&mut self) {
        for (i, &w) in Which::ALL.iter().enumerate() {
            let level = self.timer.line(w, self.cycles);
            if level != self.lines[i] {
                self.lines[i] = level;
                self.gic.set_ppi_level(0, w.intid(), level);
            }
        }
        let level = self.emu.machine.mbox.arm_irq_asserted();
        if level != self.lines[4] {
            self.lines[4] = level;
            self.gic.set_spi_level(gic::ID_MAILBOX, level);
        }
        let level = self.emu.machine.emmc2.irq_asserted();
        if level != self.lines[5] {
            self.lines[5] = level;
            self.gic.set_spi_level(gic::ID_EMMC2, level);
        }
        self.signal = self.gic.signal(0);
    }

    /// Log an access and hand back what the guest sees: the value read, or
    /// zero for a store or an access that was refused.
    fn finish(
        &mut self,
        addr: u64,
        size: usize,
        write: Option<u64>,
        r: Result<u64, String>,
    ) -> u64 {
        match r {
            Ok(v) => {
                self.record(addr, size, v, write.is_some());
                if write.is_some() {
                    0
                } else {
                    v
                }
            }
            Err(why) => {
                self.record(addr, size, write.unwrap_or(0), write.is_some());
                let line = format!(
                    "{} {size}B @ {addr:#010x}: {why}",
                    if write.is_some() { "store" } else { "load" }
                );
                if self.mmio_faults.len() < 20 {
                    eprintln!("[arm-mmio] {line}");
                }
                self.mmio_faults.push(line);
                0
            }
        }
    }
}

fn uce(e: uc_error) -> String {
    format!("unicorn: {e:?}")
}

/// Unicorn keeps QEMU's cached translation flags (`hflags`, which carry the
/// current EL) stale after a `PSTATE` write, and rebuilds them after a
/// coprocessor-register write (`qemu/target/arm/unicorn_aarch64.c`,
/// `UC_ARM64_REG_CP_REG`). Rewriting `TPIDR_EL0` with its own value is the
/// side-effect-free way to ask for that. Without it, code after an EL change
/// runs with the old EL's permissions.
fn rebuild_hflags(uc: &mut Unicorn<'_, ArmSide>) -> Result<(), uc_error> {
    let mut cp = RegisterARM64CP {
        crn: 13,
        crm: 0,
        op0: 3,
        op1: 3,
        op2: 2,
        val: 0,
    };
    uc.reg_read_arm64_coproc(&mut cp)?;
    uc.reg_write_arm64_coproc(&cp)
}

fn current_el(uc: &Unicorn<'_, ArmSide>) -> u32 {
    uc.reg_read(RegisterARM64::PSTATE)
        .map_or(0, |p| ((p >> 2) & 3) as u32)
}

/// Enter an exception at `to_el` the way the hardware would (ARM ARM D1.10.2):
/// save `PSTATE` to `SPSR_ELx` and the return address to `ELR_ELx`, record the
/// syndrome in `ESR_ELx` (synchronous exceptions only), switch to ELxh with
/// `DAIF` masked, and branch to the vector — `kind` is [`VECTOR_SYNC`],
/// [`VECTOR_IRQ`] or [`VECTOR_FIQ`]. Returns the vector address.
fn take_exception(
    uc: &mut Unicorn<'_, ArmSide>,
    to_el: u32,
    kind: u64,
    esr: Option<u64>,
    ret: u64,
) -> Result<u64, uc_error> {
    use RegisterARM64 as R;
    let t = to_el as usize;
    let old = uc.reg_read(R::PSTATE)?;
    let from_el = ((old >> 2) & 3) as usize;
    let spsel = old & 1 == 1;
    // QEMU keeps the live stack pointer in one register and banks it per EL
    // only on its own exception entry/return (`aarch64_save_sp` /
    // `aarch64_restore_sp`), which this path bypasses — so bank it here, or
    // the `eret` back would restore a stale SP.
    let banks = [R::SP_EL0, R::SP_EL1, R::SP_EL2, R::SP_EL3];
    let sp = uc.reg_read(R::SP)?;
    uc.reg_write(banks[if spsel { from_el } else { 0 }], sp)?;

    let vbar = uc.reg_read([R::VBAR_EL0, R::VBAR_EL1, R::VBAR_EL2, R::VBAR_EL3][t])?;
    // The group of four: +0x000 current EL with SP_EL0, +0x200 current EL
    // with SP_ELx, +0x400 lower EL using AArch64 (everything here is AArch64).
    let group = match (t == from_el, spsel) {
        (true, false) => 0x000,
        (true, true) => 0x200,
        (false, _) => 0x400,
    };
    uc.reg_write([R::ELR_EL0, R::ELR_EL1, R::ELR_EL2, R::ELR_EL3][t], ret)?;
    if let Some(esr) = esr {
        uc.reg_write([R::ESR_EL0, R::ESR_EL1, R::ESR_EL2, R::ESR_EL3][t], esr)?;
    }
    uc.reg_write(R::PSTATE, PSTATE_DAIF | pstate_elh(to_el))?;
    // SPSR_ELx has no Unicorn register id: through the coprocessor interface
    // (`S3_<0|4|6>_C4_C0_0`), which also rebuilds `hflags` for the new EL.
    uc.reg_write_arm64_coproc(&RegisterARM64CP {
        crn: 4,
        crm: 0,
        op0: 3,
        op1: [0, 0, 4, 6][t],
        op2: 0,
        val: old,
    })?;
    let new_sp = uc.reg_read(banks[t])?;
    uc.reg_write(R::SP, new_sp)?;
    let vector = vbar + group + kind;
    uc.set_pc(vector)?;
    Ok(vector)
}

/// Take the interrupt the GIC is signalling, if the core would take it now.
/// Returns its INTID. The preferred return address of an interrupt is the
/// next instruction to execute — the current `PC`, which for a core halted in
/// `wfi` is the instruction after it.
fn take_interrupt(uc: &mut Unicorn<'_, ArmSide>) -> Result<Option<u32>, uc_error> {
    let (signal, route) = {
        let side = uc.get_data();
        (side.signal, side.route)
    };
    let Some(sig) = signal else { return Ok(None) };
    let pstate = uc.reg_read(RegisterARM64::PSTATE)?;
    let Some(to_el) = route.target(pstate, sig.fiq) else {
        return Ok(None);
    };
    let pc = uc.pc_read()?;
    let kind = if sig.fiq { VECTOR_FIQ } else { VECTOR_IRQ };
    let vector = take_exception(uc, to_el, kind, None, pc)?;
    let side = uc.get_data_mut();
    *side.irqs.entry(sig.intid).or_default() += 1;
    let cycle = side.cycles;
    side.record_exception(ExceptionTaken {
        kind: if sig.fiq { "fiq" } else { "irq" },
        pc,
        from_el: ((pstate >> 2) & 3) as u32,
        to_el,
        esr: 0,
        intid: Some(sig.intid),
        vector,
        cycle,
    });
    Ok(Some(sig.intid))
}

/// A decoded `MRS`/`MSR` (register) instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SysregMove {
    read: bool,
    op0: u32,
    op1: u32,
    crn: u32,
    crm: u32,
    op2: u32,
    rt: u32,
}

impl SysregMove {
    /// `1101010100 L 1 o0 op1 CRn CRm op2 Rt` — the `op0` 2/3 system-register
    /// moves (ARM ARM C5.2).
    fn decode(insn: u32) -> Option<SysregMove> {
        if insn & 0xFFD0_0000 != 0xD510_0000 {
            return None;
        }
        Some(SysregMove {
            read: insn & (1 << 21) != 0,
            op0: 2 | ((insn >> 19) & 1),
            op1: (insn >> 16) & 7,
            crn: (insn >> 12) & 0xF,
            crm: (insn >> 8) & 0xF,
            op2: (insn >> 5) & 7,
            rt: insn & 0x1F,
        })
    }

    /// The encoding space the architecture reserves for IMPLEMENTATION
    /// DEFINED registers (ARM ARM D12.3.2: `op0 == 3`, `CRn` 11 or 15), where
    /// the A72 keeps `L2CTLR_EL1`, `CPUECTLR_EL1` and friends.
    fn is_impdef(&self) -> bool {
        self.op0 == 3 && (self.crn == 11 || self.crn == 15)
    }

    fn name(&self) -> String {
        format!(
            "S{}_{}_C{}_C{}_{}",
            self.op0, self.op1, self.crn, self.crm, self.op2
        )
    }
}

fn fetch_insn(uc: &Unicorn<'_, ArmSide>, pc: u64) -> Option<u32> {
    let mut b = [0u8; 4];
    uc.vmem_read(pc, Prot::EXEC, &mut b)
        .or_else(|_| uc.mem_read(pc, &mut b))
        .ok()?;
    Some(u32::from_le_bytes(b))
}

/// `UC_HOOK_INTR`: every exception the guest raises lands here, and none is
/// taken unless this takes it (module docs).
fn on_exception(uc: &mut Unicorn<'_, ArmSide>, intno: u32) {
    if let Err(stop) = handle_exception(uc, intno) {
        uc.get_data_mut().stop = Some(stop);
        let _ = uc.emu_stop();
    }
}

fn handle_exception(uc: &mut Unicorn<'_, ArmSide>, intno: u32) -> Result<(), ArmStop> {
    let pc = uc.pc_read().unwrap_or(0);
    let el = current_el(uc);
    let engine = |e: uc_error| ArmStop::Engine {
        error: uce(e),
        pc,
        unmapped: None,
    };
    let unhandled = || ArmStop::Unhandled { intno, pc, el };
    // QEMU leaves `PC` at the preferred return address: the faulting
    // instruction for UNDEFINED and BRK, the next one for SVC/HVC/SMC.
    let (kind, to_el, esr) = match intno {
        EXCP_UDEF => {
            let insn = fetch_insn(uc, pc).ok_or_else(unhandled)?;
            if let Some(m) = SysregMove::decode(insn).filter(SysregMove::is_impdef) {
                return emulate_impdef(uc, m, pc).map_err(engine);
            }
            ("undefined", el.max(1), (EC_UNKNOWN << 26) | ESR_IL)
        }
        EXCP_SWI | EXCP_HVC | EXCP_SMC => {
            let insn = fetch_insn(uc, pc.wrapping_sub(4)).ok_or_else(unhandled)?;
            let imm16 = u64::from((insn >> 5) & 0xFFFF);
            let (kind, ec, to_el) = match intno {
                EXCP_SWI => ("svc", EC_SVC64, el.max(1)),
                EXCP_HVC => ("hvc", EC_HVC64, el.max(2)),
                _ => ("smc", EC_SMC64, 3),
            };
            (kind, to_el, (ec << 26) | ESR_IL | imm16)
        }
        EXCP_BKPT => {
            let insn = fetch_insn(uc, pc).ok_or_else(unhandled)?;
            let imm16 = u64::from((insn >> 5) & 0xFFFF);
            ("brk", el.max(1), (EC_BRK64 << 26) | ESR_IL | imm16)
        }
        // Aborts and anything else: the fault address and status live in
        // QEMU's `env->exception`, which Unicorn does not expose.
        _ => return Err(unhandled()),
    };
    let vector = take_exception(uc, to_el, VECTOR_SYNC, Some(esr), pc).map_err(engine)?;
    let side = uc.get_data_mut();
    let cycle = side.cycles;
    side.record_exception(ExceptionTaken {
        kind,
        pc,
        from_el: el,
        to_el,
        esr,
        intid: None,
        vector,
        cycle,
    });
    Ok(())
}

/// Execute an IMPLEMENTATION DEFINED `MRS`/`MSR` against [`ArmSide::impdef`]
/// and step past it.
fn emulate_impdef(uc: &mut Unicorn<'_, ArmSide>, m: SysregMove, pc: u64) -> Result<(), uc_error> {
    // Rt = 31 is XZR here: reads are discarded, writes store zero.
    let xt = XREGS.get(m.rt as usize).copied();
    if m.read {
        let v = uc
            .get_data_mut()
            .impdef
            .get(&m.name())
            .copied()
            .unwrap_or(0);
        if let Some(r) = xt {
            uc.reg_write(r, v)?;
        }
    } else {
        let v = match xt {
            Some(r) => uc.reg_read(r)?,
            None => 0,
        };
        uc.get_data_mut().impdef.insert(m.name(), v);
    }
    uc.set_pc(pc + 4)
}

/// The [`ArmSide`] behind a raw hook's `user_data`.
///
/// # Safety
///
/// `data` must be the pointer [`ArmCore::new`] registered: the `ArmSide`
/// inside the `Unicorn` that is calling the hook. That `ArmSide` lives in the
/// `Unicorn`'s heap-allocated inner state for as long as the hooks can fire
/// (they die with `uc_close`), and while the guest runs no other reference to
/// it is live — the crate's own hook trampolines take theirs the same way,
/// one callback at a time.
unsafe fn side_of<'a>(data: *mut c_void) -> &'a mut ArmSide {
    unsafe { &mut *data.cast::<ArmSide>() }
}

/// `MRS` hook: a generic-timer register is read from [`GenericTimer`] at the
/// current cycle and the access skipped; anything else is let through.
extern "C" fn on_mrs(uc: *mut c_void, rt: i32, cp: *const ffi::CpReg, data: *mut c_void) -> u32 {
    // SAFETY: Unicorn passes a valid `uc_arm64_cp_reg` for the duration of
    // the call (`HELPER(uc_hooksys64)`), and `data` is ours (`side_of`).
    let cp = unsafe { &*cp };
    let Some(reg) = gentimer::Reg::decode(cp.op0, cp.op1, cp.crn, cp.crm, cp.op2) else {
        return 0;
    };
    let side = unsafe { side_of(data) };
    side.timer_accesses += 1;
    let v = side.timer.read(reg, side.cycles);
    if rt != ffi::UC_ARM64_REG_XZR {
        // SAFETY: `uc` is the engine calling us; `v` outlives the call.
        // Writing an X register from a hook is what the hook API is for — the
        // helper call makes TCG reload its register globals afterwards.
        unsafe { ffi::uc_reg_write(uc, rt, (&raw const v).cast()) };
    }
    1
}

/// `MSR` hook: a generic-timer register is written to [`GenericTimer`], the
/// interrupt lines follow at once, and the access is skipped; anything else
/// is let through to QEMU.
extern "C" fn on_msr(_uc: *mut c_void, _rt: i32, cp: *const ffi::CpReg, data: *mut c_void) -> u32 {
    // SAFETY: as in `on_mrs`. `val` is `Rt`'s value (zero for XZR).
    let cp = unsafe { &*cp };
    let Some(reg) = gentimer::Reg::decode(cp.op0, cp.op1, cp.crn, cp.crm, cp.op2) else {
        return 0;
    };
    let side = unsafe { side_of(data) };
    side.timer_accesses += 1;
    side.timer.write(reg, side.cycles, cp.val);
    side.sync_lines();
    1
}

/// `UC_HOOK_BLOCK`: count the block's instructions onto the clock, or stop
/// before it — at [`ArmSide::stop_at`], or as soon as the core would take the
/// interrupt the GIC is signalling (module docs).
fn on_block(uc: &mut Unicorn<'_, ArmSide>, _addr: u64, size: u32) {
    let (due, signal, route) = {
        let side = uc.get_data();
        (side.cycles >= side.stop_at, side.signal, side.route)
    };
    let takeable = !due
        && signal.is_some_and(|s| {
            uc.reg_read(RegisterARM64::PSTATE)
                .is_ok_and(|p| route.target(p, s.fiq).is_some())
        });
    let side = uc.get_data_mut();
    if due || takeable {
        side.hit_stop = true;
        let _ = uc.emu_stop();
    } else {
        side.cycles += u64::from(size / 4);
    }
}

/// One access of `size` bytes to a device on a 32-bit bus; `op(delta, width,
/// store)` performs one beat at `+delta`. An 8-byte access is modelled as two
/// word beats, low address first: none of the devices is wider than 32 bits,
/// and nothing on the boot path is known to do one — if something does, both
/// halves show in the MMIO trace.
fn access32(
    size: usize,
    write: Option<u64>,
    mut op: impl FnMut(u32, Width, Option<u32>) -> BusResult<u32>,
) -> Result<u64, String> {
    let e = |e: BusError| e.to_string();
    let width = match size {
        1 => Width::Byte,
        2 => Width::Half,
        4 => Width::Word,
        8 => {
            let lo = op(0, Width::Word, write.map(|v| v as u32)).map_err(e)?;
            let hi = op(4, Width::Word, write.map(|v| (v >> 32) as u32)).map_err(e)?;
            return Ok(write.unwrap_or(u64::from(lo) | (u64::from(hi) << 32)));
        }
        _ => return Err(format!("{size}-byte access")),
    };
    let v = op(0, width, write.map(|v| v as u32)).map_err(e)?;
    Ok(write.unwrap_or(u64::from(v)))
}

/// `0xFC00_0000..0xFF80_0000` → the shared bus at `0x7C00_0000 + off`.
fn periph(uc: &mut Unicorn<'_, ArmSide>, off: u64, size: usize, write: Option<u64>) -> u64 {
    let bus = PERIPH_BUS + off as u32;
    let side = uc.get_data_mut();
    let r = if Machine::in_mmio(bus) {
        let m = &mut side.emu.machine;
        access32(size, write, |d, w, store| match store {
            Some(v) => m.store(bus + d, w, v).map(|()| 0),
            None => m.load(bus + d, w),
        })
    } else {
        Err("no device decodes this bus address".to_string())
    };
    // Reading MAIL0 or writing its CONFIG moves the ARM's mailbox interrupt;
    // any eMMC2 access can move its line (a command completes at once, a
    // status write clears it).
    let emmc2 = (EMMC2_BASE..EMMC2_BASE + EMMC2_SIZE).contains(&bus);
    if emmc2 && !side.emu.machine.emmc2.take_dma_written().is_empty() {
        side.code_dirty = true;
    }
    if emmc2 || (mbox::BASE..mbox::BASE + mbox::SIZE).contains(&bus) {
        side.sync_lines();
    }
    side.finish(PERIPH_ARM + off, size, write, r)
}

fn periph_read(uc: &mut Unicorn<'_, ArmSide>, off: u64, size: usize) -> u64 {
    periph(uc, off, size, None)
}

fn periph_write(uc: &mut Unicorn<'_, ArmSide>, off: u64, size: usize, value: u64) {
    periph(uc, off, size, Some(value));
}

/// `0xFF80_0000..0x1_0000_0000`: the ARM-local block at `+0`, the GIC-400 at
/// `+0x4_0000`, nothing else.
fn local(uc: &mut Unicorn<'_, ArmSide>, off: u64, size: usize, write: Option<u64>) -> u64 {
    // The armstub runs in EL3 and is secure; after its `eret` (with
    // `SCR_EL3.NS` set) everything is non-secure. No EL1/EL2 runs secure here.
    let acc = Accessor {
        cpu: 0,
        secure: current_el(uc) == 3,
    };
    let side = uc.get_data_mut();
    let r = if off < u64::from(armlocal::SIZE) {
        let (dev, o) = (&mut side.local, off as u32);
        let r = access32(size, write, |d, w, store| match store {
            Some(v) => dev.write(o + d, w, v).map(|()| 0),
            None => dev.read(o + d, w),
        });
        // The prescaler sets the counter's rate.
        let hz = side.local.counter_hz().unwrap_or(0);
        side.timer.set_hz(side.cycles, hz);
        r
    } else if (GIC_OFF..GIC_OFF + u64::from(gic::SIZE)).contains(&off) {
        let (g, o) = (&mut side.gic, (off - GIC_OFF) as u32);
        let r = access32(size, write, |d, w, store| match store {
            Some(v) => g.write_as(acc, o + d, w, v).map(|()| 0),
            None => g.read_as(acc, o + d, w),
        });
        // Enables, priorities, IAR and EOI all change what it signals.
        side.signal = side.gic.signal(0);
        r
    } else {
        Err("nothing is modelled here in the ARM-local window".to_string())
    };
    side.finish(LOCAL_ARM + off, size, write, r)
}

fn local_read(uc: &mut Unicorn<'_, ArmSide>, off: u64, size: usize) -> u64 {
    local(uc, off, size, None)
}

fn local_write(uc: &mut Unicorn<'_, ArmSide>, off: u64, size: usize, value: u64) {
    local(uc, off, size, Some(value));
}

fn on_unmapped(
    uc: &mut Unicorn<'_, ArmSide>,
    ty: MemType,
    addr: u64,
    size: usize,
    _v: i64,
) -> bool {
    uc.get_data_mut().unmapped = Some((ty, addr, size));
    false
}

/// How [`ArmCore::run`] interleaves the two processors.
#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    /// ARM cycles between the VPU's turns.
    pub arm_slice: usize,
    /// VPU steps (both cores together) per turn.
    pub vpu_slice: u64,
}

impl Default for Schedule {
    /// A million ARM cycles to twenty thousand VPU steps. The firmware is
    /// parked in its ThreadX idle loop after `arm_loader` and only has to
    /// notice a mailbox doorbell, while the kernel has all the work; and a VPU
    /// step costs the interpreter far more than an ARM instruction costs TCG.
    fn default() -> Self {
        Schedule {
            arm_slice: 1_000_000,
            vpu_slice: 20_000,
        }
    }
}

/// When [`ArmCore::run`] gives up.
#[derive(Debug, Clone)]
pub struct ArmLimits {
    /// ARM instructions executed (not slept). Deterministic, unlike the wall
    /// clock: a run cut by this ends at the same point every time.
    pub max_insns: u64,
    pub max_wall: Duration,
    /// Echo the ARM's UART output to stderr as it is produced.
    pub live_console: bool,
}

#[derive(Debug, Clone)]
pub enum ArmEnd {
    InsnLimit,
    TimeLimit,
    Stopped(ArmStop),
    /// A VPU slice ended on something other than its step count.
    VpuStopped(RunEnd),
}

#[derive(Debug, Clone)]
pub struct ArmReport {
    pub end: ArmEnd,
    /// ARM instructions executed.
    pub insns: u64,
    /// Modelled cycles that passed: `insns` plus `slept`.
    pub cycles: u64,
    pub slept: u64,
    pub wfis: u64,
    pub tb_flushes: u64,
    pub vpu_steps: u64,
    /// ARM runs between two points where an interrupt could be taken.
    pub slices: u64,
    /// `CNTPCT` at the end.
    pub counter: u64,
    /// Interrupts taken, by INTID.
    pub irqs: BTreeMap<u32, u64>,
    /// What the ARM wrote to the console UART.
    pub console: Vec<u8>,
    /// What the VPU wrote to it during its slices (it has handed the UART over
    /// by now, so this should stay empty).
    pub vpu_console: Vec<u8>,
    pub wall: Duration,
}

/// Why a [`ArmCore::run_until`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SliceEnd {
    /// It reached its stop cycle, or an interrupt became takeable.
    Stopped,
    /// The core halted in `wfi`.
    Wfi,
}

/// An aarch64 core sharing an [`Emulator`]'s RAM and bus.
pub struct ArmCore {
    uc: Unicorn<'static, ArmSide>,
}

impl ArmCore {
    /// Put an ARM core next to `emu`'s VPU. `ram` lists the `(base, size)`
    /// ranges it sees as RAM — the dtb's `memory` node — each of which must lie
    /// inside the model's RAM and be page-aligned.
    pub fn new(emu: Emulator, ram: &[(u64, u64)]) -> Result<ArmCore, String> {
        let side = ArmSide {
            emu,
            gic: Gic::new(),
            local: ArmLocal::new(),
            timer: GenericTimer::new(),
            cycles: 0,
            slept: 0,
            wfis: 0,
            tb_flushes: 0,
            irqs: BTreeMap::new(),
            timer_accesses: 0,
            impdef: BTreeMap::new(),
            mmio_faults: Vec::new(),
            recent_mmio: VecDeque::new(),
            first_exceptions: Vec::new(),
            recent_exceptions: VecDeque::new(),
            exceptions_taken: 0,
            signal: None,
            route: Route::default(),
            lines: [false; 6],
            code_dirty: false,
            stop_at: u64::MAX,
            hit_stop: false,
            stop: None,
            unmapped: None,
        };
        let mut uc = Unicorn::new_with_data(Arch::ARM64, Mode::ARM, side).map_err(uce)?;
        // Must precede anything that creates the vCPU. It is Unicorn's default
        // as well; the Pi 4's cores are Cortex-A72 r0p3, and so is this one
        // (`MIDR_EL1` reads `0x410fd083`).
        uc.ctl_set_cpu_model(Arm64CpuModel::A72 as i32)
            .map_err(uce)?;

        let (host, len) = {
            let r = &mut uc.get_data_mut().emu.machine.ram;
            (r.host_ptr(), r.len() as u64)
        };
        for &(base, size) in ram {
            if base.checked_add(size).is_none_or(|end| end > len) {
                return Err(format!(
                    "memory {base:#x}+{size:#x} is outside the model's {len:#x} bytes of RAM"
                ));
            }
            if base % 4096 != 0 || size % 4096 != 0 {
                return Err(format!("memory {base:#x}+{size:#x} is not page-aligned"));
            }
            // SAFETY: `host` is the model's RAM backing store
            // (`Ram::host_ptr`) and the range was bounds-checked above. It
            // outlives this mapping: the `Ram` lives in `ArmSide`, which is
            // this very `Unicorn`'s user data, and `UnicornInner`'s `Drop` runs
            // `uc_close` before its `data` field is dropped. It is never
            // reallocated (the invariant documented on `Ram::host_ptr`).
            //
            // Unicorn writes through it while `emu_start` runs, so no Rust
            // reference into the RAM may be held across that call. None is:
            // Rust touches RAM only between slices and inside callbacks —
            // while the guest is stopped — and always through `Ram`'s own
            // methods, whose borrows end before they return.
            unsafe { uc.mem_map_ptr(base, size, Prot::ALL, host.add(base as usize).cast()) }
                .map_err(uce)?;
        }
        uc.mmio_map(
            PERIPH_ARM,
            PERIPH_SIZE,
            Some(periph_read),
            Some(periph_write),
        )
        .map_err(uce)?;
        uc.mmio_map(LOCAL_ARM, LOCAL_SIZE, Some(local_read), Some(local_write))
            .map_err(uce)?;
        uc.add_intr_hook(on_exception).map_err(uce)?;
        uc.add_block_hook(1, 0, on_block).map_err(uce)?;
        uc.add_mem_hook(
            HookType::MEM_READ_UNMAPPED
                | HookType::MEM_WRITE_UNMAPPED
                | HookType::MEM_FETCH_UNMAPPED,
            1,
            0,
            on_unmapped,
        )
        .map_err(uce)?;

        let handle = uc.get_handle().cast::<c_void>();
        let data = std::ptr::from_mut(uc.get_data_mut()).cast::<c_void>();
        let hooks: [(i32, ffi::SysHook); 2] = [
            (ffi::UC_ARM64_INS_MRS, on_mrs),
            (ffi::UC_ARM64_INS_MSR, on_msr),
        ];
        for (insn, callback) in hooks {
            let mut hh = 0usize;
            // SAFETY: `handle` is this live engine; `callback` has the C
            // signature `uc_cb_insn_sys_t`; `data` stays valid for the hook's
            // whole life (`side_of`). `begin > end` hooks every address, and
            // the variadic argument is the `uc_arm64_insn` to hook.
            let err = unsafe {
                ffi::uc_hook_add(
                    handle,
                    &mut hh,
                    ffi::UC_HOOK_INSN,
                    callback as *mut c_void,
                    data,
                    1,
                    0,
                    insn,
                )
            };
            if err != 0 {
                return Err(format!("uc_hook_add(UC_HOOK_INSN, {insn}): error {err}"));
            }
        }
        Ok(ArmCore { uc })
    }

    pub fn side(&mut self) -> &mut ArmSide {
        self.uc.get_data_mut()
    }

    pub fn pc(&self) -> u64 {
        self.uc.pc_read().unwrap_or(0)
    }

    pub fn el(&self) -> u32 {
        current_el(&self.uc)
    }

    /// Instructions executed so far.
    pub fn insns(&self) -> u64 {
        let s = self.uc.get_data();
        s.cycles - s.slept
    }

    /// General-purpose register `n` (0..=30), `SP` for 31.
    pub fn x(&self, n: usize) -> u64 {
        let r = XREGS.get(n).copied().unwrap_or(RegisterARM64::SP);
        self.uc.reg_read(r).unwrap_or(0)
    }

    /// `ESR`, `ELR` and `FAR` of `el` (1..=3), for the report.
    pub fn exception_regs(&self, el: u32) -> (u64, u64, u64) {
        use RegisterARM64 as R;
        let i = el.min(3) as usize;
        let rd = |r: R| self.uc.reg_read(r).unwrap_or(0);
        (
            rd([R::ESR_EL0, R::ESR_EL1, R::ESR_EL2, R::ESR_EL3][i]),
            rd([R::ELR_EL0, R::ELR_EL1, R::ELR_EL2, R::ELR_EL3][i]),
            rd([R::FAR_EL0, R::FAR_EL1, R::FAR_EL2, R::FAR_EL3][i]),
        )
    }

    /// `VBAR_EL1` — for Linux, the address of its `vectors`, which is what
    /// the KASLR slide can be read off.
    pub fn vbar_el1(&self) -> u64 {
        self.uc.reg_read(RegisterARM64::VBAR_EL1).unwrap_or(0)
    }

    /// `PSTATE`, including `DAIF`.
    pub fn pstate(&self) -> u64 {
        self.uc.reg_read(RegisterARM64::PSTATE).unwrap_or(0)
    }

    /// Release the core the way the SoC does: at physical 0, where
    /// `arm_loader` put the armstub, in EL3h with `DAIF` masked.
    pub fn reset(&mut self) -> Result<(), String> {
        self.uc
            .reg_write(RegisterARM64::PSTATE, PSTATE_DAIF | pstate_elh(3))
            .map_err(uce)?;
        rebuild_hflags(&mut self.uc).map_err(uce)?;
        self.uc.set_pc(0).map_err(uce)
    }

    fn finish_run(&mut self, r: Result<(), uc_error>) -> Result<(), ArmStop> {
        if let Some(stop) = self.side().stop.take() {
            return Err(stop);
        }
        r.map_err(|e| {
            let unmapped = self.side().unmapped.take();
            ArmStop::Engine {
                error: uce(e),
                pc: self.pc(),
                unmapped,
            }
        })
    }

    /// Run exactly `count` ARM instructions (Unicorn's own instruction
    /// count), taking no interrupts. For tests that step through a program.
    pub fn run_slice(&mut self, count: usize) -> Result<(), ArmStop> {
        self.side().stop_at = u64::MAX;
        let pc = self.pc();
        let r = self.uc.emu_start(pc, u64::MAX, 0, count);
        self.finish_run(r)
    }

    /// Run until the first block that starts at or after cycle `stop_at`, an
    /// interrupt becoming takeable, a `wfi`, or a stop.
    fn run_until(&mut self, stop_at: u64) -> Result<SliceEnd, ArmStop> {
        let side = self.side();
        // Never a stop point already behind us: that would stop before the
        // first block, with no progress.
        side.stop_at = stop_at.max(side.cycles + 1);
        side.hit_stop = false;
        // Between slices no TB is running, so a flush is safe here (not from
        // inside the MMIO hook that started the DMA). The data cannot run
        // before this: the transfer's interrupt ends the slice first.
        if std::mem::take(&mut side.code_dirty) {
            side.tb_flushes += 1;
            self.uc.ctl_flush_tb().map_err(|e| ArmStop::Engine {
                error: uce(e),
                pc: self.pc(),
                unmapped: None,
            })?;
        }
        let pc = self.pc();
        let r = self.uc.emu_start(pc, u64::MAX, 0, 0);
        self.finish_run(r)?;
        // Nothing else ends an `emu_start` with no count, no timeout and an
        // unreachable `until`: no stop was requested, so QEMU halted in `wfi`
        // (`helper_wfi`, with `PC` already past it).
        if self.side().hit_stop {
            Ok(SliceEnd::Stopped)
        } else {
            debug_assert_eq!(fetch_insn(&self.uc, self.pc() - 4), Some(INSN_WFI));
            Ok(SliceEnd::Wfi)
        }
    }

    /// Bring the interrupt state up to date — the counter's rate, every line,
    /// the routing — and take whatever the core would take now.
    fn poll_interrupts(&mut self) -> Result<Option<u32>, uc_error> {
        let route = Route::read(&self.uc)?;
        let side = self.side();
        side.route = route;
        side.sync_lines();
        take_interrupt(&mut self.uc)
    }

    /// The core is halted in `wfi`: move the clock to the next thing that can
    /// wake it — a timer compare, or `until` (the VPU's turn: the mailbox is
    /// the other interrupt source) — unless an interrupt is already pending,
    /// which wakes it at once.
    fn sleep(&mut self, until: u64) {
        let side = self.side();
        side.wfis += 1;
        side.sync_lines();
        if side.signal.is_some() {
            return;
        }
        let wake = side
            .timer
            .next_event(side.cycles)
            .unwrap_or(u64::MAX)
            .min(until);
        if wake > side.cycles {
            side.slept += wake - side.cycles;
            side.cycles = wake;
        }
    }

    /// Alternate ARM and VPU until the ARM stops or a limit is hit.
    pub fn run(&mut self, sched: Schedule, lim: &ArmLimits) -> ArmReport {
        let start = Instant::now();
        let insns0 = self.insns();
        let (cycles0, slept0, wfis0, flushes0) = {
            let s = self.side();
            (s.cycles, s.slept, s.wfis, s.tb_flushes)
        };
        let vpu_retired = |e: &Emulator| e.cpu.retired + e.cpu1.as_ref().map_or(0, |c| c.retired);
        let vpu0 = vpu_retired(&self.side().emu);
        let quantum = sched.arm_slice as u64;
        let mut vpu_due = cycles0 + quantum;
        let mut console = Vec::new();
        let mut vpu_console = Vec::new();
        let mut slices = 0u64;
        let end = loop {
            if let Err(e) = self.poll_interrupts() {
                break ArmEnd::Stopped(ArmStop::Engine {
                    error: uce(e),
                    pc: self.pc(),
                    unmapped: None,
                });
            }
            let next = {
                let s = self.side();
                s.timer.next_event(s.cycles).unwrap_or(u64::MAX)
            };
            slices += 1;
            let r = self.run_until(vpu_due.min(next));
            // Drain before the VPU slice runs: `Emulator::run` drains the same
            // UART into its own report.
            let out = self.side().emu.machine.take_console_output();
            if lim.live_console && !out.is_empty() {
                let _ = std::io::stderr().write_all(&out);
            }
            console.extend_from_slice(&out);
            match r {
                Err(stop) => break ArmEnd::Stopped(stop),
                Ok(SliceEnd::Wfi) => self.sleep(vpu_due),
                Ok(SliceEnd::Stopped) => {}
            }

            let mut vpu_end = None;
            while self.side().cycles >= vpu_due {
                let emu = &mut self.side().emu;
                let vl = RunLimits {
                    max_steps: Some(vpu_retired(emu) + sched.vpu_slice),
                    max_wall: None,
                    stop_pc: None,
                    // The firmware is in its idle loop by design now: neither
                    // the spin detector nor the silence watchdog means
                    // anything.
                    idle_spin_limit: 0,
                    silent_us: 0,
                };
                let rep = emu.run(&vl);
                vpu_console.extend_from_slice(&rep.console);
                if rep.end != RunEnd::StepLimit {
                    vpu_end = Some(rep.end);
                    break;
                }
                vpu_due += quantum;
            }
            if let Some(e) = vpu_end {
                break ArmEnd::VpuStopped(e);
            }
            if self.insns() - insns0 >= lim.max_insns {
                break ArmEnd::InsnLimit;
            }
            if start.elapsed() >= lim.max_wall {
                break ArmEnd::TimeLimit;
            }
        };
        let vpu_steps = vpu_retired(&self.side().emu) - vpu0;
        let insns = self.insns() - insns0;
        let s = self.side();
        ArmReport {
            end,
            insns,
            cycles: s.cycles - cycles0,
            slept: s.slept - slept0,
            wfis: s.wfis - wfis0,
            tb_flushes: s.tb_flushes - flushes0,
            vpu_steps,
            slices,
            counter: s.timer.count(s.cycles),
            irqs: s.irqs.clone(),
            console,
            vpu_console,
            wall: start.elapsed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAM: usize = 0x10_0000;

    fn core_with(program: &[(u32, &[u32])]) -> ArmCore {
        let mut m = Machine::new(RAM);
        for &(addr, words) in program {
            for (i, &w) in words.iter().enumerate() {
                m.ram.store(addr + 4 * i as u32, Width::Word, w).unwrap();
            }
        }
        let mut core = ArmCore::new(Emulator::new(m, 0), &[(0, RAM as u64)]).unwrap();
        core.reset().unwrap();
        core
    }

    #[test]
    fn resets_into_el3() {
        let mut core = core_with(&[(0, &[0xd538_4240])]); // mrs x0, CurrentEL
        core.run_slice(1).unwrap();
        assert_eq!(core.x(0), 0xC);
    }

    #[test]
    fn the_peripheral_window_reaches_the_shared_pl011() {
        let mut core = core_with(&[(
            0,
            &[
                0xd2bf_c401, // mov  x1, #0xfe200000
                0xf282_0001, // movk x1, #0x1000         -> 0xFE20_1000, UART0
                0x5280_0820, // mov  w0, #'A'
                0x3900_0020, // strb w0, [x1]            byte store to DR
                0x5280_0140, // mov  w0, #'\n'
                0xb900_0020, // str  w0, [x1]            word store to DR
                0xb940_1822, // ldr  w2, [x1, #0x18]     FR
            ],
        )]);
        core.run_slice(7).unwrap();
        assert_eq!(core.side().emu.machine.uart0.out, b"A\n");
        // TXFE | RXFE: the model's PL011 is always ready, as earlycon needs.
        assert_eq!(core.x(2), 0x90);
        let last = core.side().recent_mmio.back().copied().unwrap();
        assert_eq!((last.addr, last.size, last.write), (0xFE20_1018, 4, false));
    }

    #[test]
    fn an_address_no_device_decodes_is_refused_not_folded_into_ram() {
        // 0xFC00_0000 -> bus 0x7C00_0000, which the VPU's bus would fold onto
        // DRAM at 0x3C00_0000.
        let mut core = core_with(&[(
            0,
            &[
                0xd2bf_8001, // mov  x1, #0xfc000000
                0x5280_0820, // mov  w0, #0x41
                0xb900_0020, // str  w0, [x1]
            ],
        )]);
        core.run_slice(3).unwrap();
        assert_eq!(core.side().mmio_faults.len(), 1);
    }

    #[test]
    fn the_emmc2_interrupt_is_a_level_on_spi_158() {
        let mut core = core_with(&[(
            0,
            &[
                0xd2bf_c681, // mov  x1, #0xfe340000      EMMC2
                0x5280_0020, // mov  w0, #1
                0xb900_3420, // str  w0, [x1, #0x34]      INT_STATUS_EN: command complete
                0xb900_3820, // str  w0, [x1, #0x38]      INT_SIGNAL_EN: likewise
                0x52a1_a340, // mov  w0, #0x0d1a0000      CMD13, R1
                0xb900_0c20, // str  w0, [x1, #0x0c]      issue: completes at once
                0x5280_0020, // mov  w0, #1
                0xb900_3020, // str  w0, [x1, #0x30]      INT_STATUS: write 1 to clear
            ],
        )]);
        // A level SPI reads back pending for as long as it is asserted.
        let pending = |core: &mut ArmCore| {
            let s = Accessor {
                cpu: 0,
                secure: true,
            };
            let ispendr = 0x1200 + 4 * (gic::ID_EMMC2 / 32);
            let w = core.side().gic.read_as(s, ispendr, Width::Word).unwrap();
            w & (1 << (gic::ID_EMMC2 % 32)) != 0
        };
        core.run_slice(4).unwrap();
        assert!(!pending(&mut core), "enabled, nothing latched yet");
        core.run_slice(2).unwrap();
        assert!(pending(&mut core), "command complete, signalled");
        core.run_slice(2).unwrap();
        assert!(!pending(&mut core), "cleared by the W1C");
    }

    #[test]
    fn impdef_system_registers_are_storage() {
        let mut core = core_with(&[(
            0,
            &[
                0xd280_0440, // mov x0, #0x22
                0xd519_b040, // msr s3_1_c11_c0_2, x0   L2CTLR_EL1
                0xd539_b041, // mrs x1, s3_1_c11_c0_2
                0xd539_f222, // mrs x2, s3_1_c15_c2_1   CPUECTLR_EL1, never written
                0xd280_0023, // mov x3, #1
            ],
        )]);
        core.run_slice(5).unwrap();
        assert_eq!((core.x(1), core.x(2), core.x(3)), (0x22, 0, 1));
        assert_eq!(core.side().impdef.get("S3_1_C11_C0_2"), Some(&0x22));
        assert_eq!(core.side().exceptions_taken, 0, "emulated, not delivered");
    }

    #[test]
    fn hvc_from_el1_is_taken_to_el2_and_returns() {
        let mut core = core_with(&[
            (
                0,
                &[
                    0xd280_b620, // mov  x0, #0x5b1
                    0xd51e_1100, // msr  scr_el3, x0        NS | HCE | RW | SMD
                    0xd284_0000, // mov  x0, #0x2000
                    0xd51c_c000, // msr  vbar_el2, x0
                    0xd2b0_0000, // mov  x0, #0x80000000
                    0xd51c_1100, // msr  hcr_el2, x0        RW
                    0xd280_78a0, // mov  x0, #0x3c5         EL1h, DAIF masked
                    0xd51e_4000, // msr  spsr_el3, x0
                    0x1000_0060, // adr  x0, 0x2c
                    0xd51e_4020, // msr  elr_el3, x0
                    0xd69f_03e0, // eret
                    0xd282_4605, // 0x2c: mov x5, #0x1230
                    0x9100_00bf, // mov  sp, x5
                    0xd400_0842, // 0x34: hvc #0x42
                    0xd280_0026, // 0x38: mov x6, #1
                    0x1400_0000, // 0x3c: b .
                ],
            ),
            (
                0x2400, // VBAR_EL2 + 0x400: synchronous, lower EL, AArch64
                &[
                    0xd538_4247, // mrs  x7, CurrentEL
                    0xd53c_5208, // mrs  x8, esr_el2
                    0xd53c_4029, // mrs  x9, elr_el2
                    0xd53c_400a, // mrs  x10, spsr_el2
                    0x9100_03eb, // mov  x11, sp
                    0xd69f_03e0, // eret
                ],
            ),
        ]);
        core.run_slice(40).unwrap();
        assert_eq!(core.x(7), 0x8, "handler runs in EL2");
        assert_eq!(core.x(8), 0x5A00_0042, "ESR: EC HVC64, IL, imm16");
        assert_eq!(core.x(9), 0x38, "ELR: the instruction after the hvc");
        assert_eq!(core.x(10), 0x3c5, "SPSR: EL1h as it was");
        assert_eq!(core.x(11), 0, "EL2 runs on its own SP");
        assert_eq!((core.pc(), core.el(), core.x(6)), (0x3c, 1, 1));
        assert_eq!(core.x(31), 0x1230, "EL1's SP survives the round trip");
        assert_eq!(core.side().exceptions_taken, 1);
    }

    /// The Rust binding's MRS/MSR hook returns `bool` where C expects
    /// `uint32_t` (module docs); through our own registration, an access the
    /// hook lets through must land, and the instructions after a skipped one
    /// must run normally.
    #[test]
    fn the_sysreg_hook_lets_other_registers_through_and_answers_the_timer() {
        let mut core = core_with(&[(
            0,
            &[
                0xd29f_3000, // mov  x0, #0xf980
                0xf2a0_66e0, // movk x0, #0x337, lsl #16   -> 54_000_000
                0xd51b_e000, // msr  cntfrq_el0, x0        let through to QEMU
                0xd53b_e001, // mrs  x1, cntfrq_el0        let through
                0xd53b_e022, // mrs  x2, cntpct_el0        answered here
                0xd280_00a3, // mov  x3, #5                runs after a skipped MRS
                0xd280_0024, // mov  x4, #1
                0xd51b_e224, // msr  cntp_ctl_el0, x4      answered here
                0xd53b_e225, // mrs  x5, cntp_ctl_el0
                0xd280_00e6, // mov  x6, #7
            ],
        )]);
        core.side().timer.set_hz(0, armlocal::CRYSTAL_HZ);
        core.run_slice(10).unwrap();
        assert_eq!(core.x(1), 54_000_000, "the MSR passed through landed");
        assert_eq!(core.x(3), 5);
        assert_eq!(core.x(6), 7);
        // Enabled, compare 0 already met: ISTATUS.
        assert_eq!(core.x(5), 0b101);
        assert_eq!(core.side().timer_accesses, 3);
        assert_eq!(core.pc(), 40);
    }

    /// Drop from EL3 to EL1h non-secure with IRQs unmasked, at `0x40`, with
    /// `VBAR_EL1 = 0x2000` — the shape Linux runs in (its IRQs are routed to
    /// EL1: the hyp stub leaves `HCR_EL2.IMO` clear).
    const TO_EL1: [u32; 12] = [
        0xd280_b620, // mov  x0, #0x5b1
        0xd51e_1100, // msr  scr_el3, x0        NS | HCE | RW | SMD
        0xd284_0000, // mov  x0, #0x2000
        0xd518_c000, // msr  vbar_el1, x0
        0xd2b0_0000, // mov  x0, #0x80000000
        0xd51c_1100, // msr  hcr_el2, x0        RW
        0xd280_00a0, // mov  x0, #0x5           EL1h, DAIF clear
        0xd51e_4000, // msr  spsr_el3, x0
        0x1000_0100, // 0x20: adr x0, 0x40
        0xd51e_4020, // msr  elr_el3, x0
        0xd69f_03e0, // eret
        0xd503_201f, // nop
    ];

    /// VBAR_EL1 + 0x280: IRQ, current EL with SP_ELx.
    const IRQ_HANDLER: (u32, &[u32]) = (
        0x2280,
        &[
            0xd538_4247, // mrs  x7, CurrentEL
            0xd538_4028, // mrs  x8, elr_el1
            0xd538_4009, // mrs  x9, spsr_el1
            0x1400_0000, // b .
        ],
    );

    /// The GIC as the armstub (secure) leaves it plus the NS timer PPI
    /// enabled, and the counter at 54 MHz with CNTP firing at `cval`.
    fn arm_timer(core: &mut ArmCore, cval: u64) {
        let s = Accessor {
            cpu: 0,
            secure: true,
        };
        let side = core.side();
        for (off, v) in [
            (0x1000, 3),                          // GICD_CTLR
            (0x1080, !0),                         // GICD_IGROUPR0: group 1
            (0x1100, 1 << gic::ID_NS_PHYS_TIMER), // GICD_ISENABLER0
            (0x2000, 0x1e7),                      // GICC_CTLR
            (0x2004, 0xff),                       // GICC_PMR
        ] {
            side.gic.write_as(s, off, Width::Word, v).unwrap();
        }
        side.timer.set_hz(0, armlocal::CRYSTAL_HZ);
        let t = gentimer::Reg::decode(3, 3, 14, 2, 2).unwrap(); // CNTP_CVAL_EL0
        side.timer.write(t, 0, cval);
        let c = gentimer::Reg::decode(3, 3, 14, 2, 1).unwrap(); // CNTP_CTL_EL0
        side.timer.write(c, 0, 1);
    }

    #[test]
    fn a_timer_interrupt_stops_the_slice_at_its_deadline_and_is_taken_at_el1() {
        let mut prog = TO_EL1.to_vec();
        prog.push(0x1400_0000); // 0x30: b .   (never reached: eret goes to 0x40)
        let mut core = core_with(&[
            (0, &prog),
            (0x40, &[0x1400_0000]), // b .
            IRQ_HANDLER,
        ]);
        arm_timer(&mut core, 540); // 10 µs = 15_000 cycles
        core.run_slice(11).unwrap();
        assert_eq!((core.pc(), core.el()), (0x40, 1));

        assert_eq!(core.poll_interrupts().unwrap(), None);
        let due = {
            let s = core.side();
            s.timer.next_event(s.cycles).unwrap()
        };
        assert_eq!(core.run_until(due).unwrap(), SliceEnd::Stopped);
        // `b .` is a one-instruction block: the stop lands on the deadline.
        assert_eq!(core.side().cycles, due);
        assert_eq!(core.poll_interrupts().unwrap(), Some(gic::ID_NS_PHYS_TIMER));
        assert_eq!(core.pc(), 0x2280);

        core.run_until(due + 10).unwrap();
        assert_eq!(core.x(7), 0x4, "handler runs at EL1");
        assert_eq!(core.x(8), 0x40, "ELR: where the loop was");
        assert_eq!(core.x(9), 0x5, "SPSR: EL1h, IRQs unmasked");
        assert_eq!(core.side().irqs.get(&gic::ID_NS_PHYS_TIMER), Some(&1));
        // The handler has not read GICC_IAR, so the GIC still signals, but
        // PSTATE.I is set now: it is not entered twice.
        assert!(core.side().signal.is_some());
        assert_eq!(core.poll_interrupts().unwrap(), None);
    }

    #[test]
    fn wfi_sleeps_to_the_next_timer_compare_and_wakes_into_the_irq() {
        let mut core = core_with(&[
            (0, &TO_EL1),
            (0x40, &[0xd503_207f, 0x17ff_ffff]), // wfi; b .-4
            IRQ_HANDLER,
        ]);
        arm_timer(&mut core, 54_000); // 1 ms
        core.run_slice(11).unwrap();
        assert_eq!(core.poll_interrupts().unwrap(), None);
        assert_eq!(core.run_until(u64::MAX).unwrap(), SliceEnd::Wfi);
        assert_eq!(core.pc(), 0x44, "halted after the wfi");
        let before = core.side().cycles;
        core.sleep(u64::MAX);
        let s = core.side();
        assert_eq!(
            s.timer.count(s.cycles),
            54_000,
            "woke exactly at the compare"
        );
        assert_eq!(s.slept, s.cycles - before);
        assert_eq!(core.poll_interrupts().unwrap(), Some(gic::ID_NS_PHYS_TIMER));
        let now = core.side().cycles;
        core.run_until(now + 10).unwrap();
        assert_eq!(core.x(8), 0x44, "ELR: the instruction after the wfi");

        // A `wfi` with an interrupt already pending does not sleep.
        let c = core.side().cycles;
        core.sleep(u64::MAX);
        assert_eq!(core.side().cycles, c);
    }

    #[test]
    fn a_masked_interrupt_is_taken_right_after_the_unmask() {
        let mut core = core_with(&[
            (0, &TO_EL1),
            (
                0x40,
                &[
                    0xd503_42df, // msr  daifset, #2
                    0xd503_201f, // 0x44: nop            <- the timer fires while masked
                    0xd503_201f, // nop
                    0xd503_42ff, // 0x4c: msr daifclr, #2
                    0xd503_201f, // 0x50: nop            <- taken here
                    0x17ff_fffb, // b 0x40
                ],
            ),
            IRQ_HANDLER,
        ]);
        // Due within the first pass, while masked.
        arm_timer(&mut core, 0);
        core.run_slice(11).unwrap();
        core.run_slice(1).unwrap(); // msr daifset
        assert_eq!(core.poll_interrupts().unwrap(), None, "masked");
        assert!(core.side().signal.is_some());
        assert_eq!(core.run_until(u64::MAX).unwrap(), SliceEnd::Stopped);
        assert_eq!(core.pc(), 0x50, "stopped at the block after the unmask");
        assert_eq!(core.poll_interrupts().unwrap(), Some(gic::ID_NS_PHYS_TIMER));
        let now = core.side().cycles;
        core.run_until(now + 10).unwrap();
        assert_eq!(core.x(8), 0x50);
    }

    #[test]
    fn routing_follows_scr_and_hcr() {
        let el = |el: u64, masked: bool| (el << 2) | 1 | if masked { PSTATE_I } else { 0 };
        let linux = Route {
            scr: 0x5b1,
            hcr: 1 << 31,
        };
        assert_eq!(linux.target(el(1, false), false), Some(1));
        assert_eq!(linux.target(el(1, true), false), None);
        // Without IMO an IRQ goes to the current EL, EL2 included.
        assert_eq!(linux.target(el(2, false), false), Some(2));
        let kvm = Route {
            scr: 0x5b1,
            hcr: HCR_IMO,
        };
        assert_eq!(
            kvm.target(el(1, true), false),
            Some(2),
            "not maskable from below"
        );
        let secure_irq = Route {
            scr: SCR_IRQ,
            hcr: 0,
        };
        assert_eq!(secure_irq.target(el(3, false), false), Some(3));
        assert_eq!(secure_irq.target(el(3, true), false), None);
    }

    #[test]
    fn bootargs_go_onto_the_command_line_in_place() {
        let mut m = Machine::new(RAM);
        let blob = crate::fdt::tests::sample();
        m.ram.write_slice(0x8000, &blob).unwrap();
        let (old, new) = add_bootargs(&mut m, 0x8000, &BOOTARGS).unwrap();
        assert_eq!(old, "hi");
        assert_eq!(new, "earlycon keep_bootcon kvm-arm.mode=none hi");
        let back = read_dtb(&m, 0x8000).unwrap();
        let props = Fdt::parse(&back).unwrap().properties_of("/chosen").unwrap();
        assert_eq!(props[0].as_str().as_deref(), Some(new.as_str()));
        // Idempotent, and an argument already there by name is not repeated.
        assert_eq!(add_bootargs(&mut m, 0x8000, &BOOTARGS).unwrap().0, new);
        let (_, again) = add_bootargs(&mut m, 0x8000, &["kvm-arm.mode=protected"]).unwrap();
        assert_eq!(again, new);

        // Something right after the blob: refuse rather than overwrite it.
        let mut m = Machine::new(RAM);
        m.ram.write_slice(0x8000, &blob).unwrap();
        m.ram
            .store(0x8000 + blob.len() as u32 + 2, Width::Byte, 0xAA)
            .unwrap();
        assert!(add_bootargs(&mut m, 0x8000, &BOOTARGS).is_err());
    }
}
