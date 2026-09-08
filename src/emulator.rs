//! Top-level emulator: owns the [`Vpu`] and the [`Machine`] as siblings and
//! drives the run loop.

use std::time::{Duration, Instant};

use crate::bus::{Bus, Width};
use crate::machine::{Console, Machine};
use crate::vpu::{Stop, UnimplPolicy, Vpu};

/// The VPU reset vector `start4.elf` is entered at (`.crypto` region). The
/// BCM2711 boot ROM releases both VPU cores here; the bootloader also jumps
/// here after loading the image. Used as core 1's default entry.
pub const START4_ENTRY: u32 = 0xFEC0_0200;

/// `start4.elf`'s ThreadX-SMP dispatch-module global (`[gp+3672]`, gp = start
/// of `.sdata` = `0x3EE0_2D20`). It holds a pointer to the per-core scheduler
/// object once `_tx_thread_smp` init has registered it. Core 1's very first
/// instructions after the trampoline (`0x3EC2_CC28` → `0x3ED6_50B4`) do
/// `b *([[gp+3672]] + 24)`, so releasing core 1 before this is populated
/// jumps it through a null vtable. We gate the core-1 spawn on it being set.
const SMP_DISPATCH_GLOBAL: u32 = 0x3EE0_3B78;

pub struct Emulator {
    pub cpu: Vpu,
    /// VPU core 1. `None` until `start4.elf`'s trampoline releases it by writing
    /// a start vector to the core-control block; then the run loop interleaves
    /// it with core 0 over the shared bus.
    pub cpu1: Option<Vpu>,
    /// Reset PC for VPU core 1 when the firmware releases it via the core-control
    /// block. Defaults to [`START4_ENTRY`] — the shared VPU reset vector the boot
    /// ROM releases *both* cores at; core 1 runs start4's trampoline from there
    /// and diverges on `version` bit 16. Override for tests / direct-load runs.
    pub core1_entry: Option<u32>,
    /// Latched once the firmware signals it wants core 1 up (a code-address
    /// write to the CoreCtl run-state words). The actual spawn is deferred
    /// until [`SMP_DISPATCH_GLOBAL`] is populated — see there.
    core1_release_armed: bool,
    pub machine: Machine,
}

/// Stopping conditions for [`Emulator::run`].
#[derive(Debug, Clone)]
pub struct RunLimits {
    /// Hard cap on retired instructions. Always set something.
    pub max_steps: u64,
    /// Optional wall-clock cap.
    pub max_wall: Option<Duration>,
    /// Stop cleanly when the PC reaches this address (e.g. an ARM-handoff stub).
    pub stop_pc: Option<u32>,
    /// Stop if the PC revisits the same address this many steps in a row with no
    /// console output (tight spin / wfi-style wait). 0 disables.
    pub idle_spin_limit: u64,
}

impl Default for RunLimits {
    fn default() -> Self {
        RunLimits {
            max_steps: 5_000_000,
            max_wall: Some(Duration::from_secs(30)),
            stop_pc: None,
            idle_spin_limit: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEnd {
    /// The core halted (swi/sleep/breakpoint).
    Halted(Stop),
    /// `stop_pc` reached.
    StopPc(u32),
    /// `max_steps` hit.
    StepLimit,
    /// `max_wall` elapsed.
    TimeLimit,
    /// Detected a tight spin with no output.
    IdleSpin(u32),
    /// VPU core 1 halted (swi/sleep/breakpoint/fault). Core 0 may still have
    /// been running; check the report's `pc` and `core1_pc`.
    Core1Halted(Stop),
    /// Firmware asked the SoC to reset (PM `RSTC`). The caller should re-run
    /// from a fresh machine seeded with the (possibly updated) flash image.
    Reset,
}

#[derive(Debug, Clone)]
pub struct RunReport {
    pub end: RunEnd,
    pub retired: u64,
    pub cycles: u64,
    pub skipped: u64,
    pub stub_hits: u64,
    pub bus_errors: u64,
    pub wall: Duration,
    pub pc: u32,
    /// Everything the console UART transmitted during the run.
    pub console: Vec<u8>,
    /// Distinct unimplemented instructions encountered (reconnaissance).
    pub unimpl: Vec<crate::vpu::exec::UnimplHit>,
    /// Final register file (r0..r31) of core 0.
    pub regs: [u32; 32],
    /// Core 1 state, once it was released.
    pub core1_pc: Option<u32>,
    pub core1_retired: Option<u64>,
    pub core1_end: Option<RunEnd>,
    /// `start4.elf` boot-progress tags (`0xCEC0_2000`), in order.
    pub phase_tags: Vec<u32>,
}

impl Emulator {
    pub fn new(machine: Machine, entry: u32) -> Emulator {
        Emulator {
            cpu: Vpu::new(entry),
            cpu1: None,
            core1_entry: None,
            core1_release_armed: false,
            machine,
        }
    }

    /// Release VPU core 1 at `entry` (the shared trampoline), inheriting core 0's
    /// unimpl policy. Core 1 sets its own exception-vector base from the
    /// trampoline, so leave `exc_vbase` at 0 here.
    fn spawn_core1(&mut self, entry: u32) {
        let mut c1 = Vpu::new(entry);
        c1.core_id = 1;
        c1.on_unimpl = self.cpu.on_unimpl;
        c1.trace = self.cpu.trace;
        c1.trace_cf_only = self.cpu.trace_cf_only;
        c1.trace_cap = self.cpu.trace_cap;
        c1.trace_from = self.cpu.trace_from;
        c1.irq_model = self.cpu.irq_model;
        self.cpu1 = Some(c1);
    }

    /// Bring up VPU core 1 immediately at `entry` (same as core 0). The BCM2711
    /// boot ROM releases both VPU cores at `start4.elf`'s entry at once; they
    /// diverge on `version` bit 16 inside the trampoline.
    pub fn start_smp(&mut self, entry: u32) {
        let mut c1 = Vpu::new(entry);
        c1.core_id = 1;
        c1.on_unimpl = self.cpu.on_unimpl;
        c1.trace = self.cpu.trace;
        c1.trace_cf_only = self.cpu.trace_cf_only;
        c1.trace_cap = self.cpu.trace_cap;
        c1.trace_from = self.cpu.trace_from;
        c1.irq_model = self.cpu.irq_model;
        self.cpu1 = Some(c1);
    }

    pub fn set_console(&mut self, c: Console) {
        self.machine.console = c;
    }

    pub fn set_unimpl_policy(&mut self, p: UnimplPolicy) {
        self.cpu.on_unimpl = p;
    }

    pub fn run(&mut self, limits: &RunLimits) -> RunReport {
        let start = Instant::now();
        let mut console = Vec::new();
        let mut wall_check = 0u64;
        // `RVF_LIVE_CONSOLE=1` echoes UART output to stderr as it happens, so a
        // long `recon` run can be watched instead of waiting for the summary.
        let live_console = std::env::var_os("RVF_LIVE_CONSOLE").is_some();
        // `RVF_MMIO_FROM=<hex>` arms `--trace-mmio`-style logging only once the
        // PC first reaches that address — lets you capture a late boot stage
        // (e.g. start4.elf) without drowning in the bootloader's MMIO.
        let mmio_from = std::env::var("RVF_MMIO_FROM")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
        if mmio_from.is_some() {
            self.machine.mmio_trace = false;
        }
        // `RVF_TRACE_ON_CONSOLE=<substr>` arms the instruction trace the moment
        // that substring appears in the console — for pinning down a code path
        // by the log line that precedes it.
        let trace_on_console = std::env::var("RVF_TRACE_ON_CONSOLE").ok();
        let trace_on_cap: usize = std::env::var("RVF_TRACE_CAP")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300_000);
        let trace_on_cf = std::env::var_os("RVF_TRACE_CF").is_some();
        let mut console_seen = 0usize;

        // Spin detection: over a sliding window of steps, track the min/max PC
        // and whether any console output happened. If the PC stays within a
        // small window for the whole window length with no output, call it a
        // spin (a peripheral poll our stubs never satisfy).
        let win = limits.idle_spin_limit.max(1);
        let mut w_lo = u32::MAX;
        let mut w_hi = 0u32;
        let mut w_steps = 0u64;
        let mut w_output = false;
        // "Progress" = RAM stores + peripheral stores + RAM loads. A memset or
        // memcpy advances the stores; a DRAM memtest read-back advances the
        // loads. A poll loop our stubs never satisfy touches none of them
        // (MMIO loads are not counted), so it still trips.
        let progress_count =
            |m: &Machine| m.ram_writes.wrapping_add(m.mmio_writes).wrapping_add(m.ram_reads);
        let mut progress_at_window = progress_count(&self.machine);
        let mut clo_reads_at_window = self.machine.systimer.clo_reads;
        // Fast path: the exact same taken transfer repeating is a tight spin —
        // *unless* memory traffic keeps advancing.
        let mut last_cf = (u32::MAX, u32::MAX);
        let mut cf_repeat = 0u64;
        let mut progress_at_cf = progress_count(&self.machine);
        let mut clo_reads_at_cf = self.machine.systimer.clo_reads;
        // Busy-wait fast-forward: a repeating control-flow edge that only
        // advances the system-timer counter is a firmware `usleep`
        // (`while now - start < N`). Count the iterations and jump the timer
        // ahead so a multi-millisecond delay doesn't eat the step budget.
        let mut delay_ff = 0u64;
        let mut delay_ff_cf = (u32::MAX, u32::MAX);
        let mut progress_at_delay = progress_count(&self.machine);

        // Interim async-mailbox shim: the real 0x7EE0 service/completer is not
        // modeled yet. Keep this opt-in so callers cannot mistake it for the
        // hardware queue implementation tracked by issue #3.
        let mbox_kick = std::env::var("RVF_MBOX_KICK")
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok());
        let mut mbox_kick_at = 0u64;

        // DEBUG `RVF_GPIOMAN_SHIM=1`: the model never runs the schema-tree
        // apply-walk that invokes `provider_register`, so gpioman's provider
        // list (`[gp+807676]`) stays 0 and every `gpioman_get_pin_num` returns
        // -1 forever. Model the real-hardware behaviour: resolve pin names from
        // the dt-blob pin map (`load_gpioman_pins`), returning -1 + a
        // `gpioman: gpioman_get_pin_num: pin <NAME> not defined` log only for
        // names the dt-blob doesn't define (as real HW does for
        // DISPLAY_DSI_PORT / SDCARD_CONTROL_POWER). Also pass the readiness
        // gate, clear the stuck PMIC errno and skip the PMIC retry backoff so
        // the boot can move past it. Interim hack, not a substitute for real
        // gpioman/PMIC modelling.
        let gpioman_shim = std::env::var_os("RVF_GPIOMAN_SHIM").is_some();
        let mut gpioman_shim_seen: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        // Pin-name → definition for the `gpioman_get_pin_num` shim, from a real
        // dt-blob when one is present (else a small built-in essential set).
        let gpioman_pins = if gpioman_shim {
            load_gpioman_pins()
        } else {
            crate::firmware::dtblob::PinMap::new()
        };
        let dbg_tick = std::env::var_os("RVF_DBG_TICK").is_some();
        let mut tick_deliveries: u64 = 0;
        // Experiment: after the priority-1 timer ISR returns, raise the pending
        // lower-priority software interrupt (vector slot 3) — start4's deferred
        // reschedule path that runs `_tx_timer_interrupt` proper. Gated on
        // `RVF_DEFER_SLOT3`.
        let defer_slot3 = std::env::var_os("RVF_DEFER_SLOT3").is_some();
        let mut slot3_pending = false;
        // Experiment: instead of vectoring the ThreadX tick as a faked IRQ (which
        // the model's cooperative scheduler can't unwind through a real context
        // switch), plain-call start4's own tick-ISR body `0x3ED6583A` — it acks
        // the system-timer compare and runs `[[gp+879584]+56](.., 66)`, the
        // clock-service timeout processing that resumes `msleep`-suspended
        // threads (`powerman` / `do_step`). `_tx_thread_schedule`'s idle loop
        // (`0x3EC40012: sleep; di; b 0x3EC3FFCA`) re-reads the execute pointer
        // each spin, so a thread resumed here is picked up without a faked `rti`.
        let tick_call = std::env::var_os("RVF_TICK_CALL").is_some();
        const TICK_ISR_BODY: u32 = 0x3ED6_583A;

        let probe = std::env::var_os("RVF_PROBE").is_some();
        let mut probe_seen: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
        let mut probe_gp_n: u64 = 0;

        let mut core1_end: Option<RunEnd> = None;
        let end = loop {
            if self.cpu.retired + self.cpu1.as_ref().map_or(0, |c| c.retired) >= limits.max_steps {
                break RunEnd::StepLimit;
            }
            if let Some(pc) = limits.stop_pc {
                if self.cpu.pc() == pc {
                    break RunEnd::StopPc(pc);
                }
            }

            // The firmware's trampoline writes each core's exception-vector base
            // to CoreCtl (0x7E00_2030 / 0x38); pick it up so `swi` traps into
            // the firmware's own handler table. An explicit `--exc-vbase` wins.
            if self.cpu.exc_vbase == 0 && self.machine.corectl.vbase[0] != 0 {
                self.cpu.exc_vbase = self.machine.corectl.vbase[0];
            }

            let pc_before = self.cpu.pc();

            // `_tx_thread_schedule`'s *solicited* context restore (`0x3EC40034`
            // → `bx r26` at `0x3EC4003E`) resumes a thread that yielded via a
            // ThreadX call — it does NOT `rti`, so the model's `in_exception`
            // depth (bumped on the faked timer IRQ, dropped by `Op::Rti`) would
            // stay stuck above 0 after the tick ISR preempts into such a thread.
            // Rebalance it here: reaching this point means we are back in thread
            // context.
            if self.cpu.irq_model && pc_before == 0x3EC4_003E {
                self.cpu.in_exception = 0;
            }

            if let Some(from) = mmio_from {
                if !self.machine.mmio_trace && pc_before == from {
                    self.machine.mmio_trace = true;
                }
            }
            if let Some(obj) = mbox_kick {
                if pc_before == 0x3EC3_FFCA
                    && self.cpu.retired.saturating_sub(mbox_kick_at) > 2000
                {
                    let held = self.machine.load(obj, Width::Word).unwrap_or(0);
                    if held != 0 && held != 1 {
                        mbox_kick_at = self.cpu.retired;
                        self.cpu.regs.set(0, obj);
                        self.cpu.regs.set(26, pc_before);
                        self.cpu.regs.pc = 0x3ED6_51E6;
                    }
                }
            }
            if probe {
                // PMIC I²C retry loop: `0x3EDD21A0` checks the I²C call's return
                // (`r6`) — 0 == success, anything else retries until a 1 s
                // timeout. Log what the model's BSC is handing back.
                if pc_before == 0x3EDD_21A0 {
                    eprintln!(
                        "[probe] pmic i2c ret r6={:#x} @{}",
                        self.cpu.regs.get(6),
                        self.cpu.retired
                    );
                }
                // `0x3EDD22D4`: reads the transfer-status byte `[r0+0]`; `>= 2`
                // => "PMIC: timeout reading reg" at `0x3EDD22E4`.
                if pc_before == 0x3EDD_22D4 {
                    let r0 = self.cpu.regs.get(0);
                    let st = self.machine.load(r0, Width::Word).unwrap_or(0xdead);
                    eprintln!(
                        "[probe] pmic status @{r0:#x} = {st:#x}  retired={}",
                        self.cpu.retired
                    );
                }
                // DA9090 driver: `0x3EC8C4C0` pmic_read entry, `0x3EC8C4EE`
                // reads the thread status byte `[r0]` (>= 2 => timeout log at
                // `0x3EC8C4FE`).
                if pc_before == 0x3EC8_C4C0 {
                    eprintln!(
                        "[probe] da9090 pmic_read entry r6={:#x} @{}",
                        self.cpu.regs.get(6),
                        self.cpu.retired
                    );
                }
                if pc_before == 0x3EC8_C4EE {
                    let r0 = self.cpu.regs.get(0);
                    let b = self.machine.load(r0, Width::Byte).unwrap_or(0xdead);
                    eprintln!("[probe] da9090 status byte @{r0:#x} = {b:#x}");
                    // HACK: the one-time init at `0x3EDA545C` leaves the global
                    // errno (`gp+328824`) == 2 and nothing in the model clears
                    // it; the DA9090 PMIC read then reads it as a timeout. Clear
                    // it here so a successful I2C read isn't masked.
                    if std::env::var_os("RVF_PMIC_HACK").is_some() && b == 2 {
                        let _ = self.machine.store(r0, Width::Byte, 0);
                        eprintln!("[probe]   -> forced errno 0");
                    }
                }
                // `0x3ECC9C78` gpioman_get_pin_num — is the provider list ever
                // populated? Log the state struct on the LEDS lookups.
                if pc_before == 0x3ECC_9C78 {
                    probe_gp_n += 1;
                    if probe_gp_n <= 400 {
                        let gp = self.cpu.regs.get(24);
                        let a = self.machine.load(gp + 807672, Width::Word).unwrap_or(0xdead);
                        let b = self.machine.load(gp + 807676, Width::Word).unwrap_or(0xdead);
                        let c = self.machine.load(gp + 839404, Width::Word).unwrap_or(0xdead);
                        let p = self.cpu.regs.get(0);
                        let mut name = String::new();
                        for i in 0..24 {
                            match self.machine.load(p + i, Width::Byte) {
                                Ok(0) | Err(_) => break,
                                Ok(ch) => name.push(ch as u8 as char),
                            }
                        }
                        eprintln!(
                            "[probe] get_pin_num#{probe_gp_n} {name:?} gp={gp:#x} [+672]={a:#x} [+676]={b:#x} [+839404]={c:#x} @{}",
                            self.cpu.retired
                        );
                    }
                }
                // `0x3EDA4EEC` — the "record an error" helper. When called with
                // r1 = &global_errno and *errno == 0 it stamps errno = 2.
                if pc_before == 0x3EDA_4EEC && self.cpu.regs.get(1) == 0x3EE5_3198 {
                    let lr = self.cpu.regs.get(26);
                    eprintln!(
                        "[probe] errno-set caller lr={lr:#x} r0={:#x} @{}",
                        self.cpu.regs.get(0),
                        self.cpu.retired
                    );
                }
            }
            if probe {
                // Recon: gpioman config path. `0x3ECC9878` = gpioman_configure,
                // `0x3ECC9EC4` = provider_register (links a node into the pin
                // list `[gp+807676]`), `0x3ECC9DC4` = the never-called flag
                // setter. Log entry with the state words that decide the branch.
                let tag = match pc_before {
                    0x3ECC_9878 => Some("gpioman_configure"),
                    0x3ECC_9EC4 => Some("provider_register"),
                    0x3ECC_9DC4 => Some("flag_setter"),
                    0x3ECC_9D78 => Some("faillog+retry_sched"),
                    0x3ECC_93AC => Some("apply"),
                    _ => None,
                };
                if let Some(t) = tag {
                    let gp = self.cpu.regs.get(24);
                    let mut l = |o: u32| {
                        self.machine
                            .load(gp.wrapping_add(o), Width::Word)
                            .unwrap_or(0xdead)
                    };
                    let (a, b, c, d) = (l(807672), l(807676), l(807680), l(839404));
                    eprintln!(
                        "[probe] {t} @{} lr={:#x} r0={:#x} [gp+807672]={a:#x} [gp+807676]={b:#x} [gp+807680]={c:#x} [gp+839404]={d:#x}",
                        self.cpu.retired,
                        self.cpu.regs.get(26),
                        self.cpu.regs.get(0),
                    );
                    if probe_seen.insert(t) {
                        eprintln!("[probe]   cf_trace tail for first {t}:");
                        let cf = &self.cpu.cf_trace;
                        for &(from, to) in cf.iter().skip(cf.len().saturating_sub(48)) {
                            eprintln!("[probe]     {from:#010x} -> {to:#010x}");
                        }
                    }
                }
            }
            if gpioman_shim {
                // `0x3EC307DA` PLL frequency-tuning loop: `r6 = get_pll_freq(id)`
                // (`0x3EC300CE`), and while `r6 != r8` (the target) it re-sets
                // the divider, `msleep(300)` and re-measures. We don't model the
                // analogue PLL / frequency counter, so the measured value never
                // reaches the target and the loop spins. Force the just-measured
                // value (`r0`, about to land in `r6`) to the target.
                if pc_before == 0x3EC3_07F8 {
                    let target = self.cpu.regs.get(8);
                    self.cpu.regs.set(0, target);
                }
                // `0x3ED570C2` — the clock-manager rate-calibration loop:
                // `r7 = measured(r3) - target(r6)`; it re-`msleep`s + re-measures
                // while `|r7| >= 200 kHz`. Same story — the frequency monitor is
                // stubbed, so measured never converges. Force measured = target.
                if pc_before == 0x3ED5_70C2 {
                    let target = self.cpu.regs.get(6);
                    self.cpu.regs.set(3, target);
                }
                // `0x3ECC9C78` = gpioman_get_pin_num(name): the provider list
                // (`[gp+807676]`) is never populated, so the real function walks
                // an empty list and returns -1 for everything. Resolve from the
                // dt-blob pin map instead:
                //  - name has a `number` → return it;
                //  - name present but `type = "absent"` / no number → -1,
                //    silently (real HW knows the name, just has no pin);
                //  - name not in the map → -1 and log
                //    `gpioman: gpioman_get_pin_num: pin <NAME> not defined`,
                //    as real HW does for e.g. DISPLAY_DSI_PORT /
                //    SDCARD_CONTROL_POWER (see examples-on-real-hardware/).
                if pc_before == 0x3ECC_9C78 {
                    let p = self.cpu.regs.get(0);
                    let mut name = String::new();
                    for i in 0..32 {
                        match self.machine.load(p + i, Width::Byte) {
                            Ok(0) | Err(_) => break,
                            Ok(c) => name.push(c as u8 as char),
                        }
                    }
                    match gpioman_pins.get(&name).and_then(|d| d.number) {
                        Some(n) => self.cpu.regs.set(0, n),
                        None => {
                            if !gpioman_pins.contains_key(&name)
                                && gpioman_shim_seen.insert(name.clone())
                            {
                                eprintln!("gpioman: gpioman_get_pin_num: pin {name} not defined");
                            }
                            self.cpu.regs.set(0, u32::MAX);
                        }
                    }
                    self.cpu.regs.pc = self.cpu.regs.get(26);
                }
                // `0x3ECCA0A0` = gpioman lookup by pin number, gated on the
                // readiness flag `[gp+4148]`; returns -1 while gpioman is not
                // ready. Pass a plausible SoC pin straight through.
                if pc_before == 0x3ECC_A0A0 {
                    let n = self.cpu.regs.get(0);
                    if n < 54 {
                        self.cpu.regs.pc = self.cpu.regs.get(26);
                    }
                }
                // `0x3EC31A6A` = a delay/backoff helper (`(r0>>4 & 0xF)` x
                // `usleep(400 ms)` x2). The DA9090 PMIC bring-up calls it
                // (`lr` in `0x3EC8Cxxx`) after every failed register read, and
                // the read never succeeds because the model has no PMIC on the
                // bit-banged GPIO I2C — so this is ~thousands of 3.2 s backoffs.
                // Skip it for that caller so the retry at least runs at speed.
                if pc_before == 0x3EC3_1A6A {
                    let lr = self.cpu.regs.get(26);
                    if (0x3EC8_C000..0x3EC8_D000).contains(&lr)
                        || (0x3EDD_2000..0x3EDD_2400).contains(&lr)
                    {
                        self.cpu.regs.pc = lr;
                    }
                }
                // DA9090 PMIC read paths (`0x3EC8C4EE`, `0x3EDD22D4`,
                // `0x3EDD2318` all read the errno byte `[r0]` = `[gp+328824]`,
                // `>= 2` => "PMIC: timeout"): the one-time init `0x3EDA545C`
                // leaves that global == 2 and nothing in the model clears it.
                // Pin it to 1 — `< 2` passes the check, and unlike 0 it won't be
                // re-stamped to 2 by `0x3EDA4EEC` (which only stamps at 0).
                if matches!(pc_before, 0x3EC8_C4EE | 0x3EDD_22D4 | 0x3EDD_2318) {
                    let r0 = self.cpu.regs.get(0);
                    if self.machine.load(r0, Width::Byte).unwrap_or(0) >= 2 {
                        let _ = self.machine.store(r0, Width::Byte, 1);
                    }
                }
            }
            self.machine.watch_pc = pc_before;
            let exc_depth_before = self.cpu.in_exception;
            let step = self.cpu.step(&mut self.machine);
            self.machine.tick(1);

            // start4's interrupt entry (`0x3ED18004`) bumps a nesting counter
            // (`[gp+4420]`) and indexes a per-nesting IRQ record by it; the
            // matching decrement lives in `_tx_thread_context_restore`, which
            // the model's `rti` shortcut skips. Left uncorrected the counter
            // grows without bound and the record index walks off into garbage
            // after a few interrupts. Undo one increment each time an `rti`
            // unwinds a faked interrupt.
            if self.cpu.irq_model && self.cpu.in_exception < exc_depth_before {
                const IRQ_NEST: u32 = 0x3EE0_3E64; // gp + 4420
                if let Ok(n) = self.machine.load(IRQ_NEST, Width::Word) {
                    if n > 0 && n != 0xFFFF_FFFF {
                        let _ = self.machine.store(IRQ_NEST, Width::Word, n - 1);
                    }
                }
            }

            // Periodic timer interrupt. Real VC4 hardware raises an interrupt
            // when a system-timer compare matches the free-running counter and
            // that source is enabled; the model otherwise only fakes delivery
            // on the `sleep` instruction, so nothing preempts a thread that
            // busy-waits or loops on `msleep` (`do_step`, `0x3ED5766E`). Vector
            // through the firmware's own handler exactly as `Op::Sleep` does,
            // whenever a compare has fired, we are in thread context, and
            // interrupts are enabled.
            let tick_due = self.machine.systimer.take_tick_pending();
            if self.cpu.irq_model
                && tick_due
                && self.cpu.in_exception == 0
                && self.cpu.irq_enabled()
                && self.cpu.exc_vbase != 0
            {
                if let Some(slot) = self.machine.timer_tick_slot() {
                    if dbg_tick {
                        tick_deliveries += 1;
                        if tick_deliveries <= 30 || tick_deliveries % 500 == 0 {
                            let vb = self.cpu.exc_vbase;
                            let h = self.machine.load(vb.wrapping_add(slot * 4), Width::Word);
                            eprintln!(
                                "[tick] #{tick_deliveries} slot={slot} vbase={vb:#x} handler={h:x?} resume={:#x} retired={} nest={:#x}",
                                self.cpu.pc(),
                                self.cpu.retired,
                                self.machine.load(0x3EE0_3E64, Width::Word).unwrap_or(0xdead),
                            );
                        }
                    }
                    if tick_call && slot == 1 {
                        // Plain-call the tick-ISR body: lr = resume pc, it
                        // returns via `pop pc` (or never returns if it switches
                        // to a resumed thread).
                        let resume = self.cpu.pc();
                        self.cpu.regs.set(crate::vpu::reg::LR, resume);
                        self.cpu.regs.pc = TICK_ISR_BODY;
                    } else {
                        self.cpu.vector_irq(&mut self.machine, slot);
                    }
                    if defer_slot3 && slot == 1 {
                        slot3_pending = true;
                    }
                    // Experiment (`RVF_TICK_CORE1`): also vector the tick on
                    // core 1 — ThreadX-SMP may run `_tx_timer_interrupt` there.
                    if std::env::var_os("RVF_TICK_CORE1").is_some() {
                        if let Some(c1) = self.cpu1.as_mut() {
                            if c1.in_exception == 0 && c1.irq_enabled() && c1.exc_vbase != 0 {
                                c1.vector_irq(&mut self.machine, slot);
                            }
                        }
                    }
                }
            }

            // Deferred software interrupt: once the priority-1 timer ISR has
            // unwound back to thread context, vector the pending slot-3 SW IRQ.
            if defer_slot3
                && slot3_pending
                && self.cpu.irq_model
                && self.cpu.in_exception == 0
                && self.cpu.irq_enabled()
                && self.cpu.exc_vbase != 0
            {
                slot3_pending = false;
                if dbg_tick {
                    eprintln!(
                        "[slot3] deferred SW IRQ at resume={:#x} retired={}",
                        self.cpu.pc(),
                        self.cpu.retired,
                    );
                }
                self.cpu.vector_irq(&mut self.machine, 3);
            }

            if self.machine.mmio_trace && !self.machine.mmio_events.is_empty() {
                for (addr, w, val, write) in self.machine.mmio_events.drain(..) {
                    eprintln!(
                        "mmio {:#010x}  {}{}  {:#010x} <- {:#0width$x}",
                        pc_before,
                        if write { "W" } else { "R" },
                        w,
                        addr,
                        val,
                        width = (w as usize) * 2 + 2,
                    );
                }
            }

            // Firmware asked for a SoC reset (PM RSTC) — stop so the caller can
            // re-run from a fresh machine seeded with the updated flash.
            if self.machine.pm.take_reset() {
                break RunEnd::Reset;
            }

            // Core 1 (re)enters at the shared start4 reset vector, not core 0's
            // `entry` — which on the EEPROM path is the *bootcode*, long gone by
            // the time start4 brings its sibling up.
            //
            // The CoreCtl run-state write the model keys on also overlaps the
            // interrupt-priority words, so it fires early (during driver
            // bring-up) — well before start4's ThreadX-SMP init registers the
            // per-core scheduler object that core 1 immediately dereferences.
            // Latch the intent, but defer the spawn until that object exists.
            if self.cpu1.is_none() {
                if self.machine.corectl.take_core1_release() {
                    self.core1_release_armed = true;
                }
                if self.core1_release_armed
                    && self
                        .machine
                        .load(SMP_DISPATCH_GLOBAL, Width::Word)
                        .unwrap_or(0)
                        != 0
                {
                    let entry = self.core1_entry.unwrap_or(START4_ENTRY);
                    self.spawn_core1(entry);
                }
            }
            // Interleave one core-1 step per core-0 step over the shared bus.
            if let Some(c1) = self.cpu1.as_mut() {
                if c1.exc_vbase == 0 && self.machine.corectl.vbase[1] != 0 {
                    c1.exc_vbase = self.machine.corectl.vbase[1];
                }
                if !c1.is_stopped() {
                    if let crate::vpu::Step::Stopped = c1.step(&mut self.machine) {
                        core1_end =
                            Some(RunEnd::Core1Halted(c1.stopped.clone().expect("stop reason")));
                    }
                }
            }

            let fresh = self.machine.take_console_output();
            let had_output = !fresh.is_empty();
            if live_console && had_output {
                use std::io::Write;
                let _ = std::io::stderr().write_all(&fresh);
            }
            console.extend_from_slice(&fresh);
            if let Some(needle) = &trace_on_console {
                if !self.cpu.trace && console.len() > console_seen {
                    let from = console_seen.saturating_sub(needle.len());
                    if String::from_utf8_lossy(&console[from..]).contains(needle.as_str()) {
                        self.cpu.trace = true;
                        self.cpu.trace_armed = true;
                        self.cpu.trace_cf_only = trace_on_cf;
                        self.cpu.trace_cap = trace_on_cap;
                        // Also stream peripheral accesses while the trace is
                        // armed (RVF_TRACE_MMIO=1) — handy for pinning down an
                        // unmodelled block like the I2C BSC.
                        if std::env::var_os("RVF_TRACE_MMIO").is_some() {
                            self.machine.mmio_trace = true;
                        }
                    }
                    console_seen = console.len();
                }
            }

            match step {
                crate::vpu::Step::Stopped => {
                    let stop = self.cpu.stopped.clone().expect("stopped without reason");
                    break RunEnd::Halted(stop);
                }
                crate::vpu::Step::Ran => {}
            }
            // Core 1 halting does not stop core 0 — record it and carry on.

            if limits.idle_spin_limit > 0 {
                // A loop that keeps reading the free-running system timer is a
                // firmware `usleep` — time-bounded, so not a hung spin however
                // many iterations it takes. `max_steps` / `max_wall` still cap
                // a pathological one.
                let timer_polling = self.machine.systimer.clo_reads != clo_reads_at_cf;
                // Firmware busy-wait on the free-running counter: same edge,
                // timer advancing, nothing else changing, no output. Let it
                // build up, then skip the counter forward a slice at a time.
                if let Some(&cf) = self.cpu.cf_trace.last() {
                    let p = progress_count(&self.machine);
                    if cf == delay_ff_cf && timer_polling && !had_output && p == progress_at_delay
                    {
                        delay_ff += 1;
                        if delay_ff >= 2_000 {
                            self.machine.systimer.skip_ahead(4_000);
                            delay_ff = 0;
                        }
                    } else {
                        delay_ff = 0;
                        delay_ff_cf = cf;
                        progress_at_delay = p;
                    }
                }
                if let Some(&cf) = self.cpu.cf_trace.last() {
                    // A bare read-only poll counts as a spin, but firmware
                    // delay/lock loops legitimately iterate 10k+ times before
                    // giving up, so the threshold is generous.
                    let progress = progress_count(&self.machine);
                    let progressing = progress != progress_at_cf || timer_polling;
                    if cf == last_cf && !had_output && !progressing {
                        cf_repeat += 1;
                        if cf_repeat >= 200_000 {
                            break RunEnd::IdleSpin(cf.0);
                        }
                    } else {
                        cf_repeat = 0;
                        last_cf = cf;
                        progress_at_cf = progress;
                    }
                }
                clo_reads_at_cf = self.machine.systimer.clo_reads;

                w_lo = w_lo.min(pc_before);
                w_hi = w_hi.max(pc_before);
                w_output |= had_output;
                w_steps += 1;
                if w_steps >= win {
                    let clo_delta =
                        self.machine.systimer.clo_reads.wrapping_sub(clo_reads_at_window);
                    let stalled = progress_count(&self.machine) == progress_at_window
                        && self.machine.systimer.clo_reads == clo_reads_at_window;
                    if !w_output && stalled && w_hi.wrapping_sub(w_lo) <= 4096 {
                        break RunEnd::IdleSpin(w_lo);
                    }
                    // A window that spent >1/8 of its instructions reading the
                    // free-running counter, with no console output, is
                    // dominated by a firmware `usleep(n)`. The plain delay-ff
                    // path above stalls on it because the periodic tick ISR
                    // keeps bumping `progress` and briefly widening the PC
                    // range. Jump the counter forward so a multi-100 ms
                    // rail-settle delay (PMIC bring-up does several) doesn't run
                    // in real time.
                    // A tight PC window (a single small loop) that spent the
                    // whole window polling the free-running counter, no console
                    // output, is a firmware `usleep(n)` — jump the counter
                    // forward so it doesn't run in real time. A wider window
                    // (loop punctuated by a tick ISR) needs a firmer CLO-read
                    // ratio to be sure it isn't doing real work.
                    let tight = w_hi.wrapping_sub(w_lo) <= 0x40;
                    let ff = !w_output
                        && if tight {
                            clo_delta > win / 20
                        } else {
                            clo_delta > win / 8 && self.cpu.in_exception == 0
                        };
                    if ff {
                        self.machine.systimer.jump(200_000);
                    }
                    if std::env::var_os("RVF_DBG_FF").is_some() {
                        eprintln!(
                            "[ff] win close: clo_delta={clo_delta} w=[{w_lo:#x}..{w_hi:#x}] out={w_output} exc={} ff={ff} @{}",
                            self.cpu.in_exception, self.cpu.retired
                        );
                    }
                    clo_reads_at_window = self.machine.systimer.clo_reads;
                    w_lo = u32::MAX;
                    w_hi = 0;
                    w_steps = 0;
                    w_output = false;
                    progress_at_window = progress_count(&self.machine);
                }
            }

            wall_check += 1;
            if let Some(max) = limits.max_wall {
                if wall_check.is_multiple_of(65_536) && start.elapsed() >= max {
                    break RunEnd::TimeLimit;
                }
            }
        };

        console.extend_from_slice(&self.machine.take_console_output());

        let mut unimpl = self.cpu.unimpl.clone();
        unimpl.sort_by(|a, b| b.count.cmp(&a.count).then(a.pc.cmp(&b.pc)));

        RunReport {
            end,
            retired: self.cpu.retired,
            cycles: self.cpu.cycles,
            skipped: self.cpu.skipped,
            stub_hits: self.machine.stub_hits,
            bus_errors: self.machine.bus_errors,
            wall: start.elapsed(),
            pc: self.cpu.pc(),
            console,
            unimpl,
            regs: std::array::from_fn(|i| self.cpu.regs.get(i)),
            core1_pc: self.cpu1.as_ref().map(|c| c.pc()),
            core1_retired: self.cpu1.as_ref().map(|c| c.retired),
            core1_end,
            phase_tags: self.machine.phase_tags.clone(),
        }
    }
}

/// Build the pin-name → definition map the `gpioman_get_pin_num` shim uses.
///
/// Prefers a real dt-blob — `$RVF_DT_BLOB`, else `firmware/dt-blob.bin` — so the
/// model resolves exactly the pins the firmware would. When no dt-blob is
/// readable/parseable it falls back to a small compiled-in set covering the
/// pins the boot would otherwise wedge on (real HW in that case uses its
/// built-in default table and just logs a few extra "pin not defined" lines).
fn load_gpioman_pins() -> crate::firmware::dtblob::PinMap {
    use crate::firmware::dtblob::{pin_map, PinDef, PinMap};

    let path = std::env::var_os("RVF_DT_BLOB")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "firmware/dt-blob.bin".into());
    if let Ok(bytes) = std::fs::read(&path) {
        if let Some(m) = pin_map(&bytes, &["pins_4b"]) {
            eprintln!(
                "[gpioman-shim] pin map from {} ({} pins)",
                path.display(),
                m.len()
            );
            return m;
        }
    }

    // Essential fallback: the activity / power LEDs are the pins arm_loader
    // retries forever if unresolved (pins_4b numbers). Everything else the
    // model can leave "not defined" — callers cope.
    let mut m = PinMap::new();
    for (name, number) in [("LEDS_DISK_ACTIVITY", 42u32), ("LEDS_PWR_OK", 2)] {
        m.insert(
            name.to_string(),
            PinDef {
                number: Some(number),
                kind: Some("external".to_string()),
            },
        );
    }
    eprintln!(
        "[gpioman-shim] no dt-blob; built-in essential pin set ({} pins)",
        m.len()
    );
    m
}
