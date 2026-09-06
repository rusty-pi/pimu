# VideoCore IV scalar VPU — what the model assumes

The boot VPU ("VC4" scalar core) is proprietary and undocumented. Everything
here is from community reverse engineering; treat unconfirmed items as
hypotheses to check against real firmware traces.

## Instruction length (confirmed, `src/vpu/length.rs`)

Determined entirely by the first 16-bit parcel (read little-endian):

| first parcel      | length | class          |
|-------------------|--------|----------------|
| `0x0000..=0x7FFF` | 2      | scalar 16-bit  |
| `0x8000..=0xDFFF` | 4      | scalar 32-bit  |
| `0xE000..=0xEFFF` | 6      | scalar 48-bit  |
| `0xF000..=0xF7FF` | 6      | vector 48-bit  |
| `0xF800..=0xFFFF` | 10     | vector 80-bit  |

Parcel packing: 32-bit = `p0` then `p1`, each LE. 48-bit stream order is
`p0, p2, p1` (the middle parcel in memory is the *last* operand parcel).
80-bit = five LE parcels in order.

## Registers (partly confirmed)

- `r0..r31` general purpose.
- `r25 = sp` — confirmed: the 16-bit `add sp,#imm` form encodes destination 25.
- `r24 = gp` — inferred: dedicated base in 16-bit `ld/st (r24+imm)` forms.
- `r26 = lr` — inferred (community ABI). `bl`/`jl` write the return address here.
- `pc` and the status register are **not** in the GPR file; pc-relative and
  flag-consuming forms use dedicated encodings.
- Flags: N / Z / C / V with ARM semantics (`src/vpu/reg.rs`).

## Flag-setting policy (assumption — `src/vpu/insn.rs`)

`cmp` / `cmn` / `btest` set flags and discard their result. Explicit `adds` /
`subs` / `shls` set flags and write. Everything else writes its result and
leaves flags alone. This is a guess; VC4 may update flags more widely. The M1
test payloads only rely on `cmp`.

## Implemented in M1 (`src/vpu/decode.rs`, `src/vpu/exec.rs`)

16-bit: `nop`/`bkpt`/`sleep`/`rti`, `swi`, `b/bl <reg>`, `b<cond>` (7-bit),
`ld/st (sp+imm)`, `ld/st{w} (rs)`, `ld/st (rs+imm4)`, `add sp,#imm`,
`add rd,sp,#imm`, `p`-table ALU reg/reg, `q`-table ALU reg/imm5.

32-bit: `b<cond>`/`bl` (27-bit), ALU imm16 (`p` table), `add rd,rs,#imm16`,
`add rd,pc,#imm16`, triadic ALU (`p` table, predicated, reg or imm6),
`ld/st` with `gp`/`sp`/`pc`/`r0` + imm16 base, `ld/st (rs+imm12)`.

## Not yet implemented (seen in `start4.elf`, needed for M2/M3)

`version rd`, 48-bit `j`/`jl`/`b`/`bl <abs32>`, 48-bit `ld/st` with 27-bit
offset, `ldm`/`stm`, `addcmpb`, `mul`/`div` triadic variants, the entire
floating-point and vector units, `mov p<n>,r<n>` (peripheral-register moves),
exceptions/interrupts, MMU/cache.

## Sources

- Herman Hermitage, `videocoreiv.arch` and `videocore-disjs`
  <https://github.com/hermanhermitage/videocoreiv>
- vc4 binutils port (opcode tables) <https://github.com/poizan42/binutils-vc4>
- Mathias Gottschlag, `vctools` emulator (semantics cross-reference; BCM2835,
  unlicensed — read only) <https://github.com/mgottschlag/vctools>
- `librerpi/rpi-open-firmware` (an executable open bootloader for the VPU)
