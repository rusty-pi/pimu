<!-- generated from specs/corectl.toml by `cargo run -- spec-docs --update` – do not edit -->

# `corectl` – VPU core control: per-core boot handshake and interrupt controller

- Bus: `vpu` (VPU bus address)
- Base: `0x7E002000`
- Size: `0x1000`
- Banks: 2 × `0x800`; offsets below are for bank 0

One register bank per VPU core: core 0 at `+0x000`, core 1 at `+0x800`. start4 reaches its bank through a per-core pointer, so both cores run the same code.

Sources:

- decompile (high): per-core init `0x3EC3E938` sets `[blk+12] = 0x7E002000 + core * 0x800`
- trace (high): `--log irqen` and the peripheral stub show core 1 writing `0x7E002810..0x7E002844` — _the window was mapped `0x100` wide until commit 7bd21a3, which hid core 1's bank_
- inferred (medium): size: the system timer starts at `0x7E003000`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x004` | [`IRQ_PENDING`](#irq_pending) | r | 32 | 1, best medium |
| `0x010`–`0x02C` (8 × 0x4) | [`IRQ_PRIO`](#irq_prio) | rw | 32 | 6, best high |
| `0x030` | [`VBASE`](#vbase) | rw | 32 | 3, best high |
| `0x034` | [`WAKEUP`](#wakeup) | rw | 32 | 5, best high |
| `0x040`–`0x044` (2 × 0x4) | [`IRQ_PENDING_BITS`](#irq_pending_bits) | rw | 32 | 2, best high |

## `IRQ_PENDING`

Offset `0x004` · access `r` · 32 bits

Which interrupt the dispatcher should service next.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 8 | `VALID` | r | Set while an interrupt is pending. |
| 5:0 | `SOURCE` | r | Source number minus 64; the dispatcher ORs 64 back in. |

Sources:

- decompile (medium): dispatcher `0x3EC3E9BC`: `r0 = [blk+4]`, `btest r0, 8`, or 64, mask to 7 bits, index the handler table at `gp+58004`

`VALID` sources:

- decompile (high): dispatcher `0x3EC3E9BC`: `btest r0, 8`

`SOURCE` sources:

- decompile (medium): dispatcher `0x3EC3E9BC`: `or r0, 64`, then a 7-bit mask — _the dispatcher keeps 7 bits after the OR, so bit 6 may belong to the field too_

## `IRQ_PRIO`

Offset `0x010`, 8 elements 0x4 apart · access `rw` · 32 bits

One 4-bit enable/priority field per interrupt source, eight per word: source `src` (64 to 127) lives in word `(src >> 3) & 7` at bit `(src & 7) * 4`. Zero disables the source; a non-zero value enables it at that priority. The vector is the interrupt number, 64 + source, not this field. The 64 sources are the SoC's VC peripheral IRQs, offset by 64: source `64 + n` is VC peripheral IRQ `n`, which is also the number the ARM's device tree writes as `GIC_SPI n` (the GIC id is 32 above that again). So the names are: 64-67 `Timer 0`-`Timer 3` (the system timer's compare channels), 68-70 `H264 0`-`H264 2`, 71 `JPEG`, 72 `ISP`, 73 `USB`, 74 `V3D`, 75 `Transposer`, 76-79 `Multicore Sync 0`-`Multicore Sync 3`, 80-86 `DMA 0`-`DMA 6`, 87 `DMA 7 & 8`, 88 `DMA 9 & 10`, 89-92 `DMA 11`-`DMA 14`, 93 `AUX`, 94 `ARM`, 95 `DMA 15`, 96 `HDMI CEC`, 97 `HVS`, 98 `RPIVID`, 99 `SDC`, 100 `DSI 0`, 101 `Pixel Valve 2`, 102-103 `Camera 0`/`Camera 1`, 104-105 `HDMI 0`/`HDMI 1`, 106 `Pixel Valve 3`, 107 `SPI/BSC Slave`, 108 `DSI 1`, 109 `Pixel Valve 0`, 110 `Pixel Valve 1 & 4`, 111 `CPR`, 112 `SMI`, 113-116 `GPIO 0`-`GPIO 3`, 117 all I²C ORed, 118 all SPI ORed, 119 `PCM/I2S`, 120 `SDHOST`, 121 all PL011 UARTs ORed, 122 all ETH_PCIe L2 lines ORed, 123 `VEC`, 124 `CPG`, 125 `RNG`, 126 `EMMC & EMMC2`, 127 `ETH_PCIe secure`. An ORed source says only that one of the devices behind it has something pending; which one is read from `AUX_IRQ` for 93, from `PACTL_CS` (`0x7E204E00`, not modelled) for 117, 118 and 121, and from each device's own status register otherwise.

Sources:

- decompile (high): secure service `0xCEC006A6` (`r1` core, `r2` source, `r3` priority): `lsr r4, r2, 3; bmask r4, 3` picks the word from `0x7E002010 + core * 0x800`, then `bmask r2, 3` the field — _the non-secure `enable_irq_source(src, prio)` at `0x3ED72374` masks the word with `bmask r3, 2` instead; start4 only calls it for sources 64 and 78, which land in the same words either way_
- trace (high): `linux`, `RVF_TRACE_MMIO=0x7e002000-0x7e002060`: the secure service at `0xFEC006CA` writes all eight words, `+0x20 <- 0x10` (source 97, the HVS), `+0x28 <- 0x10000000` (119) and `+0x2c <- 0x100000` (125, the RNG) among them
- trace (high): start4 calls `enable_irq_source(64, 1)` for its ThreadX tick
- inferred (medium): hermanhermitage/videocoreiv, VideoCore IV Programmers Manual: 128 vector-table entries indexed by interrupt number, 0-31 exceptions, 32-63 swi, 64-127 external interrupts — _a reverse-engineered manual, not a datasheet_
- trace (high): vectoring at the field's value reached start4's exception stubs (dbe4e25, b9d53b8); vectoring at 64 + source reaches the per-source handlers (2bdbcbf)
- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102 (VC peripheral IRQs) for the 64 names, and §6.3 Figure 7 for where they land on the GIC (SPI ids 96 to 159) — _the datasheet numbers them 0 to 63; every source this model identified from the firmware sits 64 above its number there — 64 the ThreadX tick on `Timer 0`, 66 the clock service on `Timer 2`, 76 / 77 the mcsync doorbell acks on `Multicore Sync 0` / `1`, 78 / 79 the reschedule IPI on `Multicore Sync 2` / `3`, 89 and 95 DMA channels 11 and 15, 94 the ARM mailbox, 97 the HVS, 119 the `PCM/I2S` line Linux enables, 125 the RNG_

## `VBASE`

Offset `0x030` · access `rw` · 32 bits

Exception-vector base for this core. The core takes its vector from the base as it stands when the exception comes, so every write moves the table: the bootloader's halt points it at its own table and clears it again once woken, and start4, which runs after a wake without a reset in between, writes its own.

Sources:

- decompile (high): start4 entry trampoline: `mov r1, #0x7E002030`, then stores the vector base through it
- trace (high): core-control write trace: `+0x30` and `+0x830` both take `0xFEC01E00`, nothing writes `+0x38` — _replaced an earlier `+0x38` guess for core 1 (commit 06a8447)_
- decompile (high): bootsys halt `0x800005AC`: zeroes both cores' priority words and `+0x30`, sets vector 116 of a table at `0x80000000`, writes `0x80000000` here (`0x8000063A`) around its `sleep`, then 0 (`0x80000654`); after a wake the boot goes on to start4, which writes `0xFEC01E00`

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
