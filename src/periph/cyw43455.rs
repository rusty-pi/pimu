//! The CYW43455's own side of the SDIO bus: the backplane behind function 1,
//! the cores hanging off it, and the ARM's SRAM.
//!
//! [`SdCard`](super::sdcard::SdCard) plays the chip's SDIO *card* — the CCCR,
//! the FBRs and the CIS, which are the standard's and the same on any SDIO
//! card. Everything past that is this module: function 1's own registers, the
//! 32 KiB window they aim at the chip's internal bus, and what answers there.
//!
//! ```text
//!   f1 [0x1000a..0x1000c]   SBADDR{LOW,MID,HIGH}  the window base
//!   f1 [0x1000e]            CHIPCLKCSR            backplane clock request
//!   f1 [0x1001f]            SLEEPCSR              keep-sdio-on
//!   f1 [0x00000..0x07fff]   the window, byte or block, CMD52 or CMD53
//!   f1 [0x08000..0x0ffff]   the same window, asked for in 4-byte accesses
//! ```
//!
//! ### What the model has to answer
//!
//! `brcmfmac` is the specification here: it walks the chip before it will
//! download anything, and every value below is the one that walk demands.
//! Line references are to **raspberrypi/linux**
//! `16f1da3c4e94437449d6aa151589ca0ad4b388bb`, the kernel
//! `scripts/fetch-firmware.sh` pins, under
//! `drivers/net/wireless/broadcom/brcm80211/brcmfmac/`:
//! <https://github.com/raspberrypi/linux/blob/16f1da3c4e94437449d6aa151589ca0ad4b388bb/drivers/net/wireless/broadcom/brcm80211/brcmfmac/chip.c>
//!
//! 1. `brcmf_chip_recognition` (`chip.c:970`) reads the chipcommon `chipid`
//!    word at `0x18000000`. Its low half is the chip id, `[19:16]` the
//!    revision and `[31:28]` the backplane type, which has to be `SOCI_AI`.
//! 2. It then walks the **EROM**, a table of descriptors the `eromptr`
//!    register points at (`brcmf_chip_dmp_erom_scan`, `chip.c:905`), and
//!    every core it finds becomes a `brcmf_core` with a register base and a
//!    wrapper base. `brcmf_chip_cores_check` (`chip.c:524`) insists on a CPU
//!    core, and `brcmf_sdio_probe_attach` (`sdio.c:4194`) on an SDIO device
//!    core and a chipcommon core.
//! 3. `brcmf_chip_get_raminfo` (`chip.c:756`) sizes the ARM CR4's tightly
//!    coupled memory out of the core's `ARMCR4_CAP` and its per-bank info
//!    registers, and takes the base from a per-chip table (`chip.c:710`).
//!    The firmware is written there and the nvram at the top of it.
//! 4. `brcmf_chip_set_passive` / `set_active` (`chip.c:1400`, `chip.c:1423`)
//!    drive each core's wrapper `BCMA_IOCTL` and `BCMA_RESET_CTL`, which is
//!    how the ARM is held and then let go.
//!
//! The chip's own bus addresses are the model's to choose: nothing outside
//! ever names them, because the driver learns every one of them from the
//! EROM this module also writes. Only the values the driver checks against
//! its own tables — the chip id, the revision, the core ids, the RAM base —
//! are forced, and each is cited where it is defined.
//!
//! ### What it deliberately leaves out
//!
//! Function 2 is the chip's frame FIFO, and nothing on the other side of it
//! is modelled yet: reads there are zeros and writes go nowhere. The ARM
//! comes out of reset and runs nothing, so the firmware's side of the SDPCM
//! handshake never happens.

use std::collections::BTreeMap;

/// Function 1's misc registers, `SBSDIO_FUNC1_MISC_REG_START`..`_LIMIT`
/// (`sdio.h:116`).
const F1_MISC_START: u32 = 0x1_0000;
const F1_MISC_LIMIT: u32 = 0x1_001F;
/// `SBSDIO_FUNC1_SBADDR{LOW,MID,HIGH}` (`sdio.h:75`): the window base, byte
/// by byte. `brcmf_sdiod_set_backplane_window` (`bcmsdh.c:219`) writes
/// `(addr & SBSDIO_SBWINDOW_MASK) >> 8` across the three.
const F1_SBADDRLOW: u32 = 0x1_000A;
const F1_SBADDRMID: u32 = 0x1_000B;
const F1_SBADDRHIGH: u32 = 0x1_000C;
/// `SBSDIO_FUNC1_CHIPCLKCSR` (`sdio.h:83`).
const F1_CHIPCLKCSR: u32 = 0x1_000E;
/// `SBSDIO_FUNC1_SLEEPCSR` (`sdio.h:109`).
const F1_SLEEPCSR: u32 = 0x1_001F;

/// `CHIPCLKCSR` request bits (`sdio.c:180`): force ALP, force HT, ask for ALP
/// and ask for HT. The two status bits the chip answers with are
/// `SBSDIO_ALP_AVAIL` and `SBSDIO_HT_AVAIL`.
const CSR_FORCE_ALP: u8 = 0x01;
const CSR_FORCE_HT: u8 = 0x02;
const CSR_ALP_AVAIL_REQ: u8 = 0x08;
const CSR_HT_AVAIL_REQ: u8 = 0x10;
const CSR_ALP_AVAIL: u8 = 0x40;
const CSR_HT_AVAIL: u8 = 0x80;

/// `SLEEPCSR` (`sdio.h:110`): the host sets KSO and waits for the chip to
/// answer with KSO and DEVON both set (`brcmf_sdio_kso_control`,
/// `sdio.c:742`).
const SLEEPCSR_KSO: u8 = 0x01;
const SLEEPCSR_DEVON: u8 = 0x02;

/// `SBSDIO_SB_OFT_ADDR_MASK` (`sdio.h:122`): the offset a function-1 address
/// selects inside the window. Bit 15 on top of it is
/// `SBSDIO_SB_ACCESS_2_4B_FLAG`, which asks the chip for 4-byte accesses and
/// does not move the address.
const SB_OFT_ADDR_MASK: u32 = 0x07FFF;
/// `SBSDIO_SBWINDOW_MASK` (`sdio.h:128`): what of a backplane address the
/// three `SBADDR` bytes carry.
const SBWINDOW_MASK: u32 = 0xFFFF_8000;

/// `SI_ENUM_BASE_DEFAULT` (`soc.h:9`), where `brcmf_chip_enum_base` puts the
/// chipcommon core and where the chip-id read that starts everything goes.
pub const ENUM_BASE: u32 = 0x1800_0000;

/// Core ids, from `include/linux/bcma/bcma.h`. The driver looks each of
/// these up by id, so they are not the model's to choose.
const CORE_CHIPCOMMON: u16 = 0x800;
const CORE_80211: u16 = 0x812;
const CORE_SDIO_DEV: u16 = 0x829;
const CORE_ARM_CR4: u16 = 0x83E;
/// `BCMA_MANUF_BCM`: the designer field of every core's EROM entry.
const MANUF_BCM: u32 = 0x4BF;

/// The chip the SDIO card in front of this is: `BRCM_CC_4345_CHIP_ID`
/// (`brcm_hw_ids.h:34`) at revision 6. `brcmf_fw_alloc_request` picks the
/// firmware by chip id and a revision mask, and
/// `BRCMF_FW_ENTRY(BRCM_CC_4345_CHIP_ID, 0xFFFFFDC0, 43455)` (`sdio.c:665`)
/// is what maps this pair to `brcmfmac43455-sdio`, the blob the card carries.
const CHIP_ID: u32 = 0x4345;
const CHIP_REV: u32 = 6;
/// `SOCI_AI` (`chip.c:22`): an AXI backplane, the only kind
/// `brcmf_chip_recognition` will walk for anything but the 4329.
const SOCI_AI: u32 = 1;

/// Where the enumeration table sits. Any backplane address the window can
/// reach will do; the driver only ever gets here through `eromptr`.
const EROM_BASE: u32 = 0x1801_0000;

/// chipcommon register offsets (`chipcommon.h`, `struct chipcregs`).
const CC_CHIPID: u32 = 0x00;
const CC_EROMPTR: u32 = 0xFC;

/// ARM CR4 core register offsets (`chip.c:205`) and the bank-size arithmetic
/// `brcmf_chip_tcm_ramsize` (`chip.c:680`) does with them.
const ARMCR4_CAP: u32 = 0x04;
const ARMCR4_BANKIDX: u32 = 0x40;
const ARMCR4_BANKINFO: u32 = 0x44;
const ARMCR4_BSZ_MULT: u32 = 8192;

/// A core wrapper's registers (`include/linux/bcma/bcma_regs.h`), which is
/// how `brcmf_chip_ai_iscoreup` / `_coredisable` / `_resetcore`
/// (`chip.c:263`, `chip.c:346`, `chip.c:432`) hold and release a core.
const BCMA_IOCTL: u32 = 0x408;
const BCMA_RESET_CTL: u32 = 0x800;
const BCMA_IOCTL_CLK: u32 = 0x0001;
const BCMA_IOCTL_FGC: u32 = 0x0002;
const BCMA_RESET_CTL_RESET: u32 = 0x0001;
/// `ARMCR4_BCMA_IOCTL_CPUHALT` (`chip.c:78`): set while the ARM is parked,
/// cleared by `brcmf_chip_cr4_set_active` once the firmware is in RAM.
const IOCTL_CPUHALT: u32 = 0x0020;

/// Where the ARM's tightly coupled memory starts on a 4345:
/// `brcmf_chip_tcm_rambase` (`chip.c:710`) returns `0x198000` for
/// `BRCM_CC_4345_CHIP_ID`, and `brcmf_sdio_download_code_file`
/// (`sdio.c:3543`) writes the firmware image there.
pub const RAM_BASE: u32 = 0x0019_8000;
/// How much of it there is. The driver asks the ARM CR4 core rather than a
/// table, so this is the model's choice, made of six 128 KiB banks below.
/// It has to hold the image the card carries (`brcmfmac43455-sdio.bin`, some
/// 595 KiB) *and* the nvram, which `brcmf_sdio_download_nvram`
/// (`sdio.c:3562`) puts at `rambase + ramsize - varsz`, the very top; and it
/// has to stay under the 4 MiB `brcmf_chip_get_raminfo` (`chip.c:805`)
/// rejects anything above. Six banks of 128 KiB = 768 KiB does both.
pub const RAM_SIZE: u32 = RAM_BANKS * (RAM_BANK_INFO + 1) * ARMCR4_BSZ_MULT;
/// What the CR4's capability and bank-info registers report, and what
/// `brcmf_chip_tcm_ramsize` makes of them: `nab` banks, each
/// `(bankinfo + 1) * ARMCR4_BSZ_MULT` bytes.
const RAM_BANKS: u32 = 6;
const RAM_BANK_INFO: u32 = 0xF;

/// One core as the EROM describes it and as the driver then addresses it.
struct Core {
    id: u16,
    rev: u32,
    base: u32,
    wrap: u32,
}

/// The cores this chip enumerates. Chipcommon has to come first: the driver
/// takes `list_first_entry` for it (`brcmf_chip_get_chipcommon`,
/// `chip.c:1225`). The SDIO device core is what `brcmf_sdio_probe_attach`
/// looks up for its mailboxes and interrupt registers; the ARM CR4 is the
/// CPU `brcmf_chip_cores_check` demands, and the 802.11 core is the one
/// `brcmf_chip_cr4_set_passive` holds down while the firmware is loaded.
///
/// The SDIO core's revision is 12: `brcmf_sdio_kso_init` (`sdio.c:3700`)
/// only enables keep-sdio-on from rev 12, and `brcmf_sdio_bus_preinit`
/// (`sdio.c:3750`) only sets `bus:txglomalign` from rev 12 — the modern path
/// in both.
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

/// The ARM CR4 core, by the index it has in [`CORES`].
const ARM_CORE: usize = 2;

/// EROM descriptor types and fields (`chip.c:24`). A descriptor's low nibble
/// says what it is; `brcmf_chip_dmp_get_desc` (`chip.c:813`) reads them one
/// word at a time.
const DMP_DESC_COMPONENT: u32 = 0x1;
const DMP_DESC_ADDRESS: u32 = 0x5;
const DMP_DESC_EOT: u32 = 0xF;
/// `DMP_SLAVE_TYPE` in an address descriptor's `[7:6]`: a plain slave port,
/// or the wrapper `brcmf_chip_dmp_get_regaddr` (`chip.c:833`) pairs with it
/// when the component had no master port.
const DMP_SLAVE_TYPE_SLAVE: u32 = 0;
const DMP_SLAVE_TYPE_SWRAP: u32 = 2;

/// The chip behind function 1 of the WiFi card.
pub struct Cyw43455 {
    /// Function 1's misc registers, `0x10000`..`0x1001f`, as written.
    f1: [u8; 0x20],
    /// The backplane address the window is aimed at, from the three `SBADDR`
    /// bytes.
    window: u32,
    /// Backplane registers, sparse and 32 bits each: everything the driver
    /// writes and nothing else answers. Reads of what is not here are 0 —
    /// never `0xffffffff`, which `brcmf_chip_recognition` (`chip.c:985`)
    /// reads as a dead bus.
    regs: BTreeMap<u32, u32>,
    /// The ARM's tightly coupled memory, [`RAM_SIZE`] bytes at [`RAM_BASE`]:
    /// where the firmware image and the nvram land.
    ram: Vec<u8>,
    /// The enumeration table, as words at [`EROM_BASE`].
    erom: Vec<u32>,
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
        }
    }

    /// The window the `SBADDR` bytes currently aim at.
    pub fn window(&self) -> u32 {
        self.window
    }

    /// The backplane address a function-1 offset reaches: the window plus the
    /// offset, with bit 15 — the 4-byte-access flag — dropped.
    pub fn backplane_addr(&self, offset: u32) -> u32 {
        self.window | (offset & SB_OFT_ADDR_MASK)
    }

    /// The ARM is out of reset and not halted, which is the last thing
    /// `brcmf_chip_cr4_set_active` (`chip.c:1355`) does to start the
    /// firmware.
    pub fn arm_running(&self) -> bool {
        let wrap = CORES[ARM_CORE].wrap;
        let ioctl = self.reg(wrap + BCMA_IOCTL);
        let reset = self.reg(wrap + BCMA_RESET_CTL);
        // `brcmf_chip_ai_iscoreup` (`chip.c:263`) asks for the clock on
        // without the force, and the core out of reset; the halt bit on top
        // of that is the ARM's own.
        ioctl & (BCMA_IOCTL_FGC | BCMA_IOCTL_CLK) == BCMA_IOCTL_CLK
            && reset & BCMA_RESET_CTL_RESET == 0
            && ioctl & IOCTL_CPUHALT == 0
    }

    /// What is in the ARM's memory, for tests and dumps.
    pub fn ram(&self) -> &[u8] {
        &self.ram
    }

    // --- function-1 register space -------------------------------------

    /// One byte of function `func`'s address space, as CMD52 reads it.
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

    /// Write one byte of function `func`'s address space (CMD52).
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

    /// Fill `out` from function `func` at `addr` (the data phase of a CMD53
    /// read).
    pub fn read_io(&self, func: u32, addr: u32, out: &mut [u8]) {
        if func == 1 && addr < F1_MISC_START {
            self.read(self.backplane_addr(addr), out);
            return;
        }
        // Function 2 is the frame FIFO, which nothing answers yet; a misc
        // register is a byte and is not read this way.
        out.fill(0);
    }

    /// Take `data` for function `func` at `addr` (the data phase of a CMD53
    /// write).
    pub fn write_io(&mut self, func: u32, addr: u32, data: &[u8]) {
        if func == 1 && addr < F1_MISC_START {
            let a = self.backplane_addr(addr);
            self.write(a, data);
        }
    }

    fn misc_read(&self, addr: u32) -> u8 {
        let raw = self.f1[(addr - F1_MISC_START) as usize];
        match addr {
            // The clock request bits read back as written — the driver
            // compares them against what it wrote (`sdio.c:4179`) — with the
            // "available" status bits on top. The model's clocks are always
            // there the moment they are asked for.
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
            // Keep-sdio-on: the chip reports itself on as soon as the bit is
            // set, which is the pair `brcmf_sdio_kso_control` polls for.
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

    // --- the backplane -------------------------------------------------

    /// Read `out.len()` bytes from backplane address `addr`.
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

    /// Write `data` to backplane address `addr`.
    pub fn write(&mut self, addr: u32, data: &[u8]) {
        if let Some(r) = ram_range(addr, data.len()) {
            self.ram[r].copy_from_slice(data);
            return;
        }
        // Everything else is a register. The driver only ever writes whole
        // 32-bit words to one (`brcmf_sdiod_writel`), so a partial write
        // reads the word, replaces the bytes and puts it back.
        for (i, byte) in data.iter().enumerate() {
            let a = addr.wrapping_add(i as u32);
            if let Some(r) = ram_range(a, 1) {
                self.ram[r.start] = *byte;
                continue;
            }
            let word = a & !3;
            let mut bytes = self.reg(word).to_le_bytes();
            bytes[(a & 3) as usize] = *byte;
            self.write_reg(word, u32::from_le_bytes(bytes));
        }
    }

    /// One backplane register.
    fn reg(&self, addr: u32) -> u32 {
        if let Some(word) = self.erom_word(addr) {
            return word;
        }
        let cc = CORES[0].base;
        let arm = CORES[ARM_CORE].base;
        match addr {
            // The word that starts the whole bring-up.
            a if a == cc + CC_CHIPID => CHIP_ID | CHIP_REV << 16 | SOCI_AI << 28,
            a if a == cc + CC_EROMPTR => EROM_BASE,
            // The CR4's capability register says how many banks of memory
            // there are, and each bank's size comes from the info register
            // for whichever bank index was last written.
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

    /// What is stored for `addr`, with none of the derived registers.
    fn reg_raw(&self, addr: u32) -> u32 {
        self.regs.get(&addr).copied().unwrap_or(0)
    }

    fn write_reg(&mut self, addr: u32, value: u32) {
        let cc = CORES[0].base;
        let arm = CORES[ARM_CORE].base;
        // Read-only: the identity, the EROM and its pointer, and the CR4's
        // capabilities. A write to one changes nothing, as on silicon.
        if addr == cc + CC_CHIPID
            || addr == cc + CC_EROMPTR
            || addr == arm + ARMCR4_CAP
            || addr == arm + ARMCR4_BANKINFO
            || self.erom_word(addr).is_some()
        {
            return;
        }
        self.regs.insert(addr, value);
    }

    /// The EROM word at `addr`, if `addr` is in the table.
    fn erom_word(&self, addr: u32) -> Option<u32> {
        let off = addr.checked_sub(EROM_BASE)? as usize;
        if !off.is_multiple_of(4) {
            return None;
        }
        self.erom.get(off / 4).copied()
    }
}

/// The slice of [`Cyw43455::ram`] an access covers, or `None` if it is not
/// wholly inside the ARM's memory.
fn ram_range(addr: u32, len: usize) -> Option<std::ops::Range<usize>> {
    let off = addr.checked_sub(RAM_BASE)? as usize;
    let end = off.checked_add(len)?;
    if end > RAM_SIZE as usize {
        return None;
    }
    Some(off..end)
}

/// The enumeration table `brcmf_chip_dmp_erom_scan` walks: two component
/// descriptors per core, then the addresses of its register block and of its
/// wrapper, and a terminator.
///
/// Each core claims one slave wrapper and no master port, which is the shape
/// `brcmf_chip_dmp_get_regaddr` (`chip.c:833`) takes as "the descriptor after
/// the component is already an address": it then pairs the plain slave port
/// with the slave wrapper.
fn erom() -> Vec<u32> {
    let mut words = Vec::new();
    for core in CORES.iter() {
        // CompIdentA: designer, part number, and the type in [3:0].
        words.push(MANUF_BCM << 20 | u32::from(core.id) << 8 | DMP_DESC_COMPONENT);
        // CompIdentB: the revision in [31:24] and the port counts. One slave
        // wrapper ([23:19]) is what keeps `nmw + nsw` non-zero, without which
        // the scan skips the core entirely (`chip.c:944`).
        words.push(core.rev << 24 | 1 << 19 | 1 << 9 | DMP_DESC_COMPONENT);
        // The register block, 4 KiB, and the wrapper beside it. `[31:12]` is
        // the base, `[7:6]` the port type and `[5:4]` the size class (0 = 4
        // KiB).
        words.push(core.base | DMP_SLAVE_TYPE_SLAVE << 6 | DMP_DESC_ADDRESS);
        words.push(core.wrap | DMP_SLAVE_TYPE_SWRAP << 6 | DMP_DESC_ADDRESS);
    }
    words.push(DMP_DESC_EOT);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point the window at `addr` the way
    /// `brcmf_sdiod_set_backplane_window` does, and hand back the function-1
    /// offset the driver would then use for a 4-byte access.
    fn aim(chip: &mut Cyw43455, addr: u32) -> u32 {
        let v = (addr & SBWINDOW_MASK) >> 8;
        for i in 0..3 {
            chip.write_byte(1, F1_SBADDRLOW + i, (v >> (8 * i)) as u8);
        }
        (addr & SB_OFT_ADDR_MASK) | 0x8000
    }

    /// One 32-bit read through the window, as `brcmf_sdiod_readl` makes it.
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
        // The revision has to be one the driver's firmware table maps to
        // `brcmfmac43455-sdio`: mask 0xFFFFFDC0, bit per revision.
        assert_ne!(0xFFFF_FDC0u32 & (1 << ((id >> 16) & 0xF)), 0);
    }

    /// `brcmf_chip_dmp_erom_scan`, transcribed: walk the table the chip
    /// offers and collect what the driver would.
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
            // The address descriptors: the plain slave first, then the
            // wrapper, both 4 KiB.
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
        // Chipcommon first: the driver takes the head of the list for it.
        assert_eq!(cores[0].0, CORE_CHIPCOMMON);
        assert_eq!(cores[0].2, ENUM_BASE);
        for id in [CORE_SDIO_DEV, CORE_ARM_CR4, CORE_80211] {
            assert!(cores.iter().any(|c| c.0 == id), "core {id:#x} missing");
        }
        // The SDIO core has to be rev 12 or better for the driver to take
        // its modern paths, and every core needs a wrapper to be reset
        // through.
        let sdio = cores.iter().find(|c| c.0 == CORE_SDIO_DEV).unwrap();
        assert!(sdio.1 >= 12, "sdio core rev {}", sdio.1);
        assert!(cores.iter().all(|c| c.3 != 0));
    }

    #[test]
    fn the_arm_core_sizes_the_memory_the_firmware_goes_in() {
        let mut chip = Cyw43455::new();
        let arm = CORES[ARM_CORE].base;
        // `brcmf_chip_tcm_ramsize`, transcribed.
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
        // Big enough for the firmware the card carries plus its nvram at the
        // top, and under the 4 MiB the driver refuses.
        assert!(size >= 609_309 + 2_074);
        assert!(size <= 4 * 1024 * 1024);
    }

    #[test]
    fn memory_written_through_the_window_reads_back() {
        let mut chip = Cyw43455::new();
        // A block that straddles two windows, as a firmware download does:
        // the driver re-aims the window every 32 KiB.
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
        // The top of memory, where the nvram goes.
        let top = RAM_BASE + RAM_SIZE - 4;
        writel(&mut chip, top, 0xDEAD_BEEF);
        assert_eq!(readl(&mut chip, top), 0xDEAD_BEEF);
        // ...and one byte past it is not memory any more.
        assert_eq!(readl(&mut chip, RAM_BASE + RAM_SIZE), 0);
    }

    #[test]
    fn the_clock_register_reads_back_what_was_asked_for() {
        let mut chip = Cyw43455::new();
        // `BRCMF_INIT_CLKCTL1` = FORCE_HW_CLKREQ_OFF | ALP_AVAIL_REQ, which
        // the driver then checks with `(clkctl & ~SBSDIO_AVBITS) == wrote`.
        let wrote = 0x28;
        chip.write_byte(1, F1_CHIPCLKCSR, wrote);
        let read = chip.read_byte(1, F1_CHIPCLKCSR);
        assert_eq!(read & !(CSR_ALP_AVAIL | CSR_HT_AVAIL), wrote);
        assert_ne!(read & CSR_ALP_AVAIL, 0, "ALP available");
        // Asking for HT gets both bits, which is what `SBSDIO_HTAV` wants.
        chip.write_byte(1, F1_CHIPCLKCSR, CSR_HT_AVAIL_REQ);
        let read = chip.read_byte(1, F1_CHIPCLKCSR);
        assert_eq!(read & (CSR_ALP_AVAIL | CSR_HT_AVAIL), 0xC0);
        // And asking for nothing gets nothing.
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

        // `brcmf_chip_disable_arm`: halt it and put it in reset.
        writel(&mut chip, wrap + BCMA_IOCTL, IOCTL_CPUHALT | BCMA_IOCTL_FGC);
        writel(&mut chip, wrap + BCMA_RESET_CTL, BCMA_RESET_CTL_RESET);
        assert_eq!(readl(&mut chip, wrap + BCMA_RESET_CTL), 1, "in reset");
        assert!(!chip.arm_running());

        // `brcmf_chip_ai_resetcore` with postreset 0: out of reset, clocked,
        // and no longer halted.
        writel(&mut chip, wrap + BCMA_RESET_CTL, 0);
        writel(&mut chip, wrap + BCMA_IOCTL, BCMA_IOCTL_CLK);
        assert!(chip.arm_running());
        // Which is also what `brcmf_chip_ai_iscoreup` asks.
        let ioctl = readl(&mut chip, wrap + BCMA_IOCTL);
        assert_eq!(ioctl & (BCMA_IOCTL_FGC | BCMA_IOCTL_CLK), BCMA_IOCTL_CLK);
    }

    #[test]
    fn nothing_reads_as_a_dead_bus() {
        let mut chip = Cyw43455::new();
        // A read that lands nowhere is zeros, not the all-ones the driver
        // takes for a failed access.
        for addr in [0u32, 0x1800_5000, 0x1810_5000, 0xFFFF_0000] {
            assert_eq!(readl(&mut chip, addr), 0, "at {addr:#x}");
        }
        // The reset vector the driver writes to backplane address 0 goes
        // nowhere and must not fault.
        writel(&mut chip, 0, 0x1234_5678);
    }
}
