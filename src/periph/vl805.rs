//! The VIA VL805/806 xHCI USB 3.0 controller (`1106:3483`) — the one PCIe
//! endpoint on a Pi 4B, sitting on bus 1, device 0, function 0 behind the
//! BCM2711 root complex modelled in [`super::pcie`].
//!
//! This is the *identity* of the device and nothing more: its configuration
//! space, plus the vendor-specific indirect port the firmware uploads the
//! VL805's own hub/MCU firmware through. There is deliberately no xHCI register
//! block, no ring engine and no USB device behind the root hub — that is
//! stage 3 of [`docs/usb-xhci.md`](../../../docs/usb-xhci.md).
//!
//! What this is enough for is the config-space half of the conversation:
//!
//! * the second-stage EEPROM bootloader's bus scan, which walks
//!   `EXT_CFG_INDEX`/`EXT_CFG_DATA` reading vendor/device/class and prints
//!   `PCIe scan %08x:%08x` (`0x000A7118`) for each function that answers;
//! * `start4.elf`'s `XHCI_RESET` (`0x3EDC61F4`), which reads the vendor and
//!   device halfwords at `0x7D50_8000`/`0x7D50_8002` and refuses anything that
//!   is not `1106:3483`.
//!
//! ## Ground truth
//!
//! Every config-space byte below was read off a real Pi 4B (`ssh rpi-dev`):
//!
//! ```text
//! $ sudo od -Ax -tx1 -v /sys/bus/pci/devices/0000:01:00.0/config
//! 000000 06 11 83 34 46 05 10 00 01 30 03 0c 10 00 00 00
//! 000010 04 00 00 c0 00 00 00 00 00 00 00 00 00 00 00 00
//! 000020 00 00 00 00 00 00 00 00 00 00 00 00 06 11 83 34
//! 000030 00 00 00 00 80 00 00 00 00 00 00 00 1b 01 00 00
//! 000040 00 00 00 00 00 01 00 00 09 00 80 0e 04 00 00 00
//! 000050 c0 38 01 00 00 00 00 00 00 00 00 00 06 11 83 34
//! 000060 30 20 00 00 00 00 00 00 00 00 00 00 00 00 00 00
//! 000070 00 00 00 00 00 00 00 00 08 00 03 00 01 00 00 18
//! 000080 01 90 c3 89 00 00 00 00 00 00 00 00 00 00 00 00
//! 000090 05 c4 a5 00 fc ff ff ff 00 00 00 00 40 65 00 00
//! ...
//! 0000c0 00 20 00 00 10 00 02 00 01 80 00 00 1f 28 19 00
//! 0000d0 12 5c 06 00 43 01 12 10 00 00 00 00 00 00 00 00
//! 000100 01 00 01 00 00 00 00 00 00 00 00 00 31 20 06 00
//! ```
//!
//! That dump is a *running* device: Linux had already enabled the command
//! register (`0x04` = `0x0546`), assigned BAR0 (`0x10` = `0xC000_0004`) and
//! programmed the MSI address/data. The seed table below restores those to
//! their power-on values — command `0x0000`, BAR0 `0x0000_0004`, MSI address
//! and data zero — and keeps everything else verbatim. `lspci -vvv` confirms
//! the shape: `Region 0: Memory at 600000000 (64-bit, non-prefetchable)
//! [size=4K]`, capabilities PM at `0x80`, MSI at `0x90`, PCIe at `0xC4`, AER at
//! `0x100`.

use std::collections::BTreeMap;

use crate::bus::Width;

/// `lspci`: `Region 0: … [size=4K]`.
pub const BAR0_SIZE: u32 = 0x1000;

/// A PCIe function's configuration space is 4 KiB; only the first 0x200 bytes
/// are non-zero on this part.
const CFG_LEN: usize = 0x1000;

/// The measured config space, with the fields Linux had already written put
/// back to their power-on values (see the module docs).
const CFG_SEED: &[(usize, &[u8])] = &[
    // 0x00: vendor 1106, device 3483, command 0000 (power-on), status 0010,
    // rev 01, class 0c0330 (xHCI), cache line 0x10.
    (
        0x00,
        &[
            0x06, 0x11, 0x83, 0x34, 0x00, 0x00, 0x10, 0x00, 0x01, 0x30, 0x03, 0x0c, 0x10, 0x00,
            0x00, 0x00,
        ],
    ),
    // 0x10: BAR0, 64-bit memory, not yet assigned. 0x14 is its upper half.
    (0x10, &[0x04, 0x00, 0x00, 0x00]),
    // 0x2c: subsystem vendor/device = 1106:3483.
    (0x2c, &[0x06, 0x11, 0x83, 0x34]),
    // 0x34: capabilities pointer -> 0x80. 0x3c: interrupt line 0x1b, pin A.
    (0x34, &[0x80]),
    (0x3c, &[0x1b, 0x01, 0x00, 0x00]),
    // 0x44..0x5f: vendor-defined, verbatim from the dump.
    (
        0x44,
        &[
            0x00, 0x01, 0x00, 0x00, 0x09, 0x00, 0x80, 0x0e, 0x04, 0x00, 0x00, 0x00, 0xc0, 0x38,
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x11, 0x83, 0x34,
        ],
    ),
    (0x60, &[0x30, 0x20]),
    // 0x78/0x7c: the vendor indirect index/data port (see `Vl805::cfg_write`).
    // These are the values the port happened to hold on the running board.
    (0x78, &[0x08, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x18]),
    // 0x80: PCI Power Management capability, next -> 0x90.
    (0x80, &[0x01, 0x90, 0xc3, 0x89]),
    // 0x90: MSI capability, 4 vectors, 64-bit, next -> 0xc4. The address and
    // data words were programmed by Linux; power-on they are zero.
    (0x90, &[0x05, 0xc4, 0x80, 0x00]),
    // 0xc4: PCI Express capability (v2 endpoint), verbatim apart from the
    // control words Linux wrote.
    (
        0xc4,
        &[
            0x10, 0x00, 0x02, 0x00, 0x01, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1f, 0x28,
            0x19, 0x00, 0x00, 0x00, 0x00, 0x00, 0x43, 0x01, 0x12, 0x10,
        ],
    ),
    (0xe8, &[0x12]),
    (0xf4, &[0x22, 0x00, 0x01, 0x00]),
    // 0x100: AER extended capability.
    (0x100, &[0x01, 0x00, 0x01, 0x00]),
    (0x10c, &[0x31, 0x20, 0x06, 0x00]),
    (0x114, &[0x00, 0x20, 0x00, 0x00]),
];

/// Which bits of each config-space word the firmware may change. Everything not
/// listed is read-only, which is what makes BAR sizing work: writing all-ones
/// to `0x10` has to read back the size mask, not the value written.
fn cfg_write_mask(off: usize) -> u32 {
    match off {
        // Cache line size / latency timer.
        0x0c => 0x0000_FFFF,
        // Command register (low half). Status (high half) is write-1-to-clear
        // on real silicon; nothing in the firmware reads it back, so it is
        // modelled read-only.
        0x04 => 0x0000_0547,
        // BAR0: 4 KiB, 64-bit memory. Bits [3:0] are the hard-wired type.
        0x10 => !(BAR0_SIZE - 1),
        // BAR0 upper half.
        0x14 => 0xFFFF_FFFF,
        // Interrupt line.
        0x3c => 0x0000_00FF,
        // The vendor index/data port.
        0x78 | 0x7c => 0xFFFF_FFFF,
        // PM control/status.
        0x84 => 0xFFFF_FFFF,
        // MSI control (the enable/count field), address, data.
        0x90 => 0x00FF_0000,
        0x94 | 0x98 | 0x9c => 0xFFFF_FFFF,
        // PCIe device control / link control / device control 2 / link ctl 2.
        0xc8 | 0xd0 | 0xe8 | 0xf0 => 0x0000_FFFF,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// The xHCI register block behind BAR0. Every capability value here was read
// off `rpi-dev`'s VL805 through `/dev/mem` (docs/usb-xhci.md §2); `dmesg`'s
// `hcc params 0x002841eb hci version 0x100` cross-checks the pair that matters.

/// `CAPLENGTH` — where the operational registers start.
pub const CAPLENGTH: u32 = 0x20;
/// `MaxSlots` 32, `MaxIntrs` 4, `MaxPorts` 5.
const HCSPARAMS1: u32 = 0x0500_0420;
/// `IST` 1, `ERSTMax` 3, `MaxScratchpad` 31.
const HCSPARAMS2: u32 = 0xFC00_0031;
const HCSPARAMS3: u32 = 0x00E7_0004;
/// `AC64` 1, `CSZ` 0, `PPC` 1, `xECP` 0x28 (extended caps at BAR0 + 0xA0).
const HCCPARAMS1: u32 = 0x0028_41EB;
const DBOFF: u32 = 0x0000_0100;
const RTSOFF: u32 = 0x0000_0200;

/// Operational register offsets, relative to `CAPLENGTH`.
const OP_USBCMD: u32 = 0x00;
const OP_USBSTS: u32 = 0x04;
const OP_PAGESIZE: u32 = 0x08;
/// `PORTSC` for port *n* is at `CAPLENGTH + 0x400 + (n - 1) * 0x10`.
const OP_PORTSC: u32 = 0x400;
/// The VL805 has five root ports: one USB2, four USB3.
const PORTS: u32 = 5;

const USBCMD_RS: u32 = 1 << 0;
const USBCMD_HCRST: u32 = 1 << 1;
const USBCMD_LHCRST: u32 = 1 << 7;
const USBSTS_HCH: u32 = 1 << 0;
/// `HSE`, `EINT`, `PCD`, `SRE` — the write-1-to-clear half of `USBSTS`.
/// Nothing in the model ever sets them: there are no events and no port
/// changes, so the register only ever reports `HCHalted`.
const USBSTS_RW1C: u32 = (1 << 2) | (1 << 3) | (1 << 4) | (1 << 10);

/// `CCS=0 PED=0 PLS=5 (RxDetect) PP=1` — powered, nothing attached. Measured on
/// the two VL805 ports the Pi 4B routes nowhere, which are observationally
/// identical to an empty socket.
const PORTSC_EMPTY: u32 = 0x0000_02A0;

/// The `PORTSC` word index, if `off` is one.
fn portsc_index(off: u32) -> Option<u32> {
    let base = CAPLENGTH + OP_PORTSC;
    if off < base || off >= base + PORTS * 0x10 || (off - base) % 0x10 != 0 {
        return None;
    }
    Some((off - base) / 0x10)
}

/// Registers the firmware may not change: the port status words, which in this
/// model only ever say "powered, empty".
fn is_xhci_ro(off: u32) -> bool {
    portsc_index(off).is_some()
}

/// The VL805 endpoint's configuration space.
pub struct Vl805 {
    cfg: Vec<u8>,
    /// Sticky storage for the register file reached through the indirect
    /// index/data port at config `0x78`/`0x7C`. Both the EEPROM bootloader
    /// (around `0x000B6CE0`) and start4 (`0x3EDC5EF8`, `MCU FW: %x %x`) stream
    /// the VL805's firmware image through it and then read it back to verify —
    /// the bootloader byte by byte, bailing out with `HUB2.0 fail`
    /// (`0x000B6DF8`) on the first mismatch. Nothing here interprets the
    /// contents; storing and returning them is the whole job.
    vendor_regs: BTreeMap<u32, u32>,
    /// How many words have been pushed through the data port. Only an
    /// observable for tests.
    pub vendor_writes: u64,
    /// The writable half of the xHCI register block behind BAR0: everything the
    /// firmware sets and reads back (`USBCMD`, `CRCR`, `DCBAAP`, `CONFIG`, the
    /// interrupter register set, the doorbells). The capability registers are
    /// constants — see [`Vl805::bar0_read`].
    xhci: BTreeMap<u32, u32>,
}

impl Default for Vl805 {
    fn default() -> Self {
        Vl805::new()
    }
}

impl Vl805 {
    pub fn new() -> Vl805 {
        let mut cfg = vec![0u8; CFG_LEN];
        for (off, bytes) in CFG_SEED {
            cfg[*off..*off + bytes.len()].copy_from_slice(bytes);
        }
        Vl805 {
            cfg,
            vendor_regs: BTreeMap::new(),
            vendor_writes: 0,
            xhci: BTreeMap::new(),
        }
    }

    fn cfg_word(&self, off: usize) -> u32 {
        u32::from_le_bytes([
            self.cfg[off],
            self.cfg[off + 1],
            self.cfg[off + 2],
            self.cfg[off + 3],
        ])
    }

    fn set_cfg_word(&mut self, off: usize, v: u32) {
        self.cfg[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// Read `width` bytes of configuration space at `off`.
    pub fn cfg_read(&mut self, off: u32, width: Width) -> u32 {
        let off = off as usize & (CFG_LEN - 1);
        // The data half of the vendor port reads back whatever the currently
        // selected index holds.
        if off & !3 == 0x7c {
            let idx = self.cfg_word(0x78);
            let word = self.vendor_regs.get(&idx).copied().unwrap_or(0);
            self.set_cfg_word(0x7c, word);
        }
        let mut v = 0u32;
        for i in 0..width.bytes() as usize {
            v |= (*self.cfg.get(off + i).unwrap_or(&0) as u32) << (8 * i);
        }
        v
    }

    /// Write `width` bytes of configuration space at `off`.
    pub fn cfg_write(&mut self, off: u32, width: Width, value: u32) {
        let off = off as usize & (CFG_LEN - 1);
        let word_off = off & !3;
        let shift = 8 * (off & 3);
        let byte_mask: u32 = match width {
            Width::Byte => 0xFF,
            Width::Half => 0xFFFF,
            Width::Word => 0xFFFF_FFFF,
        };
        let mask = cfg_write_mask(word_off) & (byte_mask << shift);
        if mask == 0 {
            return;
        }
        let old = self.cfg_word(word_off);
        let new = (old & !mask) | ((value << shift) & mask);
        self.set_cfg_word(word_off, new);

        if word_off == 0x7c {
            let idx = self.cfg_word(0x78);
            self.vendor_regs.insert(idx, new);
            self.vendor_writes += 1;
        }
    }

    /// Read `width` bytes of the xHCI register block behind BAR0.
    ///
    /// The capability registers are the values measured on `rpi-dev` (see the
    /// module docs of [`super::pcie`] and `docs/usb-xhci.md` §2); they are
    /// read-only constants on real silicon too. Everything else is sticky
    /// storage, apart from the two bits the firmware polls for progress:
    /// `USBSTS.HCH` follows `USBCMD.RS`, and `USBCMD.HCRST` self-clears.
    pub fn bar0_read(&mut self, off: u32, width: Width) -> u32 {
        let word = self.xhci_word(off & !3);
        let shift = 8 * (off & 3);
        let mask: u32 = match width {
            Width::Byte => 0xFF,
            Width::Half => 0xFFFF,
            Width::Word => 0xFFFF_FFFF,
        };
        (word >> shift) & mask
    }

    /// Write `width` bytes of the xHCI register block behind BAR0.
    pub fn bar0_write(&mut self, off: u32, width: Width, value: u32) {
        let word_off = off & !3;
        if word_off < CAPLENGTH || is_xhci_ro(word_off) {
            return; // capability registers and PORTSC are read-only here
        }
        let shift = 8 * (off & 3);
        let mask: u32 = match width {
            Width::Byte => 0xFF << shift,
            Width::Half => 0xFFFF << shift,
            Width::Word => 0xFFFF_FFFF,
        };
        let old = self.xhci.get(&word_off).copied().unwrap_or(0);
        let mut new = (old & !mask) | ((value << shift) & mask);
        if word_off == CAPLENGTH + OP_USBSTS {
            // Write-1-to-clear, not a plain register: the bring-up's stop path
            // writes all-ones to it. Without this the firmware reads its own
            // `0xFFFF_FFFF` back and prints `USBSTS fffffffe` where a real
            // board prints `USBSTS 18`.
            self.xhci.insert(word_off, old & !(new & USBSTS_RW1C));
            return;
        }
        if word_off == CAPLENGTH + OP_USBCMD {
            // HCRST (bit 1) and LHCRST (bit 7) complete before the firmware can
            // read them back, so they always read 0. A reset also returns the
            // operational registers to their power-on values.
            if new & (USBCMD_HCRST | USBCMD_LHCRST) != 0 {
                new &= !(USBCMD_HCRST | USBCMD_LHCRST | USBCMD_RS);
                self.xhci.clear();
            }
        }
        self.xhci.insert(word_off, new);
    }

    /// The register block as the firmware sees it: capability constants first,
    /// then the sticky operational/runtime storage.
    fn xhci_word(&self, off: u32) -> u32 {
        match off {
            // CAPLENGTH 0x20 | HCIVERSION 0x0100.
            0x00 => 0x0100_0020,
            0x04 => HCSPARAMS1,
            0x08 => HCSPARAMS2,
            0x0C => HCSPARAMS3,
            0x10 => HCCPARAMS1,
            0x14 => DBOFF,
            0x18 => RTSOFF,
            0x1C => 0,
            // Extended capabilities, walked from `HCCPARAMS1.xECP` = 0xA0.
            0xA0 => 0x0000_0401,             // legacy support, next -> 0xB0
            0xB0 => 0x0200_0802,             // supported protocol, USB 2.0
            0xB4 | 0xD4 => 0x2042_5355,      // "USB "
            0xB8 => 0x0000_0101,             // port offset 1, count 1
            0xD0 => 0x0300_8C02,             // supported protocol, USB 3.0
            0xD8 => 0x0000_0402,             // port offset 2, count 4
            0x300 => 0x0000_000A,
            _ => {
                if off == CAPLENGTH + OP_PAGESIZE {
                    return 1; // 4 KiB pages
                }
                if off == CAPLENGTH + OP_USBSTS {
                    // HCHalted mirrors "not running"; the controller is never
                    // "not ready" (CNR) in the model.
                    let run = self.xhci.get(&(CAPLENGTH + OP_USBCMD)).copied().unwrap_or(0)
                        & USBCMD_RS;
                    let sticky = self.xhci.get(&off).copied().unwrap_or(0) & !USBSTS_HCH;
                    return if run != 0 { sticky } else { sticky | USBSTS_HCH };
                }
                if let Some(port) = portsc_index(off) {
                    let _ = port;
                    // Powered, nothing attached: the resting value every
                    // unpopulated VL805 port reads (docs/usb-xhci.md §5.2).
                    return PORTSC_EMPTY;
                }
                self.xhci.get(&off).copied().unwrap_or(0)
            }
        }
    }

    /// Where BAR0 currently decodes on the PCI bus, if the firmware has both
    /// assigned it and enabled memory decoding. `None` = the window is off.
    ///
    /// The second-stage bootloader assigns `0x8200_0000` here and then writes
    /// `0x0146` to the command register — the same command word `lspci` reports
    /// on the running board.
    pub fn bar0_bus_addr(&self) -> Option<u64> {
        if self.cfg_word(0x04) & 0x2 == 0 {
            return None; // memory space decoding disabled
        }
        let addr = ((self.cfg_word(0x14) as u64) << 32) | (self.cfg_word(0x10) & !0xF) as u64;
        if addr == 0 {
            None
        } else {
            Some(addr)
        }
    }
}
