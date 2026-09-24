<!-- generated from specs/corectl.toml by `cargo run -- spec-docs --update` – do not edit -->

# `corectl` – VPU core control: per-core boot handshake and interrupt controller

- Bus: `vpu` (VPU bus address)
- Base: `0x7E002000`
- Size: `0x1000`
- Banks: 2 × `0x800`; offsets below are for bank 0

One register bank per VPU core: core 0 at `+0x000`, core 1 at `+0x800`. start4 reaches its bank through a per-core pointer, so both cores run the same code. Each bank holds registers up to `+0x44`; `+0x3C` and everything from `+0x48` to the end of the measured window answers the block's own tag, `0x494E5445` — `"INTE"` big-endian — the way the GPIO block answers `0x6770696F` (`specs/gpio.toml`).

Sources:

- decompile (high): per-core init `0x3EC3E938` sets `[blk+12] = 0x7E002000 + core * 0x800`
- trace (high): `--log irqen` and the peripheral stub show core 1 writing `0x7E002810..0x7E002844` — _the window was mapped `0x100` wide until commit 7bd21a3, which hid core 1's bank_
- measured (high): Raspberry Pi 4B d03115 (`rpi-unboxed` start4 `2431cea8`, Linux idle), `/dev/mem`: `0xFE00203C` and every word from `0xFE002048` to `0xFE0020FF` read `0x494E5445`, and so do the same offsets in core 1's bank from `0xFE002800`. Reads one word at a time — back-to-back 32-bit reads of this window through one `mmap` alias, and the second read answers the first offset's value.
- inferred (medium): size: the system timer starts at `0x7E003000`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`IRQ_GATE`](#irq_gate) | rw | 32 | 3, best high |
| `0x004` | [`IRQ_PENDING`](#irq_pending) | r | 32 | 4, best high |
| `0x008`–`0x00C` (2 × 0x4) | [`IRQ_RAW`](#irq_raw) | r | 32 | 2, best high |
| `0x010`–`0x02C` (8 × 0x4) | [`IRQ_PRIO`](#irq_prio) | rw | 32 | 7, best high |
| `0x030` | [`VBASE`](#vbase) | w | 32 | 4, best high |
| `0x034` | [`WAKEUP`](#wakeup) | rw | 32 | 5, best high |
| `0x040`–`0x044` (2 × 0x4) | [`IRQ_PENDING_BITS`](#irq_pending_bits) | rw | 32 | 2, best high |
| `0x048`–`0x04C` (2 × 0x4) | [`IRQ_PENDING_BITS_SET`](#irq_pending_bits_set) | w | 32 | 3, best high |
| `0x050`–`0x054` (2 × 0x4) | [`IRQ_PENDING_BITS_CLR`](#irq_pending_bits_clr) | w | 32 | 2, best high |

## `IRQ_GATE`

Offset `0x000` · access `rw` · 32 bits

Core-wide delivery gate, four bits wide. Zero delivers; a value at or above a source's priority holds it pending until the gate drops again. Zero on a running board, so nothing the model boots ever raises it.

Sources:

- measured (high): Raspberry Pi 4B d03115 (`rpi-unboxed` start4 `2431cea8`, Linux idle), `/dev/mem` at `0xFE002000`: reads `0x0`, takes `0xf`, reads it back. With `0xf` written, a source forced through `IRQ_PENDING_BITS_SET` and enabled at priority 1 in `IRQ_PRIO` stayed undelivered for as long as the gate was up — the firmware's own handler for it did not run — and was delivered the moment the gate went back to `0x0`. Priority 7 was held the same way.
- measured (high): Raspberry Pi 4B d03115, `/dev/mem`: core 1's copy at `0xFE002800` takes `0xf` and reads it back while `0xFE002000` stays `0x0`, so the gate is per core.
- datasheet (medium): Broadcom `bcm2708_chip/intctrl0.h`: `IC0_C`, RW, mask `0x0000000f` — _the header gives no fields, so whether the four bits are a priority threshold or a plain block is ours, from the two priorities measured_

## `IRQ_PENDING`

Offset `0x004` · access `r` · 32 bits

Which interrupt is being taken: its number and the priority it was enabled at. A delivery latch, not a pending register -- it holds what has actually been vectored, and reads 0 for a source that is merely queued or held by `IRQ_GATE`. The value appears twice, once in each half-word; both halves were identical in every measurement, including two sources enabled at once. There is no valid bit: a read outside a handler answers 0, and the dispatcher's own test of bit 8 is a test of the priority field's low bit, which works only because start4 enables everything at priority 1.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 6:0 | `SOURCE` | r | The interrupt number, 64 + source, as the vector table indexes it -- already ORed with 64, so the dispatcher's `or r0, 64` is a no-op on this silicon. Bit 6 cannot be told apart from a hardwired part of the field by measurement: the 64 sources are numbered 64 to 127, so bit 6 is set for all of them. |
| 10:8 | `PRIO` | r | The priority the source's `IRQ_PRIO` field was enabled at, carried through to the handler. |

Sources:

- measured (high): Raspberry Pi 4B d03115, read from inside a handler: `rpi-unboxed` built with four instructions at `vpu_stray_irq` entry that stash `0x7E002004` in a static, then a source forced through `IRQ_PENDING_BITS_SET` from the ARM. Source 71 at priority 1 gave `0x01470147`, at priority 7 `0x07470747`, source 70 at priority 1 `0x01460146`, source 96 at priority 1 `0x01600160`. Priorities 2, 4 and 6 gave `0x0247`, `0x0447` and `0x0647`, so bit 8 is the priority's low bit and not a flag.
- measured (high): Raspberry Pi 4B d03115, same build: with source 70 at priority 2 and source 71 at priority 5 enabled and both forced, the register read `0x0547` -- source 71. With the priorities swapped it read `0x0546` -- source 70. So the higher priority number wins, which is the same sense as `IRQ_GATE`, where a gate of `0xf` holds every priority.
- decompile (medium): dispatcher `0x3EC3E9BC`: `r0 = [blk+4]`, `btest r0, 8`, or 64, mask to 7 bits, index the handler table at `gp+58004`
- datasheet (medium): Broadcom `bcm2708_chip/intctrl0.h`: `IC0_S`, RO, mask `0x073f073f` -- the two half-words, and a 6-bit source field where the BCM2711 reads 7

`SOURCE` sources:

- measured (high): internal source 32 (`IRQ_PENDING_BITS` word 1 bit 0) read back as `0x60` = 96 = 64 + 32, and internal sources 6 and 7 as `0x46` and `0x47`

`PRIO` sources:

- measured (high): priorities 1 through 7 on one source each read back as `0x01..` through `0x07..` in these bits

## `IRQ_RAW`

Offset `0x008`, 2 elements 0x4 apart · access `r` · 32 bits

The raw source lines, one bit per source, word 0 for sources 64..95 and word 1 for 96..127. A bit is up while the device holds its line up, whether or not `IRQ_PRIO` enables the source. Disjoint from `IRQ_PENDING_BITS`: a source posted in software shows up there and never here. Not modelled — a read answers 0 where a real board answers whichever devices are asserting.

Sources:

- measured (high): Raspberry Pi 4B d03115 (`rpi-unboxed` start4 `2431cea8`, Linux idle), `/dev/mem`: `0xFE002008` reads `0xa` steadily over 8 samples 3 ms apart — sources 65 and 67, two system-timer compares — while `IRQ_PRIO` enables neither and `IRQ_PENDING` reads 0. `0xFE00200C` reads 0. Both banks read the same value, so the lines are the SoC's and only the enables are per core. Setting `IRQ_PENDING_BITS` bit 7 left it at `0xa`.
- datasheet (medium): Broadcom `bcm2708_chip/intctrl0.h`: `IC0_SRC0` and `IC0_SRC1`, both RO

## `IRQ_PRIO`

Offset `0x010`, 8 elements 0x4 apart · access `rw` · 32 bits

One 4-bit enable/priority field per interrupt source, eight per word: source `src` (64 to 127) lives in word `(src >> 3) & 7` at bit `(src & 7) * 4`. Zero disables the source; a non-zero value enables it at that priority. The vector is the interrupt number, 64 + source, not this field. The 64 sources are the SoC's VC peripheral IRQs, offset by 64: source `64 + n` is VC peripheral IRQ `n`, which is also the number the ARM's device tree writes as `GIC_SPI n` (the GIC id is 32 above that again). So the names are: 64-67 `Timer 0`-`Timer 3` (the system timer's compare channels), 68-70 `H264 0`-`H264 2`, 71 `JPEG`, 72 `ISP`, 73 `USB`, 74 `V3D`, 75 `Transposer`, 76-79 `Multicore Sync 0`-`Multicore Sync 3`, 80-86 `DMA 0`-`DMA 6`, 87 `DMA 7 & 8`, 88 `DMA 9 & 10`, 89-92 `DMA 11`-`DMA 14`, 93 `AUX`, 94 `ARM`, 95 `DMA 15`, 96 `HDMI CEC`, 97 `HVS`, 98 `RPIVID`, 99 `SDC`, 100 `DSI 0`, 101 `Pixel Valve 2`, 102-103 `Camera 0`/`Camera 1`, 104-105 `HDMI 0`/`HDMI 1`, 106 `Pixel Valve 3`, 107 `SPI/BSC Slave`, 108 `DSI 1`, 109 `Pixel Valve 0`, 110 `Pixel Valve 1 & 4`, 111 `CPR`, 112 `SMI`, 113-116 `GPIO 0`-`GPIO 3`, 117 all I²C ORed, 118 all SPI ORed, 119 `PCM/I2S`, 120 `SDHOST`, 121 all PL011 UARTs ORed, 122 all ETH_PCIe L2 lines ORed, 123 `VEC`, 124 `CPG`, 125 `RNG`, 126 `EMMC & EMMC2`, 127 `ETH_PCIe secure`. An ORed source says only that one of the devices behind it has something pending; which one is read from `AUX_IRQ` for 93, from `PACTL_CS` (`0x7E204E00`, not modelled) for 117, 118 and 121, and from each device's own status register otherwise.

Sources:

- measured (high): Raspberry Pi 4B d03115 (`rpi-unboxed` start4 `2431cea8`, Linux idle), `/dev/mem`: word 0 at `0xFE002010` reads `0x00000101` and word 3 at `0xFE00201C` reads `0x01000000` — what that firmware programs for the system timer's first compare (source 64, word 0 field 0) and the ARM's mailbox (source 94, word 3 field 6); words 1, 2 and 4 to 7 read 0. Core 1's eight words from `0xFE002810` all read 0, so the second bank is a bank and not an alias. Writing field 7 of word 0 and forcing source 71 vectored it, which pins the addressing.
- decompile (high): secure service `0xCEC006A6` (`r1` core, `r2` source, `r3` priority): `lsr r4, r2, 3; bmask r4, 3` picks the word from `0x7E002010 + core * 0x800`, then `bmask r2, 3` the field — _the non-secure `enable_irq_source(src, prio)` at `0x3ED72374` masks the word with `bmask r3, 2` instead; start4 only calls it for sources 64 and 78, which land in the same words either way_
- trace (high): `linux`, `RVF_TRACE_MMIO=0x7e002000-0x7e002060`: the secure service at `0xFEC006CA` writes all eight words, `+0x20 <- 0x10` (source 97, the HVS), `+0x28 <- 0x10000000` (119) and `+0x2c <- 0x100000` (125, the RNG) among them
- trace (high): start4 calls `enable_irq_source(64, 1)` for its ThreadX tick
- inferred (medium): hermanhermitage/videocoreiv, VideoCore IV Programmers Manual: 128 vector-table entries indexed by interrupt number, 0-31 exceptions, 32-63 swi, 64-127 external interrupts — _a reverse-engineered manual, not a datasheet_
- trace (high): vectoring at the field's value reached start4's exception stubs (dbe4e25, b9d53b8); vectoring at 64 + source reaches the per-source handlers (2bdbcbf)
- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102 (VC peripheral IRQs) for the 64 names, and §6.3 Figure 7 for where they land on the GIC (SPI ids 96 to 159) — _the datasheet numbers them 0 to 63; every source this model identified from the firmware sits 64 above its number there — 64 the ThreadX tick on `Timer 0`, 66 the clock service on `Timer 2`, 76 / 77 the mcsync doorbell acks on `Multicore Sync 0` / `1`, 78 / 79 the reschedule IPI on `Multicore Sync 2` / `3`, 89 and 95 DMA channels 11 and 15, 94 the ARM mailbox, 97 the HVS, 119 the `PCM/I2S` line Linux enables, 125 the RNG_

## `VBASE`

Offset `0x030` · access `w` · 32 bits

Exception-vector base for this core. The core takes its vector from the base as it stands when the exception comes, so every write moves the table: the bootloader's halt points it at its own table and clears it again once woken, and start4, which runs after a wake without a reset in between, writes its own. Only bits 31:9 are kept, so the table has to be 512-byte aligned -- 128 entries of four bytes, the whole table -- and a misaligned one is fetched from the address below it with no indication that anything is wrong. Write-only: a real board reads 0 here whatever the live vector base is.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 31:9 | `ADDR` | w | The table's address. The low nine bits read back as zero however they are written. |

Sources:

- measured (high): Raspberry Pi 4B d03115 (`rpi-unboxed` start4 `2431cea8`, Linux idle), `/dev/mem`: `0xFE002030` reads `0x0` on both banks, yet a source forced through `IRQ_PENDING_BITS_SET` and enabled in `IRQ_PRIO` vectored into the live firmware's handler, which rewrote `IRQ_PRIO` word 0 back to its own enables. So the zero is the register not reading back, not a cleared vector base. Core 1's copy at `0xFE002830` behaves the same: written `0x3EC2A000`, reads `0x0`.
- decompile (high): start4 entry trampoline: `mov r1, #0x7E002030`, then stores the vector base through it
- trace (high): core-control write trace: `+0x30` and `+0x830` both take `0xFEC01E00`, nothing writes `+0x38` — _replaced an earlier `+0x38` guess for core 1 (commit 06a8447)_
- decompile (high): bootsys halt `0x800005AC`: zeroes both cores' priority words and `+0x30`, sets vector 116 of a table at `0x80000000`, writes `0x80000000` here (`0x8000063A`) around its `sleep`, then 0 (`0x80000654`); after a wake the boot goes on to start4, which writes `0xFEC01E00`

`ADDR` sources:

- datasheet (high): Broadcom `bcm2708_chip/intctrl0.h`: `IC0_VADDR` with `IC0_VADDR_MASK 0xfffffe00`
- measured (high): 4B rev 1.5: `rpi-unboxed` with its table at 0xFEC2B7C0 took no interrupt at all -- the compare fired, the source asserted, the routing word matched stock's and SR bit 30 was set -- and took them normally once the table was moved to 0xFEC2A000

## `WAKEUP`

Offset `0x034` · access `rw` · 32 bits

Start address. Core 1 sleeps until its copy (`0x7E002834`) is written, then starts there. start4 writes it once, with its own entry point, when its power manager first powers domain `0x20000`.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 31:1 | `ADDR` | rw | Where the core starts. Bit 0 is not stored. |

Sources:

- decompile (high): power-domain switch `0x3ED55412`, case `0x20000`: clears core 1's check-in slot, calls interrupt-controller op `+0x1c` (`0x3ED01B8A`, secure service 12 at `0xCEC006F6`: `st r1 -> 0x7E002834`) with `entry`, then spins until core 1 checks in — _`entry` comes from a pc-relative lea, so it follows start4 wherever the bootloader loaded it_
- trace (high): `linux`: `vcos_threadx.c`'s sysman user asks for bit 5 after `Booting Linux`, which powers domain `0x20000` and writes the wake once; nothing writes it before `arm_loader`
- datasheet (medium): Broadcom `bcm2708_chip/intctrl1.h` (in the published `brcm_usrlib` headers): `IC1_WAKEUP` at `0x7e002834`, RW, mask `0xfffffffe`, reset `0x10000000`; `IC0_WAKEUP` at `0x7e002034` — _a BCM2708 header, but the masks and `VADDR` around it match what start4 uses on the BCM2711_
- inferred (medium): librerpi/lk-overlay `arch/vpu/arch.c` (1c942f5) starts the second VPU core with `*REG32(IC1_WAKEUP) = &core2_start` and nothing else — _open firmware that runs on the board: a working example rather than a guess_
- measured (medium): Raspberry Pi 4B d03115 (C0) after a Linux boot, `busybox devmem 0xFE002814`: `0x10000000`, the IPI enable that only core 1 writes (the model leaves it 0 with core 1 held) — _so core 1 does run on the board; `VADDR` and `WAKEUP` themselves read 0 from the ARM even where start4 has written them_

`ADDR` sources:

- datasheet (medium): Broadcom `bcm2708_chip/intctrl1.h`: `IC1_WAKEUP_MASK` `0xfffffffe`

## `IRQ_PENDING_BITS`

Offset `0x040`, 2 elements 0x4 apart · access `rw` · 32 bits

Pending bitmask, one bit per source: word 0 covers sources 64..95, word 1 sources 96..127. Setting a bit raises that interrupt on this core in software; each ISR clears its bit on entry.

Sources:

- decompile (high): `0x3ED01896` raises (`|= 1 << bit`), `0x3ED01792` acknowledges (`&= ~(1 << bit)`), `0x3ED01980` reads one bit
- trace (high): the clock service re-posts its own source 66 this way; sources 78 / 79 are the inter-core reschedule IPI

## `IRQ_PENDING_BITS_SET`

Offset `0x048`, 2 elements 0x4 apart · access `w` · 32 bits · reset `0x494E5445`

Write-only alias of `IRQ_PENDING_BITS`: the bits written are raised, the zeroes left alone. A read answers the block tag.

Sources:

- measured (high): Raspberry Pi 4B d03115 (`rpi-unboxed` start4 `2431cea8`), `/dev/mem`: `0xFE002040` read `0x0`, `0x80` written to `0xFE002048`, `0xFE002040` then read `0x80`
- measured (high): Raspberry Pi 4B d03115, `/dev/mem`: core 1's alias at `0xFE002848` raises a bit in `0xFE002840` and leaves core 0's `0xFE002040` at `0x0`, and `0xFE002844` takes a write of its own.
- datasheet (medium): Broadcom `bcm2708_chip/intctrl0.h`: `IC0_FORCE0_SET` and `IC0_FORCE1_SET`

## `IRQ_PENDING_BITS_CLR`

Offset `0x050`, 2 elements 0x4 apart · access `w` · 32 bits · reset `0x494E5445`

Write-only alias of `IRQ_PENDING_BITS`: the bits written are cleared. A read answers the block tag.

Sources:

- measured (high): Raspberry Pi 4B d03115 (`rpi-unboxed` start4 `2431cea8`), `/dev/mem`: with `0xFE002040` reading `0x80`, `0x80` written to `0xFE002050` left it reading `0x0`
- datasheet (medium): Broadcom `bcm2708_chip/intctrl0.h`: `IC0_FORCE0_CLR` and `IC0_FORCE1_CLR`
