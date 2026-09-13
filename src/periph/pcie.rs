//! BCM2711 PCIe root complex at `0x7D50_0000` — the block Linux calls
//! `pcie@7d500000` and drives with `pcie-brcmstb`.
//!
//! Behind it sits the Pi 4B's VIA VL805 xHCI controller (`1106:3483`), modelled
//! in [`super::vl805`]. See [`docs/usb-xhci.md`](../../../docs/usb-xhci.md) for
//! the register-level survey and the staged plan; this file is stages 0 and 1.
//!
//! The window sits outside the `0x7E…` legacy peripheral aperture, so without
//! it every PCIe access folds onto DRAM (`addr & 0x3FFF_FFFF` = `0x3D50_xxxx`)
//! and the firmware silently scribbles ~37 KiB into the middle of modelled
//! memory. Stage 0 fixed that with a sticky register file.
//!
//! ## What stage 1 adds
//!
//! * **`RGR1_SW_INIT_1` (`+0x9210`)** — bit 0 is PERST#, bit 1 the bridge soft
//!   reset, the same bits `pcie-brcmstb` uses. The bootcode parks the block in
//!   reset (`0x8000AB4A` writes `2` then `3`), the second-stage bootloader's
//!   `pcie_reset` (`0x000A7034`) re-asserts both, then `pcie_init` releases the
//!   bridge (`0x000A6CA2`) and finally PERST# (`0x000A6DCC`). The bridge reset
//!   also returns the MSI block to its reset state, nothing pending and every
//!   vector masked, and the block takes no messages while it is held. Linux
//!   depends on that: `brcm_pcie_setup()` never clears the block, and
//!   `brcm_msi_set_regs()` unmasks before it clears. A vector the firmware's
//!   USB traffic left pending (the VL805 signals while every vector is still
//!   masked) used to survive into Linux, and when the ARM took the interrupt
//!   between those two writes there was no handler to clear it: an interrupt
//!   storm on `GIC_SPI 148` that hung the kernel in its PCIe probe.
//! * **`MISC_PCIE_STATUS` (`+0x4068`)** — the link-up poll. The bootloader's
//!   predicate at `0x000A6F7E` is
//!
//!   ```text
//!   r2 = [0x7D504068]
//!   r3 = r2 & 0x20            ; DL_ACTIVE
//!   if r3 != 0: r2 &= 0x10    ; PHYLINKUP
//!   return (both set) ? 0 : 1 ; 0 = stop polling, link is up
//!   ```
//!
//!   which is exactly Linux's `brcm_pcie_link_up()`. Bit 7 is the port-mode bit
//!   `brcm_pcie_rc_mode()` reads; set, it means root complex. So a live link
//!   reads `0xB0`.
//! * **The config-space router.** `EXT_CFG_INDEX` (`+0x9000`) selects a
//!   `(bus, slot, fn)` — `bus << 20 | slot << 15 | fn << 12`, per
//!   `pcie-brcmstb` — and the 4 KiB window at `EXT_CFG_DATA` (`+0x8000`) is
//!   that function's configuration space. Bus 1 device 0 is the VL805.
//!   Everything else — bus 0 included, see [`Pcie::ext_target`] — reads back
//!   all-ones, which is how PCI says "nothing there". The root port's own
//!   config space is reachable only through the direct view at `+0x0000`.
//!
//! ## The outbound window, and how the VPU reaches BAR0
//!
//! It does not reach it with a load. `CPU_2_PCIE_MEM_WIN0` (`+0x400C`,
//! `+0x4010`, `+0x4070`, `+0x4080`, `+0x4084`) puts the endpoint's registers at
//! CPU-physical `0x6_0000_0000..0x6_3FFF_FFFF` — 35 bits, which a 32-bit VPU
//! cannot form — so the firmware reads and writes them with the **40-bit DMA4
//! channel** instead: `0x000A701E` builds a control block whose `SRC` is
//! `0x6_0200_0004` (`src = 0x0200_0004`, `srci = 0x1006`, high byte 6), DMAs
//! four bytes into a bounce buffer at `0xC031B000`, and reads the buffer.
//! `docs/usb-xhci.md` §5.1 has the full trace.
//!
//! So this file translates CPU-physical → PCI bus → BAR0 offset
//! ([`Pcie::mmio_read`] / [`Pcie::mmio_write`]), and
//! [`Machine::run_dma4`](crate::machine::Machine) calls it with the composed
//! 40-bit address. Until that landed the transfer read modelled DRAM, the
//! capability registers came back zero, and the bring-up hung forever at
//! `0x000AA3C0` — which is why the endpoint used to be detached by default.
//!
//! ## The endpoint is attached by default
//!
//! A Pi 4B has the VL805 soldered on, so attached is the honest model of the
//! reference board, and with BAR0 answering, the bootloader's bring-up gets the
//! same numbers a real board prints
//! (`examples-on-real-hardware/sd-card-boot.log` lines 27-32):
//!
//! ```text
//!   2.75 PCIe scan 00001106:00003483
//!   3.31 xHC0 ver: 256 HCS: 05000420 fc000031 00e70004 HCC: 002841eb
//!   3.31 USBSTS 1
//!   3.31 xHC0 ports 5 slots 32 intrs 4
//! ```
//!
//! What it does *not* find is a device: the model has no USB device behind the
//! root hub and no VIA hub on port 1, so all five ports read "powered, empty"
//! and the bootloader falls through to the SD entry of `BOOT_ORDER` without the
//! `USB2[1] … connected` / `HUB init` lines the real board prints. That is
//! stage 3 of `docs/usb-xhci.md`.
//!
//! `RVF_PCIE_DEVICE=0` unsolders the endpoint again — the link never trains,
//! `MISC_PCIE_STATUS` reads only its port-mode strap and the bootloader prints
//! `PCIe timeout: 0x00000080` / `USB xHC init failed`, the pre-stage-1
//! transcript. It is for reproducing that, nothing else.
//!
//! ## Linux
//!
//! `pcie-brcmstb` (`drivers/pci/controller/pcie-brcmstb.c`) drives the same
//! block from the ARM, at `0xFD50_0000`. What it needs beyond the bootloader:
//!
//! * the port-mode bit of `MISC_PCIE_STATUS` read as the strap it is, set with
//!   PERST# still asserted — `brcm_pcie_setup()` checks it before starting the
//!   link, and fails the probe with `PCIe RC controller misconfigured as
//!   Endpoint` when it reads clear;
//! * the root port's config space as a real type-1 header with capabilities
//!   (the `rpi-dev` dump, [`RC_CFG_SEED`]): no BARs to size, writable bus
//!   numbers and windows, the PCIe capability that makes it a root port and
//!   reports the trained link;
//! * the SerDes MDIO port, for `brcm_pcie_set_ssc()`;
//! * the MSI block. The endpoint's MSI is a memory write to the address in
//!   `MSI_BAR_CONFIG`; the root complex catches it and turns it into a bit in
//!   `MSI_INTR2` and a level on `GIC_SPI 148` ([`Pcie::msi_line`]).
//!
//! Endpoint MMIO reaches the ARM through the outbound window: [`crate::arm`]
//! routes CPU-physical `0x6_0000_0000..` here.
//!
//! ## The inbound window
//!
//! The endpoint's DMA — TRB fetches, event writes, data buffers — goes the
//! other way, through inbound window 2 (`RC_BAR2_CONFIG_LO`/`_HI`): a PCI bus
//! address inside it reaches CPU-physical memory at the same offset from 0,
//! and one outside it reaches nothing ([`Upstream`]). The bootloader programs
//! the window at bus 0, 8 GiB wide, so its bus addresses are physical; Linux
//! moves it to where `dma-ranges` puts system memory, PCI `0x4_0000_0000`
//! (`IB MEM 0x0000000000..0x003fffffff -> 0x0400000000`), and hands the
//! endpoint addresses up there. The window's size field is decoded as Linux
//! encodes it ([`ibar_size`]).
//!
//! ## Register offsets the firmware actually touches
//!
//! `0x0000..0x0FFF` root-port config space (`0x0B4`/`0x0B8`/`0x0C8`/`0x188`,
//! class code at `0x043C`), `0x4008` `MISC_MISC_CTRL`, `0x402C`/`0x4034`/
//! `0x4038`/`0x403C` the RC BAR windows, `0x4044`/`0x4048`/`0x404C` the MSI BAR
//! and data, `0x4068` `MISC_PCIE_STATUS`, `0x4204`
//! `MISC_HARD_PCIE_HARD_DEBUG`, `0x4308`/`0x4310`/`0x4314` interrupt masks,
//! `0x8000` `EXT_CFG_DATA`, `0x9000` `EXT_CFG_INDEX`, `0x9210`
//! `RGR1_SW_INIT_1`.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::periph::vl805::Vl805;
use crate::periph::xhci::HostMem;
/// `reg = <0x0 0x7d500000 0x0 0x9310>` in the Pi 4 device tree
/// (`/proc/device-tree/scb/pcie@7d500000/reg` on a real board).
pub use crate::spec::pcie::{BASE, SIZE};
// The SerDes MDIO write port has its DONE bit where the read port does.
use crate::spec::pcie::{
    EXT_CFG_DATA, EXT_CFG_DATA_COUNT, EXT_CFG_DATA_STRIDE, EXT_CFG_INDEX,
    EXT_CFG_INDEX_BUSNUM_SHIFT as EXT_BUSNUM_SHIFT, EXT_CFG_INDEX_FUNC_SHIFT as EXT_FUNC_SHIFT,
    EXT_CFG_INDEX_SLOT_SHIFT as EXT_SLOT_SHIFT, HARD_DEBUG, INTR2_CPU_CLR, INTR2_CPU_MASK_CLR,
    INTR2_CPU_MASK_SET, INTR2_CPU_MASK_STATUS, INTR2_CPU_SET, INTR2_CPU_STATUS, MDIO_ADDR,
    MDIO_ADDR_CMD_READ_MASK as MDIO_CMD_READ, MDIO_ADDR_REGAD_MASK as MDIO_REGAD, MDIO_RD_DATA,
    MDIO_RD_DATA_DONE_MASK as MDIO_DONE, MDIO_WR_DATA, MEM_WIN0_BASE_HI, MEM_WIN0_BASE_LIMIT,
    MEM_WIN0_HI, MEM_WIN0_LIMIT_HI, MEM_WIN0_LO, MISC_CTRL, MISC_PCIE_STATUS,
    MISC_PCIE_STATUS_DL_ACTIVE_MASK as STATUS_DL_ACTIVE,
    MISC_PCIE_STATUS_PHYLINKUP_MASK as STATUS_PHYLINKUP,
    MISC_PCIE_STATUS_PORT_RC_MASK as STATUS_PORT_RC, MISC_REVISION, MISC_REVISION_RESET as HW_REV,
    MSI_BAR_CONFIG_HI, MSI_BAR_CONFIG_LO, MSI_DATA_CONFIG, MSI_INTR2_CLR, MSI_INTR2_MASK_CLR,
    MSI_INTR2_MASK_SET, MSI_INTR2_MASK_STATUS, MSI_INTR2_SET, MSI_INTR2_STATUS, PRIV1_ID_VAL3,
    RC_BAR1_CONFIG_LO, RC_BAR2_CONFIG_HI, RC_BAR2_CONFIG_LO,
    RC_BAR2_CONFIG_LO_SIZE_MASK as RC_BAR_SIZE_MASK, RC_BAR3_CONFIG_LO, RC_LNKCTL, RGR1_SW_INIT_1,
    RGR1_SW_INIT_1_INIT_MASK as SW_INIT_BRIDGE,
};
use crate::spec::Coverage;

/// Every register in `specs/pcie.toml` is modelled; the ones with no
/// behaviour of their own are storage.
pub const COVERAGE: Coverage = Coverage {
    block: "pcie",
    decoded: &[
        RC_LNKCTL,
        PRIV1_ID_VAL3,
        MDIO_ADDR,
        MDIO_WR_DATA,
        MDIO_RD_DATA,
        MISC_CTRL,
        MEM_WIN0_LO,
        MEM_WIN0_HI,
        RC_BAR1_CONFIG_LO,
        RC_BAR2_CONFIG_LO,
        RC_BAR2_CONFIG_HI,
        RC_BAR3_CONFIG_LO,
        MSI_BAR_CONFIG_LO,
        MSI_BAR_CONFIG_HI,
        MSI_DATA_CONFIG,
        MISC_PCIE_STATUS,
        MISC_REVISION,
        MEM_WIN0_BASE_LIMIT,
        MEM_WIN0_BASE_HI,
        MEM_WIN0_LIMIT_HI,
        HARD_DEBUG,
        INTR2_CPU_STATUS,
        INTR2_CPU_SET,
        INTR2_CPU_CLR,
        INTR2_CPU_MASK_STATUS,
        INTR2_CPU_MASK_SET,
        INTR2_CPU_MASK_CLR,
        MSI_INTR2_STATUS,
        MSI_INTR2_SET,
        MSI_INTR2_CLR,
        MSI_INTR2_MASK_STATUS,
        MSI_INTR2_MASK_SET,
        MSI_INTR2_MASK_CLR,
        EXT_CFG_DATA,
        EXT_CFG_INDEX,
        RGR1_SW_INIT_1,
    ],
};

/// The root port's own configuration space, directly mapped.
const RC_CFG: u32 = 0x0000;
const RC_CFG_SIZE: u32 = 0x1000;
/// End of the `EXT_CFG_DATA` window: one function's 4 KiB configuration space.
const EXT_CFG_END: u32 = EXT_CFG_DATA + EXT_CFG_DATA_COUNT * EXT_CFG_DATA_STRIDE;
/// SerDes register `0x1F` selects the block the others address.
const MDIO_BLOCK_SELECT: u32 = 0x1F;
/// The spread-spectrum block and the two registers `brcm_pcie_set_ssc()` uses.
const SSC_BLOCK: u16 = 0x1100;
const SSC_STATUS: u32 = 0x1;
const SSC_CNTL: u32 = 0x2;
/// `SSC_CNTL_OVRD_EN | SSC_CNTL_OVRD_VAL`.
const SSC_CNTL_OVRD: u16 = 0xC000;
const SSC_STATUS_SSC: u16 = 0x400;
const SSC_STATUS_PLL_LOCK: u16 = 0x800;
/// Link status of the trained link: 5 GT/s (`CLS` 2), x1, slot clock. That is
/// `rpi-dev`'s `LnkSta: Speed 5GT/s, Width x1` / `SlotClk+`, and Linux prints
/// it as `link up, 5.0 GT/s PCIe x1`.
const LNKSTA_UP: u32 = 0x1012;
const LNKSTA_SLOTCLK: u32 = 0x1000;

/// The bus number the root port assigns to its single downstream link. Fixed on
/// this topology: `lspci` on `rpi-dev` shows `00:00.0` bridge, `01:00.0` VL805.
const ENDPOINT_BUS: u32 = 1;

/// The root port's own configuration space, measured on `rpi-dev`:
///
/// ```text
/// $ sudo od -Ax -tx4 -v /sys/bus/pci/devices/0000:00:00.0/config
/// 000000 271114e4 00100006 06040020 00010000
/// ...
/// ```
///
/// All 4 KiB of it is the direct `+0x0000` view (`brcm_pcie_map_bus()` hands
/// the root bus `base + where`), so the dump also carries the Broadcom `PRIV1`
/// registers from `0x400` up. It is of a running system: the fields Linux or
/// the firmware had written are put back to their power-on values — command,
/// bus numbers, the windows, interrupt line, link and root control, AER's root
/// command — and everything else is verbatim. The chain is what `lspci -vvv`
/// prints: PM at `0x48`, a PCIe v2 root port at `0xAC`, AER at `0x100`, a
/// vendor capability at `0x180`, L1 PM substates at `0x240`. The header word
/// at `0x08` is not stored; it is a view of [`PRIV1_ID_VAL3`], which the
/// bootloader writes (`0x000A6E20`) and so does Linux.
const RC_CFG_SEED: &[(u32, u32)] = &[
    (0x000, 0x2711_14E4), // vendor 14e4, device 2711
    (0x004, 0x0010_0000), // status: capability list
    (0x00C, 0x0001_0000), // header type 1
    (0x024, 0x0001_0001), // prefetchable window: 64-bit
    (0x034, 0x0000_0048), // capabilities -> 0x48
    (0x03C, 0x0000_0100), // interrupt pin A
    (0x048, 0x4813_AC01), // PM v3, next -> 0xAC
    (0x04C, 0x0000_2008),
    (0x0AC, 0x0042_0010), // PCIe v2 root port, last
    (0x0B0, 0x0000_8002), // DevCap
    (0x0B4, 0x0000_2C10), // DevCtl
    (0x0B8, 0x0064_CC12), // LnkCap: 5 GT/s x1, port 0
    (0x0C4, 0x0040_0000), // SltSta: presence detect
    (0x0C8, 0x0001_0000), // RootCap: CRS software visibility
    (0x0D0, 0x0008_081F), // DevCap2
    (0x0D8, 0x8000_0006), // LnkCap2: 2.5 and 5 GT/s
    (0x0DC, 0x0000_0002), // LnkCtl2: target 5 GT/s
    (0x100, 0x1801_0001), // AER, next -> 0x180
    (0x10C, 0x0006_2030), // AER uncorrectable severity
    (0x114, 0x0000_2000), // AER correctable mask
    (0x180, 0x2401_000B), // vendor-specific, next -> 0x240
    (0x184, 0x0280_0000),
    (0x240, 0x0001_001E), // L1 PM substates, last
    (0x244, 0x0028_081F),
    (0x248, 0x0000_0100),
    (0x24C, 0x0000_0028),
    (0x408, 0x0001_0010),
    (0x40C, 0x8000_0000),
    (0x434, 0x14E4_2711),
    (0x438, 0x2711_14E4),
    (0x43C, 0x2006_0400), // PRIV1_ID_VAL3: revision 0x20, class 060400
    (0x440, 0x0000_3048),
    (0x444, 0x0000_0AE4),
    (0x4D0, 0x0000_0040),
    (0x4D4, 0x0000_8002),
    (0x4DC, 0x0031_5E12), // PRIV1_LINK_CAPABILITY: x1
    (0x4E4, 0x0008_001F),
    (0x4E8, 0x8000_0000),
    (0x4F0, 0x0000_0002),
    (0x4F8, 0x0000_000F), // PRIV1_ROOT_CAP
    (0x500, 0x4001_0003),
    (0x540, 0x0028_081F),
    (0x544, 0x0001_001E),
    (0x554, 0x0280_0000),
    (0x558, 0x0000_000F),
    (0x560, 0x0000_000F),
];

/// Which bits of each root-port header word software may change. The rest of
/// the header reads back what it holds, which makes the bridge what `lspci`
/// says it is: no BARs (`0x10`/`0x14`), no I/O window (`0x1C`), no expansion
/// ROM. Past the header the block is plain storage: the bootloader and Linux
/// both write its capabilities and Broadcom registers, and nothing reads a
/// fixed field back.
fn rc_cfg_write_mask(off: u32) -> u32 {
    match off {
        0x04 => 0x0000_0547,        // command
        0x0C => 0x0000_00FF,        // cache line size
        0x18 => 0x00FF_FFFF,        // primary, secondary, subordinate bus
        0x20 | 0x24 => 0xFFF0_FFF0, // memory and prefetchable windows
        0x28 | 0x2C => 0xFFFF_FFFF, // prefetchable window, upper halves
        0x3C => 0xFFFF_00FF,        // interrupt line, bridge control
        0x00..=0x3F => 0,
        _ => 0xFFFF_FFFF,
    }
}

/// Status bits past the header that software clears by writing ones (RW1C):
/// DevSta, LnkSta's bandwidth-management bits, SltSta, RootSta's PME status,
/// and AER's uncorrectable, correctable and root error status. Linux clears
/// the PME status with a read-modify-write (`pcie_clear_root_pme_status`), so
/// as plain storage the clear would set it for good, and `pcie_pme_irq` would
/// claim every interrupt on the line (docs/arm-side-findings.md).
fn rc_cfg_w1c_mask(off: u32) -> u32 {
    match off {
        0x0B4 => 0x000F_0000,
        0x0BC => 0xC000_0000,
        0x0C4 => 0x011F_0000,
        0x0CC => 0x0001_0000,
        0x104 | 0x110 => 0xFFFF_FFFF,
        0x130 => 0x0000_007F,
        _ => 0,
    }
}

/// The size an `RC_BARn_CONFIG_LO` size field encodes, the inverse of
/// `brcm_pcie_encode_ibar_size()`: `1..=0x15` is 64 KiB to 64 GiB, `0x1C..=0x1F`
/// is 4 KiB to 32 KiB, and anything else — `0` included — switches the window
/// off.
fn ibar_size(code: u32) -> Option<u64> {
    match code {
        0x01..=0x15 => Some(1 << (code + 15)),
        0x1C..=0x1F => Some(1 << (code - 0x1C + 12)),
        _ => None,
    }
}

/// The endpoint's upstream memory traffic, as the root complex forwards it:
/// a PCI bus address inside inbound window 2 becomes the CPU-physical
/// address that far into the window, and `mem` is system memory addressed
/// that way. Anything outside the window reaches no memory — on silicon the
/// request completes as Unsupported; here a read returns all-ones and a write
/// is dropped.
struct Upstream<'a> {
    /// `(bus base, size)` of inbound window 2, if it is on.
    window: Option<(u64, u64)>,
    mem: &'a mut dyn HostMem,
    dbg: bool,
}

impl Upstream<'_> {
    fn phys(&self, bus: u64, write: bool) -> Option<u64> {
        let phys = self
            .window
            .and_then(|(base, size)| bus.checked_sub(base).filter(|off| *off < size));
        if phys.is_none() && self.dbg {
            let dir = if write { "write" } else { "read" };
            eprintln!("[pcie] endpoint {dir} at bus {bus:#x} is outside the inbound window");
        }
        phys
    }
}

impl HostMem for Upstream<'_> {
    fn read8(&self, addr: u64) -> u8 {
        match self.phys(addr, false) {
            Some(p) => self.mem.read8(p),
            None => 0xFF,
        }
    }

    fn write8(&mut self, addr: u64, value: u8) {
        if let Some(p) = self.phys(addr, true) {
            self.mem.write8(p, value);
        }
    }
}

pub struct Pcie {
    storage: BTreeMap<u32, u32>,
    /// Last value written to `RGR1_SW_INIT_1`.
    sw_init: u32,
    /// The link has trained. Sticky until PERST# is asserted again.
    link_up: bool,
    ext_cfg_index: u32,
    device_present: bool,
    pub endpoint: Vl805,
    /// The SerDes MDIO port: the last command packet, the block register
    /// `0x1F` selected, the registers written, and `WR_DATA` as last left.
    mdio_pkt: u32,
    mdio_block: u16,
    mdio_regs: BTreeMap<(u16, u32), u16>,
    mdio_wr: u32,
    /// The MSI block's pending vectors, and its mask (all masked at reset).
    msi_status: u32,
    msi_mask: u32,
    /// An interrupt that stayed pending without a message (bus mastering was
    /// off when it was asserted). An MSI is an edge, so that assertion sends
    /// none.
    msi_sent: bool,
    /// The endpoint's INTA, while it is not using MSI.
    intx: bool,
    /// `RVF_DBG_PCIE`: trace every change of the endpoint's interrupt.
    dbg: bool,
}

impl Default for Pcie {
    fn default() -> Self {
        Pcie::new()
    }
}

impl Pcie {
    pub fn new() -> Pcie {
        // A Pi 4B has the VL805 soldered on, so attached is what the reference
        // board looks like. `RVF_PCIE_DEVICE=0` unsolders it — for reproducing
        // the pre-stage-1 transcript, nothing else.
        Pcie::with_device(std::env::var("RVF_PCIE_DEVICE").as_deref() != Ok("0"))
    }

    pub fn with_device(device_present: bool) -> Pcie {
        let mut storage = BTreeMap::new();
        for (off, v) in RC_CFG_SEED {
            storage.insert(*off, *v);
        }
        Pcie {
            storage,
            sw_init: 0,
            link_up: false,
            ext_cfg_index: 0,
            device_present,
            endpoint: Vl805::new(),
            mdio_pkt: 0,
            mdio_block: 0,
            mdio_regs: BTreeMap::new(),
            mdio_wr: 0,
            msi_status: 0,
            msi_mask: 0xFFFF_FFFF,
            msi_sent: false,
            intx: false,
            dbg: std::env::var("RVF_DBG_PCIE").is_ok(),
        }
    }

    pub fn link_up(&self) -> bool {
        self.link_up
    }

    fn status(&self) -> u32 {
        let link = if self.link_up {
            STATUS_PHYLINKUP | STATUS_DL_ACTIVE
        } else {
            0
        };
        STATUS_PORT_RC | link
    }

    fn stored(&self, off: u32) -> u32 {
        self.storage.get(&off).copied().unwrap_or(0)
    }

    /// The MSI block's output, `GIC_SPI 148`: a vector is pending and not
    /// masked.
    pub fn msi_line(&self) -> bool {
        self.msi_status & !self.msi_mask != 0
    }

    /// The endpoint's INTA, `GIC_SPI 143` through the device tree's
    /// `interrupt-map`.
    pub fn intx_line(&self) -> bool {
        self.intx
    }

    /// Turn the endpoint's interrupt into what it sends upstream: an MSI when
    /// the host has enabled one, INTA otherwise. Called after anything that
    /// can move it: a register write through the window, a config write.
    fn update_irq(&mut self) {
        let pending = self.link_up && self.endpoint.xhci.interrupt_pending();
        let before = (self.intx, self.msi_status);
        self.route_irq(pending);
        if self.dbg && before != (self.intx, self.msi_status) {
            eprintln!(
                "[pcie] endpoint irq {pending}: intx {} msi status {:#x} mask {:#x}",
                self.intx, self.msi_status, self.msi_mask
            );
        }
    }

    fn route_irq(&mut self, pending: bool) {
        match self.endpoint.msi_message() {
            Some((addr, data)) => {
                self.intx = false;
                let send = pending && !self.msi_sent && self.endpoint.bus_master();
                if send {
                    self.receive_msi(addr, data);
                    // `IMAN.IP` clears itself once the message is out (xHCI
                    // 5.5.2.1), so the next event is a new assertion. Linux
                    // relies on it: with MSI it never clears IP itself
                    // (`ip_autoclear`).
                    self.endpoint.xhci.msi_sent();
                }
                self.msi_sent = pending && !send;
            }
            None => {
                self.msi_sent = false;
                self.intx = pending && !self.endpoint.intx_disabled();
            }
        }
    }

    /// An upstream memory write of `data` to PCI bus address `addr`, which the
    /// root complex claims when it is its MSI target.
    fn receive_msi(&mut self, addr: u64, data: u32) {
        if self.sw_init & SW_INIT_BRIDGE != 0 {
            // A bridge held in reset takes no messages.
            return;
        }
        let lo = self.stored(MSI_BAR_CONFIG_LO);
        let target = ((self.stored(MSI_BAR_CONFIG_HI) as u64) << 32) | (lo & !0x3) as u64;
        let cfg = self.stored(MSI_DATA_CONFIG);
        let (mask, pattern) = (cfg >> 16, cfg & 0xFFFF);
        if lo & 1 == 0 || addr & !0x3 != target || data & mask != pattern & mask {
            // Not a message the block recognises. It would be an ordinary
            // write into memory, and nothing in the model sends one.
            return;
        }
        self.msi_status |= 1 << (data & !mask & 0x1F);
    }

    fn mdio_regad(&self) -> u32 {
        self.mdio_pkt & MDIO_REGAD
    }

    /// `RD_DATA`: the register the last packet addressed, and done. The PHY
    /// answers at once; the only register with behaviour is SSC status, which
    /// reports spread spectrum on once both override bits are set and the PLL
    /// always locked — the `(SSC)` in Linux's `link up` line.
    fn mdio_read(&self) -> u32 {
        let regad = self.mdio_regad();
        let reg = |r| self.mdio_regs.get(&(self.mdio_block, r)).copied();
        let v = if regad == MDIO_BLOCK_SELECT {
            self.mdio_block
        } else if self.mdio_block == SSC_BLOCK && regad == SSC_STATUS {
            let ssc = reg(SSC_CNTL).unwrap_or(0) & SSC_CNTL_OVRD == SSC_CNTL_OVRD;
            SSC_STATUS_PLL_LOCK | if ssc { SSC_STATUS_SSC } else { 0 }
        } else {
            reg(regad).unwrap_or(0)
        };
        MDIO_DONE | v as u32
    }

    /// `WR_DATA`: the host sets `DONE` to start a write, and it completes
    /// before the next read.
    fn mdio_write(&mut self, value: u32) {
        self.mdio_wr = value & !MDIO_DONE;
        if value & MDIO_DONE == 0 || self.mdio_pkt & MDIO_CMD_READ != 0 {
            return;
        }
        let (regad, data) = (self.mdio_regad(), value as u16);
        if regad == MDIO_BLOCK_SELECT {
            self.mdio_block = data;
        } else {
            self.mdio_regs.insert((self.mdio_block, regad), data);
        }
    }

    /// The CPU-physical extent of outbound window 0, as the firmware programmed
    /// it. Both ends are inclusive and 40 bits wide — well outside anything a
    /// 32-bit VPU load can form, which is why the firmware reaches through it by
    /// DMA (see [`crate::machine::Machine::run_dma4`]).
    fn outbound_window(&self) -> Option<(u64, u64, u64)> {
        let base_limit = self.storage.get(&MEM_WIN0_BASE_LIMIT).copied().unwrap_or(0);
        let base_hi = self.storage.get(&MEM_WIN0_BASE_HI).copied().unwrap_or(0) as u64;
        let limit_hi = self.storage.get(&MEM_WIN0_LIMIT_HI).copied().unwrap_or(0) as u64;
        let base_mb = (base_hi << 12) | ((base_limit >> 4) & 0xFFF) as u64;
        let limit_mb = (limit_hi << 12) | ((base_limit >> 20) & 0xFFF) as u64;
        if limit_mb < base_mb {
            return None;
        }
        let bus = ((self.storage.get(&MEM_WIN0_HI).copied().unwrap_or(0) as u64) << 32)
            | self.storage.get(&MEM_WIN0_LO).copied().unwrap_or(0) as u64;
        Some((base_mb << 20, (limit_mb << 20) | 0xF_FFFF, bus))
    }

    /// Translate a CPU-physical address into a PCI bus address, if outbound
    /// window 0 covers it.
    pub fn outbound_bus_addr(&self, cpu: u64) -> Option<u64> {
        let (base, limit, bus) = self.outbound_window()?;
        if cpu < base || cpu > limit {
            return None;
        }
        Some(bus + (cpu - base))
    }

    /// Which endpoint register a CPU-physical address lands on, if any: through
    /// the outbound window, then through the endpoint's BAR0.
    fn bar0_offset(&self, cpu: u64) -> Option<u32> {
        if !self.link_up {
            return None;
        }
        let bus = self.outbound_bus_addr(cpu)?;
        let bar = self.endpoint.bar0_bus_addr()?;
        if bus < bar || bus >= bar + crate::periph::vl805::BAR0_SIZE as u64 {
            return None;
        }
        Some((bus - bar) as u32)
    }

    /// A read of endpoint MMIO, addressed CPU-physically. `None` means nothing
    /// decodes there — the DMA engine then falls back to DRAM, the way an
    /// unclaimed address does on real silicon.
    pub fn mmio_read(&mut self, cpu: u64, width: Width) -> Option<u32> {
        let off = self.bar0_offset(cpu)?;
        Some(self.endpoint.bar0_read(off, width))
    }

    /// Inbound window 2 as `(bus base, size)`, if its size field switches it
    /// on.
    fn inbound_window(&self) -> Option<(u64, u64)> {
        let lo = self.stored(RC_BAR2_CONFIG_LO);
        let size = ibar_size(lo & RC_BAR_SIZE_MASK)?;
        let base = ((self.stored(RC_BAR2_CONFIG_HI) as u64) << 32) | lo as u64;
        Some((base & !(size - 1), size))
    }

    /// A write of endpoint MMIO, addressed CPU-physically. `mem` is system
    /// memory by CPU-physical address: an xHCI doorbell write makes the
    /// endpoint fetch TRBs from DRAM and post events back into it, through
    /// the inbound window.
    pub fn mmio_write(
        &mut self,
        cpu: u64,
        width: Width,
        value: u32,
        mem: &mut dyn HostMem,
    ) -> bool {
        match self.bar0_offset(cpu) {
            Some(off) => {
                let mut up = Upstream {
                    window: self.inbound_window(),
                    mem,
                    dbg: self.dbg,
                };
                self.endpoint.bar0_write(off, width, value, &mut up);
                self.update_irq();
                true
            }
            None => false,
        }
    }

    /// Which function the `EXT_CFG_DATA` window currently points at, if any.
    ///
    /// Bus 0 is deliberately not one of them (#21). The window turns its index
    /// into a configuration request on the link, and the link's far side is the
    /// secondary bus — the root port's own config space is never reachable this
    /// way, only through the direct `+0x0000` view. That is why `pcie-brcmstb`
    /// special-cases it in `brcm_pcie_map_conf()`:
    ///
    /// ```c
    /// /* Accesses to the RC go right to the RC registers if slot==0 */
    /// if (pci_is_root_bus(bus))
    ///     return PCI_SLOT(devfn) ? NULL : base + where;
    /// ```
    ///
    /// and it is what the reference transcript shows. The bootloader's scan
    /// (`0x000A712C`) starts at bus 0, slot 0 and prints `PCIe scan %08x:%08x`
    /// for every function whose vendor id is not `0xFFFF`, *before* it looks at
    /// the header type — the header-type byte at `0x0E` only decides whether the
    /// function is recorded in the device list (`0x000A71CC`, non-zero = bridge
    /// = skip), not whether it is printed. So if bus 0 answered here the real
    /// board would print the root complex too, and
    /// `examples-on-real-hardware/sd-card-boot.log:25-27` shows it does not.
    ///
    /// The bootloader reaches the root port's own config space the same way
    /// Linux does: its config-space selector (`0x000A6888`) writes
    /// `EXT_CFG_INDEX` and points its window at `EXT_CFG_DATA` only for a real
    /// device, and points it straight at `0x7D50_0000` for the root port.
    fn ext_target(&self) -> CfgTarget {
        let bus = (self.ext_cfg_index >> EXT_BUSNUM_SHIFT) & 0xFF;
        let slot = (self.ext_cfg_index >> EXT_SLOT_SHIFT) & 0x1F;
        let func = (self.ext_cfg_index >> EXT_FUNC_SHIFT) & 0x7;
        match (bus, slot, func) {
            (ENDPOINT_BUS, 0, 0) if self.link_up => CfgTarget::Endpoint,
            _ => CfgTarget::None,
        }
    }

    /// A word of the root port's own config space.
    fn rc_cfg_word(&self, off: u32) -> u32 {
        match off {
            0x08 => self.stored(PRIV1_ID_VAL3).rotate_left(8),
            RC_LNKCTL => {
                let sta = if self.link_up {
                    LNKSTA_UP
                } else {
                    LNKSTA_SLOTCLK
                };
                (self.stored(RC_LNKCTL) & 0xFFFF) | (sta << 16)
            }
            _ => self.stored(off),
        }
    }

    /// The value the root port's own config space holds at `off`.
    fn rc_cfg_read(&self, off: u32, width: Width) -> u32 {
        let word = self.rc_cfg_word(off & !3);
        let shift = 8 * (off & 3);
        let mask: u32 = match width {
            Width::Byte => 0xFF,
            Width::Half => 0xFFFF,
            Width::Word => 0xFFFF_FFFF,
        };
        (word >> shift) & mask
    }
}

enum CfgTarget {
    Endpoint,
    None,
}

impl MmioDevice for Pcie {
    fn name(&self) -> &'static str {
        "pcie-rc"
    }

    fn read(&mut self, offset: u32, width: Width) -> BusResult<u32> {
        if offset == MISC_PCIE_STATUS {
            return Ok(self.status());
        }
        if offset == EXT_CFG_INDEX {
            return Ok(self.ext_cfg_index);
        }
        if offset == MISC_REVISION {
            return Ok(HW_REV);
        }
        if offset == MDIO_RD_DATA {
            return Ok(self.mdio_read());
        }
        if offset == MDIO_WR_DATA {
            return Ok(self.mdio_wr);
        }
        if (MSI_INTR2_STATUS..MSI_INTR2_MASK_CLR + 4).contains(&offset) {
            return Ok(match offset {
                MSI_INTR2_STATUS => self.msi_status,
                MSI_INTR2_MASK_STATUS => self.msi_mask,
                _ => 0,
            });
        }
        if (EXT_CFG_DATA..EXT_CFG_END).contains(&offset) {
            let cfg_off = offset - EXT_CFG_DATA;
            return Ok(match self.ext_target() {
                CfgTarget::Endpoint => self.endpoint.cfg_read(cfg_off, width),
                // No function responds: the root complex returns all-ones,
                // which is what makes a scan skip the slot.
                CfgTarget::None => match width {
                    Width::Byte => 0xFF,
                    Width::Half => 0xFFFF,
                    Width::Word => 0xFFFF_FFFF,
                },
            });
        }
        if offset < RC_CFG + RC_CFG_SIZE {
            return Ok(self.rc_cfg_read(offset, width));
        }
        Ok(self.storage.get(&(offset & !3)).copied().unwrap_or(0))
    }

    fn write(&mut self, offset: u32, width: Width, value: u32) -> BusResult<()> {
        if offset == MISC_PCIE_STATUS {
            // Read-only status; the firmware never writes it.
            return Ok(());
        }
        if offset == EXT_CFG_INDEX {
            self.ext_cfg_index = value;
            return Ok(());
        }
        if offset == MDIO_WR_DATA {
            self.mdio_write(value);
            return Ok(());
        }
        if offset == MDIO_ADDR {
            self.mdio_pkt = value;
        }
        if (MSI_INTR2_STATUS..MSI_INTR2_MASK_CLR + 4).contains(&offset) {
            match offset {
                MSI_INTR2_SET => self.msi_status |= value,
                MSI_INTR2_CLR => self.msi_status &= !value,
                MSI_INTR2_MASK_SET => self.msi_mask |= value,
                MSI_INTR2_MASK_CLR => self.msi_mask &= !value,
                _ => {}
            }
            return Ok(());
        }
        if (EXT_CFG_DATA..EXT_CFG_END).contains(&offset) {
            let cfg_off = offset - EXT_CFG_DATA;
            match self.ext_target() {
                CfgTarget::Endpoint => self.endpoint.cfg_write(cfg_off, width, value),
                CfgTarget::None => {}
            }
            // The command register and the MSI capability decide how the
            // endpoint's interrupt goes upstream.
            self.update_irq();
            return Ok(());
        }
        if offset == RGR1_SW_INIT_1 {
            let was_perst = self.sw_init & 1 != 0;
            self.sw_init = value;
            if value & SW_INIT_BRIDGE != 0 {
                // The bridge soft reset puts the MSI block back to its reset
                // state (module docs): nothing pending, every vector masked.
                self.msi_status = 0;
                self.msi_mask = 0xFFFF_FFFF;
            }
            if value & 1 != 0 {
                self.link_up = false;
                if !was_perst {
                    // The fundamental reset reaches the endpoint too.
                    self.endpoint.reset();
                    self.msi_sent = false;
                }
            } else if was_perst && self.device_present {
                // PERST# released with a device on the far side: the link
                // trains. Real silicon takes a few milliseconds and the
                // bootloader polls for it at 1 ms intervals with a 1 s budget
                // (`0x000A9038`); the model brings it up on the spot, which
                // costs the transcript a millisecond of modelled time.
                self.link_up = true;
            }
            self.storage.insert(offset, value);
            self.update_irq();
            return Ok(());
        }
        if offset < RC_CFG + RC_CFG_SIZE {
            // Root-port config space is byte-addressable: the bootloader writes
            // single halfwords into the bridge header (`0x22` ← `0x8000`, and
            // so on), so a word-granular store would clobber its neighbour.
            let word_off = offset & !3;
            let shift = 8 * (offset & 3);
            let mask: u32 = match width {
                Width::Byte => 0xFF << shift,
                Width::Half => 0xFFFF << shift,
                Width::Word => 0xFFFF_FFFF,
            } & rc_cfg_write_mask(word_off);
            let old = self.storage.get(&word_off).copied().unwrap_or(0);
            let w1c = rc_cfg_w1c_mask(word_off) & mask;
            let ones = (value << shift) & mask;
            let new = (old & !mask) | (ones & !w1c) | (old & w1c & !ones);
            self.storage.insert(word_off, new);
            return Ok(());
        }
        self.storage.insert(offset & !3, value);
        if self.dbg && matches!(offset, RC_BAR2_CONFIG_LO | RC_BAR2_CONFIG_HI) {
            eprintln!("[pcie] inbound window {:x?}", self.inbound_window());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rd(p: &mut Pcie, off: u32) -> u32 {
        p.read(off, Width::Word).unwrap()
    }

    /// The bootcode/bootloader reset dance, then the link-up poll.
    #[test]
    fn perst_release_brings_the_link_up() {
        let mut p = Pcie::with_device(true);
        // Only the root-complex strap until the link trains.
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0x80);
        // bootcode parks the block in reset
        p.write(RGR1_SW_INIT_1, Width::Word, 0x3).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0x80);
        // pcie_init releases the bridge, then PERST#
        p.write(RGR1_SW_INIT_1, Width::Word, 0x1).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0x80);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x0).unwrap();
        // 0xB0: PHYLINKUP | DL_ACTIVE | root-complex mode.
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0xB0);
        // Asserting PERST# again drops it.
        p.write(RGR1_SW_INIT_1, Width::Word, 0x1).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0x80);
    }

    #[test]
    fn no_device_means_no_link() {
        let mut p = Pcie::with_device(false);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x3).unwrap();
        p.write(RGR1_SW_INIT_1, Width::Word, 0x0).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0x80);
    }

    /// `brcm_pcie_setup()` asks whether the block is a root complex with
    /// PERST# held, before it ever starts the link.
    #[test]
    fn root_complex_mode_is_a_strap() {
        let mut p = Pcie::with_device(true);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x3).unwrap();
        p.write(RGR1_SW_INIT_1, Width::Word, 0x1).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), STATUS_PORT_RC);
        // Revision 3.3 or later: the 32-vector MSI block.
        assert!(rd(&mut p, MISC_REVISION) >= 0x0303);
    }

    /// What Linux's bus scan needs from the root port: a bridge with no BARs
    /// to size, writable bus numbers, and the PCIe capability that makes it a
    /// root port and reports the trained link.
    #[test]
    fn the_root_port_is_a_bridge_without_bars() {
        let mut p = link_up_pcie();
        assert_eq!(rd(&mut p, 0x08), 0x0604_0020);
        p.write(0x10, Width::Word, 0xFFFF_FFFF).unwrap();
        assert_eq!(rd(&mut p, 0x10), 0);
        p.write(0x18, Width::Word, 0xFF01_0100).unwrap();
        assert_eq!(rd(&mut p, 0x18), 0x0001_0100);
        // PM at 0x48, then PCIe at 0xAC, device/port type 4 = root port.
        assert_eq!(rd(&mut p, 0x34) & 0xFF, 0x48);
        assert_eq!((rd(&mut p, 0x48) >> 8) & 0xFF, 0xAC);
        assert_eq!((rd(&mut p, 0xAC) >> 20) & 0xF, 4);
        assert_eq!(p.read(0xBE, Width::Half).unwrap(), 0x1012);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x1).unwrap();
        assert_eq!(p.read(0xBE, Width::Half).unwrap(), 0x1000);
    }

    /// `brcm_pcie_set_ssc()`, register by register: select the SSC block, set
    /// both override bits, then read SSC on and the PLL locked.
    #[test]
    fn spread_spectrum_clocking_comes_up_over_mdio() {
        let mut p = link_up_pcie();
        let write = |p: &mut Pcie, regad: u32, data: u32| {
            p.write(MDIO_ADDR, Width::Word, regad).unwrap();
            p.write(MDIO_WR_DATA, Width::Word, MDIO_DONE | data)
                .unwrap();
            assert_eq!(rd(p, MDIO_WR_DATA) & MDIO_DONE, 0);
        };
        let read = |p: &mut Pcie, regad: u32| {
            p.write(MDIO_ADDR, Width::Word, MDIO_CMD_READ | regad)
                .unwrap();
            let v = rd(p, MDIO_RD_DATA);
            assert_ne!(v & MDIO_DONE, 0);
            v & 0xFFFF
        };
        write(&mut p, MDIO_BLOCK_SELECT, SSC_BLOCK as u32);
        assert_eq!(read(&mut p, SSC_STATUS) & 0xC00, 0x800);
        let cntl = read(&mut p, SSC_CNTL);
        write(&mut p, SSC_CNTL, cntl | 0xC000);
        assert_eq!(read(&mut p, SSC_STATUS) & 0xC00, 0xC00);
    }

    /// `enumerated_pcie()` with MSI on at both ends, as Linux leaves it, and
    /// interrupter 0 running with one event ring segment of 16 TRBs at
    /// 0x2000. Returns BAR0 and interrupter 0's `IMAN`.
    fn msi_pcie(mem: &mut crate::periph::xhci::VecMem) -> (Pcie, u64, u64) {
        use crate::periph::xhci::{HostMem, RTSOFF};
        let mut p = enumerated_pcie();
        // What `brcm_msi_set_regs()` programs...
        p.write(MSI_INTR2_MASK_CLR, Width::Word, 0xFFFF_FFFF)
            .unwrap();
        p.write(MSI_BAR_CONFIG_LO, Width::Word, 0xFFFF_FFFD)
            .unwrap();
        p.write(MSI_BAR_CONFIG_HI, Width::Word, 0).unwrap();
        p.write(MSI_DATA_CONFIG, Width::Word, 0xFFE0_6540).unwrap();
        // ...and the function's MSI capability, as `lspci` shows it.
        for (off, v) in [
            (0x94, 0xFFFF_FFFC),
            (0x98, 0),
            (0x9C, 0x6540),
            (0x90, 1 << 16),
        ] {
            p.write(EXT_CFG_DATA + off, Width::Word, v).unwrap();
        }
        mem.write32(0x1000, 0x2000);
        mem.write32(0x1008, 16);
        let bar = 0x6_0200_0000u64;
        let ir0 = bar + RTSOFF as u64 + 0x20;
        p.mmio_write(ir0 + 0x08, Width::Word, 1, mem); // ERSTSZ
        p.mmio_write(ir0 + 0x10, Width::Word, 0x1000, mem); // ERSTBA
        p.mmio_write(ir0, Width::Word, 0x2, mem); // IMAN.IE
        p.mmio_write(bar + 0x20, Width::Word, 0x5, mem); // USBCMD RS | INTE
        (p, bar, ir0)
    }

    /// With MSI on, `IMAN.IP` clears itself as the message goes out (xHCI
    /// 5.5.2.1), and Linux counts on it (`ip_autoclear`): it acknowledges only
    /// the MSI block. The next event must still send a message of its own;
    /// with `IP` left set, every completion after the first was silent and
    /// the kernel's Address Device timed out.
    #[test]
    fn every_event_sends_its_own_msi() {
        let mut mem = crate::periph::xhci::VecMem::default();
        let (mut p, bar, ir0) = msi_pcie(&mut mem);
        let port1 = bar + 0x420;
        p.mmio_write(port1, Width::Word, (1 << 9) | (1 << 4), &mut mem);
        assert!(p.msi_line());
        assert_eq!(p.mmio_read(ir0, Width::Word).unwrap() & 1, 0, "IP");
        p.write(MSI_INTR2_CLR, Width::Word, 1).unwrap();
        assert!(!p.msi_line());
        // Acknowledge the port's change bits, then reset it again.
        let changes = (1 << 17) | (1 << 21);
        p.mmio_write(port1, Width::Word, (1 << 9) | changes, &mut mem);
        p.mmio_write(port1, Width::Word, (1 << 9) | (1 << 4), &mut mem);
        assert!(p.msi_line(), "a second message");
    }

    /// The endpoint's first event, all the way to the GIC line: xHCI sets
    /// `IMAN.IP`, the function sends its MSI, the root complex catches the
    /// write at its target and raises vector 0.
    #[test]
    fn an_xhci_event_arrives_as_an_msi() {
        let mut mem = crate::periph::xhci::VecMem::default();
        let (mut p, bar, ir0) = msi_pcie(&mut mem);
        assert!(!p.msi_line());
        // Reset root port 1, the hub's: a Port Status Change Event.
        p.mmio_write(bar + 0x420, Width::Word, (1 << 9) | (1 << 4), &mut mem);
        assert!(p.msi_line());
        assert_eq!(rd(&mut p, MSI_INTR2_STATUS), 1);
        assert!(!p.intx_line());
        // The driver acknowledges the interrupter, then the MSI block.
        p.mmio_write(ir0, Width::Word, 0x3, &mut mem);
        p.write(MSI_INTR2_CLR, Width::Word, 1).unwrap();
        assert!(!p.msi_line());
    }

    /// A vector the firmware's USB traffic left pending behind the mask must
    /// not survive Linux's bridge reset: `brcm_msi_set_regs()` unmasks before
    /// it clears, and a stale vector has no handler to clear it.
    #[test]
    fn the_bridge_reset_clears_a_stale_msi() {
        let mut p = enumerated_pcie();
        p.write(MSI_INTR2_SET, Width::Word, 1).unwrap();
        assert!(!p.msi_line(), "masked");
        // `brcm_pcie_setup()`: bridge reset, PERST#, bridge out of reset.
        for v in [0x2, 0x3, 0x1] {
            p.write(RGR1_SW_INIT_1, Width::Word, v).unwrap();
        }
        p.write(MSI_INTR2_MASK_CLR, Width::Word, 0xFFFF_FFFF)
            .unwrap();
        assert_eq!(rd(&mut p, MSI_INTR2_STATUS), 0);
        assert!(!p.msi_line());
    }

    /// Without MSI the same event is INTA, and PERST# takes it away: the
    /// firmware's last USB event must not still be asserted when Linux brings
    /// the link back up.
    #[test]
    fn perst_resets_the_endpoint() {
        use crate::periph::xhci::{HostMem, VecMem, RTSOFF};
        let mut p = enumerated_pcie();
        let mut mem = VecMem::default();
        mem.write32(0x1000, 0x2000);
        mem.write32(0x1008, 16);
        let bar = 0x6_0200_0000u64;
        let ir0 = bar + RTSOFF as u64 + 0x20;
        p.mmio_write(ir0 + 0x08, Width::Word, 1, &mut mem);
        p.mmio_write(ir0 + 0x10, Width::Word, 0x1000, &mut mem);
        p.mmio_write(ir0, Width::Word, 0x2, &mut mem);
        p.mmio_write(bar + 0x20, Width::Word, 0x5, &mut mem);
        p.mmio_write(bar + 0x420, Width::Word, (1 << 9) | (1 << 4), &mut mem);
        assert!(p.intx_line());
        p.write(RGR1_SW_INIT_1, Width::Word, 0x1).unwrap();
        assert!(!p.intx_line());
        p.write(RGR1_SW_INIT_1, Width::Word, 0x0).unwrap();
        assert!(!p.intx_line());
        assert_eq!(p.endpoint.bar0_bus_addr(), None);
    }

    /// Linux's inbound window: `dma-ranges` puts system memory at PCI
    /// `0x4_0000_0000` (`IB MEM 0x0000000000..0x003fffffff -> 0x0400000000`),
    /// and `set_inbound_win_registers()` writes the base with a 1 GiB size
    /// code. The endpoint's DMA addresses are those bus addresses; the
    /// memory behind the window is addressed physically.
    #[test]
    fn endpoint_dma_goes_through_the_inbound_window() {
        use crate::periph::xhci::{VecMem, RTSOFF};
        let mut p = enumerated_pcie();
        p.write(RC_BAR2_CONFIG_LO, Width::Word, 0xF).unwrap();
        p.write(RC_BAR2_CONFIG_HI, Width::Word, 4).unwrap();
        assert_eq!(p.inbound_window(), Some((0x4_0000_0000, 1 << 30)));
        let mut mem = VecMem::default();
        // The event ring segment table at bus 0x4_0000_1000 = physical 0x1000.
        mem.write32(0x1000, 0x2000);
        mem.write32(0x1004, 4);
        mem.write32(0x1008, 16);
        let bar = 0x6_0200_0000u64;
        let ir0 = bar + RTSOFF as u64 + 0x20;
        p.mmio_write(ir0 + 0x08, Width::Word, 1, &mut mem); // ERSTSZ
        p.mmio_write(ir0 + 0x10, Width::Word, 0x1000, &mut mem); // ERSTBA lo
        p.mmio_write(ir0 + 0x14, Width::Word, 4, &mut mem); // ERSTBA hi
        p.mmio_write(bar + 0x20, Width::Word, 0x1, &mut mem); // USBCMD RS
                                                              // A root-port reset posts a Port Status Change Event to the ring's
                                                              // first TRB, bus 0x4_0000_2000: physical 0x2000, port 1 in bits 31:24.
        p.mmio_write(bar + 0x420, Width::Word, (1 << 9) | (1 << 4), &mut mem);
        assert_eq!(mem.read32(0x2000) >> 24, 1);
        assert!(mem.bytes.keys().all(|a| *a < 0x1_0000));

        // Outside the window the endpoint reaches nothing: a segment table
        // at bus 0x1000 reads all-ones, so no event lands anywhere new.
        let before = mem.bytes.clone();
        let mut p2 = enumerated_pcie();
        p2.write(RC_BAR2_CONFIG_LO, Width::Word, 0xF).unwrap();
        p2.write(RC_BAR2_CONFIG_HI, Width::Word, 4).unwrap();
        p2.mmio_write(ir0 + 0x08, Width::Word, 1, &mut mem);
        p2.mmio_write(ir0 + 0x10, Width::Word, 0x1000, &mut mem);
        p2.mmio_write(bar + 0x20, Width::Word, 0x1, &mut mem);
        p2.mmio_write(bar + 0x420, Width::Word, (1 << 9) | (1 << 4), &mut mem);
        assert_eq!(mem.bytes, before);
    }

    #[test]
    fn inbound_window_sizes_decode_like_linux_encodes_them() {
        assert_eq!(ibar_size(0), None);
        assert_eq!(ibar_size(0x1), Some(64 << 10));
        assert_eq!(ibar_size(0xF), Some(1 << 30));
        assert_eq!(ibar_size(0x12), Some(8 << 30));
        assert_eq!(ibar_size(0x15), Some(64 << 30));
        assert_eq!(ibar_size(0x1C), Some(4 << 10));
        assert_eq!(ibar_size(0x1F), Some(32 << 10));
        assert_eq!(ibar_size(0x16), None);
    }

    fn link_up_pcie() -> Pcie {
        let mut p = Pcie::with_device(true);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x3).unwrap();
        p.write(RGR1_SW_INIT_1, Width::Word, 0x0).unwrap();
        p
    }

    #[test]
    fn ext_cfg_routes_to_the_endpoint() {
        let mut p = link_up_pcie();
        // Index 0 is bus 0 — the root port's own bus, which no configuration
        // request on the link can reach (#21). Its config space is the direct
        // `+0x0000` view, and only that.
        assert_eq!(rd(&mut p, EXT_CFG_DATA), 0xFFFF_FFFF);
        assert_eq!(rd(&mut p, RC_CFG), 0x2711_14E4);
        // Bus 1, slot 0, function 0 is the VL805.
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA), 0x3483_1106);
        // The halfword reads start4's `XHCI_RESET` does at 0x3EDC6204/0x3EDC620A.
        assert_eq!(p.read(EXT_CFG_DATA, Width::Half).unwrap(), 0x1106);
        assert_eq!(p.read(EXT_CFG_DATA + 2, Width::Half).unwrap(), 0x3483);
        // Class code 0c0330 = xHCI.
        assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x08) >> 8, 0x0C_0330);
        // An empty slot answers all-ones.
        p.write(
            EXT_CFG_INDEX,
            Width::Word,
            1 << EXT_BUSNUM_SHIFT | 1 << EXT_SLOT_SHIFT,
        )
        .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA), 0xFFFF_FFFF);
    }

    #[test]
    fn endpoint_is_invisible_until_the_link_is_up() {
        let mut p = Pcie::with_device(true);
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA), 0xFFFF_FFFF);
    }

    #[test]
    fn bar0_sizes_to_four_kib() {
        let mut p = link_up_pcie();
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        // Power-on: 64-bit memory BAR, unassigned.
        assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x10), 0x0000_0004);
        p.write(EXT_CFG_DATA + 0x10, Width::Word, 0xFFFF_FFFF)
            .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x10), 0xFFFF_F004);
        // Assign it and enable memory decoding.
        p.write(EXT_CFG_DATA + 0x10, Width::Word, 0xC000_0000)
            .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x10), 0xC000_0004);
        assert_eq!(p.endpoint.bar0_bus_addr(), None);
        p.write(EXT_CFG_DATA + 0x04, Width::Word, 0x0146).unwrap();
        assert_eq!(p.endpoint.bar0_bus_addr(), Some(0xC000_0000));
    }

    /// The bootloader's VL805 hub-firmware upload (`0x000B6CE0`) writes each
    /// byte of the image to its own index and reads it straight back, giving up
    /// with `HUB2.0 fail` on the first mismatch. A sticky index/data port is
    /// the whole requirement.
    #[test]
    fn vendor_port_round_trips_the_firmware_upload() {
        let mut p = link_up_pcie();
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        for (i, byte) in [0xDEu32, 0xAD, 0xBE, 0xEF].into_iter().enumerate() {
            let idx = 0x5_2000 + i as u32;
            // The firmware replicates the byte into all four lanes.
            let word = byte * 0x0101_0101;
            p.write(EXT_CFG_DATA + 0x78, Width::Word, idx).unwrap();
            p.write(EXT_CFG_DATA + 0x7C, Width::Word, word).unwrap();
        }
        for (i, byte) in [0xDEu32, 0xAD, 0xBE, 0xEF].into_iter().enumerate() {
            p.write(EXT_CFG_DATA + 0x78, Width::Word, 0x5_2000 + i as u32)
                .unwrap();
            assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x7C) & 0xFF, byte);
        }
        assert_eq!(p.endpoint.vendor_writes, 4);
    }

    /// The exact outbound-window programming the bootloader does at
    /// `0x000A725C`–`0x000A72F0`, followed by the BAR assignment at
    /// `0x000A6918`. The window is the only way a 32-bit VPU can name the
    /// endpoint's registers, and it names them 40 bits wide.
    fn enumerated_pcie() -> Pcie {
        let mut p = link_up_pcie();
        // Inbound window 2 at bus 0, 8 GiB: bus addresses are CPU-physical.
        p.write(RC_BAR2_CONFIG_LO, Width::Word, 0x12).unwrap();
        p.write(RC_BAR2_CONFIG_HI, Width::Word, 0).unwrap();
        p.write(MEM_WIN0_LO, Width::Word, 0x8000_0000).unwrap();
        p.write(MEM_WIN0_HI, Width::Word, 0).unwrap();
        p.write(MEM_WIN0_BASE_LIMIT, Width::Word, 0x3FF0_0000)
            .unwrap();
        p.write(MEM_WIN0_BASE_HI, Width::Word, 6).unwrap();
        p.write(MEM_WIN0_LIMIT_HI, Width::Word, 6).unwrap();
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        p.write(EXT_CFG_DATA + 0x10, Width::Word, 0x8200_0000)
            .unwrap();
        p.write(EXT_CFG_DATA + 0x14, Width::Word, 0).unwrap();
        p.write(EXT_CFG_DATA + 0x04, Width::Word, 0x0146).unwrap();
        p
    }

    #[test]
    fn outbound_window_spans_one_gib_at_six() {
        let p = enumerated_pcie();
        // `dmesg` on rpi-dev: `MEM 0x0600000000..0x063fffffff -> 0x00c0000000`.
        // The firmware picks `0x8000_0000` for the bus side rather than Linux's
        // `0xC000_0000`, so the model has to read the register, not the DT.
        assert_eq!(p.outbound_bus_addr(0x6_0000_0000), Some(0x8000_0000));
        assert_eq!(p.outbound_bus_addr(0x6_3FFF_FFFF), Some(0xBFFF_FFFF));
        assert_eq!(p.outbound_bus_addr(0x5_FFFF_FFFF), None);
        assert_eq!(p.outbound_bus_addr(0x6_4000_0000), None);
    }

    /// The read the bootloader's `xHC0 ver:` line is built from. Its DMA4
    /// control block carries `src = 0x0200_0004`, `srci = 0x1006` — a 40-bit
    /// source of `0x6_0200_0004`, i.e. BAR0 + 4 = `HCSPARAMS1`.
    #[test]
    fn the_outbound_window_reaches_xhci_capability_registers() {
        let mut p = enumerated_pcie();
        assert_eq!(p.mmio_read(0x6_0200_0000, Width::Word), Some(0x0100_0020));
        assert_eq!(p.mmio_read(0x6_0200_0004, Width::Word), Some(0x0500_0420));
        assert_eq!(p.mmio_read(0x6_0200_0010, Width::Word), Some(0x0028_41EB));
        // One page, and not a byte more.
        assert_eq!(p.mmio_read(0x6_0200_1000, Width::Word), None);
    }

    /// With memory decoding still disabled, or the link down, the window
    /// decodes nothing — the DMA engine then reads DRAM, the way an unclaimed
    /// address behaves on real silicon.
    #[test]
    fn endpoint_mmio_needs_the_command_register() {
        let mut p = link_up_pcie();
        p.write(MEM_WIN0_LO, Width::Word, 0x8000_0000).unwrap();
        p.write(MEM_WIN0_BASE_LIMIT, Width::Word, 0x3FF0_0000)
            .unwrap();
        p.write(MEM_WIN0_BASE_HI, Width::Word, 6).unwrap();
        p.write(MEM_WIN0_LIMIT_HI, Width::Word, 6).unwrap();
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        p.write(EXT_CFG_DATA + 0x10, Width::Word, 0x8200_0000)
            .unwrap();
        assert_eq!(p.mmio_read(0x6_0200_0000, Width::Word), None);
    }

    /// `USBCMD.HCRST` self-clears and returns the operational registers to
    /// their power-on state; `USBSTS.HCH` follows `USBCMD.RS`. Those two are
    /// the only live bits the bootloader's bring-up waits on.
    #[test]
    fn host_controller_reset_completes() {
        let mut p = enumerated_pcie();
        let usbcmd = 0x6_0200_0020;
        let usbsts = 0x6_0200_0024;
        assert_eq!(p.mmio_read(usbsts, Width::Word), Some(1)); // HCHalted
        let mut mem = crate::periph::xhci::VecMem::default();
        p.mmio_write(usbcmd, Width::Word, 1 << 1, &mut mem); // HCRST
        assert_eq!(p.mmio_read(usbcmd, Width::Word), Some(0));
        p.mmio_write(usbcmd, Width::Word, 1, &mut mem); // Run/Stop
        assert_eq!(p.mmio_read(usbsts, Width::Word), Some(0));
        // `USBSTS` is write-1-to-clear. The bring-up's stop path writes
        // all-ones; a plain register would read that straight back.
        p.mmio_write(usbsts, Width::Word, 0xFFFF_FFFF, &mut mem);
        assert_eq!(p.mmio_read(usbsts, Width::Word), Some(0));
        // Root port 1 carries the on-board VIA hub, so it reports a connect;
        // the four USB3 ports read "powered, empty".
        assert_eq!(p.mmio_read(0x6_0200_0420, Width::Word), Some(0x4002_02E1));
        assert_eq!(p.mmio_read(0x6_0200_0460, Width::Word), Some(0x2A0));
    }

    /// The root-port bridge header the bootloader builds at `0x000A6E80`
    /// onwards is written a halfword at a time; neighbours must survive.
    #[test]
    fn root_port_config_is_byte_addressable() {
        let mut p = link_up_pcie();
        p.write(0x22, Width::Half, 0x8000).unwrap();
        p.write(0x20, Width::Half, 0x1230).unwrap();
        assert_eq!(rd(&mut p, 0x20), 0x8000_1230);
    }

    /// `pcie_pme_probe` clears the root port's PME status with a
    /// read-modify-write that sets the bit; on RW1C it must stay clear, or
    /// `pcie_pme_irq` claims every interrupt on the line.
    #[test]
    fn root_status_pme_is_write_one_to_clear() {
        let mut p = link_up_pcie();
        let rtsta = rd(&mut p, 0xCC);
        p.write(0xCC, Width::Word, rtsta | 1 << 16).unwrap();
        assert_eq!(rd(&mut p, 0xCC) & 1 << 16, 0);
        // A set bit is cleared by a one and kept by a zero; the RW bits of
        // the same word (DevCtl) still take what is written.
        p.storage.insert(0xB4, 0x0005_2C10);
        p.write(0xB4, Width::Half, 0x2C1F).unwrap();
        assert_eq!(rd(&mut p, 0xB4), 0x0005_2C1F);
        p.write(0xB6, Width::Half, 0x0001).unwrap();
        assert_eq!(rd(&mut p, 0xB4), 0x0004_2C1F);
    }
}
