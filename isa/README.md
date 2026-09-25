# The instruction-set spec

`vpu.toml` is where every statement about the VC4 instruction set is
written down, beside the evidence for it. `docs/vpu-isa.md` is rendered from
it by `cargo run -- spec-docs --update`, the same command that regenerates the
peripheral pages under `docs/periph/`, and `tests/specs.rs` fails if the two
have drifted apart. **Edit this file, never the Markdown.**

`build.rs` also reads it: the sub-op mnemonic tables the disassembler prints
(`VEC_ALU_OPS`, `VEC_MEM_OPS`) are generated from the op rows here, and
`tests/vpu_isa.rs` checks each row's `status` against what
`VecInsn::executable` really accepts. Writing a sub-op down and forgetting to
wire it up — or the other way round — fails the build or the test.

## Format

```toml
title = "…"          # the page's H1
intro = """…"""      # Markdown under it

[[section]]
title   = "Slot descriptors"
level   = 2          # 2 = `##` (the default), 3 = `###`
body    = """…"""    # Markdown before the table
columns = ["Field", "Meaning", "Source"]
after   = """…"""    # Markdown after the table

[[section.source]]   # what the section as a whole rests on
kind       = "measured"
ref        = "`v8ld H(0,0),(r1)` over a page of ascending bytes"
confidence = "high"
note       = "optional caveat, printed in italics"

[[section.row]]
cells = ["`++`", "post-increment: the row horizontally, the element vertically"]

[[section.row.source]]
kind       = "measured"
ref        = "`probes/vinc.s`"
confidence = "high"
```

A section tagged `ops = "alu"` or `ops = "mem"` is a sub-op table, and its
rows carry three more keys — `subop`, `mnemonic` and `status` (`executes` or
`unknown`) — which is what the build reads. Every sub-op of the class has to
appear exactly once.

A table whose last column is named `Source` gets that column filled in from
each row's sources, so a row carries one fewer cell than the table has
columns; every row in such a table must name at least one source. `kind` and
`confidence` are the same vocabularies the peripheral specs use — see
[`specs/README.md`](../specs/README.md).

## Which kind to use

| kind | for |
|---|---|
| `measured` | something a probe in `vpu-probe/` established on a real board; name the probe |
| `decompile` | something read out of `start4.elf` (give the address) or out of the `binutils-vc4` opcode tables |
| `manual` | something named in Herman Hermitage's VideoCore IV Programmers Manual. Never on its own — it says where to look, and a `measured` source beside it says what the board actually did |
| `trace` | something the model's own boots pin: the firmware would come out differently if it were wrong |
| `inferred` | a guess — say what it rests on, and keep the confidence honest |
