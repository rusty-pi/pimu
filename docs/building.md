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

Set on a build without it, the `PIMU_*` variables are reported and ignored, and
`--trace*` and those channels are refused. [`diagnostics.md`](diagnostics.md)
says which switch needs which build.

## The `repo` feature

`run`, `run-all`, `boot-check` and `spec-docs` only mean something inside a
checkout: they check golden transcripts under `testdata/` and rewrite `docs/`
through the `CARGO_MANIFEST_DIR` of the build. They are the `repo` feature, on
by default, and the released binaries leave them out:

```bash
cargo build --release --no-default-features
```

Such a build answers `error: unknown command 'boot-check'` and does not list
them in `--help`.

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
the desktop lag. `PIMU_BUILD_CPU_PERCENT` changes the share (`100` lifts the
limit); with `CI` set the build gets every CPU.

## The EEPROM image a binary carries

A boot of a medium with no bootloader of its own — a firmware directory, a card
image — needs the EEPROM image `rusty-pi/pi4-firmware` publishes, and a build
from this tree fetches it with `gh` on the first such boot. A build can carry it
instead:

```bash
PIMU_EMBED_EEPROM=firmware/pieeprom.bin cargo build --release
```

`build.rs` then includes those bytes in the binary, and the boot writes them to
the same cache directory rather than downloading anything. Unset, the binary
carries no image and the fetch stays as it was.

## The release builds

`.github/workflows/release.yml` replaces the `latest` release on every push to
main: a tarball, a `.deb` and an `.rpm` for x86-64 and for aarch64, and the
`ghcr.io/rusty-pi/pimu:latest` image with a manifest for both. Both binaries are
built in a `debian:12` container, because a release should not need a newer glibc
than a Raspberry Pi OS install has, and the aarch64 one is cross-linked there —
every dependency is pure Rust, so that costs one `gcc` and no emulation. They are
built with the image above, which is why the release needs a `FIRMWARE_TOKEN`
secret: a token that can read the private `rusty-pi/pi4-firmware`, since a
workflow's own token reaches only its repository. Without the secret the job
still publishes, with binaries that fetch the image themselves.
