//! The `Machine`: RAM + peripherals + address decode. Implements [`Bus`].

use crate::bus::{Bus, BusError, BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};
use crate::mem::Ram;
use crate::periph::gpio;
use crate::periph::hdmi_ddc::AUTO_WINDOW;
use crate::periph::{
    ArmCtrl, ArmLocal, Asb, Aux, Avs, Bell, BootBox, Bsc, ClkMon, ClockManager, ConfigOtp, CoreCtl,
    Dma4, Dwc2, Emmc2, Gic, Gpio, Hdmi, HdmiDdc, Hvs, Mbox, McSync, Pactl, Pcm, Pl011, Pm, Pwm,
    Rng, Sdc, Sdramc, Spi0, StubRegion, SysTimer, Usbr, Vce, XhciOtg,
};
use crate::soc::bcm2711 as map;

/// Which UART the harness captures as "the console".
///
/// The serial header is GPIO 14/15, driven by the PL011 on ALT0 and the
/// mini-UART on ALT5, and a single boot can move between them. [`Console::Pins`]
/// follows the pins and is what a firmware boot wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Console {
    /// Whichever block GPIO 14/15 carry; the PL011 until something says otherwise.
    #[default]
    Pins,
    Pl011,
    MiniUart,
}

pub struct Machine {
    pub ram: Ram,
    /// A real BCM2711 maskROM image overlaid read-only at `0x6000_0000`
    /// (`--maskrom`). Only the code+rodata region: the ROM's scratch above it and
    /// its staging at `0x8000_0000` must stay writable DRAM.
    maskrom: Option<(u32, u32, Vec<u8>)>,
    /// The L2 the bootcode runs out of, until its flush ([`crate::l2`]).
    pub l2: crate::l2::CacheAsRam,
    pub systimer: SysTimer,
    pub uart0: Pl011,
    pub aux: Aux,
    /// The ARM property mailbox, idle until an ARM is running.
    pub mbox: Mbox,
    /// The four ARM <-> VideoCore doorbells, VCHIQ's wake path. Their apertures sit
    /// inside the mailbox and ARM-control windows, so they are decoded ahead of both.
    pub bell: Bell,
    pub armctrl: ArmCtrl,
    /// Reached from [`crate::arm`], never from the VPU.
    pub arm_local: ArmLocal,
    pub gic: Gic,
    pub mcsync: McSync,
    pub corectl: CoreCtl,
    pub pm: Pm,
    pub clockman: ClockManager,
    pub clkmon: ClkMon,
    pub avs: Avs,
    pub pvt: crate::periph::Pvt,
    /// The stop/acknowledge handshake start4 runs before gating the V3D / ISP /
    /// H264 power domains.
    pub asb: Asb,
    /// PCIe root complex. The VL805 xHCI behind it is [`crate::periph::Vl805`],
    /// reached through `EXT_CFG_DATA` and the outbound window.
    pub pcie: crate::periph::pcie::Pcie,
    /// GENET v5 Ethernet MAC with a BCM54213PE PHY on its MDIO bus.
    pub genet: crate::periph::Genet,
    /// What GENET's cable is plugged into, if anything ([`Machine::attach_net`]).
    pub net: Option<Box<dyn crate::net::NetBackend>>,
    pub rng: Rng,
    /// The codec licence check launches a program on it and waits for source 68.
    pub vce: Vce,
    pub bsc0: Bsc,
    pub gpio: Gpio,
    pub spi0: Spi0,
    pub pactl: Pactl,
    /// Register maps only, with no output and empty FIFOs; no boot programs them.
    pub pwm0: Pwm,
    pub pwm1: Pwm,
    pub pcm: Pcm,
    /// The I²C master the board PMICs are on.
    pub bsc_pmic: Bsc,
    /// The HDMI connectors' DDC masters, with no monitor on either.
    pub hdmi_ddc0: HdmiDdc,
    pub hdmi_ddc1: HdmiDdc,
    /// The packet RAM start4 sends AV mute through when it stops its display.
    pub hdmi0: Hdmi,
    pub hdmi1: Hdmi,
    /// Which board and BCM2711 stepping this is. Changed only through
    /// [`Machine::set_board`], which keeps OTP row 30 in step.
    board: crate::soc::Board,
    pub config_otp: ConfigOtp,
    /// LPDDR4 controller + PHY, driven by the `init_sdram_*` path.
    pub sdramc: Sdramc,
    /// Timing table plus the lock/ready bits polled after PHY training.
    pub sdc: Sdc,
    pub bootbox: BootBox,
    /// Unaligned scalar VPU accesses, which the model performs and the core
    /// cannot (`--check-alignment`).
    pub alignment: crate::align::Alignment,
    pub dma_legacy: crate::periph::dma_legacy::DmaLegacy,
    /// The `0x7EE0_4100` controller; its channel 15 is at `0x7EE0_5000`.
    pub dma_vpu: crate::periph::dma_legacy::DmaLegacy,
    /// [`Machine::store`] runs its control-block chain after a `CS` write.
    pub dma4: Dma4,
    /// The CYW43455's SDIO host, and the SD host of 2020-era bootcode while
    /// [`Self::sd_slot_legacy`] routes the card there.
    pub emmc: Emmc2,
    /// The WiFi chip, parked while the SD slot has its host.
    wifi: Option<crate::periph::sdcard::SdCard>,
    /// Bit 1 of the SD-slot mux word: the card is on the legacy EMMC rather than
    /// EMMC2. See [`Self::route_sd_slot`].
    pub sd_slot_legacy: bool,
    pub emmc2: Emmc2,
    pub hvs: Hvs,
    /// Carries the power acknowledge start4's USB power-on waits for.
    pub usbr: Usbr,
    pub dwc2: Dwc2,
    /// The USB-C port as a USB 2.0 host, which `--otg` plugs a stick into.
    pub xhci_otg: XhciOtg,
    pub periph_stub: StubRegion,
    /// The Bluetooth modem. It hears the port only while the serial header is on
    /// the mini-UART, since the PL011's pins are then GPIO 30..33, the modem's.
    pub bluetooth: crate::periph::bluetooth::BtModem,
    pub console: Console,
    /// Which UART [`Self::take_console_output`] last drained, so a change of pins
    /// is reported once.
    console_routed: Console,

    /// Peripheral accesses that missed a real device: a health signal for how
    /// much firmware behaviour is faked.
    pub stub_hits: u64,
    pub bus_errors: u64,
    /// Total stores — a loop that keeps writing memory is making progress.
    pub ram_writes: u64,
    pub mmio_writes: u64,
    /// Loads that resolved to RAM, which tells a bounded memory scan from a hung
    /// poll that re-reads one register forever.
    pub ram_reads: u64,
    /// Loads off the RAM path. The busy-wait detector compares it with the
    /// timer's `clo_reads`: a `udelay` reads nothing else.
    pub mmio_reads: u64,

    /// Record every peripheral access in `mmio_events` as
    /// `(addr, width_bytes, value, is_write)` for the run loop to print.
    pub mmio_trace: bool,
    /// Optional `[lo, hi)` filter for it (`PIMU_TRACE_MMIO=<lo>-<hi>`), without
    /// which one block's accesses are buried in a boot's millions.
    pub mmio_trace_range: Option<(u32, u32)>,
    pub mmio_events: Vec<(u32, u8, u32, bool)>,

    /// `PIMU_WATCH=<hex>[,<hex>...]`: log every store to one of these
    /// word-aligned addresses, tagged with `watch_pc`.
    pub watch: Vec<u32>,
    /// Where the machine's channels go; devices get a clone ([`Self::set_log`]).
    pub log: Log,
    /// Sources raised by peripherals. One waits here until core 0's CoreCtl bank
    /// enables it; `pending_irqs1` is the same for core 1.
    pending_irqs: std::collections::VecDeque<u32>,
    pending_irqs1: std::collections::VecDeque<u32>,
    /// Something happened the run loop's per-step checks may have to act on.
    /// While it stays clear those checks have nothing to do
    /// (`Emulator::fast_steps`); `Vpu::recheck` is the core's own such flag.
    pub recheck: bool,
    /// With the ARM running, a VPU `sleep` leaves its jump in [`Self::sleep_to`]
    /// for `Emulator::step_arm`, which moves the counter only as far as the first
    /// ARM write the VPU would wake for.
    pub defer_sleep: bool,
    pub sleep_to: Option<u64>,
    pub watch_pc: u32,
    /// Which VPU core `watch_pc` belongs to.
    pub watch_core: u32,

    /// `start4.elf` boot-progress tags (4-char ASCII, `_msh`, `bfsp`, ...), the
    /// closest thing to a log before any UART is up. Only a `--features diag` build
    /// collects them: outside diagnostics the model knows nothing about start4.
    pub phase_tags: Vec<u32>,
}

/// start4 runs at `0xCEC0_0000` from a direct-ELF load and `0xFEC0_0000` from the
/// bootloader; both tag writes share this low-26-bit signature.
const PHASE_TAG_SIG: u32 = 0x02C0_2000;

/// The GPIO function-select registers: a write can move a peripheral's pads
/// ([`Machine::route_gpio_pins`]).
const GPFSEL_WINDOW: std::ops::Range<u32> = {
    let base = map::GPIO_BASE + crate::spec::gpio::GPFSEL;
    base..base + crate::spec::gpio::GPFSEL_COUNT * crate::spec::gpio::GPFSEL_STRIDE
};

/// The address the Bluetooth modem comes up holding. This is the chip's own, not
/// the board's — the firmware derives that from the fuses and the host programs
/// it in at attach — so nothing here derives it: it is an invented address from
/// the RFC 7042 §2.1.2 documentation range, one past the modelled OTP MAC.
const BT_ADDRESS: [u8; 6] = [0x02, 0x00, 0x5E, 0x00, 0x53, 0x02];

/// The SD-slot mux word, and the bit that routes the card to the legacy EMMC
/// ([`Machine::route_sd_slot`]).
const SD_SLOT_MUX: u32 = map::GPIO_BASE + crate::spec::gpio::PIN_MUX;
const SD_SLOT_MUX_LEGACY: u32 = crate::spec::gpio::PIN_MUX_SD_LEGACY_MASK;

/// The L2's maintenance port (`specs/bootbox.toml`): the bootcode's flush ends
/// its cache-as-RAM window ([`crate::l2`]).
const L2_CTRL: u32 = map::BOOTBOX_BASE + crate::spec::bootbox::L2_CTRL;
const L2_FLUSH: u32 = crate::spec::bootbox::L2_CTRL_FLUSH_MASK;

/// The 64 MiB every peripheral window the VPU decodes lies in.
const MMIO_WINDOW: std::ops::Range<u32> = 0x7C00_0000..0x8000_0000;
const _: () = {
    let windows = [
        (map::PERIPH_BASE, map::PERIPH_SIZE),
        (map::SDRAMC_BASE, map::SDRAMC_SIZE),
        (map::CLKMON_BASE, map::CLKMON_SIZE),
        (map::PCIE_BASE, map::PCIE_SIZE),
        (map::GENET_BASE, map::GENET_SIZE),
    ];
    let mut i = 0;
    while i < windows.len() {
        let (base, size) = windows[i];
        assert!(base >= MMIO_WINDOW.start && size <= MMIO_WINDOW.end - base);
        i += 1;
    }
};

impl Machine {
    pub fn new(ram_bytes: usize) -> Machine {
        Machine {
            ram: Ram::new(map::SDRAM_CACHED_BASE, ram_bytes),
            maskrom: None,
            l2: Default::default(),
            systimer: SysTimer::new(),
            uart0: Pl011::new(),
            aux: Aux::new(),
            mbox: Mbox::new(),
            bell: Bell::new(),
            armctrl: ArmCtrl::new(),
            arm_local: ArmLocal::new(),
            gic: Gic::new(),
            mcsync: McSync::new(),
            corectl: CoreCtl::new(),
            pm: Pm::new(),
            clockman: ClockManager::new(),
            clkmon: ClkMon::new(),
            avs: Avs::new(),
            pvt: crate::periph::Pvt::new(),
            asb: Asb::new(),
            pcie: crate::periph::pcie::Pcie::new(),
            genet: crate::periph::Genet::new(),
            net: None,
            rng: Rng::new(),
            vce: Vce::new(),
            bsc0: {
                // Every pin is an input out of reset (`route_gpio_pins`).
                let mut bsc0 = Bsc::empty("bsc0");
                bsc0.set_pins(false);
                bsc0
            },
            gpio: Gpio::new(),
            spi0: {
                let mut spi0 = Spi0::new();
                spi0.set_pins(false);
                spi0
            },
            pactl: Pactl::new(),
            pwm0: Pwm::new("pwm0"),
            pwm1: Pwm::new("pwm1"),
            pcm: Pcm::new(),
            bsc_pmic: Bsc::new("bsc-pmic"),
            hdmi_ddc0: HdmiDdc::new("hdmi-ddc0"),
            hdmi_ddc1: HdmiDdc::new("hdmi-ddc1"),
            hdmi0: Hdmi::new("hdmi0"),
            hdmi1: Hdmi::new("hdmi1"),
            board: crate::soc::Board::default(),
            config_otp: ConfigOtp::new(),
            sdramc: Sdramc::new(),
            sdc: Sdc::new(),
            bootbox: BootBox::new(),
            alignment: crate::align::Alignment::off(),
            dma4: Dma4::new(),
            dma_legacy: crate::periph::dma_legacy::DmaLegacy::new(),
            dma_vpu: crate::periph::dma_legacy::DmaLegacy::new_vpu(),
            emmc: {
                let mut emmc = Emmc2::new_legacy();
                emmc.put_card(Some(crate::periph::sdcard::SdCard::sdio()));
                emmc
            },
            wifi: None,
            sd_slot_legacy: false,
            emmc2: Emmc2::new(),
            hvs: Hvs::new(),
            usbr: Usbr::new(),
            dwc2: Dwc2::new(),
            xhci_otg: XhciOtg::new(),
            periph_stub: StubRegion::new("periph-window"),
            bluetooth: crate::periph::bluetooth::BtModem::new(BT_ADDRESS),
            console: Console::default(),
            console_routed: Console::default(),
            stub_hits: 0,
            bus_errors: 0,
            ram_writes: 0,
            mmio_writes: 0,
            ram_reads: 0,
            mmio_reads: 0,
            mmio_trace: false,
            mmio_trace_range: None,
            mmio_events: Vec::new(),
            watch: std::env::var("PIMU_WATCH")
                .ok()
                .filter(|_| crate::diag::ON)
                .map(|v| {
                    v.split(',')
                        .filter_map(|t| {
                            u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok()
                        })
                        .map(|a| a & !3)
                        .collect()
                })
                .unwrap_or_default(),
            log: Log::default(),
            pending_irqs: std::collections::VecDeque::new(),
            pending_irqs1: std::collections::VecDeque::new(),
            recheck: false,
            defer_sleep: false,
            sleep_to: None,
            watch_pc: 0,
            watch_core: 0,
            phase_tags: Vec::new(),
        }
    }

    /// The board this machine is, and the stepping on it.
    pub fn board(&self) -> crate::soc::Board {
        self.board
    }

    /// Make this machine `board`: OTP row 30 takes its revision code, the PMIC bus
    /// its PMICs, later-built cores its stepping's `version`. Call it before anything
    /// runs; firmware reads the revision once, early.
    pub fn set_board(&mut self, board: crate::soc::Board) {
        self.board = board;
        self.config_otp.set(30, board.revision);
        self.sdc = Sdc::with_dram(crate::periph::sdc::Dram::for_memory(board.memory_bytes()));
        self.bsc_pmic
            .fit_pmics(crate::periph::Pmic::for_board(board));
        self.gpio.fit_board(board);
    }

    /// Send the machine's channels to `log`, handing every device that logs a clone.
    pub fn set_log(&mut self, log: Log) {
        self.systimer.set_log(log.clone());
        self.mbox.log = log.clone();
        self.bell.log = log.clone();
        self.corectl.log = log.clone();
        self.pcie.set_log(log.clone());
        self.spi0.log = log.clone();
        self.gpio.log = log.clone();
        self.aux.log = log.clone();
        self.bsc_pmic.set_log(log.clone());
        self.config_otp.log = log.clone();
        self.emmc.log = log.clone();
        self.emmc2.log = log.clone();
        self.dwc2.log = log.clone();
        self.xhci_otg.set_log(log.clone());
        self.log = log;
    }

    /// Plug GENET's cable into `backend`: the PHY sees a link partner.
    pub fn attach_net(&mut self, backend: Box<dyn crate::net::NetBackend>) {
        self.genet.phy.set_link(true);
        self.net = Some(backend);
    }

    /// Queue an interrupt source for core 0; it waits for that core's CoreCtl enable.
    pub fn push_pending_irq(&mut self, src: u32) {
        self.pending_irqs.push_back(src);
        self.recheck = true;
    }

    /// Is an interrupt source queued for core 0 that its CoreCtl bank enables?
    pub fn irq_queued(&self) -> bool {
        self.pending_irqs
            .iter()
            .any(|&src| self.corectl.irq_priority(0, src) != 0)
    }

    /// Queue a source start4 raised for core 1. It waits for core 1's CoreCtl enable
    /// and for core 1 to be able to take it, instead of being lost.
    pub fn push_core1_irq(&mut self, src: u32) {
        self.pending_irqs1.push_back(src);
        self.recheck = true;
    }

    /// Take the first source queued for core 1 that core 1's bank enables.
    pub fn take_core1_irq(&mut self) -> Option<u32> {
        let i = self
            .pending_irqs1
            .iter()
            .position(|&src| self.corectl.irq_priority(1, src) != 0)?;
        self.pending_irqs1.remove(i)
    }

    /// The lowest system-timer channel whose compare has fired and whose source core
    /// 0's CoreCtl bank enables. One that fired with its source disabled stays
    /// latched without holding up the channels above it.
    fn timer_channel_due(&self) -> Option<u8> {
        if !self.systimer.tick_pending() {
            return None;
        }
        (0..4u8).find(|&c| {
            self.systimer.channel_pending(c)
                && self
                    .corectl
                    .irq_priority(0, crate::periph::corectl::SYS_IRQ_SRC + c as u32)
                    != 0
        })
    }

    /// Has a compare fired whose interrupt core 0 can take?
    pub fn timer_irq_due(&self) -> bool {
        self.timer_channel_due().is_some()
    }

    fn advance_i2c(&mut self) {
        let now = self.systimer.now_us();
        self.bsc_pmic.advance_to(now);
        self.bsc0.advance_to(now);
    }

    fn advance_sd(&mut self) {
        let now = self.systimer.now_us();
        self.emmc2.advance_to(now);
        self.emmc.advance_to(now);
    }

    fn advance_pcie(&mut self) {
        let now = self.systimer.now_us();
        self.with_dma_master("the xHCI / VL805", |m| m.pcie.advance_to(now, &mut m.ram));
    }

    /// Settle the HDMI DDC masters, lazily on the way into their own registers: only
    /// the firmware's status poll observes them, and advancing them out of
    /// [`Machine::tick`] costs a few percent of throughput.
    fn advance_hdmi_ddc(&mut self, addr: u32) {
        let in_window = |base: u32, size: u32| addr >= base && addr < base + size;
        if in_window(map::HDMI_DDC0_BASE, map::HDMI_DDC_SIZE)
            || in_window(map::HDMI_DDC1_BASE, map::HDMI_DDC_SIZE)
            || in_window(map::HDMI_AUTO_I2C0_BASE, map::HDMI_AUTO_I2C_SIZE)
            || in_window(map::HDMI_AUTO_I2C1_BASE, map::HDMI_AUTO_I2C_SIZE)
        {
            let now = self.systimer.now_us();
            self.hdmi_ddc0.advance_to(now);
            self.hdmi_ddc1.advance_to(now);
        }
    }

    /// Mirror the core rail's PMIC setpoint into the AVS monitor. Channel 3 senses
    /// that rail, a board PMIC drives it over I²C, and start4's DVFS calibration
    /// requires at least 10 mV between two programmed voltages — a channel answering
    /// one fixed count reads as a dead rail. The blocks are in different windows.
    fn sync_avs_core_rail(&mut self, addr: u32) {
        if !(map::AVS_BASE..map::AVS_BASE + map::AVS_SIZE).contains(&addr) {
            return;
        }
        if let Some(uv) = self.bsc_pmic.slave().and_then(|p| p.core_rail_uv()) {
            self.avs.set_core_rail_uv(uv);
        }
    }

    /// Advance time-based peripheral state by `cycles` VPU cycles. Called every step,
    /// and all of it derives from the microsecond counter, so a step that does not
    /// move that counter has nothing to do.
    #[inline]
    pub fn tick(&mut self, cycles: u64) {
        if self.systimer.advance(cycles) {
            self.tick_us();
        }
    }

    #[inline(never)]
    fn tick_us(&mut self) {
        if self.systimer.take_fired() {
            self.recheck = true;
        }
        self.pm.advance(self.systimer.now_us());
        if self.pm.reset_pending() {
            self.recheck = true;
        }
        if self.net.is_some() {
            let now = self.systimer.now_us();
            self.with_dma_master("the GENET", |m| {
                m.genet.service(now, &mut m.ram, &mut m.net)
            });
        }
        self.advance_i2c();
        self.advance_sd();
        self.advance_pcie();
        // Each holds its line until acked: keep one delivery outstanding.
        let src = crate::periph::rng::IRQ_SRC;
        if self.rng.irq_asserted() && !self.pending_irqs.contains(&src) {
            self.push_pending_irq(src);
        }
        let src = crate::periph::mbox::IRQ_SRC;
        if self.mbox.irq_asserted() && !self.pending_irqs.contains(&src) {
            self.push_pending_irq(src);
        }
        // Doorbells 2 and 3 share source 94, the ARM control block's line.
        let src = crate::periph::bell::IRQ_SRC;
        if self.bell.vpu_irq_asserted() && !self.pending_irqs.contains(&src) {
            self.push_pending_irq(src);
        }
        let src = crate::periph::vce::IRQ_SRC;
        if self.vce.irq_asserted() && !self.pending_irqs.contains(&src) {
            self.push_pending_irq(src);
        }
        self.advance_hvs();
    }

    /// Let the HVS finish the frames the counter has reached, and hold source 97
    /// while an end-of-frame flag is set — the flag a display pause waits on.
    fn advance_hvs(&mut self) {
        self.hvs.advance_to(self.systimer.now_us());
        let src = crate::periph::hvs::IRQ_SRC;
        if self.hvs.irq_asserted() && !self.pending_irqs.contains(&src) {
            self.push_pending_irq(src);
        }
    }

    /// Which block the serial header is wired to, for [`Console::Pins`]: GPIO 14 is
    /// `TXD0` on ALT0 and `TXD1` on ALT5, and anything else reads as the PL011.
    fn console_uart(&self) -> Console {
        match self.console {
            Console::Pins => match self.gpio.function(14) {
                crate::periph::gpio::Function::Alt(5) => Console::MiniUart,
                _ => Console::Pl011,
            },
            fixed => fixed,
        }
    }

    /// Drain what the console UART transmitted. The other block wrote to pins the
    /// header does not carry, so the mini-UART's bytes are dropped and the PL011's
    /// go to the Bluetooth modem.
    pub fn take_console_output(&mut self) -> Vec<u8> {
        let routed = self.console_uart();
        if routed != self.console_routed {
            self.console_routed = routed;
            crate::log!(
                self.log,
                crate::log::Channel::Uart,
                "the serial header is on the {}",
                match routed {
                    Console::MiniUart => "mini-UART (GPIO 14/15 ALT5)",
                    _ => "PL011 (GPIO 14/15 ALT0)",
                }
            );
        }
        match routed {
            Console::MiniUart => {
                let to_modem = self.uart0.take_output();
                if !to_modem.is_empty() {
                    self.bluetooth.feed(&to_modem);
                }
                if self.bluetooth.has_output() {
                    let reply = self.bluetooth.take_output();
                    self.uart0.feed(&reply);
                    self.recheck = true;
                }
                self.aux.take_output()
            }
            _ => {
                self.aux.take_output();
                self.uart0.take_output()
            }
        }
    }

    pub fn console_feed(&mut self, bytes: &[u8]) {
        match self.console_uart() {
            Console::MiniUart => self.aux.feed(bytes),
            _ => self.uart0.feed(bytes),
        }
    }

    pub fn console_rx_backlog(&self) -> usize {
        self.uart0.rx_backlog() + self.aux.rx_backlog()
    }

    /// Advance both receivers to `now_us`; input only ever goes on one line.
    pub fn console_pump(&mut self, now_us: u64) {
        self.uart0.pump(now_us);
        self.aux.pump(now_us);
    }

    pub fn irq_pending(&self) -> bool {
        self.uart0.irq_pending() || self.aux.irq_pending()
    }

    /// The peripheral windows, which must be decoded before the cache-alias fold or
    /// their aliased addresses land in DRAM. The LPDDR4 controller counts too.
    #[inline]
    fn in_mmio(addr: u32) -> bool {
        addr >> 26 == MMIO_WINDOW.start >> 26
            && ((map::PERIPH_BASE..map::PERIPH_BASE + map::PERIPH_SIZE).contains(&addr)
                || (map::SDRAMC_BASE..map::SDRAMC_BASE + map::SDRAMC_SIZE).contains(&addr)
                || (map::CLKMON_BASE..map::CLKMON_BASE + map::CLKMON_SIZE).contains(&addr)
                || (map::PCIE_BASE..map::PCIE_BASE + map::PCIE_SIZE).contains(&addr)
                || (map::GENET_BASE..map::GENET_BASE + map::GENET_SIZE).contains(&addr))
    }

    /// Overlay a maskROM image; see the [`maskrom`](Self::maskrom) field.
    pub fn attach_maskrom(&mut self, bytes: Vec<u8>) {
        const BASE: u32 = 0x6000_0000;
        const CODE_LEN: u32 = 0x8000;
        let len = (bytes.len() as u32).min(CODE_LEN);
        self.maskrom = Some((BASE, BASE + len, bytes));
        self.l2.hold(0, 0);
    }

    #[inline]
    fn maskrom_at(&self, addr: u32) -> Option<usize> {
        let (base, end, _) = self.maskrom.as_ref()?;
        (*base..*end)
            .contains(&addr)
            .then(|| (addr - *base) as usize)
    }

    fn maskrom_load(&self, off: usize, width: Width) -> u32 {
        let bytes = &self.maskrom.as_ref().expect("overlay present").2;
        let mut v = 0u32;
        for i in 0..width.bytes() as usize {
            v |= u32::from(bytes.get(off + i).copied().unwrap_or(0)) << (8 * i);
        }
        v
    }

    fn mmio_traced(&self, addr: u32) -> bool {
        crate::diag::ON
            && self.mmio_trace
            && self
                .mmio_trace_range
                .is_none_or(|(lo, hi)| (lo..hi).contains(&addr))
    }

    /// Run `f` with the tracker told this peripheral, not the VPU, is using memory:
    /// it reads and writes behind the caches. Nests.
    #[inline]
    fn with_dma_master(&mut self, who: &'static str, f: impl FnOnce(&mut Machine)) {
        if !self.ram.coherency.is_on() {
            f(self);
            return;
        }
        self.with_master(crate::coherency::Master::Dma(who), f);
    }

    #[inline]
    fn with_vc4_dma_master(&mut self, who: &'static str, f: impl FnOnce(&mut Machine)) {
        self.with_master(crate::coherency::Master::Vc4Dma(who), f);
    }

    #[inline]
    fn with_master(&mut self, who: crate::coherency::Master, f: impl FnOnce(&mut Machine)) {
        if !self.ram.coherency.is_on() {
            f(self);
            return;
        }
        let was = self.ram.coherency.masters();
        self.ram.coherency.set_master(who);
        f(self);
        self.ram.coherency.set_masters(was.0, was.1);
    }

    /// Which side of the L2 a legacy-DMA bus address is on: only the `0xC000_0000`
    /// alias bypasses the caches, which is the one reason stock's `dma_memcpy` may
    /// copy through the `0x0` alias and read it straight back cached.
    #[inline]
    fn dma_master(addr: u32, who: &'static str) -> crate::coherency::Master {
        if addr >> 30 == 3 {
            crate::coherency::Master::Dma(who)
        } else {
            crate::coherency::Master::Vc4Dma(who)
        }
    }

    /// Fold the four VC4 cache aliases onto one backing store. Before SDRAM training
    /// that backing *is* the L2-as-SRAM the bootcode runs from ([`crate::l2`]).
    fn fold_ram_addr(addr: u32) -> u32 {
        addr & 0x3FFF_FFFF
    }

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
        for base in [map::BELL_BASE, map::BELL_VPU_BASE] {
            if let Some(off) = hit(base, map::BELL_SIZE) {
                return Some((&mut self.bell, off));
            }
        }
        if let Some(off) = hit(map::MBOX_BASE, map::MBOX_SIZE) {
            return Some((&mut self.mbox, off));
        }
        if let Some(off) = hit(map::ARMCTRL_BASE, map::ARMCTRL_SIZE) {
            return Some((&mut self.armctrl, off));
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
        if let Some(off) = hit(map::EMMC_BASE, map::EMMC_SIZE) {
            return Some((&mut self.emmc, off));
        }
        if let Some(off) = hit(map::EMMC2_BASE, map::EMMC2_SIZE) {
            return Some((&mut self.emmc2, off));
        }
        if let Some(off) = hit(map::HVS_BASE, map::HVS_SIZE) {
            return Some((&mut self.hvs, off));
        }
        if let Some(off) = hit(map::USBR_BASE, map::USBR_SIZE) {
            return Some((&mut self.usbr, off));
        }
        if let Some(off) = hit(map::DWC2_BASE, map::DWC2_SIZE) {
            return Some((&mut self.dwc2, off));
        }
        if let Some(off) = hit(map::XHCI_OTG_BASE, map::XHCI_OTG_SIZE) {
            return Some((&mut self.xhci_otg, off));
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
        if let Some(off) = hit(map::RNG_BASE, map::RNG_SIZE) {
            return Some((&mut self.rng, off));
        }
        if hit(map::VCE_BASE, map::VCE_MEM_SIZE).is_some()
            || hit(map::VCE_CTRL_BASE, map::VCE_CTRL_SIZE).is_some()
        {
            return Some((&mut self.vce, a - map::VCE_BASE));
        }
        if let Some(off) = hit(map::AVS_BASE, map::AVS_SIZE) {
            return Some((&mut self.avs, off));
        }
        if let Some(off) = hit(map::PVT_BASE, map::PVT_SIZE) {
            return Some((&mut self.pvt, off));
        }
        if let Some(off) = hit(map::ASB_BASE, map::ASB_SIZE) {
            return Some((&mut self.asb, off));
        }
        if let Some(off) = hit(map::CLKMON_BASE, map::CLKMON_SIZE) {
            return Some((&mut self.clkmon, off));
        }
        if let Some(off) = hit(map::SPI0_BASE, map::SPI0_SIZE) {
            return Some((&mut self.spi0, off));
        }
        if let Some(off) = hit(map::PACTL_BASE, map::PACTL_SIZE) {
            return Some((&mut self.pactl, off));
        }
        if let Some(off) = hit(map::PWM0_BASE, map::PWM_SIZE) {
            return Some((&mut self.pwm0, off));
        }
        if let Some(off) = hit(map::PWM1_BASE, map::PWM_SIZE) {
            return Some((&mut self.pwm1, off));
        }
        if let Some(off) = hit(map::PCM_BASE, map::PCM_SIZE) {
            return Some((&mut self.pcm, off));
        }
        if let Some(off) = hit(map::GPIO_BASE, map::GPIO_SIZE) {
            return Some((&mut self.gpio, off));
        }
        if let Some(off) = hit(map::BSC0_BASE, map::BSC0_SIZE) {
            return Some((&mut self.bsc0, off));
        }
        if let Some(off) = hit(map::BSC_PMIC_BASE, map::BSC_PMIC_SIZE) {
            return Some((&mut self.bsc_pmic, off));
        }
        if let Some(off) = hit(map::HDMI_DDC0_BASE, map::HDMI_DDC_SIZE) {
            return Some((&mut self.hdmi_ddc0, off));
        }
        if let Some(off) = hit(map::HDMI_DDC1_BASE, map::HDMI_DDC_SIZE) {
            return Some((&mut self.hdmi_ddc1, off));
        }
        if let Some(off) = hit(map::HDMI_AUTO_I2C0_BASE, map::HDMI_AUTO_I2C_SIZE) {
            return Some((&mut self.hdmi_ddc0, AUTO_WINDOW + off));
        }
        if let Some(off) = hit(map::HDMI_AUTO_I2C1_BASE, map::HDMI_AUTO_I2C_SIZE) {
            return Some((&mut self.hdmi_ddc1, AUTO_WINDOW + off));
        }
        if let Some(off) = hit(map::HDMI0_BASE, map::HDMI_SIZE) {
            return Some((&mut self.hdmi0, off));
        }
        if let Some(off) = hit(map::HDMI1_BASE, map::HDMI_SIZE) {
            return Some((&mut self.hdmi1, off));
        }
        if let Some(off) = hit(map::OTP_BASE, map::OTP_SIZE) {
            return Some((&mut self.config_otp, off));
        }
        if let Some(off) = hit(map::SDRAMC_BASE, map::SDRAMC_SIZE) {
            return Some((&mut self.sdramc, off));
        }
        if let Some(off) = hit(map::PCIE_BASE, map::PCIE_SIZE) {
            return Some((&mut self.pcie, off));
        }
        if let Some(off) = hit(map::GENET_BASE, map::GENET_SIZE) {
            return Some((&mut self.genet, off));
        }

        if (map::PERIPH_BASE..map::PERIPH_BASE + map::PERIPH_SIZE).contains(&a) {
            self.stub_hits += 1;
            return Some((&mut self.periph_stub, a - map::PERIPH_BASE));
        }
        None
    }

    /// Log the legacy DMA window on [`Channel::Dma`]. Only channel 11 is modelled;
    /// start4's `dma_memcpy` uses another, which falls through to the stub.
    fn dma_win_log(&self, rw: &str, addr: u32, value: u32) {
        if crate::diag::ON && (0x7E00_7000..0x7E00_8000).contains(&addr) {
            let ch = (addr - 0x7E00_7000) / 0x100;
            crate::log!(
                self.log,
                Channel::Dma,
                "window {rw} ch{ch} +{:#04x} ({addr:#x}) = {value:#x} pc={:#x}",
                (addr - 0x7E00_7000) % 0x100,
                self.watch_pc
            );
        }
    }

    /// Execute the chain armed on legacy DMA channel `ch`. CB: `+0x00 TI  +0x04
    /// SOURCE_AD  +0x08 DEST_AD  +0x0C TXFR_LEN  +0x10 STRIDE  +0x14 NEXTCONBK`.
    fn run_dma_legacy(&mut self, ch: usize, vpu: bool) {
        use crate::periph::dma_legacy::DmaLegacy;

        let raw = if vpu {
            self.dma_vpu.conblk_ad(ch)
        } else {
            self.dma_legacy.conblk_ad(ch)
        };
        // `CONBLK_AD` is raw for a legacy channel and shifted right by 5 for a
        // 40-bit one; only a raw VC4 pointer has its alias bits set.
        let cb_addr = if raw >> 30 != 0 { raw } else { raw << 5 };
        let who = if vpu { "the VPU DMA" } else { "the legacy DMA" };
        let fetch = Machine::dma_master(cb_addr, who);
        let mut cb = cb_addr & 0x3FFF_FFFF;
        for _ in 0..4096 {
            if cb == 0 || !self.ram.contains(cb) {
                break;
            }
            self.ram.coherency.set_master(fetch);
            let mut w = [0u32; 7];
            for (i, slot) in w.iter_mut().enumerate() {
                *slot = self.ram.load(cb + (i as u32) * 4, Width::Word).unwrap_or(0);
            }
            // Channel 15 is 40-bit on C0 and legacy on B0; start4 builds the
            // matching CB (dma40: TI, SRC, SRCI, DEST, DESTI, LEN, NEXT_CB>>5).
            let dma40 = vpu && self.board.stepping.dma_channel_15_is_40_bit();
            let d = if dma40 {
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
            if crate::diag::ON {
                crate::log!(
                    self.log,
                    Channel::Dma,
                    "legacy ch{ch} cb={cb:#x} ti={:#x} src={:#x} dest={:#x} len={:#x} stride={:#x} next={:#x}",
                    d.ti, d.src, d.dest, d.len, d.stride, d.next
                );
            }
            self.ram.coherency.set_masters(
                Machine::dma_master(d.src, who),
                Machine::dma_master(d.dest, who),
            );
            if Machine::in_mmio(d.src) || Machine::in_mmio(d.dest) {
                self.run_dma_periph(&d);
                cb = d.next & 0x3FFF_FFFF;
                continue;
            }
            let (rows, xlen) = d.rows();
            let (src_stride, dest_stride) = d.strides();
            let mut src = d.src & 0x3FFF_FFFF;
            let mut dest = d.dest & 0x3FFF_FFFF;
            for _ in 0..rows {
                for i in 0..xlen {
                    let sa = if dma40 || d.src_inc() {
                        src.wrapping_add(i)
                    } else {
                        src
                    };
                    let da = if dma40 || d.dest_inc() {
                        dest.wrapping_add(i)
                    } else {
                        dest
                    };
                    let b = self.ram.load(sa & 0x3FFF_FFFF, Width::Byte).unwrap_or(0);
                    let _ = self.ram.store(da & 0x3FFF_FFFF, Width::Byte, b);
                }
                src = src.wrapping_add(xlen).wrapping_add(src_stride as u32);
                dest = dest.wrapping_add(xlen).wrapping_add(dest_stride as u32);
            }
            cb = d.next & 0x3FFF_FFFF;
        }
        self.ram.coherency.set_master(crate::coherency::Master::Vpu);
        if vpu {
            self.dma_vpu.finish(ch);
        } else {
            self.dma_legacy.finish(ch);
        }
        let src = dma_irq_source(ch);
        self.push_pending_irq(src);
    }

    /// The SD hosts' Buffer Data Ports, the only FIFOs a legacy-DMA transfer aims at.
    const EMMC_FIFO: u32 = map::EMMC_BASE + crate::spec::emmc::BUFFER_DATA;
    const EMMC2_FIFO: u32 = map::EMMC2_BASE + crate::spec::emmc2::BUFFER_DATA;

    /// One control block with a peripheral at one end — how Linux drives the legacy
    /// engine for an SD transfer, and something start4 never does. It moves whole
    /// words and is DREQ-paced, and neither end is 2D.
    fn run_dma_periph(&mut self, d: &crate::periph::dma_legacy::Cb) {
        let mut off = 0u32;
        while off + 4 <= d.len {
            let sa = if d.src_inc() { d.src + off } else { d.src };
            let da = if d.dest_inc() { d.dest + off } else { d.dest };
            let w = self.dma_periph_load(sa);
            self.dma_periph_store(da, w);
            off += 4;
        }
    }

    fn dma_periph_load(&mut self, addr: u32) -> u32 {
        if !Machine::in_mmio(addr) {
            return self.ram.load(addr & 0x3FFF_FFFF, Width::Word).unwrap_or(0);
        }
        match addr {
            a if a == Self::EMMC_FIFO => self.emmc.dma_fifo_read(),
            a if a == Self::EMMC2_FIFO => self.emmc2.dma_fifo_read(),
            _ => self.load_device(addr, Width::Word).unwrap_or(0),
        }
    }

    fn dma_periph_store(&mut self, addr: u32, value: u32) {
        if !Machine::in_mmio(addr) {
            let _ = self.ram.store(addr & 0x3FFF_FFFF, Width::Word, value);
            return;
        }
        match addr {
            a if a == Self::EMMC_FIFO => self.emmc.dma_fifo_write(value),
            a if a == Self::EMMC2_FIFO => self.emmc2.dma_fifo_write(value),
            _ => {
                let _ = self.store_device(addr, Width::Word, value);
            }
        }
    }

    fn dma40_load(&mut self, addr: u64) -> u32 {
        // The window starts at `0x6_0000_0000`, so a 32-bit address is DRAM.
        if addr >> 32 != 0 {
            if let Some(v) = self.pcie.mmio_read(addr, Width::Word) {
                return v;
            }
        }
        self.ram
            .load((addr as u32) & 0x3FFF_FFFF, Width::Word)
            .unwrap_or(0)
    }

    /// One word to a 40-bit DMA4 address, from behind the VPU's caches.
    fn dma40_store(&mut self, addr: u64, value: u32) {
        self.with_dma_master("the 40-bit DMA / xHCI", |m| {
            if addr >> 32 != 0 && m.pcie.mmio_write(addr, Width::Word, value, &mut m.ram) {
                return;
            }
            let _ = m.ram.store((addr as u32) & 0x3FFF_FFFF, Width::Word, value);
        });
    }

    /// Run the chain the DMA4 channel was just armed with. `SRCI` bit 12 clear ⇒ fill
    /// `DEST` with the single word at `SRC` (`SRC == 0` ⇒ zero-fill scrub), set ⇒
    /// copy. Addresses are 40 bits, and one in the PCIe outbound window reaches the
    /// VL805's registers instead of DRAM.
    fn run_dma4(&mut self) {
        const S_INC: u32 = 1 << 12;
        /// `SRC_INFO` / `DEST_INFO` bits `[7:0]` are address bits `[39:32]`: how the
        /// firmware reaches the PCIe outbound window from a 32-bit core.
        const ADDR_HI: u32 = 0xFF;
        let rd = |ram: &Ram, addr: u32| ram.load(addr & 0x3FFF_FFFF, Width::Word).unwrap_or(0);

        let mut cb = self.dma4.cb_addr() & 0x3FFF_FFFF;
        let mut interrupt = false;
        for _ in 0..4096 {
            if cb == 0 || !self.ram.contains(cb) {
                break;
            }
            let src = rd(&self.ram, cb + 0x04);
            let srci = rd(&self.ram, cb + 0x08);
            let dest = rd(&self.ram, cb + 0x0C);
            let desti = rd(&self.ram, cb + 0x10);
            let len = rd(&self.ram, cb + 0x14);
            let next = rd(&self.ram, cb + 0x18);
            interrupt |= rd(&self.ram, cb) & crate::periph::dma4::TI_INTEN != 0;
            let src40 = (((srci & ADDR_HI) as u64) << 32) | src as u64;
            let dest40 = (((desti & ADDR_HI) as u64) << 32) | dest as u64;

            if crate::diag::ON {
                crate::log!(
                    self.log,
                    Channel::Dma,
                    "cb={cb:#x} ti={:#x} src={src:#x} srci={srci:#x} dest={dest:#x} len={len:#x} next={next:#x}",
                    rd(&self.ram, cb)
                );
            }
            let fill = srci & S_INC == 0;
            let fill_word = if src40 == 0 {
                0
            } else {
                self.dma40_load(src40)
            };
            let mut off = 0u32;
            while off < len {
                let v = if fill {
                    fill_word
                } else {
                    self.dma40_load(src40 + off as u64)
                };
                self.dma40_store(dest40 + off as u64, v);
                off = off.wrapping_add(4);
            }
            cb = (next << 5) & 0x3FFF_FFFF;
        }
        self.dma4.finish(interrupt);
        // The bootloader polls (no `TI.INTEN`); start4's dmalib chains on this.
        if interrupt {
            self.push_pending_irq(dma_irq_source(11));
        }
    }
}

/// The VPU source a DMA channel's completion raises: 80 + channel low down,
/// 78 + channel for DMA4's 11..14, 95 for the VPU's 15, and one line shared by
/// 7/8 and by 9/10 (unverified — nothing here uses them).
fn dma_irq_source(ch: usize) -> u32 {
    match ch {
        0..=6 => 80 + ch as u32,
        7 | 8 => 87,
        9 | 10 => 88,
        11..=14 => 78 + ch as u32,
        _ => 95,
    }
}

impl Machine {
    #[inline(never)]
    fn load_device(&mut self, addr: u32, width: Width) -> BusResult<u32> {
        self.mmio_reads = self.mmio_reads.wrapping_add(1);
        self.advance_hdmi_ddc(addr);
        self.sync_avs_core_rail(addr);
        let trace = self.mmio_traced(addr);
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

    /// Finish a VPU `sleep` [`Bus::sleep_advance`] left to the ARM: the counter moves
    /// to `us` and what is timed against it catches up.
    pub fn wake_vpu_at(&mut self, us: u64) {
        self.systimer.advance_to(us);
        self.settle_timed();
    }

    /// Bring everything timed against the counter up to where the counter is.
    ///
    /// [`Self::tick`] does this a microsecond at a time, so it is only needed where
    /// something moves the counter by itself: a `sleep` that jumps to the next
    /// deadline, an ARM-side wake, or the run loop's busy-wait fast-forward.
    pub fn settle_timed(&mut self) {
        self.advance_i2c();
        self.advance_sd();
        self.advance_pcie();
        self.advance_hvs();
    }

    /// When some timed device next has work; see [`crate::sched`].
    fn next_wake(&self) -> Option<u64> {
        crate::sched::next_due(self).map(|(us, _)| us)
    }

    /// The value a read of `addr` would return where a read has no side effect;
    /// `None` otherwise. Only the mailbox's, which is what UEFI busy-waits on.
    pub fn peek(&self, addr: u32) -> Option<u32> {
        let off = addr.checked_sub(map::MBOX_BASE)?;
        if off >= map::MBOX_SIZE {
            return None;
        }
        self.mbox.peek(off)
    }

    /// A write to the SD-slot mux word: bit 1 set routes the card to the legacy EMMC,
    /// clear to EMMC2. 2020-era bootcode (`pieeprom-2020-09-03`) writes `0x2` right
    /// before its SD init, the 2026 bootcode never writes it, start4 clears bit 1
    /// explicitly, and what bit 0 does is not known.
    fn route_sd_slot(&mut self, value: u32) {
        let legacy = value & SD_SLOT_MUX_LEGACY != 0;
        if legacy == self.sd_slot_legacy {
            return;
        }
        self.sd_slot_legacy = legacy;
        if legacy {
            // One host, one bus: the WiFi chip steps aside.
            self.wifi = self.emmc.take_card();
            let card = self.emmc2.take_card();
            self.emmc.put_card(card);
        } else {
            let card = self.emmc.take_card();
            self.emmc2.put_card(card);
            let wifi = self.wifi.take();
            self.emmc.put_card(wifi);
        }
    }

    /// Put each master's pads where the pin functions say; the pins belong to the
    /// GPIO block and the masters are their own devices. SPI0 reaches the boot flash
    /// only on GPIO 40..43 ALT4 and I²C 0 the header's HAT EEPROM only on GPIO 0/1
    /// ALT0, and the firmware moves each there and back around every use.
    fn route_gpio_pins(&mut self) {
        let alt = |gpio: &Gpio, pin, n| gpio.function(pin) == gpio::Function::Alt(n);
        let flash = (40..=43).all(|pin| alt(&self.gpio, pin, 4));
        self.spi0.set_pins(flash);
        let header = alt(&self.gpio, 0, 0) && alt(&self.gpio, 1, 0);
        self.bsc0.set_pins(header);
    }

    #[inline(never)]
    fn store_device(&mut self, addr: u32, width: Width, value: u32) -> BusResult<()> {
        self.recheck = true;
        self.mmio_writes = self.mmio_writes.wrapping_add(1);
        self.dma_win_log("wr", addr, value);
        self.advance_hdmi_ddc(addr);
        if addr == SD_SLOT_MUX && width == Width::Word {
            self.route_sd_slot(value);
        }
        if addr == L2_CTRL && value & L2_FLUSH != 0 {
            let (first, last) = (
                self.bootbox.word(crate::spec::bootbox::L2_FLUSH_START),
                self.bootbox.word(crate::spec::bootbox::L2_FLUSH_END),
            );
            self.l2.flush(first, last);
            self.ram.coherency.flushed(first, last);
        }
        let trace = self.mmio_traced(addr);
        if let Some((dev, off)) = self.device_for(addr) {
            let r = dev.write(off, width, value);
            if trace {
                self.mmio_events
                    .push((addr, width.bytes() as u8, value, true));
            }
            if let Some(buf) = self.mbox.take_property_reply() {
                let ram = &self.ram;
                let word = |o: u32| {
                    ram.load(Machine::fold_ram_addr(buf.wrapping_add(o)), Width::Word)
                        .unwrap_or(0)
                };
                // A rejected buffer as the firmware left it: which request it
                // was, and how far the tag walk got.
                let rejected = (word(4) != crate::periph::mbox::RESPONSE).then(|| {
                    let mut line = format!("property reply error at {buf:#010x}:");
                    for i in 0..16 {
                        line.push_str(&format!(" {:08x}", word(i * 4)));
                    }
                    line
                });
                self.mbox.property.record(word);
                if let Some(line) = rejected {
                    crate::log!(self.mbox.log, Channel::Mbox, "{line}");
                }
            }
            if self.dma4.take_start() {
                // CPU-physical addresses and behind the L2: stock writes its
                // CB through `0x8000_0000` without flushing.
                self.with_vc4_dma_master("the 40-bit DMA", |m| m.run_dma4());
            }
            if let Some(ch) = self.dma_legacy.take_start() {
                self.run_dma_legacy(ch, false);
            }
            if let Some(ch) = self.dma_vpu.take_start() {
                self.run_dma_legacy(ch, true);
            }
            if self.emmc2.dma_pending() {
                self.with_dma_master("the EMMC2 DMA", |m| m.emmc2.run_dma(&mut m.ram));
            }
            // A register write can run a ring, which needs DRAM of its own.
            if self.xhci_otg.write_pending() {
                self.with_dma_master("the OTG xHCI", |m| m.xhci_otg.run_pending(&mut m.ram));
            }
            if self.genet.take_kick() {
                let now = self.systimer.now_us();
                self.with_dma_master("the GENET", |m| {
                    m.genet.service(now, &mut m.ram, &mut m.net)
                });
            }
            if GPFSEL_WINDOW.contains(&addr) {
                self.route_gpio_pins();
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

impl Bus for Machine {
    fn take_pending_irq(&mut self) -> Option<u32> {
        // Present the source at CoreCtl `+0x04` as it is vectored, never when
        // queued: the dispatcher re-reads it well into its entry, so a source
        // queued in between must not overwrite the one being taken. Only a
        // source whose enable field is non-zero may be taken at all.
        let i = self
            .pending_irqs
            .iter()
            .position(|&src| self.corectl.irq_priority(0, src) != 0)?;
        let src = self.pending_irqs.remove(i)?;
        self.corectl.raise_source(0, src);
        Some(src)
    }

    fn take_tick_pending(&mut self) -> bool {
        match self.timer_channel_due() {
            Some(c) => self.systimer.take_channel(c),
            None => false,
        }
    }

    fn sleep_advance(&mut self) -> bool {
        // An ARM write that interrupts the VPU must land before the counter
        // moves past it, so with the ARM running the jump waits for it.
        if self.defer_sleep {
            self.sleep_to = self.next_wake();
            return self.sleep_to.is_some();
        }
        // The HVS can end a frame it interrupts for before the next compare, and
        // then the counter stops there instead of at the compare.
        let woke = match crate::sched::next_due(self) {
            Some((frame, crate::sched::Timed::Hvs)) => {
                self.systimer.advance_to(frame);
                true
            }
            _ => self.systimer.wake_to_next_match().is_some(),
        };
        self.settle_timed();
        woke
    }

    /// Fetch an instruction straight out of RAM when it lives there: the generic path
    /// costs an address decode per halfword, and execution is nearly always RAM.
    fn read_insn(&mut self, pc: u32, out: &mut [u8; 10]) -> BusResult<u8> {
        if let Some(off) = self.maskrom_at(pc) {
            let bytes = &self.maskrom.as_ref().expect("overlay present").2;
            let p0 = u16::from_le_bytes([
                bytes.get(off).copied().unwrap_or(0),
                bytes.get(off + 1).copied().unwrap_or(0),
            ]);
            let len = crate::vpu::length::insn_len_bytes(p0);
            for (i, slot) in out.iter_mut().take(len as usize).enumerate() {
                *slot = bytes.get(off + i).copied().unwrap_or(0);
            }
            self.ram_reads = self.ram_reads.wrapping_add(1);
            return Ok(len);
        }
        if !Machine::in_mmio(pc) && !self.l2.diverts(pc, Machine::fold_ram_addr(pc)) {
            let phys = Machine::fold_ram_addr(pc);
            if let Ok(head) = self.ram.read_slice(phys, 2) {
                let p0 = u16::from_le_bytes([head[0], head[1]]);
                let len = crate::vpu::length::insn_len_bytes(p0);
                if let Ok(bytes) = self.ram.read_slice(phys, len as usize) {
                    self.ram_reads = self.ram_reads.wrapping_add(1);
                    out[..len as usize].copy_from_slice(bytes);
                    return Ok(len);
                }
            }
        }
        let p0 = self.load(pc, Width::Half)? as u16;
        let len = crate::vpu::length::insn_len_bytes(p0);
        out[0..2].copy_from_slice(&p0.to_le_bytes());
        let mut i = 2u32;
        while i < len as u32 {
            let h = self.load(pc.wrapping_add(i), Width::Half)? as u16;
            out[i as usize..i as usize + 2].copy_from_slice(&h.to_le_bytes());
            i += 2;
        }
        Ok(len)
    }

    /// Same condition as `read_insn`'s RAM fast path. A hit must count as the RAM
    /// read that fetch would have made: `ram_reads` feeds progress heuristics a
    /// decode cache must not perturb.
    #[inline]
    fn code_gen(&mut self, pc: u32, cached: Option<u64>) -> Option<u64> {
        if self.maskrom_at(pc).is_some() {
            const ROM_GEN: u64 = u64::MAX;
            if cached == Some(ROM_GEN) {
                self.ram_reads = self.ram_reads.wrapping_add(1);
            }
            return Some(ROM_GEN);
        }
        if Machine::in_mmio(pc) || self.l2.diverts(pc, Machine::fold_ram_addr(pc)) {
            return None;
        }
        let gen = self.ram.page_gen(Machine::fold_ram_addr(pc))?;
        if cached == Some(gen) {
            self.ram_reads = self.ram_reads.wrapping_add(1);
        }
        Some(gen)
    }

    fn timer_tick_slot(&mut self) -> Option<u32> {
        // Each compare channel vectors through its own source; the 4-bit
        // per-source field the firmware writes is an enable and priority, never
        // a vector. Go by the latched match, never by what is armed: a one-shot
        // compare disarms the moment it fires.
        let ch = self.timer_channel_due()? as u32;
        let src = crate::periph::corectl::SYS_IRQ_SRC + ch;
        // Entry 64 is a direct handler that never re-reads the source, so
        // leaving a stale value would only shadow a device interrupt's own.
        if src != crate::periph::corectl::SYS_IRQ_SRC {
            self.corectl.raise_source(0, src);
        }
        Some(src)
    }

    #[inline]
    fn load(&mut self, addr: u32, width: Width) -> BusResult<u32> {
        if self.alignment.is_on() {
            self.alignment.note(addr, width, self.watch_pc, false);
        }
        if let Some(off) = self.maskrom_at(addr) {
            self.ram_reads = self.ram_reads.wrapping_add(1);
            return Ok(self.maskrom_load(off, width));
        }
        if !Machine::in_mmio(addr) {
            let phys = Machine::fold_ram_addr(addr);
            if self.ram.contains(phys) {
                self.ram_reads = self.ram_reads.wrapping_add(1);
                if self.ram.coherency.is_on() {
                    if addr >> 30 == 3 {
                        self.ram.coherency.read_by(
                            phys,
                            width.bytes(),
                            crate::coherency::Master::VpuUncached,
                        );
                    } else {
                        let pc = self.watch_pc;
                        self.ram.coherency.read_cached(phys, width.bytes(), pc);
                    }
                }
                if self.l2.covers(phys) {
                    return self.l2.load(&self.ram, addr, phys, width);
                }
                return self.ram.load(phys, width);
            }
        }
        self.load_device(addr, width)
    }

    #[inline]
    fn store(&mut self, addr: u32, width: Width, value: u32) -> BusResult<()> {
        if self.alignment.is_on() {
            self.alignment.note(addr, width, self.watch_pc, true);
        }
        self.ram_writes = self.ram_writes.wrapping_add(1);
        if crate::diag::ON && !self.watch.is_empty() {
            let a = Machine::fold_ram_addr(addr) & !3;
            if self
                .watch
                .iter()
                .any(|&w| Machine::fold_ram_addr(w) & !3 == a)
            {
                eprintln!(
                    "[watch] core{} pc={:#010x} store{} {:#010x} <- {:#x}",
                    self.watch_core,
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
                // Bits 31:30 pick the alias; only `0b11` bypasses the caches.
                if self.ram.coherency.is_on() && addr >> 30 != 3 {
                    self.ram.coherency.wrote_cached(phys, width.bytes());
                }
                if crate::diag::ON
                    && addr & 0x03FF_FFFF == PHASE_TAG_SIG
                    && width == Width::Word
                    && self.phase_tags.last() != Some(&value)
                {
                    self.phase_tags.push(value);
                }
                if self.l2.covers(phys) {
                    return self.l2.store(&mut self.ram, addr, phys, width, value);
                }
                return self.ram.store(phys, width, value);
            }
        }
        self.store_device(addr, width, value)
    }
}

#[cfg(test)]
mod tests {
    use super::dma_irq_source;
    use super::{map, Machine, L2_CTRL, SD_SLOT_MUX};
    use crate::bus::Bus;

    /// The mux moves the card between the hosts; the WiFi chip has the legacy one.
    #[test]
    fn the_sd_slot_mux_moves_the_card_between_hosts() {
        use crate::periph::sdcard::CardKind;
        let legacy_kind = |m: &Machine| m.emmc.card().map(|c| c.kind());
        let mut m = Machine::new(1 << 20);
        m.emmc2.insert_card(vec![0; 512 * 16]);
        assert_eq!(legacy_kind(&m), Some(CardKind::Sdio));
        m.store32(SD_SLOT_MUX, 0x2).unwrap();
        assert_eq!(legacy_kind(&m), Some(CardKind::Sd));
        assert!(!m.emmc2.has_card());
        // Bit 0 alone leaves the card on EMMC2 and the WiFi chip on the legacy host.
        m.store32(SD_SLOT_MUX, 0x1).unwrap();
        assert_eq!(legacy_kind(&m), Some(CardKind::Sdio));
        assert!(m.emmc2.has_card());
        m.store32(SD_SLOT_MUX, 0x0).unwrap();
        assert_eq!(legacy_kind(&m), Some(CardKind::Sdio));
        assert!(m.emmc2.has_card());
    }

    /// 2022-04-26 bootcode keeps its config in the L2 at `0x8001_8020` and loads
    /// a file to `0xC001_8000` across it, until its pre-bootmain flush.
    #[test]
    fn the_l2_keeps_the_bootcode_s_lines_until_its_flush() {
        let mut m = Machine::new(1 << 20);
        m.l2.hold(0, 0x100);
        m.store32(0x8001_80C8, 0xF41).unwrap();
        m.store32(0xC001_80C8, 0).unwrap();
        assert_eq!(m.load32(0x8001_80C8), Ok(0xF41));

        m.store32(map::BOOTBOX_BASE + 0x1004, 0).unwrap();
        m.store32(map::BOOTBOX_BASE + 0x1008, 0x0FFF_FFE0).unwrap();
        m.store32(L2_CTRL, 0x14).unwrap();
        m.store32(0xC001_80C8, 0).unwrap();
        assert_eq!(m.load32(0x8001_80C8), Ok(0));
    }

    /// Channel 15 reads a DMA4-layout CB on C0 and a legacy one on B0.
    #[test]
    fn channel_15_takes_the_control_blocks_of_its_stepping() {
        use crate::soc::{Board, Stepping};
        const CS: u32 = 0x7EE0_5000;
        const CB: u32 = 0x1000;
        const SRC: u32 = 0x2000;
        const DEST: u32 = 0x3000;
        for stepping in [Stepping::B0, Stepping::C0] {
            let mut m = Machine::new(1 << 20);
            m.set_board(Board::for_stepping(stepping));
            for i in 0..16 {
                m.store32(SRC + i * 4, 0x0101_0101 * (i + 1)).unwrap();
            }
            let cb: [u32; 7] = match stepping {
                // TI, SOURCE_AD, DEST_AD, TXFR_LEN, STRIDE, NEXTCONBK
                Stepping::B0 => [0xF331, 0x8000_0000 | SRC, 0x8000_0000 | DEST, 64, 0, 0, 0],
                // TI, SRC, SRCI, DEST, DESTI, LEN, NEXT_CB
                Stepping::C0 => [0, SRC, 0, DEST, 0, 64, 0],
            };
            for (i, w) in cb.iter().enumerate() {
                m.store32(CB + i as u32 * 4, *w).unwrap();
            }
            m.store32(CS, 1).unwrap();
            m.store32(CS + 4, 0x8000_0000 | CB).unwrap();
            for i in 0..16 {
                assert_eq!(
                    m.load32(DEST + i * 4),
                    Ok(0x0101_0101 * (i + 1)),
                    "{stepping}"
                );
            }
        }
    }

    /// The sources start4's dmalib registers `dma_interrupt` on.
    #[test]
    fn dma_completion_sources_match_dmalib() {
        for (ch, src) in [(1, 81), (3, 83), (6, 86), (11, 89), (14, 92), (15, 95)] {
            assert_eq!(dma_irq_source(ch), src, "channel {ch}");
        }
    }

    /// A queued source waits for its enable without holding up the ones behind it.
    #[test]
    fn a_queued_source_waits_for_its_corectl_enable() {
        const IRQ_PRIO: u32 = 0x7E00_2010;
        let mut m = Machine::new(1 << 20);
        m.push_pending_irq(97);
        m.push_pending_irq(94);
        assert!(!m.irq_queued());
        assert_eq!(m.take_pending_irq(), None);
        // Source 94: word 3, bits 24..28.
        m.store32(IRQ_PRIO + 0xC, 0x0100_0000).unwrap();
        assert!(m.irq_queued());
        assert_eq!(m.take_pending_irq(), Some(94));
        assert_eq!(m.take_pending_irq(), None);
        // Source 97: word 4, bits 4..8, not source 65's field in word 0.
        m.store32(IRQ_PRIO, 0x10).unwrap();
        assert_eq!(m.take_pending_irq(), None);
        m.store32(IRQ_PRIO + 0x10, 0x10).unwrap();
        assert_eq!(m.take_pending_irq(), Some(97));
        assert!(!m.irq_queued());
    }

    /// An unenabled compare stays latched; the enabled channel above goes first.
    #[test]
    fn a_timer_match_waits_for_its_source_s_enable() {
        const IRQ_PRIO: u32 = 0x7E00_2010;
        const C0: u32 = 0x7E00_300C;
        let mut m = Machine::new(1 << 20);
        m.store32(C0, 10).unwrap();
        m.store32(C0 + 8, 20).unwrap();
        m.systimer.advance_to(30);
        assert!(m.systimer.tick_pending());
        assert!(!m.timer_irq_due());
        assert_eq!(m.timer_tick_slot(), None);
        // Source 66, channel 2: word 0, bits 8..12.
        m.store32(IRQ_PRIO, 0x100).unwrap();
        assert_eq!(m.timer_tick_slot(), Some(66));
        assert!(m.take_tick_pending());
        assert!(!m.timer_irq_due() && m.systimer.tick_pending());
        // Source 64, channel 0: bits 0..4.
        m.store32(IRQ_PRIO, 0x101).unwrap();
        assert_eq!(m.timer_tick_slot(), Some(64));
        assert!(m.take_tick_pending());
        assert!(!m.systimer.tick_pending());
    }
}
