//! Run-loop diagnostics configuration: every `PIMU_*` switch the run loop
//! reads, resolved once.
//!
//! A struct rather than locals at the top of the run loop because
//! `std::env::var_os` is a locking lookup over the whole environment; reading
//! these per instruction dominates run time, so hoisting them is load-bearing.
//! Only configuration lives here — the counters and seen-sets stay with the
//! state they describe. What each switch prints is in `docs/diagnostics.md`.
//!
//! The switches that cost something per step are behind the `diag` build
//! feature: without it [`ON`] is `false` and each guarded check folds away at
//! compile time. A gated switch set on a build without the feature is reported
//! rather than silently ignored.

/// Whether this build has the `diag` feature. Guard every per-step diagnostic
/// with it (`if crate::diag::ON && …`), so a normal build compiles it out.
pub const ON: bool = cfg!(feature = "diag");

const GATED: &[&str] = &[
    "PIMU_TRACE_ON_PC",
    "PIMU_TRACE_ON_CONSOLE",
    "PIMU_TRACE_CAP",
    "PIMU_TRACE_CF",
    "PIMU_TRACE_MMIO",
    "PIMU_MMIO_FROM",
    "PIMU_TRAP",
    "PIMU_TRAP_FROM",
    "PIMU_TRAP_MAX",
    "PIMU_PROF",
    "PIMU_PROF_THREAD",
    "PIMU_HEARTBEAT",
    "PIMU_WATCH",
    "PIMU_TCB",
    "PIMU_ARM_BLOCKS",
];

fn flag(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn hex(name: &str) -> Option<u32> {
    std::env::var(name)
        .ok()
        .and_then(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok())
}

fn hex_list(name: &str) -> Vec<u32> {
    std::env::var(name)
        .map(|v| {
            v.split(',')
                .filter_map(|t| u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn num<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

#[derive(Debug, Clone, Default)]
pub struct DiagConfig {
    /// Stream the console as it is produced; on unless set to `0`, so a boot
    /// that wedges still shows what it printed.
    pub live_console: bool,

    pub trace_on_pc: Option<u32>,
    /// Arm it when this substring appears on the console; cannot reach code
    /// that runs after the firmware goes quiet.
    pub trace_on_console: Option<String>,
    pub trace_cap: usize,
    pub trace_cf: bool,
    pub trace_mmio: bool,
    pub mmio_from: Option<u32>,

    pub traps: Vec<u32>,
    pub trap_from: u64,
    pub trap_max: u64,

    pub prof: bool,
    /// The same, attributed per ThreadX thread; set to the address of the
    /// firmware's current-thread pointer, which only the firmware knows.
    pub prof_thread: Option<u32>,
    pub heartbeat: u64,

    /// Decode these ThreadX thread control blocks at exit.
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
            return DiagConfig {
                live_console: std::env::var("PIMU_LIVE_CONSOLE").as_deref() != Ok("0"),
                ..DiagConfig::default()
            };
        }
        DiagConfig {
            live_console: std::env::var("PIMU_LIVE_CONSOLE").as_deref() != Ok("0"),

            trace_on_pc: hex("PIMU_TRACE_ON_PC"),
            trace_on_console: std::env::var("PIMU_TRACE_ON_CONSOLE").ok(),
            trace_cap: num("PIMU_TRACE_CAP").unwrap_or(300_000),
            trace_cf: flag("PIMU_TRACE_CF"),
            trace_mmio: flag("PIMU_TRACE_MMIO"),
            mmio_from: hex("PIMU_MMIO_FROM"),

            traps: hex_list("PIMU_TRAP"),
            trap_from: num("PIMU_TRAP_FROM").unwrap_or(0),
            trap_max: num("PIMU_TRAP_MAX").unwrap_or(40),

            prof: flag("PIMU_PROF"),
            prof_thread: hex("PIMU_PROF_THREAD"),
            heartbeat: num("PIMU_HEARTBEAT").unwrap_or(0),

            tcbs: hex_list("PIMU_TCB"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are the quiet ones: anything on by default would change
    /// what a normal run costs.
    #[test]
    fn nothing_is_enabled_without_an_environment_variable() {
        // Build the quiet case directly: this pins the defaults, not parsing.
        let d = DiagConfig::default();
        assert!(!d.trace_cf && !d.trace_mmio && !d.prof);
        assert!(d.traps.is_empty() && d.tcbs.is_empty());
        assert_eq!(d.heartbeat, 0);
        assert!(d.trace_on_pc.is_none());
    }

    #[test]
    fn hex_switches_take_a_prefix_or_not() {
        unsafe {
            std::env::set_var("PIMU_TEST_HEX", "0x3ec5ac0c");
            assert_eq!(hex("PIMU_TEST_HEX"), Some(0x3EC5_AC0C));
            std::env::set_var("PIMU_TEST_HEX", " 3ec5ac0c ");
            assert_eq!(hex("PIMU_TEST_HEX"), Some(0x3EC5_AC0C));
            std::env::remove_var("PIMU_TEST_HEX");
        }
        assert_eq!(hex("PIMU_TEST_HEX"), None);
    }
}
