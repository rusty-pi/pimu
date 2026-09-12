<!-- generated from specs/vce_ctrl.toml by `cargo run -- spec-docs --update` – do not edit -->

# `vce_ctrl` – VCE control block: status, launch, interrupt clear and endcode enables

- Bus: `vpu` (VPU bus address)
- Base: `0x7F140000`
- Size: `0x1000`

Same device as `vce`; its offsets are relative to 0x7F140000. A completed launch raises interrupt source 68 with STATUS.INT set until vce_clear_interrupt acks it.

Sources:

- decompile (high): vce_run_start, vce_run_complete, vce_clear_interrupt and the source-68 handler 0x3ED9D1EA
- trace (high): RVF_DBG_IRQTBL=1: src 68 handler=0x3ed9d1ea

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`STATUS`](#status) | r | 32 | 1, best high |
| `0x008`–`0x014` (4 × 0x4) | [`PC`](#pc) | rw | 32 | 1, best high |
| `0x020` | [`RUN`](#run) | rw | 32 | 1, best high |
| `0x024` | [`INTCLR`](#intclr) | w | 32 | 1, best high |
| `0x028` | [`ENDCODE_ENABLE`](#endcode_enable) | rw | 32 | 1, best high |
| `0x030` | [`BAD_ADDR`](#bad_addr) | r | 32 | 1, best high |

## `STATUS`

Offset `0x000` · access `r` · 32 bits

Engine status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 20:16 | `ENDCODE` | r | Endcode the program halted on. 5 is a clock stall the handler services itself. |
| 24 | `IDLE_CHECK` | r | vce_obtain_semaphore and vce_release_semaphore assert it is clear on an idle engine; nothing else is known. |
| 31 | `INT` | r | Interrupt pending; the model drives source 68 while it is set. |

Sources:

- decompile (high): handler 0x3ED9D1EA: endcode = ([0x7F140000] >> 16) & 0x1F

`ENDCODE` sources:

- decompile (high): handler 0x3ED9D1EA; vce_run_complete requires 0 for the licence check

`IDLE_CHECK` sources:

- decompile (low): asserts in start4db.elf's vce_obtain_semaphore / vce_release_semaphore

`INT` sources:

- decompile (high): vce_clear_interrupt writes 0x80000000 to INTCLR, then asserts STATUS & 0x80000000 == 0

## `PC`

Offset `0x008`, 4 elements 0x4 apart · access `rw` · 32 bits

Per-core program counters; PC[0] takes the start pc. PC[1..3] read 0, nothing executes.

Sources:

- decompile (high): vce_run_start writes the start pc to +0x08; the timeout message prints +0x08..+0x14, +0x14 as pc_ex0

## `RUN`

Offset `0x020` · access `rw` · 32 bits

Write 1 to launch; the semaphore ops write 0. Reads 0, the engine is always already halted.

Sources:

- decompile (high): vce_run_start writes 1; vce_obtain_semaphore / vce_release_semaphore write 0

## `INTCLR`

Offset `0x024` · access `w` · 32 bits

Write 1 to clear: bit 31 the interrupt, bit n endcode n.

Sources:

- decompile (high): vce_clear_interrupt writes 0x80000000; vce_run_start 0xFF; the ISR 0x20 for endcode 5; vce_run_complete 1

## `ENDCODE_ENABLE`

Offset `0x028` · access `rw` · 32 bits

Endcodes a launch accepts. vce_run_start writes (1 << endcode) | 0x20, and only when the endcode is non-zero.

Sources:

- decompile (high): vce_run_start

## `BAD_ADDR`

Offset `0x030` · access `r` · 32 bits

Non-zero is 'bad address in VCE ld/st'. Reads 0.

Sources:

- decompile (high): vce_run_complete's assert text
