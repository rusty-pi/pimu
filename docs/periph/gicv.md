<!-- generated from specs/gicv.toml by `cargo run -- spec-docs --update` – do not edit -->

# `gicv` – GIC-400 virtual CPU interface

- Bus: `arm` (ARM physical address, low-peripheral mode)
- Base: `0xFF846000`
- Size: `0x2000`

The CPU interface a guest sees, fed from the list registers in GICH. KVM maps it into a VM's stage-2 tables and never touches it from the host, and nothing here runs a guest, so the frame reads as zero and ignores writes; no register is modelled.

Sources:

- linux (high): dtb: interrupt-controller@40041000 reg <... 0x40046000 0x2000> -> 0xFF846000, 8 KiB
- standard (high): ARM IHI 0048B (GICv2), the GICV registers

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
