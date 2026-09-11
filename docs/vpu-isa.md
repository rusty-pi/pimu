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
result-unit field. A subset is *executed* — the VRF<->memory transfers, the
scalar broadcast, `bitplanes` + lane predication, and the two forms that touch
no vector register at all (see below); everything else faults.

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

The file itself is modelled in `src/vpu/vrf.rs` — 64 rows of 64 bytes, plus one
zero flag per lane. Only *horizontal* windows (16 consecutive elements of one
row) are implemented; a vertical slot, a column of the file, faults.

Which instructions execute is decided in `VecInsn::executable`, and every one of
them is matched as a whole instruction word: a template with only the
established fields left free, plus a value whitelist on each of those. A set bit
in a field this model does not interpret falls through to a fault, which is the
point — the encoding has corners this decoder renders only approximately
(per-slot `+rN` addends, fine-x coordinate bits, the accumulator modifiers), and
a loose field test would execute one of them wrongly and corrupt memory in
silence.

| form | example | what it does here |
|---|---|---|
| `v<w>{ld,st} <reg>[++],(rB[+=rI]) [REP n]` | `v32ld HY(0,0)++,(r1+=r4) REP r0` | 16 lanes between one VRF row and memory, `n` times, stepping the address by `rI` and (with `++`) the row by one. `rB` is **not** written back |
| `v<w>mov <reg>,rN` / `,#imm` | `v32mov HY(0,0),r1` | broadcast a scalar or a 6-bit immediate over the 16 lanes |
| `v<w>bitplanes -,rN SETF` | `08 f4 38 e0 c0 03` | one flag per lane, holding that lane's bit of `rN` |
| `v8ld -,(rN)` | `00 f0 38 e0 80 03` | reads 16 bytes at `rN` and discards them; no register changes |
| `v16mov -,rN SUMS rK` | `00 fc 38 e0 80 03 c0 f3 00 12` | `rK = 16 * sext16(rN)`, and the scalar N/Z flags follow |

Predicates 2 and 3 on a transfer select the lanes whose `bitplanes` bit was 0
and 1 respectively. Both polarities are in the firmware and they disagree, so
neither is a guess: `memcpy`'s tail (`0x3EDA292C`) builds `~0 << n`, leaving the
low `n` bits *clear*, and transfers under predicate 2; `memset`'s
(`0x3EDA2B5E`) builds a band of *set* bits and stores under predicate 3. The
other five predicates fault. `REP` field 7 means "the count is in `r0`" — again
forced by the code around it, which computes `r0 = min(rows, 64)` and afterwards
advances the pointers by exactly `r0 * 64` bytes.

The last two forms come from `FUN_0edc9e20` (`0x3EDC9E20`), the vector-unit read
fence that `vrf_release_vrf_semaphore` (`0x3EDC9D3C`) calls before polling the
outstanding-read count in coprocessor register 15. That result is dead — the
caller overwrites `r0` with 256 on the next instruction — so the sum's exact
value is unobservable in this firmware; the *lane count and signedness* are from
`videocoreiv.arch`'s `<sru>` description, not measured on hardware.

Between them these forms are all of VC4 libc's `memcpy` (`0x3EDA28D6`),
`memmove` (`0x3EDA2A00`) and `memset` (`0x3EDA2AB4`), which is what made them
worth implementing: the first two used to be emulated wholesale in Rust, keyed
on their addresses in one particular `start4.elf`.

## Not yet implemented

Everything the vector unit can do beyond the table above: the ALU ops
(`vadd`/`vand`/`vshl`/...), vertical (column) register windows, the per-slot
`+rN` coordinate addends, address offsets on a load or store, the accumulator
modifiers, and any lane flag other than the zero flag `bitplanes` writes. All of
them fault. MMU/cache is modelled as flat.

## Sources

- Herman Hermitage, `videocoreiv.arch` and `videocore-disjs`
  <https://github.com/hermanhermitage/videocoreiv>
- vc4 binutils port (opcode tables) <https://github.com/poizan42/binutils-vc4>
- Mathias Gottschlag, `vctools` emulator (semantics cross-reference; BCM2835,
  unlicensed — read only) <https://github.com/mgottschlag/vctools>
- `librerpi/rpi-open-firmware` (an executable open bootloader for the VPU)
