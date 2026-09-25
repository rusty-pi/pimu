//! The CYW43455's own side of the SDIO bus: the backplane behind function 1,
//! the cores hanging off it, and the ARM's SRAM.
//!
//! [`SdCard`](super::sdcard::SdCard) plays the chip's SDIO *card* — CCCR, FBRs
//! and CIS, which are the standard's. Everything past that is here: function
//! 1's registers, the 32 KiB window they aim at the chip's internal bus, and
//! what answers there. Function 2 is the chip's frame FIFO, answered by
//! [`Sdpcm`](super::sdpcm::Sdpcm).
//!
//! `brcmfmac` is the specification: it reads the chipcommon `chipid`, walks the
//! **EROM** for the cores it insists on (a CPU, an SDIO device core, a
//! chipcommon core), sizes the ARM CR4's tightly coupled memory out of that
//! core's own registers, and drives each core's wrapper to hold and release the
//! ARM. Line references below are to raspberrypi/linux
//! `16f1da3c4e94437449d6aa151589ca0ad4b388bb`, under
//! `drivers/net/wireless/broadcom/brcm80211/brcmfmac/`.
//!
//! **The chip's own bus addresses are the model's to choose**, because the
//! driver learns every one of them from the EROM this module writes. Only the
//! values it checks against its own tables — chip id, revision, core ids, RAM
//! base — are forced, and each is cited where it is defined.
//!
//! The ARM comes out of reset and executes nothing; what a running firmware
//! would say is [`Sdpcm`](super::sdpcm::Sdpcm)'s job, and the SDIO device core's
//! mailbox registers are how the chip announces a frame.

use std::collections::BTreeMap;

use crate::periph::sdpcm::Sdpcm;

const F1_MISC_START: u32 = 0x1_0000;
const F1_MISC_LIMIT: u32 = 0x1_001F;
/// The window base, byte by byte (`sdio.h:75`).
const F1_SBADDRLOW: u32 = 0x1_000A;
const F1_SBADDRMID: u32 = 0x1_000B;
const F1_SBADDRHIGH: u32 = 0x1_000C;
const F1_CHIPCLKCSR: u32 = 0x1_000E;
const F1_SLEEPCSR: u32 = 0x1_001F;

/// `CHIPCLKCSR` request bits and the two availability bits (`sdio.c:180`).
const CSR_FORCE_ALP: u8 = 0x01;
const CSR_FORCE_HT: u8 = 0x02;
const CSR_ALP_AVAIL_REQ: u8 = 0x08;
const CSR_HT_AVAIL_REQ: u8 = 0x10;
const CSR_ALP_AVAIL: u8 = 0x40;
const CSR_HT_AVAIL: u8 = 0x80;

/// `SLEEPCSR`: the host sets KSO and waits for KSO and DEVON both (`sdio.h:110`).
const SLEEPCSR_KSO: u8 = 0x01;
const SLEEPCSR_DEVON: u8 = 0x02;

/// The offset a function-1 address selects inside the window; bit 15 above it
/// asks for 4-byte accesses and does not move the address (`sdio.h:122`).
const SB_OFT_ADDR_MASK: u32 = 0x07FFF;
const SBWINDOW_MASK: u32 = 0xFFFF_8000;

/// Where the driver expects chipcommon, and the chip-id read that starts
/// everything (`soc.h:9`).
pub const ENUM_BASE: u32 = 0x1800_0000;

/// Core ids (`bcma.h`); the driver looks each up by id.
const CORE_CHIPCOMMON: u16 = 0x800;
const CORE_80211: u16 = 0x812;
const CORE_SDIO_DEV: u16 = 0x829;
const CORE_ARM_CR4: u16 = 0x83E;
const MANUF_BCM: u32 = 0x4BF;

/// `BRCM_CC_4345_CHIP_ID` at revision 6: the pair the driver's firmware table
/// maps to `brcmfmac43455-sdio`, the blob the card carries (`sdio.c:665`).
const CHIP_ID: u32 = 0x4345;
const CHIP_REV: u32 = 6;
/// `SOCI_AI`: the only backplane type the driver walks here (`chip.c:22`).
const SOCI_AI: u32 = 1;

/// Where the enumeration table sits; the driver only gets here via `eromptr`.
const EROM_BASE: u32 = 0x1801_0000;

const CC_CHIPID: u32 = 0x00;
const CC_EROMPTR: u32 = 0xFC;

/// SDIO device core registers, `struct sdpcmd_regs` (`sdio.h:230`): the chip's
/// side of the host interrupt. The driver reads `intstatus`, writes back what it
/// saw, and takes the mailbox word out of `tohostmailboxdata`.
const SD_INTSTATUS: u32 = 0x20;
const SD_HOSTINTMASK: u32 = 0x24;
const SD_TOSBMAILBOX: u32 = 0x40;
const SD_TOHOSTMAILBOXDATA: u32 = 0x4C;

/// `intstatus` bits (`sdio.c:204`): a frame waiting on function 2, and the
/// mailbox holding a word.
const I_HMB_FRAME_IND: u32 = 1 << 6;
const I_HMB_HOST_INT: u32 = 1 << 7;

/// Written to `tosbmailbox` once the host has read the mailbox word.
const SMB_INT_ACK: u32 = 1 << 1;

/// What the firmware puts in `tohostmailboxdata` when it is up: ready, plus the
/// protocol version it speaks, which the driver compares against its own.
const HMB_DATA_FWREADY: u32 = 0x0008;
const SDPCM_PROT_VERSION: u32 = 4;
const HMB_DATA_VERSION_SHIFT: u32 = 16;

/// `struct sdpcm_shared_le` (`sdio.c:389`): the block a running firmware leaves
/// in memory, with its address in the very top word. The driver reads it to
/// decide whether the chip is alive.
const SHARED_LEN: u32 = 7 * 4 + 32 + 4;
/// The shared-block version (`sdio.c:299`); the driver refuses anything newer
/// than its own.
const SDPCM_SHARED_VERSION: u32 = 0x0003;

/// ARM CR4 register offsets and the bank-size arithmetic (`chip.c:205`).
const ARMCR4_CAP: u32 = 0x04;
const ARMCR4_BANKIDX: u32 = 0x40;
const ARMCR4_BANKINFO: u32 = 0x44;
const ARMCR4_BSZ_MULT: u32 = 8192;

/// A core wrapper's registers (`bcma_regs.h`): how a core is held and released.
const BCMA_IOCTL: u32 = 0x408;
const BCMA_RESET_CTL: u32 = 0x800;
const BCMA_IOCTL_CLK: u32 = 0x0001;
const BCMA_IOCTL_FGC: u32 = 0x0002;
const BCMA_RESET_CTL_RESET: u32 = 0x0001;
/// Set while the ARM is parked, cleared once the firmware is in RAM.
const IOCTL_CPUHALT: u32 = 0x0020;

/// Where the ARM's memory starts on a 4345, per the driver's table
/// (`chip.c:710`).
pub const RAM_BASE: u32 = 0x0019_8000;
/// How much of it there is — the model's choice, since the driver asks the CR4
/// core rather than a table. It has to hold the card's ~595 KiB image *and* the
/// nvram above it, and stay under the 4 MiB the driver refuses: six banks of
/// 128 KiB does both.
pub const RAM_SIZE: u32 = RAM_BANKS * (RAM_BANK_INFO + 1) * ARMCR4_BSZ_MULT;
const RAM_BANKS: u32 = 6;
const RAM_BANK_INFO: u32 = 0xF;

struct Core {
    id: u16,
    rev: u32,
    base: u32,
    wrap: u32,
}

/// The cores this chip enumerates. **Chipcommon has to come first**: the driver
/// takes `list_first_entry` for it (`chip.c:1225`). The SDIO core's revision is
/// 12 because the driver only takes its modern paths — keep-sdio-on and
/// `bus:txglomalign` — from rev 12.
const CORES: [Core; 4] = [
    Core {
        id: CORE_CHIPCOMMON,
        rev: 48,
        base: ENUM_BASE,
        wrap: 0x1810_0000,
    },
    Core {
        id: CORE_SDIO_DEV,
        rev: 12,
        base: 0x1800_1000,
        wrap: 0x1810_1000,
    },
    Core {
        id: CORE_ARM_CR4,
        rev: 1,
        base: 0x1800_2000,
        wrap: 0x1810_2000,
    },
    Core {
        id: CORE_80211,
        rev: 1,
        base: 0x1800_3000,
        wrap: 0x1810_3000,
    },
];

const ARM_CORE: usize = 2;
/// The SDIO device core, whose base carries the host interrupt registers.
const SDIO_CORE: usize = 1;

/// EROM descriptor types and fields (`chip.c:24`).
const DMP_DESC_COMPONENT: u32 = 0x1;
const DMP_DESC_ADDRESS: u32 = 0x5;
const DMP_DESC_EOT: u32 = 0xF;
/// An address descriptor's port type: a plain slave, or its wrapper.
const DMP_SLAVE_TYPE_SLAVE: u32 = 0;
const DMP_SLAVE_TYPE_SWRAP: u32 = 2;

pub struct Cyw43455 {
    f1: [u8; 0x20],
    window: u32,
    /// Backplane registers, sparse and 32 bits each. Reads of what is not here
    /// are 0 — never `0xffffffff`, which the driver takes for a dead bus.
    regs: BTreeMap<u32, u32>,
    ram: Vec<u8>,
    erom: Vec<u32>,
    sdpcm: Sdpcm,
    /// `intstatus`, write-one-to-clear. Every bit is a latch, the frame
    /// indication included: once per frame queued, not while one is waiting.
    intstatus: u32,
    mailbox: u32,
}

impl Default for Cyw43455 {
    fn default() -> Self {
        Cyw43455::new()
    }
}

impl Cyw43455 {
    pub fn new() -> Cyw43455 {
        Cyw43455 {
            f1: [0; 0x20],
            window: 0,
            regs: BTreeMap::new(),
            ram: vec![0; RAM_SIZE as usize],
            erom: erom(),
            sdpcm: Sdpcm::new(),
            intstatus: 0,
            mailbox: 0,
        }
    }

    pub fn window(&self) -> u32 {
        self.window
    }

    pub fn backplane_addr(&self, offset: u32) -> u32 {
        self.window | (offset & SB_OFT_ADDR_MASK)
    }

    /// The ARM is out of reset and not halted: the last thing the driver does
    /// to start the firmware.
    pub fn arm_running(&self) -> bool {
        let wrap = CORES[ARM_CORE].wrap;
        let ioctl = self.reg(wrap + BCMA_IOCTL);
        let reset = self.reg(wrap + BCMA_RESET_CTL);
        // What `brcmf_chip_ai_iscoreup` asks, plus the ARM's own halt bit.
        ioctl & (BCMA_IOCTL_FGC | BCMA_IOCTL_CLK) == BCMA_IOCTL_CLK
            && reset & BCMA_RESET_CTL_RESET == 0
            && ioctl & IOCTL_CPUHALT == 0
    }

    pub fn ram(&self) -> &[u8] {
        &self.ram
    }

    /// The firmware behind function 2, for the run report.
    pub fn sdpcm(&self) -> &Sdpcm {
        &self.sdpcm
    }

    pub fn read_byte(&self, func: u32, addr: u32) -> u8 {
        if func == 1 && (F1_MISC_START..=F1_MISC_LIMIT).contains(&addr) {
            return self.misc_read(addr);
        }
        if func == 1 && addr < F1_MISC_START {
            let mut b = [0u8];
            self.read(self.backplane_addr(addr), &mut b);
            return b[0];
        }
        0
    }

    pub fn write_byte(&mut self, func: u32, addr: u32, value: u8) {
        if func == 1 && (F1_MISC_START..=F1_MISC_LIMIT).contains(&addr) {
            self.misc_write(addr, value);
            return;
        }
        if func == 1 && addr < F1_MISC_START {
            let a = self.backplane_addr(addr);
            self.write(a, &[value]);
        }
    }

    pub fn read_io(&mut self, func: u32, addr: u32, out: &mut [u8]) {
        if func == 1 && addr < F1_MISC_START {
            self.read(self.backplane_addr(addr), out);
            return;
        }
        if func == 2 {
            // The frame FIFO: a fixed address, read in queue order.
            self.sdpcm.read(out);
            return;
        }
        out.fill(0);
    }

    pub fn write_io(&mut self, func: u32, addr: u32, data: &[u8]) {
        if func == 1 && addr < F1_MISC_START {
            let a = self.backplane_addr(addr);
            self.write(a, data);
        }
        if func == 2 {
            self.sdpcm.write(data);
        }
    }

    /// The CMD53 [`Self::write_io`] took has ended. On function 2 that
    /// terminates a frame: one per command, padded, so the chip has to be told
    /// where the padding starts.
    pub fn end_io(&mut self, func: u32) {
        if func == 2 {
            self.sdpcm.write_end();
            if self.sdpcm.frame_waiting() {
                // A latch, not a level: the driver clears it before it reads
                // the frame, so a level would re-interrupt for every frame it
                // was already fetching.
                self.intstatus |= I_HMB_FRAME_IND;
            }
        }
    }

    /// Bring the chip's firmware to model time `now_us`. Answers are queued
    /// while the host is still writing the question, so [`Self::end_io`] raises
    /// the indication for them; a scan's results come on their own clock and
    /// need it raised from here.
    #[inline]
    pub fn advance_to(&mut self, now_us: u64) {
        if self.sdpcm.advance_to(now_us) {
            self.intstatus |= I_HMB_FRAME_IND;
        }
    }

    pub fn irq_asserted(&self) -> bool {
        self.intstatus & self.reg_raw(CORES[SDIO_CORE].base + SD_HOSTINTMASK) != 0
    }

    /// Function 2 was enabled, the last thing the driver does before turning
    /// interrupts on: the firmware answers that it is ready. A real chip posts
    /// that when its firmware has started; the model uses this moment instead,
    /// and only if the ARM was actually released.
    pub fn enable_f2(&mut self) {
        if !self.arm_running() {
            return;
        }
        // A starting firmware reads the nvram at the top of memory before the
        // shared block below goes over the tail of it.
        if let Some(nvram) = nvram(&self.ram) {
            self.sdpcm.start(nvram);
        }
        self.publish_shared();
        self.mailbox = HMB_DATA_FWREADY | SDPCM_PROT_VERSION << HMB_DATA_VERSION_SHIFT;
        self.intstatus |= I_HMB_HOST_INT;
    }

    /// Leave an `sdpcm_shared` block where a running firmware leaves one, with
    /// its address in the top word of memory. It goes over the tail of the
    /// nvram, which by now both sides have read. What it says — alive, the
    /// driver's protocol version, no assertions, no trap, no console — is all
    /// true of a chip whose firmware is this model.
    fn publish_shared(&mut self) {
        let at = RAM_BASE + RAM_SIZE - 4 - SHARED_LEN;
        let mut block = vec![0u8; SHARED_LEN as usize];
        block[..4].copy_from_slice(&SDPCM_SHARED_VERSION.to_le_bytes());
        self.write(at, &block);
        self.write(RAM_BASE + RAM_SIZE - 4, &at.to_le_bytes());
    }

    fn misc_read(&self, addr: u32) -> u8 {
        let raw = self.f1[(addr - F1_MISC_START) as usize];
        match addr {
            // The request bits read back as written, which the driver checks,
            // with the availability bits on top: clocks are always there.
            F1_CHIPCLKCSR => {
                let mut avail = 0;
                if raw & (CSR_FORCE_ALP | CSR_ALP_AVAIL_REQ) != 0 {
                    avail |= CSR_ALP_AVAIL;
                }
                if raw & (CSR_FORCE_HT | CSR_HT_AVAIL_REQ) != 0 {
                    avail |= CSR_ALP_AVAIL | CSR_HT_AVAIL;
                }
                raw | avail
            }
            // Keep-sdio-on: on as soon as the bit is set.
            F1_SLEEPCSR => {
                if raw & SLEEPCSR_KSO != 0 {
                    SLEEPCSR_KSO | SLEEPCSR_DEVON
                } else {
                    0
                }
            }
            _ => raw,
        }
    }

    fn misc_write(&mut self, addr: u32, value: u8) {
        self.f1[(addr - F1_MISC_START) as usize] = value;
        if matches!(addr, F1_SBADDRLOW | F1_SBADDRMID | F1_SBADDRHIGH) {
            let byte = |a: u32| u32::from(self.f1[(a - F1_MISC_START) as usize]);
            self.window =
                (byte(F1_SBADDRLOW) << 8 | byte(F1_SBADDRMID) << 16 | byte(F1_SBADDRHIGH) << 24)
                    & SBWINDOW_MASK;
        }
    }

    pub fn read(&self, addr: u32, out: &mut [u8]) {
        if let Some(r) = ram_range(addr, out.len()) {
            out.copy_from_slice(&self.ram[r]);
            return;
        }
        for (i, b) in out.iter_mut().enumerate() {
            let a = addr.wrapping_add(i as u32);
            *b = match ram_range(a, 1) {
                Some(r) => self.ram[r.start],
                None => (self.reg(a & !3) >> (8 * (a & 3))) as u8,
            };
        }
    }

    pub fn write(&mut self, addr: u32, data: &[u8]) {
        if let Some(r) = ram_range(addr, data.len()) {
            self.ram[r].copy_from_slice(data);
            return;
        }
        // A whole aligned word goes in as a word: writing a
        // write-one-to-clear register byte at a time would clear bits the
        // driver never named.
        let mut off = 0;
        while off < data.len() {
            let a = addr.wrapping_add(off as u32);
            if let Some(r) = ram_range(a, 1) {
                self.ram[r.start] = data[off];
                off += 1;
                continue;
            }
            if a.is_multiple_of(4) && data.len() - off >= 4 {
                let word = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
                self.write_reg(a, word);
                off += 4;
                continue;
            }
            // A narrow access is a read-modify-write, as on the bus.
            let word = a & !3;
            let mut bytes = self.reg(word).to_le_bytes();
            bytes[(a & 3) as usize] = data[off];
            self.write_reg(word, u32::from_le_bytes(bytes));
            off += 1;
        }
    }

    fn reg(&self, addr: u32) -> u32 {
        if let Some(word) = self.erom_word(addr) {
            return word;
        }
        let cc = CORES[0].base;
        let arm = CORES[ARM_CORE].base;
        let sd = CORES[SDIO_CORE].base;
        match addr {
            a if a == cc + CC_CHIPID => CHIP_ID | CHIP_REV << 16 | SOCI_AI << 28,
            a if a == cc + CC_EROMPTR => EROM_BASE,
            a if a == sd + SD_INTSTATUS => self.intstatus,
            a if a == sd + SD_TOHOSTMAILBOXDATA => self.mailbox,
            // Bank count from the capability register, each bank's size from
            // the info register for the last bank index written.
            a if a == arm + ARMCR4_CAP => RAM_BANKS,
            a if a == arm + ARMCR4_BANKINFO => {
                if self.reg_raw(arm + ARMCR4_BANKIDX) < RAM_BANKS {
                    RAM_BANK_INFO
                } else {
                    0
                }
            }
            _ => self.reg_raw(addr),
        }
    }

    fn reg_raw(&self, addr: u32) -> u32 {
        self.regs.get(&addr).copied().unwrap_or(0)
    }

    fn write_reg(&mut self, addr: u32, value: u32) {
        let cc = CORES[0].base;
        let arm = CORES[ARM_CORE].base;
        let sd = CORES[SDIO_CORE].base;
        // Read-only, as on silicon: identity, EROM and its pointer, the CR4's
        // capabilities, and the chip's own mailbox word.
        if addr == cc + CC_CHIPID
            || addr == cc + CC_EROMPTR
            || addr == arm + ARMCR4_CAP
            || addr == arm + ARMCR4_BANKINFO
            || addr == sd + SD_TOHOSTMAILBOXDATA
            || self.erom_word(addr).is_some()
        {
            return;
        }
        // `intstatus` is write-one-to-clear.
        if addr == sd + SD_INTSTATUS {
            self.intstatus &= !value;
            return;
        }
        // The acknowledgement is the only bit the chip acts on: it takes back
        // the word it posted and the interrupt that announced it.
        if addr == sd + SD_TOSBMAILBOX {
            if value & SMB_INT_ACK != 0 {
                self.mailbox = 0;
                self.intstatus &= !I_HMB_HOST_INT;
            }
            return;
        }
        self.regs.insert(addr, value);
    }

    fn erom_word(&self, addr: u32) -> Option<u32> {
        let off = addr.checked_sub(EROM_BASE)? as usize;
        if !off.is_multiple_of(4) {
            return None;
        }
        self.erom.get(off / 4).copied()
    }
}

fn ram_range(addr: u32, len: usize) -> Option<std::ops::Range<usize>> {
    let off = addr.checked_sub(RAM_BASE)? as usize;
    let end = off.checked_add(len)?;
    if end > RAM_SIZE as usize {
        return None;
    }
    Some(off..end)
}

/// The nvram at the top of the chip's memory (`sdio.c:3562`), ending in the
/// token the host appends: length in words in `[15:0]` and its complement in
/// `[31:16]`, the complement being what tells a token from any other word. The
/// bytes are NUL-separated `key=value` entries, read by [`Sdpcm::start`].
fn nvram(ram: &[u8]) -> Option<&[u8]> {
    let end = ram.len().checked_sub(4)?;
    let token = u32::from_le_bytes(ram[end..].try_into().ok()?);
    let words = token & 0xFFFF;
    if words == 0 || (!token) >> 16 != words {
        return None;
    }
    let at = end.checked_sub(words as usize * 4)?;
    Some(&ram[at..end])
}

/// The enumeration table the driver walks: two component descriptors per core,
/// the addresses of its register block and wrapper, then a terminator. Each core
/// claims one slave wrapper and no master port, the shape the driver reads as
/// "pair the plain slave port with the slave wrapper" (`chip.c:833`).
fn erom() -> Vec<u32> {
    let mut words = Vec::new();
    for core in CORES.iter() {
        words.push(MANUF_BCM << 20 | u32::from(core.id) << 8 | DMP_DESC_COMPONENT);
        // CompIdentB: revision and port counts. The one slave wrapper keeps
        // `nmw + nsw` non-zero, without which the scan skips the core.
        words.push(core.rev << 24 | 1 << 19 | 1 << 9 | DMP_DESC_COMPONENT);
        // The 4 KiB register block and the wrapper beside it.
        words.push(core.base | DMP_SLAVE_TYPE_SLAVE << 6 | DMP_DESC_ADDRESS);
        words.push(core.wrap | DMP_SLAVE_TYPE_SWRAP << 6 | DMP_DESC_ADDRESS);
    }
    words.push(DMP_DESC_EOT);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Aim the window at `addr` and return the function-1 offset for a
    /// 4-byte access.
    fn aim(chip: &mut Cyw43455, addr: u32) -> u32 {
        let v = (addr & SBWINDOW_MASK) >> 8;
        for i in 0..3 {
            chip.write_byte(1, F1_SBADDRLOW + i, (v >> (8 * i)) as u8);
        }
        (addr & SB_OFT_ADDR_MASK) | 0x8000
    }

    fn readl(chip: &mut Cyw43455, addr: u32) -> u32 {
        let off = aim(chip, addr);
        let mut b = [0u8; 4];
        chip.read_io(1, off, &mut b);
        u32::from_le_bytes(b)
    }

    fn writel(chip: &mut Cyw43455, addr: u32, value: u32) {
        let off = aim(chip, addr);
        chip.write_io(1, off, &value.to_le_bytes());
    }

    #[test]
    fn the_chip_id_says_a_4345_on_an_axi_backplane() {
        let mut chip = Cyw43455::new();
        let id = readl(&mut chip, ENUM_BASE);
        assert_eq!(id & 0xFFFF, 0x4345);
        assert_eq!((id >> 16) & 0xF, 6);
        assert_eq!(id >> 28, SOCI_AI);
        assert_ne!(0xFFFF_FDC0u32 & (1 << ((id >> 16) & 0xF)), 0);
    }

    fn scan(chip: &mut Cyw43455) -> Vec<(u16, u32, u32, u32)> {
        let mut at = readl(chip, ENUM_BASE + CC_EROMPTR);
        let mut found = Vec::new();
        loop {
            let val = readl(chip, at);
            at += 4;
            let desc = val & 0xF;
            if desc == DMP_DESC_EOT {
                break;
            }
            if desc != DMP_DESC_COMPONENT {
                continue;
            }
            let id = ((val & 0x000F_FF00) >> 8) as u16;
            let b = readl(chip, at);
            at += 4;
            assert_eq!(b & 0xF, DMP_DESC_COMPONENT, "CompIdentB at {at:#x}");
            let rev = b >> 24;
            assert_ne!((b >> 19 & 0x1F) + (b >> 14 & 0x1F), 0, "no ports");
            let (mut base, mut wrap) = (0, 0);
            while base == 0 || wrap == 0 {
                let a = readl(chip, at);
                if a & 0xF == DMP_DESC_COMPONENT {
                    break;
                }
                at += 4;
                assert_eq!(a & 0xF, DMP_DESC_ADDRESS);
                assert_eq!(a >> 4 & 3, 0, "4 KiB region");
                match a >> 6 & 3 {
                    0 => base = a & 0xFFFF_F000,
                    2 => wrap = a & 0xFFFF_F000,
                    other => panic!("port type {other}"),
                }
            }
            found.push((id, rev, base, wrap));
        }
        found
    }

    #[test]
    fn the_erom_enumerates_the_cores_the_driver_insists_on() {
        let mut chip = Cyw43455::new();
        let cores = scan(&mut chip);
        assert_eq!(cores.len(), CORES.len());
        assert_eq!(cores[0].0, CORE_CHIPCOMMON);
        assert_eq!(cores[0].2, ENUM_BASE);
        for id in [CORE_SDIO_DEV, CORE_ARM_CR4, CORE_80211] {
            assert!(cores.iter().any(|c| c.0 == id), "core {id:#x} missing");
        }
        let sdio = cores.iter().find(|c| c.0 == CORE_SDIO_DEV).unwrap();
        assert!(sdio.1 >= 12, "sdio core rev {}", sdio.1);
        assert!(cores.iter().all(|c| c.3 != 0));
    }

    #[test]
    fn the_arm_core_sizes_the_memory_the_firmware_goes_in() {
        let mut chip = Cyw43455::new();
        let arm = CORES[ARM_CORE].base;
        let cap = readl(&mut chip, arm + ARMCR4_CAP);
        let banks = (cap & 0xF) + (cap >> 4 & 0xF);
        let mut size = 0;
        for i in 0..banks {
            writel(&mut chip, arm + ARMCR4_BANKIDX, i);
            let info = readl(&mut chip, arm + ARMCR4_BANKINFO);
            let mult = if info & 0x200 != 0 {
                ARMCR4_BSZ_MULT / 8
            } else {
                ARMCR4_BSZ_MULT
            };
            size += ((info & 0x7F) + 1) * mult;
        }
        assert_eq!(size, RAM_SIZE);
        assert!(size >= 609_309 + 2_074);
        assert!(size <= 4 * 1024 * 1024);
    }

    #[test]
    fn memory_written_through_the_window_reads_back() {
        let mut chip = Cyw43455::new();
        let pattern: Vec<u8> = (0..0x2000u32).map(|i| (i * 7) as u8).collect();
        for (i, chunk) in pattern.chunks(0x800).enumerate() {
            let at = RAM_BASE + 0x7000 + (i as u32) * 0x800;
            let off = aim(&mut chip, at);
            chip.write_io(1, off, chunk);
        }
        let mut back = vec![0u8; pattern.len()];
        for (i, chunk) in back.chunks_mut(0x800).enumerate() {
            let at = RAM_BASE + 0x7000 + (i as u32) * 0x800;
            let off = aim(&mut chip, at);
            chip.read_io(1, off, chunk);
        }
        assert_eq!(back, pattern);
        let top = RAM_BASE + RAM_SIZE - 4;
        writel(&mut chip, top, 0xDEAD_BEEF);
        assert_eq!(readl(&mut chip, top), 0xDEAD_BEEF);
        assert_eq!(readl(&mut chip, RAM_BASE + RAM_SIZE), 0);
    }

    #[test]
    fn the_clock_register_reads_back_what_was_asked_for() {
        let mut chip = Cyw43455::new();
        let wrote = 0x28;
        chip.write_byte(1, F1_CHIPCLKCSR, wrote);
        let read = chip.read_byte(1, F1_CHIPCLKCSR);
        assert_eq!(read & !(CSR_ALP_AVAIL | CSR_HT_AVAIL), wrote);
        assert_ne!(read & CSR_ALP_AVAIL, 0, "ALP available");
        chip.write_byte(1, F1_CHIPCLKCSR, CSR_HT_AVAIL_REQ);
        let read = chip.read_byte(1, F1_CHIPCLKCSR);
        assert_eq!(read & (CSR_ALP_AVAIL | CSR_HT_AVAIL), 0xC0);
        chip.write_byte(1, F1_CHIPCLKCSR, 0);
        assert_eq!(chip.read_byte(1, F1_CHIPCLKCSR), 0);
    }

    #[test]
    fn keep_sdio_on_answers_with_the_device_on() {
        let mut chip = Cyw43455::new();
        assert_eq!(chip.read_byte(1, F1_SLEEPCSR), 0);
        chip.write_byte(1, F1_SLEEPCSR, SLEEPCSR_KSO);
        assert_eq!(
            chip.read_byte(1, F1_SLEEPCSR),
            SLEEPCSR_KSO | SLEEPCSR_DEVON
        );
        chip.write_byte(1, F1_SLEEPCSR, 0);
        assert_eq!(chip.read_byte(1, F1_SLEEPCSR), 0);
    }

    #[test]
    fn the_arm_starts_held_and_is_let_go_through_its_wrapper() {
        let mut chip = Cyw43455::new();
        let wrap = CORES[ARM_CORE].wrap;
        assert!(!chip.arm_running(), "nothing has released it yet");

        writel(&mut chip, wrap + BCMA_IOCTL, IOCTL_CPUHALT | BCMA_IOCTL_FGC);
        writel(&mut chip, wrap + BCMA_RESET_CTL, BCMA_RESET_CTL_RESET);
        assert_eq!(readl(&mut chip, wrap + BCMA_RESET_CTL), 1, "in reset");
        assert!(!chip.arm_running());

        writel(&mut chip, wrap + BCMA_RESET_CTL, 0);
        writel(&mut chip, wrap + BCMA_IOCTL, BCMA_IOCTL_CLK);
        assert!(chip.arm_running());
        let ioctl = readl(&mut chip, wrap + BCMA_IOCTL);
        assert_eq!(ioctl & (BCMA_IOCTL_FGC | BCMA_IOCTL_CLK), BCMA_IOCTL_CLK);
    }

    fn release_arm(chip: &mut Cyw43455) {
        let wrap = CORES[ARM_CORE].wrap;
        writel(chip, wrap + BCMA_RESET_CTL, 0);
        writel(chip, wrap + BCMA_IOCTL, BCMA_IOCTL_CLK);
    }

    fn download_nvram(chip: &mut Cyw43455, entries: &[&str]) {
        let mut vars = Vec::new();
        for e in entries {
            vars.extend_from_slice(e.as_bytes());
            vars.push(0);
        }
        vars.resize(vars.len().next_multiple_of(4), 0);
        let words = (vars.len() / 4) as u32;
        let at = RAM_BASE + RAM_SIZE - 4 - vars.len() as u32;
        let off = aim(chip, at);
        chip.write_io(1, off, &vars);
        writel(chip, RAM_BASE + RAM_SIZE - 4, (!words) << 16 | words);
    }

    #[test]
    fn the_firmware_starts_on_the_nvram_the_host_left_at_the_top_of_memory() {
        let mut chip = Cyw43455::new();
        download_nvram(
            &mut chip,
            &["sromrev=11", "macaddr=02:00:5e:00:57:01", "boardtype=0x6e4"],
        );
        release_arm(&mut chip);
        chip.enable_f2();
        assert_eq!(chip.sdpcm().mac(), [0x02, 0x00, 0x5E, 0x00, 0x57, 0x01]);

        let top = readl(&mut chip, RAM_BASE + RAM_SIZE - 4);
        assert_eq!(top, RAM_BASE + RAM_SIZE - 4 - SHARED_LEN);
    }

    #[test]
    fn a_top_word_that_is_not_a_token_is_not_nvram() {
        let mut chip = Cyw43455::new();
        release_arm(&mut chip);
        chip.enable_f2();
        assert_eq!(
            chip.sdpcm().mac_source(),
            crate::periph::sdpcm::MacSource::Otp
        );

        let mut chip = Cyw43455::new();
        writel(&mut chip, RAM_BASE + RAM_SIZE - 4, 0xDEAD_BEEF);
        assert!(nvram(chip.ram()).is_none());
    }

    fn set_iovar(chip: &mut Cyw43455, seq: u8, name: &str, value: &[u8]) {
        let mut payload = name.as_bytes().to_vec();
        payload.push(0);
        payload.extend_from_slice(value);

        let mut bcdc = Vec::new();
        bcdc.extend_from_slice(&263u32.to_le_bytes());
        bcdc.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bcdc.extend_from_slice(&(1u32 << 16 | 2).to_le_bytes());
        bcdc.extend_from_slice(&0u32.to_le_bytes());
        bcdc.extend_from_slice(&payload);

        let len = (12 + bcdc.len()) as u16;
        let mut frame = Vec::new();
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&(!len).to_le_bytes());
        frame.extend_from_slice(&(u32::from(seq) | 12 << 24).to_le_bytes());
        frame.extend_from_slice(&0u32.to_le_bytes());
        frame.extend_from_slice(&bcdc);
        chip.write_io(2, 0, &frame);
        chip.end_io(2);
        let mut ack = [0u8; 64];
        chip.read_io(2, 0, &mut ack);
        writel(chip, CORES[SDIO_CORE].base + SD_INTSTATUS, I_HMB_FRAME_IND);
    }

    #[test]
    fn a_scan_raises_the_frame_indication_on_its_own_clock() {
        let mut chip = Cyw43455::new();
        release_arm(&mut chip);
        chip.enable_f2();
        writel(
            &mut chip,
            CORES[SDIO_CORE].base + SD_HOSTINTMASK,
            I_HMB_FRAME_IND,
        );
        let mut mask = vec![0u8; 25];
        mask[69 / 8] |= 1 << (69 % 8);
        let mut ext = vec![1, 3, mask.len() as u8, 0];
        ext.extend_from_slice(&mask);
        set_iovar(&mut chip, 0, "event_msgs_ext", &ext);

        let mut params = vec![0u8; 8 + 72];
        params[0] = 2;
        params[4] = 1;
        set_iovar(&mut chip, 1, "escan", &params);
        assert!(!chip.irq_asserted(), "a scan that answered instantly");

        chip.advance_to(1_000);
        assert!(!chip.irq_asserted());
        chip.advance_to(10_000_000);
        assert!(chip.irq_asserted(), "no frame indication for the results");
        assert_ne!(chip.intstatus & I_HMB_FRAME_IND, 0);
        assert!(chip.sdpcm().frame_waiting());
        assert_eq!(chip.sdpcm().escans(), 1);
        assert_eq!(chip.sdpcm().escan_results(), 1);
    }

    #[test]
    fn nothing_reads_as_a_dead_bus() {
        let mut chip = Cyw43455::new();
        for addr in [0u32, 0x1800_5000, 0x1810_5000, 0xFFFF_0000] {
            assert_eq!(readl(&mut chip, addr), 0, "at {addr:#x}");
        }
        writel(&mut chip, 0, 0x1234_5678);
    }
}
