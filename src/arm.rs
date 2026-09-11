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
//! undefined-instruction path ([`ArmSide::impdef`]). Unicorn's MRS/MSR
//! instruction hook is not used for this: with one installed, an MSR the hook
//! lets through is dropped (probed: `msr cntfrq_el0` then read back QEMU's
//! 62.5 MHz default instead of the value written), and one it skips
//! re-executes forever unless the hook also moves `PC`.
//!
//! ## Exceptions are taken by hand
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
//! ## Scheduling
//!
//! [`ArmCore::run`] alternates a fixed number of ARM instructions
//! (`emu_start(.., count)`) with a fixed number of VPU steps, so the firmware
//! keeps servicing the mailbox and a run is reproducible: the same inputs give
//! the same interleaving. The one input that is not the model's own is the
//! generic timer — Unicorn's `CNTVCT` follows the host clock — and nothing on
//! the way to the first `earlycon` line reads it.
//!
//! ## Known gaps
//!
//! * No interrupt is ever delivered: the GIC is mapped and programmed, but
//!   nothing yet asks it [`Gic::signal`] between slices (milestone 2).
//! * `wfi` makes Unicorn return from `emu_start` early; the next slice resumes
//!   after it, which the architecture allows (a spurious wake) but which spins
//!   rather than sleeps.
//! * Code the VPU writes into RAM the ARM has already translated is not seen
//!   by the ARM: QEMU tracks self-modifying code only for its own guest's
//!   stores. Nothing does that today.

use std::collections::{BTreeMap, VecDeque};
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
use crate::periph::gic::{self, Accessor};
use crate::periph::{armlocal, ArmLocal, Gic};

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

/// `PSTATE.{D,A,I,F}`, and `M` for ELx with `SP_ELx`.
const PSTATE_DAIF: u64 = 0xF << 6;
fn pstate_elh(el: u32) -> u64 {
    (u64::from(el) << 2) | 1
}

/// How many recent MMIO accesses to keep for the "where did it stop" report.
const RECENT_MMIO: usize = 32;

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

/// Put `earlycon` on the kernel command line, in the device tree the firmware
/// already placed, before the ARM is released.
///
/// Why here and not in `cmdline.txt`: the firmware echoes `cmdline.txt` into
/// its log (`Read command line from file 'cmdline.txt':`, the line itself, then
/// `brfs: File read: 97 bytes`), so a changed `cmdline.txt` moves the firmware
/// regression's golden transcript. Patching `/chosen/bootargs` after the
/// firmware is done leaves every byte it printed alone.
///
/// A bare `earlycon` resolves through `/chosen/stdout-path` =
/// `"serial0:115200n8"` and the `serial0` alias to the PL011 at `0x7e201000`.
///
/// The blob grows by a few bytes and is rewritten in place, so the armstub's
/// `dtb_ptr32` stays what the firmware wrote. The bytes it grows into must be
/// zero — unused — or this refuses. Linux reserves the blob by the header's
/// new `totalsize`. Returns the old and new command lines.
pub fn add_earlycon(machine: &mut Machine, dtb: u32) -> Result<(String, String), String> {
    let blob = read_dtb(machine, dtb)?;
    let fdt = Fdt::parse(&blob)?;
    let old = fdt
        .properties_of("/chosen")
        .and_then(|p| p.into_iter().find(|p| p.name == "bootargs"))
        .and_then(|p| p.as_str())
        .ok_or("no /chosen/bootargs to add earlycon to")?;
    if old
        .split_whitespace()
        .any(|a| a == "earlycon" || a.starts_with("earlycon="))
    {
        return Ok((old.clone(), old));
    }
    let new = format!("earlycon {old}");
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

/// An exception the core took (by hand, see the module docs).
#[derive(Debug, Clone)]
pub struct ExceptionTaken {
    pub kind: &'static str,
    pub pc: u64,
    pub from_el: u32,
    pub to_el: u32,
    pub esr: u64,
    pub vector: u64,
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

/// Everything the ARM's callbacks reach: the VPU model and its bus, and the
/// devices only the ARM sees. It is Unicorn's user data, so `mmio_map` and hook
/// callbacks get it as `uc.get_data_mut()`.
pub struct ArmSide {
    pub emu: Emulator,
    pub gic: Gic,
    pub local: ArmLocal,
    /// The A72's IMPLEMENTATION DEFINED system registers Unicorn lacks
    /// (`op0 = 3`, `CRn` 11 or 15), as plain storage keyed by encoding. Reset
    /// values are not modelled: the armstub writes the two it touches before
    /// anything reads them, and nothing after it reads them at all.
    pub impdef: BTreeMap<String, u64>,
    pub mmio_faults: Vec<String>,
    pub recent_mmio: VecDeque<MmioAccess>,
    pub exceptions: Vec<ExceptionTaken>,
    pub exceptions_taken: u64,
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
/// syndrome in `ESR_ELx`, switch to ELxh with `DAIF` masked, and branch to the
/// vector. Returns the vector address.
fn take_exception(
    uc: &mut Unicorn<'_, ArmSide>,
    to_el: u32,
    esr: u64,
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
    // Synchronous exception: +0x000 current EL with SP_EL0, +0x200 current EL
    // with SP_ELx, +0x400 lower EL using AArch64 (everything here is AArch64).
    let offset = match (t == from_el, spsel) {
        (true, false) => 0x000,
        (true, true) => 0x200,
        (false, _) => 0x400,
    };
    uc.reg_write([R::ELR_EL0, R::ELR_EL1, R::ELR_EL2, R::ELR_EL3][t], ret)?;
    uc.reg_write([R::ESR_EL0, R::ESR_EL1, R::ESR_EL2, R::ESR_EL3][t], esr)?;
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
    let vector = vbar + offset;
    uc.set_pc(vector)?;
    Ok(vector)
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
    let vector = take_exception(uc, to_el, esr, pc).map_err(engine)?;
    let side = uc.get_data_mut();
    side.exceptions_taken += 1;
    if side.exceptions.len() < 64 {
        side.exceptions.push(ExceptionTaken {
            kind,
            pc,
            from_el: el,
            to_el,
            esr,
            vector,
        });
    }
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
        access32(size, write, |d, w, store| match store {
            Some(v) => dev.write(o + d, w, v).map(|()| 0),
            None => dev.read(o + d, w),
        })
    } else if (GIC_OFF..GIC_OFF + u64::from(gic::SIZE)).contains(&off) {
        let (g, o) = (&mut side.gic, (off - GIC_OFF) as u32);
        access32(size, write, |d, w, store| match store {
            Some(v) => g.write_as(acc, o + d, w, v).map(|()| 0),
            None => g.read_as(acc, o + d, w),
        })
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
    /// ARM instructions per slice.
    pub arm_slice: usize,
    /// VPU steps (both cores together) per slice.
    pub vpu_slice: u64,
}

impl Default for Schedule {
    /// A million ARM instructions to twenty thousand VPU steps. The firmware
    /// is parked in its ThreadX idle loop after `arm_loader` and only has to
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
    pub insns: u64,
    pub vpu_steps: u64,
    pub slices: u64,
    /// What the ARM wrote to the console UART.
    pub console: Vec<u8>,
    /// What the VPU wrote to it during its slices (it has handed the UART over
    /// by now, so this should stay empty).
    pub vpu_console: Vec<u8>,
    pub wall: Duration,
}

/// An aarch64 core sharing an [`Emulator`]'s RAM and bus.
pub struct ArmCore {
    uc: Unicorn<'static, ArmSide>,
    /// ARM instructions executed so far (slices that ran to their count).
    pub insns: u64,
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
            impdef: BTreeMap::new(),
            mmio_faults: Vec::new(),
            recent_mmio: VecDeque::new(),
            exceptions: Vec::new(),
            exceptions_taken: 0,
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
        uc.add_mem_hook(
            HookType::MEM_READ_UNMAPPED
                | HookType::MEM_WRITE_UNMAPPED
                | HookType::MEM_FETCH_UNMAPPED,
            1,
            0,
            on_unmapped,
        )
        .map_err(uce)?;
        Ok(ArmCore { uc, insns: 0 })
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

    /// Release the core the way the SoC does: at physical 0, where
    /// `arm_loader` put the armstub, in EL3h with `DAIF` masked.
    pub fn reset(&mut self) -> Result<(), String> {
        self.uc
            .reg_write(RegisterARM64::PSTATE, PSTATE_DAIF | pstate_elh(3))
            .map_err(uce)?;
        rebuild_hflags(&mut self.uc).map_err(uce)?;
        self.uc.set_pc(0).map_err(uce)
    }

    /// Run up to `count` ARM instructions.
    pub fn run_slice(&mut self, count: usize) -> Result<(), ArmStop> {
        let pc = self.pc();
        let r = self.uc.emu_start(pc, u64::MAX, 0, count);
        if let Some(stop) = self.side().stop.take() {
            return Err(stop);
        }
        match r {
            Ok(()) => {
                self.insns += count as u64;
                Ok(())
            }
            Err(e) => {
                let unmapped = self.side().unmapped.take();
                Err(ArmStop::Engine {
                    error: uce(e),
                    pc: self.pc(),
                    unmapped,
                })
            }
        }
    }

    /// Alternate ARM and VPU slices until the ARM stops or a limit is hit.
    pub fn run(&mut self, sched: Schedule, lim: &ArmLimits) -> ArmReport {
        let start = Instant::now();
        let insns0 = self.insns;
        let vpu_retired = |e: &Emulator| e.cpu.retired + e.cpu1.as_ref().map_or(0, |c| c.retired);
        let vpu0 = vpu_retired(&self.side().emu);
        let mut console = Vec::new();
        let mut vpu_console = Vec::new();
        let mut slices = 0u64;
        let end = loop {
            slices += 1;
            let r = self.run_slice(sched.arm_slice);
            // Drain before the VPU slice runs: `Emulator::run` drains the same
            // UART into its own report.
            let out = self.side().emu.machine.take_console_output();
            if lim.live_console && !out.is_empty() {
                let _ = std::io::stderr().write_all(&out);
            }
            console.extend_from_slice(&out);
            if let Err(stop) = r {
                break ArmEnd::Stopped(stop);
            }

            let emu = &mut self.side().emu;
            let vl = RunLimits {
                max_steps: Some(vpu_retired(emu) + sched.vpu_slice),
                max_wall: None,
                stop_pc: None,
                // The firmware is in its idle loop by design now: neither the
                // spin detector nor the silence watchdog means anything.
                idle_spin_limit: 0,
                silent_us: 0,
            };
            let rep = emu.run(&vl);
            vpu_console.extend_from_slice(&rep.console);
            if rep.end != RunEnd::StepLimit {
                break ArmEnd::VpuStopped(rep.end);
            }
            if self.insns - insns0 >= lim.max_insns {
                break ArmEnd::InsnLimit;
            }
            if start.elapsed() >= lim.max_wall {
                break ArmEnd::TimeLimit;
            }
        };
        let vpu_steps = vpu_retired(&self.side().emu) - vpu0;
        ArmReport {
            end,
            insns: self.insns - insns0,
            vpu_steps,
            slices,
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

    #[test]
    fn earlycon_goes_onto_the_command_line_in_place() {
        let mut m = Machine::new(RAM);
        let blob = crate::fdt::tests::sample();
        m.ram.write_slice(0x8000, &blob).unwrap();
        let (old, new) = add_earlycon(&mut m, 0x8000).unwrap();
        assert_eq!((old.as_str(), new.as_str()), ("hi", "earlycon hi"));
        let back = read_dtb(&m, 0x8000).unwrap();
        let props = Fdt::parse(&back).unwrap().properties_of("/chosen").unwrap();
        assert_eq!(props[0].as_str().as_deref(), Some("earlycon hi"));
        // Idempotent.
        assert_eq!(add_earlycon(&mut m, 0x8000).unwrap().0, "earlycon hi");

        // Something right after the blob: refuse rather than overwrite it.
        let mut m = Machine::new(RAM);
        m.ram.write_slice(0x8000, &blob).unwrap();
        m.ram
            .store(0x8000 + blob.len() as u32 + 2, Width::Byte, 0xAA)
            .unwrap();
        assert!(add_earlycon(&mut m, 0x8000).is_err());
    }
}
