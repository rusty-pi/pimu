# Peripheral register specs

One TOML file per modelled block, with provenance on every register and field
([#39](https://github.com/valtzu/rpi-virt-fw/issues/39)). Most of these blocks —
the VideoCore-side ones especially — have no public register documentation, so
where each fact came from matters as much as the fact itself.

Every modelled register block has one — the VPU-side peripherals, the ARM-only
GIC and local block, the xHCI controller and PCI function behind PCIe, and the
I²C and MDIO devices on the board. Not covered, because they are not register
maps: the generic timer (system registers, `src/periph/gentimer.rs`), the SD
card and USB devices (command protocols), and the catch-all stubs.

The files are used three ways:

1. `build.rs` generates Rust constants from them (`crate::spec::<block>`), and
   the device models match on those instead of literals. A malformed spec fails
   the build.
2. `tests/specs.rs` checks them against the model: every spec has exactly one
   device, every offset a device claims to decode (its `COVERAGE`) is in its
   spec, every register of a VPU-bus spec reaches the device rather than the
   catch-all stub or DRAM, and the registers the model leaves stubbed are
   reported.
3. `cargo run -- spec-docs --update` writes the Markdown under
   [`docs/periph/`](../docs/periph/). That directory is generated in full and
   committed; CI regenerates it and fails on any difference, so edit the spec,
   never the Markdown.

## Format

```toml
[block]
name    = "mcsync"          # module name and file stem
# bus   = "vpu"             # optional, see below
base    = 0x7E000000        # VPU bus address
size    = 0x1000            # decoded window
summary = "Doorbells / semaphores between the two VPU cores"
notes   = "..."             # optional
# instances       = 2       # optional: identical banks, e.g. one per core
# instance_stride = 0x800   # register offsets are for bank 0

[[block.source]]
kind       = "decompile"
ref        = "0x3ED3A114 / 0x3ED3A00C address the slot array at 0x7E000000"
confidence = "high"

# [[block.copy]]            # optional: the same block again at another base
# name  = "PMIC"            # generates PMIC_BASE
# base  = 0x7E205E00
# notes = "..."
#
# [[block.copy.source]]     # a copy needs its own provenance
# kind       = "decompile"
# ref        = "..."
# confidence = "high"

[[register]]
name   = "DOORBELL"
offset = 0x00
count  = 32                 # optional array; stride required with it
stride = 4
width  = 32                 # optional, 8 / 16 / 32, default 32
access = "rw"               # r, w, rw, w1c (write 1 to clear), rc (read to clear)
reset  = 0                  # optional
notes  = "Poster writes 1; the receiving core clears it once the work is done."

[[register.source]]
kind       = "decompile"
ref        = "0x3ED3A114 (post), 0x3ED3A00C (wait while non-zero)"
confidence = "high"

[[register.field]]
name   = "VALID"
bits   = "8"                # "hi:lo" or a single bit
access = "r"                # optional, defaults to the register's
notes  = "..."

[[register.field.source]]
kind       = "decompile"
ref        = "..."
confidence = "high"
```

The block, every register and every field take **one or more** source entries
— none at all fails the build. Several sources for the same fact is the point:
a register decoded from the firmware *and* confirmed on the reference board is
worth more than either alone, and a single `inferred` source is visibly weaker
than three agreeing ones. When sources disagree, keep all of them and say so in
the `note`; don't silently pick one.

Each source has `kind`, `ref`, `confidence` (`high` / `medium` / `low`) and an
optional `note`. `kind` is one of:

1. `datasheet` – BCM2711 / BCM2835 ARM Peripherals, or a third-party part's
   datasheet (the FXL6408)
2. `standard` – a published specification the block implements: ARM GICv2,
   PCI / PCIe, xHCI, SDHCI, IEEE 802.3 clause 22 — with the section
3. `linux` – upstream driver or DT binding
4. `decompile` – `firmware/source/*.c` / disassembly, with the address
5. `measured` – read on the reference board (debugfs etc.), with how it was read
6. `trace` – observed in a `boot` run
7. `inferred` – a guess; say why

`bus` says what the base and the offsets address:

| `bus` | `base` | offsets |
|---|---|---|
| `vpu` (default) | VPU bus address | bytes |
| `arm` | ARM physical address, low-peripheral mode — for blocks the VPU has no view of | bytes |
| `pci` | 0 | bytes into one PCI function's configuration space or BAR |
| `i2c` | 7-bit slave address | register numbers |
| `mdio` | PHY address | register numbers |

On `i2c` and `mdio` a register takes up one register number whatever its
width, and `size` is the number of register numbers the device decodes.

The build also refuses overlapping registers, a register past the window (or
past its bank), a field outside the register width or overlapping another
field, an unaligned offset, an address the bus cannot carry, a copy without a
source, unknown keys, and names that would generate the same constant twice.

## Generated constants

```rust
// generated from specs/mcsync.toml – do not edit
pub mod mcsync {
    pub const BASE: u32 = 0x7E000000;
    pub const SIZE: u32 = 0x1000;
    pub const DOORBELL: u32 = 0x0;
    pub const DOORBELL_COUNT: u32 = 32;
    pub const DOORBELL_STRIDE: u32 = 0x4;
    // ...
}
```

A block with banks also gets `INSTANCES` / `INSTANCE_STRIDE`; each copy gets
`<COPY>_BASE`; a register with a `reset` gets `<REG>_RESET`; each field gets
`<REG>_<FIELD>_SHIFT` and `<REG>_<FIELD>_MASK` (the mask is in place, i.e.
already shifted).

Measured read-only values (ID registers, capability words) go in as `reset`
with a `measured` source, and the device returns the generated `<REG>_RESET`
rather than a literal. Values that differ per bank (the PVT thresholds, the AVS
channel counts) cannot be a single `reset` and stay in the device model, with
the spec pointing at them.

## Rules

- No real OTP rows or anything device-unique — same rule as `CLAUDE.md`.
  Invented bench values only.
- Offsets and bit layouts are facts and fine to record; descriptions are written
  in our own words, not copied from datasheets or kernel headers.
- Values measured on the reference board get baked in with
  `kind = "measured"`; nothing reads the board at build or test time.
- A new device model comes with its spec: it exports a `COVERAGE` and is
  listed in `periph::SPEC_COVERAGE`, and `tests/specs.rs` fails until both
  exist.
