//! The xHCI register block and ring engine behind the VL805's BAR0 — issue #18
//! stage 3.
//!
//! Stage 2a made the capability registers readable (the firmware reaches them
//! by 40-bit DMA, not by a load — see [`super::pcie`]). This file is the half
//! that makes them mean something: the operational registers, the command and
//! event rings, slot and endpoint contexts, the doorbells, and the port state
//! machine that tells the bootloader a device is attached.
//!
//! ## Where the numbers come from
//!
//! Every capability value is measured on `rpi-dev` through `/dev/mem`
//! (`docs/usb-xhci.md` §2); `dmesg`'s `hcc params 0x002841eb hci version 0x100`
//! cross-checks the pair that matters. The `PORTSC` values are measured too,
//! by moving a stick between sockets and re-reading:
//!
//! ```text
//! 0x400202e1  USB2 port, device just connected     (CCS=1 PLS=Polling CSC=1 DR=1)
//! 0x40000e03  USB2 port, enumerated                (CCS=1 PED=1 PLS=U0 speed=High)
//! 0x00021203  USB3 port, SuperSpeed device         (CCS=1 PED=1 PLS=U0 speed=SS)
//! 0x000002a0  empty but powered                    (PLS=RxDetect PP=1)
//! ```
//!
//! ## The topology this models
//!
//! A Pi 4B has a VIA Labs four-port hub soldered to xHCI root port 1 — that is
//! the USB2 half of all four type-A sockets — and routes root ports 2 and 3 to
//! the two blue sockets' SuperSpeed lanes. Ports 4 and 5 go nowhere. So the hub
//! is present on every board, plugged in or not, and it is why the reference
//! log's pre-handover `XHCI-STOP` prints `USBSTS 18` (`EINT | PCD`) rather than
//! `USBSTS 0`.
//!
//! ## What the engine does and does not do
//!
//! It is a synchronous model: a doorbell write runs the ring it points at to
//! completion and posts the events before the write returns. There is no
//! MSI/MSI-X delivery — the bootloader polls `USBSTS` and the event ring — no
//! streams, no isochronous endpoints, and no scratchpad buffer use (the
//! firmware still allocates them; the model simply never touches them).

use std::collections::BTreeMap;

use crate::bus::Width;
use crate::periph::usb::{Setup, Speed, UsbDevice, Xfer};
use crate::spec::xhci as regs;
use crate::spec::xhci::{
    CRCR_HI, CRCR_LO, DCBAAP_LO, DOORBELL, DOORBELL_COUNT, DOORBELL_STRIDE, ERDP_LO,
    ERDP_LO_EHB_MASK, ERSTBA_LO, ERSTSZ, IMAN, IMAN_IE_MASK as IMAN_IE, IMAN_IP_MASK as IMAN_IP,
    PAGESIZE, PAGESIZE_RESET, PORTSC, PORTSC_CCS_MASK as PORTSC_CCS, PORTSC_CEC_MASK as PORTSC_CEC,
    PORTSC_CSC_MASK as PORTSC_CSC, PORTSC_DR_MASK as PORTSC_DR, PORTSC_OCC_MASK as PORTSC_OCC,
    PORTSC_PEC_MASK as PORTSC_PEC, PORTSC_PED_MASK as PORTSC_PED, PORTSC_PLC_MASK as PORTSC_PLC,
    PORTSC_PLS_MASK, PORTSC_PLS_SHIFT, PORTSC_PP_MASK as PORTSC_PP, PORTSC_PRC_MASK as PORTSC_PRC,
    PORTSC_PR_MASK as PORTSC_PR, PORTSC_SPEED_SHIFT, PORTSC_STRIDE, PORTSC_WPR_MASK as PORTSC_WPR,
    PORTSC_WRC_MASK as PORTSC_WRC, USBCMD, USBCMD_HCRST_MASK as USBCMD_HCRST,
    USBCMD_INTE_MASK as USBCMD_INTE, USBCMD_LHCRST_MASK as USBCMD_LHCRST,
    USBCMD_RS_MASK as USBCMD_RS, USBSTS, USBSTS_EINT_MASK as USBSTS_EINT,
    USBSTS_HCH_MASK as USBSTS_HCH, USBSTS_HSE_MASK as USBSTS_HSE, USBSTS_PCD_MASK as USBSTS_PCD,
    USBSTS_SRE_MASK as USBSTS_SRE,
};
use crate::spec::Coverage;

/// Every register in `specs/xhci.toml` is modelled: the capabilities read the
/// measured values, the rest is register, ring or port state, or storage.
pub const COVERAGE: Coverage = Coverage {
    block: "xhci",
    decoded: &[
        regs::CAPLENGTH,
        regs::HCIVERSION,
        regs::HCSPARAMS1,
        regs::HCSPARAMS2,
        regs::HCSPARAMS3,
        regs::HCCPARAMS1,
        regs::DBOFF,
        regs::RTSOFF,
        regs::HCCPARAMS2,
        regs::USBCMD,
        regs::USBSTS,
        regs::PAGESIZE,
        regs::DNCTRL,
        regs::CRCR_LO,
        regs::CRCR_HI,
        regs::DCBAAP_LO,
        regs::DCBAAP_HI,
        regs::CONFIG,
        regs::USBLEGSUP,
        regs::SUPPORTED_USB2,
        regs::SUPPORTED_USB2_NAME,
        regs::SUPPORTED_USB2_PORTS,
        regs::SUPPORTED_USB3,
        regs::SUPPORTED_USB3_NAME,
        regs::SUPPORTED_USB3_PORTS,
        regs::DOORBELL,
        regs::MFINDEX,
        regs::IMAN,
        regs::IMOD,
        regs::ERSTSZ,
        regs::ERSTBA_LO,
        regs::ERSTBA_HI,
        regs::ERDP_LO,
        regs::ERDP_HI,
        regs::DEBUG_CAP,
        regs::PORTSC,
        regs::PORTPMSC,
        regs::PORTLI,
        regs::PORTHLPMC,
    ],
};

// ---------------------------------------------------------------------------
// Host memory, as the endpoint sees it.

/// The memory the endpoint's DMA reaches. The controller calls it with the
/// PCI bus addresses written into `DCBAAP`, `CRCR` and the TRBs; the root
/// complex ([`crate::periph::pcie`]) translates those through its inbound
/// window `RC_BAR2` and hands the result to system memory, itself a `HostMem`
/// addressed CPU-physically.
pub trait HostMem {
    fn read8(&self, addr: u64) -> u8;
    fn write8(&mut self, addr: u64, value: u8);

    fn read32(&self, addr: u64) -> u32 {
        u32::from_le_bytes([
            self.read8(addr),
            self.read8(addr + 1),
            self.read8(addr + 2),
            self.read8(addr + 3),
        ])
    }

    fn write32(&mut self, addr: u64, value: u32) {
        for (i, b) in value.to_le_bytes().iter().enumerate() {
            self.write8(addr + i as u64, *b);
        }
    }

    fn read64(&self, addr: u64) -> u64 {
        self.read32(addr) as u64 | ((self.read32(addr + 4) as u64) << 32)
    }

    fn read_bytes(&self, addr: u64, len: usize) -> Vec<u8> {
        (0..len as u64).map(|i| self.read8(addr + i)).collect()
    }

    fn write_bytes(&mut self, addr: u64, data: &[u8]) {
        for (i, b) in data.iter().enumerate() {
            self.write8(addr + i as u64, *b);
        }
    }
}

/// DRAM by CPU-physical address, which on this SoC is the offset into it.
/// Past its end nothing answers: reads return zero, writes are dropped.
impl HostMem for crate::mem::Ram {
    fn read8(&self, addr: u64) -> u8 {
        if addr >= self.len() as u64 {
            return 0;
        }
        self.load(self.base() + addr as u32, Width::Byte)
            .unwrap_or(0) as u8
    }

    fn write8(&mut self, addr: u64, value: u8) {
        if addr < self.len() as u64 {
            let _ = self.store(self.base() + addr as u32, Width::Byte, value as u32);
        }
    }
}

/// A plain byte map, for tests and for anything that needs a host memory that
/// is not the machine's DRAM.
#[derive(Default)]
pub struct VecMem {
    pub bytes: BTreeMap<u64, u8>,
}

impl HostMem for VecMem {
    fn read8(&self, addr: u64) -> u8 {
        self.bytes.get(&addr).copied().unwrap_or(0)
    }
    fn write8(&mut self, addr: u64, value: u8) {
        self.bytes.insert(addr, value);
    }
}

// ---------------------------------------------------------------------------
// Register map

/// `CAPLENGTH`'s value: where the operational registers start.
pub const CAPLENGTH: u32 = regs::CAPLENGTH_RESET;
/// `RTSOFF`'s value: where the runtime registers start.
pub const RTSOFF: u32 = regs::RTSOFF_RESET;
/// `DBOFF`'s value: where the doorbells start.
const DBOFF: u32 = regs::DBOFF_RESET;
// The spec's operational, runtime and doorbell offsets are absolute BAR0
// offsets, so they have to agree with what the capability registers announce.
const _: () = assert!(USBCMD == CAPLENGTH && IMAN == RTSOFF + 0x20 && DOORBELL == DBOFF);

/// The VL805 has five root ports: port 1 USB2, ports 2-5 USB3
/// (`HCSPARAMS1.MaxPorts`).
pub const PORTS: usize = (regs::HCSPARAMS1_RESET >> 24) as usize;
/// `HCSPARAMS1.MaxSlots`.
const MAX_SLOTS: usize = (regs::HCSPARAMS1_RESET & 0xFF) as usize;
const _: () =
    assert!(PORTS == regs::PORTSC_COUNT as usize && MAX_SLOTS + 1 == DOORBELL_COUNT as usize);
/// The write-1-to-clear half of `USBSTS`.
const USBSTS_RW1C: u32 = USBSTS_HSE | USBSTS_EINT | USBSTS_PCD | USBSTS_SRE;

/// Bits the firmware clears by writing one.
const PORTSC_RW1C: u32 =
    PORTSC_CSC | PORTSC_PEC | PORTSC_WRC | PORTSC_OCC | PORTSC_PRC | PORTSC_PLC | PORTSC_CEC;
/// Bits the firmware may set directly (`PP`, the wake enables, `PIC`, `LWS`).
const PORTSC_RW: u32 = PORTSC_PP | (0x3 << 14) | (1 << 16) | (0x7 << 25);

/// `PLS` values used here.
const PLS_U0: u32 = 0;
const PLS_RXDETECT: u32 = 5;
const PLS_POLLING: u32 = 7;

/// `CCS=0 PED=0 PLS=RxDetect PP=1` — powered, nothing attached, the value every
/// unpopulated VL805 port reads (`docs/usb-xhci.md` §5.2).
const PORTSC_EMPTY: u32 = PORTSC_PP | (PLS_RXDETECT << PORTSC_PLS_SHIFT);

/// `ERDP.EHB`, the Event Handler Busy bit the driver clears when it is done.
const ERDP_EHB: u64 = ERDP_LO_EHB_MASK as u64;

// TRB types (xHCI 6.4.6).
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_EVENT_DATA: u32 = 7;
const TRB_NO_OP: u32 = 8;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_ENDPOINT: u32 = 12;
const TRB_EVALUATE_CONTEXT: u32 = 13;
const TRB_RESET_ENDPOINT: u32 = 14;
const TRB_STOP_ENDPOINT: u32 = 15;
const TRB_SET_TR_DEQUEUE: u32 = 16;
const TRB_RESET_DEVICE: u32 = 17;
const TRB_NO_OP_COMMAND: u32 = 23;
const TRB_TRANSFER_EVENT: u32 = 32;
const TRB_COMMAND_COMPLETION: u32 = 33;
const TRB_PORT_STATUS_CHANGE: u32 = 34;

// Completion codes (xHCI 6.4.5).
const CC_SUCCESS: u32 = 1;
const CC_TRB_ERROR: u32 = 5;
const CC_STALL: u32 = 6;
const CC_SLOT_NOT_ENABLED: u32 = 11;
const CC_SHORT_PACKET: u32 = 13;
const CC_NO_SLOTS: u32 = 9;

/// Control-transfer state carried between the Setup, Data and Status stages of
/// one control TRB chain.
#[derive(Default)]
struct ControlState {
    setup: Option<Setup>,
    /// Bytes collected from OUT Data Stage TRBs, handed to the device at the
    /// Status Stage.
    out: Vec<u8>,
}

/// One root port.
struct Port {
    device: Option<Box<dyn UsbDevice>>,
    portsc: u32,
    /// USB2 ports need an explicit reset before they enable; USB3 ports train
    /// their link themselves and come up enabled.
    usb2: bool,
}

impl Port {
    fn new(usb2: bool) -> Port {
        Port {
            device: None,
            portsc: PORTSC_EMPTY | if usb2 { PORTSC_DR } else { 0 },
            usb2,
        }
    }

    /// The resting value with whatever is attached, as measured on a real
    /// board. Called on power-on and on `HCRST`.
    fn settle(&mut self) {
        let base = PORTSC_EMPTY | if self.usb2 { PORTSC_DR } else { 0 };
        self.portsc = match self.device.as_ref().map(|d| d.speed()) {
            None => base,
            Some(Speed::Super) => {
                // `0x00021203` plus the connect-change bit: a SuperSpeed link
                // trains without host intervention, so the port is already
                // enabled when the driver first looks.
                PORTSC_CCS
                    | PORTSC_PED
                    | PORTSC_PP
                    | ((Speed::Super as u32) << PORTSC_SPEED_SHIFT)
                    | (PLS_U0 << PORTSC_PLS_SHIFT)
                    | PORTSC_CSC
            }
            Some(_) => {
                // `0x400202e1` — connected, link polling, waiting for the host
                // to drive a reset.
                PORTSC_CCS | PORTSC_PP | PORTSC_DR | (PLS_POLLING << PORTSC_PLS_SHIFT) | PORTSC_CSC
            }
        };
    }
}

/// A slot the host has enabled. The device context itself lives in host memory
/// — that is the point of the DCBAA — so all that is kept here is whether the
/// slot is in use.
#[derive(Clone, Copy, Default)]
struct Slot {
    enabled: bool,
}

/// The event ring's producer side: the host owns the dequeue pointer, the
/// controller owns this.
#[derive(Default)]
struct EventRing {
    /// Where the next event TRB goes. Zero means "not started yet".
    enqueue: u64,
    segment: u32,
    offset: u32,
    /// The Producer Cycle State.
    cycle: bool,
}

pub struct Xhci {
    /// Sticky operational and runtime registers, keyed by BAR0 offset.
    regs: BTreeMap<u32, u32>,
    ports: Vec<Port>,
    slots: [Slot; MAX_SLOTS + 1],
    event: EventRing,
    /// The command ring dequeue pointer and its Consumer Cycle State.
    cmd_ptr: u64,
    cmd_ccs: bool,
    running: bool,
    dbg: bool,
    /// Observables for tests: how many commands and transfers the engine has
    /// completed.
    pub commands: u64,
    pub transfers: u64,
}

impl Default for Xhci {
    fn default() -> Self {
        Xhci::new()
    }
}

impl Xhci {
    pub fn new() -> Xhci {
        let mut ports = Vec::with_capacity(PORTS);
        // Port 1 is the USB2 root port; ports 2-5 are the four USB3 lanes. That
        // split is the VL805's own supported-protocol extended capabilities,
        // read back from `rpi-dev`: `id=2 "USB " rev 2.0 portoff=1 count=1` and
        // `id=2 "USB " rev 3.0 portoff=2 count=4`.
        ports.push(Port::new(true));
        for _ in 1..PORTS {
            ports.push(Port::new(false));
        }
        let mut hc = Xhci {
            regs: BTreeMap::new(),
            ports,
            slots: [Slot::default(); MAX_SLOTS + 1],
            event: EventRing::default(),
            cmd_ptr: 0,
            cmd_ccs: true,
            running: false,
            dbg: std::env::var("RVF_DBG_XHCI").is_ok(),
            commands: 0,
            transfers: 0,
        };
        hc.settle_ports();
        hc
    }

    /// Plug `device` into root port `port` (1-based).
    pub fn attach(&mut self, port: usize, device: Box<dyn UsbDevice>) {
        self.ports[port - 1].device = Some(device);
        self.ports[port - 1].settle();
    }

    /// The device on root port `port`, for attaching something below a hub.
    pub fn port_device(&mut self, port: usize) -> Option<&mut (dyn UsbDevice + 'static)> {
        self.ports[port - 1].device.as_deref_mut()
    }

    fn settle_ports(&mut self) {
        for p in &mut self.ports {
            p.settle();
        }
    }

    fn reg(&self, off: u32) -> u32 {
        self.regs.get(&off).copied().unwrap_or(0)
    }

    fn set_reg(&mut self, off: u32, v: u32) {
        self.regs.insert(off, v);
    }

    fn reg64(&self, lo: u32) -> u64 {
        self.reg(lo) as u64 | ((self.reg(lo + 4) as u64) << 32)
    }

    fn portsc_index(off: u32) -> Option<usize> {
        let rel = off.checked_sub(PORTSC)?;
        (rel % PORTSC_STRIDE == 0 && rel / PORTSC_STRIDE < PORTS as u32)
            .then_some((rel / PORTSC_STRIDE) as usize)
    }

    // -----------------------------------------------------------------------
    // Register access

    pub fn read(&mut self, off: u32, width: Width) -> u32 {
        let word = self.read_word(off & !3);
        let shift = 8 * (off & 3);
        let mask: u32 = match width {
            Width::Byte => 0xFF,
            Width::Half => 0xFFFF,
            Width::Word => 0xFFFF_FFFF,
        };
        (word >> shift) & mask
    }

    fn read_word(&mut self, off: u32) -> u32 {
        match off {
            // The word at 0 is CAPLENGTH with HCIVERSION in its top half.
            regs::CAPLENGTH => return regs::CAPLENGTH_RESET | regs::HCIVERSION_RESET << 16,
            regs::HCSPARAMS1 => return regs::HCSPARAMS1_RESET,
            regs::HCSPARAMS2 => return regs::HCSPARAMS2_RESET,
            regs::HCSPARAMS3 => return regs::HCSPARAMS3_RESET,
            regs::HCCPARAMS1 => return regs::HCCPARAMS1_RESET,
            regs::DBOFF => return regs::DBOFF_RESET,
            regs::RTSOFF => return regs::RTSOFF_RESET,
            regs::HCCPARAMS2 => return regs::HCCPARAMS2_RESET,
            // Extended capabilities, walked from `HCCPARAMS1.xECP` = 0xA0.
            regs::USBLEGSUP => return regs::USBLEGSUP_RESET,
            regs::SUPPORTED_USB2 => return regs::SUPPORTED_USB2_RESET,
            regs::SUPPORTED_USB2_NAME => return regs::SUPPORTED_USB2_NAME_RESET,
            regs::SUPPORTED_USB2_PORTS => return regs::SUPPORTED_USB2_PORTS_RESET,
            regs::SUPPORTED_USB3 => return regs::SUPPORTED_USB3_RESET,
            regs::SUPPORTED_USB3_NAME => return regs::SUPPORTED_USB3_NAME_RESET,
            regs::SUPPORTED_USB3_PORTS => return regs::SUPPORTED_USB3_PORTS_RESET,
            regs::DEBUG_CAP => return regs::DEBUG_CAP_RESET,
            _ => {}
        }
        if off == PAGESIZE {
            return PAGESIZE_RESET; // 4 KiB pages
        }
        if off == USBSTS {
            let sticky = self.reg(off) & !USBSTS_HCH;
            return if self.running {
                sticky
            } else {
                sticky | USBSTS_HCH
            };
        }
        if off == CRCR_LO {
            // The dequeue pointer reads as zero (xHCI 5.4.5); only the Command
            // Ring Running bit is observable.
            return if self.running && self.cmd_ptr != 0 {
                1 << 3
            } else {
                0
            };
        }
        if off == CRCR_HI {
            return 0;
        }
        if let Some(i) = Xhci::portsc_index(off) {
            return self.ports[i].portsc;
        }
        self.reg(off)
    }

    /// A write to the register block. `mem` is host memory: a doorbell write
    /// runs the ring it points at, which reads TRBs out of DRAM and writes
    /// events back.
    pub fn write(&mut self, off: u32, width: Width, value: u32, mem: &mut dyn HostMem) {
        let word_off = off & !3;
        if word_off < CAPLENGTH {
            return; // capability registers are read-only
        }
        let shift = 8 * (off & 3);
        let mask: u32 = match width {
            Width::Byte => 0xFF << shift,
            Width::Half => 0xFFFF << shift,
            Width::Word => 0xFFFF_FFFF,
        };
        let value = (value << shift) & mask;

        if let Some(i) = Xhci::portsc_index(word_off) {
            self.write_portsc(i, value, mask, mem);
            return;
        }
        // Doorbells: `DBOFF` + 4 * target.
        if (DOORBELL..DOORBELL + DOORBELL_COUNT * DOORBELL_STRIDE).contains(&word_off) {
            let target = (word_off - DOORBELL) / DOORBELL_STRIDE;
            self.set_reg(word_off, value);
            self.ring_doorbell(target, value, mem);
            return;
        }

        let old = self.reg(word_off);
        let new = (old & !mask) | value;

        if word_off == USBSTS {
            // Write-1-to-clear. The bring-up's stop path writes all-ones here;
            // without this the firmware reads its own `0xFFFF_FFFF` back.
            self.set_reg(word_off, old & !(value & USBSTS_RW1C));
            return;
        }
        if word_off == USBCMD {
            self.write_usbcmd(new);
            return;
        }
        if word_off == IMAN {
            // `IP` is write-1-to-clear: the driver acknowledges an interrupt
            // by writing it back (xHCI 5.5.2.1). Only the controller sets it.
            let ip = old & IMAN_IP & !(value & IMAN_IP);
            self.set_reg(word_off, (new & !IMAN_IP) | ip);
            return;
        }
        if word_off == CRCR_LO || word_off == CRCR_HI {
            self.set_reg(word_off, new);
            let ptr = self.reg64(CRCR_LO);
            self.cmd_ptr = ptr & !0x3F;
            if word_off == CRCR_LO {
                self.cmd_ccs = ptr & 1 != 0;
            }
            return;
        }
        self.set_reg(word_off, new);
        if word_off == ERDP_LO {
            // Clearing `EHB` acknowledges the events consumed so far. Nothing
            // in the model depends on it, but it must not be sticky.
            self.set_reg(word_off, new & !(ERDP_EHB as u32));
        }
    }

    fn write_usbcmd(&mut self, new: u32) {
        if new & (USBCMD_HCRST | USBCMD_LHCRST) != 0 {
            self.reset();
            return;
        }
        self.set_reg(USBCMD, new & !(USBCMD_HCRST | USBCMD_LHCRST));
        let run = new & USBCMD_RS != 0;
        if run && !self.running {
            self.running = true;
        } else if !run {
            self.running = false;
        }
    }

    /// `HCRST`: everything back to power-on, including the ports, which
    /// re-report whatever is plugged in. PERST# does the same to the whole
    /// controller.
    pub fn reset(&mut self) {
        self.regs.clear();
        self.slots = [Slot::default(); MAX_SLOTS + 1];
        self.event = EventRing::default();
        self.cmd_ptr = 0;
        self.cmd_ccs = true;
        self.running = false;
        self.settle_ports();
    }

    fn write_portsc(&mut self, i: usize, value: u32, mask: u32, mem: &mut dyn HostMem) {
        let port = &mut self.ports[i];
        // Write-1-to-clear change bits.
        port.portsc &= !(value & PORTSC_RW1C & mask);
        // Plain read/write bits.
        port.portsc = (port.portsc & !(PORTSC_RW & mask)) | (value & PORTSC_RW & mask);
        // Writing PED with a one *disables* the port; it is never set that way.
        if value & mask & PORTSC_PED != 0 {
            port.portsc &= !PORTSC_PED;
        }
        if value & mask & (PORTSC_PR | PORTSC_WPR) != 0 {
            self.reset_port(i, mem);
        }
    }

    /// A port reset. On real silicon this takes milliseconds and completes with
    /// a Port Status Change Event; here it completes before the write returns,
    /// which is the same thing as far as a polling driver is concerned.
    fn reset_port(&mut self, i: usize, mem: &mut dyn HostMem) {
        let speed = {
            let port = &mut self.ports[i];
            let Some(dev) = port.device.as_mut() else {
                // Resetting an empty port just sets the change bit.
                port.portsc &= !PORTSC_PR;
                port.portsc |= PORTSC_PRC;
                return;
            };
            dev.reset();
            dev.speed()
        };
        let port = &mut self.ports[i];
        port.portsc &= !(PORTSC_PR | PORTSC_PLS_MASK);
        port.portsc |= PORTSC_CCS
            | PORTSC_PED
            | PORTSC_PP
            | PORTSC_PRC
            | ((speed as u32) << PORTSC_SPEED_SHIFT)
            | (PLS_U0 << PORTSC_PLS_SHIFT);
        self.port_status_change(i, mem);
    }

    fn port_status_change(&mut self, i: usize, mem: &mut dyn HostMem) {
        let trb = [
            ((i as u32 + 1) << 24),
            0,
            CC_SUCCESS << 24,
            TRB_PORT_STATUS_CHANGE << 10,
        ];
        self.set_reg(USBSTS, self.reg(USBSTS) | USBSTS_PCD);
        self.post_event(trb, mem);
    }

    // -----------------------------------------------------------------------
    // Event ring

    fn post_event(&mut self, mut trb: [u32; 4], mem: &mut dyn HostMem) {
        let erstba = self.reg64(ERSTBA_LO) & !0x3F;
        let erstsz = self.reg(ERSTSZ) & 0xFFFF;
        if erstba == 0 || erstsz == 0 {
            return; // no event ring yet; the event is simply lost, as on silicon
        }
        if self.event.enqueue == 0 {
            self.event.segment = 0;
            self.event.offset = 0;
            self.event.cycle = true;
            self.event.enqueue = mem.read64(erstba) & !0x3F;
        }
        trb[3] = (trb[3] & !1) | u32::from(self.event.cycle);
        let at = self.event.enqueue;
        for (i, w) in trb.iter().enumerate() {
            mem.write32(at + 4 * i as u64, *w);
        }
        if self.dbg {
            eprintln!(
                "[xhci] event@{at:#x} type={} {:08x} {:08x} {:08x} {:08x}",
                (trb[3] >> 10) & 0x3F,
                trb[0],
                trb[1],
                trb[2],
                trb[3]
            );
        }

        // Advance within the segment, then to the next segment, toggling the
        // cycle when the whole ring wraps.
        let seg_entry = erstba + 16 * self.event.segment as u64;
        let seg_size = mem.read32(seg_entry + 8) & 0xFFFF;
        self.event.offset += 1;
        if self.event.offset >= seg_size {
            self.event.offset = 0;
            self.event.segment += 1;
            if self.event.segment >= erstsz {
                self.event.segment = 0;
                self.event.cycle = !self.event.cycle;
            }
            self.event.enqueue = mem.read64(erstba + 16 * self.event.segment as u64) & !0x3F;
        } else {
            self.event.enqueue += 16;
        }

        let sts = self.reg(USBSTS) | USBSTS_EINT;
        self.set_reg(USBSTS, sts);
        let iman = self.reg(IMAN) | IMAN_IP;
        self.set_reg(IMAN, iman);
    }

    // -----------------------------------------------------------------------
    // Rings

    fn ring_doorbell(&mut self, target: u32, value: u32, mem: &mut dyn HostMem) {
        if target == 0 {
            self.run_command_ring(mem);
        } else {
            let dci = value & 0xFF;
            self.run_transfer_ring(target as usize, dci, mem);
        }
    }

    fn read_trb(mem: &dyn HostMem, at: u64) -> [u32; 4] {
        [
            mem.read32(at),
            mem.read32(at + 4),
            mem.read32(at + 8),
            mem.read32(at + 12),
        ]
    }

    fn run_command_ring(&mut self, mem: &mut dyn HostMem) {
        if !self.running || self.cmd_ptr == 0 {
            return;
        }
        for _ in 0..4096 {
            let trb = Xhci::read_trb(mem, self.cmd_ptr);
            if (trb[3] & 1 != 0) != self.cmd_ccs {
                break;
            }
            let kind = (trb[3] >> 10) & 0x3F;
            if kind == TRB_LINK {
                let next = (trb[0] as u64 | ((trb[1] as u64) << 32)) & !0xF;
                if trb[3] & 2 != 0 {
                    self.cmd_ccs = !self.cmd_ccs;
                }
                self.cmd_ptr = next;
                continue;
            }
            if self.dbg {
                eprintln!(
                    "[xhci] cmd@{:#x} type={kind} {:08x} {:08x} {:08x} {:08x}",
                    self.cmd_ptr, trb[0], trb[1], trb[2], trb[3]
                );
            }
            let this = self.cmd_ptr;
            let (code, slot) = self.run_command(kind, &trb, mem);
            self.commands += 1;
            let event = [
                this as u32,
                (this >> 32) as u32,
                code << 24,
                (slot << 24) | (TRB_COMMAND_COMPLETION << 10),
            ];
            self.post_event(event, mem);
            self.cmd_ptr = this + 16;
        }
    }

    /// Run one command TRB. Returns its completion code and the slot id the
    /// completion event carries.
    fn run_command(&mut self, kind: u32, trb: &[u32; 4], mem: &mut dyn HostMem) -> (u32, u32) {
        let param = trb[0] as u64 | ((trb[1] as u64) << 32);
        let slot_id = (trb[3] >> 24) & 0xFF;
        match kind {
            TRB_NO_OP_COMMAND | TRB_NO_OP => (CC_SUCCESS, 0),
            TRB_ENABLE_SLOT => match (1..=MAX_SLOTS).find(|i| !self.slots[*i].enabled) {
                Some(i) => {
                    self.slots[i].enabled = true;
                    (CC_SUCCESS, i as u32)
                }
                None => (CC_NO_SLOTS, 0),
            },
            TRB_DISABLE_SLOT => {
                if let Some(s) = self.slots.get_mut(slot_id as usize) {
                    s.enabled = false;
                }
                (CC_SUCCESS, slot_id)
            }
            TRB_ADDRESS_DEVICE => (self.address_device(slot_id, param, trb, mem), slot_id),
            TRB_CONFIGURE_ENDPOINT | TRB_EVALUATE_CONTEXT => {
                (self.apply_input_context(slot_id, param, kind, mem), slot_id)
            }
            TRB_SET_TR_DEQUEUE => {
                let dci = trb[3] >> 16 & 0x1F;
                if let Some(ctx) = self.device_context(slot_id, mem) {
                    let ep = ctx + 0x20 * dci as u64;
                    mem.write32(ep + 8, param as u32);
                    mem.write32(ep + 12, (param >> 32) as u32);
                }
                (CC_SUCCESS, slot_id)
            }
            TRB_RESET_ENDPOINT | TRB_STOP_ENDPOINT => {
                if let Some(ctx) = self.device_context(slot_id, mem) {
                    let dci = trb[3] >> 16 & 0x1F;
                    let ep = ctx + 0x20 * dci as u64;
                    // Endpoint State back to Running.
                    let dw0 = mem.read32(ep) & !0x7;
                    mem.write32(ep, dw0 | 1);
                }
                (CC_SUCCESS, slot_id)
            }
            TRB_RESET_DEVICE => (CC_SUCCESS, slot_id),
            _ => (CC_TRB_ERROR, slot_id),
        }
    }

    /// The output device context for `slot`, from the Device Context Base
    /// Address Array.
    fn device_context(&self, slot: u32, mem: &dyn HostMem) -> Option<u64> {
        if slot == 0 || slot as usize > MAX_SLOTS || !self.slots[slot as usize].enabled {
            return None;
        }
        let dcbaap = self.reg64(DCBAAP_LO) & !0x3F;
        if dcbaap == 0 {
            return None;
        }
        let ctx = mem.read64(dcbaap + 8 * slot as u64) & !0x3F;
        if ctx == 0 {
            None
        } else {
            Some(ctx)
        }
    }

    fn address_device(
        &mut self,
        slot: u32,
        input: u64,
        trb: &[u32; 4],
        mem: &mut dyn HostMem,
    ) -> u32 {
        let Some(ctx) = self.device_context(slot, mem) else {
            return CC_SLOT_NOT_ENABLED;
        };
        // Input context: 32-byte Input Control Context, then the slot context,
        // then the endpoint contexts (CSZ = 0, so 32 bytes each).
        let in_slot = input + 0x20;
        let slot_dw0 = mem.read32(in_slot);
        let slot_dw1 = mem.read32(in_slot + 4);
        let route = slot_dw0 & 0x000F_FFFF;
        let root_port = ((slot_dw1 >> 16) & 0xFF) as usize;
        let speed = match self.resolve(root_port, route) {
            Some(d) => d.speed(),
            None => return CC_TRB_ERROR,
        };
        // Copy slot and endpoint-0 contexts into the output device context.
        let slot_ctx = mem.read_bytes(in_slot, 0x20);
        mem.write_bytes(ctx, &slot_ctx);
        let ep0 = mem.read_bytes(input + 0x40, 0x20);
        mem.write_bytes(ctx + 0x20, &ep0);
        // Slot Context: speed, then state Addressed (or Default for a Block Set
        // Address Request) and the device address the controller assigned.
        let dw0 = (mem.read32(ctx) & !(0xF << 20)) | ((speed as u32) << 20);
        mem.write32(ctx, dw0);
        let bsr = trb[3] & (1 << 9) != 0;
        let state = if bsr { 1u32 } else { 2 };
        let addr = if bsr { 0 } else { slot };
        mem.write32(ctx + 12, (state << 27) | (addr & 0xFF));
        // Endpoint 0: state Running.
        let ep_dw0 = (mem.read32(ctx + 0x20) & !0x7) | 1;
        mem.write32(ctx + 0x20, ep_dw0);
        CC_SUCCESS
    }

    /// `Configure Endpoint` and `Evaluate Context`: copy the contexts the Input
    /// Control Context's Add flags select into the output device context.
    fn apply_input_context(
        &mut self,
        slot: u32,
        input: u64,
        kind: u32,
        mem: &mut dyn HostMem,
    ) -> u32 {
        let Some(ctx) = self.device_context(slot, mem) else {
            return CC_SLOT_NOT_ENABLED;
        };
        let drop_flags = mem.read32(input);
        let add_flags = mem.read32(input + 4);
        if add_flags & 1 != 0 {
            let slot_ctx = mem.read_bytes(input + 0x20, 0x20);
            // The device address and slot state belong to the controller, so
            // only the first three dwords come from the input context.
            mem.write_bytes(ctx, &slot_ctx[..12]);
        }
        for dci in 1..32u32 {
            let bit = 1u32 << dci;
            if drop_flags & bit != 0 && add_flags & bit == 0 {
                // Dropped: Endpoint State back to Disabled.
                let ep = ctx + 0x20 * dci as u64;
                let dw0 = mem.read32(ep) & !0x7;
                mem.write32(ep, dw0);
                continue;
            }
            if add_flags & bit == 0 {
                continue;
            }
            // Input context: control context, slot context, then endpoint
            // context DCI *n* at `0x20 * (n + 1)`.
            let src = mem.read_bytes(input + 0x20 * (dci as u64 + 1), 0x20);
            let ep = ctx + 0x20 * dci as u64;
            mem.write_bytes(ep, &src);
            // Endpoint State = Running.
            let dw0 = mem.read32(ep) & !0x7;
            mem.write32(ep, dw0 | 1);
        }
        if kind == TRB_CONFIGURE_ENDPOINT {
            // Slot State = Configured.
            let dw3 = mem.read32(ctx + 12);
            mem.write32(ctx + 12, (dw3 & !(0x1F << 27)) | (3 << 27));
        }
        CC_SUCCESS
    }

    /// The device a slot context addresses: root port, then one hub tier per
    /// non-zero nibble of the route string.
    fn resolve(&mut self, root_port: usize, route: u32) -> Option<&mut (dyn UsbDevice + 'static)> {
        if root_port == 0 || root_port > PORTS {
            return None;
        }
        let mut dev: &mut (dyn UsbDevice + 'static) =
            self.ports[root_port - 1].device.as_deref_mut()?;
        for tier in 0..5 {
            let hub_port = ((route >> (4 * tier)) & 0xF) as u8;
            if hub_port == 0 {
                break;
            }
            dev = dev.child(hub_port)?;
        }
        Some(dev)
    }

    /// The device a slot's *device context* addresses, read back out of host
    /// memory.
    fn slot_device(
        &mut self,
        slot: u32,
        mem: &dyn HostMem,
    ) -> Option<&mut (dyn UsbDevice + 'static)> {
        let ctx = self.device_context(slot, mem)?;
        let route = mem.read32(ctx) & 0x000F_FFFF;
        let root_port = ((mem.read32(ctx + 4) >> 16) & 0xFF) as usize;
        self.resolve(root_port, route)
    }

    fn run_transfer_ring(&mut self, slot: usize, dci: u32, mem: &mut dyn HostMem) {
        if !self.running || dci == 0 || dci > 31 {
            return;
        }
        let Some(ctx) = self.device_context(slot as u32, mem) else {
            return;
        };
        let ep_ctx = ctx + 0x20 * dci as u64;
        let deq = mem.read64(ep_ctx + 8);
        let mut ptr = deq & !0xF;
        let mut ccs = deq & 1 != 0;
        if ptr == 0 {
            return;
        }
        let mut ctrl = ControlState::default();
        for _ in 0..4096 {
            let trb = Xhci::read_trb(mem, ptr);
            if (trb[3] & 1 != 0) != ccs {
                break;
            }
            let kind = (trb[3] >> 10) & 0x3F;
            if kind == TRB_LINK {
                let next = (trb[0] as u64 | ((trb[1] as u64) << 32)) & !0xF;
                if trb[3] & 2 != 0 {
                    ccs = !ccs;
                }
                ptr = next;
                continue;
            }
            if self.dbg {
                eprintln!(
                    "[xhci] xfer slot={slot} dci={dci} @{ptr:#x} type={kind} {:08x} {:08x} {:08x} {:08x}",
                    trb[0], trb[1], trb[2], trb[3]
                );
            }
            let (code, residue) =
                self.run_transfer_trb(slot as u32, dci, kind, &trb, &mut ctrl, mem);
            self.transfers += 1;
            // Interrupt On Completion, or Interrupt On Short Packet when the
            // transfer came up short.
            let ioc = trb[3] & (1 << 5) != 0;
            let isp = trb[3] & (1 << 2) != 0 && code == CC_SHORT_PACKET;
            if ioc || isp || code > CC_SHORT_PACKET {
                let ed = kind == TRB_EVENT_DATA;
                let pointer = if ed {
                    trb[0] as u64 | ((trb[1] as u64) << 32)
                } else {
                    ptr
                };
                let event = [
                    pointer as u32,
                    (pointer >> 32) as u32,
                    (code << 24) | (residue & 0x00FF_FFFF),
                    ((slot as u32) << 24)
                        | (dci << 16)
                        | (TRB_TRANSFER_EVENT << 10)
                        | if ed { 1 << 2 } else { 0 },
                ];
                self.post_event(event, mem);
            }
            ptr += 16;
            if code > CC_SHORT_PACKET {
                break; // an error halts the endpoint
            }
        }
        // Hand the dequeue pointer back to the endpoint context.
        mem.write32(ep_ctx + 8, (ptr as u32 & !0xF) | u32::from(ccs));
        mem.write32(ep_ctx + 12, (ptr >> 32) as u32);
    }

    /// Run one transfer TRB. Returns its completion code and the untransferred
    /// residue.
    fn run_transfer_trb(
        &mut self,
        slot: u32,
        dci: u32,
        kind: u32,
        trb: &[u32; 4],
        ctrl: &mut ControlState,
        mem: &mut dyn HostMem,
    ) -> (u32, u32) {
        let buf = trb[0] as u64 | ((trb[1] as u64) << 32);
        let len = (trb[2] & 0x1_FFFF) as usize;
        let idt = trb[3] & (1 << 6) != 0;
        match kind {
            TRB_SETUP => {
                let mut b = [0u8; 8];
                b[..4].copy_from_slice(&trb[0].to_le_bytes());
                b[4..].copy_from_slice(&trb[1].to_le_bytes());
                *ctrl = ControlState {
                    setup: Some(Setup::from_bytes(b)),
                    out: Vec::new(),
                };
                (CC_SUCCESS, 0)
            }
            TRB_DATA | TRB_STATUS | TRB_NORMAL if dci == 1 => {
                let Some(setup) = ctrl.setup else {
                    return (CC_TRB_ERROR, len as u32);
                };
                let dir_in = if kind == TRB_STATUS {
                    // The Status Stage runs opposite the data direction; it
                    // moves no bytes either way.
                    return self.control_status(slot, &setup, ctrl, mem);
                } else {
                    trb[3] & (1 << 16) != 0
                };
                if dir_in {
                    let Some(dev) = self.slot_device(slot, mem) else {
                        return (CC_TRB_ERROR, len as u32);
                    };
                    match dev.control(&setup, &[]) {
                        Xfer::Stall => (CC_STALL, len as u32),
                        Xfer::Ok(mut data) => {
                            data.truncate(len.min(setup.length as usize));
                            mem.write_bytes(buf, &data);
                            let residue = (len - data.len()) as u32;
                            (
                                if residue > 0 {
                                    CC_SHORT_PACKET
                                } else {
                                    CC_SUCCESS
                                },
                                residue,
                            )
                        }
                    }
                } else {
                    let data = if idt {
                        let mut v = trb[0].to_le_bytes().to_vec();
                        v.extend_from_slice(&trb[1].to_le_bytes());
                        v.truncate(len);
                        v
                    } else {
                        mem.read_bytes(buf, len)
                    };
                    ctrl.out.extend_from_slice(&data);
                    (CC_SUCCESS, 0)
                }
            }
            TRB_NORMAL => {
                let ep = dci / 2;
                let dir_in = dci % 2 == 1;
                let Some(dev) = self.slot_device(slot, mem) else {
                    return (CC_TRB_ERROR, len as u32);
                };
                if dir_in {
                    match dev.data_in(ep as u8, len) {
                        Xfer::Stall => (CC_STALL, len as u32),
                        Xfer::Ok(data) => {
                            mem.write_bytes(buf, &data);
                            let residue = (len - data.len().min(len)) as u32;
                            (
                                if residue > 0 {
                                    CC_SHORT_PACKET
                                } else {
                                    CC_SUCCESS
                                },
                                residue,
                            )
                        }
                    }
                } else {
                    let data = mem.read_bytes(buf, len);
                    match dev.data_out(ep as u8, &data) {
                        Xfer::Stall => (CC_STALL, len as u32),
                        Xfer::Ok(_) => (CC_SUCCESS, 0),
                    }
                }
            }
            TRB_EVENT_DATA | TRB_NO_OP => (CC_SUCCESS, 0),
            _ => (CC_TRB_ERROR, len as u32),
        }
    }

    /// The Status Stage: this is where an OUT control transfer is finally
    /// handed to the device, since only now are all its data bytes in hand.
    fn control_status(
        &mut self,
        slot: u32,
        setup: &Setup,
        ctrl: &mut ControlState,
        mem: &mut dyn HostMem,
    ) -> (u32, u32) {
        if setup.dir_in() {
            // The IN data already went to the device at the Data Stage.
            return (CC_SUCCESS, 0);
        }
        let out = std::mem::take(&mut ctrl.out);
        let Some(dev) = self.slot_device(slot, mem) else {
            return (CC_TRB_ERROR, 0);
        };
        match dev.control(setup, &out) {
            Xfer::Stall => (CC_STALL, 0),
            Xfer::Ok(_) => (CC_SUCCESS, 0),
        }
    }

    /// Interrupter 0 wants the host's attention: an event is pending, the
    /// interrupter is enabled, and so are interrupts as a whole. This is the
    /// level the PCI function turns into an MSI or INTA (xHCI 4.17).
    pub fn interrupt_pending(&self) -> bool {
        self.reg(IMAN) & (IMAN_IP | IMAN_IE) == IMAN_IP | IMAN_IE
            && self.reg(USBCMD) & USBCMD_INTE != 0
    }

    /// The port status words, for tests and for `RVF_DBG_XHCI`.
    pub fn portsc(&self, port: usize) -> u32 {
        self.ports[port - 1].portsc
    }

    /// `USBSTS` as the firmware reads it.
    pub fn usbsts(&self) -> u32 {
        let sticky = self.reg(USBSTS) & !USBSTS_HCH;
        if self.running {
            sticky
        } else {
            sticky | USBSTS_HCH
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::periph::usb::Hub;

    #[test]
    fn empty_ports_read_the_measured_resting_value() {
        let hc = Xhci::new();
        // Port 1 is the USB2 root port, so it also reports Device Removable.
        assert_eq!(hc.portsc(1), 0x0000_02A0 | PORTSC_DR);
        for p in 2..=PORTS {
            assert_eq!(hc.portsc(p), 0x0000_02A0, "port {p}");
        }
    }

    #[test]
    fn a_hub_on_port_1_reports_the_measured_connect_word() {
        let mut hc = Xhci::new();
        hc.attach(1, Box::new(Hub::new()));
        // `USB2[1] 400202e1 connected` in the reference log.
        assert_eq!(hc.portsc(1), 0x4002_02E1);
    }

    #[test]
    fn resetting_the_port_enables_it_at_high_speed() {
        let mut hc = Xhci::new();
        hc.attach(1, Box::new(Hub::new()));
        let mut mem = VecMem::default();
        hc.reset_port(0, &mut mem);
        // `0x40000e03` plus the reset-change bit the driver then clears.
        assert_eq!(hc.portsc(1) & !PORTSC_PRC & !PORTSC_CSC, 0x4000_0E03);
    }

    #[test]
    fn usbsts_reports_halted_until_run_stop_is_set() {
        let mut hc = Xhci::new();
        let mut mem = VecMem::default();
        assert_eq!(hc.usbsts() & USBSTS_HCH, USBSTS_HCH);
        hc.write(USBCMD, Width::Word, USBCMD_RS, &mut mem);
        assert_eq!(hc.usbsts() & USBSTS_HCH, 0);
    }

    // -----------------------------------------------------------------------
    // A whole enumeration, driven the way the firmware drives it: rings in
    // host memory, doorbells, events read back out of memory.

    const ERST: u64 = 0x2000;
    const EVENT_RING: u64 = 0x3000;
    const CMD_RING: u64 = 0x4000;
    const DCBAA: u64 = 0x5000;
    const DEV_CTX: u64 = 0x6000;
    const INPUT_CTX: u64 = 0x7000;
    const EP0_RING: u64 = 0x8000;
    const BUFFER: u64 = 0x9000;

    fn put_trb(mem: &mut VecMem, at: u64, trb: [u32; 4]) {
        for (i, w) in trb.iter().enumerate() {
            mem.write32(at + 4 * i as u64, *w);
        }
    }

    /// Bring the controller up the way a driver does: event ring, command
    /// ring, DCBAA, Run/Stop.
    fn started() -> (Xhci, VecMem) {
        let mut hc = Xhci::new();
        hc.attach(1, Box::new(Hub::new()));
        let mut mem = VecMem::default();
        // One event ring segment, 16 TRBs.
        mem.write32(ERST, EVENT_RING as u32);
        mem.write32(ERST + 4, 0);
        mem.write32(ERST + 8, 16);
        let w = |hc: &mut Xhci, mem: &mut VecMem, off: u32, v: u32| {
            hc.write(off, Width::Word, v, mem);
        };
        w(&mut hc, &mut mem, ERSTSZ, 1);
        w(&mut hc, &mut mem, ERDP_LO, EVENT_RING as u32);
        w(&mut hc, &mut mem, ERSTBA_LO, ERST as u32);
        w(&mut hc, &mut mem, DCBAAP_LO, DCBAA as u32);
        w(&mut hc, &mut mem, CRCR_LO, CMD_RING as u32 | 1);
        w(&mut hc, &mut mem, USBCMD, USBCMD_RS);
        (hc, mem)
    }

    /// Place a command TRB at command-ring slot `cmd`, ring doorbell 0, and
    /// return the completion event that landed at event-ring slot `ev`.
    fn command(hc: &mut Xhci, mem: &mut VecMem, cmd: u64, ev: u64, trb: [u32; 4]) -> [u32; 4] {
        put_trb(mem, CMD_RING + 16 * cmd, trb);
        hc.write(DBOFF, Width::Word, 0, mem);
        [0, 1, 2, 3].map(|i| mem.read32(EVENT_RING + 16 * ev + 4 * i))
    }

    /// Enable Slot, Address Device, then `GET_DESCRIPTOR(DEVICE)` over the
    /// control ring — the sequence that produces the bootloader's
    /// `DEV [01:00] 2.16 000000:01 class 9 VID 2109 PID 3431`.
    #[test]
    fn enumerating_the_hub_over_the_rings() {
        let (mut hc, mut mem) = started();

        // The port has to be reset before the driver addresses anything.
        hc.write(PORTSC, Width::Word, PORTSC_PR | PORTSC_PP, &mut mem);
        assert_eq!(hc.portsc(1) & PORTSC_PED, PORTSC_PED, "port enabled");
        // The reset posted a Port Status Change Event for port 1.
        let ev = [0, 1, 2, 3].map(|i| mem.read32(EVENT_RING + 4 * i));
        assert_eq!((ev[3] >> 10) & 0x3F, TRB_PORT_STATUS_CHANGE);
        assert_eq!(ev[0] >> 24, 1);
        assert_eq!(hc.usbsts() & USBSTS_PCD, USBSTS_PCD);

        // Enable Slot.
        let ev = command(
            &mut hc,
            &mut mem,
            0,
            1,
            [0, 0, 0, (TRB_ENABLE_SLOT << 10) | 1],
        );
        assert_eq!((ev[3] >> 10) & 0x3F, TRB_COMMAND_COMPLETION);
        assert_eq!(ev[2] >> 24, CC_SUCCESS);
        let slot = ev[3] >> 24;
        assert_eq!(slot, 1);

        // Address Device: DCBAA entry, then an input context naming root port
        // 1 and an endpoint-0 transfer ring.
        mem.write32(DCBAA + 8 * slot as u64, DEV_CTX as u32);
        mem.write32(INPUT_CTX + 4, 0x3); // add slot context + endpoint 0
        mem.write32(INPUT_CTX + 0x20, 1 << 27); // context entries 1, route 0
        mem.write32(INPUT_CTX + 0x24, 1 << 16); // root hub port 1
        mem.write32(INPUT_CTX + 0x48, EP0_RING as u32 | 1); // TR dequeue + DCS
        let ev = command(
            &mut hc,
            &mut mem,
            1,
            2,
            [
                INPUT_CTX as u32,
                0,
                0,
                (slot << 24) | (TRB_ADDRESS_DEVICE << 10) | 1,
            ],
        );
        assert_eq!(ev[2] >> 24, CC_SUCCESS);
        // Slot Context: speed High, state Addressed, address = slot id.
        assert_eq!((mem.read32(DEV_CTX) >> 20) & 0xF, Speed::High as u32);
        assert_eq!(mem.read32(DEV_CTX + 12) & 0xFF, slot);
        assert_eq!(mem.read32(DEV_CTX + 12) >> 27, 2);

        // GET_DESCRIPTOR(DEVICE, 18) as a three-stage control transfer.
        let setup = [0x80u8, 6, 0, 1, 0, 0, 18, 0];
        put_trb(
            &mut mem,
            EP0_RING,
            [
                u32::from_le_bytes([setup[0], setup[1], setup[2], setup[3]]),
                u32::from_le_bytes([setup[4], setup[5], setup[6], setup[7]]),
                8,
                (3 << 16) | (TRB_SETUP << 10) | (1 << 6) | 1,
            ],
        );
        put_trb(
            &mut mem,
            EP0_RING + 16,
            [BUFFER as u32, 0, 18, (1 << 16) | (TRB_DATA << 10) | 1],
        );
        put_trb(
            &mut mem,
            EP0_RING + 32,
            [0, 0, 0, (TRB_STATUS << 10) | (1 << 5) | 1],
        );
        hc.write(DBOFF + 4, Width::Word, 1, &mut mem);

        let got = mem.read_bytes(BUFFER, 18);
        assert_eq!(
            got,
            vec![
                0x12, 0x01, 0x10, 0x02, 0x09, 0x00, 0x01, 0x40, 0x09, 0x21, 0x31, 0x34, 0x21, 0x04,
                0x00, 0x01, 0x00, 0x01
            ],
            "the hub's device descriptor, byte for byte off rpi-dev"
        );
        // The Status Stage asked for an interrupt, so a Transfer Event landed
        // on the event ring naming slot 1, endpoint 0.
        let ev = [0, 1, 2, 3].map(|i| mem.read32(EVENT_RING + 48 + 4 * i));
        assert_eq!((ev[3] >> 10) & 0x3F, TRB_TRANSFER_EVENT);
        assert_eq!(ev[3] >> 24, slot);
        assert_eq!((ev[3] >> 16) & 0x1F, 1, "endpoint 0");
        assert_eq!(ev[2] >> 24, CC_SUCCESS);
        // The endpoint's dequeue pointer moved past all three stages.
        assert_eq!(mem.read32(DEV_CTX + 0x28) & !0xF, EP0_RING as u32 + 48);
    }

    /// A short IN transfer reports `Short Packet` with the residue, which is
    /// how the driver learns a descriptor was smaller than its buffer.
    #[test]
    fn a_short_in_transfer_reports_its_residue() {
        let (mut hc, mut mem) = started();
        hc.write(PORTSC, Width::Word, PORTSC_PR, &mut mem);
        command(
            &mut hc,
            &mut mem,
            0,
            1,
            [0, 0, 0, (TRB_ENABLE_SLOT << 10) | 1],
        );
        mem.write32(DCBAA + 8, DEV_CTX as u32);
        mem.write32(INPUT_CTX + 4, 0x3);
        mem.write32(INPUT_CTX + 0x24, 1 << 16);
        mem.write32(INPUT_CTX + 0x48, EP0_RING as u32 | 1);
        command(
            &mut hc,
            &mut mem,
            1,
            2,
            [
                INPUT_CTX as u32,
                0,
                0,
                (1 << 24) | (TRB_ADDRESS_DEVICE << 10) | 1,
            ],
        );
        // Ask for 64 bytes of an 18-byte descriptor.
        let setup = [0x80u8, 6, 0, 1, 0, 0, 64, 0];
        put_trb(
            &mut mem,
            EP0_RING,
            [
                u32::from_le_bytes([setup[0], setup[1], setup[2], setup[3]]),
                u32::from_le_bytes([setup[4], setup[5], setup[6], setup[7]]),
                8,
                (3 << 16) | (TRB_SETUP << 10) | (1 << 6) | 1,
            ],
        );
        put_trb(
            &mut mem,
            EP0_RING + 16,
            [
                BUFFER as u32,
                0,
                64,
                (1 << 16) | (TRB_DATA << 10) | (1 << 5) | 1,
            ],
        );
        hc.write(DBOFF + 4, Width::Word, 1, &mut mem);
        let ev = [0, 1, 2, 3].map(|i| mem.read32(EVENT_RING + 48 + 4 * i));
        assert_eq!(ev[2] >> 24, CC_SHORT_PACKET);
        assert_eq!(ev[2] & 0x00FF_FFFF, 64 - 18, "residue");
    }

    /// The capability block is what `xHC0 ver: … HCS: … HCC: …` prints, and it
    /// has to stay byte-identical to the values read off `rpi-dev`.
    #[test]
    fn capability_registers_match_rpi_dev() {
        let mut hc = Xhci::new();
        assert_eq!(hc.read(0x00, Width::Word), 0x0100_0020);
        assert_eq!(hc.read(0x04, Width::Word), 0x0500_0420);
        assert_eq!(hc.read(0x08, Width::Word), 0xFC00_0031);
        assert_eq!(hc.read(0x0C, Width::Word), 0x00E7_0004);
        assert_eq!(hc.read(0x10, Width::Word), 0x0028_41EB);
        assert_eq!(hc.read(0x14, Width::Word), 0x0000_0100);
        assert_eq!(hc.read(0x18, Width::Word), 0x0000_0200);
    }
}
