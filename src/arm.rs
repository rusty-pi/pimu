//! The ARM side of the machine (#40, milestone 2): Cortex-A72 core 0 on the
//! BCM2711's ARM physical map, run in lock-step with the VPU.
//!
//! ## Release
//!
//! The core stays in reset until `arm_loader` lets it go, which it does by
//! writing the ARM control block ([`crate::periph::armctrl`]). It then starts
//! the way the SoC starts it: PC 0, EL3, `DAIF` masked — in the firmware's
//! armstub ([`crate::armstub`]), which drops to non-secure EL2 and jumps to
//! the kernel with `x0` = the dtb.
//!
//! ## Address map
//!
//! Low-peripheral mode, the one the handed-over device tree describes:
//!
//! ```text
//!   0x0_0000_0000 ..             RAM: the VPU's SDRAM, no aliases
//!   0x0_FC00_0000 .. 0xFF80_0000  peripherals; ARM 0xFC00_0000 + x is VPU
//!                                 bus address 0x7C00_0000 + x
//!   0x0_FF80_0000                 ARM local block (periph/armlocal.rs)
//!   0x0_FF84_0000                 GIC-400 (periph/gic.rs)
//! ```
//!
//! Anything else is a bus abort. RAM accesses go straight to the backing
//! store rather than through [`Machine`]'s VPU address decode, so they do not
//! count towards the VPU run loop's progress heuristics.
//!
//! ## Time and scheduling
//!
//! One ARM instruction is one cycle at the nominal [`gentimer::ARM_HZ`].
//! After every VPU step the run loop calls [`ArmSide::catch_up`], which runs
//! the ARM until it has had as many cycles as the system timer says have
//! passed since release: about 28 per VPU step, and a whole slice at once
//! when the VPU's `sleep` or `usleep` fast-forward jumps the counter. Either
//! way a run is a pure function of its inputs — the reproducibility the
//! regression bench depends on — and the ARM's clock never falls behind the
//! VPU's. A core in `wfi` spends its cycles asleep, skipping ahead to the
//! next generic-timer event inside its slice, and wakes as soon as the GIC
//! signals it (masked or not, as the architecture says).
//!
//! A fast-forward slice runs with the VPU frozen, so a mailbox request the
//! ARM makes in one is seen by the VPU only when the slice ends (at most one
//! VPU timer interval late).
//!
//! Not yet: stage 2 translation (a core that sets `HCR_EL2.VM` stops with
//! [`ArmStop::Unsupported`]), secondary cores (they stay in reset; Linux
//! gives up on them after its own timeout), and time spent asleep on the VPU
//! side (`sleep` fast-forwards the system timer without the ARM).

use crate::aarch64::{sysreg, Abort, Cpu, Memory, Step};
use crate::armstub::{self, Handoff};
use crate::bus::{Bus, MmioDevice, Width};
use crate::machine::Machine;
use crate::periph::gentimer::{self, GenericTimer, Reg, Which};
use crate::periph::{armlocal, gic};

/// The peripheral window, and how far below it the VPU sees the same thing.
const PERIPH: std::ops::Range<u64> = 0xFC00_0000..0xFF80_0000;
const PERIPH_TO_BUS: u64 = 0x8000_0000;

/// Why the ARM stopped. The core never stops by itself; these are gaps in the
/// model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmStop {
    /// An instruction the interpreter does not implement yet.
    Unimplemented { pc: u64, el: u32, insn: u32 },
    /// The guest enabled something not modelled yet (the MMU).
    Unsupported {
        pc: u64,
        el: u32,
        what: &'static str,
    },
}

/// ARM core 0 and what it has done since release.
pub struct ArmSide {
    pub cpu: Cpu,
    pub timer: GenericTimer,
    /// Modelled cycles since release: instructions plus time asleep.
    pub cycles: u64,
    pub insns: u64,
    pub slept: u64,
    pub exceptions: u64,
    pub interrupts: u64,
    /// In `wfi`, waiting for an interrupt.
    pub waiting: bool,
    pub stopped: Option<ArmStop>,
    /// What `arm_loader` left in the armstub, read at release.
    pub handoff: Option<Handoff>,
    /// The kernel command line, before and after [`armstub::BOOTARGS`] were
    /// added at release.
    pub bootargs: Option<Result<(String, String), String>>,
    /// The first time the core reached the kernel entry: `(cycles, EL, x0)`.
    pub kernel_entered: Option<(u64, u32, u64)>,
    /// The system timer, in ARM cycles, at the first [`Self::catch_up`].
    released_at: Option<u64>,
}

impl Default for ArmSide {
    fn default() -> Self {
        Self::new()
    }
}

impl ArmSide {
    /// Core 0 in its reset state, not yet looking at any hand-off.
    pub fn new() -> ArmSide {
        ArmSide {
            cpu: Cpu::new(),
            timer: GenericTimer::new(),
            cycles: 0,
            insns: 0,
            slept: 0,
            exceptions: 0,
            interrupts: 0,
            waiting: false,
            stopped: None,
            handoff: None,
            bootargs: None,
            kernel_entered: None,
            released_at: None,
        }
    }

    /// Run until the ARM has had every cycle of modelled time since release
    /// (module docs, "Time and scheduling").
    pub fn catch_up(&mut self, m: &mut Machine) {
        let now = m.systimer.cycles_at(gentimer::ARM_HZ);
        let since = now - *self.released_at.get_or_insert(now);
        if since > self.cycles {
            self.run(m, since - self.cycles);
        }
    }

    /// Core 0 as `arm_loader` releases it: read the armstub's hand-off
    /// words, and put the harness's kernel arguments into the device tree
    /// before the first instruction runs.
    pub fn released(m: &mut Machine) -> ArmSide {
        let mut arm = ArmSide::new();
        if let Ok(h) = armstub::read_handoff(m) {
            arm.handoff = Some(h);
            arm.bootargs = Some(armstub::add_bootargs(m, h.dtb, &armstub::BOOTARGS));
        }
        arm
    }

    /// Bring the interrupt inputs up to date: the counter's rate, the timer
    /// and device lines into the GIC, and the GIC's verdict onto the core.
    fn sync(&mut self, m: &mut Machine) {
        self.timer
            .set_hz(self.cycles, m.arm_local.counter_hz().unwrap_or(0));
        for w in Which::ALL {
            m.gic
                .set_ppi_level(0, w.intid(), self.timer.line(w, self.cycles));
        }
        m.gic
            .set_spi_level(gic::ID_MAILBOX, m.mbox.arm_irq_asserted());
        m.gic.set_spi_level(gic::ID_EMMC2, m.emmc2.irq_asserted());
        let [genet_a, genet_b] = m.genet.irq_lines();
        m.gic.set_spi_level(gic::ID_GENET_A, genet_a);
        m.gic.set_spi_level(gic::ID_GENET_B, genet_b);
        let s = m.gic.signal(0);
        self.cpu.irq_line = s.is_some_and(|s| !s.fiq);
        self.cpu.fiq_line = s.is_some_and(|s| s.fiq);
    }

    /// Run for `budget` cycles, or until the core stops.
    pub fn run(&mut self, m: &mut Machine, budget: u64) {
        let end = self.cycles + budget;
        let kernel = self.handoff.map(|h| u64::from(h.kernel));
        while self.cycles < end && self.stopped.is_none() {
            self.sync(m);
            if self.waiting {
                if self.cpu.irq_line || self.cpu.fiq_line {
                    self.waiting = false;
                } else {
                    let wake = self
                        .timer
                        .next_event(self.cycles)
                        .unwrap_or(u64::MAX)
                        .min(end);
                    self.slept += wake - self.cycles;
                    self.cycles = wake;
                    continue;
                }
            }
            let secure = self.cpu.el == 3 || self.cpu.sys.scr_el3 & sysreg::SCR_NS == 0;
            let pc = self.cpu.pc;
            let step = self.cpu.step_system(&mut ArmBus {
                m,
                timer: &mut self.timer,
                cycles: self.cycles,
                secure,
            });
            self.cycles += 1;
            match step {
                Step::Retired => self.insns += 1,
                Step::Wfi => {
                    self.insns += 1;
                    self.waiting = true;
                }
                // No other core to send an event, and an event-less `wfe`
                // may complete at once.
                Step::Wfe => self.insns += 1,
                Step::Took(_) => self.exceptions += 1,
                Step::Interrupt { .. } => self.interrupts += 1,
                Step::Unimplemented(insn) => {
                    self.stopped = Some(ArmStop::Unimplemented {
                        pc,
                        el: self.cpu.el,
                        insn,
                    })
                }
                Step::Unsupported(what) => {
                    self.stopped = Some(ArmStop::Unsupported {
                        pc,
                        el: self.cpu.el,
                        what,
                    })
                }
                Step::Exception(_) => unreachable!("step_system takes exceptions"),
            }
            if self.kernel_entered.is_none() && Some(self.cpu.pc) == kernel {
                self.kernel_entered = Some((self.cycles, self.cpu.el, self.cpu.x[0]));
            }
        }
    }
}

/// Where an ARM physical address lands.
enum Target {
    Ram(u32),
    /// A VPU bus address in the peripheral window.
    Periph(u32),
    Local(u32),
    Gic(u32),
}

/// The ARM's view of the machine for one step.
struct ArmBus<'a> {
    m: &'a mut Machine,
    timer: &'a mut GenericTimer,
    cycles: u64,
    /// The access's security state, for the GIC's banked views.
    secure: bool,
}

impl ArmBus<'_> {
    fn route(&self, addr: u64, size: u32, write: bool) -> Result<Target, Abort> {
        let abort = Abort { addr, write };
        let end = addr.checked_add(u64::from(size)).ok_or(abort)?;
        let within = |base: u32, len: u32| {
            let base = u64::from(base);
            (addr >= base && end <= base + u64::from(len)).then(|| (addr - base) as u32)
        };
        if end <= self.m.ram.len() as u64 {
            Ok(Target::Ram(addr as u32))
        } else if PERIPH.contains(&addr) && end <= PERIPH.end {
            Ok(Target::Periph((addr - PERIPH_TO_BUS) as u32))
        } else if let Some(off) = within(armlocal::BASE, armlocal::SIZE) {
            Ok(Target::Local(off))
        } else if let Some(off) = within(gic::BASE, gic::SIZE) {
            Ok(Target::Gic(off))
        } else {
            Err(abort)
        }
    }

    fn width(size: u32) -> Width {
        match size {
            1 => Width::Byte,
            2 => Width::Half,
            _ => Width::Word,
        }
    }

    fn accessor(&self) -> gic::Accessor {
        gic::Accessor {
            cpu: 0,
            secure: self.secure,
        }
    }

    /// An access of at most 4 bytes.
    fn read32(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
        let w = Self::width(size);
        let r = match self.route(addr, size, false)? {
            Target::Ram(a) => self.m.ram.load(self.m.ram.base() + a, w),
            Target::Periph(a) => self.m.load(a, w),
            Target::Local(o) => self.m.arm_local.read(o, w),
            Target::Gic(o) => {
                let acc = self.accessor();
                self.m.gic.read_as(acc, o, w)
            }
        };
        r.map(u64::from).map_err(|_| Abort { addr, write: false })
    }

    fn write32(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
        let (w, v) = (Self::width(size), value as u32);
        let r = match self.route(addr, size, true)? {
            Target::Ram(a) => {
                let base = self.m.ram.base();
                self.m.ram.store(base + a, w, v)
            }
            Target::Periph(a) => self.m.store(a, w, v),
            Target::Local(o) => self.m.arm_local.write(o, w, v),
            Target::Gic(o) => {
                let acc = self.accessor();
                self.m.gic.write_as(acc, o, w, v)
            }
        };
        r.map_err(|_| Abort { addr, write: true })
    }
}

fn timer_reg(key: u32) -> Option<Reg> {
    Reg::decode(
        key >> 14,
        (key >> 11) & 7,
        (key >> 7) & 15,
        (key >> 3) & 15,
        key & 7,
    )
}

impl Memory for ArmBus<'_> {
    fn read(&mut self, addr: u64, size: u32) -> Result<u64, Abort> {
        if size == 8 {
            let lo = self.read32(addr, 4)?;
            let hi = self.read32(addr.wrapping_add(4), 4)?;
            return Ok(lo | (hi << 32));
        }
        self.read32(addr, size)
    }

    fn write(&mut self, addr: u64, size: u32, value: u64) -> Result<(), Abort> {
        if size == 8 {
            self.write32(addr, 4, value)?;
            return self.write32(addr.wrapping_add(4), 4, value >> 32);
        }
        self.write32(addr, size, value)
    }

    fn sysreg_read(&mut self, key: u32) -> Option<u64> {
        timer_reg(key).map(|r| self.timer.read(r, self.cycles))
    }

    fn sysreg_write(&mut self, key: u32, value: u64) -> bool {
        match timer_reg(key) {
            Some(r) => {
                self.timer.write(r, self.cycles, value);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine_with(code: &[u32]) -> Machine {
        let mut m = Machine::new(1 << 20);
        for (i, w) in code.iter().enumerate() {
            let base = m.ram.base();
            m.ram.store(base + 4 * i as u32, Width::Word, *w).unwrap();
        }
        m
    }

    /// `movz`/`movk` a 32-bit constant into `xd`.
    fn mov32(rd: u32, v: u32) -> [u32; 2] {
        [
            0xD280_0000 | ((v & 0xFFFF) << 5) | rd,
            0xF2A0_0000 | ((v >> 16) << 5) | rd,
        ]
    }

    #[test]
    fn the_peripheral_window_reaches_the_shared_pl011() {
        // x1 = 0xFE201000 (PL011 DR); x0 = 'A'; str w0, [x1]; b .
        let mut code = mov32(1, 0xFE20_1000).to_vec();
        code.extend([0xD280_0820, 0xB900_0020, 0x1400_0000]);
        let mut m = machine_with(&code);
        let mut arm = ArmSide::new();
        arm.run(&mut m, 10);
        assert_eq!(arm.stopped, None);
        assert_eq!(m.uart0.take_output(), b"A");
    }

    #[test]
    fn gic_and_generic_timer_answer() {
        // x1 = GICD_TYPER; ldr w2, [x1]; x3 = ARM local; str wzr,[x3];
        // w4 = 0x80000000, str w4, [x3, #8] (prescaler); nop x10; mrs x5,
        // cntpct_el0; b .
        let mut code = mov32(1, 0xFF84_1004).to_vec();
        code.push(0xB940_0022);
        code.extend(mov32(3, 0xFF80_0000));
        code.push(0xB900_007F);
        code.extend(mov32(4, 0x8000_0000));
        code.push(0xB900_0864);
        code.extend([0xD503_201F; 10]);
        code.extend([0xD53B_E025, 0x1400_0000]);
        let mut m = machine_with(&code);
        let mut arm = ArmSide::new();
        arm.run(&mut m, 40);
        assert_eq!(arm.stopped, None);
        assert_eq!(arm.cpu.x[2], 0xFC67, "GICD_TYPER as measured on the board");
        // The counter started when the prescaler was written, and has run
        // at 54 MHz on a 1.5 GHz clock since: ~11 cycles -> 0 ticks, so
        // just check it is not running wild.
        assert!(arm.cpu.x[5] < 10);
    }

    #[test]
    fn the_arm_keeps_up_with_the_system_timer() {
        // b . — always busy.
        let mut m = machine_with(&[0x1400_0000]);
        let mut arm = ArmSide::new();
        arm.catch_up(&mut m);
        assert_eq!(arm.cycles, 0);
        // 54 VPU cycles = 1 µs = 1500 ARM cycles.
        m.tick(54);
        arm.catch_up(&mut m);
        assert_eq!(arm.cycles, 1500);
        // A `sleep`-style jump of 1 ms: the ARM gets the whole of it.
        m.systimer.jump(1000);
        arm.catch_up(&mut m);
        assert_eq!(arm.cycles, 1500 + 1_500_000);
    }

    #[test]
    fn a_stray_address_aborts_into_the_guest() {
        // ldr x0, [x1] with x1 = 0x1_0000_0000: nothing there. VBAR_EL3 = 0
        // so the sync vector (current EL, SP_ELx) is at 0x200.
        let mut m = machine_with(&[0xF940_0020]);
        let mut arm = ArmSide::new();
        arm.cpu.x[1] = 0x1_0000_0000;
        arm.run(&mut m, 1);
        assert_eq!(arm.exceptions, 1);
        assert_eq!(arm.cpu.pc, 0x200);
        assert_eq!(arm.cpu.sys.far[3], 0x1_0000_0000);
        assert_eq!(arm.cpu.sys.esr[3] >> 26, 0x25);
    }
}
