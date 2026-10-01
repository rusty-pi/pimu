<!-- generated from isa/vpu.toml by `cargo run -- spec-docs --update` – do not edit -->

# VC4 VPU — instruction set reference

> **Disclaimer**
>
> This is an independent documentation project, put together from static
> analysis of published firmware images and from trial and error on real
> hardware. It is not sanctioned by, connected with or endorsed by Broadcom or
> Raspberry Pi Ltd., and no Broadcom document or material beyond the publicly
> available ones was used in making it. No copyrighted material is reproduced
> here.
>
> It is written for non-commercial use, in the expectation that it is useful to
> anyone trying to understand the processor their Raspberry Pi actually boots
> on. Everything in it is offered as a description of what one board did when
> asked, not as a specification: where the two disagree, the silicon is right.

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
| `manual` | named in Herman Hermitage's VideoCore IV Programmers Manual, and then measured here — never on its own |
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
| 8-bit | 64 | four elements, at bytes 0, 1, 2, 3 of the lane | measured: `vpu-probe/probes/layout.s`, `vpu-probe/probes/mix.s` |
| 16-bit | 32 | two elements, at byte 0 and byte 2 | measured: `vpu-probe/probes/layout.s` |
| 32-bit | 16 | one element, the whole lane | measured: `vpu-probe/probes/layout.s` |
| vertical | 16 rows | the same element of sixteen consecutive rows | measured: `vpu-probe/probes/vert.s`, `vpu-probe/probes/vert3.s` |

Sources:

- measured (high): `v8ld H(0,0),(r1)` over a page of ascending bytes lands them at columns 0, 4, 8 … of the row

## Slot descriptors

Each instruction names three operand slots — D, A and B — and each spells a
window into the file.

| Field | Meaning | Source |
|---|---|---|
| type nibble | the **register's** element width and direction: `H`/`V` 8-bit, `HX`/`VX` 16-bit, `HY`/`VY` 32-bit; odd nibbles are vertical, 14 and 15 are the dash | measured: `vpu-probe/probes/layout.s`, `vpu-probe/probes/conv.s` |
| `y` | the row (horizontal), or the 16-row band (vertical) | measured: `vpu-probe/probes/vert3.s` |
| `e0` | the first element, `band * 16 + fine`, counted **in elements** — which is why the byte coordinate objdump prints and the element index part company above byte elements | measured: `vpu-probe/probes/vx46.s`, `vpu-probe/probes/layout.s` |
| `++` | post-increment: steps the row horizontally, the element vertically | measured: `vpu-probe/probes/vinc.s` |
| `+rN` | adds a scalar to the element index, in elements, wrapping within the row | measured: `vpu-probe/probes/pa48.s` |
| `*` | **no effect** on the register file or memory — on any slot, under `REP`, on a load or a store | measured: `vpu-probe/probes/star.s` |
| dash in B | names a scalar register instead, with a signed displacement in the 80-bit encodings — the operand is `r<N> + disp`, plain addition | decompile: `binutils-vc4` opcode tables; measured: `vpu-probe/probes/disp.s`, `vpu-probe/probes/sdisp.s`: with `r2` = 100, `r2+0`, `r2-1`, `r2-2`, `r2+1` and `r2+100` reach the lanes as 100, 99, 98, 101 and 200. The assembler cannot spell the form, so the words are built by hand and checked against objdump |
| dash in D | the result is discarded. Any addend nibble, `*` or `++` it carries has nothing left to act on: the flags come out the same with none, with `+r0` and with `+r3`, measured with `vpu-probe/probes/dashd.s` | measured: `vpu-probe/probes/dashd.s`, three hand-built words the assembler cannot spell |
| dash in A | an operand of **zeros** for an ALU op; ignored altogether by a load | measured: `vpu-probe/probes/alu6.s`, `vpu-probe/probes/ldodd.s` |
| dash in D | discards the result — the load still reads its bytes | measured: `vpu-probe/probes/ldodd.s`; decompile: `FUN_0edc9e20`, the vector-unit read fence |

## Operand width conversion

The operation has a width of its own (`v8`, `v16`, `v32`) and the registers
have theirs. Two thirds of the vector ALU instructions in `start4.elf` name
registers **narrower** than the operation, and the unit converts — but it
converts the other way too, and the element is always addressed at the
**register's** own width, which is where it lives.

| Direction | Rule | Source |
|---|---|---|
| multiply, which carries no width of its own | the widest **source** it names, not the widest register: `vmulhdt.ss HY(4,0),HX(62,0),HX(63,0)` is a 16-bit multiply whose product is written into a 32-bit register. With the `L` bit — a `v32` spelling — the width is 32 whatever the registers say, and a multiply naming no source register at all has a zero product at any width | measured: `vpu-probe/probes/setfmul.s` |
| ALU source, a register **wider** than the operation | read at the operation's width — the element's low half. `v16or HX(0,0),HY(20,0),0`, the same with the wide register in B, `v16mov` and `v16add` all answer what the `HX(20,0)` control answers, and `v16adds`, `v16shl` and `v16subs` match theirs lane for lane | measured: `vpu-probe/probes/wide1.s`, `vpu-probe/probes/wide2.s` |
| ALU result, into a destination **wider** than the operation | sign-extended, not zero-extended: `v16or HY(2,0),HX(20,0),0` answers `0xffffffff` where the halfword is `0xffff`. The truncation on the source side is at the **read**, so `v16or HY(0,0),HY(20,0),0` — wide on both sides — answers the low halfword sign-extended, not the 32-bit source untouched | measured: `vpu-probe/probes/wide1.s`, `vpu-probe/probes/wide2.s` |
| source, byte register into a wider operation | zero-extend | measured: `vpu-probe/probes/wmix.s`, `vpu-probe/probes/wmix2.s` |
| source, halfword register into a 32-bit operation | **sign**-extend | measured: `vpu-probe/probes/wmix2.s` |
| result into a narrower destination | truncate | measured: `vpu-probe/probes/wmix.s` |
| result into a narrower destination, saturating op | clamp into what that element holds: `0..=0xff` for a byte register, signed for a wider one | measured: `vpu-probe/probes/wmix.s`, `vpu-probe/probes/wmix2.s` |
| memory transfer, either direction | narrowing truncates, widening zero-extends | measured: `vpu-probe/probes/conv.s` |
| register **wider** than the operation | refused: `binutils-vc4` cannot spell one, and `start4.elf` has thirteen, all in data | decompile: `binutils-vc4` opcode tables |
| shift, rotate or `brev` count | taken in the **operation's** width: five bits of B for `v32`, four for `v16` | measured: `vpu-probe/probes/wmix3.s`, `vpu-probe/probes/setf5.s` |

## Addressing

| Element | Rule | Source |
|---|---|---|
| displacement | a plain **byte** offset, at every width | measured: `vpu-probe/probes/disp.s` |
| `+=rN` (80-bit forms) | added to the address after each repetition | decompile: VC4 libc's `memcpy` (`0x3EDA28D6`) advances by `r0 * 64` afterwards |
| base register | never written back | measured: `vpu-probe/probes/mix.s` |
| load straddling a 16-byte block | **wraps inside the block**: `v32ld HY(0,0),(r1+13)` over ascending bytes reads `0e 0f 10 01` | measured: `vpu-probe/probes/wrap.s`, `vpu-probe/probes/wrap2.s` |
| store straddling a 16-byte block | crosses normally | measured: `vpu-probe/probes/wrap2.s` |
| B-position immediate | **signed**: six bits in the 48-bit encoding, sixteen in the 80-bit one — `v32mov HY(0,0),#0x20` fills every lane with `0xffffffe0` | measured: `vpu-probe/probes/imm.s` |

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
- decompile (high): the whole of `start4.elf`'s `.text`, compared word for word against `binutils-vc4`'s objdump: 14390 of the 14650 vector instructions this decoder finds sit where objdump decodes a vector instruction too, and the spellings agree but for the ones objdump prints as a raw `vec48`/`vec80`, having no form for them — _the remainder is the two linear sweeps drifting apart inside data, where a different length read leads into a different stream_

## Memory-class sub-ops

A transfer that names **no address** — a `vst` or an `indexwritem` whose B
slot holds a vector, or a scatter with a dash source — writes nothing at all,
whatever its three slots hold:
not where its operands point, not at address 0 where the matching load reads,
and nowhere in the 64 KiB a probe compares byte for byte either side of it.

The gather and the scatter scale their index by the operation's element width
and take the ordinary base-plus-displacement address. `memread` and `memwrite`
address neither: they are the unit's own lookup table, 1 KiB of it, banked
sixteen ways so that each lane indexes its own 64 bytes.

The sub-ops with no name of their own — 3, 7, 10-23 and 25-31 — are
**unallocated**, and there is no use for them. Their names are not the
silicon's: `binutils-vc4` builds its mnemonic table from a CGEN enum over the
5-bit sub-op field, so the list has to be 32 entries long, and the twenty-two
Broadcom never documented were filled in from their own numbers —

```scheme
(define-normal-insn-enum
  insn-vecmemops "Vector memory ops" () VMEMOP_ f-op9-5
  (LD LOOKUPM LOOKUPML MEM03 ST INDEXWRITEM INDEXWRITEML MEM07
   MEMREAD MEMWRITE MEM10 MEM11 MEM12 MEM13 MEM14 MEM15
   MEM16 MEM17 MEM18 MEM19 MEM20 MEM21 MEM22 MEM23
   GETACC MEM25 MEM26 MEM27 MEM28 MEM29 MEM30 MEM31)
)
```

— which is where `mem31` comes from. The ALU class does the same, and that is
what `vop62.1` is. This page keeps those names so that it, the model's
disassembler and objdump all print the same thing.

What the silicon does with one is uniform: it **answers a lane of zeros** at
the operation's width and waits for nothing. It reads nothing at the address it is handed, leaves the
lookup table alone, and retires in microseconds. A blob whose *first*
instruction is one of them, with nothing vector before it at all, retires just
the same.

That last measurement is what corrected the earlier reading of these as
**fences** waiting on a write to the register file. Every probe behind that
reading spelled the B slot as a vector register, and in that shape seven of
them — 10, 16, 19 and 28-31 — stall the vector unit outright. Spelled instead
with a scalar address, `v8mem10 H(0,0),H(20,0),(r4)`, all seven retire and
write their zero like the rest. The "fence" was the malformed operand, not the
sub-op, and "it retires in 2 ms when a load precedes it" was a property of the
probes rather than of the silicon.

The zero is easy to mistake for a load, which is the other thing that went
wrong: a `v8ld` from an address holding zeros leaves a destination preset to
all-ones reading `00ffffff` in every lane, and so does one of these. Handed a
page of 16 known-good pointers, a plain `v8ld` answers the pointer bytes and
`v8mem10` still answers zero — measured side by side, which is what settles it.

That zero is a default, not an instruction — the result path with no sub-unit
claiming it — and two things say so. Executing one leaves the vector memory
unit in a state the firmware's own next memory operation cannot get past, and
nothing anyone shipped behaves that way. And nothing executes one: every hit
in `start4.elf` is a linear sweep landing in a jump table or a constant pool,
which is the same reason 72 of the words below are ones objdump refuses too.

Two things they do not share:

- **3** and **7** leave the firmware running. Every other one kills it: the
  board answers no further mailbox call and has to be rebooted, whether or not
  the instruction itself retired. A board wedged this way does not always come
  back from a reboot — three of them needed a power cycle.
- with a **vector register** in the B slot, an encoding `binutils-vc4` will
  print, sub-ops 10, 16, 19 and 28-31 stall the unit instead of retiring. The
  rest write their zero whatever B holds.

In that vector-B shape the stall also has a pattern, measured before the shape
itself was understood and left here as data rather than as a rule. Each row is
a board that answered the mailbox the moment before:

| what runs | what gets past it |
|---|---|
| load, sub-op | the sub-op, in 2 ms |
| load, sub-op, sub-op | the first only |
| load, `v32mov`, sub-op, sub-op | both, in 1 ms |
| load, `v32mov`, `v32mov`, sub-op, sub-op | both |
| load, `v32mov`, `v32mov`, sub-op x3 | none of them |
| load, `v32mov`, sub-op x4 | none |
| load, load, sub-op, sub-op | none |
| load, sub-op, load, sub-op | none |
| load, `v16add`, sub-op, sub-op | both, in 2 ms |
| load, `v8st`, sub-op, sub-op | none |
| load, scalar `mov`, sub-op, scalar `mov`, sub-op | none |

Nothing is corrupted by any of it — 96 MB of firmware memory diffed across a
run moved only counters and timestamps — the status register is unchanged, and
a deliberate `bkpt` in the same place behaves nothing like it.

All of them are carried out here as the zero write, at every B shape: the
model has no vector unit to stall, and the stall is a property of an operand
the firmware would have to be executing as data to reach.

| Sub-op | Mnemonic | What it does | Source |
|---|---|---|---|
| 0 | `ld` | 16 elements between memory and the file, one per lane, at `base + disp` | measured: `vpu-probe/probes/mix.s`, `vpu-probe/probes/conv.s` |
| 1 | `lookupm` | gather: each lane reads element `acc >> 16` of the table at the address. An address-less form — `(r63)`, or a vector in the B slot — gathers from **zero** — the address is then just the displacement — the A slot is read for nothing, and a dash destination reads and discards, which is how `start4.elf` spells every one of them | measured: `vpu-probe/probes/mem6.s`, `vpu-probe/probes/mem9.s`, `vpu-probe/probes/r63.s`, `vpu-probe/probes/r63b.s`, `vpu-probe/probes/r63c.s`: with the accumulators cleared, a gather off `(r63)` hands every lane the byte at address 0; junk in the A slot changes nothing; a dash destination leaves a witness register untouched |
| 2 | `lookupml` | gather indexed by `acc & 0xffff` | measured: `vpu-probe/probes/mem5.s`, `vpu-probe/probes/mem9.s` |
| 3 | `mem03` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The board goes on running | measured: `vpu-probe/probes/hbare03.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 4 | `st` | the transfer the other way | measured: `vpu-probe/probes/mix.s` |
| 5 | `indexwritem` | scatter: each lane writes its element at index `acc >> 16` | measured: `vpu-probe/probes/mem7.s`, `vpu-probe/probes/mem9.s` |
| 6 | `indexwriteml` | scatter indexed by `acc & 0xffff` | measured: `vpu-probe/probes/mem7.s`, `vpu-probe/probes/mem9.s` |
| 7 | `mem07` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The board goes on running | measured: `vpu-probe/probes/m07.s`, `vpu-probe/probes/addr07.s`: over a destination preset to all-ones, with operands that are a valid bus address, with zeros, and with junk — the 32 bytes at the address it was handed were unchanged afterwards, and three lookup-table indices read the same before and after |
| 8 | `memread` | `readlut`: each lane reads its own 64-byte region of the unit's 1 KiB table at `b * width`, A unused. B is a vector slot, a scalar register or an immediate; a scalar reaches every lane alike | measured: `vpu-probe/probes/lut.s`: a `v8memwrite` then a `v8memread` over the same indices hands every lane its own value back — seven lanes sharing index `0xff` and each keeping its own value is what says the table is banked — and the `v16` pair round-trips at twice the index; manual: the VideoCore IV Programmers Manual names sub-ops 8 and 9 `readlut`/`writelut` over a 1 KB table |
| 9 | `memwrite` | `writelut`: puts A at that index, and hands the destination the same value | measured: `vpu-probe/probes/lut.s`, `vpu-probe/probes/lut2.s`: a scalar write at 3 and a vector index of threes reach the same byte, and a `v16` write at 3 leaves byte 3 alone — the index scales by the element width whichever way it is spelled |
| 10 | `mem10` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction, *provided* its B slot is a scalar address: spelled with a **vector register** there it stalls the unit outright. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/hgat10.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; `vpu-probe/probes/f10.s`, which spells B as a vector register, never returns; decompile: `binutils-vc4` names the encoding |
| 11 | `mem11` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f11.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 12 | `mem12` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f12.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 13 | `mem13` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f13.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 14 | `mem14` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f14.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 15 | `mem15` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f15.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 16 | `mem16` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction, *provided* its B slot is a scalar address: spelled with a **vector register** there it stalls the unit outright. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/hgat16.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; `vpu-probe/probes/f16.s`, which spells B as a vector register, never returns; decompile: `binutils-vc4` names the encoding |
| 17 | `mem17` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f17.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 18 | `mem18` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f18.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 19 | `mem19` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction, *provided* its B slot is a scalar address: spelled with a **vector register** there it stalls the unit outright. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/hgat19.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; `vpu-probe/probes/f19.s`, which spells B as a vector register, never returns; decompile: `binutils-vc4` names the encoding |
| 20 | `mem20` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f20.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 21 | `mem21` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f21.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 22 | `mem22` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f22.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 23 | `mem23` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f23.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 24 | `getacc` | each lane's accumulator, shifted right by `b & 31`; A is read for nothing. The width field picks the saturation, not an element size: `v8` plain, `v16` clamps into signed 32-bit, `v32` into signed 16-bit. A **dash destination** keeps only the scalar result unit's aggregate, which is the form `start4.elf` uses | measured: `vpu-probe/probes/setf4.s`, `vpu-probe/probes/gacc.s`: over sixteen known accumulators `SUMS` and `SUMU` both answer their plain sum, `MAX` the largest, `IMIN`/`IMAX` an index, and the `B` shift applies before the aggregate. The lane values go in whole, not re-read at the operation's element width, `vpu-probe/probes/setf5.s` |
| 25 | `mem25` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f25.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 26 | `mem26` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f26.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 27 | `mem27` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/f27.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; decompile: `binutils-vc4` names the encoding |
| 28 | `mem28` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction, *provided* its B slot is a scalar address: spelled with a **vector register** there it stalls the unit outright. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/hgat28.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; `vpu-probe/probes/f28.s`, which spells B as a vector register, never returns; decompile: `binutils-vc4` names the encoding |
| 29 | `mem29` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction, *provided* its B slot is a scalar address: spelled with a **vector register** there it stalls the unit outright. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/hgat29.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; `vpu-probe/probes/f29.s`, which spells B as a vector register, never returns; decompile: `binutils-vc4` names the encoding |
| 30 | `mem30` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction, *provided* its B slot is a scalar address: spelled with a **vector register** there it stalls the unit outright. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/hgat30.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; `vpu-probe/probes/f30.s`, which spells B as a vector register, never returns; decompile: `binutils-vc4` names the encoding |
| 31 | `mem31` | **unallocated**; the unit answers a lane of **zeros** at the operation's width and waits for nothing — nothing read at the address it is handed, nothing changed in the lookup table, and it retires even as a blob's first instruction, *provided* its B slot is a scalar address: spelled with a **vector register** there it stalls the unit outright. The firmware is dead afterwards either way: the board answers no further mailbox call | measured: `vpu-probe/probes/hgat31.s` on a Raspberry Pi 4B d03115: over a destination preset to all-ones, with a page of sixteen known-good pointers at the address in its B slot, the element came back zero where a `v8ld` of the same page answers the pointer bytes; `vpu-probe/probes/f31.s`, which spells B as a vector register, never returns; decompile: `binutils-vc4` names the encoding |

## ALU-class sub-ops

`a` and `b` are the operands after the width conversion above; `n` is `b`'s low
bits, as many as the operation's width needs. Some sub-ops mean one thing at
one width and write a lane of **zeros** at the other — measured over a
destination preset to all-ones, so they do write.

| Sub-op | Mnemonic | Result | Source |
|---|---|---|---|
| 0 | `mov` | `b` | measured: `vpu-probe/probes/alu.s` |
| 1 | `bitplanes` | bit **transpose**: lane `i` gets the word whose bit `j` is bit `i` of lane `j` of B — with a scalar B, all-ones wherever B's bit `i` is set. Zeros at `v32` | measured: `vpu-probe/probes/bp.s` |
| 2 | `even` | A's even elements into lanes 0–7, **B's** into lanes 8–15 | measured: `vpu-probe/probes/alu.s` |
| 3 | `odd` | the odd elements, the same way | measured: `vpu-probe/probes/alu.s` |
| 4 | `interl` | A and B alternating, from the low half | measured: `vpu-probe/probes/alu.s` |
| 5 | `interh` | the same from the high half | measured: `vpu-probe/probes/alu.s` |
| 6 | `brev` | `a`'s low `n` bits reversed — the whole operation width when `n` is zero | measured: `vpu-probe/probes/alu2.s`, `vpu-probe/probes/wmix3.s` |
| 7 | `ror` | rotate right by `n` | measured: `vpu-probe/probes/alu2.s` |
| 8 | `shl` | left shift by `n` | measured: `vpu-probe/probes/alu2.s` |
| 9 | `shls` | left shift, saturating into the destination | measured: `vpu-probe/probes/alu2.s`, `vpu-probe/probes/wmix3.s` |
| 10 | `lsr` | logical right shift | measured: `vpu-probe/probes/alu2.s` |
| 11 | `asr` | arithmetic right shift | measured: `vpu-probe/probes/alu2.s` |
| 12 | `signshl` | shift by a **signed, unmasked** count: left when `b` is positive, right when negative, zeros shifting in; a count past the width empties the element | measured: `vpu-probe/probes/alu3.s`, `vpu-probe/probes/alu4.s` |
| 13 | `op13` | writes a lane of zeros, at both widths | measured: `vpu-probe/probes/alu4.s`, `vpu-probe/probes/alu5.s` |
| 14 | `signasl` | the same with the sign shifting in | measured: `vpu-probe/probes/alu4.s` |
| 15 | `signasls` | `signasl`, saturating | measured: `vpu-probe/probes/alu4.s` |
| 16 | `and` | `a & b` | measured: `vpu-probe/probes/alu.s` |
| 17 | `or` | `a \| b` | measured: `vpu-probe/probes/alu.s` |
| 18 | `eor` | `a ^ b` | measured: `vpu-probe/probes/alu.s` |
| 19 | `bic` | `a & !b` | measured: `vpu-probe/probes/alu.s` |
| 20 | `count` | `popcount(a) + popcount(b)`; zeros at `v32` | measured: `vpu-probe/probes/alu.s`, `vpu-probe/probes/alu5.s` |
| 21 | `msb` | the index of the highest bit set in **either** operand; all-ones when neither has one | measured: `vpu-probe/probes/wmix3.s` |
| 22 | `op22` | writes a lane of zeros, at both widths | measured: `vpu-probe/probes/alu5.s` |
| 23 | `op23` | writes a lane of zeros, at both widths | measured: `vpu-probe/probes/alu5.s` |
| 24 | `min` | the smaller, signed | measured: `vpu-probe/probes/alu2.s` |
| 25 | `max` | the larger, signed | measured: `vpu-probe/probes/alu2.s` |
| 26 | `dist` | `abs(a - b)`, wrapping | measured: `vpu-probe/probes/alu2.s` |
| 27 | `dists` | `abs(a - b)`, saturating | measured: `vpu-probe/probes/alu2.s` |
| 28 | `clip` | `a` clamped into `0 ..= b`, signed | measured: `vpu-probe/probes/alu2.s` |
| 29 | `sign` | `b + signum(a)` | measured: `vpu-probe/probes/alu2.s` |
| 30 | `clips` | `b * signum(a)`, `signum(0)` counting as `+1`; zeros at `v16` | measured: `vpu-probe/probes/alu4.s`, `vpu-probe/probes/alu5.s` |
| 31 | `testmag` | `1` where `abs(a) >= b`, else `0`; zeros at `v32` | measured: `vpu-probe/probes/alu3.s`, `vpu-probe/probes/alu4.s` |
| 32 | `add` | `a + b` | measured: `vpu-probe/probes/alu2.s` |
| 33 | `adds` | `a + b`, saturating | measured: `vpu-probe/probes/alu2.s` |
| 34 | `addc` | `a + b` **plus the lane's carry flag** | measured: `vpu-probe/probes/alu3.s` |
| 35 | `addsc` | the same, saturating | measured: `vpu-probe/probes/alu3.s` |
| 36 | `sub` | `a - b` | measured: `vpu-probe/probes/alu2.s` |
| 37 | `subs` | `a - b`, saturating | measured: `vpu-probe/probes/alu2.s` |
| 38 | `subc` | `a - b` minus the lane's carry flag | measured: `vpu-probe/probes/alu3.s` |
| 39 | `subsc` | the same, saturating | measured: `vpu-probe/probes/alu3.s` |
| 40 | `rsub` | `b - a` | measured: `vpu-probe/probes/alu2.s` |
| 41 | `rsubs` | `b - a`, saturating | measured: `vpu-probe/probes/alu2.s` |
| 42 | `rsubc` | `b - a` minus the lane's carry flag | measured: `vpu-probe/probes/alu3.s` |
| 43 | `rsubsc` | the same, saturating | measured: `vpu-probe/probes/alu3.s` |
| 44 | `op44` | writes a lane of zeros, at both widths | measured: `vpu-probe/probes/alu3.s`, `vpu-probe/probes/alu5.s` |
| 45 | `op45` | writes a lane of zeros, at both widths | measured: `vpu-probe/probes/alu3.s`, `vpu-probe/probes/alu5.s` |
| 46 | `op46` | writes a lane of zeros, at both widths | measured: `vpu-probe/probes/alu3.s`, `vpu-probe/probes/alu5.s` |
| 47 | `op47` | writes a lane of zeros, at both widths | measured: `vpu-probe/probes/alu3.s`, `vpu-probe/probes/alu5.s` |
| 48 | `mull` | the product's low half | measured: `vpu-probe/probes/mul.s` |
| 49 | `mulls` | the product's low half, saturating | measured: `vpu-probe/probes/mul.s` |
| 50 | `mulm` | the product shifted right by eight — a fixed-point multiply | measured: `vpu-probe/probes/mul.s` |
| 51 | `mulms` | the same, saturating | measured: `vpu-probe/probes/mul.s` |
| 52 | `mulhd.ss` | the product's high half, both operands signed — or, with the `L` bit, `vmul32.ss` | measured: `vpu-probe/probes/mul.s`, `vpu-probe/probes/mul32.s` |
| 53 | `mulhd.su` | the high half, A signed and B unsigned — with `L`, `vmul32.su` | measured: `vpu-probe/probes/mul.s`, `vpu-probe/probes/mul32.s` |
| 54 | `mulhd.us` | the high half, A unsigned and B signed — with `L`, `vmul32.us` | measured: `vpu-probe/probes/mul.s`, `vpu-probe/probes/mul32.s` |
| 55 | `mulhd.uu` | the high half, both unsigned — with `L`, `vmul32.uu` | measured: `vpu-probe/probes/mul.s`, `vpu-probe/probes/mul32.s` |
| 56 | `mulhn.ss` | the high half, rounded, both signed | measured: `vpu-probe/probes/mul.s` |
| 57 | `mulhn.su` | the high half, rounded, A signed | measured: `vpu-probe/probes/mul.s` |
| 58 | `mulhn.us` | the high half, rounded, B signed | measured: `vpu-probe/probes/mul.s` |
| 59 | `mulhn.uu` | the high half, rounded, both unsigned | measured: `vpu-probe/probes/mul.s` |
| 60 | `mulht.ss` | the product's high half **truncated** towards zero, not floored — both operands signed | measured: `vpu-probe/probes/mhdt.s`: `0x0ff0 * 0xfff1` — product −61200 — answers `0x0000` here and `0xffff` from `mulhd`; manual: the VideoCore IV Programmers Manual calls sub-ops 60 and 61 the round-to-zero high multiply |
| 61 | `mulht.su` | the product's high half **truncated** towards zero, not floored — A signed, B unsigned | measured: `vpu-probe/probes/mhdt.s`: `0x0ff0 * 0xfff1` — product −61200 — answers `0x0000` here and `0xffff` from `mulhd`; manual: the VideoCore IV Programmers Manual calls sub-ops 60 and 61 the round-to-zero high multiply |
| 62 | `op62` | writes a lane of zeros, at both widths | measured: `vpu-probe/probes/mhdt.s`, over a destination preset to all-ones |
| 63 | `op63` | writes a lane of zeros, at both widths | measured: `vpu-probe/probes/mhdt.s`, over a destination preset to all-ones |

With the `L` bit set — a `v32` width on sub-ops 52–55 — the multiply group
becomes `vmul32.{ss,su,us,uu}`: a **16 × 16 into 32** multiply, taking the low
halfword of each operand and keeping the whole product. A multiply carries no
width of its own, so it works at the **widest** register it names and converts
the narrower ones into it.

Sources:

- measured (high): `vpu-probe/probes/mul32.s`
- measured (high): `vpu-probe/probes/alu6.s`

## Repetition and predication

`REP` field 0–6 means `1 << n` repetitions and field 7 means "the count is in
`r0`". Each repetition steps the address by `+=rN` and, with `++`, the register
by one. Each lane carries a zero, a negative and a carry flag; the predicate
field picks which lanes execute.

| Field | Mnemonic | Lanes | Source |
|---|---|---|---|
| 0 | `ALL` | every lane | decompile: `binutils-vc4` opcode tables |
| 1 | `NONE` | none | decompile: `binutils-vc4` opcode tables |
| 2 | `IFZ` | zero flag set | decompile: `memcpy`'s tail (`0x3EDA292C`) builds `~0 << n` and transfers under it; measured: `vpu-probe/probes/setf.s` |
| 3 | `IFNZ` | zero flag clear | decompile: `memset`'s tail (`0x3EDA2B5E`) builds a band of set bits and stores under it; measured: `vpu-probe/probes/setf.s` |
| 4 | `IFN` | negative flag set | measured: `vpu-probe/probes/setf.s` |
| 5 | `IFNN` | negative flag clear | measured: `vpu-probe/probes/setf.s` |
| 6 | `IFC` | carry flag set | measured: `vpu-probe/probes/setf.s` |
| 7 | `IFNC` | carry flag clear | measured: `vpu-probe/probes/setf.s` |

A masked-off lane touches nothing — no register, no memory, not even its own
flags — and contributes nothing to a scalar aggregate.

Sources:

- measured (high): `vpu-probe/probes/noena.s`
- measured (high): `vpu-probe/probes/bp.s`

## `SETF` — the lane flags

`SETF` on an ALU op leaves the flags holding that lane's result: **zero** and
**negative** from the result at the *operation's* width, after a saturating op
has clamped it. The **carry** comes only from the ops that make one. A
**transfer** with `SETF` writes no flag at all.

| Op | Carry | Source |
|---|---|---|
| `add`, `addc` | carry out of the operation's width | measured: `vpu-probe/probes/setf.s`, `vpu-probe/probes/setfc.s` |
| `sub`, `subc` | borrow | measured: `vpu-probe/probes/setf.s`, `vpu-probe/probes/setfc.s` |
| `rsub`, `rsubc` | borrow of `b - a` | measured: `vpu-probe/probes/setf2.s`, `vpu-probe/probes/setfc.s` |
| `adds`, `subs`, `rsubs`, `addsc`, `subsc`, `rsubsc`, `dists`, `shls`, `mulls` | whether the result was clamped | measured: `vpu-probe/probes/setf2.s`, `vpu-probe/probes/setf3.s`, `vpu-probe/probes/setfc.s` |
| `min`, `max` | whether **B** was the operand chosen | measured: `vpu-probe/probes/setf2.s`, `vpu-probe/probes/setf3.s` |
| `shl` | the bit that fell off the top: bit `(width - n) & (width - 1)` of `a` | measured: `vpu-probe/probes/setf2.s`, `vpu-probe/probes/setf5.s` |
| `lsr`, `asr`, `ror` | the last bit to leave the bottom: bit `(n - 1) & (width - 1)` of `a` | measured: `vpu-probe/probes/setf3.s`, `vpu-probe/probes/setf5.s` |
| `signshl` | the same by the signed count; nothing when the count passes the width | measured: `vpu-probe/probes/setfc.s` |
| `signasl`, `signasls` | the same going right, the sign when the count passes the width, nothing going left | measured: `vpu-probe/probes/setfc.s` |
| `mulm`, `mulms` | a carry whose meaning did not fall out of the probe — `SETF` on them faults | measured: `vpu-probe/probes/setfc.s` — the marks fit neither a clamp nor the discarded bits |
| everything else measured | **left exactly as it was** | measured: `vpu-probe/probes/setf3.s`, `vpu-probe/probes/setfc.s` |

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
| `CLRA` | clears the accumulator first — **even without `ENA`** | measured: `vpu-probe/probes/noena.s` |
| `ENA` | accumulate at all; without it the destination takes the raw result | measured: `vpu-probe/probes/noena.s` |
| `ENA` without `WBA` | the destination takes `result + accumulator` — `result` taken off it with `SUB` — and the accumulator itself does not move: `UADD`/`USUB`, not an accumulate | measured: `vpu-probe/probes/mhdt.s`: `CLRA UACC(A)` then `UADD(B)` answers `A + B + A`, with `A` still in the accumulator afterwards; manual: the VideoCore IV Programmers Manual's names for the two forms |
| `SIGN` | read the result signed (`SACC`) rather than unsigned (`UACC`) on the way in | measured: `vpu-probe/probes/acc3.s`, `vpu-probe/probes/acch.s` |
| `HIGH` | accumulate the result **shifted left by sixteen**; a write-back reads it back shifted down by sixteen, clamped into the destination's signed range | measured: `vpu-probe/probes/acch.s` |
| `WBA` | the destination takes the accumulator rather than the raw result | measured: `vpu-probe/probes/accmix.s`, `vpu-probe/probes/wacc.s` |
| `SUB` | with `ENA` and no `WBA` — the `USUB` form — **not** a subtracting accumulate: the accumulator is left alone and the destination takes `accumulator - result`. Alongside `WBA` the accumulator takes `acc - (result << 16)` as well | measured: `vpu-probe/probes/usub.s`, read back with `vgetacc`, `vpu-probe/probes/acchu.s`: `vgetacc` after a `UACC HIGH SUB` reads `acc - (result << 16)`, lane for lane |
| `SUB` or no `SUB`, with `HIGH` | the destination is the accumulator's **own** high half against the result — `(acc >> 16) - result`, saturated into the operation's width — taken *before* the update, not read back out of the accumulator afterwards. The two agree until the low half borrows: with the high half zero and a result of `0xffff`, the unsigned form answers `-32768`, the clamp, where the accumulator afterwards reads `1`. Both polarities follow it, and the saturation is at the **operation's** width even when the destination is wider | measured: `vpu-probe/probes/acchu.s` |

## The scalar result unit

Field bit `0x40` selects it; bits 3–5 pick the function and bits 0–2 the scalar
register (`r0`–`r7`). It writes an aggregate of the sixteen lane results, and a
lane predicate applies to the aggregate as well.

| Function | Result | Source |
|---|---|---|
| `SUMU` | the lanes added up, each read unsigned at the operation's width | measured: `vpu-probe/probes/sru.s` |
| `SUMS` | the same, read signed | measured: `vpu-probe/probes/sru.s` |
| `IMIN` | the index of the smallest lane — the first, on a tie | measured: `vpu-probe/probes/sru.s`, `vpu-probe/probes/sru2.s` |
| `IMAX` | the index of the largest — the last, on a tie | measured: `vpu-probe/probes/sru.s`, `vpu-probe/probes/sru2.s` |
| `MAX`, `max2`, `max4`, `max6` | the largest lane, signed; the three `maxN` spellings answered exactly what `MAX` did over every vector tried | measured: `vpu-probe/probes/sru.s`, `vpu-probe/probes/sru2.s` |

## What the model executes

`VecInsn::executable` decides, by field rather than by whole-word template.
A linear sweep of `start4.elf`'s `.text` with this decoder finds **14650**
vector instructions, and **14369 of them execute**. That denominator is a
sweep, not a count of real instructions: it steps two bytes at a time and
cannot tell a jump table from code, which is most of what is left over.

The 281 that do not split by what `binutils-vc4` objdump makes of the same
address — a better measure than the page they sit in, since a linear sweep
through a jump table produces valid-looking encodings by accident:

| Left over | What it is | Source |
|---|---|---|
| 149 | ALU sub-ops objdump itself only numbers — `vop48.1`, `vop49.1` and so on up to `vop63.1`, with 63 alone accounting for 75. The name is a CGEN enum slot, the same way `mem31` is; neither decoder has a form for them | decompile: `binutils-vc4` objdump over the same addresses |
| 72 | words objdump prints raw, as `vec48` or `vec80`: it recognises the class and nothing inside it | decompile: `binutils-vc4` objdump over the same addresses |
| 44 | memory-class words objdump spells `vunkld`, `vunkst` or `vunklookupml` — its own name for a transfer whose operands fit no form it knows | decompile: `binutils-vc4` objdump over the same addresses |
| 11 | addresses objdump does not decode at all: the two linear sweeps drifting apart inside data | decompile: `binutils-vc4` objdump over the same addresses |
| 5 | ordinary instructions objdump names and this decoder refuses, each one of a kind. Three of them objdump cannot spell either — it prints the B operand `r56?bit4??bit5?`, a 48-bit scalar field naming a register past `r31`. The other two carry something on a slot that has no business being there: a `*` on the inert dash of a store that names an address, and a `++` on a gather's inert slot, which is measurably **not** inert — `v8lookupm H(1,0),H(20++,0),H(21,0)` answers `0x04` in every lane where the plain form answers `0x40`, and what it steps is not established | decompile: `binutils-vc4` objdump over the same addresses |

None of it is reached on a firmware boot: `boot` stops on an unimplemented
instruction by default, and `boot-check testdata/boot/firmware.yaml`
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
| the probes themselves | `vpu-probe/` | measured: one `.s` file per question, with the findings in its `README.md` |

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
