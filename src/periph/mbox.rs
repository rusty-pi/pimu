//! The ARM↔VideoCore mailboxes at `0x7E00_B880`.
//!
//! This is the channel a booted Linux talks to the firmware over: the
//! `bcm2835-mbox` driver and `raspberrypi-firmware` on top of it, `/dev/vcio`,
//! and the `rpi-fw-crypto` service that [rpi-mkosi#37] turns on. We are the
//! VideoCore side of it, so the polarity is the mirror of every Linux-side
//! description: **the ARM writes MAIL1 and reads MAIL0; we read MAIL1 and write
//! MAIL0.**
//!
//! **There are two apertures onto the same pair of FIFOs, and the firmware does
//! not use the one the device tree names.** Linux's `mailbox@7e00b880`
//! (`reg = <0x7e00b880 0x40>`, confirmed on the reference board) is the ARM's
//! view. `start4`'s own driver uses `0x7E00_B980` — read out of the receive op
//! at `0x3EC5AC0C`, which loads `0x7E00_B980` as a literal. Modelling only the
//! documented window is why a posted request went unseen: the firmware was
//! reading an address the model did not map.
//!
//! Each window has the same shape, and the two are mirror images — the
//! direction a given register moves data in depends on which side you are:
//!
//! ```text
//!   +0x00  data      ARM view: read a reply     VPU view: write a reply
//!   +0x10  peek
//!   +0x14  sender
//!   +0x18  status    of the VPU->ARM FIFO       bit 31 FULL, bit 30 EMPTY
//!   +0x1C  config
//!   +0x20  data      ARM view: post a request   VPU view: read a request
//!   +0x30  peek
//!   +0x34  sender
//!   +0x38  status    of the ARM->VPU FIFO
//!   +0x3C  config    interrupt enables
//! ```
//!
//! The receive op reads `+0x20`, tests bit 30 of `+0x38`, and on "empty" sets a
//! bit in `+0x3C` — it arms the interrupt and returns rather than spinning,
//! which is why nothing shows up in an MMIO trace of a normal boot.
//!
//! ## The wake path
//!
//! A third block, at [`PEND_BASE`], carries the interrupt state, and
//! `start4`'s ISR for it is `0x3EC58302`:
//!
//! ```text
//!   Mov   r6, 0x7E00B940
//!   Load  r0, [r6+8]     ; mailbox 0 pending
//!   Btest r0, #2         ;   -> call [gp+243092] with [gp+243100]
//!   Load  r5, [r6+12]    ; mailbox 1 pending
//!   Btest r5, #2         ;   -> call [gp+243096] with [gp+243104]
//!   Load  r8, [0x7E00B9BC]  ; config: clear bits 6 and 4
//! ```
//!
//! Bit 2 of each pending word is "this mailbox wants service", and the ISR
//! dispatches through per-mailbox callbacks the driver registered. It arrives
//! as **interrupt source 94** — read out of the firmware's own handler table
//! (`RVF_DBG_IRQTBL`: `src 94 handler=0x3ec58302`), not guessed, and the boot
//! does enable that source.
//!
//! So a request only reaches the `mbox_read` task if all three parts line up:
//! the word queued on the FIFO, bit 2 set in the pending word, and source 94
//! raised. Faking the wake without the pending bit would leave the firmware's
//! own bookkeeping (`gp+243084` / `gp+243088`) out of step with the hardware.
//!
//! A message is `(address & !0xF) | channel`: the low nibble is the channel and
//! the rest is the **bus address** of the request buffer. Channel 8 is the
//! property interface. Linux `dma_alloc_coherent`s that buffer and `/soc`
//! carries `dma-ranges = <0xc0000000 0x0 0x0 0x40000000>`, so the address on
//! the wire is `0xC000_0000 | phys` — the uncached SDRAM alias this model
//! already implements, which is why no translation happens here.
//!
//! Nothing in the boot touches this block: the firmware only services it once
//! an ARM is running, and this bench has no ARM. It is modelled so that a
//! request can be posted *as if* from the ARM — see `recon --mbox-property` —
//! and answered by the `mbox_read` task `start4.elf` leaves running after
//! `arm_loader` (the blob says `Creating mailbox reading task ...`).
//!
//! [rpi-mkosi#37]: https://github.com/valtzu/rpi-mkosi/issues/37

use std::collections::VecDeque;

use crate::bus::{BusResult, MmioDevice, Width};

/// The ARM's view — what Linux's device tree calls `mailbox@7e00b880`.
pub const ARM_BASE: u32 = 0x7E00_B880;
/// The VideoCore's view, which is what `start4.elf` actually drives.
pub const VPU_BASE: u32 = 0x7E00_B980;
/// One window.
pub const WINDOW: u32 = 0x40;

/// Interrupt pending / enable block, between the two windows.
pub const PEND_BASE: u32 = 0x7E00_B940;
/// `+0x08` is mailbox 0's pending word, `+0x0C` is mailbox 1's.
const PEND_MBOX0: u32 = 0x08;
const PEND_MBOX1: u32 = 0x0C;
/// Bit 2 of a pending word: this mailbox wants service.
const PEND_BIT: u32 = 1 << 2;

/// Every block above, as one mapped region starting at [`ARM_BASE`].
pub const BASE: u32 = ARM_BASE;
pub const SIZE: u32 = (VPU_BASE - ARM_BASE) + WINDOW;

/// The interrupt source the mailbox arrives on. From the firmware's own
/// handler table — `src 94 handler=0x3ec58302`.
pub const IRQ_SRC: u32 = 94;

const DATA0: u32 = 0x00;
const PEEK0: u32 = 0x10;
const SENDER0: u32 = 0x14;
const STATUS0: u32 = 0x18;
const CONFIG0: u32 = 0x1C;
const DATA1: u32 = 0x20;
const PEEK1: u32 = 0x30;
const SENDER1: u32 = 0x34;
const STATUS1: u32 = 0x38;
const CONFIG1: u32 = 0x3C;

/// `STATUS` bit 31: this mailbox cannot take another word.
const STATUS_FULL: u32 = 1 << 31;
/// `STATUS` bit 30: this mailbox has nothing queued.
const STATUS_EMPTY: u32 = 1 << 30;

/// Hardware FIFOs are 8 deep.
const DEPTH: usize = 8;

/// The property interface — the channel `/dev/vcio` and `raspberrypi-firmware`
/// use, and the only one that carries a buffer address.
pub const CHANNEL_PROPERTY: u32 = 8;

#[derive(Default)]
pub struct Mbox {
    /// Replies we have written for the ARM to read (MAIL0).
    to_arm: VecDeque<u32>,
    /// Requests the ARM has posted for us (MAIL1).
    to_vpu: VecDeque<u32>,
    config0: u32,
    config1: u32,
    sender0: u32,
    sender1: u32,
    /// Requests the firmware has taken off MAIL1. A request the model posted
    /// and the firmware never read is the signal that the `mbox_read` task is
    /// not listening the way we assumed.
    pub reads: u64,
    /// Replies the firmware has written to MAIL0.
    pub writes: u64,
    /// `RVF_DBG_MBOX`, read once — this device sits on the step path.
    dbg: bool,
}

impl Mbox {
    pub fn new() -> Mbox {
        Mbox {
            dbg: std::env::var_os("RVF_DBG_MBOX").is_some(),
            ..Mbox::default()
        }
    }

    /// Post a request as the ARM would: `(bus_addr & !0xF) | channel`.
    /// Returns false if MAIL1 is full, exactly as the hardware would.
    pub fn post_from_arm(&mut self, message: u32) -> bool {
        if self.to_vpu.len() >= DEPTH {
            return false;
        }
        if self.dbg {
            eprintln!(
                "[mbox] ARM -> VPU {:#010x} (channel {}, addr {:#010x})",
                message,
                message & 0xF,
                message & !0xF
            );
        }
        self.to_vpu.push_back(message);
        true
    }

    /// Take a reply the firmware left for the ARM, if any.
    pub fn take_reply(&mut self) -> Option<u32> {
        self.to_arm.pop_front()
    }

    /// True while the firmware has not drained the request we posted.
    pub fn request_outstanding(&self) -> bool {
        !self.to_vpu.is_empty()
    }

    /// True while a request is queued for the firmware and it has armed the
    /// interrupt. The run loop turns this into source [`IRQ_SRC`].
    pub fn irq_asserted(&self) -> bool {
        !self.to_vpu.is_empty() && self.config1 != 0
    }

    fn status(q: &VecDeque<u32>) -> u32 {
        let mut s = 0;
        if q.is_empty() {
            s |= STATUS_EMPTY;
        }
        if q.len() >= DEPTH {
            s |= STATUS_FULL;
        }
        s
    }
}

impl MmioDevice for Mbox {
    fn name(&self) -> &'static str {
        "mbox"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        // The interrupt block sits between the two windows.
        if (PEND_BASE - ARM_BASE..VPU_BASE - ARM_BASE).contains(&offset) {
            let reg = (offset & !3) - (PEND_BASE - ARM_BASE);
            return Ok(match reg {
                // Mailbox 1 is the ARM->VPU direction — the one the receive op
                // reads. Mailbox 0 never asks for service here: the VPU is the
                // writer on that side, so nothing notifies it about its own
                // outbox.
                PEND_MBOX1 if self.irq_asserted() => PEND_BIT,
                PEND_MBOX0 | PEND_MBOX1 => 0,
                _ => 0,
            });
        }
        let vpu = offset >= (VPU_BASE - ARM_BASE);
        let reg = (offset & !3) % WINDOW;
        Ok(match reg {
            // `+0x00` / `+0x18`: the VPU->ARM FIFO. The VPU writes it, so from
            // this side a read is only meaningful for the ARM.
            DATA0 if !vpu => self.to_arm.pop_front().unwrap_or(0),
            PEEK0 => self.to_arm.front().copied().unwrap_or(0),
            STATUS0 => Mbox::status(&self.to_arm),
            SENDER0 => self.sender0,
            CONFIG0 => self.config0,
            // `+0x20` / `+0x38`: the ARM->VPU FIFO. The VPU drains it here —
            // this is the read the `mbox_read` task's receive op makes.
            DATA1 if vpu => {
                let v = self.to_vpu.pop_front().unwrap_or(0);
                if v != 0 {
                    self.reads += 1;
                    if self.dbg {
                        eprintln!("[mbox] VPU read request {v:#010x}");
                    }
                }
                v
            }
            PEEK1 => self.to_vpu.front().copied().unwrap_or(0),
            STATUS1 => Mbox::status(&self.to_vpu),
            SENDER1 => self.sender1,
            CONFIG1 => self.config1,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        if (PEND_BASE - ARM_BASE..VPU_BASE - ARM_BASE).contains(&offset) {
            // Pending bits are computed from the FIFO, so an ack does not
            // latch; the line drops when the firmware drains the request.
            return Ok(());
        }
        let vpu = offset >= (VPU_BASE - ARM_BASE);
        let reg = (offset & !3) % WINDOW;
        match reg {
            // The VPU posting a reply for the ARM to read.
            DATA0 if vpu => {
                if self.to_arm.len() < DEPTH {
                    self.to_arm.push_back(value);
                    self.writes += 1;
                }
                if self.dbg {
                    eprintln!("[mbox] VPU -> ARM {value:#010x}");
                }
            }
            // The ARM posting a request. Nothing in this bench does it through
            // MMIO — `post_from_arm` is the entry point — but model it anyway
            // so the window is honest.
            DATA1 if !vpu => {
                self.post_from_arm(value);
            }
            SENDER0 => self.sender0 = value,
            CONFIG0 => self.config0 = value,
            SENDER1 => self.sender1 = value,
            CONFIG1 => self.config1 = value,
            // Status bits are computed from the queues, so a write to one is an
            // acknowledge of something that does not latch.
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_posted_request_is_readable_once_from_mail1() {
        // Offsets the firmware itself uses: the VPU window, not the ARM one.
        let vpu = VPU_BASE - ARM_BASE;
        let mut m = Mbox::default();
        assert_eq!(m.read(vpu + STATUS1, Width::Word).unwrap(), STATUS_EMPTY);

        assert!(m.post_from_arm(0xC000_1000 | CHANNEL_PROPERTY));
        assert_eq!(m.read(vpu + STATUS1, Width::Word).unwrap(), 0);
        assert_eq!(m.read(vpu + PEEK1, Width::Word).unwrap(), 0xC000_1008);
        assert_eq!(m.read(vpu + DATA1, Width::Word).unwrap(), 0xC000_1008);
        // Drained: empty again, and the read counted.
        assert_eq!(m.read(vpu + STATUS1, Width::Word).unwrap(), STATUS_EMPTY);
        assert_eq!(m.reads, 1);
    }

    #[test]
    fn a_reply_written_to_mail0_comes_back_to_the_arm_side() {
        let vpu = VPU_BASE - ARM_BASE;
        let mut m = Mbox::default();
        assert_eq!(m.take_reply(), None);
        // The VPU writes its reply through its own window; the ARM reads it
        // through the documented one.
        m.write(vpu + DATA0, Width::Word, 0xC000_1008).unwrap();
        assert_eq!(m.read(STATUS0, Width::Word).unwrap(), 0);
        assert_eq!(m.take_reply(), Some(0xC000_1008));
        assert_eq!(m.take_reply(), None);
        assert_eq!(m.writes, 1);
    }

    #[test]
    fn the_fifos_are_eight_deep_and_report_full() {
        let mut m = Mbox::default();
        for i in 0..DEPTH {
            assert!(m.post_from_arm(0x1000 * i as u32 + CHANNEL_PROPERTY), "{i}");
        }
        assert_eq!(
            m.read(VPU_BASE - ARM_BASE + STATUS1, Width::Word).unwrap() & STATUS_FULL,
            STATUS_FULL
        );
        assert!(!m.post_from_arm(CHANNEL_PROPERTY));
    }
}
