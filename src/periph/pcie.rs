//! BCM2711 PCIe root complex at `0x7D50_0000` — Linux's `pcie@7d500000`,
//! driven by `pcie-brcmstb`. Registers and fields: `specs/pcie.toml`.
//!
//! Behind it sits the Pi 4B's VL805 xHCI controller ([`super::vl805`], with its
//! register block in [`super::xhci`]). This file is the root complex: the
//! config-space router, the windows and the interrupts. The endpoint is
//! attached by default because a Pi 4B has it soldered on; `PIMU_PCIE_DEVICE=0`
//! unsolders it, for reproducing the `PCIe timeout` / `USB xHC init failed`
//! transcript and nothing else.
//!
//! The four things that are not obvious from the registers:
//!
//! * **The bridge soft reset in `RGR1_SW_INIT_1` also returns the MSI block to
//!   its reset state** — nothing pending, every vector masked — and the block
//!   takes no messages while held. Linux depends on it: `brcm_pcie_setup()`
//!   never clears the block and `brcm_msi_set_regs()` unmasks before it clears,
//!   so a vector the firmware's USB traffic left pending behind the mask would
//!   become an unhandled interrupt storm on `GIC_SPI 148`.
//! * **Bus 0 is not reachable through `EXT_CFG_INDEX`** ([`Pcie::ext_target`]):
//!   the window turns its index into a configuration request on the link, and
//!   the root port's own config space is only the direct view at `+0x0000`.
//!   Both `pcie-brcmstb` and the bootloader special-case it.
//! * **The VPU cannot reach BAR0 with a load.** `CPU_2_PCIE_MEM_WIN0` puts the
//!   endpoint at CPU-physical `0x6_0000_0000..`, 35 bits, so the firmware goes
//!   through the 40-bit DMA4 channel into a bounce buffer. This file translates
//!   CPU-physical → PCI bus → BAR0 offset ([`Pcie::mmio_read`] /
//!   [`Pcie::mmio_write`]) and [`Machine::run_dma4`](crate::machine::Machine)
//!   calls it with the composed address. [`crate::arm`] routes the ARM's
//!   accesses to the same window.
//! * **Endpoint DMA comes back through inbound window 2**
//!   (`RC_BAR2_CONFIG_LO`/`_HI`): a bus address inside it reaches CPU-physical
//!   memory at the same offset from 0, one outside it reaches nothing
//!   ([`Upstream`]). The bootloader programs it at bus 0, so its bus addresses
//!   are physical; Linux moves it to PCI `0x4_0000_0000` per `dma-ranges`.
//!
//! What Linux needs beyond the bootloader: the port-mode bit of
//! `MISC_PCIE_STATUS` read as the strap it is (it fails the probe with `PCIe RC
//! controller misconfigured as Endpoint` otherwise), the root port's config
//! space as a real type-1 header with capabilities ([`RC_CFG_SEED`], measured
//! on a Raspberry Pi 4B d03115), the SerDes MDIO port for `brcm_pcie_set_ssc()`,
//! and the MSI block, which catches the endpoint's memory write to
//! `MSI_BAR_CONFIG` and turns it into `GIC_SPI 148` ([`Pcie::msi_line`]).

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};
use crate::periph::vl805::Vl805;
use crate::periph::xhci::HostMem;
/// `reg = <0x0 0x7d500000 0x0 0x9310>` in the Pi 4 device tree
/// (`/proc/device-tree/scb/pcie@7d500000/reg` on a Raspberry Pi 4B d03115).
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

const RC_CFG: u32 = 0x0000;
const RC_CFG_SIZE: u32 = 0x1000;
const EXT_CFG_END: u32 = EXT_CFG_DATA + EXT_CFG_DATA_COUNT * EXT_CFG_DATA_STRIDE;
const MDIO_BLOCK_SELECT: u32 = 0x1F;
const SSC_BLOCK: u16 = 0x1100;
const SSC_STATUS: u32 = 0x1;
const SSC_CNTL: u32 = 0x2;
const SSC_CNTL_OVRD: u16 = 0xC000;
const SSC_STATUS_SSC: u16 = 0x400;
const SSC_STATUS_PLL_LOCK: u16 = 0x800;
/// Link status of the trained link: 5 GT/s (`CLS` 2), x1, slot clock. That is
/// what a Raspberry Pi 4B d03115 reports as `LnkSta: Speed 5GT/s, Width x1` /
/// `SlotClk+`, and Linux prints it as `link up, 5.0 GT/s PCIe x1`.
const LNKSTA_UP: u32 = 0x1012;
const LNKSTA_SLOTCLK: u32 = 0x1000;

const ENDPOINT_BUS: u32 = 1;

/// The root port's own configuration space, measured on a Raspberry Pi 4B
/// d03115. All 4 KiB is the direct `+0x0000` view, so it carries the Broadcom
/// `PRIV1` registers from `0x400` up too. The dump is of a running system, so
/// the fields Linux or the firmware had written are back at their power-on
/// values and the rest is verbatim. The header word at `0x08` is not stored: it
/// is a view of [`PRIV1_ID_VAL3`], which both the bootloader and Linux write.
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

/// Which bits of each root-port header word software may change; the rest reads
/// back what it holds, which is what makes the bridge have no BARs, no I/O
/// window and no expansion ROM. Past the header the block is plain storage.
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

/// Status bits past the header that software clears by writing ones (RW1C).
/// Linux clears the PME status with a read-modify-write, so as plain storage
/// the clear would set it for good and `pcie_pme_irq` would claim every
/// interrupt on the line — a silent kernel hang.
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
/// `brcm_pcie_encode_ibar_size()`; anything else switches the window off.
fn ibar_size(code: u32) -> Option<u64> {
    match code {
        0x01..=0x15 => Some(1 << (code + 15)),
        0x1C..=0x1F => Some(1 << (code - 0x1C + 12)),
        _ => None,
    }
}

/// The endpoint's upstream traffic, as the root complex forwards it through
/// inbound window 2. Outside the window nothing answers: a read returns
/// all-ones and a write is dropped, as an Unsupported Request does.
struct Upstream<'a> {
    window: Option<(u64, u64)>,
    mem: &'a mut dyn HostMem,
    log: &'a Log,
}

impl Upstream<'_> {
    fn phys(&self, bus: u64, write: bool) -> Option<u64> {
        let phys = self
            .window
            .and_then(|(base, size)| bus.checked_sub(base).filter(|off| *off < size));
        if phys.is_none() {
            let dir = if write { "write" } else { "read" };
            crate::log!(
                self.log,
                Channel::Pcie,
                "endpoint {dir} at bus {bus:#x} is outside the inbound window"
            );
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
    sw_init: u32,
    link_up: bool,
    ext_cfg_index: u32,
    device_present: bool,
    pub endpoint: Vl805,
    mdio_pkt: u32,
    mdio_block: u16,
    mdio_regs: BTreeMap<(u16, u32), u16>,
    mdio_wr: u32,
    msi_status: u32,
    msi_mask: u32,
    /// An interrupt asserted while bus mastering was off: an MSI is an edge,
    /// so no message went out.
    msi_sent: bool,
    intx: bool,
    log: Log,
}

impl Default for Pcie {
    fn default() -> Self {
        Pcie::new()
    }
}

impl Pcie {
    pub fn new() -> Pcie {
        Pcie::with_device(std::env::var("PIMU_PCIE_DEVICE").as_deref() != Ok("0"))
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
            log: Log::default(),
        }
    }

    pub fn set_log(&mut self, log: Log) {
        self.endpoint.xhci.log = log.clone();
        self.log = log;
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

    pub fn msi_line(&self) -> bool {
        self.msi_status & !self.msi_mask != 0
    }

    pub fn intx_line(&self) -> bool {
        self.intx
    }

    /// Turn the endpoint's interrupt into what it sends upstream: an MSI when
    /// the host enabled one, INTA otherwise.
    fn update_irq(&mut self) {
        let pending = self.link_up && self.endpoint.xhci.interrupt_pending();
        let before = (self.intx, self.msi_status);
        self.route_irq(pending);
        if before != (self.intx, self.msi_status) {
            crate::log!(
                self.log,
                Channel::Pcie,
                "endpoint irq {pending}: intx {} msi status {:#x} mask {:#x}",
                self.intx,
                self.msi_status,
                self.msi_mask
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
                    // 5.5.2.1); Linux relies on it (`ip_autoclear`).
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

    /// An upstream memory write, which the root complex claims when it is its
    /// MSI target.
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
            return;
        }
        self.msi_status |= 1 << (data & !mask & 0x1F);
    }

    fn mdio_regad(&self) -> u32 {
        self.mdio_pkt & MDIO_REGAD
    }

    /// `RD_DATA`: the register the last packet addressed, and done. The only
    /// register with behaviour is SSC status — spread spectrum on once both
    /// override bits are set, and the PLL always locked.
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

    pub fn outbound_bus_addr(&self, cpu: u64) -> Option<u64> {
        let (base, limit, bus) = self.outbound_window()?;
        if cpu < base || cpu > limit {
            return None;
        }
        Some(bus + (cpu - base))
    }

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
    /// decodes there, and the DMA engine falls back to DRAM as silicon does.
    pub fn mmio_read(&mut self, cpu: u64, width: Width) -> Option<u32> {
        let off = self.bar0_offset(cpu)?;
        Some(self.endpoint.bar0_read(off, width))
    }

    /// Bring the endpoint's clock to `now_us`, bringing up any SuperSpeed link
    /// that has finished training.
    pub fn advance_to(&mut self, now_us: u64, mem: &mut dyn HostMem) {
        let links = self.endpoint.xhci.link_due(now_us);
        let events = self
            .endpoint
            .xhci
            .deferred_due()
            .is_some_and(|due| due <= now_us);
        if !links && !events {
            return;
        }
        let mut up = Upstream {
            window: self.inbound_window(),
            mem,
            log: &self.log,
        };
        if links {
            self.endpoint.xhci.train_links(&mut up);
        }
        if events {
            self.endpoint.xhci.drain_deferred(now_us, &mut up);
        }
        self.update_irq();
    }

    fn inbound_window(&self) -> Option<(u64, u64)> {
        let lo = self.stored(RC_BAR2_CONFIG_LO);
        let size = ibar_size(lo & RC_BAR_SIZE_MASK)?;
        let base = ((self.stored(RC_BAR2_CONFIG_HI) as u64) << 32) | lo as u64;
        Some((base & !(size - 1), size))
    }

    /// A write of endpoint MMIO, addressed CPU-physically. `mem` is system
    /// memory: a doorbell makes the endpoint fetch TRBs and post events.
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
                    log: &self.log,
                };
                self.endpoint.bar0_write(off, width, value, &mut up);
                self.update_irq();
                true
            }
            None => false,
        }
    }

    /// Bus 0 is deliberately not one of them (module docs): the bootloader's
    /// scan prints every function whose vendor id answers, and a real board's
    /// log does not show the root complex among them.
    fn ext_target(&self) -> CfgTarget {
        let bus = (self.ext_cfg_index >> EXT_BUSNUM_SHIFT) & 0xFF;
        let slot = (self.ext_cfg_index >> EXT_SLOT_SHIFT) & 0x1F;
        let func = (self.ext_cfg_index >> EXT_FUNC_SHIFT) & 0x7;
        match (bus, slot, func) {
            (ENDPOINT_BUS, 0, 0) if self.link_up => CfgTarget::Endpoint,
            _ => CfgTarget::None,
        }
    }

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
                // The link trains. Silicon takes a few milliseconds, which
                // the model skips: a millisecond of transcript time.
                self.link_up = true;
            }
            self.storage.insert(offset, value);
            self.update_irq();
            return Ok(());
        }
        if offset < RC_CFG + RC_CFG_SIZE {
            // Byte-addressable: the bootloader writes single halfwords into
            // the bridge header.
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
        if matches!(offset, RC_BAR2_CONFIG_LO | RC_BAR2_CONFIG_HI) {
            crate::log!(
                self.log,
                Channel::Pcie,
                "inbound window {:x?}",
                self.inbound_window()
            );
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

    #[test]
    fn perst_release_brings_the_link_up() {
        let mut p = Pcie::with_device(true);
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0x80);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x3).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0x80);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x1).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0x80);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x0).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), 0xB0);
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

    /// `brcm_pcie_setup()` reads the port-mode strap with PERST# held.
    #[test]
    fn root_complex_mode_is_a_strap() {
        let mut p = Pcie::with_device(true);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x3).unwrap();
        p.write(RGR1_SW_INIT_1, Width::Word, 0x1).unwrap();
        assert_eq!(rd(&mut p, MISC_PCIE_STATUS), STATUS_PORT_RC);
        assert!(rd(&mut p, MISC_REVISION) >= 0x0303);
    }

    /// What Linux's bus scan needs: no BARs to size, writable bus numbers, and
    /// a PCIe capability reporting a root port on a trained link.
    #[test]
    fn the_root_port_is_a_bridge_without_bars() {
        let mut p = link_up_pcie();
        assert_eq!(rd(&mut p, 0x08), 0x0604_0020);
        p.write(0x10, Width::Word, 0xFFFF_FFFF).unwrap();
        assert_eq!(rd(&mut p, 0x10), 0);
        p.write(0x18, Width::Word, 0xFF01_0100).unwrap();
        assert_eq!(rd(&mut p, 0x18), 0x0001_0100);
        assert_eq!(rd(&mut p, 0x34) & 0xFF, 0x48);
        assert_eq!((rd(&mut p, 0x48) >> 8) & 0xFF, 0xAC);
        assert_eq!((rd(&mut p, 0xAC) >> 20) & 0xF, 4);
        assert_eq!(p.read(0xBE, Width::Half).unwrap(), 0x1012);
        p.write(RGR1_SW_INIT_1, Width::Word, 0x1).unwrap();
        assert_eq!(p.read(0xBE, Width::Half).unwrap(), 0x1000);
    }

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

    /// MSI on at both ends and interrupter 0 running, as Linux leaves it.
    fn msi_pcie(mem: &mut crate::periph::xhci::VecMem) -> (Pcie, u64, u64) {
        use crate::periph::xhci::{HostMem, RTSOFF};
        let mut p = enumerated_pcie();
        p.write(MSI_INTR2_MASK_CLR, Width::Word, 0xFFFF_FFFF)
            .unwrap();
        p.write(MSI_BAR_CONFIG_LO, Width::Word, 0xFFFF_FFFD)
            .unwrap();
        p.write(MSI_BAR_CONFIG_HI, Width::Word, 0).unwrap();
        p.write(MSI_DATA_CONFIG, Width::Word, 0xFFE0_6540).unwrap();
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

    /// Linux acknowledges only the MSI block (`ip_autoclear`), so the next
    /// event must still send a message of its own.
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
        let changes = (1 << 17) | (1 << 21);
        p.mmio_write(port1, Width::Word, (1 << 9) | changes, &mut mem);
        p.mmio_write(port1, Width::Word, (1 << 9) | (1 << 4), &mut mem);
        assert!(p.msi_line(), "a second message");
    }

    /// The endpoint's first event, all the way to the GIC line.
    #[test]
    fn an_xhci_event_arrives_as_an_msi() {
        let mut mem = crate::periph::xhci::VecMem::default();
        let (mut p, bar, ir0) = msi_pcie(&mut mem);
        assert!(!p.msi_line());
        p.mmio_write(bar + 0x420, Width::Word, (1 << 9) | (1 << 4), &mut mem);
        assert!(p.msi_line());
        assert_eq!(rd(&mut p, MSI_INTR2_STATUS), 1);
        assert!(!p.intx_line());
        p.mmio_write(ir0, Width::Word, 0x3, &mut mem);
        p.write(MSI_INTR2_CLR, Width::Word, 1).unwrap();
        assert!(!p.msi_line());
    }

    /// A vector left pending behind the mask must not survive Linux's bridge
    /// reset (module docs).
    #[test]
    fn the_bridge_reset_clears_a_stale_msi() {
        let mut p = enumerated_pcie();
        p.write(MSI_INTR2_SET, Width::Word, 1).unwrap();
        assert!(!p.msi_line(), "masked");
        for v in [0x2, 0x3, 0x1] {
            p.write(RGR1_SW_INIT_1, Width::Word, v).unwrap();
        }
        p.write(MSI_INTR2_MASK_CLR, Width::Word, 0xFFFF_FFFF)
            .unwrap();
        assert_eq!(rd(&mut p, MSI_INTR2_STATUS), 0);
        assert!(!p.msi_line());
    }

    /// Without MSI the same event is INTA, and PERST# must take it away.
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

    /// Linux's inbound window at PCI `0x4_0000_0000`: the endpoint's DMA
    /// addresses are bus addresses, the memory behind it is physical.
    #[test]
    fn endpoint_dma_goes_through_the_inbound_window() {
        use crate::periph::xhci::{VecMem, RTSOFF};
        let mut p = enumerated_pcie();
        p.write(RC_BAR2_CONFIG_LO, Width::Word, 0xF).unwrap();
        p.write(RC_BAR2_CONFIG_HI, Width::Word, 4).unwrap();
        assert_eq!(p.inbound_window(), Some((0x4_0000_0000, 1 << 30)));
        let mut mem = VecMem::default();
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
        assert_eq!(rd(&mut p, EXT_CFG_DATA), 0xFFFF_FFFF);
        assert_eq!(rd(&mut p, RC_CFG), 0x2711_14E4);
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA), 0x3483_1106);
        assert_eq!(p.read(EXT_CFG_DATA, Width::Half).unwrap(), 0x1106);
        assert_eq!(p.read(EXT_CFG_DATA + 2, Width::Half).unwrap(), 0x3483);
        assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x08) >> 8, 0x0C_0330);
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
        assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x10), 0x0000_0004);
        p.write(EXT_CFG_DATA + 0x10, Width::Word, 0xFFFF_FFFF)
            .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x10), 0xFFFF_F004);
        p.write(EXT_CFG_DATA + 0x10, Width::Word, 0xC000_0000)
            .unwrap();
        assert_eq!(rd(&mut p, EXT_CFG_DATA + 0x10), 0xC000_0004);
        assert_eq!(p.endpoint.bar0_bus_addr(), None);
        p.write(EXT_CFG_DATA + 0x04, Width::Word, 0x0146).unwrap();
        assert_eq!(p.endpoint.bar0_bus_addr(), Some(0xC000_0000));
    }

    /// The bootloader's hub-firmware upload reads every byte straight back, so
    /// a sticky index/data port is the whole requirement.
    #[test]
    fn vendor_port_round_trips_the_firmware_upload() {
        let mut p = link_up_pcie();
        p.write(EXT_CFG_INDEX, Width::Word, 1 << EXT_BUSNUM_SHIFT)
            .unwrap();
        for (i, byte) in [0xDEu32, 0xAD, 0xBE, 0xEF].into_iter().enumerate() {
            let idx = 0x5_2000 + i as u32;
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

    /// The bootloader's outbound-window programming and BAR assignment.
    fn enumerated_pcie() -> Pcie {
        let mut p = link_up_pcie();
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
        // The firmware picks `0x8000_0000` for the bus side where Linux picks
        // `0xC000_0000`, so the model has to read the register, not the DT.
        assert_eq!(p.outbound_bus_addr(0x6_0000_0000), Some(0x8000_0000));
        assert_eq!(p.outbound_bus_addr(0x6_3FFF_FFFF), Some(0xBFFF_FFFF));
        assert_eq!(p.outbound_bus_addr(0x5_FFFF_FFFF), None);
        assert_eq!(p.outbound_bus_addr(0x6_4000_0000), None);
    }

    /// The read the bootloader's `xHC0 ver:` line is built from: a 40-bit DMA4
    /// source of `0x6_0200_0004`, BAR0 + 4.
    #[test]
    fn the_outbound_window_reaches_xhci_capability_registers() {
        let mut p = enumerated_pcie();
        assert_eq!(p.mmio_read(0x6_0200_0000, Width::Word), Some(0x0100_0020));
        assert_eq!(p.mmio_read(0x6_0200_0004, Width::Word), Some(0x0500_0420));
        assert_eq!(p.mmio_read(0x6_0200_0010, Width::Word), Some(0x0028_41EB));
        assert_eq!(p.mmio_read(0x6_0200_1000, Width::Word), None);
    }

    /// With memory decoding off or the link down the window decodes nothing.
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

    /// `USBCMD.HCRST` self-clears and `USBSTS.HCH` follows `USBCMD.RS`: the
    /// only live bits the bootloader's bring-up waits on.
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
        p.mmio_write(usbsts, Width::Word, 0xFFFF_FFFF, &mut mem);
        assert_eq!(p.mmio_read(usbsts, Width::Word), Some(0));
        assert_eq!(p.mmio_read(0x6_0200_0420, Width::Word), Some(0x4002_02E1));
        assert_eq!(p.mmio_read(0x6_0200_0460, Width::Word), Some(0x2A0));
    }

    /// The root-port bridge header the bootloader builds is written a
    /// halfword at a time; neighbours must survive.
    #[test]
    fn root_port_config_is_byte_addressable() {
        let mut p = link_up_pcie();
        p.write(0x22, Width::Half, 0x8000).unwrap();
        p.write(0x20, Width::Half, 0x1230).unwrap();
        assert_eq!(rd(&mut p, 0x20), 0x8000_1230);
    }

    /// `pcie_pme_probe`'s read-modify-write must leave PME status clear.
    #[test]
    fn root_status_pme_is_write_one_to_clear() {
        let mut p = link_up_pcie();
        let rtsta = rd(&mut p, 0xCC);
        p.write(0xCC, Width::Word, rtsta | 1 << 16).unwrap();
        assert_eq!(rd(&mut p, 0xCC) & 1 << 16, 0);
        p.storage.insert(0xB4, 0x0005_2C10);
        p.write(0xB4, Width::Half, 0x2C1F).unwrap();
        assert_eq!(rd(&mut p, 0xB4), 0x0005_2C1F);
        p.write(0xB6, Width::Half, 0x0001).unwrap();
        assert_eq!(rd(&mut p, 0xB4), 0x0004_2C1F);
    }
}
