<!-- generated from specs/mbox.toml by `cargo run -- spec-docs --update` – do not edit -->

# `mbox` – ARM <-> VideoCore mailboxes: two views of the same pair of FIFOs, and the interrupt block between them

- Bus: `vpu` (VPU bus address)
- Base: `0x7E00B880`
- Size: `0x140`

Every mailbox register is a two-element array 0x100 apart: element 0 is the ARM's view (what Linux's device tree names), element 1 the VPU's (what start4 drives). The two views are mirror images: the ARM posts requests into MAIL1 and reads replies from MAIL0, the VPU the other way round. The pending words sit in a block at +0xC0 between the views.

Sources:

- linux (high): mailbox@7e00b880, brcm,bcm2835-mbox, reg = <0x7e00b880 0x40>; confirmed in the reference board's /proc/device-tree
- decompile (high): start4's receive op 0x3EC5AC0C loads 0x7E00B980 as a literal; its ISR 0x3EC58302 reads 0x7E00B940

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000`–`0x100` (2 × 0x100) | [`DATA0`](#data0) | rw | 32 | 1, best high |
| `0x010`–`0x110` (2 × 0x100) | [`PEEK0`](#peek0) | r | 32 | 1, best medium |
| `0x014`–`0x114` (2 × 0x100) | [`SENDER0`](#sender0) | rw | 32 | 1, best medium |
| `0x018`–`0x118` (2 × 0x100) | [`STATUS0`](#status0) | r | 32 | 1, best high |
| `0x01C`–`0x11C` (2 × 0x100) | [`CONFIG0`](#config0) | rw | 32 | 2, best high |
| `0x020`–`0x120` (2 × 0x100) | [`DATA1`](#data1) | rw | 32 | 2, best high |
| `0x030`–`0x130` (2 × 0x100) | [`PEEK1`](#peek1) | r | 32 | 1, best medium |
| `0x034`–`0x134` (2 × 0x100) | [`SENDER1`](#sender1) | rw | 32 | 1, best medium |
| `0x038`–`0x138` (2 × 0x100) | [`STATUS1`](#status1) | r | 32 | 2, best high |
| `0x03C`–`0x13C` (2 × 0x100) | [`CONFIG1`](#config1) | rw | 32 | 2, best high |
| `0x0C8` | [`PEND0`](#pend0) | rw | 32 | 1, best high |
| `0x0CC` | [`PEND1`](#pend1) | rw | 32 | 2, best high |

## `DATA0`

Offset `0x000`, 2 elements 0x100 apart · access `rw` · 32 bits

MAIL0 data: the VPU writes replies, the ARM reads them.

Sources:

- linux (high): bcm2835-mailbox.c reads replies at +0x00

## `PEEK0`

Offset `0x010`, 2 elements 0x100 apart · access `r` · 32 bits

MAIL0 head, without popping it.

Sources:

- linux (medium): bcm2835-mailbox.c register list (MAIL0_PEEK)

## `SENDER0`

Offset `0x014`, 2 elements 0x100 apart · access `rw` · 32 bits

MAIL0 sender tag.

Sources:

- linux (medium): bcm2835-mailbox.c register list (MAIL0_SENDER)

## `STATUS0`

Offset `0x018`, 2 elements 0x100 apart · access `r` · 32 bits

MAIL0 FIFO state. STATUS1 has the same layout.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 30 | `EMPTY` | r | Nothing queued. |
| 31 | `FULL` | r | No room for another word; the FIFOs are 8 deep. |

Sources:

- linux (high): bcm2835-mailbox.c: ARM_MS_FULL / ARM_MS_EMPTY

`EMPTY` sources:

- decompile (high): receive op 0x3EC5AC0C tests bit 30 of the status word

`FULL` sources:

- linux (high): bcm2835-mailbox.c: ARM_MS_FULL

## `CONFIG0`

Offset `0x01C`, 2 elements 0x100 apart · access `rw` · 32 bits

MAIL0 interrupt enables and pending flags. CONFIG1 has the same layout. A pending flag is its condition AND its enable, not the raw condition.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `EN_HAVE_DATA` | rw | Interrupt while this mailbox has data. |
| 1 | `EN_HAVE_SPACE` | rw | Interrupt while this mailbox has room. |
| 2 | `EN_OPP_EMPTY` | rw | Interrupt while the opposite mailbox is empty: how a sender waits for the far side to drain. |
| 3 | `CLEAR` | w | Flush this mailbox's FIFO. Does not latch. |
| 4 | `PEND_HAVE_DATA` | r | EN_HAVE_DATA's condition is true. |
| 5 | `PEND_HAVE_SPACE` | r | EN_HAVE_SPACE's condition is true. |
| 6 | `PEND_OPP_EMPTY` | r | EN_OPP_EMPTY's condition is true. |

Sources:

- linux (high): bcm2835-mailbox.c sets ARM_MC_IHAVEDATAIRQEN (bit 0) at +0x1C
- decompile (high): ISR 0x3EC58302 reads 0x7E00B9BC, releases the receive lock on bit 4 and the send lock on bit 6, writes the rest back

`EN_HAVE_DATA` sources:

- linux (high): bcm2835-mailbox.c: ARM_MC_IHAVEDATAIRQEN

`EN_HAVE_SPACE` sources:

- inferred (medium): the enable matching pending bit 5

`EN_OPP_EMPTY` sources:

- decompile (medium): send op arms it; ISR 0x3EC58302 releases the send lock on the matching pending bit 6

`CLEAR` sources:

- decompile (medium): the driver's init writes 8, then 1, to CONFIG1

`PEND_HAVE_DATA` sources:

- decompile (high): ISR 0x3EC58302: bit 4 releases the mbox_read task's lock gp+243076

`PEND_HAVE_SPACE` sources:

- inferred (low): sits between the two pending bits the ISR tests

`PEND_OPP_EMPTY` sources:

- decompile (high): ISR 0x3EC58302: bit 6 releases the send lock gp+243080

## `DATA1`

Offset `0x020`, 2 elements 0x100 apart · access `rw` · 32 bits

MAIL1 data: the ARM posts requests, the VPU reads them. A message is the bus address of a buffer with the channel in the low nibble.

Sources:

- linux (high): bcm2835-mailbox.c writes requests at +0x20
- decompile (high): receive op 0x3EC5AC0C reads 0x7E00B9A0

## `PEEK1`

Offset `0x030`, 2 elements 0x100 apart · access `r` · 32 bits

MAIL1 head, without popping it.

Sources:

- inferred (medium): MAIL0's layout, 0x20 higher

## `SENDER1`

Offset `0x034`, 2 elements 0x100 apart · access `rw` · 32 bits

MAIL1 sender tag.

Sources:

- inferred (medium): MAIL0's layout, 0x20 higher

## `STATUS1`

Offset `0x038`, 2 elements 0x100 apart · access `r` · 32 bits

MAIL1 FIFO state, laid out as STATUS0.

Sources:

- linux (high): bcm2835-mailbox.c polls +0x38 for FULL before posting
- decompile (high): receive op 0x3EC5AC0C tests bit 30 of 0x7E00B9B8

## `CONFIG1`

Offset `0x03C`, 2 elements 0x100 apart · access `rw` · 32 bits

MAIL1 interrupt enables and pending flags, laid out as CONFIG0.

Sources:

- decompile (high): receive op 0x3EC5AC0C arms it on empty; ISR 0x3EC58302 reads and re-arms 0x7E00B9BC
- trace (high): pinned start4 writes the VPU's element (0x7E00B9BC) 0x8 at 0x3EC5AF6C and 0x1 at 0x3EC5AF7C, shortly before it releases the ARM

## `PEND0`

Offset `0x0C8` · access `rw` · 32 bits

Mailbox 0 wants service. Computed; writes do not latch.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 2 | `SERVICE` | rw | Dispatch this mailbox's registered callback. |

Sources:

- decompile (high): ISR 0x3EC58302: Load r0, [0x7E00B940 + 8]; Btest r0, #2

`SERVICE` sources:

- decompile (high): ISR 0x3EC58302 calls [gp+243092] on bit 2

## `PEND1`

Offset `0x0CC` · access `rw` · 32 bits

Mailbox 1 (ARM -> VPU) wants service; set while CONFIG1 has an enabled condition pending. Same layout as PEND0.

Sources:

- decompile (high): ISR 0x3EC58302: Load r5, [0x7E00B940 + 12]; Btest r5, #2
- trace (high): --log irqtbl: src 94 handler=0x3ec58302
