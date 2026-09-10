//! Memory-mapped peripheral models.
//!
//! Everything here implements [`MmioDevice`](crate::bus::MmioDevice). Devices are
//! self-contained: they never reference each other or the bus. Anything one
//! device needs from another is routed by [`Machine`](crate::machine::Machine).

pub mod avs;
pub mod aux;
pub mod bootbox;
pub mod bsc;
pub mod clkmon;
pub mod clockman;
pub mod configotp;
pub mod corectl;
pub mod dma4;
pub mod dma_legacy;
pub mod emmc2;
pub mod hvs;
pub mod mcsync;
pub mod pm;
pub mod readystub;
pub mod rng;
pub mod sdc;
pub mod sdcard;
pub mod sdramc;
pub mod spi0;
pub mod stub;
pub mod systimer;
pub mod uart_pl011;

pub use avs::Avs;
pub use aux::Aux;
pub use bootbox::BootBox;
pub use bsc::Bsc;
pub use clkmon::ClkMon;
pub use clockman::ClockManager;
pub use configotp::ConfigOtp;
pub use corectl::CoreCtl;
pub use dma4::Dma4;
pub use emmc2::Emmc2;
pub use hvs::Hvs;
pub use mcsync::McSync;
pub use pm::Pm;
pub use readystub::ReadyStub;
pub use rng::Rng;
pub use sdc::Sdc;
pub use sdramc::Sdramc;
pub use spi0::Spi0;
pub use stub::StubRegion;
pub use systimer::SysTimer;
pub use uart_pl011::Pl011;
