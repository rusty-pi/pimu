<!-- generated from specs/bell.toml by `cargo run -- spec-docs --update` – do not edit -->

# `bell` – The four ARM <-> VideoCore doorbells, VCHIQ's wake path

- Bus: `vpu` (VPU bus address)
- Base: `0x7E00B840`
- `VPU` copy: `0x7E00B940`
- Size: `0x10`
- Interrupts: VPU source 94 · `DOORBELL0` GIC id 66 (`GIC_SPI 34`) · `DOORBELL1` GIC id 67 (`GIC_SPI 35`)

Four one-word doorbells, carved out of the ARM control block's window and with a second view `0x100` above like every register in this block. Bells 0 and 1 raise the ARM (ARMC interrupts 2 and 3), bells 2 and 3 raise the VPU. A write rings a bell whatever value it carries; a read returns bit 2 set if it was rung and clears it, which is how the interrupt is acknowledged. Which *view* a sender writes is not decided by any source here — the stock firmware's accessors take the view as an argument (`0x7E00B840 + view * 0x100 + bell * 4`, read at `0xEC56066`, write at `0xEC5607A`) — so the model rings and clears one bell per index, whichever view the access came through. Every access either side actually makes is consistent with that: Linux reads bell 0 and writes bell 2 through the ARM's view, the firmware writes bell 0 and reads bells 2 and 3 through the VPU's.

Sources:

- linux (high): `mailbox@7e00b840`, `brcm,bcm2711-vchiq`, `reg = <0x7e00b840 0x3c>`; `vchiq_arm.c` has `BELL0 0x00`, `BELL2 0x08` and reads `ARM_DS_ACTIVE` (bit 2) out of `BELL0` in `vchiq_doorbell_irq`, writing `BELL2` to `trigger vc interrupt` in `remote_event_signal`
- decompile (high): start4's bell accessors: `0xEC56066` reads `[0x7E00B840 + bell * 4]`, `0xEC5607A` writes `[0x7E00B840 + view * 0x100 + bell * 4]`
- datasheet (high): BCM2711 ARM Peripherals, §6.2.3 Table 101: ARMC IRQs 2 and 3 are `Doorbell 0` and `Doorbell 1`. Table 118: `BELL_IRQ0` / `BELL_IRQ1` are bits 2 and 3 of the ARMC pending word, each "cleared by reading the relevant doorbell register"
- inferred (medium): size: four words. The device tree claims `0x3c` from here, but nothing in the ARM's view above `+0x0C` is a doorbell -- `0x7E00B880` is the mailbox block

`VPU` copy:

The VPU's view, which is what start4 drives: its ISR for source 94 reads `0x7E00B948` and `0x7E00B94C`. It sits inside the window `specs/mbox.toml` claims, so this block is decoded ahead of the mailbox.

- decompile (high): ISR `0xEC58302`: `Mov r6, 0x7E00B940; Load r0, (r6+8); Btest r0, #2` then the same for `(r6+12)`, each dispatching a registered callback, before it looks at the mailbox at `0x7E00B980`

Interrupts (VPU source 94 · `DOORBELL0` GIC id 66 (`GIC_SPI 34`) · `DOORBELL1` GIC id 67 (`GIC_SPI 35`)):

Bells 2 and 3 raise the VPU on source 94, the whole ARM control block's line -- the same source the mailbox arrives on, which is why one stock ISR services both. Bells 0 and 1 raise the ARM as ARMC interrupts 2 and 3; the ARMC peripheral IRQs are GIC ids 64 to 79, so doorbell 0 is `GIC_SPI 34` -- the `interrupts = <0 0x22 4>` of the `brcm,bcm2711-vchiq` node -- and doorbell 1 `GIC_SPI 35`.

- linux (high): `mailbox@7e00b840 { interrupts = <0x00 0x22 0x04>; }` in the tree the reference board boots, bound by `devm_request_irq(..., vchiq_doorbell_irq, ...)`
- decompile (high): start4's handler table has `src 94 handler=0xEC58302`, and that handler reads the VPU view of bells 2 and 3
- datasheet (high): BCM2711 ARM Peripherals, §6.2.3 Table 101 (ARMC IRQ 2 / 3) and §6.3 Figure 7 (ARMC peripheral IRQs on GIC SPI ids 64 to 79)

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000`–`0x00C` (4 × 0x4) | [`BELL`](#bell) | rw | 32 | 2, best high |

## `BELL`

Offset `0x000`, 4 elements 0x4 apart · access `rw` · 32 bits

Doorbell 0 to 3. Any write rings it; a read returns `RUNG` if it was rung and clears it.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 2 | `RUNG` | rw | The bell has been rung and not yet read. `ARM_DS_ACTIVE` in `vchiq_arm.c`, `BELL_IRQ0` / `BELL_IRQ1`'s position in the ARMC pending word, and the bit start4's ISR tests on bells 2 and 3. |

Sources:

- linux (high): `vchiq_arm.c`: `readl(regs + BELL0)` in the doorbell ISR, `writel(0, regs + BELL2)` to ring the VPU
- decompile (high): start4 `0xEC56066` / `0xEC5607A` index the block by bell number

`RUNG` sources:

- linux (high): `#define ARM_DS_ACTIVE BIT(2)`, tested against a `BELL0` read
- decompile (high): ISR `0xEC58302`: `Btest r0, #2` on the word read from `0x7E00B948`
