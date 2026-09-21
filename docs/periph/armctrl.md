<!-- generated from specs/armctrl.toml by `cargo run -- spec-docs --update` – do not edit -->

# `armctrl` – ARM control block as the VPU sees it, below the mailboxes: where `arm_loader` releases the ARM

- Bus: `vpu` (VPU bus address)
- Base: `0x7E00B000`
- Size: `0x880`

The whole pinned boot touches this block a handful of times. Only the release bit has a known meaning; everything else is stored and read back.

Sources:

- trace (high): `RVF_TRACE_MMIO=7e00b000-7e101000 recon`: the writes right after `arm_loader: Starting ARM`, all from start4's MMIO write helper `0xFEC0043A`
- inferred (high): size: up to the mailboxes at `0x7E00B880`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CONTROL`](#control) | rw | 32 | 2, best high |
| `0x008` | [`REG_008`](#reg_008) | rw | 32 | 1, best high |
| `0x400` | [`TIMER_LOAD`](#timer_load) | rw | 32 | 1, best high |
| `0x404` | [`TIMER_VALUE`](#timer_value) | r | 32 | 1, best high |
| `0x408` | [`TIMER_CONTROL`](#timer_control) | rw | 32 | 1, best high |
| `0x40C` | [`TIMER_IRQCNTL`](#timer_irqcntl) | w | 32 | 1, best high |
| `0x410` | [`TIMER_RAWIRQ`](#timer_rawirq) | r | 32 | 1, best high |
| `0x414` | [`TIMER_MSKIRQ`](#timer_mskirq) | r | 32 | 1, best high |
| `0x418` | [`TIMER_RELOAD`](#timer_reload) | rw | 32 | 1, best high |
| `0x41C` | [`TIMER_PREDIV`](#timer_prediv) | rw | 32 | 3, best high |
| `0x420` | [`TIMER_FREECNT`](#timer_freecnt) | r | 32 | 1, best high |
| `0x440` | [`REG_440`](#reg_440) | rw | 32 | 2, best high |

## `CONTROL`

Offset `0x000` · access `rw` · 32 bits

Written `0x200` shortly before the ARM starts, and `0x1000` as the last access before it does, which clears `0x200` again.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 12 | `RELEASE` | rw | Let the ARM out of reset. The ARM starts at PC 0 in EL3. |

Sources:

- trace (high): `0x7E00B000` <- `0x00000200` (early), <- `0x00001000` (after `arm_loader`)
- trace (high): `--trace-mmio` through the ARM release: the `0x200` write comes after the UART handover and the PLLB bring-up, some 1300 accesses before the release — _the `recon` above calls the `0x200` write early; a trace that runs past the release puts it close to the release_

`RELEASE` sources:

- inferred (medium): it is the last write before the ARM runs, and nothing but PM housekeeping follows

## `REG_008`

Offset `0x008` · access `rw` · 32 bits

Written `0x3030` at the ARM release. Meaning unknown.

Sources:

- trace (high): `0x7E00B008` <- `0x00003030` after `arm_loader`

## `TIMER_LOAD`

Offset `0x400` · access `rw` · 32 bits

What the countdown reloads with, and what a write here puts straight into `TIMER_VALUE`.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

## `TIMER_VALUE`

Offset `0x404` · access `r` · 32 bits

The countdown as it stands. Reads 0 here: the model does not run this timer.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

## `TIMER_CONTROL`

Offset `0x408` · access `rw` · 32 bits

Which parts of the timer run, and how the two clocks are divided. Wider than a real SP804’s, which stops at bit 7.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3:2 | `DIV` | rw | The SP804 pre-scale: 0 divides the pre-divided clock by 1, 1 by 16, 2 by 256. |
| 5 | `IE` | rw | Timer interrupt enabled. Set out of reset. |
| 7 | `ENABLE` | rw | Run the countdown. |
| 8 | `DBGHALT` | rw | Stop the timers while the ARM is halted in debug. |
| 9 | `ENAFREE` | rw | Run the free-running counter. |
| 23:16 | `FREEDIV` | rw | Free-running counter pre-scaler: it counts at `sys_clock / (FREEDIV + 1)`. Reset `0x3E`. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

`DIV` sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

`IE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

`ENABLE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

`DBGHALT` sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

`ENAFREE` sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

`FREEDIV` sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

## `TIMER_IRQCNTL`

Offset `0x40C` · access `w` · 32 bits

Write 1 to bit 0 to acknowledge the timer interrupt.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

## `TIMER_RAWIRQ`

Offset `0x410` · access `r` · 32 bits

Bit 0: the countdown has expired, whether or not the interrupt is enabled.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

## `TIMER_MSKIRQ`

Offset `0x414` · access `r` · 32 bits

Bit 0: the interrupt line is up — `TIMER_RAWIRQ` and `TIMER_CONTROL.IE` together.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

## `TIMER_RELOAD`

Offset `0x418` · access `rw` · 32 bits

Like `TIMER_LOAD`, but it does not restart the countdown.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

## `TIMER_PREDIV`

Offset `0x41C` · access `rw` · 32 bits

The ARM timer’s pre-divider, 10 bits, reset `0x7D`: the timer counts at `apb_clock / (TIMER_PREDIV + 1)`. Not a register a real SP804 has. start4 writes `0x1F3` (a divisor of 500) before the release and `0xA` (11) at it, which is it setting the ARM timer’s rate for whatever runs next. The model does not run the timer, so the word is storage.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`, and §12.2 Table 200 (`PREDIV`, bits 9:0, reset `0x7D`)
- trace (high): `0x7E00B41C` <- `0x0000000A` after `arm_loader`
- trace (high): `0x7E00B41C` <- `0x000001F3` at `0x3EC53594`, before the release

## `TIMER_FREECNT`

Offset `0x420` · access `r` · 32 bits

The free-running counter `TIMER_CONTROL.ENAFREE` starts. Reads 0 here.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, §12.2 Table 192 (Timer Registers): the ARM-side SP804-like timer, based at `0x7E00B000` with its registers from `+0x400`

## `REG_440`

Offset `0x440` · access `rw` · 32 bits

start4 clears bit 9 (read-modify-write) just after the ARM release; the value read then is 0. Another routine sets bit 9 and waits for bit 31 of `+0x444`. Meaning unknown.

Sources:

- trace (high): `0x7E00B440` <- `0x00000000` at `0x3EC81F8A`, after the release
- decompile (high): start4 `0x3EC81F80`..`0x3EC81F8A` (`bitclear 9`); `0x3EC81F8E`..`0x3EC81F9E` (`bitset 9`, then spin until `+0x444` bit 31)
