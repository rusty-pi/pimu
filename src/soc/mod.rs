//! SoC-level definitions: memory maps and board assembly.

pub mod bcm2711;
pub mod board;
pub mod stepping;

pub use board::Board;
pub use stepping::Stepping;
