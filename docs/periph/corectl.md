<!-- generated from specs/corectl.toml by `cargo run -- spec-docs --update` – do not edit -->

# `corectl` – VPU core control: per-core boot handshake and interrupt controller

- Bus: `vpu` (VPU bus address)
- Base: `0x7E002000`
- Size: `0x1000`
- Banks: 2 × `0x800`; offsets below are for bank 0

One register bank per VPU core: core 0 at +0x000, core 1 at +0x800. start4 reaches its bank through a per-core pointer, so both cores run the same code.

Sources:

- decompile (high): per-core init 0x3EC3E938 sets [blk+12] = 0x7E002000 + core * 0x800
- trace (high): RVF_DBG_IRQEN and the peripheral stub show core 1 writing 0x7E002810..0x7E002844 — _the window was mapped 0x100 wide until commit 7bd21a3, which hid core 1's bank_
- inferred (medium): size: the system timer starts at 0x7E003000

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x004` | [`IRQ_PENDING`](#irq_pending) | r | 32 | 1, best medium |
| `0x010`–`0x01C` (4 × 0x4) | [`IRQ_PRIO`](#irq_prio) | rw | 32 | 5, best high |
| `0x030` | [`VBASE`](#vbase) | rw | 32 | 2, best high |
| `0x040`–`0x044` (2 × 0x4) | [`IRQ_PENDING_BITS`](#irq_pending_bits) | rw | 32 | 2, best high |

## `IRQ_PENDING`

Offset `0x004` · access `r` · 32 bits

Which interrupt the dispatcher should service next.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 8 | `VALID` | r | Set while an interrupt is pending. |
| 5:0 | `SOURCE` | r | Source number minus 64; the dispatcher ORs 64 back in. |

Sources:

- decompile (medium): dispatcher 0x3EC3E9BC: r0 = [blk+4], btest r0, 8, or 64, mask to 7 bits, index the handler table at gp+58004

`VALID` sources:

- decompile (high): dispatcher 0x3EC3E9BC: btest r0, 8

`SOURCE` sources:

- decompile (medium): dispatcher 0x3EC3E9BC: or r0, 64, then a 7-bit mask — _the dispatcher keeps 7 bits after the OR, so bit 6 may belong to the field too_

## `IRQ_PRIO`

Offset `0x010`, 4 elements 0x4 apart · access `rw` · 32 bits

One 4-bit enable/priority field per interrupt source, eight per word: source `src` lives in word `(src >> 3) & 3` at bit `(src & 7) * 4`. Zero disables the source; a non-zero value enables it at that priority. The vector is the interrupt number, 64 + source, not this field.

Sources:

- decompile (high): enable_irq_source(src, prio) at 0x3ED72374
- trace (high): start4 calls enable_irq_source(64, 1) for its ThreadX tick
- inferred (low): core 1 release: the model treats a code-address write (>= 0x1000) to the first two words of bank 0 as core 1's start vector — _these words otherwise only ever hold small priority bitfields; the real release mechanism is not decoded_
- inferred (medium): hermanhermitage/videocoreiv, VideoCore IV Programmers Manual: 128 vector-table entries indexed by interrupt number, 0-31 exceptions, 32-63 swi, 64-127 external interrupts — _a reverse-engineered manual, not a datasheet_
- trace (high): vectoring at the field's value reached start4's exception stubs (dbe4e25, b9d53b8); vectoring at 64 + source reaches the per-source handlers (2bdbcbf)

## `VBASE`

Offset `0x030` · access `rw` · 32 bits

Exception-vector base for this core.

Sources:

- decompile (high): start4 entry trampoline: mov r1, #0x7E002030, then stores the vector base through it
- trace (high): core-control write trace: +0x30 and +0x830 both take 0xFEC01E00, nothing writes +0x38 — _replaced an earlier +0x38 guess for core 1 (commit 06a8447)_

## `IRQ_PENDING_BITS`

Offset `0x040`, 2 elements 0x4 apart · access `rw` · 32 bits

Pending bitmask, one bit per source: word 0 covers sources 64..95, word 1 sources 96..127. Setting a bit raises that interrupt on this core in software; each ISR clears its bit on entry.

Sources:

- decompile (high): 0x3ED01896 raises (|= 1 << bit), 0x3ED01792 acknowledges (&= ~(1 << bit)), 0x3ED01980 reads one bit
- trace (high): the clock service re-posts its own source 66 this way; sources 78 / 79 are the inter-core reschedule IPI
