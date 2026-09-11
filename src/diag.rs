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
//! The reading is hosted-only. A `no_std` build has no environment, so the
//! four accessors below answer "unset" and a bare-metal frontend constructs
//! the [`DiagConfig`] it wants directly. That keeps every call site one shape
//! — `crate::diag::flag("RVF_DBG_SPI")` — instead of a `cfg` per switch.

use alloc::string::String;
use alloc::vec::Vec;

/// One `RVF_*` switch that is either on or off.
pub fn flag(name: &str) -> bool {
    let _ = name;
    #[cfg(feature = "std")]
    {
        std::env::var_os(name).is_some()
    }
    #[cfg(not(feature = "std"))]
    {
        false
    }
}

/// A `RVF_*` switch's raw value.
pub fn var(name: &str) -> Option<String> {
    let _ = name;
    #[cfg(feature = "std")]
    {
        std::env::var(name).ok()
    }
    #[cfg(not(feature = "std"))]
    {
        None
    }
}

/// A `RVF_*` switch carrying a hex address, with or without a `0x` prefix.
pub fn hex(name: &str) -> Option<u32> {
    var(name).and_then(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok())
}

/// A `RVF_*` switch carrying a decimal number.
pub fn num<T: core::str::FromStr>(name: &str) -> Option<T> {
    var(name).and_then(|v| v.trim().parse().ok())
}

/// A `RVF_*` switch carrying a comma-separated list of hex addresses.
pub fn hex_list(name: &str) -> Vec<u32> {
    var(name)
        .map(|v| {
            v.split(',')
                .filter_map(|t| u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Diagnostic output: `eprintln!` in a hosted build, and in a `no_std` one the
/// sink the frontend installed with [`set_sink`] — dropped if it installed
/// none. Everything the library prints is a diagnostic; the modelled UART
/// console goes through [`crate::machine::Machine`], not through here.
#[macro_export]
macro_rules! diag_eprintln {
    ($($arg:tt)*) => {
        $crate::diag::emit_line(::core::format_args!($($arg)*))
    };
}

/// One diagnostic line, newline included.
#[cfg(feature = "std")]
pub fn emit_line(args: core::fmt::Arguments<'_>) {
    eprintln!("{args}");
}

#[cfg(not(feature = "std"))]
pub fn emit_line(args: core::fmt::Arguments<'_>) {
    emit(args);
    emit(format_args!("\n"));
}

/// Bytes from the modelled UART, streamed as the firmware produces them
/// (`RVF_LIVE_CONSOLE`). Verbatim: no newline, no lossy framing, because this
/// is the transcript `scripts/boot-check.sh` diffs against the golden.
#[cfg(feature = "std")]
pub fn emit_console(bytes: &[u8]) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(bytes);
}

#[cfg(not(feature = "std"))]
pub fn emit_console(bytes: &[u8]) {
    emit(format_args!(
        "{}",
        alloc::string::String::from_utf8_lossy(bytes)
    ));
}

#[cfg(not(feature = "std"))]
mod sink {
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// Receives formatted output and writes it verbatim — the newline, when
    /// there is one, is already in the arguments.
    pub type Sink = fn(core::fmt::Arguments<'_>);

    /// Null pointer = no sink installed, so diagnostics are dropped.
    static SINK: AtomicUsize = AtomicUsize::new(0);

    pub fn set_sink(f: Sink) {
        SINK.store(f as usize, Ordering::Relaxed);
    }

    pub fn emit(args: core::fmt::Arguments<'_>) {
        let p = SINK.load(Ordering::Relaxed);
        if p == 0 {
            return;
        }
        // SAFETY: `p` is non-null, so it was stored by `set_sink` from a
        // `Sink` and nothing else ever writes this slot.
        let f = unsafe { core::mem::transmute::<usize, Sink>(p) };
        f(args);
    }
}

#[cfg(not(feature = "std"))]
pub use sink::{emit, set_sink, Sink};

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
    /// only the firmware knows where that lives. `RVF_DBG_IRQTBL` prints `gp`,
    /// and the pointer is findable from a `RVF_TRACE_ON_PC` trace of a context
    /// switch.
    pub prof_thread: Option<u32>,
    /// Print progress every N instructions.
    pub heartbeat: u64,

    // Subsystem logs.
    pub dbg_tick: bool,
    pub dbg_swirq: bool,
    /// Dump the firmware's per-source interrupt handler table at exit. Derived
    /// from `gp`, so it survives a firmware whose layout moved.
    pub dbg_irqtbl: bool,
    pub dbg_ff: bool,
}

impl DiagConfig {
    /// The configuration a build with no environment starts from: everything
    /// quiet except the live console, which matches `from_env`'s default.
    pub fn quiet() -> DiagConfig {
        DiagConfig {
            live_console: true,
            ..DiagConfig::default()
        }
    }

    #[cfg(feature = "std")]
    pub fn from_env() -> DiagConfig {
        DiagConfig {
            live_console: var("RVF_LIVE_CONSOLE").as_deref() != Some("0"),

            trace_on_pc: hex("RVF_TRACE_ON_PC"),
            trace_on_console: var("RVF_TRACE_ON_CONSOLE"),
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

            dbg_tick: flag("RVF_DBG_TICK"),
            dbg_swirq: flag("RVF_DBG_SWIRQ"),
            dbg_irqtbl: flag("RVF_DBG_IRQTBL"),
            dbg_ff: flag("RVF_DBG_FF"),
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
    #[cfg(feature = "std")]
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
