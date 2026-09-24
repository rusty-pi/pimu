# Documentation

## The board

Every part the model knows about, where it hangs off the BCM2711, and what
reaches what. The default board: a Raspberry Pi 4B d03115 on C0 silicon.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="board-sheet-dark.svg">
  <img alt="Block diagram of the Raspberry Pi 4B as this model implements it: the BCM2711 in the centre with its controller ports on the edges, the SPI NOR flash, the PMICs and the FXL6408 expander on the left, the Ethernet PHY and the VL805 xHCI controller on the right, and the SD slot, USB-C port, header and connectors along the bottom." src="board-sheet.svg">
</picture>

[`board-sheet.svg`](board-sheet.svg) is **drawn by hand**: no generator places a
controller on the edge of the die, carries the power rails, or uses net labels
for the expander pins. What is generated is everything that could drift —

- `tests/board_sheet.rs` checks the drawing against `specs/*.toml`: every block
  is named in a `data-block`, every `parent` is drawn as a `data-edge`, and an
  address written on a part is the one its spec gives.
- [`board-sheet-dark.svg`](board-sheet-dark.svg) is the same drawing with the
  dark palette, written by `cargo run -- spec-docs --update`
  (`src/sheet.rs`). Edit the light sheet; never that one.

So a new block, `parent`, copy or base means editing one file, and the checks
say so until you do.

### Another board

This sheet is one board: the `d03115` its `<svg data-board="d03115">` names. A
4B rev 1.2, a Pi 400 or a CM4 differ in what hangs off the SoC — the PMIC at
`0x1D` instead of the pair at `0x1B` / `0x1E`, no VL805 on a CM4 — so each gets
its own drawing rather than a switch inside this one:

1. copy the sheet to `docs/board-sheet-<board>.svg` and set its `data-board` to
   that revision code;
2. redraw the parts that differ;
3. run `cargo run -- spec-docs --update`, which writes its dark twin.

Nothing else: `tests/board_sheet.rs` picks up every `board-sheet*.svg` under
`docs/`, and asks the model — `Pmic::for_board`, the same code the boot uses —
which board-conditional parts that revision has. A sheet that draws a part its
board does not have fails; a part named for contrast is marked
`data-block-alt` (as `pmic_1d` is here) and must *not* be one the board has.
When a second kind of part becomes board-conditional, `fitted_on` in that test
is the one place to say so.

Legend, in short: a solid green wire is a modelled link, a dashed grey one is
probed or not modelled (nothing answers), a dotted one is "carried by" where
there is no wire to draw, amber is a power rail, and a flag is a net label
joined by name to an FXL6408 pin.

## The rest

| Document | What is in it |
|---|---|
| [`boot-chain.md`](boot-chain.md) | The boot stages, from the VPU ROM to Linux, and what each one reads |
| [`running.md`](running.md) | Boot media, the SD-card variants, the OTP fuse files, wall budgets |
| [`building.md`](building.md) | Build profiles, the `diag` feature, PGO, the build's share of the machine |
| [`device-tree.md`](device-tree.md) | Getting the patched device tree out, and where `rpi-machine-id` comes from |
| [`periph/`](periph/) | Generated from `specs/*.toml`: one page per register block, with provenance, plus the `parent` tree and the interrupt lines |
| [`vpu-isa.md`](vpu-isa.md) | The VideoCore IV instruction set, with the evidence for each statement — generated from [`isa/vpu.toml`](../isa/vpu.toml) |
| [`diagnostics.md`](diagnostics.md) | The log channels and `PIMU_*` switches that find a wall |
| [`references.md`](references.md) | Outside sources: datasheets, kernel drivers, other people's reverse engineering |
