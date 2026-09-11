//! ASB — the AXI async slave bridges at `0x7E00_A000`.
//!
//! Each of the SoC's video blocks (V3D, ISP, H264) sits behind a pair of
//! asynchronous AXI bridges — one for the block's slave port, one for its
//! master port — that let the block's clock domain be stopped independently of
//! the VC bus. Before a power domain can be gated off, both of its bridges have
//! to be told to stop and then acknowledge that they have drained; before the
//! domain comes back, both have to be released and acknowledge that too. That
//! handshake is the whole content of this block.
//!
//! The block was unmapped, so every status read came back 0 from the peripheral
//! stub, the acknowledge bit never appeared, and the boot spun forever at
//! `0x3ED550A8` — the H264 bridge-stop poll — just after reference line 65
//! (`uart: Baud rate change done`).
//!
//! ## Register map
//!
//! Straight out of Linux's `drivers/pmdomain/bcm/bcm2835-power.c` (formerly
//! `drivers/soc/bcm/`), which maps this same base from the `raspberrypi,bcm2835-power`
//! node and names every register:
//!
//! | Offset | Linux symbol | What |
//! | --- | --- | --- |
//! | `+0x00` | `ASB_BRDG_VERSION` | bridge version |
//! | `+0x04` | `ASB_CPR_CTRL` | clock/power-reduction control |
//! | `+0x08` | `ASB_V3D_S_CTRL` | V3D slave-port bridge |
//! | `+0x0C` | `ASB_V3D_M_CTRL` | V3D master-port bridge |
//! | `+0x10` | `ASB_ISP_S_CTRL` | ISP slave-port bridge |
//! | `+0x14` | `ASB_ISP_M_CTRL` | ISP master-port bridge |
//! | `+0x18` | `ASB_H264_S_CTRL` | H264 slave-port bridge |
//! | `+0x1C` | `ASB_H264_M_CTRL` | H264 master-port bridge |
//! | `+0x20` | `ASB_AXI_BRDG_ID` | reads `0x6272_6467`, `"brdg"` |
//!
//! and the bits of a `*_CTRL` register:
//!
//! | Bit | Linux symbol | What |
//! | --- | --- | --- |
//! | 0 | `ASB_REQ_STOP` | request: stop this bridge |
//! | 1 | `ASB_ACK` | acknowledge: the bridge is stopped |
//! | 2 | `ASB_EMPTY` | the bridge's transaction queue is empty |
//! | 3 | `ASB_FULL` | the bridge's transaction queue is full |
//!
//! ## The firmware's own corroboration
//!
//! start4's power-domain switch is `FUN_0ED54E40` (`0x3ED54E40`), a `switch`
//! over a domain bitmask. Each arm drives one ASB pair and then one PM register,
//! and the PM register is what identifies the pair — the PM bit names are in the
//! same Linux file:
//!
//! | Domain bit | ASB pair | PM register write |
//! | --- | --- | --- |
//! | `0x40` | `+0x08` / `+0x0C` | `PM_GRAFX` (`0x7E10010C`) `& ~BIT(6)` = `PM_V3DRSTN` |
//! | `0x08` | `+0x10` / `+0x14` | `PM_IMAGE` (`0x7E100108`) `& ~BIT(8)` = `PM_ISPRSTN` |
//! | `0x04` | `+0x18` / `+0x1C` | `PM_IMAGE` `& ~BIT(7)` = `PM_H264RSTN` |
//!
//! So V3D/ISP/H264 line up with `ASB_V3D_*`/`ASB_ISP_*`/`ASB_H264_*` exactly,
//! and the wall — `+0x18` / `+0x1C` — is the H264 pair.
//!
//! ## The acknowledge polarity
//!
//! Ghidra renders the stop sequence as a poll that *precedes* the request:
//!
//! ```c
//! do { } while ((_DAT_7e00a01c & 2) == 0);
//! do { } while ((_DAT_7e00a018 & 2) == 0);
//! _DAT_7e00a018 = _DAT_7e00a018 | 1;
//! _DAT_7e00a01c = _DAT_7e00a01c | 1;
//! ```
//!
//! which cannot be right: it waits for `ACK` before asking for anything, and
//! the matching release sequence waits for `ACK` to clear before clearing
//! `REQ_STOP`. No mirroring of `REQ_STOP` into `ACK` satisfies both. The
//! decompiler has sunk the stores below the loops; the instructions
//! (`rpi-virt-fw recon firmware/start4.elf --disasm 0xced550a2:12`, ELF vaddr =
//! runtime + `0x9000_0000`) say otherwise:
//!
//! ```text
//!   0xced550a2:  ld   r2, [r4+28]       ; r4 = 0x7E00A000, so +0x1C
//!   0xced550a4:  bset r2, #0            ; REQ_STOP
//!   0xced550a6:  st   r2, [r4+28]
//!   0xced550a8:  ld   r3, [r4+28]       ; <- the wall
//!   0xced550aa:  btest r3, #1           ; ACK
//!   0xced550ac:  beq  0xced550a8        ; spin while ACK == 0
//! ```
//!
//! and the release path at `0xced55782`:
//!
//! ```text
//!   0xced55782:  ld   r1, [r5+24]       ; +0x18
//!   0xced55784:  and  r1, r1, #-2       ; clear REQ_STOP
//!   0xced55788:  st   r1, [r5+24]
//!   0xced5578a:  ld   r0, [r5+24]
//!   0xced5578c:  btest r0, #1
//!   0xced5578e:  bne  0xced5578a        ; spin while ACK == 1
//! ```
//!
//! Request first, then wait for `ACK` to follow — which is precisely what
//! `bcm2835_asb_control()` does:
//!
//! ```c
//! if (enable) val = readl(base + reg) & ~ASB_REQ_STOP;
//! else        val = readl(base + reg) | ASB_REQ_STOP;
//! writel(PM_PASSWORD | val, base + reg);
//! readl_poll_timeout_atomic(base + reg, val, !!(val & ASB_ACK) != enable, 0, 100);
//! ```
//!
//! There is no contradiction to resolve once the real instruction order is in
//! hand: `ACK` follows `REQ_STOP`, and both of the firmware's phases run in this
//! boot (the stop phase for H264, and the release phases for whichever domains
//! come back up). The model therefore makes `ACK` mirror `REQ_STOP`.
//!
//! ## What is modelled and what is left at a default
//!
//! Modelled: `REQ_STOP` is the only writable bit of a `*_CTRL` register, `ACK`
//! mirrors it, and `ASB_AXI_BRDG_ID` reads `"brdg"`. The mirror is immediate.
//! On real silicon `ACK` lags by however long the bridge takes to drain — Linux
//! allows it 100 µs — but the model has no queued AXI traffic to drain, so zero
//! is the honest latency, and both the firmware and Linux only ever poll.
//!
//! Password: Linux ORs `PM_PASSWORD` (`0x5A00_0000`) into every ASB write and
//! start4 does not. Writes are accepted either way and the top bits are ignored,
//! since nothing observable distinguishes the two and this model has no reason
//! to reject one of its two known drivers.
//!
//! Left at a default: `EMPTY` reads 1 and `FULL` reads 0 on every bridge.
//! Nothing in the model issues transactions through these bridges, so their
//! queues are genuinely always empty; a stopped-and-acknowledged bridge is
//! drained by definition, and an idle running one is empty too. Neither start4
//! nor Linux reads either bit. `ASB_BRDG_VERSION` and `ASB_CPR_CTRL` are plain
//! read/write storage reading 0 — start4 never touches them (the only offsets
//! it uses are `+0x08`..`+0x1C`), and inventing a version word would be
//! fabrication.
//!
//! Reset value: every bridge comes up with `REQ_STOP` clear, i.e. running. This
//! is don't-care for the handshake — `ACK` mirrors `REQ_STOP` whatever it
//! starts at — and "running" is the state the firmware's stop phase implicitly
//! assumes when it asks a live domain to quiesce.
//!
//! ## The `0x7F50_0E80` reads next door
//!
//! Immediately before the H264 stop sequence, `FUN_0ED54E40` reads
//! `0x7F50_0E80` sixteen times and sums the results. The block is unmapped too,
//! so the sum is 0 — and that is fine, because the sum is dead. The
//! instructions at `0xced55096`..`0xced550c6` spill it to the stack and then
//! load `r0 = 28` and `r1`..`r5 = 0` before calling `FUN_0ED5625A`, which is a
//! trace hook (`if (gp+0xD3590 == 0) FUN_0EC7FCA6()`); Ghidra shows the sum as
//! a seventh argument only because it mismatches the callee's signature. The
//! reads are a bus-drain before the domain is gated, and nothing consumes what
//! they return. They stay unmapped deliberately.
//!
//! Not modelled: the bridges do not actually gate anything. Stopping the H264
//! bridge in the model does not make accesses to the H264 block fail, because
//! there is no H264 block behind it. Nor is there a second ASB instance —
//! BCM2711 has an `rpivid_asb` that Linux uses for V3D instead of this one
//! (`bcm2835_asb_control()`'s `power->rpivid_asb` switch); start4 does not use
//! it on this path and it stays unmapped.

use crate::bus::{BusResult, MmioDevice, Width};

/// Base of the ASB register block.
pub const BASE: u32 = 0x7E00_A000;
/// One peripheral page, as every other block in this window gets.
pub const SIZE: u32 = 0x1000;

/// `ASB_BRDG_VERSION`.
const BRDG_VERSION: u32 = 0x00;
/// `ASB_CPR_CTRL`.
const CPR_CTRL: u32 = 0x04;
/// First `*_CTRL` register (`ASB_V3D_S_CTRL`).
const CTRL_FIRST: u32 = 0x08;
/// Last `*_CTRL` register (`ASB_H264_M_CTRL`).
const CTRL_LAST: u32 = 0x1C;
/// `ASB_AXI_BRDG_ID`.
const AXI_BRDG_ID: u32 = 0x20;

/// The value Linux's probe requires at [`AXI_BRDG_ID`]: `BCM2835_BRDG_ID`,
/// which is `"brdg"` little-endian.
const BRDG_ID: u32 = 0x6272_6467;

/// `ASB_REQ_STOP`.
const REQ_STOP: u32 = 1 << 0;
/// `ASB_ACK`.
const ACK: u32 = 1 << 1;
/// `ASB_EMPTY`.
const EMPTY: u32 = 1 << 2;

/// Number of `*_CTRL` registers: three blocks, slave and master port each.
const BRIDGES: usize = ((CTRL_LAST - CTRL_FIRST) / 4 + 1) as usize;

#[derive(Default)]
pub struct Asb {
    /// Per-bridge `REQ_STOP`. `ACK` is derived from it, never stored.
    stopped: [bool; BRIDGES],
    /// `ASB_BRDG_VERSION` / `ASB_CPR_CTRL`, plain storage.
    version: u32,
    cpr_ctrl: u32,
}

impl Asb {
    pub fn new() -> Asb {
        Asb::default()
    }

    fn bridge(offset: u32) -> Option<usize> {
        if (CTRL_FIRST..=CTRL_LAST).contains(&offset) && offset.is_multiple_of(4) {
            Some(((offset - CTRL_FIRST) / 4) as usize)
        } else {
            None
        }
    }

    /// A bridge's status word. `ACK` mirrors `REQ_STOP`; the queue is always
    /// empty because nothing in the model drives traffic through the bridge.
    fn ctrl(&self, i: usize) -> u32 {
        let mut v = EMPTY;
        if self.stopped[i] {
            v |= REQ_STOP | ACK;
        }
        v
    }
}

impl MmioDevice for Asb {
    fn name(&self) -> &'static str {
        "asb"
    }

    fn read(&mut self, offset: u32, _width: Width) -> BusResult<u32> {
        if let Some(i) = Asb::bridge(offset) {
            return Ok(self.ctrl(i));
        }
        Ok(match offset {
            BRDG_VERSION => self.version,
            CPR_CTRL => self.cpr_ctrl,
            AXI_BRDG_ID => BRDG_ID,
            _ => 0,
        })
    }

    fn write(&mut self, offset: u32, _width: Width, value: u32) -> BusResult<()> {
        if let Some(i) = Asb::bridge(offset) {
            // `REQ_STOP` is the only writable bit. The `PM_PASSWORD` Linux ORs
            // in (and start4 does not) lands in the ignored top bits.
            self.stopped[i] = value & REQ_STOP != 0;
            return Ok(());
        }
        match offset {
            BRDG_VERSION => self.version = value,
            CPR_CTRL => self.cpr_ctrl = value,
            _ => {}
        }
        Ok(())
    }
}
