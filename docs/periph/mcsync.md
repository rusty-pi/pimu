<!-- generated from specs/mcsync.toml by `cargo run -- spec-docs --update` – do not edit -->

# `mcsync` – Thirty-two hardware semaphores, plus the mailbox words and ack registers around them

- Bus: `vpu` (VPU bus address)
- Base: `0x7E000000`
- Size: `0x1000`
- Interrupts: `ACK76` VPU source 76 · `ACK77` VPU source 77

Sources:

- measured (high): Raspberry Pi 4B d03115, `/dev/mem`: the block answers its own tag `0x4D554C54` -- `"MULT"` big-endian -- at `0xFE00008C` and at every word from `0xFE0000CC` to `0xFE000FFC`, so the registers end at `+0xC8` and the tag names the block the way `corectl` answers `INTE` and the GPIO block `gpio`. Every register from `+0x80` to `+0xC8` is decoded, checked by reading each after three different preceding values.
- decompile (high): `0x3ED3A114` / `0x3ED3A00C` address the slot array at `0x7E000000`
- inferred (medium): size: the SDC block starts at `0x7E001000`

Interrupts (`ACK76` VPU source 76 · `ACK77` VPU source 77):

Each line is acked through the word named after it. Neither core enables them on the pinned boot, so the doorbells are polled rather than taken.

- decompile (high): ISR `0x3ED3A098` (handler table `gp+58004`) does `[ACK76] &= ~[PENDING]`, and `ACK77` for 77
- trace (high): `--log irqen`: neither core calls `enable_irq_source` for 76 or 77
- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102: VC peripheral IRQs 12 and 13 are `Multicore Sync 0` and `Multicore Sync 1`, sources 76 and 77 — _IRQs 14 and 15 are `Multicore Sync 2` and `Multicore Sync 3`, the sources 78 and 79 start4 posts in software as its reschedule IPI; they are not acked through this block_

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000`–`0x07C` (32 × 0x4) | [`SEMA`](#sema) | rw | 32 | 4, best high |
| `0x080` | [`STATUS`](#status) | r | 32 | 3, best high |
| `0x084` | [`ACK76`](#ack76) | rw | 32 | 2, best medium |
| `0x088` | [`ACK77`](#ack77) | rw | 32 | 2, best medium |
| `0x0B4` | [`MBOX5`](#mbox5) | rw | 32 | 2, best high |
| `0x0BC` | [`MBOX7`](#mbox7) | rw | 32 | 2, best high |
| `0x0C0` | [`VPUSEMA0`](#vpusema0) | rw | 32 | 4, best high |
| `0x090`–`0x094` (2 × 0x4) | [`ICSET`](#icset) | rw | 32 | 2, best high |
| `0x098`–`0x09C` (2 × 0x4) | [`ICCLR`](#icclr) | rw | 32 | 1, best medium |
| `0x0A0`–`0x0B0` (5 × 0x4) | [`MBOX`](#mbox) | rw | 32 | 2, best high |
| `0x0B8` | [`MBOX6`](#mbox6) | rw | 32 | 1, best medium |
| `0x0C4` | [`VPUSEMA1`](#vpusema1) | rw | 32 | 2, best high |
| `0x0C8` | [`VPU_STAT`](#vpu_stat) | r | 32 | 2, best high |

## `SEMA`

Offset `0x000`, 32 elements 0x4 apart · access `rw` · 32 bits

Thirty-two hardware test-and-set semaphores, one per word. A read answers 0 if the semaphore was free **and takes it**, or 1 if it was already held; any write releases it, whatever value is written. So a read is not a load: `0x3ED3A00C`'s spin **while** the word is non-zero is an acquire loop that ends the moment the semaphore is free, and `0x3ED3A114`'s write of 1 is a release, not a post. Which ones are held reads out of `STATUS`.

Sources:

- measured (high): Raspberry Pi 4B d03115 running a firmware that never touches this block, `/dev/mem` at `0xFE000000`: with every slot released, six reads of slot 5 answer `0 1 1 1 1 1` and `STATUS` then reads `0x00000020`. Reading slots 0, 7 and 31 makes `STATUS` exactly `0x80000081`; writing slot 7 leaves `0x80000001`. A write of 0 and a write of 1 both release, and `0xffffffff` written reads back 0, so the slot is one bit. All 32 slots behave alike.
- datasheet (medium): Broadcom `bcm2708_chip/multicore_sync.h`: `MS_SEMA_0`..`MS_SEMA_31` at `0x7E000000`..`0x7E00007C`, RW, one bit each
- decompile (high): `0x3ED3A114` (write, i.e. release), `0x3ED3A00C` (spin while non-zero, i.e. acquire)
- trace (high): `recon` run: clock-service manager posts slot 6, then suspends

## `STATUS`

Offset `0x080` · access `r` · 32 bits

Which of the 32 `SEMA` semaphores are currently held, one bit each. The firmware treats a held semaphore as a message waiting, so its ISR clears exactly these bits from its ack word -- but the register is the hardware's held-bitmap, not a separate pending set.

Sources:

- measured (high): Raspberry Pi 4B d03115: 0 with every semaphore released; exactly `0x80000081` with slots 0, 7 and 31 taken, and `0x80000001` after slot 7 is released. Under stock start4 `f5e89631` with Linux idle it reads 0.
- datasheet (medium): Broadcom `bcm2708_chip/multicore_sync.h`: `MS_STATUS` at `0x7E000080`, RO
- decompile (medium): ISR `0x3ED3A098`

## `ACK76`

Offset `0x084` · access `rw` · 32 bits

Acknowledge word for VPU interrupt source 76. The ISR does `[+0x84] &= ~[+0x80]`.

Sources:

- decompile (medium): ISR `0x3ED3A098` (handler table `gp+58004`)
- trace (medium): `--log irqen`: neither core calls `enable_irq_source` for 76 or 77 — _so the interrupt path itself is not exercised by the boot so far_

## `ACK77`

Offset `0x088` · access `rw` · 32 bits

Acknowledge word for VPU interrupt source 77. The ISR does `[+0x88] &= ~[+0x80]`.

Sources:

- decompile (medium): ISR `0x3ED3A098` (handler table `gp+58004`)
- trace (medium): `--log irqen`: neither core calls `enable_irq_source` for 76 or 77 — _so the interrupt path itself is not exercised by the boot so far_

## `MBOX5`

Offset `0x0B4` · access `rw` · 32 bits

Mailbox word 5, which the model used to call `SECURE_MARK`. Both bootloader stages write 0 here as they start, OR bit 31 of it into their secure-boot flags each time they work them out, and set bit 31 once any flag is set (`SIGNED_BOOT`, or the OTP secure-boot rows), so a later look within the stage sees secure boot even if the settings no longer say so. Reads 0 in the model, so there every look starts afresh.

Sources:

- decompile (high): EEPROM bootmain: `0xA7B88` (clear at start), `0xA92C6` (read into the flags), `0xA934A` / `0xA9352` (set bit 31 when the flags are non-zero)
- trace (high): `--trace-mmio`, `--bootconf SIGNED_BOOT=1`: bootsys writes 0 at `0x80007ED0` and reads / sets it at `0x8000902C`, `0x800090A4` / `0x800090A8`; bootmain clears it, then every flags check reads it twice and writes `0x80000000`

## `MBOX7`

Offset `0x0BC` · access `rw` · 32 bits

Mailbox word 7, which the model used to call `REG_0BC`. **The boot ROM records here how it booted**, and the bits 16-23 both bootloader stages keep are that mode: `0x0A060000` for the SPI EEPROM, which is the `BOOTMODE: 0x06` of their banners. The model kept nothing in this word until it was given storage, so every stage read mode 0 where a board reads 6. The ROM clears `MBOX6` (`0xB8`) immediately afterwards.

Sources:

- trace (high): `--trace-mmio`: bootsys `0x80007EBE`, bootmain `0xA7B70`, each followed by a read of the system timer and the write to `SECURE_MARK`
- trace (high): boot ROM (BCM2711C0 dump, `--maskrom`): `0x0A060000` at `0x60000DA4`, then `MBOX6` `0x00000000` at `0x60000DAC`; nothing else in the run writes either word

## `VPUSEMA0`

Offset `0x0C0` · access `rw` · 32 bits

The first of the two semaphores that sit apart from the 32, which the model used to call `LOCK`. start4 reads this word and writes its own address back into it from dozens of places, tens of thousands of times a boot -- read to claim, write to let go, which is what the name says it is. Measured, though, it does not behave as the 32 do: every read answers the same value rather than 0 once and 1 after. It reads 0 in the model.

Sources:

- trace (high): pinned start4: reads at `0x3EC3C6F6`, `0x3EC3E1CC` and others, each followed by `0x7E0000C0` <- `0x7E0000C0` (`0x3EC3C756`, `0x3EC3E23A`, ...); the busiest pair of sites is `0x3ED651D8` / `0x3ED65216`
- measured (high): Raspberry Pi 4B d03115: reads 0 on three consecutive reads under stock start4 `f5e89631` and under a from-scratch firmware, so unlike `SEMA` a read neither takes it nor changes it
- datasheet (medium): Broadcom `bcm2708_chip/multicore_sync.h`: `MS_VPUSEMA_0`, 1 bit
- inferred (medium): read, then write your own address, is a claim protocol -- and the block's own name for the register agrees

## `ICSET`

Offset `0x090`, 2 elements 0x4 apart · access `rw` · 32 bits

One bit each. Not modelled; reads 0 on both boards measured.

Sources:

- datasheet (medium): Broadcom `bcm2708_chip/multicore_sync.h`: `MS_ICSET_0` and `MS_ICSET_1`, 1 bit
- measured (high): Raspberry Pi 4B d03115: 0 under stock start4 `f5e89631` and under a firmware that never touches the block

## `ICCLR`

Offset `0x098`, 2 elements 0x4 apart · access `rw` · 32 bits

One bit each, the counterpart of `ICSET`. Not modelled; reads 0.

Sources:

- datasheet (medium): Broadcom `bcm2708_chip/multicore_sync.h`: `MS_ICCLR_0` and `MS_ICCLR_1`, 1 bit

## `MBOX`

Offset `0x0A0`, 5 elements 0x4 apart · access `rw` · 32 bits

Mailbox words 0 to 4. Words 5 and 7 have their own entries below, for what the firmware does with them, and word 6 is `MBOX6`. Not modelled; reads 0.

Sources:

- datasheet (medium): Broadcom `bcm2708_chip/multicore_sync.h`: `MS_MBOX_0`..`MS_MBOX_7` at `0x7E0000A0`..`0x7E0000BC`
- measured (high): Raspberry Pi 4B d03115, Linux idle: words 0 to 6 read 0 and word 7 reads `0x0A060000` under stock start4 `f5e89631`, `0x03060000` under a from-scratch firmware -- so word 7 carries something each firmware sets differently, and word 5 is 0 on both

## `MBOX6`

Offset `0x0B8` · access `rw` · 32 bits

Mailbox word 6. The boot ROM clears it right after it writes the boot mode into `MBOX7`; no bootloader stage touches it. Purpose otherwise unknown.

Sources:

- datasheet (medium): Broadcom `bcm2708_chip/multicore_sync.h`: `MS_MBOX_6`

## `VPUSEMA1`

Offset `0x0C4` · access `rw` · 32 bits

The second of the two semaphores that sit apart from the 32. Unlike those, it does not behave as test-and-set: it answers the same value to every read. Not modelled; reads 0.

Sources:

- datasheet (medium): Broadcom `bcm2708_chip/multicore_sync.h`: `MS_VPUSEMA_1`, 1 bit
- measured (high): Raspberry Pi 4B d03115: reads 1 on three consecutive reads under stock start4 `f5e89631` and again under a from-scratch firmware that never touches the block, so something holds it on every boot and a read does not take or clear it

## `VPU_STAT`

Offset `0x0C8` · access `r` · 32 bits

Not modelled; reads 0.

Sources:

- datasheet (medium): Broadcom `bcm2708_chip/multicore_sync.h`: `MS_VPU_STAT` at `0x7E0000C8`, RO, mask `0x00FF00FF`
- measured (high): Raspberry Pi 4B d03115: `0x00000004` under stock start4 `f5e89631`, `0x00000001` under a from-scratch firmware
