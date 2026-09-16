<!-- generated from specs/gicc.toml by `cargo run -- spec-docs --update` – do not edit -->

# `gicc` – GIC-400 CPU interface, banked per CPU and per security state

- Bus: `arm` (ARM physical address, low-peripheral mode)
- Base: `0xFF842000`
- Size: `0x2000`

Word access only. Linux enters at EL2 and, because this window is 8 KiB, runs split EOI: priority drop on `EOIR`, deactivation on `DIR`. The virtualisation interface follows it: `gich` at `0xFF844000`, `gicv` at `0xFF846000`.

Sources:

- linux (high): dtb: `interrupt-controller@40041000` `reg <... 0x40042000 0x2000 ...>` -> `0xFF842000`, 8 KiB
- standard (high): ARM IHI 0048B (GICv2), section 4.4
- measured (high): reference board `dmesg`: `GIC: Using split EOI/Deactivate mode`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CTLR`](#ctlr) | rw | 32 | 2, best high |
| `0x004` | [`PMR`](#pmr) | rw | 32 | 1, best high |
| `0x008` | [`BPR`](#bpr) | rw | 32 | 1, best high |
| `0x00C` | [`IAR`](#iar) | r | 32 | 1, best high |
| `0x010` | [`EOIR`](#eoir) | w | 32 | 1, best high |
| `0x014` | [`RPR`](#rpr) | r | 32 | 1, best high |
| `0x018` | [`HPPIR`](#hppir) | r | 32 | 1, best high |
| `0x01C` | [`ABPR`](#abpr) | rw | 32 | 1, best high |
| `0x020` | [`AIAR`](#aiar) | r | 32 | 1, best high |
| `0x024` | [`AEOIR`](#aeoir) | w | 32 | 1, best high |
| `0x028` | [`AHPPIR`](#ahppir) | r | 32 | 1, best high |
| `0x0D0` | [`APR0`](#apr0) | rw | 32 | 1, best high |
| `0x0E0` | [`NSAPR0`](#nsapr0) | rw | 32 | 1, best high |
| `0x0FC` | [`IIDR`](#iidr) | r | 32 | 1, best high |
| `0x1000` | [`DIR`](#dir) | w | 32 | 2, best high |

## `CTLR`

Offset `0x000` · access `rw` · 32 bits

Stored in the secure layout; the non-secure view remaps four of its bits.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `ENABLE_GRP0` | rw | Signal group-0 interrupts. |
| 1 | `ENABLE_GRP1` | rw | Signal group-1 interrupts. |
| 2 | `ACKCTL` | rw | Secure `IAR` may acknowledge group-1 interrupts. |
| 3 | `FIQEN` | rw | Group 0 signals as FIQ. |
| 4 | `CBPR` | rw | `BPR` governs both groups. |
| 5 | `FIQBYPDIS_GRP1` | rw | Bypass disable (stored). |
| 6 | `IRQBYPDIS_GRP1` | rw | Bypass disable (stored). |
| 7 | `FIQBYPDIS_GRP0` | rw | Bypass disable (stored). |
| 8 | `IRQBYPDIS_GRP0` | rw | Bypass disable (stored). |
| 9 | `EOIMODE_S` | rw | Secure split EOI. |
| 10 | `EOIMODE_NS` | rw | Non-secure split EOI; what Linux sets. |

Sources:

- standard (high): GICv2 4.4.1 `GICC_CTLR`
- decompile (high): armstub writes `0x1e7`: `EnableGrp0 | EnableGrp1 | AckCtl` | bypass disables

`ENABLE_GRP0` sources:

- standard (high): GICv2 4.4.1

`ENABLE_GRP1` sources:

- standard (high): GICv2 4.4.1

`ACKCTL` sources:

- standard (high): GICv2 4.4.1

`FIQEN` sources:

- standard (high): GICv2 4.4.1

`CBPR` sources:

- standard (high): GICv2 4.4.1

`FIQBYPDIS_GRP1` sources:

- standard (high): GICv2 4.4.1

`IRQBYPDIS_GRP1` sources:

- standard (high): GICv2 4.4.1

`FIQBYPDIS_GRP0` sources:

- standard (high): GICv2 4.4.1

`IRQBYPDIS_GRP0` sources:

- standard (high): GICv2 4.4.1

`EOIMODE_S` sources:

- standard (high): GICv2 4.4.1

`EOIMODE_NS` sources:

- standard (high): GICv2 4.4.1

## `PMR`

Offset `0x004` · access `rw` · 32 bits

Priority mask.

Sources:

- standard (high): GICv2 4.4.2 `GICC_PMR`

## `BPR`

Offset `0x008` · access `rw` · 32 bits

Binary point; minimum 2 secure, 3 non-secure.

Sources:

- standard (high): GICv2 4.4.3 `GICC_BPR`

## `IAR`

Offset `0x00C` · access `r` · 32 bits

Acknowledge: returns the ID (with the source CPU for SGIs) and makes it active. 1023 spurious, 1022 group 1 refused to secure without `AckCtl`.

Sources:

- standard (high): GICv2 4.4.4 `GICC_IAR`

## `EOIR`

Offset `0x010` · access `w` · 32 bits

End of interrupt: priority drop, plus deactivation unless split EOI is on.

Sources:

- standard (high): GICv2 4.4.5 `GICC_EOIR`

## `RPR`

Offset `0x014` · access `r` · 32 bits

Running priority; `0xFF` when nothing is active.

Sources:

- standard (high): GICv2 4.4.6 `GICC_RPR`

## `HPPIR`

Offset `0x018` · access `r` · 32 bits

What `IAR` would return, without taking it.

Sources:

- standard (high): GICv2 4.4.7 `GICC_HPPIR`

## `ABPR`

Offset `0x01C` · access `rw` · 32 bits

Aliased (group-1) binary point, secure access only.

Sources:

- standard (high): GICv2 4.4.8 `GICC_ABPR`

## `AIAR`

Offset `0x020` · access `r` · 32 bits

Aliased acknowledge: group-1 behaviour for secure software.

Sources:

- standard (high): GICv2 4.4.9 `GICC_AIAR`

## `AEOIR`

Offset `0x024` · access `w` · 32 bits

Aliased end of interrupt.

Sources:

- standard (high): GICv2 4.4.10 `GICC_AEOIR`

## `AHPPIR`

Offset `0x028` · access `r` · 32 bits

Aliased highest pending.

Sources:

- standard (high): GICv2 4.4.11 `GICC_AHPPIR`

## `APR0`

Offset `0x0D0` · access `rw` · 32 bits

Active priorities.

Sources:

- standard (high): GICv2 4.4.12 `GICC_APRn`

## `NSAPR0`

Offset `0x0E0` · access `rw` · 32 bits

Non-secure active priorities.

Sources:

- standard (high): GICv2 4.4.13 `GICC_NSAPRn`

## `IIDR`

Offset `0x0FC` · access `r` · 32 bits · reset `0x202143B`

ARM, GICv2 CPU interface.

Sources:

- measured (high): `/dev/mem` read of `0xFF8420FC` on the reference board

## `DIR`

Offset `0x1000` · access `w` · 32 bits

Deactivate: the second half of split EOI.

Sources:

- standard (high): GICv2 4.4.15 `GICC_DIR`
- linux (high): `irq-gic.c` deactivates through `GICC_DIR` in `EOImodeNS`
