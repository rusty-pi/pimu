<!-- generated from specs/armlocal.toml by `cargo run -- spec-docs --update` – do not edit -->

# `armlocal` – ARM local block: only the two registers the armstub writes

- Bus: `arm` (ARM physical address, low-peripheral mode)
- Base: `0xFF800000`
- Size: `0x100`

On BCM2836/7 this was the interrupt controller; BCM2711 has a GIC-400 instead and Linux never probes this node (no interrupt-controller property). Every other offset faults rather than read 0, so an unexpected client is loud.

Sources:

- linux (high): dtb start4 hands to Linux: interrupt-controller@40000000, brcm,bcm2836-l1-intc, reg = <0x40000000 0x100> (0xFF800000 through the soc ranges), no interrupt-controller property
- decompile (high): the armstub start4 places at ARM address 0 (start4.elf file offset 0x1E40EC, armstub8 shape)

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`ARM_CONTROL`](#arm_control) | rw | 32 | 1, best high |
| `0x008` | [`CORE_TIMER_PRESCALER`](#core_timer_prescaler) | rw | 32 | 2, best high |

## `ARM_CONTROL`

Offset `0x000` · access `rw` · 32 bits

Written 0 by the armstub.

Sources:

- decompile (high): armstub: ldr x0, =0xff800000; str wzr, [x0]

## `CORE_TIMER_PRESCALER`

Offset `0x008` · access `rw` · 32 bits

Written 0x80000000 by the armstub; with ARM_CONTROL 0 the generic-timer counter runs at the 54 MHz crystal.

Sources:

- decompile (high): armstub: mov w1, #0x80000000; str w1, [x0, #8]; cntfrq_el0 = 54000000
- measured (high): reference board dmesg: 'arch_timer: cp15 timer(s) running at 54.00MHz (phys)'
