//! Memory-mapped peripheral models.
//!
//! Everything here implements [`MmioDevice`](crate::bus::MmioDevice). Devices are
//! self-contained: they never reference each other or the bus. Anything one
//! device needs from another is routed by [`Machine`](crate::machine::Machine).

pub mod armctrl;
pub mod armlocal;
pub mod asb;
pub mod aux;
pub mod avs;
pub mod bcm54213pe;
pub mod bluetooth;
pub mod bootbox;
pub mod bsc;
pub mod clkmon;
pub mod clockman;
pub mod configotp;
pub mod corectl;
pub mod disk;
pub mod dma4;
pub mod dma_legacy;
pub mod dwc2;
pub mod emmc2;
pub mod fxl6408;
pub mod genet;
pub mod gentimer;
pub mod gic;
pub mod gpio;
pub mod hat;
pub mod hd;
pub mod hdmi;
pub mod hdmi_ddc;
pub mod hvs;
pub mod mbox;
pub mod mcsync;
pub mod pcie;
pub mod pm;
pub mod pmic;
pub mod pvt;
pub mod readystub;
pub mod rng;
pub mod sdc;
pub mod sdcard;
pub mod sdramc;
pub mod spi0;
pub mod stub;
pub mod systimer;
pub mod uart_pl011;
pub mod usb;
pub mod vce;
pub mod vl805;
pub mod xhci;
pub mod xhci_otg;

pub use armctrl::ArmCtrl;
pub use armlocal::ArmLocal;
pub use asb::Asb;
pub use aux::Aux;
pub use avs::Avs;
pub use bootbox::BootBox;
pub use bsc::Bsc;
pub use clkmon::ClkMon;
pub use clockman::ClockManager;
pub use configotp::ConfigOtp;
pub use corectl::CoreCtl;
pub use dma4::Dma4;
pub use dwc2::Dwc2;
pub use emmc2::Emmc2;
pub use genet::Genet;
pub use gic::Gic;
pub use gpio::Gpio;
pub use hd::Hd;
pub use hdmi::Hdmi;
pub use hdmi_ddc::HdmiDdc;
pub use hvs::Hvs;
pub use mbox::Mbox;
pub use mcsync::McSync;
pub use pm::Pm;
pub use pmic::Pmic;
pub use pvt::Pvt;
pub use readystub::ReadyStub;
pub use rng::Rng;
pub use sdc::Sdc;
pub use sdramc::Sdramc;
pub use spi0::Spi0;
pub use stub::StubRegion;
pub use systimer::SysTimer;
pub use uart_pl011::Pl011;
pub use vce::Vce;
pub use vl805::Vl805;
pub use xhci::Xhci;
pub use xhci_otg::XhciOtg;

/// Every device's register map lives in `specs/*.toml`; this is what each one
/// models of it. `tests/specs.rs` checks these against the specs, and that
/// every spec has a device here.
pub const SPEC_COVERAGE: &[crate::spec::Coverage] = &[
    armctrl::COVERAGE,
    armlocal::COVERAGE,
    asb::COVERAGE,
    aux::COVERAGE,
    avs::COVERAGE,
    bcm54213pe::COVERAGE,
    bootbox::COVERAGE,
    bsc::COVERAGE,
    clkmon::COVERAGE,
    clockman::COVERAGE,
    configotp::COVERAGE,
    corectl::COVERAGE,
    dma4::COVERAGE,
    dma_legacy::COVERAGE,
    dma_legacy::COVERAGE_VPU,
    dwc2::COVERAGE,
    emmc2::COVERAGE,
    emmc2::COVERAGE_LEGACY,
    fxl6408::COVERAGE,
    genet::COVERAGE,
    gic::COVERAGE_CPU,
    gic::COVERAGE_DIST,
    gic::COVERAGE_VCPU,
    gic::COVERAGE_VIRT,
    gpio::COVERAGE,
    hd::COVERAGE,
    hdmi::COVERAGE,
    hdmi_ddc::COVERAGE,
    hdmi_ddc::COVERAGE_AUTO,
    hvs::COVERAGE,
    mbox::COVERAGE,
    mcsync::COVERAGE,
    pcie::COVERAGE,
    pm::COVERAGE,
    pmic::COVERAGE_CORE,
    pmic::COVERAGE_RAILS,
    pmic::COVERAGE_1D,
    pvt::COVERAGE,
    rng::COVERAGE,
    sdc::COVERAGE,
    sdramc::COVERAGE,
    spi0::COVERAGE,
    systimer::COVERAGE,
    uart_pl011::COVERAGE,
    vce::COVERAGE,
    vce::COVERAGE_CTRL,
    vl805::COVERAGE,
    xhci::COVERAGE,
    xhci_otg::COVERAGE,
];
