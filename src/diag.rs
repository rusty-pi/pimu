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
//!
//! The switches that cost something on every step are a build feature, `diag`
//! (Cargo.toml). Without it [`ON`] is `false`, so each per-step check guarded
//! by it folds away at compile time — hoisting the flags into a struct only
//! removed the environment lookups, not the branches, and #29 measured that the
//! branches are what cost. A gated switch set on such a build is reported, not
//! silently ignored.

/// Whether this build has the `diag` feature. Guard every per-step diagnostic
/// with it (`if crate::diag::ON && …`), so a normal build compiles it out.
pub const ON: bool = cfg!(feature = "diag");

/// The switches that only do anything in a `diag` build.
const GATED: &[&str] = &[
    "RVF_TRACE_ON_PC",
    "RVF_TRACE_ON_CONSOLE",
    "RVF_TRACE_CAP",
    "RVF_TRACE_CF",
    "RVF_TRACE_MMIO",
    "RVF_MMIO_FROM",
    "RVF_TRAP",
    "RVF_TRAP_FROM",
    "RVF_TRAP_MAX",
    "RVF_PROF",
    "RVF_PROF_THREAD",
    "RVF_HEARTBEAT",
    "RVF_WATCH",
    "RVF_TCB",
];

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

/// A `RVF_*` switch carrying hex addresses, comma-separated.
fn hex_list(name: &str) -> Vec<u32> {
    std::env::var(name)
        .map(|v| {
            v.split(',')
                .filter_map(|t| u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok())
                .collect()
        })
        .unwrap_or_default()
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
    /// The same, attributed per ThreadX thread — set to the address of the
    /// firmware's current-thread pointer (`_tx_thread_current_ptr`), since
    /// only the firmware knows where that lives. `--log irqtbl` prints `gp`,
    /// and the pointer is findable from a `RVF_TRACE_ON_PC` trace of a context
    /// switch.
    pub prof_thread: Option<u32>,
    /// Print progress every N instructions.
    pub heartbeat: u64,

    /// Decode these ThreadX thread control blocks at exit: where each thread
    /// is parked, and a rough backtrace.
    pub tcbs: Vec<u32>,
}

impl DiagConfig {
    pub fn from_env() -> DiagConfig {
        if !ON {
            let set: Vec<&str> = GATED
                .iter()
                .copied()
                .filter(|n| std::env::var_os(n).is_some())
                .collect();
            if !set.is_empty() {
                eprintln!(
                    "warning: {} ignored: this build has no `diag` feature \
                     (cargo build --release --features diag)",
                    set.join(", ")
                );
            }
            // The live console is not a diagnostic: boot-check reads it.
            return DiagConfig {
                live_console: std::env::var("RVF_LIVE_CONSOLE").as_deref() != Ok("0"),
                ..DiagConfig::default()
            };
        }
        DiagConfig {
            live_console: std::env::var("RVF_LIVE_CONSOLE").as_deref() != Ok("0"),

            trace_on_pc: hex("RVF_TRACE_ON_PC"),
            trace_on_console: std::env::var("RVF_TRACE_ON_CONSOLE").ok(),
            trace_cap: num("RVF_TRACE_CAP").unwrap_or(300_000),
            trace_cf: flag("RVF_TRACE_CF"),
            trace_mmio: flag("RVF_TRACE_MMIO"),
            mmio_from: hex("RVF_MMIO_FROM"),

            traps: hex_list("RVF_TRAP"),
            trap_from: num("RVF_TRAP_FROM").unwrap_or(0),
            trap_max: num("RVF_TRAP_MAX").unwrap_or(40),

            prof: flag("RVF_PROF"),
            prof_thread: hex("RVF_PROF_THREAD"),
            heartbeat: num("RVF_HEARTBEAT").unwrap_or(0),

            tcbs: hex_list("RVF_TCB"),
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
        assert!(!d.trace_cf && !d.trace_mmio && !d.prof);
        assert!(d.traps.is_empty() && d.tcbs.is_empty());
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
