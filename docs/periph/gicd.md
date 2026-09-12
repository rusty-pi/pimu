<!-- generated from specs/gicd.toml by `cargo run -- spec-docs --update` – do not edit -->

# `gicd` – GIC-400 distributor

- Bus: `arm` (ARM physical address, low-peripheral mode)
- Base: `0xFF841000`
- Size: `0x1000`

256 interrupt IDs, 4 CPUs, Security Extensions. The bit-per-interrupt arrays are listed at their architectural extent (1020 IDs); IDs past 255 read as zero. IPRIORITYR, ITARGETSR and the SGI source registers are byte-accessible. The armstub moves every interrupt to group 1 before Linux runs non-secure.

Sources:

- linux (high): dtb: interrupt-controller@40041000, arm,gic-400, reg <0x40041000 0x1000 ...> -> 0xFF841000
- standard (high): ARM IHI 0048B (GICv2), section 4.3, and the GIC-400 TRM

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CTLR`](#ctlr) | rw | 32 | 2, best high |
| `0x004` | [`TYPER`](#typer) | r | 32 | 1, best high |
| `0x008` | [`IIDR`](#iidr) | r | 32 | 1, best high |
| `0x080`–`0x0FC` (32 × 0x4) | [`IGROUPR`](#igroupr) | rw | 32 | 2, best high |
| `0x100`–`0x17C` (32 × 0x4) | [`ISENABLER`](#isenabler) | rw | 32 | 1, best high |
| `0x180`–`0x1FC` (32 × 0x4) | [`ICENABLER`](#icenabler) | rw | 32 | 1, best high |
| `0x200`–`0x27C` (32 × 0x4) | [`ISPENDR`](#ispendr) | rw | 32 | 1, best high |
| `0x280`–`0x2FC` (32 × 0x4) | [`ICPENDR`](#icpendr) | rw | 32 | 1, best high |
| `0x300`–`0x37C` (32 × 0x4) | [`ISACTIVER`](#isactiver) | rw | 32 | 1, best high |
| `0x380`–`0x3FC` (32 × 0x4) | [`ICACTIVER`](#icactiver) | rw | 32 | 1, best high |
| `0x400`–`0x7FC` (256 × 0x4) | [`IPRIORITYR`](#ipriorityr) | rw | 32 | 1, best high |
| `0x800`–`0xBFC` (256 × 0x4) | [`ITARGETSR`](#itargetsr) | rw | 32 | 2, best high |
| `0xC00`–`0xCFC` (64 × 0x4) | [`ICFGR`](#icfgr) | rw | 32 | 1, best high |
| `0xF00` | [`SGIR`](#sgir) | w | 32 | 1, best high |
| `0xF10`–`0xF1C` (4 × 0x4) | [`CPENDSGIR`](#cpendsgir) | rw | 32 | 1, best high |
| `0xF20`–`0xF2C` (4 × 0x4) | [`SPENDSGIR`](#spendsgir) | rw | 32 | 1, best high |

## `CTLR`

Offset `0x000` · access `rw` · 32 bits

Secure view: EnableGrp0 (0), EnableGrp1 (1). Non-secure view: EnableGrp1 in bit 0.

Sources:

- standard (high): GICv2 4.3.1 GICD_CTLR
- decompile (high): armstub writes 3 on the secondary cores

## `TYPER`

Offset `0x004` · access `r` · 32 bits · reset `0xFC67`

256 IDs, 4 CPUs, Security Extensions, LSPI 31.

Sources:

- measured (high): /dev/mem read of 0xFF841004 on the reference board

## `IIDR`

Offset `0x008` · access `r` · 32 bits · reset `0x200143B`

ARM (0x43B), GIC-400 distributor r0p1.

Sources:

- measured (high): /dev/mem read of 0xFF841008 on the reference board

## `IGROUPR`

Offset `0x080`, 32 elements 0x4 apart · access `rw` · 32 bits

Group per interrupt: 1 = group 1 (non-secure). Secure access only.

Sources:

- standard (high): GICv2 4.3.4 GICD_IGROUPRn
- decompile (high): armstub writes ~0 to IGROUPR0..7

## `ISENABLER`

Offset `0x100`, 32 elements 0x4 apart · access `rw` · 32 bits

Set-enable, one bit per interrupt.

Sources:

- standard (high): GICv2 4.3.5 GICD_ISENABLERn

## `ICENABLER`

Offset `0x180`, 32 elements 0x4 apart · access `rw` · 32 bits

Clear-enable.

Sources:

- standard (high): GICv2 4.3.6 GICD_ICENABLERn

## `ISPENDR`

Offset `0x200`, 32 elements 0x4 apart · access `rw` · 32 bits

Set-pending. SGI bits are read-only here.

Sources:

- standard (high): GICv2 4.3.7 GICD_ISPENDRn

## `ICPENDR`

Offset `0x280`, 32 elements 0x4 apart · access `rw` · 32 bits

Clear-pending.

Sources:

- standard (high): GICv2 4.3.8 GICD_ICPENDRn

## `ISACTIVER`

Offset `0x300`, 32 elements 0x4 apart · access `rw` · 32 bits

Set-active.

Sources:

- standard (high): GICv2 4.3.9 GICD_ISACTIVERn

## `ICACTIVER`

Offset `0x380`, 32 elements 0x4 apart · access `rw` · 32 bits

Clear-active.

Sources:

- standard (high): GICv2 4.3.10 GICD_ICACTIVERn

## `IPRIORITYR`

Offset `0x400`, 256 elements 0x4 apart · access `rw` · 32 bits

One priority byte per interrupt; 32 levels, the low three bits read 0. Non-secure software sees group-1 priorities shifted up a bit.

Sources:

- standard (high): GICv2 4.3.11 GICD_IPRIORITYRn

## `ITARGETSR`

Offset `0x800`, 256 elements 0x4 apart · access `rw` · 32 bits

One CPU-target byte per interrupt; banked IDs read back as 'this CPU'.

Sources:

- standard (high): GICv2 4.3.12 GICD_ITARGETSRn
- linux (high): irq-gic.c gic_get_cpumask relies on the banked read-back

## `ICFGR`

Offset `0xC00`, 64 elements 0x4 apart · access `rw` · 32 bits

Two bits per interrupt, bit 1 set = edge. SGIs are fixed at edge.

Sources:

- standard (high): GICv2 4.3.13 GICD_ICFGRn

## `SGIR`

Offset `0xF00` · access `w` · 32 bits

Raise an SGI: ID 3:0, target list 23:16, filter 25:24, NSATT 15.

Sources:

- standard (high): GICv2 4.3.15 GICD_SGIR

## `CPENDSGIR`

Offset `0xF10`, 4 elements 0x4 apart · access `rw` · 32 bits

Clear SGI pending, one source-CPU byte per SGI.

Sources:

- standard (high): GICv2 4.3.16 GICD_CPENDSGIRn

## `SPENDSGIR`

Offset `0xF20`, 4 elements 0x4 apart · access `rw` · 32 bits

Set SGI pending, one source-CPU byte per SGI.

Sources:

- standard (high): GICv2 4.3.17 GICD_SPENDSGIRn
