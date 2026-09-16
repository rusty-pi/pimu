<!-- generated from specs/gich.toml by `cargo run -- spec-docs --update` – do not edit -->

# `gich` – GIC-400 virtual interface control, banked per CPU

- Bus: `arm` (ARM physical address, low-peripheral mode)
- Base: `0xFF844000`
- Size: `0x2000`

Word access only. `+0x0000` is the accessing CPU's own block; `+0x1000 + 0x200 × n` is CPU n's, for a hypervisor that manages another core's list registers (address bits [11:9] pick the CPU, and the four past this GIC's CPUs read as zero). Four list registers per CPU. Only KVM uses it: with no guest running, the host kernel reads `VTR` when it probes the vgic and clears the list registers on every core.

Sources:

- linux (high): dtb: `interrupt-controller@40041000` `reg <... 0x40044000 0x2000 ...>` -> `0xFF844000`, 8 KiB
- standard (high): ARM IHI 0048B (GICv2), the GICH registers; GIC-400 TRM (ARM DDI 0471) for the memory map and the per-processor aliases
- measured (high): reference board `dmesg`: `kvm [1]: vgic interrupt IRQ9`, `kvm [1]: Hyp nVHE mode initialized successfully`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`HCR`](#hcr) | rw | 32 | 1, best high |
| `0x004` | [`VTR`](#vtr) | r | 32 | 1, best high |
| `0x008` | [`VMCR`](#vmcr) | rw | 32 | 1, best high |
| `0x010` | [`MISR`](#misr) | r | 32 | 1, best high |
| `0x020` | [`EISR0`](#eisr0) | r | 32 | 1, best high |
| `0x024` | [`EISR1`](#eisr1) | r | 32 | 1, best high |
| `0x030` | [`ELRSR0`](#elrsr0) | r | 32 | 1, best high |
| `0x034` | [`ELRSR1`](#elrsr1) | r | 32 | 1, best high |
| `0x0F0` | [`APR`](#apr) | rw | 32 | 1, best high |
| `0x100`–`0x10C` (4 × 0x4) | [`LR`](#lr) | rw | 32 | 1, best high |

## `HCR`

Offset `0x000` · access `rw` · 32 bits · reset `0x0`

Bits 1..7 enable the matching `MISR` conditions; `En` gates the maintenance interrupt as a whole.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `EN` | rw |  |
| 1 | `UIE` | rw |  |
| 2 | `LRENPIE` | rw |  |
| 3 | `NPIE` | rw |  |
| 4 | `VGRP0EIE` | rw |  |
| 5 | `VGRP0DIE` | rw |  |
| 6 | `VGRP1EIE` | rw |  |
| 7 | `VGRP1DIE` | rw |  |
| 31:27 | `EOICOUNT` | rw |  |

Sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_HCR`

`EN` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_HCR`

`UIE` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_HCR`

`LRENPIE` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_HCR`

`NPIE` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_HCR`

`VGRP0EIE` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_HCR`

`VGRP0DIE` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_HCR`

`VGRP1EIE` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_HCR`

`VGRP1DIE` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_HCR`

`EOICOUNT` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_HCR`

## `VTR`

Offset `0x004` · access `r` · 32 bits · reset `0x90000003`

Four list registers, five preemption bits, five priority bits. Linux's `vgic_v2_probe` reads it for the list-register count.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 5:0 | `LISTREGS` | r |  |
| 28:26 | `PREBITS` | r |  |
| 31:29 | `PRIBITS` | r |  |

Sources:

- standard (high): GIC-400 TRM (ARM DDI 0471), virtual interface control register summary: `GICH_VTR` resets to `0x90000003`

`LISTREGS` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VTR`

`PREBITS` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VTR`

`PRIBITS` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VTR`

## `VMCR`

Offset `0x008` · access `rw` · 32 bits · reset `0x0`

The guest's `GICV_CTLR`, `PMR` and binary points, as the hypervisor saves and restores them.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `VMGRP0EN` | rw |  |
| 1 | `VMGRP1EN` | rw |  |
| 2 | `VMACKCTL` | rw |  |
| 3 | `VMFIQEN` | rw |  |
| 4 | `VMCBPR` | rw |  |
| 9 | `VEM` | rw |  |
| 20:18 | `VMABP` | rw |  |
| 23:21 | `VMBP` | rw |  |
| 31:27 | `VMPRIMASK` | rw |  |

Sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VMCR`

`VMGRP0EN` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VMCR`

`VMGRP1EN` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VMCR`

`VMACKCTL` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VMCR`

`VMFIQEN` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VMCR`

`VMCBPR` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VMCR`

`VEM` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VMCR`

`VMABP` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VMCR`

`VMBP` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VMCR`

`VMPRIMASK` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_VMCR`

## `MISR`

Offset `0x010` · access `r` · 32 bits

Computed: EOI when any `EISR` bit is set; the others when their `HCR` enable is set and the condition holds. `HCR.En` with any bit set asserts the maintenance interrupt, PPI 9 (ID 25), level-sensitive.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `EOI` | r | Some list register has an EOI maintenance request (`EISR`). |
| 1 | `U` | r | Underflow: at most one list register holds a valid interrupt. |
| 2 | `LRENP` | r | `HCR.EOICount` is non-zero. |
| 3 | `NP` | r | No list register is pending. |
| 4 | `VGRP0E` | r |  |
| 5 | `VGRP0D` | r |  |
| 6 | `VGRP1E` | r |  |
| 7 | `VGRP1D` | r |  |

Sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_MISR`

`EOI` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_MISR`

`U` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_MISR`

`LRENP` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_MISR`

`NP` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_MISR`

`VGRP0E` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_MISR`

`VGRP0D` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_MISR`

`VGRP1E` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_MISR`

`VGRP1D` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_MISR`

## `EISR0`

Offset `0x020` · access `r` · 32 bits

Bit n: list register n is invalid, has `HW` clear and asks for an EOI maintenance interrupt (bit 19).

Sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_EISRn`

## `EISR1`

Offset `0x024` · access `r` · 32 bits

List registers 32..63: none here, reads as zero.

Sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_EISRn`

## `ELRSR0`

Offset `0x030` · access `r` · 32 bits

Bit n: list register n holds no valid interrupt and no pending EOI request, so the hypervisor may reuse it.

Sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_ELRSRn`

## `ELRSR1`

Offset `0x034` · access `r` · 32 bits

List registers 32..63: none here, reads as zero.

Sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_ELRSRn`

## `APR`

Offset `0x0F0` · access `rw` · 32 bits · reset `0x0`

The guest's active priorities, one bit per group priority.

Sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_APR`

## `LR`

Offset `0x100`, 4 elements 0x4 apart · access `rw` · 32 bits · reset `0x0`

One virtual interrupt each. With `HW` clear, `PHYSICALID` holds the requesting CPU of an SGI in [12:10] and the EOI maintenance request in bit 19. Bits [22:20] are reserved.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 9:0 | `VIRTUALID` | rw |  |
| 19:10 | `PHYSICALID` | rw |  |
| 27:23 | `PRIORITY` | rw |  |
| 29:28 | `STATE` | rw | 0 invalid, 1 pending, 2 active, 3 pending and active. |
| 30 | `GRP1` | rw |  |
| 31 | `HW` | rw |  |

Sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_LRn`

`VIRTUALID` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_LR`

`PHYSICALID` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_LR`

`PRIORITY` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_LR`

`STATE` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_LR`

`GRP1` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_LR`

`HW` sources:

- standard (high): ARM IHI 0048B (GICv2), `GICH_LR`
