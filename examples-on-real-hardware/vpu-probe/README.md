# Probing the VPU's vector unit on a real Pi

The vector unit is the part of the VC4 with no documentation worth the name,
and several of its questions cannot be answered by reading firmware: where a
`+rN` addend lands, what an address displacement counts in, how the register
file is actually laid out. This directory answers them by running VPU code on
a real board and reading back what it left behind.

Nothing here runs in CI, and nothing in the model depends on it. The findings
are baked into `src/vpu/` and into `tests/vpu_isa.rs`, with the board they came
from named; this is the apparatus that produced them, kept so the next question
does not have to rebuild it.

## How it works

The firmware's property mailbox has an `EXECUTE_CODE` tag (`0x00030010`) that
calls a function on the VPU with `r0..r5` and hands back `r0`. `vpuprobe.py`
allocates 64 KiB of VC memory through the same mailbox, drops a blob at offset
0, enters it with

- `r0` = bus address of a zeroed 4 KiB output page (offset `0x1000`),
- `r1` = bus address of a marker page whose byte `n` holds `n + 1`,
- `r2..r5` = whatever the caller passed,

and prints the output page base64-encoded when it returns. Each probe clears
the register file, runs the instruction under test, and dumps the file back out
with `v32st HY(0++,0),(r0+=r3) REP64` — 64 rows of 64 bytes, exactly one page.

`/dev/vc-mem` maps that memory one 4 KiB window at a time (more than one page
per `mmap` faults), and `/dev/mem` will not map it at all on a stock 64-bit
Raspberry Pi OS.

`vpuprobe4.py` times a blob instead of waiting a fixed five seconds for it: it
polls the output page while the code runs and says when the dump started. That
is what turned "it hangs" into "it retires in 2 ms when a load precedes it".

`vpudiag.py` is the same idea aimed at the firmware rather than the probe: it
snapshots ~96 MB of VC memory, runs the blob, snapshots again and prints which
of the firmware's own pages changed. `vcscan.py` reads VC memory without the
mailbox at all, which is the only thing left once a probe has wedged the
firmware.

`vpuprobe3.py` goes further: it reads every page of the allocation before and
after the call and prints what changed, so a write that lands somewhere other
than its stated destination is caught wherever it is inside the buffer. That
is what settled the stores below.

`vpuprobe2.py` is the same thing with the mailbox call in a thread that times
out after five seconds: when the blob wedges the VPU the page is read and
printed anyway, so a probe of an encoding that hangs still says how far it
got. Use it for anything undocumented — the firmware is gone either way, but
at least the measurement survives.

## Running one

Assemble with the `vc4` binutils port — `poizan42/binutils-vc4`, whose vector
decoding has been corrected against hardware probes:

```sh
vc4-elf-as -o probes/layout.o probes/layout.s
vc4-elf-objcopy -O binary probes/layout.o layout.bin
base64 -w0 layout.bin > layout.b64
scp layout.b64 vpuprobe.py <board>:/tmp/
ssh <board> 'cd /tmp && sudo python3 vpuprobe.py layout.b64'
```

**This runs arbitrary code on the processor the firmware itself runs on.** A
bad blob can take the firmware down with it, and the board then needs a power
cycle. Keep probes short, return with `rts`, and do not leave the register file
in a state a firmware thread might be mid-way through using.

Two things are known to wedge it, both found the hard way:

- the **undocumented memory sub-ops**, which **wait for an outstanding vector
  load**. Put a `v8ld` in front of one and it retires in 2 ms; with nothing in
  front of it, it never returns. A vector *store* does not satisfy it, and
  neither does a *scalar* load — only a vector load does. What licenses the next one is
  narrower than a count of loads: a load then two fences back to back hangs,
  but a load, two `v32mov`s and then two fences retires both in 1 ms. Writing
  the fences to different rows does not help, nor does a second load before
  the second fence. So it wants other vector work in between, and what
  exactly is being drained is still open. That single fact
  explains everything else below — every probe of these sub-ops loaded its
  vectors first, which is why the first one always seemed to work.

  What that makes them is a **fence** on the vector memory unit, not an
  illegal encoding: an instruction a compiler emits rarely and a jump table
  contains by accident, which is exactly where they are found.

  The rest of this entry is how it looked before that was understood — the
  same instruction, seen from the outside: With interrupts enabled one of them completes, everything
  after it in the blob runs, and the register-file dump lands — but
  `EXECUTE_CODE` never returns and the driver gives up with `ETIMEDOUT`. Run
  one behind a `di` and there is no dump at all: the core is still inside the
  instruction, because nothing can interrupt it. Two in a row both complete.
  The status register is unchanged either side (`0x40000008`, IE still set),
  no firmware memory is corrupted — 96 MB diffed across a run, only counters
  and timestamps moved — and a deliberate `bkpt` in the same position behaves
  completely differently: it completes, the mailbox answers, the board lives.
  So this is not the ordinary trap path and not memory damage; it is a wait
  for something that never arrives. Powering up the QPU first
  (`vcmailbox 0x00030012 4 4 1`) changes nothing. `mem03`, `mem10`, `mem16` and `mem19`
  never return at all; `mem11`-`mem15`, `mem17`, `mem18` and `mem20`-`mem23`
  hand back their page and leave the firmware dead behind them. Linux stays up
  and answers SSH; `vcgencmd` hangs, and the probe process sits in an
  uninterruptible `ioctl`, so it cannot even be killed. A reboot usually brings
  the firmware back — but not always: a board wedged this way can fail to boot
  Linux at all afterwards and need a power cycle, since the VPU is what boots
  it. Sub-op 7 is the one exception in the group: it writes zeros and the
  board goes on running.
- **clobbering `r6` and above.** Probes that write `r6`–`r9` came back as
  `OSError: [Errno 22] Invalid argument` from the mailbox `ioctl` — the
  firmware's own path through `EXECUTE_CODE` wants them intact. Keep to
  `r0`–`r5`.

## The probes

| probe | question it answers |
|---|---|
| `layout.s` | where each element of `H`/`HX`/`HY` lands in a row, at each operation width |
| `vert.s`, `vert3.s` | the same for the `V` family, and how a vertical coordinate splits |
| `vinc.s` | what `++` steps on a vertical slot |
| `pa48.s` | what `+rN` adds to (run it with `r2` = 0, 1, 2, 3, 16) |
| `disp.s` | what an address displacement counts in |
| `conv.s` | narrowing and widening between the operation's width and the register's |
| `wrap.s`, `wrap2.s` | what an unaligned element does at a 16-byte boundary |
| `mix.s` | all of it at once — the run `tests/vpu_isa.rs` pins as a regression |
| `vx46.s` | one prediction of the layout formula, checked against silicon |
| `alu.s`, `alu2.s` | the ALU ops, over a plain pair of vectors and over edge cases (`alu2-vectors.hex`) |
| `mul.s` | the multiplies, signed and unsigned, low / middle / high |
| `acc2.s`, `acc3.s` | the accumulator: one effect per row, then polarity and width (`acc3-vectors.hex`) |
| `accmix.s` | a whole program — multiplies, accumulate, `REP` — replayed against the model in `tests/vpu_isa.rs` |
| `wmix.s`, `wmix2.s`, `wmix3.s` | what an ALU op does when it is wider than the registers it names: the extension, the saturation, the shift count (`wmix-vectors.hex`, `wmix2-vectors.hex`) |
| `wacc.s` | the accumulator across the same width change, replayed as a program in `tests/vpu_isa.rs` |
| `setf.s`, `setf3.s`, `setf4.s`, `setf5.s` | what `SETF` leaves in the lane flags, which ops touch the carry, and what `vgetacc` reads |
| `imm.s` | whether a vector immediate is signed |
| `alu3.s`, `alu4.s`, `alu5.s` | the sub-ops that were still blank: the carry-in forms, the signed shifts, and which sub-op means what at which width (`alu4-vectors.hex`) |
| `alu6.s`, `star.s` | a dash A operand, multiplies whose registers differ in width, and what a `*` on a slot changes |
| `sru.s`, `sru2.s` | the scalar result unit's eight functions, and how it breaks a tie (`sru2-vectors.hex`) |
| `bp.s` | what `bitplanes` does with a vector B, and whether a predicate reaches the scalar result |
| `acch.s`, `usub.s` | the `...H` accumulator forms and the `SUB` modifier, read back with `vgetacc` |
| `setf.s`, `setf2.s`, `setf3.s`, `setfc.s` | what `SETF` leaves in the lane flags, and which ops touch the carry |
| `setf4.s`, `setf5.s` | whether a transfer writes flags at all, and what `vgetacc` reads |
| `noena.s` | an accumulator modifier without `ENA`, and `SETF` under a predicate |
| `mem5.s`–`mem9.s`, `memr.s` | the gather and the scatter, indexed by the accumulator (`mem2-vectors.hex`, `mr-vectors.hex`) |
| `mul32.s` | the family the `L` bit selects (`mul32-vectors.hex`) |
| `ldodd.s` | what a load does with an A slot that names a register, and with a dash that carries an addend |
| `mhdt.s` | ALU sub-ops 60-63, and an accumulator modifier that reads without writing back |
| `lut.s`, `lut2.s` | `memread`/`memwrite`: the unit's own lookup table, how it is banked, and the three ways an index is spelled |
| `sdisp.s` | what a displacement beside a scalar B operand does — hand-assembled, the one form `binutils-vc4` prints but cannot assemble |
| `gacc.s` | `vgetacc` with a dash destination, feeding the scalar result unit |
| `mld.s`, `where.s` | a `vld` with a vector slot in the B position: that it reads **address 0**, confirmed against the same RAM through `0x80000000` and `0xc0000000` |
| `st63.s`, `st63b.s`, `st63c.s`, `st64.s` | the stores and scatters that name no address — they write nothing at all |
| `r63.s`, `r63b.s`, `r63c.s` | `(r63)` in a gather's address, whether the A slot matters, and a gather with a dash destination |
| `lkb.s`, `lkc.s` | a gather whose B slot holds a vector instead of an address: it reads from zero, like `(r63)` |
| `m07.s`, `addr07.s`, `addr07b.s` | memory sub-op 7: that it writes zeros, writes nothing at an address it is handed, and leaves the lookup table alone |
| `m11.s`-`m23.s` | the rest of the unnamed memory sub-ops. **Each one kills the firmware** — run them only on a board you can power-cycle |
| `di11.s`, `two11.s`, `sr11.s` | what one of them does to interrupts, to a second one after it, and to the status register |
| `after_ld.s`, `after_st.s`, `trap.s` | what still runs after one, and how a deliberate `bkpt` compares |
| `ldop.s`, `stop.s`, `scop.s` | **what satisfies the fence**: a vector load does, a vector store and a scalar load do not |
| `unroll1.s`, `unroll8.s`, `loop8ld.s`, `rep64.s` | how many of them one load covers, and that `REP` repeats without waiting again |
| `two11.s`, `dd2.s`, `ld2x2.s`, `ld3.s`, `alt2.s` | which arrangements of load, fence and `v32mov` retire and which hang |

## What they found (Raspberry Pi 4B d03115, firmware 1.20260824)

- A row is **sixteen lanes of four bytes**, not sixty-four bytes in a line.
  Element `e` of a register `w` bytes wide sits at byte
  `(e & 15) * 4 + (e >> 4) * w`.
- The slot's type nibble is the **register's** element width (`H`/`V` 8-bit,
  `HX`/`VX` 16-bit, `HY`/`VY` 32-bit). The operation's width is separate, and
  the unit converts: narrowing truncates, widening zero-extends, both on loads
  and on stores.
- A slot's first element is `band * 16 + fine`, in elements — which is why the
  byte coordinate objdump prints and the element index part company as soon as
  an element is wider than a byte.
- `+rN` adds to that element index, in elements, wrapping within the row.
- An address displacement is a plain **byte** offset, at every width.
- `++` steps the row horizontally and the element vertically.
- A **load** whose element straddles a 16-byte boundary wraps inside that
  block instead of crossing it: `v32ld HY(0,0),(r1+13)` over ascending bytes
  reads `0e 0f 10 01`. A **store** crosses normally.
- The ALU: `s` suffixes saturate to the destination element, `min`/`max`/`asr`
  are signed and `lsr` is not. `even`/`odd` pack A's
  alternate elements into lanes 0-7 and **B's** into 8-15; `clip` is
  `clamp(a, 0, b)`; `sign` is `b + signum(a)`; `count` is
  `popcount(a) + popcount(b)`; `brev` reverses a's low `n` bits, `n` being b's
  low bits, or the whole operation width when they are zero.
- The multiplies: `mull` keeps the product's low half, `mulm` shifts it right
  by 8, `mulhd` keeps the high half and `mulhn` rounds while doing so; the
  suffix says which operand is signed.
- The accumulator is one per lane and wider than an element (four accumulates
  of `0xffff` read back as `0x3fffc`). `CLRA` clears it, the result is added or
  — with `SUB` — taken off, read signed or unsigned as `SIGN` says, and `WBA`
  makes the destination take the accumulator instead of the raw result.
  `vgetacc D,A,B` reads it shifted right by `B & 15`, with `s16`/`s32`
  saturating variants.
- An ALU op **wider than its registers** — which most of them are — reads each
  source at the register's own width and widens it: a byte unsigned, a halfword
  signed. The result is truncated into the destination element, except for the
  saturating ops, which clamp to what that element holds: `0..=0xff` for a byte
  register, signed for a wider one. A shift, rotate or `brev` counts in the
  operation's width (`b & 31` for `v32`, `b & 15` for `v16`), and `brev` with a
  zero count reverses the whole operation width. `msb` answers the index of the
  highest bit set in **either** operand. Nothing about the accumulator changes.
  `v32count` writes a zero into every lane whatever its operands.

- Each lane has a zero, a negative and a carry flag. `SETF` on an ALU op
  writes zero and negative from the result at the operation's width — after a
  saturating op has clamped it — and the carry only where the op has one: a
  carry out of an addition, a borrow out of a subtraction, "it clamped" out of
  a saturating op, "B won" out of `min`/`max`, and the last bit to leave the
  element out of a shift, its index taken modulo the width. The logical ops,
  the shuffles, `dist`, `count`, `msb`, `brev`, `clip`, `sign`, `mov` and
  `mull` leave the carry alone, and a **load or store with `SETF` writes no
  flag at all**. The eight predicates `ALL`/`NONE`/`IFZ`/`IFNZ`/`IFN`/`IFNN`/
  `IFC`/`IFNC` read them back.
- `vgetacc D,A,B` writes each lane's accumulator shifted right by `B & 31` —
  five bits, the accumulator being wider than an element — reads A for
  nothing, and clamps into a signed 16- or 32-bit range in its `s16`/`s32`
  forms. The width field picks the saturation, not an element size.
- A vector immediate is **signed** in both encodings: six bits in the 48-bit
  form, sixteen in the 80-bit one, so `#0x20` on a 48-bit `v32mov` fills the
  lanes with `0xffffffe0`.

- `bitplanes` **transposes** the lanes' bits: lane `i` of the result is the
  word whose bit `j` is bit `i` of lane `j` of B. A scalar B — which every lane
  sees alike — therefore comes out as all-ones wherever B's bit `i` is set,
  which is the one-flag-per-bit form the firmware uses.
- A transfer that names **no address** — a `vst` or an `indexwritem` whose B
  slot holds a vector, or a scatter with a dash source — writes nothing
  anywhere: not at the operand address, not at address 0 where the matching
  load reads, and not one byte of the 64 KiB `vpuprobe3.py` watches.
- A `lookupm` that names no address — `(r63)`, or a vector in the B slot —
  gathers from **zero**: with 16 in the accumulator's high half both forms
  answer the byte at address 16.
- A dash in the A position is an operand of **zeros**, whatever the op. A `*`
  on any slot changes nothing a register or memory can see — destination,
  source, `+rN`, under `REP`, on a load or on a store alike.
- A multiply works at the **widest** register it names and converts the
  narrower ones into it.
- `addc`/`subc`/`rsubc` take the lane's carry flag in; `signshl` and `signasl`
  shift by a *signed, unmasked* count — left when B is positive, right when it
  is negative, zeros or the sign shifting in — and a count past the width
  empties the element.
- Some sub-ops compute at one width and write a lane of **zeros** at the other:
  `count`, `testmag` and `bitplanes` are live at `v16` and blank at `v32`,
  sub-op 30 the other way round (`b * signum(a)`), and 13, 22, 23 and 44–47 are
  blank at both. Measured over a destination preset to all-ones, so they write.
- The `...H` accumulator forms accumulate into the **high half**: the result
  goes in shifted left by sixteen, and a write-back reads it shifted back down,
  clamped into the destination's signed range. `SUB` is not a subtracting
  accumulate — it leaves the accumulator alone and hands the destination
  `accumulator - result`.
- **Sub-ops 60 and 61 truncate where `mulhd` floors**: `0x0ff0 * 0xfff1` — a
  product of −61200 — answers `0x0000` there and `0xffff` from `mulhd`. 62 and
  63 write a lane of zeros, like the other blanks.
- An accumulator modifier with `ENA` but **no `WBA`** does not accumulate: the
  destination takes `result + accumulator`, or `accumulator - result` with
  `SUB`, and the accumulator itself does not move. `CLRA UACC(A)` followed by
  `UADD(B)` answers `A + B + A` with `A` still in the accumulator.
- `memread` and `memwrite` are the **lookup table**: 1 KiB inside the unit,
  banked sixteen ways so each lane indexes its own 64 bytes at `b * width`.
  Seven lanes writing index `0xff` each read their own value back, which is
  what says it is banked rather than shared. The index is a vector slot, a
  scalar register or an immediate — a scalar reaching every lane alike — and
  scales by the element width whichever way it is spelled, so a `v16` write at
  3 leaves byte 3 alone.
- A dash B operand's **displacement simply adds to the register**: with
  `r2` = 100, `r2-1` reaches the lanes as 99 and `r2+100` as 200. The
  assembler has no syntax for the form, so the words were built by hand and
  checked against objdump's rendering of the same bytes.
- `vgetacc` with a **dash destination** keeps only the scalar result unit's
  aggregate — the form the firmware uses. The lane values go in whole rather
  than re-read at the element width: sixteen accumulators holding `0x0000ffff`
  and the like summed to their plain total under `SUMS`, and the `B` shift
  applies before the sum.
- The scalar result unit: `SUMU`/`SUMS` add the lanes up unsigned and signed,
  `MAX` answers the largest signed, `IMIN` the index of the first smallest and
  `IMAX` the index of the last largest; `max2`, `max4` and `max6` answered
  exactly what `MAX` did over every vector tried. A lane predicate applies to
  the aggregate as well.

## Still open

The **`indexwritem` whose index comes from a slot** the scatter decode does
not take — around twenty of them, all in pages that look like data.

Memory sub-op 3 and the rest above 9: sub-op 3 is the one that took the
firmware down when a probe ran it, and 16 and 19 did the same, so whatever
they do costs a reboot to find out. Then what an unsigned `SUB` in the high
half answers — it matched neither the wrapped difference nor a clamped one,
lane for lane.
