//! Memory-mapped peripheral models.
//!
//! Everything here implements [`MmioDevice`](crate::bus::MmioDevice). Devices are
//! self-contained: they never reference each other or the bus. Anything one
//! device needs from another is routed by [`Machine`](crate::machine::Machine).

pub mod aux;
pub mod clockman;
pub mod configotp;
pub mod corectl;
pub mod mcsync;
pub mod readystub;
pub mod spi0;
pub mod stub;
pub mod systimer;
pub mod uart_pl011;

pub use aux::Aux;
pub use clockman::ClockManager;
pub use configotp::ConfigOtp;
pub use corectl::CoreCtl;
pub use mcsync::McSync;
pub use readystub::ReadyStub;
pub use spi0::Spi0;
pub use stub::StubRegion;
pub use systimer::SysTimer;
pub use uart_pl011::Pl011;
