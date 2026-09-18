//! The ARM's GIC-400 interrupt controller — distributor and CPU interface.
//!
//! Nothing on the VPU side ever touches this block; it exists for the ARM
//! cores that run Linux against the live firmware (#40). The core asks this
//! model "is anything pending for CPU n" ([`Gic::signal`]) and does the
//! exception entry itself (the rules are in [`crate::aarch64`]). It first ran
//! under an in-process Unicorn core on the `arm-unicorn` branch (PR #36),
//! which took Linux through its scheduler with it. Whether Linux's scheduler
//! ever runs comes down to the
//! acknowledge/EOI state machine here being right: an interrupt that never
//! leaves the active state, or a level line that never becomes pending again,
//! hangs the kernel silently.
//!
//! ## What Linux will use — ground truth
//!
//! From the device tree **this firmware hands to Linux** (`boot … --dump-fdt`,
//! decoded with `dtc`), not from other Pi generations' documentation:
//!
//! ```text
//!   / { interrupt-parent = <0x01>; ... }                 // every device -> the GIC
//!   soc { ranges = <... 0x40000000 0x00 0xff800000 0x800000>; }
//!   interrupt-controller@40041000 {
//!       interrupt-controller;
//!       #interrupt-cells = <0x03>;
//!       compatible = "arm,gic-400";
//!       reg = <0x40041000 0x1000     // GICD -> 0xFF84_1000
//!              0x40042000 0x2000     // GICC -> 0xFF84_2000, 8 KiB
//!              0x40044000 0x2000     // GICH (virtualisation control)
//!              0x40046000 0x2000>;   // GICV (virtual CPU interface)
//!       interrupts = <0x01 0x09 0xf04>;          // maintenance: PPI 9 = ID 25
//!       phandle = <0x01>;
//!   };
//!   timer { compatible = "arm,armv8-timer";
//!           interrupts = <1 13 0xf08  1 14 0xf08  1 11 0xf08  1 10 0xf08>; };
//!                        // IDs 29 secure-phys, 30 NS-phys, 27 virt, 26 hyp; level
//!   mailbox@7e00b880 { interrupts = <0x00 0x21 0x04>; };  // SPI 33 = ID 65, level
//!   serial@7e201000  { interrupts = <0x00 0x79 0x04>; };  // SPI 121 = ID 153, level
//! ```
//!
//! The reference board (a Raspberry Pi 4B d03115) agrees, and settles which of
//! the four timer lines matters — `/proc/interrupts` there:
//!
//! ```text
//!    9:  ...  GICv2  25 Level  vgic
//!   11:  ...  GICv2  30 Level  arch_timer
//!   12:  ...  GICv2  27 Level  kvm guest vtimer
//!   14:  ...  GICv2  65 Level  fe00b880.mailbox
//!   39:  ...  GICv2 153 Level  uart-pl011
//! ```
//!
//! and its `dmesg` has `CPU: All CPU(s) started at EL2`, `arch_timer: cp15
//! timer(s) running at 54.00MHz (phys)` and `GIC: Using split EOI/Deactivate
//! mode`. So: Linux enters at EL2 without VHE (Cortex-A72 is ARMv8.0) and binds
//! the **non-secure physical timer, ID 30**; and because the GICC window in the
//! dtb is 8 KiB and the kernel is in hyp mode, `irq-gic` sets `EOImodeNS` and
//! deactivates through `GICC_DIR` at `+0x1000`. Both halves of that split
//! (priority drop on `EOIR`, deactivation on `DIR`) are modelled.
//!
//! ## Identification registers
//!
//! Read off the reference board through `/dev/mem` — only these three, which
//! are read-only and side-effect free (reading `GICC_IAR` would acknowledge a
//! live interrupt):
//!
//! ```text
//!   0xFF841004  GICD_TYPER  0x0000FC67   256 IDs, 4 CPUs, Security Extensions, LSPI 31
//!   0xFF841008  GICD_IIDR   0x0200143B   ARM (0x43B), GIC-400 distributor r0p1
//!   0xFF8420FC  GICC_IIDR   0x0202143B   ARM, GICv2 CPU interface
//! ```
//!
//! Everything else is the architecture (ARM IHI 0048B, GICv2) and the GIC-400
//! TRM, not a measurement: 32 priority levels (the low three bits of every
//! priority read as zero), secure/non-secure minimum binary points 2 and 3,
//! SGI configuration fixed at edge.
//!
//! ## Security state, and why accesses carry it
//!
//! `TYPER.SecurityExtn` is set, and the firmware relies on it. The armstub
//! start4 puts at ARM address 0 (file offset `0x1E40EC` in `start4.elf`, the
//! upstream `armstub8` shape) runs in EL3 and, per core:
//!
//! ```text
//!   GICD_CTLR      = 3         (secondary cores only)
//!   GICC_CTLR      = 0x1e7     EnableGrp0|EnableGrp1|AckCtl|bypass disables
//!   GICC_PMR       = 0xff
//!   GICD_IGROUPR0..7 = ~0      every interrupt -> group 1 (non-secure)
//! ```
//!
//! then sets `SCR_EL3.NS` and `eret`s to EL2. From there on Linux is a
//! non-secure master and sees the banked, non-secure views: `GICD_CTLR` bit 0
//! is `EnableGrp1`, priorities and `PMR` are shifted, and group-0 interrupts
//! are invisible. A model without the distinction breaks either way round: if
//! Linux's `GICD_CTLR = 1` is taken as a secure write it enables group 0 only,
//! and every interrupt the armstub moved to group 1 is never forwarded.
//!
//! [`MmioDevice`] has no notion of who is accessing, so the integration tells
//! the model with [`Gic::set_accessor`] before dispatching an access (or calls
//! the explicit [`Gic::read_as`] / [`Gic::write_as`]). The default is CPU 0,
//! secure — an ARM core comes out of reset in EL3. If Linux is entered directly
//! without the armstub, leaving every access secure also works: nothing is
//! moved to group 1, and the secure view of each register Linux writes is a
//! superset of the non-secure one at the same bit positions.
//!
//! ## Virtualisation interface
//!
//! A kernel UEFI starts runs at EL2 with KVM on, and `vgic_v2_probe` reads
//! `GICH_VTR` for the number of list registers. With nothing at `+0x4000`
//! that read took an external abort and killed init (#58); the reference
//! board's `dmesg` has `kvm [1]: vgic interrupt IRQ9` and `Hyp nVHE mode
//! initialized successfully`.
//!
//! GICH at `+0x4000` is the accessing CPU's own control block, and `+0x5000 +
//! 0x200 × n` is CPU n's. Each has four list registers (`GICH_VTR` =
//! `0x9000_0003` from the GIC-400 TRM: 4 list registers, 5 preemption and 5
//! priority bits). `MISR`, `EISR` and `ELRSR` are computed from `HCR`, `VMCR`
//! and the list registers, and `HCR.En` with a non-zero `MISR` holds the
//! maintenance interrupt (ID 25) high on that CPU.
//!
//! ## Not modelled
//!
//! * GICV (`+0x6000..+0x8000`), the virtual CPU interface: only a guest
//!   reaches it, and nothing here runs one. It reads as zero and ignores
//!   writes.
//! * The legacy bypass path (`GICC_CTLR` bypass-disable bits are stored, but a
//!   disabled CPU interface simply signals nothing).
//! * Peripheral ID registers (`0xFD0..0xFFC`) read as zero: Linux's GICv2
//!   driver does not read them, and they were not among the registers it was
//!   safe to read on the board.
//! * `GICD_PPISR`/`SPISR` (GIC-400 status registers), and the priority-drop
//!   ordering corner cases where software EOIs out of order.

use crate::bus::{BusError, BusResult, MmioDevice, Width};
// Distributor registers are offsets from GICD, CPU-interface ones from GICC.
// The three ID words are measured on the reference board (module docs).
use crate::spec::gicc::{
    ABPR as C_ABPR, AEOIR as C_AEOIR, AHPPIR as C_AHPPIR, AIAR as C_AIAR, APR0 as C_APR0,
    BPR as C_BPR, CTLR as C_CTLR, CTLR_ACKCTL_MASK as CTLR_ACKCTL, CTLR_CBPR_MASK as CTLR_CBPR,
    CTLR_ENABLE_GRP0_MASK as CTLR_ENABLE_GRP0, CTLR_ENABLE_GRP1_MASK as CTLR_ENABLE_GRP1,
    CTLR_EOIMODE_NS_MASK as CTLR_EOIMODE_NS, CTLR_EOIMODE_S_MASK as CTLR_EOIMODE_S,
    CTLR_FIQEN_MASK as CTLR_FIQEN, DIR as C_DIR, EOIR as C_EOIR, HPPIR as C_HPPIR, IAR as C_IAR,
    IIDR as C_IIDR, IIDR_RESET as GICC_IIDR, NSAPR0 as C_NSAPR0, PMR as C_PMR, RPR as C_RPR,
};
use crate::spec::gicd::{
    CPENDSGIR as D_CPENDSGIR, CTLR as D_CTLR, ICACTIVER as D_ICACTIVER, ICENABLER as D_ICENABLER,
    ICFGR as D_ICFGR, ICFGR_COUNT, ICFGR_STRIDE, ICPENDR as D_ICPENDR, IGROUPR as D_IGROUPR,
    IIDR as D_IIDR, IIDR_RESET as GICD_IIDR, IPRIORITYR as D_IPRIORITYR, ISACTIVER as D_ISACTIVER,
    ISENABLER as D_ISENABLER, ISPENDR as D_ISPENDR, ITARGETSR as D_ITARGETSR, SGIR as D_SGIR,
    SPENDSGIR as D_SPENDSGIR, SPENDSGIR_COUNT, SPENDSGIR_STRIDE, TYPER as D_TYPER,
    TYPER_RESET as TYPER,
};
// The virtual interface's registers are offsets from its own block.
use crate::spec::gich::{
    APR as H_APR, EISR0 as H_EISR0, EISR1 as H_EISR1, ELRSR0 as H_ELRSR0, ELRSR1 as H_ELRSR1,
    HCR as H_HCR, HCR_EN_MASK as HCR_EN, HCR_EOICOUNT_MASK as HCR_EOICOUNT,
    HCR_LRENPIE_MASK as HCR_LRENPIE, HCR_NPIE_MASK as HCR_NPIE, HCR_UIE_MASK as HCR_UIE,
    HCR_VGRP0DIE_MASK as HCR_VGRP0DIE, HCR_VGRP0EIE_MASK as HCR_VGRP0EIE,
    HCR_VGRP1DIE_MASK as HCR_VGRP1DIE, HCR_VGRP1EIE_MASK as HCR_VGRP1EIE, LR as H_LR, LR_COUNT,
    LR_HW_MASK as LR_HW, LR_STATE_MASK as LR_STATE, LR_STATE_SHIFT, LR_STRIDE, MISR as H_MISR,
    MISR_EOI_MASK as MISR_EOI, MISR_LRENP_MASK as MISR_LRENP, MISR_NP_MASK as MISR_NP,
    MISR_U_MASK as MISR_U, MISR_VGRP0D_MASK as MISR_VGRP0D, MISR_VGRP0E_MASK as MISR_VGRP0E,
    MISR_VGRP1D_MASK as MISR_VGRP1D, MISR_VGRP1E_MASK as MISR_VGRP1E, VMCR as H_VMCR,
    VMCR_VMGRP0EN_MASK as VMCR_VMGRP0EN, VMCR_VMGRP1EN_MASK as VMCR_VMGRP1EN, VTR as H_VTR,
    VTR_RESET as GICH_VTR,
};
use crate::spec::{gicc, gicd, gich, gicv, Coverage};

/// Every distributor register in `specs/gicd.toml` is modelled.
pub const COVERAGE_DIST: Coverage = Coverage {
    block: "gicd",
    decoded: &[
        D_CTLR,
        D_TYPER,
        D_IIDR,
        D_IGROUPR,
        D_ISENABLER,
        D_ICENABLER,
        D_ISPENDR,
        D_ICPENDR,
        D_ISACTIVER,
        D_ICACTIVER,
        D_IPRIORITYR,
        D_ITARGETSR,
        D_ICFGR,
        D_SGIR,
        D_CPENDSGIR,
        D_SPENDSGIR,
    ],
};

/// Every CPU-interface register in `specs/gicc.toml` is modelled.
pub const COVERAGE_CPU: Coverage = Coverage {
    block: "gicc",
    decoded: &[
        C_CTLR, C_PMR, C_BPR, C_IAR, C_EOIR, C_RPR, C_HPPIR, C_ABPR, C_AIAR, C_AEOIR, C_AHPPIR,
        C_APR0, C_NSAPR0, C_IIDR, C_DIR,
    ],
};

/// Every virtual-interface control register in `specs/gich.toml` is modelled.
pub const COVERAGE_VIRT: Coverage = Coverage {
    block: "gich",
    decoded: &[
        H_HCR, H_VTR, H_VMCR, H_MISR, H_EISR0, H_EISR1, H_ELRSR0, H_ELRSR1, H_APR, H_LR,
    ],
};

/// `specs/gicv.toml` lists no registers: the frame reads as zero (module docs).
pub const COVERAGE_VCPU: Coverage = Coverage {
    block: "gicv",
    decoded: &[],
};

/// Where the ARM sees the GIC-400 in low-peripheral mode — the block's own 32
/// KiB map, of which `+0x1000` is the distributor, `+0x2000` the CPU
/// interface, and `+0x4000`/`+0x6000` the virtualisation interface's control
/// block and virtual CPU interface (see the module docs for the dtb `reg`).
pub const BASE: u32 = 0xFF84_0000;
pub const SIZE: u32 = 0x8000;
pub const GICD_OFFSET: u32 = gicd::BASE - BASE;
pub const GICC_OFFSET: u32 = gicc::BASE - BASE;
const GICC_END: u32 = GICC_OFFSET + gicc::SIZE;
pub const GICH_OFFSET: u32 = gich::BASE - BASE;
pub const GICV_OFFSET: u32 = gicv::BASE - BASE;
const GICV_END: u32 = GICV_OFFSET + gicv::SIZE;
/// Past the accessing CPU's own GICH block, one per CPU, picked by address
/// bits [11:9] (the GIC-400 TRM's memory map).
const GICH_ALIAS: u32 = 0x1000;
const GICH_ALIAS_STRIDE: u32 = 0x200;

/// `GICD_TYPER.CPUNumber` = 3.
pub const NUM_CPUS: usize = 4;
const ALL_CPUS: u8 = (1 << NUM_CPUS) - 1;
/// `GICD_TYPER.ITLinesNumber` = 7: 32 × (7 + 1) interrupt IDs.
pub const NUM_IRQS: usize = 256;

/// `GICC_IAR`/`HPPIR`: nothing to acknowledge.
pub const SPURIOUS: u32 = 1023;
/// `GICC_IAR` from secure with `AckCtl` clear: the winner is a group-1
/// interrupt, which only non-secure software may take.
pub const SPURIOUS_GROUP1: u32 = 1022;

/// Interrupt IDs Linux on this board binds (dtb + `/proc/interrupts`, module
/// docs). The first 16 IDs are SGIs, 16..31 PPIs (banked per CPU), 32.. SPIs.
pub const ID_GIC_MAINTENANCE: u32 = 25;
pub const ID_HYP_TIMER: u32 = 26;
pub const ID_VIRT_TIMER: u32 = 27;
pub const ID_SEC_PHYS_TIMER: u32 = 29;
/// The one Linux actually uses as `arch_timer` (EL2, no VHE).
pub const ID_NS_PHYS_TIMER: u32 = 30;
/// The peripheral lines below come from the `[block.irq]` of the block that
/// drives each one, with that spec's provenance; the names here are how the
/// model refers to them.
pub const ID_MAILBOX: u32 = crate::spec::mbox::IRQ_GIC;
pub const ID_PL011: u32 = crate::spec::uart0::IRQ_GIC;
/// EMMC2, and the legacy EMMC with it.
pub const ID_EMMC2: u32 = crate::spec::emmc2::IRQ_GIC;
/// GENET's two lines: `INTRL2_0`, then `INTRL2_1`.
pub const ID_GENET_A: u32 = crate::spec::genet::IRQ_GIC_INTRL2_0;
pub const ID_GENET_B: u32 = crate::spec::genet::IRQ_GIC_INTRL2_1;
/// The endpoint's legacy line as the root port reports it. A Raspberry Pi 4B
/// d03115 shows it as `GICv2 175 Level PCIe PME, aerdrv`.
pub const ID_PCIE_INTA: u32 = crate::spec::pcie::IRQ_GIC_INTA;
/// The root complex's own MSI block.
pub const ID_PCIE_MSI: u32 = crate::spec::pcie::IRQ_GIC_MSI;
/// The BCM2711's own xHCI, the USB-C port's host controller (`otg_mode=1`).
pub const ID_XHCI_OTG: u32 = crate::spec::xhci_otg::IRQ_GIC;
/// The GPIO block's two lines, one a bank of pins.
pub const ID_GPIO_BANK0: u32 = crate::spec::gpio::IRQ_GIC_BANK0;
pub const ID_GPIO_BANK1: u32 = crate::spec::gpio::IRQ_GIC_BANK1;

/// Ends of the distributor arrays with byte or pair fields, as far as
/// [`NUM_IRQS`] implements them.
const D_IPRIORITYR_END: u32 = D_IPRIORITYR + NUM_IRQS as u32;
const D_ITARGETSR_END: u32 = D_ITARGETSR + NUM_IRQS as u32;
const D_ICFGR_END: u32 = D_ICFGR + ICFGR_COUNT * ICFGR_STRIDE;
/// End of `GICD_CPENDSGIR` / `GICD_SPENDSGIR`, one byte per SGI each.
const D_SGI_END: u32 = D_SPENDSGIR + SPENDSGIR_COUNT * SPENDSGIR_STRIDE;

// `GICC_CTLR` is stored in the secure-view layout (GICv2 4.4.1); the
// non-secure view is a remapping of four of its bits, `CTLR_NS_VIEW`.
const CTLR_SECURE_MASK: u32 = 0x7FF;
/// (non-secure bit, secure bit): EnableGrp1, FIQBypDisGrp1, IRQBypDisGrp1,
/// EOImodeNS.
const CTLR_NS_VIEW: [(u32, u32); 4] = [(0, 1), (5, 7), (6, 8), (9, 10)];

/// 32 priority levels: bits [7:3].
const PRIORITY_MASK: u8 = 0xF8;
/// Minimum binary points for 5 priority bits, and their reset values.
const BPR_S_MIN: u8 = 2;
const BPR_NS_MIN: u8 = 3;
/// `GICC_RPR` with nothing active.
const IDLE_PRIORITY: u8 = 0xFF;

/// What software can set in `GICH_HCR`, `GICH_VMCR` and a list register; the
/// rest reads as zero.
const HCR_WRITABLE: u32 = HCR_EN
    | HCR_UIE
    | HCR_LRENPIE
    | HCR_NPIE
    | HCR_VGRP0EIE
    | HCR_VGRP0DIE
    | HCR_VGRP1EIE
    | HCR_VGRP1DIE
    | HCR_EOICOUNT;
const VMCR_WRITABLE: u32 = VMCR_VMGRP0EN
    | VMCR_VMGRP1EN
    | gich::VMCR_VMACKCTL_MASK
    | gich::VMCR_VMFIQEN_MASK
    | gich::VMCR_VMCBPR_MASK
    | gich::VMCR_VEM_MASK
    | gich::VMCR_VMABP_MASK
    | gich::VMCR_VMBP_MASK
    | gich::VMCR_VMPRIMASK_MASK;
const LR_WRITABLE: u32 = gich::LR_VIRTUALID_MASK
    | gich::LR_PHYSICALID_MASK
    | gich::LR_PRIORITY_MASK
    | LR_STATE
    | gich::LR_GRP1_MASK
    | LR_HW;
/// With `HW` clear, list-register bit 19 asks for an EOI maintenance
/// interrupt once the guest deactivates the interrupt.
const LR_EOI: u32 = 1 << 19;
/// The state is pending, or pending and active.
const LR_PENDING: u32 = 1 << LR_STATE_SHIFT;

/// Which CPU, in which security state, an MMIO access comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accessor {
    pub cpu: usize,
    pub secure: bool,
}

/// What [`Gic::signal`] reports: the interrupt the CPU interface is asserting
/// towards a core, and on which line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signal {
    pub intid: u32,
    /// As stored (secure view).
    pub priority: u8,
    /// Group 0 with `GICC_CTLR.FIQEn` set; everything else is an IRQ.
    pub fiq: bool,
}

/// One interrupt's distributor state. SGIs keep their pending state per
/// source CPU in [`Gic::sgi_sources`] instead of `latch`.
#[derive(Debug, Clone, Copy, Default)]
struct Irq {
    enabled: bool,
    group1: bool,
    /// `GICD_ICFGR` Int_config[1]: edge-triggered, else level-sensitive.
    edge: bool,
    priority: u8,
    /// SPIs only; SGI/PPI targets are implicitly the owning CPU.
    targets: u8,
    /// Pending state latched by an edge or set by software. For a level
    /// interrupt this is only the software-set part; the line adds to it.
    latch: bool,
    /// The input as the peripheral drives it.
    line: bool,
    active: bool,
}

#[derive(Debug, Clone, Copy)]
struct CpuIf {
    /// Secure-view layout.
    ctlr: u32,
    pmr: u8,
    bpr_s: u8,
    bpr_ns: u8,
    /// Active priorities: bit n = group priority `n << 3` has been
    /// acknowledged and not yet dropped. The running priority is the lowest
    /// set bit.
    apr: u32,
}

impl Default for CpuIf {
    fn default() -> Self {
        CpuIf {
            ctlr: 0,
            pmr: 0,
            bpr_s: BPR_S_MIN,
            bpr_ns: BPR_NS_MIN,
            apr: 0,
        }
    }
}

/// One CPU's virtual interface control block (GICH).
#[derive(Debug, Clone, Copy, Default)]
struct VirtIf {
    hcr: u32,
    vmcr: u32,
    apr: u32,
    lr: [u32; LR_COUNT as usize],
}

impl VirtIf {
    /// The list register at `off`, if there is one.
    fn lr_index(off: u32) -> Option<usize> {
        let rel = off.checked_sub(H_LR)?;
        (rel % LR_STRIDE == 0 && rel / LR_STRIDE < LR_COUNT).then_some((rel / LR_STRIDE) as usize)
    }

    fn read(&self, off: u32) -> u32 {
        match off {
            H_HCR => self.hcr,
            H_VTR => GICH_VTR,
            H_VMCR => self.vmcr,
            H_MISR => self.misr(),
            H_EISR0 => self.eisr(),
            H_ELRSR0 => self.elrsr(),
            // No list registers past the fourth.
            H_EISR1 | H_ELRSR1 => 0,
            H_APR => self.apr,
            _ => Self::lr_index(off).map_or(0, |i| self.lr[i]),
        }
    }

    fn write(&mut self, off: u32, value: u32) {
        match off {
            H_HCR => self.hcr = value & HCR_WRITABLE,
            H_VMCR => self.vmcr = value & VMCR_WRITABLE,
            H_APR => self.apr = value,
            _ => {
                if let Some(i) = Self::lr_index(off) {
                    self.lr[i] = value & LR_WRITABLE;
                }
            }
        }
    }

    /// Bit n: list register n is done with and asks for an EOI maintenance
    /// interrupt.
    fn eisr(&self) -> u32 {
        self.lr_bits(|lr| lr & (LR_STATE | LR_HW | LR_EOI) == LR_EOI)
    }

    /// Bit n: list register n is free for the hypervisor to reuse.
    fn elrsr(&self) -> u32 {
        self.lr_bits(|lr| lr & LR_STATE == 0 && (lr & LR_HW != 0 || lr & LR_EOI == 0))
    }

    fn lr_bits(&self, f: impl Fn(u32) -> bool) -> u32 {
        self.lr
            .iter()
            .enumerate()
            .filter(|&(_, &lr)| f(lr))
            .fold(0, |bits, (i, _)| bits | 1 << i)
    }

    /// `GICH_MISR`: the EOI request, and every condition whose `HCR` enable
    /// is set.
    fn misr(&self) -> u32 {
        let valid = self.lr.iter().filter(|&&lr| lr & LR_STATE != 0).count();
        let pending = self.lr.iter().any(|&lr| lr & LR_PENDING != 0);
        let grp0 = self.vmcr & VMCR_VMGRP0EN != 0;
        let grp1 = self.vmcr & VMCR_VMGRP1EN != 0;
        let hcr = self.hcr;
        [
            (MISR_EOI, self.eisr() != 0),
            (MISR_U, hcr & HCR_UIE != 0 && valid <= 1),
            (
                MISR_LRENP,
                hcr & HCR_LRENPIE != 0 && hcr & HCR_EOICOUNT != 0,
            ),
            (MISR_NP, hcr & HCR_NPIE != 0 && !pending),
            (MISR_VGRP0E, hcr & HCR_VGRP0EIE != 0 && grp0),
            (MISR_VGRP0D, hcr & HCR_VGRP0DIE != 0 && !grp0),
            (MISR_VGRP1E, hcr & HCR_VGRP1EIE != 0 && grp1),
            (MISR_VGRP1D, hcr & HCR_VGRP1DIE != 0 && !grp1),
        ]
        .iter()
        .filter(|&&(_, on)| on)
        .fold(0, |m, &(bit, _)| m | bit)
    }
}

pub struct Gic {
    /// `GICD_CTLR`, secure view: bit 0 EnableGrp0, bit 1 EnableGrp1.
    ctlr: u32,
    /// SGIs and PPIs, banked per CPU.
    private: [[Irq; 32]; NUM_CPUS],
    /// SPIs, `intid - 32`.
    spi: Vec<Irq>,
    /// `[target][sgi]`: bitmap of CPUs that have an SGI pending towards
    /// `target` — each source is a separate pending instance, and
    /// `GICC_IAR` reports which one it took in bits [12:10].
    sgi_sources: [[u8; 16]; NUM_CPUS],
    cpu: [CpuIf; NUM_CPUS],
    /// GICH, banked per CPU.
    virt: [VirtIf; NUM_CPUS],
    accessor: Accessor,
}

impl Default for Gic {
    fn default() -> Self {
        Gic::new()
    }
}

impl Gic {
    pub fn new() -> Gic {
        let sgi_edge = Irq {
            edge: true,
            ..Irq::default()
        };
        let mut banked = [Irq::default(); 32];
        banked[..16].fill(sgi_edge);
        Gic {
            ctlr: 0,
            private: [banked; NUM_CPUS],
            spi: vec![Irq::default(); NUM_IRQS - 32],
            sgi_sources: [[0; 16]; NUM_CPUS],
            cpu: [CpuIf::default(); NUM_CPUS],
            virt: [VirtIf::default(); NUM_CPUS],
            accessor: Accessor {
                cpu: 0,
                secure: true,
            },
        }
    }

    /// Set who the next [`MmioDevice`] accesses come from. Banked registers
    /// (SGI/PPI state, the whole CPU interface) follow `cpu`; `secure` picks
    /// the secure or non-secure register view — flip it when the core leaves
    /// EL3 (the armstub's `eret`).
    pub fn set_accessor(&mut self, cpu: usize, secure: bool) {
        assert!(cpu < NUM_CPUS, "GIC-400 here has {NUM_CPUS} CPU interfaces");
        self.accessor = Accessor { cpu, secure };
    }

    pub fn accessor(&self) -> Accessor {
        self.accessor
    }

    /// Drive an SPI input (`intid` 32..256 — the dtb's `<0 n …>` is `32 + n`).
    /// A rising edge latches an edge-triggered interrupt pending; a
    /// level-sensitive one is pending for as long as it stays asserted.
    pub fn set_spi_level(&mut self, intid: u32, asserted: bool) {
        assert!(
            (32..NUM_IRQS as u32).contains(&intid),
            "SPI {intid} out of range"
        );
        Self::drive(&mut self.spi[intid as usize - 32], asserted);
    }

    /// Drive a PPI input of one CPU (`intid` 16..32 — the dtb's `<1 n …>` is
    /// `16 + n`). The generic timer's lines live here, one per core.
    pub fn set_ppi_level(&mut self, cpu: usize, intid: u32, asserted: bool) {
        assert!((16..32).contains(&intid), "PPI {intid} out of range");
        Self::drive(&mut self.private[cpu][intid as usize], asserted);
    }

    fn drive(s: &mut Irq, asserted: bool) {
        if s.edge && asserted && !s.line {
            s.latch = true;
        }
        s.line = asserted;
    }

    /// The highest-priority pending interrupt the CPU interface of `cpu` is
    /// signalling: enabled, pending and not active, targeted at this CPU, its
    /// group enabled in both `GICD_CTLR` and `GICC_CTLR`, priority below
    /// `GICC_PMR`, and able to preempt the running priority. `None` = the
    /// core's IRQ and FIQ inputs are both low.
    pub fn signal(&self, cpu: usize) -> Option<Signal> {
        let (intid, priority, group1) = self.candidate(cpu)?;
        let fiq = !group1 && self.cpu[cpu].ctlr & CTLR_FIQEN != 0;
        Some(Signal {
            intid,
            priority,
            fiq,
        })
    }

    pub fn irq_asserted(&self, cpu: usize) -> bool {
        self.signal(cpu).is_some_and(|s| !s.fiq)
    }

    pub fn fiq_asserted(&self, cpu: usize) -> bool {
        self.signal(cpu).is_some_and(|s| s.fiq)
    }

    /// [`MmioDevice::read`] on behalf of a specific accessor.
    pub fn read_as(&mut self, acc: Accessor, offset: u32, width: Width) -> BusResult<u32> {
        match offset {
            0..GICD_OFFSET => Ok(0),
            GICD_OFFSET..GICC_OFFSET => self.dist_read(acc, offset - GICD_OFFSET, width),
            GICC_OFFSET..GICC_END => {
                word_only(offset, width, false)?;
                Ok(self.cpu_read(acc, offset - GICC_OFFSET))
            }
            GICH_OFFSET..GICV_OFFSET => {
                word_only(offset, width, false)?;
                Ok(gich_target(acc, offset).map_or(0, |(cpu, off)| self.virt[cpu].read(off)))
            }
            // No guest runs here to use it (module docs).
            GICV_OFFSET..GICV_END => Ok(0),
            _ => Err(outside(offset, width, false)),
        }
    }

    /// [`MmioDevice::write`] on behalf of a specific accessor.
    pub fn write_as(
        &mut self,
        acc: Accessor,
        offset: u32,
        width: Width,
        value: u32,
    ) -> BusResult<()> {
        match offset {
            0..GICD_OFFSET => Ok(()),
            GICD_OFFSET..GICC_OFFSET => self.dist_write(acc, offset - GICD_OFFSET, width, value),
            GICC_OFFSET..GICC_END => {
                word_only(offset, width, true)?;
                self.cpu_write(acc, offset - GICC_OFFSET, value);
                Ok(())
            }
            GICH_OFFSET..GICV_OFFSET => {
                word_only(offset, width, true)?;
                if let Some((cpu, off)) = gich_target(acc, offset) {
                    self.virt[cpu].write(off, value);
                    self.update_maintenance(cpu);
                }
                Ok(())
            }
            GICV_OFFSET..GICV_END => Ok(()),
            _ => Err(outside(offset, width, true)),
        }
    }

    // ---- interrupt state -------------------------------------------------

    fn irq(&self, cpu: usize, id: u32) -> &Irq {
        match id {
            0..32 => &self.private[cpu][id as usize],
            _ => &self.spi[id as usize - 32],
        }
    }

    fn irq_mut(&mut self, cpu: usize, id: u32) -> &mut Irq {
        match id {
            0..32 => &mut self.private[cpu][id as usize],
            _ => &mut self.spi[id as usize - 32],
        }
    }

    fn pending(&self, cpu: usize, id: u32) -> bool {
        if id < 16 {
            return self.sgi_sources[cpu][id as usize] != 0;
        }
        let s = self.irq(cpu, id);
        s.latch || (!s.edge && s.line)
    }

    /// Whether a non-secure access may see this interrupt's fields: group-0
    /// interrupts are secure, and their bits are RAZ/WI to non-secure masters.
    fn visible(&self, acc: Accessor, id: u32) -> bool {
        acc.secure || self.irq(acc.cpu, id).group1
    }

    /// Mask applied to a priority to get its group priority — the part that
    /// takes part in preemption — from the binary point that governs its group.
    fn preempt_mask(&self, cpu: usize, group1: bool) -> u8 {
        let c = &self.cpu[cpu];
        let shift = if !group1 || c.ctlr & CTLR_CBPR != 0 {
            c.bpr_s + 1
        } else {
            c.bpr_ns
        };
        (0xFFu32 << shift) as u8
    }

    fn running_priority(&self, cpu: usize) -> u8 {
        match self.cpu[cpu].apr {
            0 => IDLE_PRIORITY,
            apr => (apr.trailing_zeros() << 3) as u8,
        }
    }

    /// One line of CPU `cpu`'s interrupt state, for the run report: its
    /// running priority and mask, and which interrupts are active, pending, or
    /// have their line held high.
    pub fn describe(&self, cpu: usize) -> String {
        let ids = |f: &dyn Fn(u32) -> bool| {
            (0..NUM_IRQS as u32)
                .filter(|&id| f(id))
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        let targeted = |id: u32| id < 32 || self.irq(cpu, id).targets & (1 << cpu) != 0;
        format!(
            "rpr {:#04x} pmr {:#04x} active [{}] pending [{}] line [{}]",
            self.running_priority(cpu),
            self.cpu[cpu].pmr,
            ids(&|id| targeted(id) && self.irq(cpu, id).active),
            ids(&|id| targeted(id) && self.pending(cpu, id)),
            ids(&|id| id >= 16 && targeted(id) && self.irq(cpu, id).line),
        )
    }

    /// `(intid, priority, group1)` of what the CPU interface would signal.
    fn candidate(&self, cpu: usize) -> Option<(u32, u8, bool)> {
        let c = &self.cpu[cpu];
        let mut best: Option<(u32, u8, bool)> = None;
        for id in 0..NUM_IRQS as u32 {
            let s = self.irq(cpu, id);
            if !s.enabled || s.active || !self.pending(cpu, id) {
                continue;
            }
            if id >= 32 && s.targets & (1 << cpu) == 0 {
                continue;
            }
            let group = if s.group1 {
                CTLR_ENABLE_GRP1
            } else {
                CTLR_ENABLE_GRP0
            };
            // The distributor's and CPU interface's enables share bit
            // positions.
            if self.ctlr & group == 0 || c.ctlr & group == 0 {
                continue;
            }
            // Strictly lower wins; on a tie the lower ID, which the scan
            // order gives for free.
            if best.is_none_or(|(_, p, _)| s.priority < p) {
                best = Some((id, s.priority, s.group1));
            }
        }
        let (id, priority, group1) = best?;
        if priority >= c.pmr {
            return None;
        }
        if priority & self.preempt_mask(cpu, group1) >= self.running_priority(cpu) {
            return None;
        }
        Some((id, priority, group1))
    }

    /// `GICC_HPPIR`: the ID `acknowledge` would return, without taking it.
    fn highest_pending(&self, cpu: usize, secure: bool) -> u32 {
        let Some((id, _, group1)) = self.candidate(cpu) else {
            return SPURIOUS;
        };
        if let Some(refused) = self.refuse(cpu, secure, group1) {
            return refused;
        }
        if id < 16 {
            id | (self.sgi_sources[cpu][id as usize].trailing_zeros() << 10)
        } else {
            id
        }
    }

    /// Whether this security state may take an interrupt of this group.
    fn refuse(&self, cpu: usize, secure: bool, group1: bool) -> Option<u32> {
        match (secure, group1) {
            (false, false) => Some(SPURIOUS),
            (true, true) if self.cpu[cpu].ctlr & CTLR_ACKCTL == 0 => Some(SPURIOUS_GROUP1),
            _ => None,
        }
    }

    /// `GICC_IAR`: pending -> active (or active-and-pending, for a level line
    /// still asserted or an SGI with more sources), and the priority joins the
    /// active priorities so only something higher can preempt it.
    fn acknowledge(&mut self, cpu: usize, secure: bool) -> u32 {
        let Some((id, priority, group1)) = self.candidate(cpu) else {
            return SPURIOUS;
        };
        if let Some(refused) = self.refuse(cpu, secure, group1) {
            return refused;
        }
        let mut value = id;
        if id < 16 {
            let sources = &mut self.sgi_sources[cpu][id as usize];
            let src = sources.trailing_zeros();
            *sources &= !(1 << src);
            value |= src << 10;
        }
        let s = self.irq_mut(cpu, id);
        s.latch = false;
        s.active = true;
        let group_priority = priority & self.preempt_mask(cpu, group1);
        self.cpu[cpu].apr |= 1 << (group_priority >> 3);
        value
    }

    /// `GICC_EOIR`: drop the running priority; with `EOImode` clear, also
    /// deactivate. A level line still asserted is pending again the moment it
    /// is inactive — `pending` recomputes it from the line.
    fn end_of_interrupt(&mut self, cpu: usize, secure: bool, value: u32) {
        let id = value & 0x3FF;
        if id as usize >= NUM_IRQS {
            return;
        }
        if !secure && !self.irq(cpu, id).group1 {
            return;
        }
        let c = &mut self.cpu[cpu];
        // The highest active priority is the one being completed, provided
        // software EOIs in nesting order (which Linux does).
        c.apr &= c.apr.wrapping_sub(1);
        let split = if secure {
            CTLR_EOIMODE_S
        } else {
            CTLR_EOIMODE_NS
        };
        if c.ctlr & split == 0 {
            self.irq_mut(cpu, id).active = false;
        }
    }

    /// `GICC_DIR`: the deactivation half of split EOI mode.
    fn deactivate(&mut self, cpu: usize, secure: bool, value: u32) {
        let id = value & 0x3FF;
        if id as usize >= NUM_IRQS || (!secure && !self.irq(cpu, id).group1) {
            return;
        }
        self.irq_mut(cpu, id).active = false;
    }

    /// `GICD_SGIR`.
    fn send_sgi(&mut self, acc: Accessor, value: u32) {
        let sgi = (value & 0xF) as usize;
        let targets = match (value >> 24) & 3 {
            0 => (value >> 16) as u8 & ALL_CPUS,
            1 => ALL_CPUS & !(1 << acc.cpu),
            2 => 1 << acc.cpu,
            _ => return,
        };
        // A secure write picks the group with NSATT; a non-secure one can only
        // raise group-1 SGIs. Either way the SGI is only forwarded to a target
        // on which it is configured in that group.
        let group1 = !acc.secure || value & (1 << 15) != 0;
        for t in 0..NUM_CPUS {
            if targets & (1 << t) != 0 && self.private[t][sgi].group1 == group1 {
                self.sgi_sources[t][sgi] |= 1 << acc.cpu;
            }
        }
    }

    // ---- distributor -----------------------------------------------------

    fn dist_read(&mut self, acc: Accessor, off: u32, width: Width) -> BusResult<u32> {
        if byte_accessible(off) {
            let mut v = 0;
            for i in 0..width.bytes() {
                v |= (self.dist_read_byte(acc, off + i) as u32) << (8 * i);
            }
            return Ok(v);
        }
        word_only(GICD_OFFSET + off, width, false)?;
        let bitmap = |f: &dyn Fn(u32) -> bool| -> u32 {
            let first = (off & 0x7F) / 4 * 32;
            (0..32)
                .map(|i| first + i)
                .filter(|&id| (id as usize) < NUM_IRQS && self.visible(acc, id) && f(id))
                .fold(0, |w, id| w | 1 << (id % 32))
        };
        Ok(match off {
            D_CTLR if acc.secure => self.ctlr,
            D_CTLR => (self.ctlr >> 1) & 1,
            D_TYPER => TYPER,
            D_IIDR => GICD_IIDR,
            D_IGROUPR..D_ISENABLER if acc.secure => bitmap(&|id| self.irq(acc.cpu, id).group1),
            D_IGROUPR..D_ISENABLER => 0,
            D_ISENABLER..D_ISPENDR => bitmap(&|id| self.irq(acc.cpu, id).enabled),
            D_ISPENDR..D_ISACTIVER => bitmap(&|id| self.pending(acc.cpu, id)),
            D_ISACTIVER..D_IPRIORITYR => bitmap(&|id| self.irq(acc.cpu, id).active),
            D_ICFGR..D_ICFGR_END => {
                let first = (off - D_ICFGR) / 4 * 16;
                (0..16)
                    .map(|i| (i, first + i))
                    .filter(|&(_, id)| (id as usize) < NUM_IRQS && self.visible(acc, id))
                    .filter(|&(_, id)| self.irq(acc.cpu, id).edge)
                    .fold(0, |w, (i, _)| w | 2 << (2 * i))
            }
            _ => 0,
        })
    }

    fn dist_write(&mut self, acc: Accessor, off: u32, width: Width, value: u32) -> BusResult<()> {
        if byte_accessible(off) {
            for i in 0..width.bytes() {
                self.dist_write_byte(acc, off + i, (value >> (8 * i)) as u8);
            }
            return Ok(());
        }
        word_only(GICD_OFFSET + off, width, true)?;
        let first = (off & 0x7F) / 4 * 32;
        let ids: Vec<u32> = (0..32)
            .filter(|i| value & (1 << i) != 0)
            .map(|i| first + i)
            .filter(|&id| (id as usize) < NUM_IRQS && self.visible(acc, id))
            .collect();
        match off {
            D_CTLR if acc.secure => self.ctlr = value & 3,
            D_CTLR => self.ctlr = (self.ctlr & !2) | ((value & 1) << 1),
            D_IGROUPR..D_ISENABLER if acc.secure => {
                for i in 0..32 {
                    let id = first + i;
                    if (id as usize) < NUM_IRQS {
                        self.irq_mut(acc.cpu, id).group1 = value & (1 << i) != 0;
                    }
                }
            }
            D_IGROUPR..D_ISENABLER => {}
            D_ISENABLER..D_ICENABLER => ids
                .iter()
                .for_each(|&id| self.irq_mut(acc.cpu, id).enabled = true),
            D_ICENABLER..D_ISPENDR => ids
                .iter()
                .for_each(|&id| self.irq_mut(acc.cpu, id).enabled = false),
            // SGI pending bits are read-only here; GICD_SPENDSGIR/CPENDSGIR
            // own them, per source.
            D_ISPENDR..D_ICPENDR => ids
                .iter()
                .filter(|&&id| id >= 16)
                .for_each(|&id| self.irq_mut(acc.cpu, id).latch = true),
            D_ICPENDR..D_ISACTIVER => ids
                .iter()
                .filter(|&&id| id >= 16)
                .for_each(|&id| self.irq_mut(acc.cpu, id).latch = false),
            D_ISACTIVER..D_ICACTIVER => ids
                .iter()
                .for_each(|&id| self.irq_mut(acc.cpu, id).active = true),
            D_ICACTIVER..D_IPRIORITYR => ids
                .iter()
                .for_each(|&id| self.irq_mut(acc.cpu, id).active = false),
            D_ICFGR..D_ICFGR_END => {
                let first = (off - D_ICFGR) / 4 * 16;
                for i in 0..16 {
                    let id = first + i;
                    // SGIs are always edge-triggered.
                    if id < 16 || id as usize >= NUM_IRQS || !self.visible(acc, id) {
                        continue;
                    }
                    self.irq_mut(acc.cpu, id).edge = value & (2 << (2 * i)) != 0;
                }
            }
            D_SGIR => self.send_sgi(acc, value),
            _ => {}
        }
        Ok(())
    }

    fn dist_read_byte(&self, acc: Accessor, off: u32) -> u8 {
        let (id, field) = byte_field(off);
        if id as usize >= NUM_IRQS || !self.visible(acc, id) {
            return 0;
        }
        let s = self.irq(acc.cpu, id);
        match field {
            ByteField::Priority if acc.secure => s.priority,
            // Non-secure software sees group-1 priorities shifted up a bit:
            // it only owns the lower half of the priority range.
            ByteField::Priority => s.priority << 1,
            // Banked IDs read back as "this CPU" — Linux's `gic_get_cpumask`
            // relies on it to learn its own interface number.
            ByteField::Targets if id < 32 => 1 << acc.cpu,
            ByteField::Targets => s.targets,
            ByteField::SgiSources => self.sgi_sources[acc.cpu][id as usize],
        }
    }

    fn dist_write_byte(&mut self, acc: Accessor, off: u32, v: u8) {
        let (id, field) = byte_field(off);
        if id as usize >= NUM_IRQS || !self.visible(acc, id) {
            return;
        }
        match field {
            ByteField::Priority => {
                let p = if acc.secure { v } else { (v >> 1) | 0x80 };
                self.irq_mut(acc.cpu, id).priority = p & PRIORITY_MASK;
            }
            ByteField::Targets if id < 32 => {}
            ByteField::Targets => self.irq_mut(acc.cpu, id).targets = v & ALL_CPUS,
            ByteField::SgiSources => {
                let sources = &mut self.sgi_sources[acc.cpu][id as usize];
                if off < D_SPENDSGIR {
                    *sources &= !v;
                } else {
                    *sources |= v & ALL_CPUS;
                }
            }
        }
    }

    // ---- CPU interface ---------------------------------------------------

    fn cpu_read(&mut self, acc: Accessor, off: u32) -> u32 {
        let cpu = acc.cpu;
        let c = self.cpu[cpu];
        match off {
            C_CTLR if acc.secure => c.ctlr,
            C_CTLR => CTLR_NS_VIEW
                .iter()
                .filter(|&&(_, s)| c.ctlr & (1 << s) != 0)
                .fold(0, |v, &(ns, _)| v | 1 << ns),
            C_PMR if acc.secure => c.pmr as u32,
            C_PMR => ns_priority_view(c.pmr),
            C_BPR if acc.secure => c.bpr_s as u32,
            C_BPR if c.ctlr & CTLR_CBPR != 0 => (c.bpr_s + 1).min(7) as u32,
            C_BPR => c.bpr_ns as u32,
            C_IAR => self.acknowledge(cpu, acc.secure),
            C_RPR => match self.running_priority(cpu) {
                p if acc.secure || p == IDLE_PRIORITY => p as u32,
                p => ns_priority_view(p),
            },
            C_HPPIR => self.highest_pending(cpu, acc.secure),
            // The aliased registers give secure software the non-secure
            // (group-1) behaviour; non-secure accesses are RAZ/WI.
            C_ABPR if acc.secure => c.bpr_ns as u32,
            C_AIAR if acc.secure => self.acknowledge(cpu, false),
            C_AHPPIR if acc.secure => self.highest_pending(cpu, false),
            C_APR0 | C_NSAPR0 => c.apr,
            C_IIDR => GICC_IIDR,
            _ => 0,
        }
    }

    fn cpu_write(&mut self, acc: Accessor, off: u32, value: u32) {
        let cpu = acc.cpu;
        match off {
            C_CTLR if acc.secure => self.cpu[cpu].ctlr = value & CTLR_SECURE_MASK,
            C_CTLR => {
                let c = &mut self.cpu[cpu];
                for &(ns, s) in &CTLR_NS_VIEW {
                    c.ctlr = (c.ctlr & !(1 << s)) | (((value >> ns) & 1) << s);
                }
            }
            C_PMR if acc.secure => self.cpu[cpu].pmr = value as u8 & PRIORITY_MASK,
            // Non-secure software cannot lower a mask secure software set in
            // the secure half of the range.
            C_PMR if self.cpu[cpu].pmr >= 0x80 => {
                self.cpu[cpu].pmr = ((value as u8 >> 1) | 0x80) & PRIORITY_MASK;
            }
            C_BPR if acc.secure => self.cpu[cpu].bpr_s = (value as u8 & 7).max(BPR_S_MIN),
            C_BPR if self.cpu[cpu].ctlr & CTLR_CBPR == 0 => {
                self.cpu[cpu].bpr_ns = (value as u8 & 7).max(BPR_NS_MIN);
            }
            C_EOIR => self.end_of_interrupt(cpu, acc.secure, value),
            C_ABPR if acc.secure => self.cpu[cpu].bpr_ns = (value as u8 & 7).max(BPR_NS_MIN),
            C_AEOIR if acc.secure => self.end_of_interrupt(cpu, false, value),
            C_DIR => self.deactivate(cpu, acc.secure, value),
            _ => {}
        }
    }

    // ---- virtual interface control ---------------------------------------

    /// The maintenance interrupt is a level: `HCR.En` and a non-zero `MISR`.
    fn update_maintenance(&mut self, cpu: usize) {
        let v = &self.virt[cpu];
        let line = v.hcr & HCR_EN != 0 && v.misr() != 0;
        Self::drive(&mut self.private[cpu][ID_GIC_MAINTENANCE as usize], line);
    }
}

impl MmioDevice for Gic {
    fn name(&self) -> &'static str {
        "gic-400"
    }

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32> {
        self.read_as(self.accessor, offset, width)
    }

    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()> {
        self.write_as(self.accessor, offset, width, value)
    }
}

enum ByteField {
    Priority,
    Targets,
    SgiSources,
}

/// `GICD_IPRIORITYR`, `GICD_ITARGETSR` and the SGI pending registers are
/// byte-accessible — Linux sets an SPI's affinity with a byte write — the
/// rest of the distributor is word-only.
fn byte_accessible(off: u32) -> bool {
    matches!(
        off,
        D_IPRIORITYR..D_IPRIORITYR_END | D_ITARGETSR..D_ITARGETSR_END | D_CPENDSGIR..D_SGI_END
    )
}

fn byte_field(off: u32) -> (u32, ByteField) {
    match off {
        D_IPRIORITYR..D_IPRIORITYR_END => (off - D_IPRIORITYR, ByteField::Priority),
        D_ITARGETSR..D_ITARGETSR_END => (off - D_ITARGETSR, ByteField::Targets),
        _ => (
            (off - D_CPENDSGIR) % (D_SPENDSGIR - D_CPENDSGIR),
            ByteField::SgiSources,
        ),
    }
}

/// Non-secure view of a secure-view priority value (`GICC_PMR`, `GICC_RPR`).
fn ns_priority_view(p: u8) -> u32 {
    if p < 0x80 {
        0
    } else {
        (p << 1) as u32
    }
}

fn word_only(offset: u32, width: Width, write: bool) -> BusResult<()> {
    if width == Width::Word && offset.is_multiple_of(4) {
        return Ok(());
    }
    Err(BusError::Faulted {
        addr: BASE + offset,
        width,
        write,
        reason: "GIC-400 register is word-access only",
    })
}

fn outside(offset: u32, width: Width, write: bool) -> BusError {
    BusError::Faulted {
        addr: BASE + offset,
        width,
        write,
        reason: "past the end of the GIC-400",
    }
}

/// The CPU whose GICH an access at `offset` reaches, and the register offset
/// in it: `+0x4000` is the accessor's own, `+0x5000 + 0x200 × n` CPU n's.
/// `None` for the aliases of CPUs this GIC does not have.
fn gich_target(acc: Accessor, offset: u32) -> Option<(usize, u32)> {
    let rel = offset - GICH_OFFSET;
    let Some(alias) = rel.checked_sub(GICH_ALIAS) else {
        return Some((acc.cpu, rel));
    };
    let cpu = (alias / GICH_ALIAS_STRIDE) as usize;
    (cpu < NUM_CPUS).then_some((cpu, alias % GICH_ALIAS_STRIDE))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S0: Accessor = Accessor {
        cpu: 0,
        secure: true,
    };

    fn ns(cpu: usize) -> Accessor {
        Accessor { cpu, secure: false }
    }

    fn sec(cpu: usize) -> Accessor {
        Accessor { cpu, secure: true }
    }

    fn rd(g: &mut Gic, acc: Accessor, off: u32) -> u32 {
        g.read_as(acc, off, Width::Word).unwrap()
    }

    fn wr(g: &mut Gic, acc: Accessor, off: u32, v: u32) {
        g.write_as(acc, off, Width::Word, v).unwrap()
    }

    const D: u32 = GICD_OFFSET;
    const C: u32 = GICC_OFFSET;
    const H: u32 = GICH_OFFSET;

    /// KVM's `vgic_v2_probe` and `vgic_v2_init_lrs`: the list-register count
    /// out of `GICH_VTR`, then every list register cleared (#58).
    #[test]
    fn kvm_finds_four_list_registers_and_clears_them() {
        let mut g = Gic::new();
        let vtr = rd(&mut g, ns(1), H + H_VTR);
        assert_eq!(vtr, 0x9000_0003);
        let nr_lr = (vtr & 0x3f) + 1;
        assert_eq!(nr_lr, 4);
        for i in 0..nr_lr {
            wr(&mut g, ns(1), H + H_LR + 4 * i, 0);
        }
        assert_eq!(rd(&mut g, ns(1), H + H_ELRSR0), 0xF, "all four free");
        assert_eq!(rd(&mut g, ns(1), H + H_MISR), 0);
        assert_eq!(rd(&mut g, ns(1), H + H_LR + 4 * nr_lr), 0, "no fifth");
    }

    /// `+0x5000 + 0x200 × n` is CPU n's block, `+0x4000` the accessor's own.
    #[test]
    fn the_alias_blocks_reach_each_cpus_own_list_registers() {
        let mut g = Gic::new();
        let pending = LR_PENDING | 42;
        wr(
            &mut g,
            ns(0),
            H + GICH_ALIAS + 2 * GICH_ALIAS_STRIDE + H_LR,
            pending,
        );
        assert_eq!(rd(&mut g, ns(2), H + H_LR), pending);
        assert_eq!(rd(&mut g, ns(0), H + H_LR), 0);
        assert_eq!(rd(&mut g, ns(2), H + H_ELRSR0), 0xE);
        // CPUs 4..7 are not there.
        let absent = H + GICH_ALIAS + 5 * GICH_ALIAS_STRIDE + H_VTR;
        assert_eq!(rd(&mut g, ns(0), absent), 0);
    }

    /// With `HCR.En` and `UIE`, fewer than two valid list registers is an
    /// underflow: the maintenance interrupt goes up on that CPU only, and
    /// comes down once a second list register fills.
    #[test]
    fn the_maintenance_interrupt_follows_misr() {
        let mut g = Gic::new();
        let maint = |g: &Gic, cpu: usize| g.private[cpu][ID_GIC_MAINTENANCE as usize].line;
        wr(&mut g, ns(1), H + H_HCR, HCR_EN | HCR_UIE);
        assert_eq!(rd(&mut g, ns(1), H + H_MISR), MISR_U);
        assert!(maint(&g, 1));
        assert!(!maint(&g, 0));
        wr(&mut g, ns(1), H + H_LR, LR_PENDING | 40);
        assert!(maint(&g, 1), "one valid entry is still an underflow");
        wr(&mut g, ns(1), H + H_LR + 4, LR_PENDING | 41);
        assert_eq!(rd(&mut g, ns(1), H + H_MISR), 0);
        assert!(!maint(&g, 1));
    }

    /// A list register the guest is done with that asked for an EOI
    /// maintenance interrupt: in `EISR`, and not free until handled.
    #[test]
    fn an_eoi_request_shows_until_the_hypervisor_handles_it() {
        let mut g = Gic::new();
        wr(&mut g, ns(0), H + H_LR + 8, LR_EOI | 33);
        assert_eq!(rd(&mut g, ns(0), H + H_EISR0), 1 << 2);
        assert_eq!(rd(&mut g, ns(0), H + H_ELRSR0), 0xB);
        assert_eq!(rd(&mut g, ns(0), H + H_MISR), MISR_EOI);
    }

    #[test]
    fn gicv_reads_as_zero() {
        let mut g = Gic::new();
        wr(&mut g, ns(0), GICV_OFFSET, 1);
        assert_eq!(rd(&mut g, ns(0), GICV_OFFSET), 0);
    }

    /// What the armstub start4 loads at ARM address 0 does on each core
    /// (`start4.elf` `0x1E41EC`), in EL3.
    fn armstub_setup_gic(g: &mut Gic, cpu: usize) {
        let acc = sec(cpu);
        if cpu != 0 {
            wr(g, acc, D + D_CTLR, 3);
        }
        wr(g, acc, C + C_CTLR, 0x1e7);
        wr(g, acc, C + C_PMR, 0xff);
        for n in (0..8).rev() {
            wr(g, acc, D + D_IGROUPR + 4 * n, !0);
        }
    }

    /// Linux `irq-gic.c` bring-up: `gic_dist_init` on the boot CPU, then
    /// `gic_cpu_init` + `gic_cpu_if_up` on `cpu`, in split EOI mode.
    fn linux_init(g: &mut Gic, acc: Accessor, boot: bool) {
        if boot {
            wr(g, acc, D + D_CTLR, 0);
            // `gic_get_cpumask`: fold the banked target bytes into one.
            let m = rd(g, acc, D + D_ITARGETSR);
            let mask = 0x0101_0101 * ((m | m >> 16 | m >> 8) & 0xFF);
            for n in (32..NUM_IRQS as u32).step_by(4) {
                wr(g, acc, D + D_ITARGETSR + n, mask);
            }
            for n in (32..NUM_IRQS as u32).step_by(16) {
                wr(g, acc, D + D_ICFGR + n / 4, 0);
            }
            for n in (32..NUM_IRQS as u32).step_by(4) {
                wr(g, acc, D + D_IPRIORITYR + n, 0xa0a0_a0a0);
            }
            for n in (32..NUM_IRQS as u32).step_by(32) {
                wr(g, acc, D + D_ICACTIVER + n / 8, !0);
                wr(g, acc, D + D_ICENABLER + n / 8, !0);
            }
            wr(g, acc, D + D_CTLR, 1);
        }
        wr(g, acc, D + D_ICACTIVER, !0);
        wr(g, acc, D + D_ICENABLER, 0xffff_0000);
        wr(g, acc, D + D_ISENABLER, 0x0000_ffff);
        for n in (0..32).step_by(4) {
            wr(g, acc, D + D_IPRIORITYR + n, 0xa0a0_a0a0);
        }
        wr(g, acc, C + C_PMR, 0xf0);
        let bypass = rd(g, acc, C + C_CTLR) & 0x1e0;
        wr(g, acc, C + C_CTLR, bypass | 1 | (1 << 9));
    }

    fn enable(g: &mut Gic, acc: Accessor, id: u32) {
        wr(g, acc, D + D_ISENABLER + id / 32 * 4, 1 << (id % 32));
    }

    fn set_edge(g: &mut Gic, acc: Accessor, id: u32) {
        let off = D + D_ICFGR + id / 16 * 4;
        let v = rd(g, acc, off) | 2 << (2 * (id % 16));
        wr(g, acc, off, v);
    }

    /// Linux with everything left secure (no armstub).
    fn booted() -> Gic {
        let mut g = Gic::new();
        linux_init(&mut g, S0, true);
        for cpu in 1..NUM_CPUS {
            linux_init(&mut g, sec(cpu), false);
        }
        g
    }

    #[test]
    fn id_registers_match_the_reference_board() {
        let mut g = Gic::new();
        assert_eq!(rd(&mut g, S0, D + D_TYPER), 0x0000_FC67);
        assert_eq!(rd(&mut g, S0, D + D_IIDR), 0x0200_143B);
        assert_eq!(rd(&mut g, ns(2), C + C_IIDR), 0x0202_143B);
    }

    #[test]
    fn banked_targets_read_back_the_accessing_cpu() {
        let mut g = Gic::new();
        for cpu in 0..NUM_CPUS {
            assert_eq!(rd(&mut g, sec(cpu), D + D_ITARGETSR), 0x0101_0101 << cpu);
        }
    }

    #[test]
    fn level_ppi_split_eoi_reasserts_while_the_line_is_high() {
        let mut g = booted();
        enable(&mut g, S0, ID_NS_PHYS_TIMER);
        assert_eq!(g.signal(0), None);

        g.set_ppi_level(0, ID_NS_PHYS_TIMER, true);
        assert!(g.irq_asserted(0));
        assert_eq!(rd(&mut g, S0, C + C_HPPIR), ID_NS_PHYS_TIMER);
        assert_eq!(rd(&mut g, S0, C + C_IAR), ID_NS_PHYS_TIMER);
        // Active (and, with the line still high, pending): not signalled again.
        assert_eq!(g.signal(0), None);
        assert_eq!(rd(&mut g, S0, C + C_RPR), 0xa0);

        // EOImode 1: EOIR only drops the priority.
        wr(&mut g, S0, C + C_EOIR, ID_NS_PHYS_TIMER);
        assert_eq!(rd(&mut g, S0, C + C_RPR), 0xff);
        assert_eq!(g.signal(0), None, "still active until GICC_DIR");

        // DIR deactivates; the line is still asserted, so it is pending again.
        wr(&mut g, S0, C + C_DIR, ID_NS_PHYS_TIMER);
        assert_eq!(g.signal(0).map(|s| s.intid), Some(ID_NS_PHYS_TIMER));

        // The handler reprograms the timer: line drops, nothing left.
        g.set_ppi_level(0, ID_NS_PHYS_TIMER, false);
        assert_eq!(g.signal(0), None);
        assert_eq!(rd(&mut g, S0, C + C_IAR), SPURIOUS);
    }

    #[test]
    fn level_spi_goes_pending_again_after_eoi_mode_0() {
        let mut g = booted();
        wr(&mut g, S0, C + C_CTLR, 1); // EOImode 0: EOIR also deactivates
        enable(&mut g, S0, ID_PL011);
        g.set_spi_level(ID_PL011, true);
        assert_eq!(rd(&mut g, S0, C + C_IAR), ID_PL011);
        wr(&mut g, S0, C + C_EOIR, ID_PL011);
        assert_eq!(g.signal(0).map(|s| s.intid), Some(ID_PL011));
        g.set_spi_level(ID_PL011, false);
        assert_eq!(g.signal(0), None);
    }

    /// Linux's `gic_handle_irq` loop in split EOI mode, on a level SPI whose
    /// line nobody clears: IAR, EOIR, handler, DIR, and IAR again returns the
    /// same ID without the core ever leaving the exception. A run that shows
    /// few interrupts taken, the SPI active and the running priority idle is
    /// that loop, not a lost deactivation.
    #[test]
    fn level_spi_split_eoi_is_acknowledged_again_after_each_dir() {
        let mut g = booted();
        enable(&mut g, S0, ID_PL011);
        g.set_spi_level(ID_PL011, true);
        for _ in 0..3 {
            assert_eq!(rd(&mut g, S0, C + C_IAR), ID_PL011);
            wr(&mut g, S0, C + C_EOIR, ID_PL011);
            assert_eq!(rd(&mut g, S0, C + C_RPR), 0xff);
            assert_eq!(g.signal(0), None, "active until GICC_DIR");
            wr(&mut g, S0, C + C_DIR, ID_PL011);
        }
        g.set_spi_level(ID_PL011, false);
        assert_eq!(rd(&mut g, S0, C + C_IAR), SPURIOUS);
    }

    #[test]
    fn edge_spi_is_taken_once_and_a_new_edge_waits_for_deactivation() {
        let mut g = booted();
        wr(&mut g, S0, C + C_CTLR, 1);
        let id = 32 + 96; // aon_intr, `interrupts = <0x00 0x60 0x01>`: edge
        set_edge(&mut g, S0, id);
        enable(&mut g, S0, id);

        g.set_spi_level(id, true);
        g.set_spi_level(id, false);
        assert_eq!(rd(&mut g, S0, C + C_IAR), id);
        // A second edge while active: active-and-pending, not signalled.
        g.set_spi_level(id, true);
        g.set_spi_level(id, false);
        assert_eq!(g.signal(0), None);
        wr(&mut g, S0, C + C_EOIR, id);
        assert_eq!(rd(&mut g, S0, C + C_IAR), id);
        wr(&mut g, S0, C + C_EOIR, id);
        // The edge was consumed: nothing left although nothing was lowered.
        assert_eq!(g.signal(0), None);

        // Holding the line high does not re-trigger an edge interrupt.
        g.set_spi_level(id, true);
        assert_eq!(rd(&mut g, S0, C + C_IAR), id);
        wr(&mut g, S0, C + C_EOIR, id);
        assert_eq!(g.signal(0), None);
    }

    #[test]
    fn priority_mask_and_running_priority_gate_signalling() {
        let mut g = booted();
        wr(&mut g, S0, C + C_CTLR, 1);
        let (low, high) = (ID_PL011, ID_MAILBOX);
        enable(&mut g, S0, low);
        enable(&mut g, S0, high);
        g.write_as(S0, D + D_IPRIORITYR + high, Width::Byte, 0x80)
            .unwrap();

        // Masked by PMR: nothing signalled, IAR spurious.
        wr(&mut g, S0, C + C_PMR, 0xa0);
        g.set_spi_level(low, true);
        assert_eq!(g.signal(0), None);
        assert_eq!(rd(&mut g, S0, C + C_IAR), SPURIOUS);
        wr(&mut g, S0, C + C_PMR, 0xf0);
        assert_eq!(rd(&mut g, S0, C + C_IAR), low);

        // Running at 0xa0: a higher priority preempts, an equal one does not.
        g.set_spi_level(ID_MAILBOX, true);
        assert_eq!(g.signal(0).map(|s| s.intid), Some(high));
        assert_eq!(rd(&mut g, S0, C + C_IAR), high);
        assert_eq!(rd(&mut g, S0, C + C_RPR), 0x80);
        enable(&mut g, S0, 32);
        g.set_spi_level(32, true);
        assert_eq!(g.signal(0), None, "0xa0 cannot preempt 0x80");
        // Nested EOIs unwind the priorities in order.
        g.set_spi_level(high, false);
        wr(&mut g, S0, C + C_EOIR, high);
        assert_eq!(rd(&mut g, S0, C + C_RPR), 0xa0);
        assert_eq!(g.signal(0), None, "0xa0 cannot preempt 0xa0 either");
        g.set_spi_level(low, false);
        wr(&mut g, S0, C + C_EOIR, low);
        assert_eq!(rd(&mut g, S0, C + C_IAR), 32);
    }

    #[test]
    fn a_ppi_on_one_cpu_is_invisible_to_the_others() {
        let mut g = booted();
        for cpu in 0..NUM_CPUS {
            enable(&mut g, sec(cpu), ID_NS_PHYS_TIMER);
        }
        g.set_ppi_level(1, ID_NS_PHYS_TIMER, true);
        assert_eq!(g.signal(0), None);
        assert_eq!(rd(&mut g, S0, C + C_IAR), SPURIOUS);
        assert_eq!(rd(&mut g, S0, D + D_ISPENDR), 0);
        assert_eq!(rd(&mut g, sec(1), D + D_ISPENDR), 1 << ID_NS_PHYS_TIMER);
        assert_eq!(rd(&mut g, sec(1), C + C_IAR), ID_NS_PHYS_TIMER);
        assert_eq!(g.signal(2), None);
    }

    #[test]
    fn distributor_and_cpu_interface_enables_gate_everything() {
        let mut g = booted();
        enable(&mut g, S0, ID_MAILBOX);
        g.set_spi_level(ID_MAILBOX, true);
        assert!(g.irq_asserted(0));

        wr(&mut g, S0, D + D_CTLR, 0);
        assert_eq!(g.signal(0), None);
        assert_eq!(rd(&mut g, S0, C + C_IAR), SPURIOUS);
        wr(&mut g, S0, D + D_CTLR, 1);
        assert!(g.irq_asserted(0));

        wr(&mut g, S0, C + C_CTLR, 0);
        assert_eq!(g.signal(0), None);
        assert_eq!(rd(&mut g, S0, C + C_IAR), SPURIOUS);
        wr(&mut g, S0, C + C_CTLR, 1);
        assert!(g.irq_asserted(0));

        // And the per-interrupt enable.
        wr(&mut g, S0, D + D_ICENABLER + 8, 1 << (ID_MAILBOX % 32));
        assert_eq!(g.signal(0), None);
    }

    #[test]
    fn an_spi_targeted_at_two_cpus_is_taken_by_the_first_to_acknowledge() {
        let mut g = booted();
        enable(&mut g, S0, ID_MAILBOX);
        g.write_as(S0, D + D_ITARGETSR + ID_MAILBOX, Width::Byte, 0b0110)
            .unwrap();
        g.set_spi_level(ID_MAILBOX, true);
        assert_eq!(g.signal(0), None);
        assert!(g.irq_asserted(1) && g.irq_asserted(2));
        assert_eq!(rd(&mut g, sec(2), C + C_IAR), ID_MAILBOX);
        assert_eq!(rd(&mut g, sec(1), C + C_IAR), SPURIOUS);
    }

    #[test]
    fn sgis_carry_their_source_and_honour_the_target_filter() {
        let mut g = booted();
        // cpu 3 -> cpu 1, SGI 2, target list.
        wr(&mut g, sec(3), D + D_SGIR, (0b0010 << 16) | 2);
        assert_eq!(g.signal(0), None);
        assert_eq!(rd(&mut g, sec(1), C + C_IAR), 2 | (3 << 10));
        wr(&mut g, sec(1), C + C_EOIR, 2 | (3 << 10));
        wr(&mut g, sec(1), C + C_DIR, 2 | (3 << 10));
        assert_eq!(g.signal(1), None);

        // cpu 0, "all but self".
        wr(&mut g, S0, D + D_SGIR, (1 << 24) | 5);
        assert_eq!(g.signal(0), None);
        for cpu in 1..NUM_CPUS {
            assert_eq!(rd(&mut g, sec(cpu), C + C_HPPIR), 5);
        }
        // Two sources pending at once are two instances.
        wr(&mut g, sec(2), D + D_SGIR, (0b0010 << 16) | 5);
        let first = rd(&mut g, sec(1), C + C_IAR);
        assert_eq!(first, 5);
        wr(&mut g, sec(1), C + C_EOIR, first);
        wr(&mut g, sec(1), C + C_DIR, first);
        assert_eq!(rd(&mut g, sec(1), C + C_IAR), 5 | (2 << 10));
    }

    /// The real sequence: armstub in EL3 on every core, then Linux as a
    /// non-secure master. The timer PPI must come out the other side.
    #[test]
    fn armstub_then_non_secure_linux_delivers_the_timer() {
        let mut g = Gic::new();
        for cpu in 0..NUM_CPUS {
            armstub_setup_gic(&mut g, cpu);
        }
        linux_init(&mut g, ns(0), true);
        for cpu in 1..NUM_CPUS {
            linux_init(&mut g, ns(cpu), false);
        }
        // Non-secure views: GICD_CTLR's EnableGrp1 at bit 0, shifted
        // priorities, PMR as written.
        assert_eq!(rd(&mut g, ns(0), D + D_CTLR), 1);
        assert_eq!(rd(&mut g, S0, D + D_CTLR), 3);
        assert_eq!(rd(&mut g, ns(0), D + D_IPRIORITYR), 0xa0a0_a0a0);
        assert_eq!(rd(&mut g, S0, D + D_IPRIORITYR), 0xd0d0_d0d0);
        assert_eq!(rd(&mut g, ns(0), C + C_PMR), 0xf0);

        enable(&mut g, ns(2), ID_NS_PHYS_TIMER);
        g.set_ppi_level(2, ID_NS_PHYS_TIMER, true);
        let s = g.signal(2).unwrap();
        assert_eq!((s.intid, s.fiq), (ID_NS_PHYS_TIMER, false));
        // Secure software may not take a group-1 interrupt without AckCtl —
        // the armstub set it, so clear it to check the refusal.
        let ctlr = rd(&mut g, sec(2), C + C_CTLR);
        wr(&mut g, sec(2), C + C_CTLR, ctlr & !CTLR_ACKCTL);
        assert_eq!(rd(&mut g, sec(2), C + C_IAR), SPURIOUS_GROUP1);
        let ack = rd(&mut g, ns(2), C + C_IAR);
        assert_eq!(ack, ID_NS_PHYS_TIMER);
        g.set_ppi_level(2, ID_NS_PHYS_TIMER, false);
        wr(&mut g, ns(2), C + C_EOIR, ack);
        wr(&mut g, ns(2), C + C_DIR, ack);
        assert_eq!(g.signal(2), None);
        assert_eq!(rd(&mut g, ns(2), C + C_RPR), 0xff);

        // Linux's IPIs (non-secure GICD_SGIR) reach group-1 SGIs.
        wr(&mut g, ns(0), D + D_SGIR, (0b1000 << 16) | 1);
        assert_eq!(rd(&mut g, ns(3), C + C_IAR), 1);
    }

    #[test]
    fn non_secure_software_cannot_see_or_touch_group_0() {
        let mut g = Gic::new();
        wr(&mut g, S0, D + D_CTLR, 3);
        wr(&mut g, S0, C + C_CTLR, 3);
        wr(&mut g, S0, C + C_PMR, 0xff);
        wr(&mut g, S0, D + D_ISENABLER + 8, 1 << (ID_MAILBOX % 32));
        g.write_as(S0, D + D_ITARGETSR + ID_MAILBOX, Width::Byte, 1)
            .unwrap();
        // Group 0 at reset.
        assert_eq!(rd(&mut g, ns(0), D + D_ISENABLER + 8), 0);
        wr(&mut g, ns(0), D + D_ICENABLER + 8, !0);
        assert_eq!(rd(&mut g, S0, D + D_ISENABLER + 8), 1 << (ID_MAILBOX % 32));
        g.set_spi_level(ID_MAILBOX, true);
        assert_eq!(rd(&mut g, ns(0), C + C_HPPIR), SPURIOUS);
        assert_eq!(rd(&mut g, ns(0), C + C_IAR), SPURIOUS);
        assert_eq!(rd(&mut g, S0, C + C_IAR), ID_MAILBOX);
    }

    #[test]
    fn fiq_enable_routes_group_0_to_the_fiq_line() {
        let mut g = booted();
        enable(&mut g, S0, ID_MAILBOX);
        g.set_spi_level(ID_MAILBOX, true);
        assert!(g.irq_asserted(0) && !g.fiq_asserted(0));
        wr(&mut g, S0, C + C_CTLR, 1 | CTLR_FIQEN);
        assert!(g.fiq_asserted(0) && !g.irq_asserted(0));
    }

    #[test]
    fn narrow_accesses_fault_where_the_gic_is_word_only() {
        let mut g = Gic::new();
        assert!(g.read(GICD_OFFSET + D_CTLR, Width::Byte).is_err());
        assert!(g.read(GICH_OFFSET + H_VTR, Width::Half).is_err());
        // The byte-accessible distributor registers take bytes.
        assert!(g.read(GICD_OFFSET + D_ITARGETSR + 1, Width::Byte).is_ok());
        assert!(g.read(SIZE, Width::Word).is_err(), "past the block");
    }
}
