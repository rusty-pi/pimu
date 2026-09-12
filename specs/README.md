# Peripheral register specs

One TOML file per modelled block, with provenance on every register and field
([#39](https://github.com/valtzu/rpi-virt-fw/issues/39)). Most of these blocks —
the VideoCore-side ones especially — have no public register documentation, so
where each fact came from matters as much as the fact itself.

The files are used three ways:

1. `build.rs` generates Rust constants from them (`crate::spec::<block>`), and
   the device models match on those instead of literals. A malformed spec fails
   the build.
2. `tests/specs.rs` checks them against the model: every offset a device
   claims to decode (its `COVERAGE`) is in its spec, every spec register
   reaches the device rather than the catch-all stub, and the registers the
   model leaves stubbed are reported.
3. `cargo run -- spec-docs --update` writes the Markdown under
   [`docs/periph/`](../docs/periph/). That directory is generated in full and
   committed; CI regenerates it and fails on any difference, so edit the spec,
   never the Markdown.

## Format

```toml
[block]
name    = "mcsync"          # module name and file stem
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

1. `datasheet` – BCM2711 / BCM2835 ARM Peripherals
2. `linux` – upstream driver or DT binding
3. `decompile` – `firmware/source/*.c` / disassembly, with the address
4. `measured` – read on the reference board (debugfs etc.), with how it was read
5. `trace` – observed in a `recon` run
6. `inferred` – a guess; say why

The build also refuses overlapping registers, a register past the window (or
past its bank), a field outside the register width or overlapping another
field, an unaligned offset, unknown keys, and names that would generate the
same constant twice.

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

A block with banks also gets `INSTANCES` / `INSTANCE_STRIDE`; a register with a
`reset` gets `<REG>_RESET`; each field gets `<REG>_<FIELD>_SHIFT` and
`<REG>_<FIELD>_MASK` (the mask is in place, i.e. already shifted).

## Rules

- No real OTP rows or anything device-unique — same rule as `CLAUDE.md`.
  Invented bench values only.
- Offsets and bit layouts are facts and fine to record; descriptions are written
  in our own words, not copied from datasheets or kernel headers.
- Values measured on the reference board get baked in with
  `kind = "measured"`; nothing reads the board at build or test time.
- Blocks are converted as they get touched, not in one sweep. A converted
  device exports a `COVERAGE` and is listed in `periph::SPEC_COVERAGE`.
