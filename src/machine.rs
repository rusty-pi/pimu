//! The `Machine`: RAM + peripherals + address decode. Implements [`Bus`].

use crate::bus::{Bus, BusError, BusResult, MmioDevice, Width};
use crate::mem::Ram;
use crate::periph::{Aux, CoreCtl, McSync, Pl011, ReadyStub, Spi0, StubRegion, SysTimer};
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
    /// SPI0 master (`0x7E20_4000`) — minimal model for the EEPROM bootloader.
    pub spi0: Spi0,
    /// "Always ready" stub for the EEPROM bootloader's FIFO at 0x7E20_F000.
    pub fifo_stub: ReadyStub,
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

    /// `start4.elf` logs boot progress by writing 4-char ASCII tags (`_msh`,
    /// `_osh`, `bfsp`, ...) to a register at `0xCEC0_2000`. We capture the
    /// sequence — it is the closest thing to an early-boot log before any UART
    /// is up.
    pub phase_tags: Vec<u32>,
}

/// Folded address of the `start4.elf` boot-progress register (`0xCEC0_2000`).
const PHASE_TAG_ADDR: u32 = 0xCEC0_2000 & 0x3FFF_FFFF;

impl Machine {
    pub fn new(ram_bytes: usize) -> Machine {
        Machine {
            ram: Ram::new(map::SDRAM_CACHED_BASE, ram_bytes),
            systimer: SysTimer::new(),
            uart0: Pl011::new(),
            aux: Aux::new(),
            mcsync: McSync::new(),
            corectl: CoreCtl::new(),
            spi0: Spi0::new(),
            fifo_stub: ReadyStub::new("fifo-stub"),
            periph_stub: StubRegion::new("periph-window"),
            console: Console::default(),
            stub_hits: 0,
            bus_errors: 0,
            ram_writes: 0,
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
        self.uart0.irq_pending() || self.aux.irq_pending() || self.systimer.irq_pending()
    }

    /// The VPU addresses peripherals only through the `0x7E00_0000` window (no
    /// cache aliasing, unlike RAM).
    fn in_periph_window(addr: u32) -> bool {
        (map::PERIPH_BASE..map::PERIPH_BASE + map::PERIPH_SIZE).contains(&addr)
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
        if let Some(off) = hit(map::SPI0_BASE, map::SPI0_SIZE) {
            return Some((&mut self.spi0, off));
        }
        if let Some(off) = hit(map::FIFO_STUB_BASE, map::FIFO_STUB_SIZE) {
            return Some((&mut self.fifo_stub, off));
        }

        if (map::PERIPH_BASE..map::PERIPH_BASE + map::PERIPH_SIZE).contains(&a) {
            self.stub_hits += 1;
            return Some((&mut self.periph_stub, a - map::PERIPH_BASE));
        }
        None
    }
}

impl Bus for Machine {
    fn load(&mut self, addr: u32, width: Width) -> BusResult<u32> {
        if !Machine::in_periph_window(addr) {
            let phys = Machine::fold_ram_addr(addr);
            if self.ram.contains(phys) {
                return self.ram.load(phys, width);
            }
        }
        if let Some((dev, off)) = self.device_for(addr) {
            return dev.read(off, width);
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
        if !Machine::in_periph_window(addr) {
            let phys = Machine::fold_ram_addr(addr);
            if self.ram.contains(phys) {
                if phys == PHASE_TAG_ADDR && width == Width::Word {
                    if self.phase_tags.last() != Some(&value) {
                        self.phase_tags.push(value);
                    }
                }
                return self.ram.store(phys, width, value);
            }
        }
        if let Some((dev, off)) = self.device_for(addr) {
            return dev.write(off, width, value);
        }
        self.bus_errors += 1;
        Err(BusError::Unmapped {
            addr,
            width,
            write: true,
        })
    }
}
