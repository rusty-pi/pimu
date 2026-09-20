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
- `r30 = sr` and `r31 = pc` *in the encodings*: the `lea rd,(rN+imm16)` form
  reads `N == 31` as the program counter (`src/vpu/decode.rs`), and the
  exception path saves and restores `r30` as the status word, interrupt-enable
  bit 30 and all (`Vpu::sr`, `src/vpu/exec.rs`). The model still holds `pc` and
  N/Z/C/V in their own fields and folds the flags into the low nibble of `r30`
  only when an exception saves it.
- Flags: N / Z / C / V, one of which is **not** ARM's. `c` is *borrow* on a
  subtraction, so `cs` means unsigned-below (`lo`) and `cc` unsigned
  higher-or-same (`hs`) — the opposite way round from ARM (`Flags::test`,
  `src/vpu/reg.rs`).

## Three encodings that are easy to get backwards (confirmed)

- **`switch` table entries are signed.** The table starts right after the
  2-byte instruction, and `entry[idx]` is a *signed* displacement in halfwords
  from that base: a handler defined before the `switch` is reached through a
  negative entry, and the default case is a small negative offset back to the
  fallback. Read unsigned, a firmware `switch` lands in the middle of
  unrelated code (fixed 2026-09-07).
- **`ldm` / `stm` put the highest register at the lowest address**, with `lr`
  in the top word of a frame. ThreadX's interrupt frame is the proof: the ISR
  stub pushes `{r0-r5, lr}`, `_tx_thread_context_save` (`0x3EC3FA34`) then
  pushes `{r6-r15}` and `{r16-r23}`, and `_tx_thread_schedule` (`0x3EC40040`)
  undoes all three with `pop {r16-r23}; pop {r0-r15}; ld r26,(sp)++; rti` —
  which only composes if each block runs downwards in register number
  (fixed 2026-09-10).
- **There is no signed store, so the `ww = 11` store slot is a signed-byte
  *load*.** The `{ww, L}` bits spell word / half / byte / signed-half against
  load / store, which leaves one encoding spare, and the core spends it on
  `ldsb`. start4's bootloader stage is the proof: mbedtls' `ecp_mod_p256`
  loads its `signed char` carry (a byte at `sp+7`) with `1010 1001 111d dddd`
  and branches on the sign. Decoded as a store, the fast reduction came out
  wrong and the `while (N >= P) N -= P` loop after it never finished
  (fixed 2026-09-12, `src/vpu/decode.rs`).

## Flag-setting policy (assumption — `src/vpu/insn.rs`)

`cmp` / `cmn` / `btest` set flags and discard their result. The explicit
flag-setting triadics `adds` / `subs` / `shls` (sub-op `0x28..=0x2a`) set flags
and write, and `fcmp` sets them. Everything else writes its result and leaves
flags alone. This is still a guess; VC4 may update flags more widely. What is
behind it now is a whole firmware boot and a Linux boot running on it without
a golden-log divergence — which is evidence, not proof: an instruction whose
flags nothing consumes before the next `cmp` would look the same either way.

## Implemented (`src/vpu/decode.rs`, `src/vpu/exec.rs`)

The ALU tables are whole: all 32 `p` entries, all 16 `q` entries, the `f`
float table, and every triadic sub-op through `0x38`. Triadic `0x39..=0x3F` is
the only gap left as `AluOp::Unimpl`, and no firmware this model boots has
executed one. An encoding outside the decoder is `Op::Unimpl`, which faults
under the default policy and is collected as an `UnimplHit` under the
lenient one, so a gap shows up as a report rather than as a wrong answer.

16-bit: `nop`/`bkpt`/`sleep`/`rti`, `ei`/`di`, `swi <reg>` and `swi #imm6`,
`version rd`, `switch` and `switch.b`, `b/bl <reg>`, `b<cond>` (7-bit),
`ld/st (sp+imm)`, `ld/st{w} (rs)` (all eight sub-ops, `ldsb` included),
`ld/st (rs+imm4)`, `add sp,#imm`, `lea rd,(sp+imm)`, `ldm`/`stm` (`push`/`pop`
multi, `r31`/wrap forms included), `p`-table ALU reg/reg, `q`-table ALU
reg/imm5.

32-bit: `b<cond>`/`bl` (27-bit), `addcmpb`, ALU imm16 (`p` table),
`lea rd,(rN+imm16)` and `lea rd,(pc+imm16)`, triadic ALU incl.
`mul`/`div`/`mulhd`/`count`/`clamp16`/`add`- and `subscale` (predicated, reg or
imm6), scalar float ALU (`fadd`/`fmul`/… incl. the 6-bit minifloat immediate)
and the `0xCA00` convert block (`ftrunc`/`floor`/`flts`/`fltu`),
`mov p<n>,r<n>` / `mov r<n>,p<n>` (coprocessor-register moves), and the
load/store block: `(ra+rb)`, `(rs+imm12)`, `(--rs)` / `(rs++)` writeback, and
`gp`/`sp`/`pc`/`r0` + imm16 — the register-indexed and writeback forms with
their condition field.

48-bit: `j`/`jl`/`b`/`bl <abs32>`, `lea rd,(pc+off32)`, `ld/st` with a 27-bit
offset, ALU with a 32-bit immediate.

Vector (48/80-bit, `0xF000..`): decoded to operands — class, sub-op, element
width, the three VRF slot descriptors and their coordinates, the memory
addressing form, `REP`/predication/`SETF`, and the 80-bit accumulator / scalar-
result-unit field. A subset is *executed* — the VRF<->memory transfers, the
scalar broadcast, `bitplanes` + lane predication, and the two forms that touch
no vector register at all (see below); everything else faults.

Machine side: exception / timer-IRQ delivery through the firmware's vector
table, and two VPU cores. Core 1 starts where `start4` writes its entry to
`IC1_WAKEUP` (`corectl` `+0x834`, `0x7E00_2834`), which on this bench happens
only on a boot that goes on to Linux.

## The vector unit (confirmed against two references)

The Vector Register File is a 64x64 array of bytes; a vector register is a
16-element window into it, named by a slot: a 4-bit type nibble, a coordinate,
a `+rN` scalar addend, and the `*` and `++` modifiers. Types 14 and 15 are the
"dash" slot, which names no register at all — `-` in `videocoreiv.arch`, whose
meaning depends on position: *discard the result* (D), *ignore* (A), *use the
coordinate as a scalar register* (B).

**The type nibble is not an element width.** It says how coarsely the slot can
spell its column — `H` in steps of 16 bytes, `HX` in steps of 32, `HY` only 0,
and the three `V` types the same for a column — while the width of an element
comes from the operation (`v8` / `v16` / `v32`). Reading it as a width is what
made 3659 of `start4.elf`'s vector instructions look like a width mismatch: a
16-bit operation on an `H` slot, such as `v16mov H(0,32),0x4`, is ordinary.

The fields are `binutils-vc4`'s — `print_vector_reg_1` in `opcodes/vc4-dis.c`,
a fork whose vector decoding has been corrected against hardware probes. Its
`f-op<hi>-<lo>` field names number the bits by 16-bit parcel in *memory* order,
which `src/vpu/decode.rs` reads through one helper (`cg`). The whole of
`start4.elf`'s `.text` was then compared against that disassembler instruction
by instruction: **15054 of the 15180 vector words agree**, and the 126 that do
not are ones objdump itself renders as a raw `vec48` / `vec80`, having no form
for them.

Two details are easy to get wrong, and both were:

- a **vertical** slot's coordinate splits — `y` names the 16-aligned band of
  rows the column covers, and the coordinate's low nibble belongs to `x`. Read
  as a horizontal coordinate it prints rows that cannot exist.
- the bit beside the B slot (`f-op38`) is that slot's `+rN` **only when B is a
  vector register**. With a dash or an immediate there is no coordinate to step
  and the same bit is `SETF` — and `vgetacc`, which never addresses memory,
  always reads it as `SETF`.

The file itself is modelled in `src/vpu/vrf.rs` — 64 rows of 64 bytes, plus one
zero flag per lane. Both directions of window are implemented: 16 consecutive
elements along a row, or the same 16 read down a column, one per row, from the
16-aligned band the slot names. What a vertical slot's `++` steps its column by
is not established, so that one still faults.

Which instructions execute is decided in `VecInsn::executable`, and every one of
them is matched as a whole instruction word: a template with only the
established fields left free, plus a value whitelist on each of those. A set bit
in a field this model does not interpret falls through to a fault, which is the
point — a loose field test would execute one of those forms wrongly and corrupt
memory in silence. Decoding a field is not the same as executing it: a load's
address displacement is read correctly now and still faults, because nothing
says whether it counts bytes or elements.

| form | example | what it does here |
|---|---|---|
| `v<w>{ld,st} <reg>[++],(rB[+=rI]) [REP n]` | `v32ld HY(0,0)++,(r1+=r4) REP r0` | 16 lanes between one VRF row and memory, `n` times, stepping the address by `rI` and (with `++`) the row by one. `rB` is **not** written back |
| `v<w>mov <reg>[++],rN` / `,#imm` `[REP n]` | `v32mov HY(0,0),r1` | broadcast a scalar or a 6-bit immediate over the 16 lanes; the 80-bit form repeats it down the rows, which is how the boot ROM clears memory |
| `v<w>bitplanes -,rN SETF` | `08 f4 38 e0 c0 03` | one flag per lane, holding that lane's bit of `rN` |
| `v8ld -,(rN)` | `00 f0 38 e0 80 03` | reads 16 bytes at `rN` and discards them; no register changes |
| `v16mov -,rN SUM{U,S} rK` | `00 fc 38 e0 80 03 c0 f3 00 12` | `rK = 16 * rN`, sign- or zero-extended to the lane width, and the scalar N/Z flags follow |

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
(`vadd`/`vand`/`vshl`/...), the per-slot `+rN` coordinate addends, the `*`
column offset, `++` on a vertical slot, address displacements on a load or
store, the accumulator modifiers, and any lane flag other than the zero flag
`bitplanes` writes. All of them fault.

Of the 15180 vector instructions in `start4.elf`'s `.text`, 1567 are forms this
model executes. That is fewer than the 1634 it used to run, in both directions:
vertical windows and mixed-width operands are new, while 300 instructions whose
slot carries a `+rN` addend now fault instead of running with the addend
silently ignored — the old decoder had no field for it.

Outside the ISA proper: no dual-issue pipeline, and the MMU and the caches are
flat — the four VC4 aliases (`0x0`, `0x4000_0000`, `0x8000_0000`,
`0xC000_0000`) fold onto one backing store, so a line the firmware writes
cached and never flushes reads back as the new bytes here and the old ones on
silicon. Two checks stand in for the behaviour rather than modelling it, both
off by default: `--check-coherency` reports each read that would see stale
bytes on hardware (`src/coherency.rs`), and `--check-alignment` reports every
scalar access the core could not do in one, GCC's VC4 port being
`STRICT_ALIGNMENT` (`src/align.rs`).

## Sources

- Herman Hermitage, `videocoreiv.arch` and `videocore-disjs`
  <https://github.com/hermanhermitage/videocoreiv>
- vc4 binutils port (opcode tables) <https://github.com/poizan42/binutils-vc4>
- Mathias Gottschlag, `vctools` emulator (semantics cross-reference; BCM2835,
  unlicensed — read only) <https://github.com/mgottschlag/vctools>
- `librerpi/rpi-open-firmware` (an executable open bootloader for the VPU)
