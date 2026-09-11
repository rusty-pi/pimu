//! Run-loop diagnostics configuration.
//!
//! Every `RVF_*` switch the run loop reads, resolved once. Two reasons this is
//! a struct rather than forty locals at the top of
//! [`Emulator::run`](crate::emulator::Emulator::run):
//!
//! 1. `std::env::var_os` is a locking lookup over the whole environment. These
//!    were once evaluated per instruction, which dominated run time — hoisting
//!    them is load-bearing, not tidiness, and a struct makes that hard to undo
//!    by accident.
//! 2. The run loop is long enough (#25) that the reconnaissance switches were
//!    drowning the parts that actually model hardware.
//!
//! Only *configuration* lives here. The counters and seen-sets the diagnostics
//! accumulate stay in the run loop with the state they describe.
//!
//! What each switch prints is documented in `docs/diagnostics.md`, which is the
//! reference for using them; this is just where they are read.

/// One `RVF_*` switch that is either on or off.
fn flag(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

/// A `RVF_*` switch carrying a hex address, with or without a `0x` prefix.
fn hex(name: &str) -> Option<u32> {
    std::env::var(name)
        .ok()
        .and_then(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok())
}

/// A `RVF_*` switch carrying a decimal number.
fn num<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

#[derive(Debug, Clone, Default)]
pub struct DiagConfig {
    /// Stream the console as it is produced instead of buffering it.
    /// On unless explicitly set to `0` — a boot that wedges should still show
    /// what it printed before it did.
    pub live_console: bool,

    // Tracing.
    /// Arm the instruction trace when core 0 first reaches this address.
    pub trace_on_pc: Option<u32>,
    /// Arm it when this substring appears on the console. Cannot reach code
    /// that runs after the firmware goes quiet — `trace_on_pc` is for that.
    pub trace_on_console: Option<String>,
    /// Stop tracing after this many instructions.
    pub trace_cap: usize,
    /// Trace only control flow, not every instruction.
    pub trace_cf: bool,
    /// Trace every MMIO access.
    pub trace_mmio: bool,
    /// Start the MMIO trace when core 0 reaches this address.
    pub mmio_from: Option<u32>,

    // Traps.
    /// Print registers whenever core 0 reaches one of these addresses.
    pub traps: Vec<u32>,
    /// Ignore traps until this many instructions have retired.
    pub trap_from: u64,
    /// Stop printing a given trap after this many hits.
    pub trap_max: u64,

    // Profiling.
    /// Bucket the core-0 PC and dump the hottest slots on exit.
    pub prof: bool,
    /// The same, attributed per ThreadX thread.
    pub prof_thread: bool,
    /// Print progress every N instructions.
    pub heartbeat: u64,

    // Subsystem logs.
    pub dbg_tick: bool,
    pub dbg_swirq: bool,
    pub dbg_irqtbl: bool,
    pub dbg_mainsus: bool,
    pub dbg_resume: bool,
    pub dbg_evget: bool,
    pub dbg_evset: bool,
    pub dbg_ff: bool,
    /// confzilla / dt-blob schema matching, at this verbosity.
    pub cz_log: Option<u32>,
}

impl DiagConfig {
    pub fn from_env() -> DiagConfig {
        DiagConfig {
            live_console: std::env::var("RVF_LIVE_CONSOLE").as_deref() != Ok("0"),

            trace_on_pc: hex("RVF_TRACE_ON_PC"),
            trace_on_console: std::env::var("RVF_TRACE_ON_CONSOLE").ok(),
            trace_cap: num("RVF_TRACE_CAP").unwrap_or(300_000),
            trace_cf: flag("RVF_TRACE_CF"),
            trace_mmio: flag("RVF_TRACE_MMIO"),
            mmio_from: hex("RVF_MMIO_FROM"),

            traps: std::env::var("RVF_TRAP")
                .ok()
                .map(|v| {
                    v.split(',')
                        .filter_map(|t| {
                            u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok()
                        })
                        .collect()
                })
                .unwrap_or_default(),
            trap_from: num("RVF_TRAP_FROM").unwrap_or(0),
            trap_max: num("RVF_TRAP_MAX").unwrap_or(40),

            prof: flag("RVF_PROF"),
            prof_thread: flag("RVF_PROF_THREAD"),
            heartbeat: num("RVF_HEARTBEAT").unwrap_or(0),

            dbg_tick: flag("RVF_DBG_TICK"),
            dbg_swirq: flag("RVF_DBG_SWIRQ"),
            dbg_irqtbl: flag("RVF_DBG_IRQTBL"),
            dbg_mainsus: flag("RVF_DBG_MAINSUS"),
            dbg_resume: flag("RVF_DBG_RESUME"),
            dbg_evget: flag("RVF_DBG_EVGET"),
            dbg_evset: flag("RVF_DBG_EVSET"),
            dbg_ff: flag("RVF_DBG_FF"),
            cz_log: num("RVF_CZ_LOG"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults have to be the quiet ones: a diagnostic that is on unless
    /// switched off would change what a normal run costs.
    #[test]
    fn nothing_is_enabled_without_an_environment_variable() {
        // `from_env` reads the real environment, so build the quiet case
        // directly — this pins the defaults, not the parsing.
        let d = DiagConfig::default();
        assert!(!d.trace_cf && !d.trace_mmio && !d.prof && !d.dbg_tick);
        assert!(d.traps.is_empty());
        assert_eq!(d.heartbeat, 0);
        assert!(d.trace_on_pc.is_none());
    }

    #[test]
    fn hex_switches_take_a_prefix_or_not() {
        // SAFETY: single-threaded test, and the variable is removed after.
        unsafe {
            std::env::set_var("RVF_TEST_HEX", "0x3ec5ac0c");
            assert_eq!(hex("RVF_TEST_HEX"), Some(0x3EC5_AC0C));
            std::env::set_var("RVF_TEST_HEX", " 3ec5ac0c ");
            assert_eq!(hex("RVF_TEST_HEX"), Some(0x3EC5_AC0C));
            std::env::remove_var("RVF_TEST_HEX");
        }
        assert_eq!(hex("RVF_TEST_HEX"), None);
    }
}
