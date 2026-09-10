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
