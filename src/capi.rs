//! C ABI for embedding the VideoCore model in another emulator.
//!
//! This is the seam the QEMU frontend uses (`qemu/` in the repository): a
//! patched `raspi4b` machine carries an `rvf-videocore` device that links this
//! crate as a static library, runs the VPU model on its own thread over the
//! guest's RAM, and lets the ARM cores reach the model's peripherals through
//! the same MMIO dispatch the VPU uses. The header that mirrors these
//! declarations is `include/rvf.h`; keep the two in step by hand — there is no
//! bindgen in the build, on purpose, so that the ABI is a reviewed artefact.
//!
//! Threading: nothing here locks. The host serialises every call on one
//! `RvfVc` (QEMU does it with the big lock: the VPU thread takes it for the
//! duration of a slice, and the ARM's MMIO handlers already run under it).
//! The callbacks in [`RvfHostOps`] are invoked from inside those calls, on the
//! calling thread.
//!
//! Addresses are VC bus addresses throughout (`0x7E00_B880` for the mailbox),
//! which is what both the firmware and the ARM's `0xFE..` window decode to.

use std::ffi::{c_char, c_int, c_void, CString};

use crate::bus::{Bus, Width};
use crate::emulator::{Emulator, RunEnd, RunLimits};
use crate::firmware::Payload;
use crate::machine::{ForeignBus, Machine};
use crate::mem::Ram;
use crate::soc::bcm2711 as map;

/// Callbacks into the host. Any of them may be null.
#[repr(C)]
pub struct RvfHostOps {
    /// Passed back as the first argument of every callback.
    pub opaque: *mut c_void,
    /// A VPU access to a foreign window (see [`rvf_vc_add_foreign`]).
    pub mmio_read: Option<unsafe extern "C" fn(*mut c_void, u32, u32) -> u32>,
    pub mmio_write: Option<unsafe extern "C" fn(*mut c_void, u32, u32, u32)>,
    /// Bytes the firmware wrote to the console UART the model owns. Not called
    /// while UART0 is a foreign window — then the bytes went to the host's UART.
    pub console: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize)>,
    /// A diagnostic line, NUL-terminated, no trailing newline.
    pub log: Option<unsafe extern "C" fn(*mut c_void, *const c_char)>,
    /// The firmware released the ARM cores: `arm_loader` has placed the ARM
    /// stub at 0, the kernel and the device tree, and started the cluster.
    /// Called once.
    pub arm_release: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// Status of a [`rvf_vc_run`] slice.
pub const RVF_RUN_RUNNING: c_int = 0;
/// The VPU stopped for good (a fault, or a halt with no interrupt to resume
/// on). The reason was reported through `log`.
pub const RVF_RUN_STOPPED: c_int = 1;
/// The firmware asked the power manager for a SoC reset (the watchdog fired,
/// or an EEPROM self-update rebooted). The host decides what a board reset is.
pub const RVF_RUN_RESET: c_int = 2;

struct HostForeign {
    ops: *const RvfHostOps,
    ranges: Vec<(u32, u32)>,
    /// Bytes written to UART0's data register, when that window is foreign:
    /// the firmware log still has to be watched for the hand-off line.
    tee: Vec<u8>,
}

/// UART0's data register, the one write that is a console byte.
const UART0_DR: u32 = map::UART0_BASE;

// `ops` points at the host's table for the life of the `RvfVc`; see
// `rvf_vc_new`. The host also serialises all use.
unsafe impl Send for HostForeign {}

impl HostForeign {
    fn ops(&self) -> &RvfHostOps {
        // SAFETY: the host keeps the table alive (documented in rvf.h).
        unsafe { &*self.ops }
    }
}

impl ForeignBus for HostForeign {
    fn covers(&self, addr: u32) -> bool {
        self.ranges.iter().any(|&(lo, hi)| (lo..hi).contains(&addr))
    }
    fn read(&mut self, addr: u32, width: Width) -> u32 {
        match self.ops().mmio_read {
            // SAFETY: host-provided function pointer with the documented ABI.
            Some(f) => unsafe { f(self.ops().opaque, addr, width.bytes()) },
            None => 0,
        }
    }
    fn write(&mut self, addr: u32, width: Width, value: u32) {
        if addr == UART0_DR {
            self.tee.push(value as u8);
        }
        if let Some(f) = self.ops().mmio_write {
            // SAFETY: as above.
            unsafe { f(self.ops().opaque, addr, width.bytes(), value) }
        }
    }
    fn take_console_tee(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tee)
    }
}

/// One VideoCore, its machine and the host it reports to.
pub struct RvfVc {
    emu: Emulator,
    ops: RvfHostOps,
    /// VC addresses forwarded to the host, installed on the machine at the
    /// first `rvf_vc_run`, so windows can be added while the model is still
    /// being set up.
    foreign_ranges: Vec<(u32, u32)>,
    foreign_installed: bool,
    released: bool,
    /// Console text since the last look, for the hand-off detector.
    console_tail: Vec<u8>,
    limits: RunLimits,
}

impl RvfVc {
    fn log(&self, msg: &str) {
        if let Some(f) = self.ops.log {
            if let Ok(c) = CString::new(msg) {
                // SAFETY: host-provided function pointer with the documented ABI.
                unsafe { f(self.ops.opaque, c.as_ptr()) }
            }
        }
    }

    fn deliver_console(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(f) = self.ops.console {
            // SAFETY: as above; the slice outlives the call.
            unsafe { f(self.ops.opaque, bytes.as_ptr(), bytes.len()) }
        }
    }

    /// The hand-off, as the firmware announces it. `arm_loader` logs
    /// `Starting ARM with <n>MB` on the line before it releases the cluster,
    /// and the release itself has no MMIO signature in the model yet (it goes
    /// through a block the firmware pokes once). Until that block is
    /// identified the log line is the signal; it is the same line the hosted
    /// boot's last milestone asserts on.
    fn detect_release(&mut self, fresh: &[u8]) -> bool {
        if self.released {
            return false;
        }
        const NEEDLE: &[u8] = b"arm_loader: Starting ARM";
        self.console_tail.extend_from_slice(fresh);
        let hit = self.console_tail.windows(NEEDLE.len()).any(|w| w == NEEDLE);
        if !hit {
            let keep = NEEDLE.len() - 1;
            if self.console_tail.len() > keep {
                let drop = self.console_tail.len() - keep;
                self.console_tail.drain(..drop);
            }
            return false;
        }
        self.released = true;
        self.console_tail.clear();
        true
    }
}

fn set_err(err: *mut c_char, err_len: usize, msg: &str) {
    if err.is_null() || err_len == 0 {
        return;
    }
    let bytes = msg.as_bytes();
    let n = bytes.len().min(err_len - 1);
    // SAFETY: the host promised `err_len` writable bytes at `err`.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), err as *mut u8, n);
        *err.add(n) = 0;
    }
}

/// # Safety
/// `ram` must be valid for `ram_len` bytes for the life of the returned
/// object (this is guest RAM: other bus masters may write it concurrently, and
/// the model only ever copies bytes in and out). `eeprom`/`sd` are copied.
/// `ops` is copied by value; its `opaque` is the host's. Returns null and fills
/// `err` on failure.
#[no_mangle]
pub unsafe extern "C" fn rvf_vc_new(
    ram: *mut u8,
    ram_len: usize,
    eeprom: *const u8,
    eeprom_len: usize,
    sd: *const u8,
    sd_len: usize,
    ops: *const RvfHostOps,
    err: *mut c_char,
    err_len: usize,
) -> *mut RvfVc {
    if ram.is_null() || ops.is_null() || eeprom.is_null() {
        set_err(err, err_len, "rvf_vc_new: null argument");
        return std::ptr::null_mut();
    }
    // The address decode folds every SDRAM alias with `& 0x3FFF_FFFF`, so the
    // model can address one gigabyte at most; a larger region is simply not
    // reachable from the VPU and is left to the ARM.
    let ram_len = ram_len.min(1 << 30);
    // The hosted `recon` streams the console to stderr by default; in-process
    // it belongs to the host's UART or to `ops.console`, not our stderr.
    if std::env::var_os("RVF_LIVE_CONSOLE").is_none() {
        std::env::set_var("RVF_LIVE_CONSOLE", "0");
    }
    let flash = std::slice::from_raw_parts(eeprom, eeprom_len).to_vec();
    let payload = match Payload::from_eeprom_bytes(&flash) {
        Ok(p) => p,
        Err(e) => {
            set_err(err, err_len, &format!("EEPROM image: {e}"));
            return std::ptr::null_mut();
        }
    };
    let mut machine = Machine::with_ram(Ram::over_raw(map::SDRAM_CACHED_BASE, ram, ram_len));
    machine.spi0.attach_flash(flash);
    if !sd.is_null() && sd_len > 0 {
        machine
            .emmc2
            .insert_card(std::slice::from_raw_parts(sd, sd_len).to_vec());
    }
    if let Err(e) = payload.load_into(&mut machine) {
        set_err(err, err_len, &format!("loading bootcode: {e}"));
        return std::ptr::null_mut();
    }
    let entry = payload.entry();
    let mut emu = Emulator::new(machine, entry);
    // What `recon` runs with: `sleep` parks the core until the next interrupt
    // instead of halting the run, and the firmware's inline `0x0000` padding
    // is stepped over. The strict policy is for hand-assembled test payloads.
    emu.set_unimpl_policy(crate::vpu::UnimplPolicy::ReconFault);
    let limits = RunLimits {
        max_steps: None,
        max_wall: None,
        stop_pc: None,
        // Keeps the firmware's `udelay` loops from being executed iteration by
        // iteration (the model's biggest time sink); the spin *stop* it can
        // also produce is treated as "carry on" in `rvf_vc_run`.
        idle_spin_limit: 200_000,
        silent_us: 0,
    };
    Box::into_raw(Box::new(RvfVc {
        emu,
        ops: std::ptr::read(ops),
        foreign_ranges: Vec::new(),
        foreign_installed: false,
        released: false,
        console_tail: Vec::new(),
        limits,
    }))
}

/// # Safety
/// `vc` came from `rvf_vc_new` and is not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn rvf_vc_free(vc: *mut RvfVc) {
    if !vc.is_null() {
        drop(Box::from_raw(vc));
    }
}

/// Forward VPU accesses in `[lo, hi)` (VC bus addresses) to the host's
/// `mmio_read`/`mmio_write` instead of the model's own peripheral. Returns 0,
/// or -1 once the model has started running.
///
/// # Safety
/// `vc` came from `rvf_vc_new`.
#[no_mangle]
pub unsafe extern "C" fn rvf_vc_add_foreign(vc: *mut RvfVc, lo: u32, hi: u32) -> c_int {
    let vc = &mut *vc;
    if vc.foreign_installed || hi <= lo {
        return -1;
    }
    vc.foreign_ranges.push((lo, hi));
    0
}

/// Run up to `max_steps` VPU instructions (both cores together). Returns one
/// of the `RVF_RUN_*` statuses; `RVF_RUN_RUNNING` means call again.
///
/// # Safety
/// `vc` came from `rvf_vc_new`.
#[no_mangle]
pub unsafe extern "C" fn rvf_vc_run(vc: *mut RvfVc, max_steps: u64) -> c_int {
    let vc = &mut *vc;
    if !vc.foreign_installed {
        vc.foreign_installed = true;
        if !vc.foreign_ranges.is_empty() {
            vc.emu.machine.foreign = Some(Box::new(HostForeign {
                ops: &vc.ops,
                ranges: std::mem::take(&mut vc.foreign_ranges),
                tee: Vec::new(),
            }));
        }
    }
    let retired = vc.emu.cpu.retired + vc.emu.cpu1.as_ref().map_or(0, |c| c.retired);
    let limits = RunLimits {
        max_steps: Some(retired.saturating_add(max_steps.max(1))),
        ..vc.limits.clone()
    };
    let report = vc.emu.run(&limits);
    let console = report.console;
    let seen = match vc.emu.machine.foreign.as_mut() {
        Some(f) => f.take_console_tee(),
        None => console.clone(),
    };
    if vc.detect_release(&seen) {
        vc.log("arm_loader released the ARM");
        if let Some(f) = vc.ops.arm_release {
            f(vc.ops.opaque);
        }
    }
    vc.deliver_console(&console);
    match report.end {
        RunEnd::StepLimit | RunEnd::IdleSpin(_) | RunEnd::TimeLimit => RVF_RUN_RUNNING,
        RunEnd::Reset => {
            vc.log("firmware requested a SoC reset");
            RVF_RUN_RESET
        }
        other => {
            vc.log(&format!(
                "VPU stopped: {other:?} at pc {:#010x} after {} instructions",
                report.pc, report.retired
            ));
            RVF_RUN_STOPPED
        }
    }
}

/// Model time in microseconds — the VPU's system-timer counter.
///
/// # Safety
/// `vc` came from `rvf_vc_new`.
#[no_mangle]
pub unsafe extern "C" fn rvf_vc_now_us(vc: *const RvfVc) -> u64 {
    (*vc).emu.machine.systimer.now_us()
}

/// Instructions retired by core 0 so far.
///
/// # Safety
/// `vc` came from `rvf_vc_new`.
#[no_mangle]
pub unsafe extern "C" fn rvf_vc_retired(vc: *const RvfVc) -> u64 {
    (*vc).emu.cpu.retired
}

/// Whether `arm_release` has fired.
///
/// # Safety
/// `vc` came from `rvf_vc_new`.
#[no_mangle]
pub unsafe extern "C" fn rvf_vc_arm_released(vc: *const RvfVc) -> c_int {
    c_int::from((*vc).released)
}

/// Level of the ARM's mailbox interrupt (GIC SPI 33).
///
/// # Safety
/// `vc` came from `rvf_vc_new`.
#[no_mangle]
pub unsafe extern "C" fn rvf_vc_arm_irq(vc: *const RvfVc) -> c_int {
    c_int::from((*vc).emu.machine.arm_mbox_irq())
}

fn width_of(size: u32) -> Option<Width> {
    match size {
        1 => Some(Width::Byte),
        2 => Some(Width::Half),
        4 => Some(Width::Word),
        _ => None,
    }
}

/// An ARM-side access to one of the model's peripherals, at a VC bus address.
/// Unmapped or malformed accesses read as 0.
///
/// # Safety
/// `vc` came from `rvf_vc_new`.
#[no_mangle]
pub unsafe extern "C" fn rvf_vc_mmio_read(vc: *mut RvfVc, addr: u32, size: u32) -> u32 {
    let vc = &mut *vc;
    match width_of(size) {
        Some(w) => vc.emu.machine.load(addr, w).unwrap_or(0),
        None => 0,
    }
}

/// # Safety
/// `vc` came from `rvf_vc_new`.
#[no_mangle]
pub unsafe extern "C" fn rvf_vc_mmio_write(vc: *mut RvfVc, addr: u32, size: u32, value: u32) {
    let vc = &mut *vc;
    if let Some(w) = width_of(size) {
        let _ = vc.emu.machine.store(addr, w, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_detector_fires_once_and_only_on_the_hand_off_line() {
        let mut vc = detector();
        assert!(!vc.detect_release(b"MESS: Watchdog stopped\n"));
        assert!(!vc.detect_release(b"MESS: arm_loader: Start"));
        assert!(vc.detect_release(b"ing ARM with 948MB\n"));
        assert!(!vc.detect_release(b"arm_loader: Starting ARM again\n"));
    }

    fn detector() -> RvfVc {
        RvfVc {
            emu: Emulator::new(Machine::new(1 << 16), 0),
            ops: RvfHostOps {
                opaque: std::ptr::null_mut(),
                mmio_read: None,
                mmio_write: None,
                console: None,
                log: None,
                arm_release: None,
            },
            foreign_ranges: Vec::new(),
            foreign_installed: false,
            released: false,
            console_tail: Vec::new(),
            limits: RunLimits::default(),
        }
    }
}
