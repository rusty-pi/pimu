<!-- generated from isa/vpu.toml by `cargo run -- spec-docs --update` – do not edit -->

# VC4 VPU — instruction set reference

The **VPU** is the processor that runs `bootcode` and `start4.elf` on a
BCM2711 — a dual-core scalar-plus-vector machine, and the first thing the chip
executes. It is a **VC4** core: the same instruction set as the BCM2835's,
which is why a VideoCore IV-era binutils port disassembles a Pi 4 firmware
image instruction for instruction. The chip's *3D* engine is a different and
newer thing — V3D 4.2, "VideoCore VI" — and nothing here describes it.

Broadcom publishes nothing about the VPU, so every statement here carries
the evidence it rests on, in the same form as the peripheral specs under
[`docs/periph/`](periph/README.md): the kind of source, how much it is worth,
and the reference.

| kind | what it means |
|---|---|
| `measured` | run on a Raspberry Pi 4B d03115 through the firmware's `EXECUTE_CODE` mailbox tag; the probe that did it is named |
| `decompile` | read out of `start4.elf` — an address, or the `binutils-vc4` opcode tables |
| `trace` | the model boots the real firmware, and this is what makes the boot come out right |
| `inferred` | a guess, with the reasoning |

The model this describes is in [`src/vpu/`](../src/vpu/); the measurements are
replayed against it in [`tests/vpu_isa.rs`](../tests/vpu_isa.rs).

## Instruction length

Length comes from the first 16-bit parcel alone, read little-endian. Parcels
are little-endian individually; a 32-bit instruction is `p0, p1`, a 48-bit one
is stored `p0, p2, p1` — the middle parcel in memory is the *last* operand
parcel — and an 80-bit one is five parcels in order.

| First parcel | Bytes | Class | Source |
|---|---|---|---|
| `0x0000..=0x7FFF` | 2 | scalar 16-bit | trace: `src/vpu/length.rs`; every firmware boot |
| `0x8000..=0xDFFF` | 4 | scalar 32-bit | trace: `src/vpu/length.rs`; every firmware boot |
| `0xE000..=0xEFFF` | 6 | scalar 48-bit | trace: `src/vpu/length.rs`; every firmware boot |
| `0xF000..=0xF7FF` | 6 | vector 48-bit | decompile: `binutils-vc4` opcode tables |
| `0xF800..=0xFFFF` | 10 | vector 80-bit | decompile: `binutils-vc4` opcode tables |

## Registers and flags

| Register | Role | Source |
|---|---|---|
| `r0`–`r31` | general purpose | decompile: `binutils-vc4` opcode tables |
| `r24` | `gp`: the dedicated base of the 16-bit `ld`/`st (r24+imm)` forms | inferred: the community ABI, and start4 uses it as one |
| `r25` | `sp` | decompile: the 16-bit `add sp,#imm` form encodes destination 25 |
| `r26` | `lr`: `bl` and `jl` write the return address here | decompile: ThreadX's context save/restore, `0x3EC3FA34` and `0x3EC40040` |
| `r30` | `sr` *in the encodings* | decompile: the exception path saves and restores it, interrupt-enable bit 30 and all |
| `r31` | `pc` *in the encodings* | decompile: `lea rd,(rN+imm16)` reads `N == 31` as the program counter |

Flags are N, Z, C, V — but **`C` is borrow on a subtraction**, so `cs` means
unsigned-below (`lo`) and `cc` unsigned-higher-or-same (`hs`), the opposite way
round from ARM (`Flags::test`, `src/vpu/reg.rs`). The model keeps `pc` and the
flags in their own fields and folds the flags into the low nibble of `r30` only
when an exception saves it.

Sources:

- trace (high): a firmware boot diverges from the golden log if the polarity is ARM's

## Flag-setting policy

`cmp`, `cmn` and `btest` set flags and discard their result; the explicit
flag-setting triadics `adds`, `subs` and `shls` (sub-ops `0x28..=0x2a`) set
flags and write; `fcmp` sets them. Everything else writes its result and leaves
the flags alone.

This is the one load-bearing assumption in the scalar core.

Sources:

- inferred (medium): a whole firmware boot and a Linux boot run on it without a golden-log divergence — _evidence, not proof: an instruction whose flags nothing reads before the next `cmp` looks the same either way_

## Scalar instruction set

The ALU tables are complete: all 32 `p` entries, all 16 `q` entries, the `f`
float table, and every triadic sub-op through `0x38`. Triadic `0x39..=0x3F` is
the only gap, and no firmware this model boots has executed one.

| Width | Forms | Source |
|---|---|---|
| 16-bit | `nop`, `bkpt`, `sleep`, `rti`, `ei`/`di`, `swi`, `version`, `switch`/`switch.b`, `b`/`bl <reg>`, `b<cond>` (7-bit), `ld`/`st (sp+imm)`, `ld`/`st{w} (rs)`, `ld`/`st (rs+imm4)`, `add sp,#imm`, `lea rd,(sp+imm)`, `ldm`/`stm`, `p`-table ALU reg/reg, `q`-table ALU reg/imm5 | decompile: `binutils-vc4` opcode tables; trace: `src/vpu/decode.rs`; a firmware boot exercises all of them |
| 32-bit | `b<cond>`/`bl` (27-bit), `addcmpb`, ALU imm16, `lea rd,(rN+imm16)`, `lea rd,(pc+imm16)`, triadic ALU (`mul`, `div`, `mulhd`, `count`, `clamp16`, `addscale`, `subscale`), scalar float ALU and the `0xCA00` convert block, `mov p<n>,r<n>`, and the load/store block | decompile: `binutils-vc4` opcode tables; trace: `src/vpu/decode.rs`; a firmware boot exercises all of them |
| 48-bit | `j`/`jl`/`b`/`bl <abs32>`, `lea rd,(pc+off32)`, `ld`/`st` with a 27-bit offset, ALU with a 32-bit immediate | decompile: `binutils-vc4` opcode tables; trace: `src/vpu/decode.rs` |

## Three encodings that are easy to get backwards

| Encoding | The trap | Source |
|---|---|---|
| `switch` table | entries are **signed** halfword displacements from just past the 2-byte instruction | decompile: a handler defined before the `switch` is reached through a negative entry — _read unsigned, the firmware lands mid-instruction; fixed 2026-09-07_ |
| `ldm` / `stm` | the **highest** register sits at the **lowest** address, `lr` in the top word | decompile: ThreadX's interrupt frame: `0x3EC3FA34` pushes `{r6-r15}` and `{r16-r23}` over the stub's `{r0-r5, lr}`, and `_tx_thread_schedule` (`0x3EC40040`) undoes all three in order — _fixed 2026-09-10_ |
| `ww = 11` with the store bit | there is no signed store, so the slot is **`ldsb`** | decompile: mbedtls' `ecp_mod_p256` loads a `signed char` carry at `sp+7` and branches on its sign — _as a store, the reduction loop never terminates; fixed 2026-09-12_ |

## Vector register file

The file is 64 rows of 64 bytes, and a row is **sixteen lanes of four bytes**,
not sixty-four bytes in a line. Element `e` of a register whose elements are
`w` bytes wide sits at byte `(e & 15) * 4 + (e >> 4) * w` of its row: the lane
is the element's low four bits, and the rest of the index picks the sub-field
inside that lane. Consecutive elements are four bytes apart, interleaved.

A register is always sixteen elements. A **horizontal** one takes them along
one row from element `e0`; a **vertical** one takes the same element of sixteen
consecutive rows, from the 16-aligned band its coordinate names.

| Register width | Elements per row | Within a lane | Source |
|---|---|---|---|
| 8-bit | 64 | four elements, at bytes 0, 1, 2, 3 of the lane | measured: `probes/layout.s`, `probes/mix.s` |
| 16-bit | 32 | two elements, at byte 0 and byte 2 | measured: `probes/layout.s` |
| 32-bit | 16 | one element, the whole lane | measured: `probes/layout.s` |
| vertical | 16 rows | the same element of sixteen consecutive rows | measured: `probes/vert.s`, `probes/vert3.s` |

Sources:

- measured (high): `v8ld H(0,0),(r1)` over a page of ascending bytes lands them at columns 0, 4, 8 … of the row

## Slot descriptors

Each instruction names three operand slots — D, A and B — and each spells a
window into the file.

| Field | Meaning | Source |
|---|---|---|
| type nibble | the **register's** element width and direction: `H`/`V` 8-bit, `HX`/`VX` 16-bit, `HY`/`VY` 32-bit; odd nibbles are vertical, 14 and 15 are the dash | measured: `probes/layout.s`, `probes/conv.s` |
| `y` | the row (horizontal), or the 16-row band (vertical) | measured: `probes/vert3.s` |
| `e0` | the first element, `band * 16 + fine`, counted **in elements** — which is why the byte coordinate objdump prints and the element index part company above byte elements | measured: `probes/vx46.s`, `probes/layout.s` |
| `++` | post-increment: steps the row horizontally, the element vertically | measured: `probes/vinc.s` |
| `+rN` | adds a scalar to the element index, in elements, wrapping within the row | measured: `probes/pa48.s` |
| `*` | **no effect** on the register file or memory — on any slot, under `REP`, on a load or a store | measured: `probes/star.s` |
| dash in B | names a scalar register instead, with a signed displacement in the 80-bit encodings | decompile: `binutils-vc4` opcode tables; measured: `probes/disp.s` |
| dash in A | an operand of **zeros** for an ALU op; ignored altogether by a load | measured: `probes/alu6.s`, `probes/ldodd.s` |
| dash in D | discards the result — the load still reads its bytes | measured: `probes/ldodd.s`; decompile: `FUN_0edc9e20`, the vector-unit read fence |

## Operand width conversion

The operation has a width of its own (`v8`, `v16`, `v32`) and the registers
have theirs. Two thirds of the vector ALU instructions in `start4.elf` name
registers **narrower** than the operation, and the unit converts.

| Direction | Rule | Source |
|---|---|---|
| source, byte register into a wider operation | zero-extend | measured: `probes/wmix.s`, `probes/wmix2.s` |
| source, halfword register into a 32-bit operation | **sign**-extend | measured: `probes/wmix2.s` |
| result into a narrower destination | truncate | measured: `probes/wmix.s` |
| result into a narrower destination, saturating op | clamp into what that element holds: `0..=0xff` for a byte register, signed for a wider one | measured: `probes/wmix.s`, `probes/wmix2.s` |
| memory transfer, either direction | narrowing truncates, widening zero-extends | measured: `probes/conv.s` |
| register **wider** than the operation | refused: `binutils-vc4` cannot spell one, and `start4.elf` has thirteen, all in data | decompile: `binutils-vc4` opcode tables |
| shift, rotate or `brev` count | taken in the **operation's** width: five bits of B for `v32`, four for `v16` | measured: `probes/wmix3.s`, `probes/setf5.s` |

## Addressing

| Element | Rule | Source |
|---|---|---|
| displacement | a plain **byte** offset, at every width | measured: `probes/disp.s` |
| `+=rN` (80-bit forms) | added to the address after each repetition | decompile: VC4 libc's `memcpy` (`0x3EDA28D6`) advances by `r0 * 64` afterwards |
| base register | never written back | measured: `probes/mix.s` |
| load straddling a 16-byte block | **wraps inside the block**: `v32ld HY(0,0),(r1+13)` over ascending bytes reads `0e 0f 10 01` | measured: `probes/wrap.s`, `probes/wrap2.s` |
| store straddling a 16-byte block | crosses normally | measured: `probes/wrap2.s` |
| B-position immediate | **signed**: six bits in the 48-bit encoding, sixteen in the 80-bit one — `v32mov HY(0,0),#0x20` fills every lane with `0xffffffe0` | measured: `probes/imm.s` |

## Field layout

`f-op15-10` selects the class: 60 and 62 are the memory class, 61 and 63 the
ALU class. The sub-op is `f-op9-5` (memory, 5 bits) or `f-op8-3` (ALU, 6 bits);
the width is `f-op4-3` (memory: 0/1/2 = 8/16/32) or `f-op9` (ALU: the `L` bit,
16 or 32). The three slots are composites:

```text
48-bit  D = op31-29 : op27-22                      (9 bits)
        A = op21-19 : op17-16 : op47-44            (9 bits)
        B = op41-39 : op37-32                      (9 bits)
80-bit  D = op63-58 : op31-22                     (16 bits)
        A = op51-48 : op57-52 : op21-16 : op47-44 (20 bits)
        B = op69-64 : op41-32                     (16 bits)
```

The 48-bit composites carry no direction bit, no addend and no modifiers:
direction is `op28` for all three slots at once, and a `+rN` addend is one
presence bit per slot (`op43` / `op18` / `op38`) against the shared register
number in `op2-0`. Bit numbering is cgen's — 16-bit parcel in **memory** order
— bridged to the model's `raw` by `decode::cg`.

Sources:

- decompile (high): `binutils-vc4` opcode tables
- decompile (high): 15054 of the 15180 vector words in `start4.elf`'s `.text` disassemble identically in this model and in `binutils-vc4`'s objdump — _the other 126 are ones objdump prints as a raw `vec48`/`vec80`, having no form for them_

## Memory-class sub-ops

The gather and the scatter scale their index by the operation's element width
and take the ordinary base-plus-displacement address.

| Sub-op | Mnemonic | What it does | Source |
|---|---|---|---|
| 0 | `ld` | 16 elements between memory and the file, one per lane, at `base + disp` | measured: `probes/mix.s`, `probes/conv.s` |
| 1 | `lookupm` | gather: each lane reads element `acc >> 16` of the table at the address | measured: `probes/mem6.s`, `probes/mem9.s` |
| 2 | `lookupml` | gather indexed by `acc & 0xffff` | measured: `probes/mem5.s`, `probes/mem9.s` |
| 3 | `mem03` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 4 | `st` | the transfer the other way | measured: `probes/mix.s` |
| 5 | `indexwritem` | scatter: each lane writes its element at index `acc >> 16` | measured: `probes/mem7.s`, `probes/mem9.s` |
| 6 | `indexwriteml` | scatter indexed by `acc & 0xffff` | measured: `probes/mem7.s`, `probes/mem9.s` |
| 7 | `mem07` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 8 | `memread` | three vector slots and no address operand; neither the accumulator, a two-register address nor the operands themselves explain what it reads | measured: `probes/memr.s`, `probes/memr2.s` |
| 9 | `memwrite` | ditto; its destination came back as A widened into the destination's elements | measured: `probes/memr.s` |
| 10 | `mem10` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 11 | `mem11` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 12 | `mem12` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 13 | `mem13` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 14 | `mem14` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 15 | `mem15` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 16 | `mem16` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 17 | `mem17` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 18 | `mem18` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 19 | `mem19` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 20 | `mem20` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 21 | `mem21` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 22 | `mem22` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 23 | `mem23` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 24 | `getacc` | each lane's accumulator, shifted right by `b & 31`; A is read for nothing. The width field picks the saturation, not an element size: `v8` plain, `v16` clamps into signed 32-bit, `v32` into signed 16-bit | measured: `probes/setf4.s`, `probes/setf5.s` |
| 25 | `mem25` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 26 | `mem26` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 27 | `mem27` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 28 | `mem28` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 29 | `mem29` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 30 | `mem30` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 31 | `mem31` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |

## ALU-class sub-ops

`a` and `b` are the operands after the width conversion above; `n` is `b`'s low
bits, as many as the operation's width needs. Some sub-ops mean one thing at
one width and write a lane of **zeros** at the other — measured over a
destination preset to all-ones, so they do write.

| Sub-op | Mnemonic | Result | Source |
|---|---|---|---|
| 0 | `mov` | `b` | measured: `probes/alu.s` |
| 1 | `bitplanes` | bit **transpose**: lane `i` gets the word whose bit `j` is bit `i` of lane `j` of B — with a scalar B, all-ones wherever B's bit `i` is set. Zeros at `v32` | measured: `probes/bp.s` |
| 2 | `even` | A's even elements into lanes 0–7, **B's** into lanes 8–15 | measured: `probes/alu.s` |
| 3 | `odd` | the odd elements, the same way | measured: `probes/alu.s` |
| 4 | `interl` | A and B alternating, from the low half | measured: `probes/alu.s` |
| 5 | `interh` | the same from the high half | measured: `probes/alu.s` |
| 6 | `brev` | `a`'s low `n` bits reversed — the whole operation width when `n` is zero | measured: `probes/alu2.s`, `probes/wmix3.s` |
| 7 | `ror` | rotate right by `n` | measured: `probes/alu2.s` |
| 8 | `shl` | left shift by `n` | measured: `probes/alu2.s` |
| 9 | `shls` | left shift, saturating into the destination | measured: `probes/alu2.s`, `probes/wmix3.s` |
| 10 | `lsr` | logical right shift | measured: `probes/alu2.s` |
| 11 | `asr` | arithmetic right shift | measured: `probes/alu2.s` |
| 12 | `signshl` | shift by a **signed, unmasked** count: left when `b` is positive, right when negative, zeros shifting in; a count past the width empties the element | measured: `probes/alu3.s`, `probes/alu4.s` |
| 13 | `op13` | writes a lane of zeros, at both widths | measured: `probes/alu4.s`, `probes/alu5.s` |
| 14 | `signasl` | the same with the sign shifting in | measured: `probes/alu4.s` |
| 15 | `signasls` | `signasl`, saturating | measured: `probes/alu4.s` |
| 16 | `and` | `a & b` | measured: `probes/alu.s` |
| 17 | `or` | `a \| b` | measured: `probes/alu.s` |
| 18 | `eor` | `a ^ b` | measured: `probes/alu.s` |
| 19 | `bic` | `a & !b` | measured: `probes/alu.s` |
| 20 | `count` | `popcount(a) + popcount(b)`; zeros at `v32` | measured: `probes/alu.s`, `probes/alu5.s` |
| 21 | `msb` | the index of the highest bit set in **either** operand; all-ones when neither has one | measured: `probes/wmix3.s` |
| 22 | `op22` | writes a lane of zeros, at both widths | measured: `probes/alu5.s` |
| 23 | `op23` | writes a lane of zeros, at both widths | measured: `probes/alu5.s` |
| 24 | `min` | the smaller, signed | measured: `probes/alu2.s` |
| 25 | `max` | the larger, signed | measured: `probes/alu2.s` |
| 26 | `dist` | `abs(a - b)`, wrapping | measured: `probes/alu2.s` |
| 27 | `dists` | `abs(a - b)`, saturating | measured: `probes/alu2.s` |
| 28 | `clip` | `a` clamped into `0 ..= b`, signed | measured: `probes/alu2.s` |
| 29 | `sign` | `b + signum(a)` | measured: `probes/alu2.s` |
| 30 | `clips` | `b * signum(a)`, `signum(0)` counting as `+1`; zeros at `v16` | measured: `probes/alu4.s`, `probes/alu5.s` |
| 31 | `testmag` | `1` where `abs(a) >= b`, else `0`; zeros at `v32` | measured: `probes/alu3.s`, `probes/alu4.s` |
| 32 | `add` | `a + b` | measured: `probes/alu2.s` |
| 33 | `adds` | `a + b`, saturating | measured: `probes/alu2.s` |
| 34 | `addc` | `a + b` **plus the lane's carry flag** | measured: `probes/alu3.s` |
| 35 | `addsc` | the same, saturating | measured: `probes/alu3.s` |
| 36 | `sub` | `a - b` | measured: `probes/alu2.s` |
| 37 | `subs` | `a - b`, saturating | measured: `probes/alu2.s` |
| 38 | `subc` | `a - b` minus the lane's carry flag | measured: `probes/alu3.s` |
| 39 | `subsc` | the same, saturating | measured: `probes/alu3.s` |
| 40 | `rsub` | `b - a` | measured: `probes/alu2.s` |
| 41 | `rsubs` | `b - a`, saturating | measured: `probes/alu2.s` |
| 42 | `rsubc` | `b - a` minus the lane's carry flag | measured: `probes/alu3.s` |
| 43 | `rsubsc` | the same, saturating | measured: `probes/alu3.s` |
| 44 | `op44` | writes a lane of zeros, at both widths | measured: `probes/alu3.s`, `probes/alu5.s` |
| 45 | `op45` | writes a lane of zeros, at both widths | measured: `probes/alu3.s`, `probes/alu5.s` |
| 46 | `op46` | writes a lane of zeros, at both widths | measured: `probes/alu3.s`, `probes/alu5.s` |
| 47 | `op47` | writes a lane of zeros, at both widths | measured: `probes/alu3.s`, `probes/alu5.s` |
| 48 | `mull` | the product's low half | measured: `probes/mul.s` |
| 49 | `mulls` | the product's low half, saturating | measured: `probes/mul.s` |
| 50 | `mulm` | the product shifted right by eight — a fixed-point multiply | measured: `probes/mul.s` |
| 51 | `mulms` | the same, saturating | measured: `probes/mul.s` |
| 52 | `mulhd.ss` | the product's high half, both operands signed — or, with the `L` bit, `vmul32.ss` | measured: `probes/mul.s`, `probes/mul32.s` |
| 53 | `mulhd.su` | the high half, A signed and B unsigned — with `L`, `vmul32.su` | measured: `probes/mul.s`, `probes/mul32.s` |
| 54 | `mulhd.us` | the high half, A unsigned and B signed — with `L`, `vmul32.us` | measured: `probes/mul.s`, `probes/mul32.s` |
| 55 | `mulhd.uu` | the high half, both unsigned — with `L`, `vmul32.uu` | measured: `probes/mul.s`, `probes/mul32.s` |
| 56 | `mulhn.ss` | the high half, rounded, both signed | measured: `probes/mul.s` |
| 57 | `mulhn.su` | the high half, rounded, A signed | measured: `probes/mul.s` |
| 58 | `mulhn.us` | the high half, rounded, B signed | measured: `probes/mul.s` |
| 59 | `mulhn.uu` | the high half, rounded, both unsigned | measured: `probes/mul.s` |
| 60 | `mulht.ss` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 61 | `mulht.su` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 62 | `op62` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |
| 63 | `op63` | — | decompile: `binutils-vc4` names the encoding; what it does is not established |

With the `L` bit set — a `v32` width on sub-ops 52–55 — the multiply group
becomes `vmul32.{ss,su,us,uu}`: a **16 × 16 into 32** multiply, taking the low
halfword of each operand and keeping the whole product. A multiply carries no
width of its own, so it works at the **widest** register it names and converts
the narrower ones into it.

Sources:

- measured (high): `probes/mul32.s`
- measured (high): `probes/alu6.s`

## Repetition and predication

`REP` field 0–6 means `1 << n` repetitions and field 7 means "the count is in
`r0`". Each repetition steps the address by `+=rN` and, with `++`, the register
by one. Each lane carries a zero, a negative and a carry flag; the predicate
field picks which lanes execute.

| Field | Mnemonic | Lanes | Source |
|---|---|---|---|
| 0 | `ALL` | every lane | decompile: `binutils-vc4` opcode tables |
| 1 | `NONE` | none | decompile: `binutils-vc4` opcode tables |
| 2 | `IFZ` | zero flag set | decompile: `memcpy`'s tail (`0x3EDA292C`) builds `~0 << n` and transfers under it; measured: `probes/setf.s` |
| 3 | `IFNZ` | zero flag clear | decompile: `memset`'s tail (`0x3EDA2B5E`) builds a band of set bits and stores under it; measured: `probes/setf.s` |
| 4 | `IFN` | negative flag set | measured: `probes/setf.s` |
| 5 | `IFNN` | negative flag clear | measured: `probes/setf.s` |
| 6 | `IFC` | carry flag set | measured: `probes/setf.s` |
| 7 | `IFNC` | carry flag clear | measured: `probes/setf.s` |

A masked-off lane touches nothing — no register, no memory, not even its own
flags — and contributes nothing to a scalar aggregate.

Sources:

- measured (high): `probes/noena.s`
- measured (high): `probes/bp.s`

## `SETF` — the lane flags

`SETF` on an ALU op leaves the flags holding that lane's result: **zero** and
**negative** from the result at the *operation's* width, after a saturating op
has clamped it. The **carry** comes only from the ops that make one. A
**transfer** with `SETF` writes no flag at all.

| Op | Carry | Source |
|---|---|---|
| `add`, `addc` | carry out of the operation's width | measured: `probes/setf.s`, `probes/setfc.s` |
| `sub`, `subc` | borrow | measured: `probes/setf.s`, `probes/setfc.s` |
| `rsub`, `rsubc` | borrow of `b - a` | measured: `probes/setf2.s`, `probes/setfc.s` |
| `adds`, `subs`, `rsubs`, `addsc`, `subsc`, `rsubsc`, `dists`, `shls`, `mulls` | whether the result was clamped | measured: `probes/setf2.s`, `probes/setf3.s`, `probes/setfc.s` |
| `min`, `max` | whether **B** was the operand chosen | measured: `probes/setf2.s`, `probes/setf3.s` |
| `shl` | the bit that fell off the top: bit `(width - n) & (width - 1)` of `a` | measured: `probes/setf2.s`, `probes/setf5.s` |
| `lsr`, `asr`, `ror` | the last bit to leave the bottom: bit `(n - 1) & (width - 1)` of `a` | measured: `probes/setf3.s`, `probes/setf5.s` |
| `signshl` | the same by the signed count; nothing when the count passes the width | measured: `probes/setfc.s` |
| `signasl`, `signasls` | the same going right, the sign when the count passes the width, nothing going left | measured: `probes/setfc.s` |
| `mulm`, `mulms` | a carry whose meaning did not fall out of the probe — `SETF` on them faults | measured: `probes/setfc.s` — the marks fit neither a clamp nor the discarded bits |
| everything else measured | **left exactly as it was** | measured: `probes/setf3.s`, `probes/setfc.s` |

`bitplanes` is the firmware's own producer: its lane result is that lane's bit
of B, so `IFZ` selects the lanes whose bit was 0.

## The accumulator

One per lane, wider than an element. The modifier field is
`SRU 0x40 | ENA 0x20 | HIGH 0x10 | SIGN 0x08 | CLRA 0x04 | WBA 0x02 | SUB 0x01`;
the `UACC` and `SACC` mnemonics set `ENA|WBA` together, which is why an
accumulate usually writes the accumulator out. A dash destination keeps only
the accumulator's effect — the shape the codec code's multiply-accumulate
chains are written in.

| Bit | Effect | Source |
|---|---|---|
| `CLRA` | clears the accumulator first — **even without `ENA`** | measured: `probes/noena.s` |
| `ENA` | accumulate at all; without it the destination takes the raw result | measured: `probes/noena.s` |
| `SIGN` | read the result signed (`SACC`) rather than unsigned (`UACC`) on the way in | measured: `probes/acc3.s`, `probes/acch.s` |
| `HIGH` | accumulate the result **shifted left by sixteen**; a write-back reads it back shifted down by sixteen, clamped into the destination's signed range | measured: `probes/acch.s` |
| `WBA` | the destination takes the accumulator rather than the raw result | measured: `probes/accmix.s`, `probes/wacc.s` |
| `SUB` | **not** a subtracting accumulate: the accumulator is left alone and the destination takes `accumulator - result` — `(acc - (result << 16)) >> 16` with `HIGH` | measured: `probes/usub.s` |
| `SUB` with `HIGH`, unsigned | matched neither the wrapped difference nor a clamped one, lane for lane — it faults | measured: `probes/usub.s` |

## The scalar result unit

Field bit `0x40` selects it; bits 3–5 pick the function and bits 0–2 the scalar
register (`r0`–`r7`). It writes an aggregate of the sixteen lane results, and a
lane predicate applies to the aggregate as well.

| Function | Result | Source |
|---|---|---|
| `SUMU` | the lanes added up, each read unsigned at the operation's width | measured: `probes/sru.s` |
| `SUMS` | the same, read signed | measured: `probes/sru.s` |
| `IMIN` | the index of the smallest lane — the first, on a tie | measured: `probes/sru.s`, `probes/sru2.s` |
| `IMAX` | the index of the largest — the last, on a tie | measured: `probes/sru.s`, `probes/sru2.s` |
| `MAX`, `max2`, `max4`, `max6` | the largest lane, signed; the three `maxN` spellings answered exactly what `MAX` did over every vector tried | measured: `probes/sru.s`, `probes/sru2.s` |

## What the model executes

`VecInsn::executable` decides, by field rather than by whole-word template.
Against the 15180 vector instructions a linear sweep of `start4.elf`'s `.text`
decodes, **13282 execute**.

What is left is mostly not instructions. Splitting it by whether the
instruction's 4 KiB page looks like code — 60% or more of its vector words
executable — puts about 190 in code and about 1700 in pages that disassemble as
vector instructions only because a linear sweep cannot tell a jump table from
one.

| Left in code pages | Reason | Source |
|---|---|---|
| ~57 | `memread`, whose operands the probes could not pin | measured: `probes/memr.s` |
| ~34 | `vld` forms whose remaining fields are unexplained | decompile: `binutils-vc4` spells them; the fields are not established |
| ~12 | memory sub-op 3 | decompile: `binutils-vc4` names it `mem03` and nothing more |
| ~11 | `vgetacc` with a dash destination | inferred: what a discarded accumulator read is for was not established |
| ~10 | `SETF` where the B slot is a scalar with a displacement | decompile: `binutils-vc4` prints `r2-1`; what the displacement does to a scalar operand is not established |
| ~9 | `memwrite` | measured: `probes/memr.s` |
| rest | one-offs: a register wider than the operation, an unsigned `SUB` in the high half, `mulm` under `SETF` | measured: `probes/usub.s`, `probes/setfc.s` |

None of it is reached on a firmware boot: `boot` stops on an unimplemented
instruction by default, and `boot-check testdata/boot/firmware-boot.toml`
passes.

## Outside the instruction set

No dual-issue pipeline. The MMU and the caches are flat: the four VC4 aliases
(`0x0`, `0x4000_0000`, `0x8000_0000`, `0xC000_0000`) fold onto one backing
store, so a line the firmware writes cached and never flushes reads back as the
new bytes here and the old ones on silicon. Two checks stand in for that rather
than modelling it, both off by default.

| Switch | What it reports | Source |
|---|---|---|
| `--check-coherency` | each read that would see stale bytes on hardware (`src/coherency.rs`) | inferred: the VC4 alias map; the check reports rather than models |
| `--check-alignment` | each scalar access the core could not do in one (`src/align.rs`) | decompile: GCC's VC4 port is `STRICT_ALIGNMENT` |

Machine side: exception and timer-IRQ delivery through the firmware's vector
table, and two VPU cores. Core 1 starts where `start4` writes its entry to
`IC1_WAKEUP` (`corectl` `+0x834`, `0x7E00_2834`), which on this bench happens
only on a boot that goes on to Linux.

## Where the model keeps each part

| Part | File | Source |
|---|---|---|
| instruction length | `src/vpu/length.rs` | trace: the model |
| decode, both classes | `src/vpu/decode.rs` | trace: the model |
| instruction shapes, slot descriptors, `executable` | `src/vpu/insn.rs` | trace: the model |
| execution, lane loops, flags | `src/vpu/exec.rs` | trace: the model |
| the register file | `src/vpu/vrf.rs` | trace: the model |
| the measurements, replayed | `tests/vpu_isa.rs` | trace: the model |
| the probes themselves | `examples-on-real-hardware/vpu-probe/` | measured: one `.s` file per question, with the findings in its `README.md` |

## How the measurements were taken

Everything marked `measured` was taken on a **Raspberry Pi 4B d03115** through
the firmware's property-mailbox `EXECUTE_CODE` tag (`0x00030010`), which calls
a function on the VPU and hands back `r0`. Each probe clears the register file,
runs the instructions under test, and dumps the file back out with
`v32st HY(0++,0),(r0+=r3) REP64` — 64 rows of 64 bytes, exactly one page — read
back through `/dev/vc-mem`.

Nothing in that directory runs in CI and nothing in the model depends on it:
the results are baked into `src/vpu/` and replayed in `tests/vpu_isa.rs`,
several of them as whole programs compared against the board's dump byte for
byte.

### Other sources

- Herman Hermitage, `videocoreiv.arch` and `videocore-disjs` —
  <https://github.com/hermanhermitage/videocoreiv>
- the vc4 binutils port — <https://github.com/poizan42/binutils-vc4>
- Mathias Gottschlag, `vctools` (BCM2835, unlicensed — read only) —
  <https://github.com/mgottschlag/vctools>
- `librerpi/rpi-open-firmware`, an open bootloader for the VPU
