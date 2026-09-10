//! The `Machine`: RAM + peripherals + address decode. Implements [`Bus`].

use crate::bus::{Bus, BusError, BusResult, MmioDevice, Width};
use crate::mem::Ram;
use crate::periph::{
    Aux, BootBox, Bsc, ClkMon, ClockManager, ConfigOtp, CoreCtl, Dma4, Emmc2, Hvs, McSync, Pl011,
    Pm, Sdc, Sdramc, Spi0, StubRegion, SysTimer,
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
    /// VPU clock block (`0x7D5D_0000`) — PLLs + frequency monitors.
    pub clkmon: ClkMon,
    /// SPI0 master (`0x7E20_4000`) — minimal model for the EEPROM bootloader.
    pub spi0: Spi0,
    /// BSC / I²C master at `0x7E20_5E00` + the board PMIC — start4 reads the
    /// PMIC over this on its way to bringing up the "external" GPIO pins.
    pub bsc_pmic: Bsc,
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
    /// Legacy DMA controller (`0x7E00_7000`) — start4's bulk memory copies.
    pub dma_legacy: crate::periph::dma_legacy::DmaLegacy,
    /// The `0x7EE0_4100` DMA controller (channel 15 at `0x7EE0_5000`).
    pub dma_vpu: crate::periph::dma_legacy::DmaLegacy,
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

    /// Reconnaissance aid: when `RVF_WATCH=<hex>[,<hex>...]` is set, every store
    /// whose word-aligned address matches one of them is logged to stderr tagged
    /// with the current PC (`watch_pc`, refreshed by the run loop each step).
    /// Complements `mmio_trace` for pinning down who writes a given RAM word.
    pub watch: Vec<u32>,
    /// Cached `RVF_TICK_SLOT` / `RVF_IRQ_SLOT` overrides. `timer_tick_slot` runs
    /// on every `sleep` (millions of times), so these must not re-read the
    /// environment per call.
    tick_slot_override: Option<u32>,
    /// `RVF_DBG_DMA=1`: log every control block the DMA4 channel executes.
    dbg_dma: bool,
    /// Interrupt sources raised by peripherals, waiting to be vectored.
    pending_irqs: std::collections::VecDeque<u32>,
    irq_slot_override: Option<u32>,
    pub watch_pc: u32,

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
            clkmon: ClkMon::new(),
            spi0: Spi0::new(),
            bsc_pmic: Bsc::new("bsc-pmic"),
            config_otp: ConfigOtp::new(),
            sdramc: Sdramc::new(),
            sdc: Sdc::new(),
            bootbox: BootBox::new(),
            dma4: Dma4::new(),
            dma_legacy: crate::periph::dma_legacy::DmaLegacy::new(),
            dma_vpu: crate::periph::dma_legacy::DmaLegacy::new_vpu(),
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
            watch: std::env::var("RVF_WATCH")
                .ok()
                .map(|v| {
                    v.split(',')
                        .filter_map(|t| {
                            u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok()
                        })
                        .map(|a| a & !3)
                        .collect()
                })
                .unwrap_or_default(),
            dbg_dma: std::env::var_os("RVF_DBG_DMA").is_some(),
            pending_irqs: std::collections::VecDeque::new(),
            tick_slot_override: std::env::var("RVF_TICK_SLOT")
                .ok()
                .and_then(|s| s.trim().parse().ok()),
            irq_slot_override: std::env::var("RVF_IRQ_SLOT")
                .ok()
                .and_then(|s| s.trim().parse().ok()),
            watch_pc: 0,
            phase_tags: Vec::new(),
        }
    }

    /// Advance time-based peripheral state by `cycles` VPU cycles.
    pub fn tick(&mut self, cycles: u64) {
        self.systimer.tick(cycles);
        self.bsc_pmic.tick(cycles);
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
            || (map::CLKMON_BASE..map::CLKMON_BASE + map::CLKMON_SIZE).contains(&addr)
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
        if let Some(off) = hit(map::DMA_LEGACY_BASE, map::DMA_LEGACY_SIZE) {
            return Some((&mut self.dma_legacy, off));
        }
        if let Some(off) = hit(map::DMA_VPU_BASE, map::DMA_VPU_SIZE) {
            return Some((&mut self.dma_vpu, off));
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
        if let Some(off) = hit(map::CLKMON_BASE, map::CLKMON_SIZE) {
            return Some((&mut self.clkmon, off));
        }
        if let Some(off) = hit(map::SPI0_BASE, map::SPI0_SIZE) {
            return Some((&mut self.spi0, off));
        }
        if let Some(off) = hit(map::BSC_PMIC_BASE, map::BSC_PMIC_SIZE) {
            return Some((&mut self.bsc_pmic, off));
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
    /// `RVF_DBG_DMA=1`: trace every access to the legacy DMA controller window
    /// (`0x7E00_7000..0x7E00_8000`, 15 channels x 0x100). Only channel 11
    /// (DMA4, `0x7E00_7B00`) is modelled; start4's `dma_memcpy` uses one of the
    /// others, so those accesses currently fall through to the catch-all stub.
    fn dma_win_log(&self, rw: &str, addr: u32, value: u32) {
        if self.dbg_dma && (0x7E00_7000..0x7E00_8000).contains(&addr) {
            let ch = (addr - 0x7E00_7000) / 0x100;
            eprintln!(
                "[dmawin] {rw} ch{ch} +{:#04x} ({addr:#x}) = {value:#x} pc={:#x}",
                (addr - 0x7E00_7000) % 0x100,
                self.watch_pc
            );
        }
    }

    /// Execute the control-block chain armed on legacy DMA channel `ch`.
    ///
    /// CB layout (32 bytes): `+0x00 TI  +0x04 SOURCE_AD  +0x08 DEST_AD
    /// +0x0C TXFR_LEN  +0x10 STRIDE  +0x14 NEXTCONBK`. Bus addresses are folded
    /// onto flat DRAM the same way the CPU's are.
    fn run_dma_legacy(&mut self, ch: usize, vpu: bool) {
        use crate::periph::dma_legacy::DmaLegacy;

        let raw = if vpu {
            self.dma_vpu.conblk_ad(ch)
        } else {
            self.dma_legacy.conblk_ad(ch)
        };
        // `dma_chain_start` (`0x3EC97544`) writes `CONBLK_AD` two ways:
        //
        //   legacy channel: *(base + ch*0x100 + 4) = cb            (raw pointer)
        //   40-bit channel: cb>>30 == 3 ? (cb & 0x3fffffff) >> 5
        //                               : (cb >> 5) | 0x20000000
        //
        // Both shifted forms leave the top two bits clear, and a raw VC4
        // pointer always has an alias in them, so that is the discriminator.
        // Shifting back by 5 restores the alias bits for the `| 0x20000000`
        // form (0x25F7B6A5 << 5 == 0xBEF6D4A0).
        let cb_addr = if raw >> 30 != 0 { raw } else { raw << 5 };
        let mut cb = cb_addr & 0x3FFF_FFFF;
        for _ in 0..4096 {
            if cb == 0 || !self.ram.contains(cb) {
                break;
            }
            let mut w = [0u32; 7];
            for (i, slot) in w.iter_mut().enumerate() {
                *slot = self
                    .ram
                    .load(cb + (i as u32) * 4, Width::Word)
                    .unwrap_or(0);
            }
            // Channel 15 of the `0x7EE0_4100` controller is a 40-bit ("dma40")
            // channel - `dma_memcpy` builds its CB with
            // `dma_transfer_setup_memcpy_vpu40` (`0x3EC99EE0`) - so it uses the
            // DMA4 control-block layout, not the legacy one:
            //   +0x00 TI  +0x04 SRC  +0x08 SRCI  +0x0C DEST  +0x10 DESTI
            //   +0x14 LEN +0x18 NEXT_CB(>>5)
            let d = if vpu {
                crate::periph::dma_legacy::Cb {
                    ti: w[0],
                    src: w[1],
                    dest: w[3],
                    len: w[5],
                    stride: 0,
                    next: w[6] << 5,
                }
            } else {
                DmaLegacy::decode_cb([w[0], w[1], w[2], w[3], w[4], w[5]])
            };
            if self.dbg_dma {
                eprintln!(
                    "[dma-legacy] ch{ch} cb={cb:#x} ti={:#x} src={:#x} dest={:#x} len={:#x} stride={:#x} next={:#x}",
                    d.ti, d.src, d.dest, d.len, d.stride, d.next
                );
            }
            let (rows, xlen) = d.rows();
            let (src_stride, dest_stride) = d.strides();
            let mut src = d.src & 0x3FFF_FFFF;
            let mut dest = d.dest & 0x3FFF_FFFF;
            for _ in 0..rows {
                for i in 0..xlen {
                    let sa = if vpu || d.src_inc() {
                        src.wrapping_add(i)
                    } else {
                        src
                    };
                    let da = if vpu || d.dest_inc() {
                        dest.wrapping_add(i)
                    } else {
                        dest
                    };
                    let b = self.ram.load(sa & 0x3FFF_FFFF, Width::Byte).unwrap_or(0);
                    let _ = self.ram.store(da & 0x3FFF_FFFF, Width::Byte, b);
                }
                // 2D mode advances by the row length plus the signed stride.
                src = src
                    .wrapping_add(xlen)
                    .wrapping_add(src_stride as u32);
                dest = dest
                    .wrapping_add(xlen)
                    .wrapping_add(dest_stride as u32);
            }
            cb = d.next & 0x3FFF_FFFF;
        }
        if vpu {
            self.dma_vpu.finish(ch);
        } else {
            self.dma_legacy.finish(ch);
        }
        // Completion interrupt. `dma_interrupt` (`0x3EC980E8`) asserts its
        // argument is in `0x50..=0x5F` and maps it back to a channel, and the
        // firmware's own poll loop calls it as `dma_interrupt(channel + 0x50)`
        // (channels 11..14 fold to 0xB, >= 15 to 0xF). So the source is
        // `0x50 + channel`, i.e. 95 for channel 15 - which is one of the
        // sources `enable_irq_source` turns on. `dma_chan_interrupt` then runs,
        // retires the transfer, signals its waiter and starts the next one in
        // the queue.
        {
            let folded = if ch >= 15 {
                0xF
            } else if ch > 10 {
                0xB
            } else {
                ch
            };
            let src = 0x50 + folded as u32;
            self.corectl.raise_source(src);
            self.pending_irqs.push_back(src);
        }
    }

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

            if self.dbg_dma {
                eprintln!(
                    "[dma] cb={cb:#x} ti={:#x} src={src:#x} srci={srci:#x} dest={dest:#x} len={len:#x} next={next:#x}",
                    rd(&self.ram, cb)
                );
            }
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
        // The caller (`Op::Sleep`) only asks while not already in an exception,
        // so the previous tick's handler has returned. The slot-1 ThreadX tick
        // ISR acks the BCM system timer directly (writes the CS match bit at
        // `0x3ED65846`), never the `0x7EE0_1080` VPU-dispatch window — so gating
        // on `bootbox.irq_pending()` here wedged the tick after the first one.
        let _ch = self.systimer.wake_to_next_match()?;
        // start4 routes the ThreadX tick to interrupt source 66 (its handler at
        // 0x3ED6583A acks system-timer CS and calls the tick with source id 66).
        let src = crate::periph::corectl::SYS_IRQ_SRC + 2;
        // `enable_irq_source(src, prio)` stores the 4-bit priority, and start4's
        // exception entry vectors the interrupt through the table slot == that
        // priority. `enable_irq_source(66, 1)` ⇒ slot 1, whose ISR path runs
        // the plain ThreadX tick (`0x3ED65142`) and returns cleanly.
        let slot = self.corectl.irq_priority(src);
        if slot == 0 {
            return None;
        }
        self.bootbox.raise_irq(src, 0);
        // `RVF_IRQ_SLOT` overrides for experiments (slots 3/10 force the ISR's
        // deferred-reschedule path `0x3EDA2594` instead of the plain tick).
        Some(self.irq_slot_override.unwrap_or(slot as u32))
    }
}

impl Bus for Machine {
    fn timer_wake(&mut self) -> Option<u32> {
        self.timer_wake_impl()
    }

    fn take_pending_irq(&mut self) -> Option<u32> {
        self.pending_irqs.pop_front()
    }

    fn take_tick_pending(&mut self) -> bool {
        self.systimer.take_tick_pending()
    }

    fn timer_tick_slot(&mut self) -> Option<u32> {
        // Each system-timer compare channel is its own VPU interrupt source
        // (`SYS_IRQ_SRC + channel`), and start4's vector table has 128 entries /
        // 0x200 bytes: [0..15] are the per-priority stubs, [64..127] the direct
        // per-source handlers. Entry 64 is the ThreadX tick `0x3EC40B7C` (read
        // back off a running Pi 4, issue #7) — the only code that acks `CS.M0`,
        // re-arms `C0`, walks the 32-bucket timer wheel and calls
        // `_tx_thread_system_resume`. Entry 66 is the clock service's timeout
        // timer, which reaches `0x3ED6588A` through the generic dispatcher.
        //
        // So deliver the raw source, not the 4-bit priority
        // `enable_irq_source(64, prio)` recorded. Routing through the priority
        // stub reached `0x3ECB0BF0(slot)`, which does nothing for slot 1, so no
        // timed wait ever expired and every blocking `msleep` hung forever.
        //
        // A channel that has already matched is what we are delivering, and
        // under one-shot compares it disarms at the moment it fires — so check
        // "something is pending" first and only fall back to "something is
        // armed". Testing `any_armed()` alone dropped every one-shot match.
        if self.systimer.pending_channel().is_none() && !self.systimer.any_armed() {
            return None;
        }
        let ch = self.systimer.pending_channel().unwrap_or(0) as u32;
        let src = self
            .tick_slot_override
            .map_or(crate::periph::corectl::SYS_IRQ_SRC + ch, |base| {
                if base == crate::periph::corectl::SYS_IRQ_SRC {
                    base + ch
                } else {
                    base
                }
            });
        // The generic dispatcher `0x3EC3E9BC` does not take the source from the
        // vector number — it re-reads it from CoreCtl `+0x04` and indexes the
        // handler table at `gp+58004`. Entry 64 is a direct handler and never
        // looks, so leaving a stale pending value there would only shadow a
        // device interrupt's own source.
        if src != crate::periph::corectl::SYS_IRQ_SRC {
            self.corectl.raise_source(src);
        }
        Some(src)
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
            self.dma_win_log("rd", addr, got);
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
        if !self.watch.is_empty() {
            let a = Machine::fold_ram_addr(addr) & !3;
            if self.watch.iter().any(|&w| Machine::fold_ram_addr(w) & !3 == a) {
                eprintln!(
                    "[watch] pc={:#010x} store{} {:#010x} <- {:#x}",
                    self.watch_pc,
                    width.bytes() * 8,
                    addr,
                    value
                );
            }
        }
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
        self.dma_win_log("wr", addr, value);
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
            if let Some(ch) = self.dma_legacy.take_start() {
                self.run_dma_legacy(ch, false);
            }
            if let Some(ch) = self.dma_vpu.take_start() {
                self.run_dma_legacy(ch, true);
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
