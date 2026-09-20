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

## Still open

The `...H` accumulator forms (`UACCH`, `SACCH`) did not fall out of these runs:
each hypothesis that fits one lane breaks another, and they look like a second
accumulator or a second half of one rather than a write-back mode. `v16clips`
and `v32count` each write a zero into every lane over the same vectors their
unsuffixed forms answer sensibly on, so neither is the operation its name
suggests. The other open questions are what a multiply does when its registers
disagree in width — it carries no width of its own to convert to — what the
scalar-result unit does beyond `SUMU`/`SUMS`, and what `SETF` leaves in the
lane flags.
