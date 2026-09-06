//! Scenario files: a TOML description of one bench run.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::machine::Console;
use crate::vpu::UnimplPolicy;

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub payload: PayloadSpec,
    #[serde(default)]
    pub machine: MachineSpec,
    #[serde(default)]
    pub run: RunSpec,
    pub golden: GoldenSpec,

    /// Directory the scenario file lives in; relative paths resolve against it.
    #[serde(skip)]
    pub base_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PayloadKind {
    /// A named payload from [`crate::payloads`].
    Builtin,
    /// An ELF32 file on disk.
    Elf,
    /// A flat binary on disk.
    Raw,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PayloadSpec {
    pub kind: PayloadKind,
    /// Builtin name, or path (relative to the scenario file) for elf/raw.
    pub source: String,
    /// Load address — required for `builtin` and `raw`.
    #[serde(default)]
    pub load_addr: Option<u32>,
    /// Entry-point override. Defaults to `load_addr` (raw/builtin) or the ELF
    /// header entry.
    #[serde(default)]
    pub entry: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MachineSpec {
    #[serde(default = "default_ram_mb")]
    pub ram_mb: u32,
    #[serde(default)]
    pub console: ConsoleSpec,
}

impl Default for MachineSpec {
    fn default() -> Self {
        MachineSpec {
            ram_mb: default_ram_mb(),
            console: ConsoleSpec::default(),
        }
    }
}

fn default_ram_mb() -> u32 {
    64
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsoleSpec {
    #[default]
    Pl011,
    MiniUart,
}

impl From<ConsoleSpec> for Console {
    fn from(c: ConsoleSpec) -> Console {
        match c {
            ConsoleSpec::Pl011 => Console::Pl011,
            ConsoleSpec::MiniUart => Console::MiniUart,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunSpec {
    #[serde(default = "default_max_steps")]
    pub max_steps: u64,
    #[serde(default)]
    pub stop_pc: Option<u32>,
    #[serde(default)]
    pub unimpl: UnimplSpec,
    #[serde(default)]
    pub idle_spin_limit: u64,
}

impl Default for RunSpec {
    fn default() -> Self {
        RunSpec {
            max_steps: default_max_steps(),
            stop_pc: None,
            unimpl: UnimplSpec::default(),
            idle_spin_limit: 0,
        }
    }
}

fn default_max_steps() -> u64 {
    1_000_000
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnimplSpec {
    #[default]
    Fault,
    Skip,
}

impl From<UnimplSpec> for UnimplPolicy {
    fn from(u: UnimplSpec) -> UnimplPolicy {
        match u {
            UnimplSpec::Fault => UnimplPolicy::Fault,
            UnimplSpec::Skip => UnimplPolicy::Skip,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GoldenSpec {
    /// Path to the golden transcript, relative to the scenario file.
    pub path: String,
}

impl Scenario {
    pub fn load(path: &Path) -> Result<Scenario> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading scenario {}", path.display()))?;
        let mut s: Scenario = toml::from_str(&text)
            .with_context(|| format!("parsing scenario {}", path.display()))?;
        s.base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();

        match s.payload.kind {
            PayloadKind::Builtin | PayloadKind::Raw if s.payload.load_addr.is_none() => {
                bail!(
                    "payload.load_addr is required for kind = {:?}",
                    s.payload.kind
                );
            }
            _ => {}
        }
        Ok(s)
    }

    pub fn golden_path(&self) -> PathBuf {
        self.base_dir.join(&self.golden.path)
    }

    pub fn payload_path(&self) -> PathBuf {
        self.base_dir.join(&self.payload.source)
    }
}
