//! The `Machine`: RAM + peripherals + address decode. Implements [`Bus`].

use crate::bus::{Bus, BusError, BusResult, MmioDevice, Width};
use crate::mem::Ram;
use crate::periph::{
    Aux, BootBox, ClockManager, ConfigOtp, CoreCtl, Dma4, Emmc2, Hvs, McSync, Pl011, Pm, Sdc,
    Sdramc, Spi0, StubRegion, SysTimer,
};
use crate::soc::bcm2711 as map;

/// Which UART the harness captures as "the console".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Console {
    #[default]
    Pl011,
    MiniUart,
}

pub struct Machine {
    pub ram: Ram,
    pub systimer: SysTimer,
    pub uart0: Pl011,
    pub aux: Aux,
    /// Inter-core sync block (`0x7E00_0000`) — stubbed as auto-acknowledged.
    pub mcsync: McSync,
    /// VPU core-control block (`0x7E00_2000`) — brings up VPU core 1.
    pub corectl: CoreCtl,
    /// Power-management block (`0x7E10_0000`) — SoC reset / watchdog.
    pub pm: Pm,
    /// Clock manager (`0x7E10_1000`) — PLL locks always report ready.
    pub clockman: ClockManager,
    /// SPI0 master (`0x7E20_4000`) — minimal model for the EEPROM bootloader.
    pub spi0: Spi0,
    /// Always-on config / OTP engine (`0x7E20_F000`) — board identity reads.
    pub config_otp: ConfigOtp,
    /// LPDDR4 controller + PHY (`0x7DC0_0000`, below the peripheral window) —
    /// the `init_sdram_*` training path drives this.
    pub sdramc: Sdramc,
    /// Legacy SDRAM-controller register interface (`0x7E00_1000`) — DRAM timing
    /// table plus lock/ready bits polled after PHY training.
    pub sdc: Sdc,
    /// Boot-info handoff doorbell (`0x7EE0_2000`).
    pub bootbox: BootBox,
    /// DMA4 channel (`0x7E00_7B00`) — the bootloader scrubs / moves DRAM through
    /// it; [`Machine::store`] runs the control-block chain after a `CS` write.
    pub dma4: Dma4,
    /// EMMC2 SD host controller (`0x7E34_0000`).
    pub emmc2: Emmc2,
    /// HVS (`0x7E40_0000`) — display frame-swap registers auto-complete.
    pub hvs: Hvs,
    /// Catch-all for the rest of the peripheral window.
    pub periph_stub: StubRegion,
    pub console: Console,

    /// Total peripheral accesses that missed a real device (fell through to the
    /// stub). A quick health signal for how much firmware behaviour is faked.
    pub stub_hits: u64,
    pub bus_errors: u64,
    /// Total store operations (any address). A liveness signal for the run loop:
    /// a loop that keeps writing memory is making progress, not spinning.
    pub ram_writes: u64,
    pub mmio_writes: u64,
    /// Loads that resolved to RAM. Lets the run loop tell a bounded memory scan
    /// (a DRAM memtest read-back walks fresh addresses, no writes) from a hung
    /// poll (re-reads one MMIO register forever).
    pub ram_reads: u64,

    /// When set, every peripheral (non-RAM) access is appended to `mmio_events`
    /// as `(addr, width_bytes, value, is_write)`. The run loop drains and prints
    /// it tagged with the current PC. A reconnaissance aid for unmodelled blocks.
    pub mmio_trace: bool,
    pub mmio_events: Vec<(u32, u8, u32, bool)>,

    /// `start4.elf` logs boot progress by writing 4-char ASCII tags (`_msh`,
    /// `_osh`, `bfsp`, ...) to a register at `0xCEC0_2000`. We capture the
    /// sequence — it is the closest thing to an early-boot log before any UART
    /// is up.
    pub phase_tags: Vec<u32>,
}

/// `start4.elf` writes 4-char boot-progress tags to `0x?EC0_2000`. Direct-ELF
/// load runs it at `0xCEC0_0000` (tags `0xCEC0_2000`); the real bootloader
/// relocates it to `0xFEC0_0000` (tags `0xFEC0_2000`). Both share the low-26-bit
/// signature `0x02C0_2000` (each `0x?C00_0000` alias is 64 MiB), so match on
/// that regardless of which alias the write used.
const PHASE_TAG_SIG: u32 = 0x02C0_2000;

impl Machine {
    pub fn new(ram_bytes: usize) -> Machine {
        Machine {
            ram: Ram::new(map::SDRAM_CACHED_BASE, ram_bytes),
            systimer: SysTimer::new(),
            uart0: Pl011::new(),
            aux: Aux::new(),
            mcsync: McSync::new(),
            corectl: CoreCtl::new(),
            pm: Pm::new(),
            clockman: ClockManager::new(),
            spi0: Spi0::new(),
            config_otp: ConfigOtp::new(),
            sdramc: Sdramc::new(),
            sdc: Sdc::new(),
            bootbox: BootBox::new(),
            dma4: Dma4::new(),
            emmc2: Emmc2::new(),
            hvs: Hvs::new(),
            periph_stub: StubRegion::new("periph-window"),
            console: Console::default(),
            stub_hits: 0,
            bus_errors: 0,
            ram_writes: 0,
            mmio_writes: 0,
            ram_reads: 0,
            mmio_trace: false,
            mmio_events: Vec::new(),
            phase_tags: Vec::new(),
        }
    }

    /// Advance time-based peripheral state by `cycles` VPU cycles.
    pub fn tick(&mut self, cycles: u64) {
        self.systimer.tick(cycles);
    }

    /// Drain and return whatever the console UART has transmitted.
    pub fn take_console_output(&mut self) -> Vec<u8> {
        match self.console {
            Console::Pl011 => self.uart0.take_output(),
            Console::MiniUart => self.aux.take_output(),
        }
    }

    /// Drain both UARTs regardless of console selection (for tracing).
    pub fn take_all_uart_output(&mut self) -> (Vec<u8>, Vec<u8>) {
        (self.uart0.take_output(), self.aux.take_output())
    }

    pub fn irq_pending(&self) -> bool {
        self.uart0.irq_pending() || self.aux.irq_pending()
    }

    /// The VPU addresses peripherals only through the `0x7E00_0000` window (no
    /// cache aliasing, unlike RAM) — plus the LPDDR4 controller/PHY, which is
    /// mapped just *below* that window at `0x7DC0_0000`. Both must be decoded
    /// before the cache-alias fold, or their (aliased) addresses land in DRAM.
    fn in_mmio(addr: u32) -> bool {
        (map::PERIPH_BASE..map::PERIPH_BASE + map::PERIPH_SIZE).contains(&addr)
            || (map::SDRAMC_BASE..map::SDRAMC_BASE + map::SDRAMC_SIZE).contains(&addr)
    }

    /// Fold the four VC4 cache aliases (`0x0`, `0x4000_0000`, `0x8000_0000`,
    /// `0xC000_0000`) of physical memory onto a single backing store. Before
    /// SDRAM training this backing *is* the ~128 KiB of L2-as-SRAM the bootcode
    /// runs from; afterwards it stands in for DRAM.
    fn fold_ram_addr(addr: u32) -> u32 {
        addr & 0x3FFF_FFFF
    }

    /// Resolve an address to `(device, offset)`, or `None` for RAM / unmapped.
    fn device_for(&mut self, addr: u32) -> Option<(&mut dyn MmioDevice, u32)> {
        let a = addr;
        let hit = |base: u32, size: u32| {
            if a >= base && a < base + size {
                Some(a - base)
            } else {
                None
            }
        };

        if let Some(off) = hit(map::SYSTIMER_BASE, map::SYSTIMER_SIZE) {
            return Some((&mut self.systimer, off));
        }
        if let Some(off) = hit(map::UART0_BASE, map::UART0_SIZE) {
            return Some((&mut self.uart0, off));
        }
        if let Some(off) = hit(map::AUX_BASE, map::AUX_SIZE) {
            return Some((&mut self.aux, off));
        }
        if let Some(off) = hit(map::MCSYNC_BASE, map::MCSYNC_SIZE) {
            return Some((&mut self.mcsync, off));
        }
        if let Some(off) = hit(map::CORECTL_BASE, map::CORECTL_SIZE) {
            return Some((&mut self.corectl, off));
        }
        if let Some(off) = hit(map::SDC_BASE, map::SDC_SIZE) {
            return Some((&mut self.sdc, off));
        }
        if let Some(off) = hit(map::DMA4_BASE, map::DMA4_SIZE) {
            return Some((&mut self.dma4, off));
        }
        if let Some(off) = hit(map::EMMC2_BASE, map::EMMC2_SIZE) {
            return Some((&mut self.emmc2, off));
        }
        if let Some(off) = hit(map::HVS_BASE, map::HVS_SIZE) {
            return Some((&mut self.hvs, off));
        }
        if let Some(off) = hit(map::BOOTBOX_BASE, map::BOOTBOX_SIZE) {
            return Some((&mut self.bootbox, off));
        }
        if let Some(off) = hit(map::PM_BASE, map::PM_SIZE) {
            return Some((&mut self.pm, off));
        }
        if let Some(off) = hit(map::CM_BASE, map::CM_SIZE) {
            return Some((&mut self.clockman, off));
        }
        if let Some(off) = hit(map::SPI0_BASE, map::SPI0_SIZE) {
            return Some((&mut self.spi0, off));
        }
        if let Some(off) = hit(map::FIFO_STUB_BASE, map::FIFO_STUB_SIZE) {
            return Some((&mut self.config_otp, off));
        }
        if let Some(off) = hit(map::SDRAMC_BASE, map::SDRAMC_SIZE) {
            return Some((&mut self.sdramc, off));
        }

        if (map::PERIPH_BASE..map::PERIPH_BASE + map::PERIPH_SIZE).contains(&a) {
            self.stub_hits += 1;
            return Some((&mut self.periph_stub, a - map::PERIPH_BASE));
        }
        None
    }

    /// Run the DMA4 control-block chain the channel was just armed with. A word
    /// in RAM (`+0x08` `SRCI` bit 12 = "source increments"): clear ⇒ fill `DEST`
    /// with the single word at `SRC` (`SRC == 0` ⇒ zero-fill scrub); set ⇒ copy
    /// `SRC`→`DEST`.
    fn run_dma4(&mut self) {
        const S_INC: u32 = 1 << 12;
        let rd = |ram: &Ram, addr: u32| ram.load(addr & 0x3FFF_FFFF, Width::Word).unwrap_or(0);

        let mut cb = self.dma4.cb_addr() & 0x3FFF_FFFF;
        for _ in 0..4096 {
            if cb == 0 || !self.ram.contains(cb) {
                break;
            }
            let src = rd(&self.ram, cb + 0x04);
            let srci = rd(&self.ram, cb + 0x08);
            let dest = rd(&self.ram, cb + 0x0C);
            let len = rd(&self.ram, cb + 0x14);
            let next = rd(&self.ram, cb + 0x18);

            let fill = srci & S_INC == 0;
            let fill_word = if src == 0 { 0 } else { rd(&self.ram, src) };
            let mut off = 0u32;
            while off < len {
                let v = if fill {
                    fill_word
                } else {
                    rd(&self.ram, src.wrapping_add(off))
                };
                let _ = self
                    .ram
                    .store(dest.wrapping_add(off) & 0x3FFF_FFFF, Width::Word, v);
                off = off.wrapping_add(4);
            }
            cb = (next << 5) & 0x3FFF_FFFF;
        }
        self.dma4.finish();
    }
}

impl Machine {
    /// `sleep` support: jump to the next armed system-timer compare, fire it, and
    /// return the interrupt vector-table slot if the firmware has enabled that
    /// timer source (`enable_irq_source(SYS_IRQ_SRC + channel, prio)`).
    fn timer_wake_impl(&mut self) -> Option<u32> {
        let ch = self.systimer.wake_to_next_match()?;
        let src = crate::periph::corectl::SYS_IRQ_SRC + ch as u32;
        let prio = self.corectl.irq_priority(src);
        if prio == 0 {
            None
        } else {
            Some(prio as u32)
        }
    }
}

impl Bus for Machine {
    fn timer_wake(&mut self) -> Option<u32> {
        self.timer_wake_impl()
    }

    fn load(&mut self, addr: u32, width: Width) -> BusResult<u32> {
        if !Machine::in_mmio(addr) {
            let phys = Machine::fold_ram_addr(addr);
            if self.ram.contains(phys) {
                self.ram_reads = self.ram_reads.wrapping_add(1);
                return self.ram.load(phys, width);
            }
        }
        let trace = self.mmio_trace;
        if let Some((dev, off)) = self.device_for(addr) {
            let v = dev.read(off, width);
            let got = *v.as_ref().unwrap_or(&0);
            if trace {
                self.mmio_events
                    .push((addr, width.bytes() as u8, got, false));
            }
            return v;
        }
        self.bus_errors += 1;
        Err(BusError::Unmapped {
            addr,
            width,
            write: false,
        })
    }

    fn store(&mut self, addr: u32, width: Width, value: u32) -> BusResult<()> {
        self.ram_writes = self.ram_writes.wrapping_add(1);
        if !Machine::in_mmio(addr) {
            let phys = Machine::fold_ram_addr(addr);
            if self.ram.contains(phys) {
                if addr & 0x03FF_FFFF == PHASE_TAG_SIG && width == Width::Word {
                    if self.phase_tags.last() != Some(&value) {
                        self.phase_tags.push(value);
                    }
                }
                return self.ram.store(phys, width, value);
            }
        }
        self.mmio_writes = self.mmio_writes.wrapping_add(1);
        let trace = self.mmio_trace;
        if let Some((dev, off)) = self.device_for(addr) {
            let r = dev.write(off, width, value);
            if trace {
                self.mmio_events
                    .push((addr, width.bytes() as u8, value, true));
            }
            if self.dma4.take_start() {
                self.run_dma4();
            }
            return r;
        }
        self.mmio_writes = self.mmio_writes.wrapping_sub(1);
        self.bus_errors += 1;
        Err(BusError::Unmapped {
            addr,
            width,
            write: true,
        })
    }
}
