# The bare-metal aarch64 frontend

`aarch64/` builds `vc4-to-aarch64`, an image `qemu-system-aarch64 -machine
raspi4b -kernel` boots. It is stage 2 of [#32](https://github.com/valtzu/rpi-virt-fw/issues/32),
whose end goal is to run Linux against the *live* firmware model: the emulator
stays resident at EL2, installs stage-2 translation, and services the guest's
mailbox traffic from the same models the hosted `recon` binary drives.

Today the image is scaffolding. It brings up a stack, clears `.bss`, prints a
banner over the PL011 and parks. **There is no VPU model in it yet** — that is
stage 3, and it depends on the `no_std` port (stage 1).

```console
$ scripts/qemu-kernel.sh
image: target/aarch64-unknown-none/release/vc4-to-aarch64.img (6449 bytes)

rpi-virt-fw: aarch64 frontend (issue #32 stage 2)
  CurrentEL   EL2
  MPIDR_EL1   0x0000000080000000
  DTB (x0)    0x0000000000000100
  image       0x00200000 .. 0x00211940 (0x11940 bytes)
  bss         0x00201940 .. 0x00201940   stack top 0x00211940
  UART0       0xfe201000 (PL011)
scaffolding only: no VPU model in this image yet
qemu -kernel check passed
```

The script builds, flattens the ELF to a raw image, boots it, and fails unless
the guest reports `EL2`. It exits 2 — the same convention as
`scripts/qemu-check.sh` — when this QEMU has no `raspi4b`, so a toolchain gap
reads as a skip and not as a regression.

## Building it by hand

The crate is a workspace member excluded from `default-members`, so `cargo
build`, `cargo test` and `cargo clippy` at the repo root do not touch it; it
only compiles for `aarch64-unknown-none`.

```bash
rustup target add aarch64-unknown-none
cargo build --manifest-path aarch64/Cargo.toml --target aarch64-unknown-none --release
# or, from inside aarch64/, where .cargo/config.toml pins the target:
cd aarch64 && cargo build --release
```

CI builds it in the fast `tests` job of `.github/workflows/boot-log.yml`. It does
not *run* it: the runners' QEMU has no `raspi4b`.

## Why `raspi4b`

`-kernel` on aarch64 does not need a Linux kernel — it needs the arm64 boot
protocol (`Documentation/arch/arm64/booting.rst`), which U-Boot, EDK2 and Xen
also satisfy. Both `virt` and `raspi4b` will boot such an image at EL2, but only
`raspi4b` puts RAM at physical 0, which is the BCM2711 map the firmware produces
addresses for (`memory@0`, ARM entry `0x80000`, `vc_mem.mem_base=0x3ec00000`).
`virt` starts RAM at `0x4000_0000` with `flash@0` at the bottom, so every
firmware-produced address would land in the wrong place.

EL2 is not a nicety either: stage-2 translation, the mechanism that lets us take
one peripheral window at a time away from QEMU's own BCM2835 models and service
it from ours, exists only at EL2. The banner prints `CurrentEL` so the claim is
attested by the binary rather than by a host-side trace.

## Why the image loads at 0x20_0000 and not 0x8_0000

This is the one thing in stage 2 that is easy to get wrong and hard to debug, so
it is worth stating plainly.

The header carries a `text_offset` (where the image wants to sit, relative to
the base of RAM) and an `image_size` (how much memory to reserve there, `.bss`
included). QEMU's `load_aarch64_image()` in `hw/arm/boot.c` only looks at
`text_offset` when `image_size` is non-zero — and then adds 2 MiB if the
requested offset would collide with the boot stub QEMU writes at the bottom of
RAM.

So an image asking for offset 0 is loaded at `0x20_0000`, not at `0x8_0000`.
The first cut of this frontend was linked for `0x8_0000` with `text_offset = 0`
and failed exactly there: the entry stub and everything reachable by PC-relative
code ran fine at the wrong address, and the first absolute pointer — a
`core::fmt` vtable — branched into unmapped RAM. The symptom is
`Taking exception 1 [Undefined Instruction] ... ELR 0x810f0` under `-d int`, at
an address that is real in the binary and zero in the machine.

The fix is to ask for `0x20_0000` explicitly, which is also where we want to be:
`0x8_0000` is where the VideoCore firmware puts the ARM kernel, and stage 4
`ERET`s into that kernel. A resident sitting at `0x8_0000` would be overwritten
by its own guest.

The arm64 spec lets a bootloader ignore `text_offset` entirely as long as the
base is 2 MiB aligned, so the entry stub compares where it is running (`adr`,
PC-relative) against where it was linked (a literal the linker filled in) and
prints a fixed message instead of crashing if they differ. Both values reach
`rvf_main` as arguments; see `aarch64/src/boot.rs`.

## Layout

| file | what it is |
| --- | --- |
| `aarch64/src/boot.rs` | the 64-byte arm64 Image header and the entry stub (CPU park, stack, `.bss` clear, placement evidence) |
| `aarch64/src/main.rs` | `rvf_main`, the banner, and the `#[panic_handler]` |
| `aarch64/src/uart.rs` | PL011 transmit at `0xFE20_1000`, register names shared with `src/periph/uart_pl011.rs` |
| `aarch64/link.ld` | load address, section order, `.bss` and stack bounds, `_image_size` |
| `aarch64/build.rs` | passes the linker script by absolute path |

## What stage 3 needs from this

* An allocator and a RAM window. The image reserves only its own `.bss` and a
  64 KiB stack; the model's memory has to come from somewhere deliberate —
  physical RAM above the image, not a `Vec`, because QEMU devices DMA straight
  into machine memory (there is no SMMU by default).
* Blobs via `-initrd`. `-pflash` is not available on `raspi4b`; `-initrd` places
  a bundle in RAM and reports its address in the device tree QEMU passes in `x0`
  (`0x100` on this machine). Nothing in the image parses the device tree yet.
* A decision on the harness. `boot-check`, the golden transcript and `cargo test`
  do not run inside a bare-metal image; the hosted binary remains the test path
  unless semihosting is added. #32 flags this as the epic's real open risk.
