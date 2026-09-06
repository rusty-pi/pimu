//! The `Machine`: RAM + peripherals + address decode. Implements [`Bus`].

use crate::bus::{Bus, BusError, BusResult, MmioDevice, Width};
use crate::mem::Ram;
use crate::periph::{Aux, Pl011, StubRegion, SysTimer};
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
    /// Catch-all for the rest of the peripheral window.
    pub periph_stub: StubRegion,
    pub console: Console,

    /// Total peripheral accesses that missed a real device (fell through to the
    /// stub). A quick health signal for how much firmware behaviour is faked.
    pub stub_hits: u64,
    pub bus_errors: u64,
}

impl Machine {
    pub fn new(ram_bytes: usize) -> Machine {
        Machine {
            ram: Ram::new(map::SDRAM_CACHED_BASE, ram_bytes),
            systimer: SysTimer::new(),
            uart0: Pl011::new(),
            aux: Aux::new(),
            periph_stub: StubRegion::new("periph-window"),
            console: Console::default(),
            stub_hits: 0,
            bus_errors: 0,
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

    /// Resolve an address to `(device, offset)`, or `None` for RAM / unmapped.
    fn device_for(&mut self, addr: u32) -> Option<(&mut dyn MmioDevice, u32)> {
        let hit = |base: u32, size: u32| {
            if addr >= base && addr < base + size {
                Some(addr - base)
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

        // Anything else inside the peripheral window -> stub (offset kept
        // absolute-within-window so the log is meaningful).
        if (map::PERIPH_BASE..map::PERIPH_BASE + map::PERIPH_SIZE).contains(&addr) {
            self.stub_hits += 1;
            return Some((&mut self.periph_stub, addr - map::PERIPH_BASE));
        }
        None
    }
}

impl Bus for Machine {
    fn load(&mut self, addr: u32, width: Width) -> BusResult<u32> {
        if self.ram.contains(addr) {
            return self.ram.load(addr, width);
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
        if self.ram.contains(addr) {
            return self.ram.store(addr, width, value);
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
