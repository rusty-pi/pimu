<!-- generated from specs/rng.toml by `cargo run -- spec-docs --update` – do not edit -->

# `rng` – Hardware RNG (RNG200): generator control, warm-up counter, interrupt and FIFO

- Bus: `vpu` (VPU bus address)
- Base: `0x7E104000`
- Size: `0x28`
- Interrupts: VPU source 125

Output comes from a fixed-seed xorshift so transcripts stay byte-identical. A running generator always has words waiting.

Sources:

- linux (high): `rng@7e104000`, `brcm,bcm2711-rng200`, `reg = <0x7e104000 0x28>`; `drivers/char/hw_random/iproc-rng200.c` (`rpi-6.12.y` at aa731bab)
- decompile (high): start4's RNG accesses, all in `0x3ED64A4E..0x3ED64E28`; bootloader init `0x8000378E`

Interrupts (VPU source 125):

`INT_STATUS` drives the line while an enabled condition is latched.

- decompile (high): start4's RNG interrupt handler `0x3ED64BE8` reads `INT_STATUS`; its table entry is source 125
- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102: VC peripheral IRQ 61 is `RNG`, source 125

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CTRL`](#ctrl) | rw | 32 | 1, best high |
| `0x004` | [`RNG_SOFT_RESET`](#rng_soft_reset) | rw | 32 | 2, best high |
| `0x008` | [`RBG_SOFT_RESET`](#rbg_soft_reset) | rw | 32 | 1, best high |
| `0x00C` | [`TOTAL_BIT_COUNT`](#total_bit_count) | r | 32 | 1, best high |
| `0x010` | [`TOTAL_BIT_COUNT_THRESHOLD`](#total_bit_count_threshold) | rw | 32 | 2, best high |
| `0x014` | [`PROBE`](#probe) | r | 32 | 1, best high |
| `0x018` | [`INT_STATUS`](#int_status) | w1c | 32 | 3, best high |
| `0x01C` | [`INT_ENABLE`](#int_enable) | rw | 32 | 2, best high |
| `0x020` | [`FIFO_DATA`](#fifo_data) | r | 32 | 3, best high |
| `0x024` | [`FIFO_COUNT`](#fifo_count) | rw | 32 | 1, best high |

## `CTRL`

Offset `0x000` · access `rw` · 32 bits

Generator control. Linux and start4 leave a block with `RBGEN` already set alone.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 12:0 | `RBGEN` | rw | Any bit set runs the generator. |
| 14:13 | `DIV` | rw | Sample-rate divider. |

Sources:

- linux (high): `iproc-rng200.c`: `RNG_CTRL = (3 << 13) | 0x1FFF`

`RBGEN` sources:

- linux (high): `iproc-rng200.c`: `RNG_CTRL_RNG_RBGEN_MASK = 0x1FFF`

`DIV` sources:

- linux (medium): `iproc-rng200.c` writes `3 << 13`

## `RNG_SOFT_RESET`

Offset `0x004` · access `rw` · 32 bits

Bit 0 holds the generator in reset.

Sources:

- linux (high): `iproc-rng200.c`: `RNG_SOFT_RESET`
- decompile (high): bootloader `0x8000378E` pulses it 1 then 0

## `RBG_SOFT_RESET`

Offset `0x008` · access `rw` · 32 bits

Bit 0 holds the bit generator in reset.

Sources:

- linux (high): `iproc-rng200.c`: `RBG_SOFT_RESET`

## `TOTAL_BIT_COUNT`

Offset `0x00C` · access `r` · 32 bits

Bits generated since reset. Linux spins until it passes 16.

Sources:

- linux (high): `bcm2711_rng200_read` spins on `RNG_TOTAL_BIT_COUNT > 16`

## `TOTAL_BIT_COUNT_THRESHOLD`

Offset `0x010` · access `rw` · 32 bits

Warm-up bits to discard.

Sources:

- linux (high): `bcm2711_rng200_init` writes `0x40000`
- decompile (high): start4 open `0x3ED64CDC` writes `0x40000`

## `PROBE`

Offset `0x014` · access `r` · 32 bits · reset `0x40000`

Not in the kernel's list. start4 picks its RNG200 driver when bit 18 reads set, and the legacy BCM2835 driver (whose `STATUS` / `DATA` sit where the soft resets are) otherwise.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 18 | `RNG200` | r | This is an RNG200. |

Sources:

- decompile (high): probe `0x3ED64B0A` returns the driver table at `0x3EDFC0BC` on bit 18, `0x3EDFC0D4` without; start4db `FUN_0ee13458` makes the same choice

`RNG200` sources:

- decompile (high): probe `0x3ED64B0A` tests bit 18

## `INT_STATUS`

Offset `0x018` · access `w1c` · 32 bits

Interrupt status (source 125). Each bit latches when its condition becomes true, and a write-one clear sticks until the condition goes false and true again. `0x80000022` are the failure bits, which the model never raises.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `TOTAL_BITS` | w1c | Warm-up threshold reached. |
| 2 | `FIFO_FULL` | w1c | The FIFO holds `FIFO_COUNT.THRESHOLD` words. |
| 5 | `NIST_FAIL` | w1c | Health test failed. |
| 31 | `MASTER_FAIL_LOCKOUT` | w1c | Generator locked out. |

Sources:

- linux (high): `iproc-rng200.c`: `RNG_INT_STATUS`
- decompile (medium): start4 1.20210303: open arms `INT_ENABLE = 0x80000026` with `FIFO_COUNT = 0x200`, and the handler `0x0ED565C6` only acks bit 2 (`|= 4`), leaving it enabled with the FIFO still full — _that build boots on hardware, so the bits are latched events, not levels (#73)_
- decompile (high): irq `0x3ED64BE8` reads it, acks with `|= 4` or `|= 0x80000022`

`TOTAL_BITS` sources:

- linux (high): `iproc-rng200.c`: `RNG_INT_STATUS_TOTAL_BITS_COUNT_IRQ_MASK`

`FIFO_FULL` sources:

- decompile (high): irq `0x3ED64BE8`: on bit 2, re-arms and releases the reader waiting on `gp+879560`

`NIST_FAIL` sources:

- linux (high): `iproc-rng200.c`: `RNG_INT_STATUS_NIST_FAIL_IRQ_MASK`

`MASTER_FAIL_LOCKOUT` sources:

- linux (high): `iproc-rng200.c`: `RNG_INT_STATUS_MASTER_FAIL_LOCKOUT_IRQ_MASK`

## `INT_ENABLE`

Offset `0x01C` · access `rw` · 32 bits

Interrupt enables, same layout as `INT_STATUS`. start4 writes `0` just before it releases the ARM.

Sources:

- decompile (high): start4 open writes `0x80000022`, read writes `0x80000026` before it blocks
- trace (high): pinned start4: `0x00000000` at `0x3ED64AD4`, after `arm_loader: Starting ARM` and before the release

## `FIFO_DATA`

Offset `0x020` · access `r` · 32 bits

Pops one word. start4 reads `FIFO_COUNT` before each word; the device tree it hands Linux takes `kaslr-seed` from the first two words and `rng-seed` from the next sixteen, each word little-endian.

Sources:

- linux (high): `iproc-rng200.c`: `RNG_FIFO_DATA`
- decompile (high): start4 read `0x3ED64DD0` returns `[+0x20]`
- trace (high): pinned start4: `FIFO_COUNT` at `0x3ED64DE4` then `FIFO_DATA` at `0x3ED64E0A`, 2 words and then 16; the handed-over `/chosen/kaslr-seed` is the first two words' bytes and `/chosen/rng-seed` the other sixteen's

## `FIFO_COUNT`

Offset `0x024` · access `rw` · 32 bits

Words ready, and the FIFO-full interrupt level.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 7:0 | `COUNT` | r | Words ready. |
| 15:8 | `THRESHOLD` | rw | FIFO-full interrupt level; 0 never fires. |

Sources:

- linux (high): `iproc-rng200.c`: `RNG_FIFO_COUNT`

`COUNT` sources:

- linux (high): `iproc-rng200.c`: `RNG_FIFO_COUNT_RNG_FIFO_COUNT_MASK`

`THRESHOLD` sources:

- linux (high): `bcm2711_rng200_init` writes `2 << 8`
