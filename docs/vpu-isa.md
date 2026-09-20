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

## The vector unit (measured on real hardware)

The Vector Register File is 64 rows of 64 bytes, and a row is **sixteen lanes
of four bytes** — not sixty-four bytes in a line. Element `e` of a register
whose elements are `w` bytes wide sits at byte `(e & 15) * 4 + (e >> 4) * w` of
its row: the lane is the element's low four bits, and the rest of the index
picks the sub-field inside that lane. Consecutive elements are four bytes
apart, interleaved.

A register is sixteen elements. A *horizontal* one takes them along one row
from element `e0`; a *vertical* one takes the same element of sixteen
consecutive rows, from the 16-aligned band its coordinate names.

The slot's type nibble is the **register's** element width — `H`/`V` are
8-bit, `HX`/`VX` 16-bit, `HY`/`VY` 32-bit — and the operation's width is a
separate thing. The unit converts between them: narrowing truncates, widening
zero-extends, on loads and on stores alike. `v8ld HY(3,0),(r1)` reads sixteen
bytes and leaves sixteen 32-bit elements; `v32st H(1,0),(r0)` writes each 8-bit
element out as a word.

An ALU op converts the same way, and that is its commonest shape rather than a
corner: two thirds of the vector ALU instructions in `start4.elf` are an
operation wider than the registers it names. A source is read at its own
register's width and widened into the operation's — **a byte unsigned, a
halfword signed**: `v32mov HY(0,0),H(60,0)` leaves `0x000000ff` where the byte
was `0xff`, while `v32mov HY(0,0),HX(62,0)` leaves `0xffff8000` where the
halfword was `0x8000`. The result goes back the other way, truncated into the
destination element — except for the saturating ops, which clamp to what that
element can hold, `0..=0xff` for a byte register and signed for a wider one. So
`v32adds H(0,0),H(60,0),H(61,0)` over `0x80 + 0x80` answers `0xff`, while the
same addition into an `HX` destination answers `0x0100`. A shift, rotate or
reversal counts in the **operation's** width: `v32` takes five bits of B where
`v16` takes four, and `brev` with a zero count reverses the whole operation
width, not the register's. A register *wider* than the operation is refused —
`binutils-vc4` has no spelling for one, and `start4.elf` has thirteen.

A slot's first element is `band * 16 + fine`, counted in elements, which is why
the byte coordinate objdump prints and the element index part company as soon
as an element is wider than a byte. `+rN` adds to that element index, also in
elements. An address displacement is a plain byte offset. `++` steps the row
horizontally and the element vertically. A B-position immediate is **signed**
in both encodings — six bits wide in the 48-bit form, sixteen in the 80-bit
one: `v32mov HY(0,0),#0x20` fills every lane with `0xffffffe0`.

One asymmetry, and it matters: a **load** whose element straddles a 16-byte
boundary wraps inside that block rather than crossing it — `v32ld
HY(0,0),(r1+13)` over a page of ascending bytes reads `0e 0f 10 01`, taking its
fourth byte from the start of the block. A **store** crosses normally.

None of that is inferred. All of it was measured by running VPU code on a
Raspberry Pi 4B d03115 through the firmware's `EXECUTE_CODE` mailbox tag, and
reading the register file back out with `v32st HY(0++,0),(r0+=r3) REP64`; the
apparatus is in `examples-on-real-hardware/vpu-probe/`, and
`tests/vpu_isa.rs` pins one whole run of it against the model. The encodings
themselves come from `binutils-vc4` (see above); what the encodings *do* comes
from the board.

The file is modelled in `src/vpu/vrf.rs`. Which instructions execute is decided
in `VecInsn::executable`, by field, not by whole-word template: every field of
the memory encoding has an established meaning now. What it still refuses is
`SETF` on a transfer, a `*` on a slot, and the five lane predicates the
firmware does not pin down.

| form | example | what it does here |
|---|---|---|
| `v<w>{ld,st} <reg>[++][+rA],(rB+off[+=rI]) [REP n]` | `v32ld HY(0,0)++,(r1+=r4) REP r0` | 16 elements between the register file and memory, `n` times, stepping the address by `rI` and (with `++`) the register by one. `off` is a byte displacement, `+rA` an element offset into the register. `rB` is **not** written back |
| `v<w>mov <reg>[++],rN` / `,#imm` `[REP n]` | `v32mov HY(0,0),r1` | broadcast a scalar or a 6-bit immediate over the 16 lanes; the 80-bit form repeats it down the rows, which is how the boot ROM clears memory |
| `v<w>bitplanes -,rN SETF` | `08 f4 38 e0 c0 03` | one flag per lane, holding that lane's bit of `rN` |
| `vgetacc[s16\|s32] <d>,<a>,<b>` | `00 f3 23 82 38 e2` | each lane's accumulator, shifted right by `b & 31`; `A` is read for nothing, and the suffix clamps the value into a signed 16- or 32-bit range |
| `v8ld -,(rN)` | `00 f0 38 e0 80 03` | reads 16 bytes at `rN` and discards them; no register changes |
| `v16mov -,rN SUM{U,S} rK` | `00 fc 38 e0 80 03 c0 f3 00 12` | `rK = 16 * rN`, sign- or zero-extended to the lane width, and the scalar N/Z flags follow |

Each lane carries a zero, a negative and a carry flag, and an ALU op with
`SETF` writes them: zero and negative from the result at the operation's width
— before it is narrowed into the destination, and after a saturating op has
clamped it — and the carry only from the ops that have one. An addition
carries out of the operation's width, a subtraction borrows, a saturating op
answers whether it clamped, `min`/`max` answer whether B was the operand
chosen, and a shift answers the last bit to leave the element, its index taken
modulo the width, so a count of zero reads the bit at the far end. Everything
else — the logical ops, the shuffles, `dist`, `count`, `msb`, `brev`, `clip`,
`sign`, `mov`, `mull` — leaves the carry exactly as it found it, and so does a
**transfer**: a load or a store with `SETF` set writes no flag at all. The
eight predicates `ALL`, `NONE`, `IFZ`, `IFNZ`, `IFN`, `IFNN`, `IFC`, `IFNC`
read them back.

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

Most of the ALU executes now, each op measured lane by lane against vectors of
edge cases and pinned in `tests/vpu_isa.rs`: `mov`, `bitplanes`, the four
shuffles `even`/`odd`/`interl`/`interh`, `brev`, `ror`, the shifts
`shl`/`shls`/`lsr`/`asr` and the signed-count `signshl`/`signasl`/`signasls`,
the logical ops, `count`, `msb`, `min`/`max`, `dist`/`dists`, `clip`, `sign`,
`testmag`, the adds and subtracts with their saturating and carry-in forms,
and the multiplies. `even` and `odd` pack A's alternate elements into the low
eight lanes and B's into the high eight; `clip` is `a` clamped into `0 ..= b`;
`sign` is `b + signum(a)` and the `v32` form of the sub-op beside it is
`b * signum(a)`; `count` is `popcount(a) + popcount(b)`; `msb` is the index of
the highest bit set in either operand; `testmag` answers whether `|a| >= b`;
`brev` reverses the low `n` bits of `a`, `n` being `b`'s low bits, or the whole
operation width when they are zero; and `bitplanes` **transposes** the lanes'
bits — lane `i` of the result is the word whose bit `j` is bit `i` of lane `j`
of B, which with a scalar B is the one-flag-per-bit form the firmware uses.

Some sub-ops mean one thing at one width and write a lane of **zeros** at the
other: `count`, `testmag` and `bitplanes` compute at `v16` and write zeros at
`v32`, and sub-op 30 is the other way round. Sub-ops 13, 22, 23 and 44–47
write zeros at both. That is a write, not a skip — measured over a destination
preset to all-ones.

The **accumulator** is one per lane and wider than an element. `CLRA` clears
it, the result is added to it — shifted left by sixteen in the `...H` forms,
which is what makes them the "high half" — read signed or unsigned as `SIGN`
says, and `WBA` makes the destination take the accumulator rather than the raw
result (shifted back down by sixteen, and clamped, for `...H`). `SUB` is not a
subtracting accumulate at all: it leaves the accumulator alone and hands the
destination `accumulator - result`. `vgetacc` reads the accumulator out,
shifted right by `b & 31`. A dash destination discards the result and keeps
only the accumulator's effect, which is how the codec code's
multiply-accumulate chains are written.

The **scalar result unit** writes an aggregate of the sixteen lane results to
`r0`..`r7`: `SUMU` and `SUMS` add them up unsigned and signed, `MAX` answers
the largest signed, and `IMIN`/`IMAX` answer the index of the smallest and of
the largest. A lane predicate applies to the aggregate too — the lanes it
masks off contribute nothing.

What still faults, and why:

| instructions | reason |
|---|---|
| 2113 | memory sub-ops beyond `vld`/`vst`/`vgetacc` |
| 280 | `SETF` on an op whose carry was not measured |
| 91 | a multiply with the `L` bit set, which selects another family |
| 41 | an accumulator modifier without `ENA` |
| 32 | multiply sub-op 63 |
| 24 | the rest: a register wider than the operation, an unsigned `SUB` in the high half |

Of the 15180 vector instructions in `start4.elf`'s `.text`, 12649 execute.

Much of what is left is not code at all: the pages between `0x0ec24000` and
`0x0ec2e000` disassemble as vector instructions but decode as data — a linear
sweep of `.text` cannot tell a jump table from an instruction, and those pages
are where most of the remaining encodings live.

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
