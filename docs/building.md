# Building

```bash
cargo build --release
```

## The `diag` feature

The run-loop diagnostics — tracing, traps, watchpoints, profiling and the log
channels of the run loop, the VPU core and the DMA window — are checked on every
instruction, so a normal build compiles them out:

```bash
cargo build --release --features diag
```

Set on a build without it, the `RVF_*` variables are reported and ignored, and
`--trace*` and those channels are refused. [`diagnostics.md`](diagnostics.md)
says which switch needs which build.

## Profile-guided optimisation

`scripts/pgo-build.sh` does the release build with PGO: an instrumented build
runs the firmware boot and the start of the Linux boot, and the release build is
redone with what it counted. It needs what `boot-check` needs and takes about
10 minutes on a Pi 4, where both boots then run 1.45x faster; the guest runs the
same instructions either way.

CI does not use it — the extra build and training cost more than the boot jobs
would save.

## The build's share of the machine

Cargo runs rustc through `scripts/rustc-wrapper.sh` (`.cargo/config.toml`),
which outside CI keeps the compiler on 80% of the CPUs so a build does not make
the desktop lag. `RVF_BUILD_CPU_PERCENT` changes the share (`100` lifts the
limit); with `CI` set the build gets every CPU.
