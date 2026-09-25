//! The ARM↔VideoCore mailboxes at `0x7E00_B880`. Registers and fields:
//! `specs/mbox.toml`.
//!
//! We are the VideoCore side, so the polarity is the mirror of every Linux-side
//! description: **the ARM writes MAIL1 and reads MAIL0; we read MAIL1 and write
//! MAIL0.** A message is `(address & !0xF) | channel`, the rest being the bus
//! address of the request buffer; channel 8 is the property interface.
//!
//! Two things this block cannot be modelled without:
//!
//! * **There are two apertures onto the same pair of FIFOs, and the firmware
//!   does not use the one the device tree names.** Linux's `mailbox@7e00b880`
//!   is the ARM's view; start4's driver uses `0x7E00_B980`. Map only the
//!   documented window and a posted request is never seen.
//! * A request reaches start4's `mbox_read` task only if all four line up: the
//!   word on the FIFO, bit 2 of the pending word at `0x7E00_B940`, interrupt
//!   source 94, and `PEND_HAVE_DATA` in `CONFIG1`. Those pending bits are
//!   read-only and are the condition AND its enable, not the raw condition —
//!   a `CONFIG` answering with the enables alone leaves the ISR nothing to do
//!   and the task parked forever, and raw conditions would release the send
//!   lock on every unrelated interrupt.
//!
//! Every property reply is decoded into a [`PropertyLog`] as the firmware posts
//! it, whoever asked, and the run report prints it — Linux checks the buffer's
//! status word but not always the values, and the firmware's own prints go to
//! its message ring rather than the UART.

use std::collections::{BTreeMap, VecDeque};

use crate::bus::{BusResult, MmioDevice, Width};
use crate::log::{Channel, Log};

use crate::spec::mbox::{
    CONFIG0, CONFIG0_CLEAR_MASK as CFG_CLEAR, CONFIG0_EN_HAVE_DATA_MASK as CFG_EN_HAVE_DATA,
    CONFIG0_EN_HAVE_SPACE_MASK as CFG_EN_HAVE_SPACE, CONFIG0_EN_OPP_EMPTY_MASK as CFG_EN_OPP_EMPTY,
    CONFIG0_PEND_HAVE_DATA_MASK as CFG_PEND_HAVE_DATA,
    CONFIG0_PEND_HAVE_SPACE_MASK as CFG_PEND_HAVE_SPACE,
    CONFIG0_PEND_OPP_EMPTY_MASK as CFG_PEND_OPP_EMPTY, CONFIG1, DATA0, DATA0_STRIDE, DATA1, PEEK0,
    PEEK1, SENDER0, SENDER1, STATUS0, STATUS0_EMPTY_MASK as STATUS_EMPTY,
    STATUS0_FULL_MASK as STATUS_FULL, STATUS1,
};
use crate::spec::Coverage;

/// Every register in `specs/mbox.toml` is modelled.
pub const COVERAGE: Coverage = Coverage {
    block: "mbox",
    decoded: &[
        DATA0, PEEK0, SENDER0, STATUS0, CONFIG0, DATA1, PEEK1, SENDER1, STATUS1, CONFIG1,
    ],
};

/// Offset of the VPU's view: every register's second element.
const VPU: u32 = DATA0_STRIDE;
/// One view's registers, `DATA0..=CONFIG1`; the rest of a view aliases them.
const WINDOW: u32 = CONFIG1 + 4;
/// The gap between the two views. Its first four words are the VPU's view of
/// the doorbells, decoded by [`crate::periph::bell`] ahead of this block.
const PEND_BLOCK: std::ops::Range<u32> = 0x40..VPU;

/// The interrupt source the mailbox arrives on.
pub const IRQ_SRC: u32 = crate::spec::mbox::IRQ_VPU;

/// `CONFIG` bits 0..2, the part that latches.
const CFG_ENABLES: u32 = CFG_EN_HAVE_DATA | CFG_EN_HAVE_SPACE | CFG_EN_OPP_EMPTY;
/// `CONFIG` bits 4..6: the interrupt-pending flag for each enable.
const CFG_PENDING: u32 = CFG_PEND_HAVE_DATA | CFG_PEND_HAVE_SPACE | CFG_PEND_OPP_EMPTY;

const DEPTH: usize = 8;

/// The property interface, the only channel carrying a buffer address.
pub const CHANNEL_PROPERTY: u32 = 8;

/// Marks a response in a buffer's code word, and the "handled" mark in a
/// tag's third word.
pub const RESPONSE: u32 = 0x8000_0000;
/// Largest buffer [`PropertyLog::record`] walks, so a bad size word is bounded.
const MAX_BUFFER: u32 = 0x1_0000;

/// What the firmware answered on the property channel, tag by tag. A reply can
/// carry a value nothing else checks — `reset-raspberrypi` looks only at the
/// buffer's status word, so a failed VL805 firmware load boots the same —
/// which is what makes recording it worth the trouble.
#[derive(Default)]
pub struct PropertyLog {
    pub replies: u64,
    /// Replies whose buffer-level code is not `0x8000_0000`, success.
    pub failed: u64,
    tags: BTreeMap<u32, TagLog>,
}

/// One tag's history in a [`PropertyLog`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TagLog {
    pub marked: u64,
    /// Replies that carried it without: an unknown tag, but also
    /// `SET_GPIO_STATE` / `SET_GPIO_CONFIG`, which answer unmarked with their
    /// status in the value.
    pub unmarked: u64,
    /// The first word of the tag's value buffer as the latest reply left it,
    /// if the buffer has one.
    pub last: Option<u32>,
    /// Replies carrying the tag whose buffer-level code was not success. Per
    /// buffer, not per tag: one code covers every tag in a rejected buffer.
    pub errors: u64,
}

impl PropertyLog {
    /// Decode one reply buffer; `word` reads a word at a byte offset into it.
    pub fn record(&mut self, word: impl Fn(u32) -> u32) {
        self.replies += 1;
        let ok = word(4) == RESPONSE;
        if !ok {
            self.failed += 1;
        }
        let size = word(0).min(MAX_BUFFER);
        let mut off = 8;
        while off + 12 <= size {
            let tag = word(off);
            if tag == 0 {
                break;
            }
            let slot = word(off + 4).min(MAX_BUFFER);
            let code = word(off + 8);
            let t = self.tags.entry(tag).or_default();
            if code & RESPONSE != 0 {
                t.marked += 1;
            } else {
                t.unmarked += 1;
            }
            t.last = (slot >= 4).then(|| word(off + 12));
            if !ok {
                t.errors += 1;
            }
            off += 12 + ((slot + 3) & !3);
        }
    }

    pub fn tags(&self) -> impl Iterator<Item = (u32, TagLog)> + '_ {
        self.tags.iter().map(|(&tag, &log)| (tag, log))
    }
}

#[derive(Default)]
pub struct Mbox {
    to_arm: VecDeque<u32>,
    to_vpu: VecDeque<u32>,
    config0: u32,
    config1: u32,
    sender0: u32,
    sender1: u32,
    /// Requests the firmware has taken off MAIL1: a request it never read
    /// means the `mbox_read` task is not listening.
    pub reads: u64,
    pub writes: u64,
    pub log: Log,
    pub property: PropertyLog,
    /// A property reply posted and not yet decoded: its buffer's bus address.
    reply_to_decode: Option<u32>,
}

impl Mbox {
    pub fn new() -> Mbox {
        Mbox::default()
    }

    /// Post a request as the ARM would; false if MAIL1 is full.
    pub fn post_from_arm(&mut self, message: u32) -> bool {
        if self.to_vpu.len() >= DEPTH {
            return false;
        }
        crate::log!(
            self.log,
            Channel::Mbox,
            "ARM -> VPU {:#010x} (channel {}, addr {:#010x})",
            message,
            message & 0xF,
            message & !0xF
        );
        self.to_vpu.push_back(message);
        true
    }

    pub fn take_reply(&mut self) -> Option<u32> {
        self.to_arm.pop_front()
    }

    /// The bus address of a reply just posted, for
    /// [`crate::machine::Machine`] to decode: the buffer is in DRAM, which
    /// this device cannot see.
    pub fn take_property_reply(&mut self) -> Option<u32> {
        self.reply_to_decode.take()
    }

    /// MAIL1's `CONFIG` as the firmware reads it. Zero means the driver has
    /// not armed the mailbox and nothing queued can raise [`IRQ_SRC`].
    pub fn interrupt_armed(&self) -> u32 {
        self.config1_word()
    }

    /// True while the firmware has not drained the request we posted.
    pub fn request_outstanding(&self) -> bool {
        !self.to_vpu.is_empty()
    }

    /// A pending, enabled condition on either mailbox: source [`IRQ_SRC`].
    pub fn irq_asserted(&self) -> bool {
        (self.config0_word() | self.config1_word()) & CFG_PENDING != 0
    }

    /// The ARM's mailbox interrupt (INTID 65): mailbox 0's pending bits, which
    /// with Linux's enables mean "a reply is waiting in MAIL0".
    pub fn arm_irq_asserted(&self) -> bool {
        self.config0_word() & CFG_PENDING != 0
    }

    /// The interrupt-pending bits of one mailbox's `CONFIG`: each is its
    /// condition AND its own enable, which is the whole wake path (module
    /// docs).
    fn pending_bits(enables: u32, own: &VecDeque<u32>, opp: &VecDeque<u32>) -> u32 {
        let mut p = 0;
        if enables & CFG_EN_HAVE_DATA != 0 && !own.is_empty() {
            p |= CFG_PEND_HAVE_DATA;
        }
        if enables & CFG_EN_HAVE_SPACE != 0 && own.len() < DEPTH {
            p |= CFG_PEND_HAVE_SPACE;
        }
        if enables & CFG_EN_OPP_EMPTY != 0 && opp.is_empty() {
            p |= CFG_PEND_OPP_EMPTY;
        }
        p
    }

    fn config0_word(&self) -> u32 {
        self.config0 | Mbox::pending_bits(self.config0, &self.to_arm, &self.to_vpu)
    }

    fn config1_word(&self) -> u32 {
        self.config1 | Mbox::pending_bits(self.config1, &self.to_vpu, &self.to_arm)
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

    /// What a read of `offset` returns, for every register a read leaves as it
    /// is — all but the FIFO data words, which pop. The ARM run loop watches
    /// these while a core busy-waits on one.
    pub fn peek(&self, offset: u32) -> Option<u32> {
        // The gap between the two windows: nothing of the mailbox's is in it.
        if PEND_BLOCK.contains(&offset) {
            return Some(0);
        }
        let vpu = offset >= VPU;
        Some(match (offset & !3) % WINDOW {
            DATA0 if !vpu => return None,
            DATA1 if vpu => return None,
            PEEK0 => self.to_arm.front().copied().unwrap_or(0),
            STATUS0 => Mbox::status(&self.to_arm),
            SENDER0 => self.sender0,
            CONFIG0 => self.config0_word(),
            PEEK1 => self.to_vpu.front().copied().unwrap_or(0),
            STATUS1 => Mbox::status(&self.to_vpu),
            SENDER1 => self.sender1,
            CONFIG1 => self.config1_word(),
            _ => 0,
        })
    }
}

impl MmioDevice for Mbox {
    fn name(&self) -> &'static str {
        "mbox"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        if let Some(v) = self.peek(offset) {
            return Ok(v);
        }
        let vpu = offset >= VPU;
        let reg = (offset & !3) % WINDOW;
        Ok(match reg {
            DATA0 if !vpu => self.to_arm.pop_front().unwrap_or(0),
            // The ARM->VPU FIFO, drained by the `mbox_read` task's receive op.
            DATA1 if vpu => {
                let v = self.to_vpu.pop_front().unwrap_or(0);
                if v != 0 {
                    self.reads += 1;
                    crate::log!(self.log, Channel::Mbox, "VPU read request {v:#010x}");
                }
                v
            }
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        if PEND_BLOCK.contains(&offset) {
            return Ok(());
        }
        let vpu = offset >= VPU;
        let reg = (offset & !3) % WINDOW;
        match reg {
            DATA0 if vpu => {
                if self.to_arm.len() < DEPTH {
                    self.to_arm.push_back(value);
                    self.writes += 1;
                }
                if value & 0xF == CHANNEL_PROPERTY {
                    self.reply_to_decode = Some(value & !0xF);
                }
                crate::log!(self.log, Channel::Mbox, "VPU -> ARM {value:#010x}");
            }
            // The ARM posting a request; `post_from_arm` is the usual entry.
            DATA1 if !vpu => {
                self.post_from_arm(value);
            }
            SENDER0 => self.sender0 = value,
            // Bit 3 flushes and does not latch, bits 4..6 are ours to
            // compute: only the enables stay.
            CONFIG0 => {
                if value & CFG_CLEAR != 0 {
                    self.to_arm.clear();
                }
                self.config0 = value & CFG_ENABLES;
            }
            SENDER1 => self.sender1 = value,
            CONFIG1 => {
                if value & CFG_CLEAR != 0 {
                    self.to_vpu.clear();
                }
                self.config1 = value & CFG_ENABLES;
            }
            // Status comes from the queues; a write acknowledges nothing.
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
        let vpu = VPU;
        let mut m = Mbox::default();
        assert_eq!(m.read(vpu + STATUS1, Width::Word).unwrap(), STATUS_EMPTY);

        assert!(m.post_from_arm(0xC000_1000 | CHANNEL_PROPERTY));
        assert_eq!(m.read(vpu + STATUS1, Width::Word).unwrap(), 0);
        assert_eq!(m.read(vpu + PEEK1, Width::Word).unwrap(), 0xC000_1008);
        assert_eq!(m.read(vpu + DATA1, Width::Word).unwrap(), 0xC000_1008);
        assert_eq!(m.read(vpu + STATUS1, Width::Word).unwrap(), STATUS_EMPTY);
        assert_eq!(m.reads, 1);
    }

    #[test]
    fn a_reply_written_to_mail0_comes_back_to_the_arm_side() {
        let vpu = VPU;
        let mut m = Mbox::default();
        assert_eq!(m.take_reply(), None);
        m.write(vpu + DATA0, Width::Word, 0xC000_1008).unwrap();
        assert_eq!(m.read(STATUS0, Width::Word).unwrap(), 0);
        assert_eq!(m.take_reply(), Some(0xC000_1008));
        assert_eq!(m.take_reply(), None);
        assert_eq!(m.writes, 1);
    }

    #[test]
    fn the_config_word_carries_the_pending_bit_the_isr_releases_on() {
        let vpu = VPU;
        let mut m = Mbox::default();
        m.write(vpu + CONFIG1, Width::Word, CFG_CLEAR).unwrap();
        m.write(vpu + CONFIG1, Width::Word, CFG_EN_HAVE_DATA)
            .unwrap();
        assert_eq!(
            m.read(vpu + CONFIG1, Width::Word).unwrap(),
            CFG_EN_HAVE_DATA
        );
        assert!(!m.irq_asserted());

        assert!(m.post_from_arm(0xC000_1000 | CHANNEL_PROPERTY));
        assert_eq!(
            m.read(vpu + CONFIG1, Width::Word).unwrap(),
            CFG_EN_HAVE_DATA | CFG_PEND_HAVE_DATA
        );
        assert!(m.irq_asserted());

        m.write(vpu + CONFIG1, Width::Word, 0).unwrap();
        assert!(!m.irq_asserted());
        assert_eq!(m.read(vpu + DATA1, Width::Word).unwrap(), 0xC000_1008);
    }

    #[test]
    fn opp_empty_only_pends_once_the_sender_asks_for_it() {
        let vpu = VPU;
        let mut m = Mbox::default();
        m.write(vpu + CONFIG1, Width::Word, CFG_EN_HAVE_DATA)
            .unwrap();
        assert_eq!(
            m.read(vpu + CONFIG1, Width::Word).unwrap() & CFG_PEND_OPP_EMPTY,
            0
        );
        m.write(vpu + CONFIG1, Width::Word, CFG_EN_OPP_EMPTY)
            .unwrap();
        assert_eq!(
            m.read(vpu + CONFIG1, Width::Word).unwrap(),
            CFG_EN_OPP_EMPTY | CFG_PEND_OPP_EMPTY
        );
    }

    #[test]
    fn config_bit_three_flushes_the_fifo_and_does_not_latch() {
        let vpu = VPU;
        let mut m = Mbox::default();
        assert!(m.post_from_arm(CHANNEL_PROPERTY));
        m.write(vpu + CONFIG1, Width::Word, CFG_CLEAR).unwrap();
        assert!(!m.request_outstanding());
        assert_eq!(m.read(vpu + CONFIG1, Width::Word).unwrap(), 0);
    }

    #[test]
    fn a_property_reply_leaves_its_buffer_to_decode() {
        let mut m = Mbox::default();
        m.write(VPU + DATA0, Width::Word, 0xCEF0_0000 | CHANNEL_PROPERTY)
            .unwrap();
        assert_eq!(m.take_property_reply(), Some(0xCEF0_0000));
        assert_eq!(m.take_property_reply(), None);
        m.write(VPU + DATA0, Width::Word, 0xCEF0_0000 | 9).unwrap();
        assert_eq!(m.take_property_reply(), None);
    }

    #[test]
    fn a_property_reply_is_decoded_tag_by_tag() {
        // What start4 leaves: an answered tag, an unknown one left as staged,
        // then the end tag.
        let buf = [
            48,
            RESPONSE,
            0x0003_0058,
            4,
            RESPONSE | 4,
            0,
            0x0003_0999,
            4,
            0,
            0x1234,
            0,
            0,
        ];
        let mut log = PropertyLog::default();
        log.record(|o| buf.get(o as usize / 4).copied().unwrap_or(0));
        assert_eq!((log.replies, log.failed), (1, 0));
        let tags: Vec<_> = log.tags().collect();
        assert_eq!(
            tags,
            [
                (
                    0x0003_0058,
                    TagLog {
                        marked: 1,
                        unmarked: 0,
                        last: Some(0),
                        errors: 0,
                    }
                ),
                (
                    0x0003_0999,
                    TagLog {
                        marked: 0,
                        unmarked: 1,
                        last: Some(0x1234),
                        errors: 0,
                    }
                ),
            ]
        );

        // A parse error is charged to the tag it was carrying, so the report
        // says which request failed.
        let bad = [24, RESPONSE | 1, 0x0003_0058, 4, RESPONSE | 4, 0, 0, 0];
        log.record(|o| bad.get(o as usize / 4).copied().unwrap_or(0));
        assert_eq!((log.replies, log.failed), (2, 1));
        let after: Vec<_> = log.tags().collect();
        assert_eq!(after[0].1.errors, 1);
        assert_eq!(after[1].1.errors, 0);
    }

    #[test]
    fn the_fifos_are_eight_deep_and_report_full() {
        let mut m = Mbox::default();
        for i in 0..DEPTH {
            assert!(m.post_from_arm(0x1000 * i as u32 + CHANNEL_PROPERTY), "{i}");
        }
        assert_eq!(
            m.read(VPU + STATUS1, Width::Word).unwrap() & STATUS_FULL,
            STATUS_FULL
        );
        assert!(!m.post_from_arm(CHANNEL_PROPERTY));
    }
}
