<!-- generated from specs/mcsync.toml by `cargo run -- spec-docs --update` – do not edit -->

# `mcsync` – Doorbells / semaphores between the two VPU cores

- Bus: `vpu` (VPU bus address)
- Base: `0x7E000000`
- Size: `0x1000`
- Interrupts: `ACK76` VPU source 76 · `ACK77` VPU source 77

Sources:

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
| `0x000`–`0x07C` (32 × 0x4) | [`DOORBELL`](#doorbell) | rw | 32 | 2, best high |
| `0x080` | [`PENDING`](#pending) | r | 32 | 1, best medium |
| `0x084` | [`ACK76`](#ack76) | rw | 32 | 2, best medium |
| `0x088` | [`ACK77`](#ack77) | rw | 32 | 2, best medium |
| `0x0B4` | [`SECURE_MARK`](#secure_mark) | rw | 32 | 2, best high |
| `0x0BC` | [`REG_0BC`](#reg_0bc) | r | 32 | 1, best high |
| `0x0C0` | [`LOCK`](#lock) | rw | 32 | 2, best high |

## `DOORBELL`

Offset `0x000`, 32 elements 0x4 apart · access `rw` · 32 bits

Poster writes 1; the receiving core clears it once the work is done.

Sources:

- decompile (high): `0x3ED3A114` (post), `0x3ED3A00C` (wait while non-zero)
- trace (high): `recon` run: clock-service manager posts slot 6, then suspends

## `PENDING`

Offset `0x080` · access `r` · 32 bits

Doorbells pending for this core's interrupt; the ISR clears exactly these bits from its ack word.

Sources:

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

## `SECURE_MARK`

Offset `0x0B4` · access `rw` · 32 bits

Both bootloader stages write 0 here as they start, OR bit 31 of it into their secure-boot flags each time they work them out, and set bit 31 once any flag is set (`SIGNED_BOOT`, or the OTP secure-boot rows), so a later look within the stage sees secure boot even if the settings no longer say so. Reads 0 in the model, so there every look starts afresh.

Sources:

- decompile (high): EEPROM bootmain: `0xA7B88` (clear at start), `0xA92C6` (read into the flags), `0xA934A` / `0xA9352` (set bit 31 when the flags are non-zero)
- trace (high): `--trace-mmio`, `--bootconf SIGNED_BOOT=1`: bootsys writes 0 at `0x80007ED0` and reads / sets it at `0x8000902C`, `0x800090A4` / `0x800090A8`; bootmain clears it, then every flags check reads it twice and writes `0x80000000`

## `REG_0BC`

Offset `0x0BC` · access `r` · 32 bits

Both bootloader stages read this as they start, just before clearing `SECURE_MARK`, and keep bits 16-23. Meaning unknown; it reads 0 in the model.

Sources:

- trace (high): `--trace-mmio`: bootsys `0x80007EBE`, bootmain `0xA7B70`, each followed by a read of the system timer and the write to `SECURE_MARK`

## `LOCK`

Offset `0x0C0` · access `rw` · 32 bits

start4 reads this word and writes its own address back into it from dozens of places, tens of thousands of times a boot; it reads 0 in the model. By the look of it a lock word the two cores claim.

Sources:

- trace (high): pinned start4: reads at `0x3EC3C6F6`, `0x3EC3E1CC` and others, each followed by `0x7E0000C0` <- `0x7E0000C0` (`0x3EC3C756`, `0x3EC3E23A`, ...); the busiest pair of sites is `0x3ED651D8` / `0x3ED65216`
- inferred (low): read, then write your own address, is a claim protocol; nothing in the boot reads it back as anything but 0
