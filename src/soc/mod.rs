//! SoC-level definitions: memory maps and board assembly.

pub mod bcm2711;
pub mod board;
pub mod stepping;

pub use board::Board;
pub use stepping::Stepping;

/// Which SoC / board we are modelling. Only BCM2711 for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Soc {
    #[default]
    Bcm2711,
}
