# Building

```bash
cargo build --release
```

## The layout

```
src/
  lib.rs        the library; its crate docs map the modules
  cli/          the command line: main.rs = usage + dispatch, one file per
                command (boot, scenario, disasm), mbox.rs, config.rs
  vpu/          VideoCore IV scalar core: length, decode, exec, registers
  aarch64/      A64 core: integer, SIMD/FP, MMU, system registers
  arm/          the four A72 cores, released at arm_loader, lock-stepped
  armstub.rs    armstub hand-off words, image check, bootargs patch
  bus.rs        Bus + MmioDevice traits
  mem.rs        RAM region
  l2.rs         the VPU's L2 while the bootcode runs out of it
  machine.rs    Machine: owns RAM + peripherals, decodes addresses
  periph/       one file per block, stub.rs = catch-all + log
  net/          built-in DHCP/DNS/TFTP/HTTP peer for --tftp / --http; passt for --net
  soc/          BCM2711 memory map, stepping, board
  spec/         register-spec schema (specs/*.toml, via build.rs)
  firmware/     boot ROM stage; ELF32 loader; EEPROM image parse; dt-blob; Payload
  fdt.rs        device tree reader/patcher
  log/          --log channels; fatmap.rs = which file a disk block belongs to
  stdio.rs      host terminal as the serial console (--stdin)
  diag.rs       PIMU_* diagnostics
  emulator.rs   Emulator = VPU cores + ARM side + Machine, run loop
  harness/      scenario parsing, transcript capture, golden diff,
                boot.rs = the boot scenarios and their milestones,
                payloads.rs = hand-assembled VPU test programs
specs/          register maps with provenance; docs/periph/ is generated
docs/           board sheet, boot chain, diagnostics, VPU ISA, references
scripts/        fetch-firmware.sh, make-sd.sh, make-netboot.sh, pgo-build.sh,
                provision-eeprom.sh, make-dt-blob.py, vc4-xref.py
testdata/       in-process scenarios and the boot scenarios, with their goldens
```

## The `diag` feature

The run-loop diagnostics — tracing, traps, watchpoints, profiling and the log
channels of the run loop, the VPU core and the DMA window — are checked on every
instruction, so a normal build compiles them out:

```bash
cargo build --release --features diag
```

Set on a build without it, the `PIMU_*` variables are reported and ignored, and
`--trace*` and those channels are refused. [`diagnostics.md`](diagnostics.md)
says which switch needs which build.

A `diag` build also takes every step through every check of the run loop instead
of skipping the ones that cannot act (`Emulator::fast_steps`), so the switches
see each instruction, and it records start4's boot-progress tags (stores to
`0x?EC0_2000`), which `boot` prints after the run.

## The `repo` feature

Three commands only mean something inside a checkout: they check golden files
under `testdata/` and rewrite `docs/` through the `CARGO_MANIFEST_DIR` of the
build.

| Command | What it does |
|---|---|
| `run <scenario.yaml>` | Run one in-process scenario and check it against its golden transcript. |
| `run-all [<dir>]` | The same for every `*.yaml` in `<dir>` (default `testdata/scenarios`). |
| `spec-docs [--update]` | Check (or regenerate) `docs/periph/` against `specs/*.toml`, the dark board sheet against the hand-drawn one, and `schemas/` against the scenario types. |

`--update` rewrites the golden files instead of failing on a mismatch, and `-v`
prints the whole transcript. Firmware boots are checked by `boot --scenario`,
which every build has: see [`running.md`](running.md).

```bash
cargo run -- run-all -v                                       # every in-process scenario, with transcripts
cargo run --release -- boot --scenario testdata/boot/firmware.yaml  # one real boot, checked three ways
```

They are the `repo` feature, on by default, and the released binaries leave them
out:

```bash
cargo build --release --no-default-features
```

Such a build answers `error: unknown command 'run'` and does not list
them in `--help`.

## Profile-guided optimisation

`scripts/pgo-build.sh` does the release build with PGO: an instrumented build
runs the firmware boot and the start of the Linux boot, and the release build is
redone with what it counted. It needs what `boot --scenario` needs and takes about
10 minutes on a Pi 4, where both boots then run 1.45x faster; the guest runs the
same instructions either way.

CI's boot jobs do not use it — the extra build and training cost more than they
would save. The release builds do; see below.

Each training run prints how long it took and the `result:` line it ended on,
and a run that ends badly stops the build rather than leaving a profile that
says nothing.

`--profile-only` stops after `llvm-profdata merge`, leaving the profile at
`target/pgo/merged.profdata` for a caller that wants to do the final build
itself.

## The build's share of the machine

Cargo runs rustc through `scripts/rustc-wrapper.sh` (`.cargo/config.toml`),
which outside CI keeps the compiler on 80% of the CPUs so a build does not make
the desktop lag. `PIMU_BUILD_CPU_PERCENT` changes the share (`100` lifts the
limit); with `CI` set the build gets every CPU.

## The release builds

Pushing an `X.Y.Z` tag on main runs `.github/workflows/release.yml`, which
publishes what the tagged commit builds: a tarball, a `.deb` and an `.rpm` for
x86-64 and for aarch64, and the `ghcr.io/rusty-pi/pimu` image with a manifest
for both, tagged `:X.Y.Z` and `:latest`. The release body is the annotated tag's
own message, so the notes are written when the tag is made — by the `release`
skill in `.claude/skills/`, which also works out whether the change is a patch
or a minor bump and edits `Cargo.toml` to match. Both binaries are
built in a `debian:12` container, because a release should not need a newer glibc
than a Raspberry Pi OS install has, and the aarch64 one is cross-linked there —
every dependency is pure Rust, so that costs one `gcc` and no emulation. They
are built `--no-default-features`, without the `repo` commands. They carry no
firmware of any kind: a boot is given its EEPROM image with `--eeprom`.

A tag is made by hand rather than on every push to main, because a hosted
runner's minutes are billed and the PGO below roughly doubles the job. To rerun
a release that failed part way, dispatch the workflow on the tag's own ref: the
version and the notes both come from the tag, not from the branch.

The binaries are built with PGO, from one profile trained on x86-64 and used for
both targets: `-Cprofile-use` keys on function names and CFG hashes rather than
on the target, and training the aarch64 build honestly would mean an
instrumented interpreter booting Linux under qemu-user. The job fetches the
firmware blobs and builds the two training cards first, since that is what
`boot --scenario` boots. The instrumented build keeps the default features — the
profile names functions the final `--no-default-features` builds do not have, which LLVM
ignores.
