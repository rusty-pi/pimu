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

## Implemented (`src/vpu/decode.rs`, `src/vpu/exec.rs`)

16-bit: `nop`/`bkpt`/`sleep`/`rti`, `swi`, `version rd`, `switch`, `b/bl <reg>`,
`b<cond>` (7-bit), `ld/st (sp+imm)`, `ld/st{w} (rs)`, `ld/st (rs+imm4)`,
`add sp,#imm`, `add rd,sp,#imm`, `ldm`/`stm` (`push`/`pop` multi),
`p`-table ALU reg/reg, `q`-table ALU reg/imm5.

32-bit: `b<cond>`/`bl` (27-bit), `addcmpb`, ALU imm16 (`p` table),
`add rd,rs,#imm16`, `add rd,pc,#imm16`, triadic ALU incl. `mul`/`div`/`mulhd`
(predicated, reg or imm6), scalar float ALU (`fadd`/`fmul`/… incl. the 6-bit
minifloat immediate), `mov p<n>,r<n>` / `mov r<n>,p<n>` (coprocessor-register
moves), `ld/st` with `gp`/`sp`/`pc`/`r0` + imm16 base, `ld/st (rs+imm12)`.

48-bit: `j`/`jl`/`b`/`bl <abs32>`, `ld/st` with 27-bit offset, ALU forms.

Vector (48/80-bit, `0xF000..`): decoded to operands — class, sub-op, element
width, the three VRF slot descriptors and their coordinates, the memory
addressing form, `REP`/predication/`SETF`, and the 80-bit accumulator / scalar-
result-unit field. Only the two encodings that touch **no** vector register are
*executed* (see below); everything else faults.

Machine side: exception / timer-IRQ delivery through the firmware's vector
table, dual VPU cores (core 1 brought up at the `start4` trampoline).

## The vector unit (confirmed against two references)

The Vector Register File is a 64x64 array of bytes; a vector register is a
16-element window into it, named by a 4-bit descriptor (element width,
horizontal/vertical direction, column band) plus a 6-bit coordinate. Descriptors
14 and 15 are the "dash" slot, which names no register at all — `-` in
`videocoreiv.arch`, whose meaning depends on position: *discard the result* (D),
*ignore* (A), *use the coordinate as a scalar register* (B).

Field layout is transcribed from `videocoreiv.arch` (which flags its own vector
section as experimental) and then **checked byte for byte** against
`binutils-vc4`'s gas test corpus — `gas/testsuite/gas/vc4/{dash,accmods,
alu80-setf,wide,vldst}.d` — and against that assembler's objdump run over
`start4.elf` itself. Hermitage's 80-bit *memory* patterns place the address
fields correctly; his 48-bit and 80-bit *ALU* patterns match exactly.

The VRF itself is **not modelled**, so nearly every vector instruction faults.
Exactly two encodings are executed, matched as whole instruction words with only
the established fields left free:

| encoding | spelling | what it does here |
|---|---|---|
| `00 f0 38 e0 80 03` | `v8ld -,(rN)` | reads 16 bytes at `rN` and discards them; no register changes |
| `00 fc 38 e0 80 03 c0 f3 00 12` | `v16mov -,rN SUMS rK` | `rK = 16 * sext16(rN)`, and the scalar N/Z flags follow |

Both come from `FUN_0edc9e20` (`0x3EDC9E20`), the vector-unit read fence that
`vrf_release_vrf_semaphore` (`0x3EDC9D3C`) calls before polling the
outstanding-read count in coprocessor register 15. Its result is dead — the
caller overwrites `r0` with 256 on the next instruction — so the sum's exact
value is unobservable in this firmware; the *lane count and signedness* are from
`videocoreiv.arch`'s `<sru>` description, not measured on hardware.

## Not yet implemented

The vector register file, and therefore every vector instruction that reads or
writes one — including the `v32` vld/vst pair inside VC4 libc's `memcpy`
(`0x3EDA28F2` / `0x3EDA2904`), which is special-cased in the executor under
`--skip-unimpl` and is not reached by the current boot. MMU/cache is modelled as
flat.

## Sources

- Herman Hermitage, `videocoreiv.arch` and `videocore-disjs`
  <https://github.com/hermanhermitage/videocoreiv>
- vc4 binutils port (opcode tables) <https://github.com/poizan42/binutils-vc4>
- Mathias Gottschlag, `vctools` emulator (semantics cross-reference; BCM2835,
  unlicensed — read only) <https://github.com/mgottschlag/vctools>
- `librerpi/rpi-open-firmware` (an executable open bootloader for the VPU)
