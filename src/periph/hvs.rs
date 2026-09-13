//! BCM2711 HVS (Hardware Video Scaler) register block at `0x7E40_0000`.
//!
//! The main bootloader brings up a diagnostic display and, after queuing a
//! frame, calls a "channel swap" wait (`0x0008adc0`): it polls a pair of
//! per-channel words and spins — 100 × `delay(1000µs)` — until
//!
//! ```text
//!   (*current & 0xFFFF) == (*requested & 0xFFF)
//! ```
//!
//! `requested` lives at `+0x20 + 4*chan`, `current` at `+0x30 + 4*chan`. Real
//! hardware advances `current` to `requested` when the scanout hits the queued
//! frame; we have no display, so without help every swap runs the full timeout
//! (~0.5 s of modelled time each, hundreds of times) and the boot crawls.
//!
//! Model: reads of the `current` slot return whatever was last written to the
//! matching `requested` slot, so the swap always reports complete on the first
//! poll.
//!
//! ## Frame interrupts
//!
//! A running channel — `DISPCTRL.ENABLE` and its own `DISPCTRLX.ENABLE` —
//! raises three flags in `DISPSTAT` every [`FRAME_US`] (write one to clear):
//! `EOLN` when compositing reaches line `DISPEOLN`, `EOF` after the last active
//! line, and `VSTART` as the next frame starts. With `DISPEIRQx` and the flag's
//! own enable in `DISPCTRL` (the HVS5 layout, `SCALER5_DISPCTRL_*` in Linux's
//! `vc4_regs.h`) set, a flag holds VPU interrupt source [`IRQ_SRC`], and a
//! read of `DISPSTAT` shows `IRQDISPx` for it.
//!
//! start4 turns them all on, and its source-97 handler (`0x3ECEED5C`) drives
//! the display off them. On this HVS — start4's flag at `gp+0x1564` is set —
//! it posts a channel's display events on `VSTART` (`0x3ECEEFCE`); the older
//! path does the display-list work on an `EOLN` that comes without `EOF`
//! (`0x3ECEF014`). `NOTIFY_DISPLAY_DONE` waits on that work, so without these
//! interrupts it never came back: the mailbox service slept for good, and
//! every later property request from Linux timed out (#61).
//!
//! With no scanout to place them in, the flags come at fixed points of the
//! frame: `EOLN` half-way, `EOF` after 480 of the mode's 525 lines, `VSTART` at
//! its end. A `sleep` wakes for each on its own ([`Hvs::deadline`]), so the
//! handler sees them apart, as on the board.
//!
//! `DISPSTATX` reads a stopped channel as `MODE` disabled with its FIFO
//! `EMPTY` — the state in which start4 pauses a channel at once, without
//! waiting for a frame (`0x3ECEF33C`) — and a running one as `MODE` run.
//!
//! No pixel valve stands behind the channels, so the frame time is that of a
//! single mode: 640x480 at 60 Hz, the framebuffer UEFI allocates on this
//! board. Everything else in the block is plain sticky storage.

use std::collections::BTreeMap;

use crate::bus::{BusResult, MmioDevice, Width};

use crate::spec::hvs::{
    CURRENT, CURRENT_COUNT, CURRENT_STRIDE, DISPCTRL, DISPCTRLX, DISPCTRLX_COUNT,
    DISPCTRLX_ENABLE_MASK, DISPCTRLX_STRIDE, DISPCTRL_DISPEIRQ0_MASK, DISPCTRL_DISPEIRQ1_MASK,
    DISPCTRL_DISPEIRQ2_MASK, DISPCTRL_DSPEIEOF0_MASK, DISPCTRL_DSPEIEOF1_MASK,
    DISPCTRL_DSPEIEOF2_MASK, DISPCTRL_DSPEIEOLN0_MASK, DISPCTRL_DSPEIEOLN1_MASK,
    DISPCTRL_DSPEIEOLN2_MASK, DISPCTRL_DSPEIVST0_MASK, DISPCTRL_DSPEIVST1_MASK,
    DISPCTRL_DSPEIVST2_MASK, DISPCTRL_ENABLE_MASK, DISPID, DISPID_RESET as DISPID_VALUE, DISPSTAT,
    DISPSTATX, DISPSTATX_EMPTY_MASK, DISPSTATX_MODE_SHIFT, DISPSTATX_STRIDE, DISPSTAT_EOF0_MASK,
    DISPSTAT_EOF1_MASK, DISPSTAT_EOF2_MASK, DISPSTAT_EOLN0_MASK, DISPSTAT_EOLN1_MASK,
    DISPSTAT_EOLN2_MASK, DISPSTAT_IRQDISP0_MASK, DISPSTAT_IRQDISP1_MASK, DISPSTAT_IRQDISP2_MASK,
    DISPSTAT_VSTART0_MASK, DISPSTAT_VSTART1_MASK, DISPSTAT_VSTART2_MASK, REQUESTED,
};
use crate::spec::Coverage;

/// `DISPID` answers the measured id — start4 gates its whole display bring-up
/// on it, and 0 read as "no HVS" (issue #13) — the frame-swap words are
/// modelled, and so are the frame interrupts; the rest of the block is
/// storage.
pub const COVERAGE: Coverage = Coverage {
    block: "hvs",
    decoded: &[
        DISPCTRL, DISPSTAT, DISPID, REQUESTED, CURRENT, DISPCTRLX, DISPSTATX,
    ],
};

/// The HVS's VPU interrupt source: GIC SPI 97 in the device tree, and the
/// source start4 registers its HVS handler (`0x3ECEED5C`) on.
pub const IRQ_SRC: u32 = 97;

/// One frame of 640x480 at 60 Hz (CEA-861 VIC 1: 800 × 525 pixels at
/// 25.175 MHz), in µs.
pub const FRAME_US: u64 = 16_683;

/// `DISPSTATX.MODE` of a channel that is scanning out.
const MODE_RUN: u32 = 2;

const CHANNELS: usize = DISPCTRLX_COUNT as usize;

/// A flag every running channel raises once a frame.
struct FrameEvent {
    /// How far into the frame it comes, in µs.
    at: u64,
    /// Its `DISPSTAT` bit, per channel.
    flag: [u32; CHANNELS],
    /// Its interrupt enable in `DISPCTRL`, per channel.
    enable: [u32; CHANNELS],
}

/// In the order they come in a frame.
const EVENTS: [FrameEvent; 3] = [
    FrameEvent {
        at: FRAME_US / 2,
        flag: [
            DISPSTAT_EOLN0_MASK,
            DISPSTAT_EOLN1_MASK,
            DISPSTAT_EOLN2_MASK,
        ],
        enable: [
            DISPCTRL_DSPEIEOLN0_MASK,
            DISPCTRL_DSPEIEOLN1_MASK,
            DISPCTRL_DSPEIEOLN2_MASK,
        ],
    },
    FrameEvent {
        at: FRAME_US * 480 / 525,
        flag: [DISPSTAT_EOF0_MASK, DISPSTAT_EOF1_MASK, DISPSTAT_EOF2_MASK],
        enable: [
            DISPCTRL_DSPEIEOF0_MASK,
            DISPCTRL_DSPEIEOF1_MASK,
            DISPCTRL_DSPEIEOF2_MASK,
        ],
    },
    FrameEvent {
        at: FRAME_US,
        flag: [
            DISPSTAT_VSTART0_MASK,
            DISPSTAT_VSTART1_MASK,
            DISPSTAT_VSTART2_MASK,
        ],
        enable: [
            DISPCTRL_DSPEIVST0_MASK,
            DISPCTRL_DSPEIVST1_MASK,
            DISPCTRL_DSPEIVST2_MASK,
        ],
    },
];

const IRQDISP: [u32; CHANNELS] = [
    DISPSTAT_IRQDISP0_MASK,
    DISPSTAT_IRQDISP1_MASK,
    DISPSTAT_IRQDISP2_MASK,
];
const DISPEIRQ: [u32; CHANNELS] = [
    DISPCTRL_DISPEIRQ0_MASK,
    DISPCTRL_DISPEIRQ1_MASK,
    DISPCTRL_DISPEIRQ2_MASK,
];

/// Which channel's copy of a per-channel register at `base` offset `off` is.
fn channel_of(off: u32, base: u32, stride: u32) -> Option<usize> {
    let rel = off.checked_sub(base)?;
    (rel % stride == 0 && rel / stride < CHANNELS as u32).then_some((rel / stride) as usize)
}

#[derive(Default)]
pub struct Hvs {
    storage: BTreeMap<u32, u32>,
    /// `DISPSTAT`'s frame flags.
    flags: u32,
    /// When each running channel next raises each of [`EVENTS`], in model µs.
    next: [[Option<u64>; EVENTS.len()]; CHANNELS],
    /// The model time [`Hvs::advance_to`] last brought the block to.
    now: u64,
}

impl Hvs {
    pub fn new() -> Hvs {
        Hvs::default()
    }

    fn reg(&self, off: u32) -> u32 {
        self.storage.get(&off).copied().unwrap_or(0)
    }

    /// Is channel `x` scanning out?
    fn running(&self, x: usize) -> bool {
        self.reg(DISPCTRL) & DISPCTRL_ENABLE_MASK != 0
            && self.reg(DISPCTRLX + x as u32 * DISPCTRLX_STRIDE) & DISPCTRLX_ENABLE_MASK != 0
    }

    /// Does event `e` of channel `x` interrupt?
    fn irq_enabled(&self, x: usize, e: usize) -> bool {
        let ctrl = self.reg(DISPCTRL);
        ctrl & DISPCTRL_ENABLE_MASK != 0
            && ctrl & DISPEIRQ[x] != 0
            && ctrl & EVENTS[e].enable[x] != 0
    }

    /// Time a channel's frames from the moment it starts running, and stop
    /// when it stops.
    fn schedule(&mut self) {
        for x in 0..CHANNELS {
            if !self.running(x) {
                self.next[x] = Default::default();
            } else if self.next[x].iter().all(Option::is_none) {
                for (next, event) in self.next[x].iter_mut().zip(&EVENTS) {
                    *next = Some(self.now + event.at);
                }
            }
        }
    }

    /// Bring the block's clock to `now_us`, raising the flags it reaches. A
    /// jump across several frames raises each once: they are flags, not
    /// counts.
    pub fn advance_to(&mut self, now_us: u64) {
        if now_us <= self.now {
            return;
        }
        self.now = now_us;
        for x in 0..CHANNELS {
            for (next, event) in self.next[x].iter_mut().zip(&EVENTS) {
                if let Some(t) = *next {
                    if now_us >= t {
                        self.flags |= event.flag[x];
                        *next = Some(now_us + FRAME_US - (now_us - t) % FRAME_US);
                    }
                }
            }
        }
    }

    /// Is one of channel `x`'s flags holding the interrupt?
    fn irq_from(&self, x: usize) -> bool {
        (0..EVENTS.len()).any(|e| self.flags & EVENTS[e].flag[x] != 0 && self.irq_enabled(x, e))
    }

    /// True while a flag holds source [`IRQ_SRC`]. Level, not edge: it stays
    /// asserted until the handler writes the flag back.
    pub fn irq_asserted(&self) -> bool {
        (0..CHANNELS).any(|x| self.irq_from(x))
    }

    /// The next flag that will raise the interrupt, in model µs: a VPU
    /// `sleep` has to wake for it.
    pub fn deadline(&self) -> Option<u64> {
        (0..CHANNELS)
            .flat_map(|x| (0..EVENTS.len()).map(move |e| (x, e)))
            .filter(|&(x, e)| self.irq_enabled(x, e) && self.flags & EVENTS[e].flag[x] == 0)
            .filter_map(|(x, e)| self.next[x][e])
            .min()
    }
}

impl MmioDevice for Hvs {
    fn name(&self) -> &'static str {
        "hvs"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        let off = offset & !3;
        if off == DISPID {
            return Ok(DISPID_VALUE);
        }
        if off == DISPSTAT {
            let irq = (0..CHANNELS)
                .filter(|&x| self.irq_from(x))
                .fold(0, |v, x| v | IRQDISP[x]);
            return Ok(self.flags | irq);
        }
        if let Some(x) = channel_of(off, DISPSTATX, DISPSTATX_STRIDE) {
            return Ok(match self.running(x) {
                true => MODE_RUN << DISPSTATX_MODE_SHIFT,
                false => DISPSTATX_EMPTY_MASK,
            });
        }
        if (CURRENT..CURRENT + CURRENT_COUNT * CURRENT_STRIDE).contains(&off) {
            // Scanout instantly caught up to the requested frame.
            let requested = REQUESTED + (off - CURRENT);
            return Ok(self.reg(requested));
        }
        Ok(self.reg(off))
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        let off = offset & !3;
        if off == DISPID || channel_of(off, DISPSTATX, DISPSTATX_STRIDE).is_some() {
            // Read-only.
            return Ok(());
        }
        if off == DISPSTAT {
            // Write one to clear.
            self.flags &= !value;
            return Ok(());
        }
        self.storage.insert(off, value);
        if off == DISPCTRL || channel_of(off, DISPCTRLX, DISPCTRLX_STRIDE).is_some() {
            self.schedule();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HDMI0's channel, the one UEFI's framebuffer is on.
    const CHAN: usize = 1;
    const EOLN: usize = 0;
    const EOF: usize = 1;
    const VSTART: usize = 2;

    fn ctrlx(x: usize) -> u32 {
        DISPCTRLX + x as u32 * DISPCTRLX_STRIDE
    }

    fn statx(x: usize) -> u32 {
        DISPSTATX + x as u32 * DISPSTATX_STRIDE
    }

    fn flag(e: usize) -> u32 {
        EVENTS[e].flag[CHAN]
    }

    /// Start channel `x` with every frame interrupt on, as start4 leaves it.
    fn start(hvs: &mut Hvs, x: usize) {
        hvs.write(ctrlx(x), Width::Word, DISPCTRLX_ENABLE_MASK)
            .unwrap();
        let enables = EVENTS.iter().fold(0, |v, e| v | e.enable[x]);
        hvs.write(
            DISPCTRL,
            Width::Word,
            DISPCTRL_ENABLE_MASK | DISPEIRQ[x] | enables,
        )
        .unwrap();
    }

    fn stat(hvs: &mut Hvs) -> u32 {
        hvs.read(DISPSTAT, Width::Word).unwrap()
    }

    /// What start4's handler (`0x3ECEED5C`) writes back: the status it read,
    /// less the summary bits.
    fn ack(hvs: &mut Hvs) {
        let s = stat(hvs);
        let summary = IRQDISP.iter().fold(0, |v, m| v | m);
        hvs.write(DISPSTAT, Width::Word, s & !summary).unwrap();
    }

    #[test]
    fn a_running_channel_raises_each_flag_on_its_own_once_a_frame() {
        let mut hvs = Hvs::new();
        hvs.advance_to(5_000);
        start(&mut hvs, CHAN);
        hvs.advance_to(5_000 + EVENTS[EOLN].at - 1);
        assert_eq!(stat(&mut hvs), 0);
        assert!(!hvs.irq_asserted());
        for e in [EOLN, EOF, VSTART] {
            let at = 5_000 + EVENTS[e].at;
            assert_eq!(hvs.deadline(), Some(at), "event {e}");
            hvs.advance_to(at);
            assert_eq!(stat(&mut hvs), flag(e) | IRQDISP[CHAN], "event {e}");
            assert!(hvs.irq_asserted());
            ack(&mut hvs);
            assert_eq!(stat(&mut hvs), 0);
            assert!(!hvs.irq_asserted());
        }
        assert_eq!(hvs.deadline(), Some(5_000 + FRAME_US + EVENTS[EOLN].at));
    }

    #[test]
    fn a_long_jump_raises_each_flag_once_and_keeps_the_cadence() {
        let mut hvs = Hvs::new();
        start(&mut hvs, CHAN);
        hvs.advance_to(10 * FRAME_US + 7);
        assert_eq!(
            stat(&mut hvs),
            flag(EOLN) | flag(EOF) | flag(VSTART) | IRQDISP[CHAN]
        );
        ack(&mut hvs);
        assert!(!hvs.irq_asserted(), "flags, not counts");
        assert_eq!(hvs.deadline(), Some(10 * FRAME_US + EVENTS[EOLN].at));
    }

    #[test]
    fn each_flag_interrupts_only_with_its_own_enable() {
        let mut hvs = Hvs::new();
        hvs.write(ctrlx(CHAN), Width::Word, DISPCTRLX_ENABLE_MASK)
            .unwrap();
        let ctrl = DISPCTRL_ENABLE_MASK | DISPEIRQ[CHAN] | EVENTS[VSTART].enable[CHAN];
        hvs.write(DISPCTRL, Width::Word, ctrl).unwrap();
        assert_eq!(hvs.deadline(), Some(FRAME_US), "only VSTART wakes a core");
        hvs.advance_to(EVENTS[EOF].at);
        assert_eq!(stat(&mut hvs), flag(EOLN) | flag(EOF), "they still come");
        assert!(!hvs.irq_asserted());
        // Without DISPEIRQx no flag interrupts.
        hvs.write(DISPCTRL, Width::Word, ctrl & !DISPEIRQ[CHAN])
            .unwrap();
        hvs.advance_to(FRAME_US);
        assert!(!hvs.irq_asserted());
        assert_eq!(hvs.deadline(), None);
    }

    #[test]
    fn a_stopped_channel_reads_disabled_and_empty() {
        let mut hvs = Hvs::new();
        // The state in which start4 pauses a channel at once (`0x3ECEF33C`).
        assert_eq!(
            hvs.read(statx(CHAN), Width::Word).unwrap() & 0xD000_0000,
            0x1000_0000
        );
        start(&mut hvs, CHAN);
        assert_eq!(
            hvs.read(statx(CHAN), Width::Word).unwrap() >> DISPSTATX_MODE_SHIFT,
            MODE_RUN
        );
    }

    #[test]
    fn stopping_a_channel_stops_its_frames() {
        let mut hvs = Hvs::new();
        start(&mut hvs, CHAN);
        hvs.advance_to(EVENTS[EOLN].at);
        assert!(hvs.irq_asserted());
        // The handler acks the flag and stops the channel, as a pause
        // finishing on a frame does (`0x3ECEEF80`).
        ack(&mut hvs);
        let c = hvs.read(ctrlx(CHAN), Width::Word).unwrap();
        hvs.write(ctrlx(CHAN), Width::Word, c & !DISPCTRLX_ENABLE_MASK)
            .unwrap();
        hvs.advance_to(10 * FRAME_US);
        assert_eq!(stat(&mut hvs), 0);
        assert_eq!(hvs.deadline(), None);
        assert_eq!(
            hvs.read(statx(CHAN), Width::Word).unwrap() & 0xD000_0000,
            0x1000_0000
        );
    }

    #[test]
    fn scanout_catches_up_with_a_queued_frame() {
        let mut hvs = Hvs::new();
        hvs.write(REQUESTED + 4, Width::Word, 0x123).unwrap();
        assert_eq!(hvs.read(CURRENT + 4, Width::Word).unwrap(), 0x123);
    }
}
