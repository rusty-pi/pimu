# The bare-metal aarch64 frontend

`aarch64/` builds `vc4-to-aarch64`, an image `qemu-system-aarch64 -machine
raspi4b -kernel` boots. It is stage 2 of [#32](https://github.com/valtzu/rpi-virt-fw/issues/32),
whose end goal is to run Linux against the *live* firmware model: the emulator
stays resident at EL2, installs stage-2 translation, and services the guest's
mailbox traffic from the same models the hosted `recon` binary drives.

Today the image is scaffolding. It brings up a stack, installs EL2 exception
vectors, clears `.bss`, prints a banner over the PL011, checks its own exception
level and exits. **There is no VPU model in it yet** — that is stage 3, and it
depends on the `no_std` port (stage 1).

```console
$ scripts/qemu-kernel.sh
image: target/aarch64-unknown-none/release/vc4-to-aarch64.img (16336 bytes)

rpi-virt-fw: aarch64 frontend (issue #32 stage 2)
  CurrentEL   EL2
  MPIDR_EL1   0x0000000080000000
  DTB (x0)    0x0000000000000100
  image       0x00200000 .. 0x002180e0 (0x180e0 bytes)
  bss         0x00203fd0 .. 0x002040e0   stack top 0x002140e0
  UART0       0xfe201000 (PL011)
  VBAR_EL2    0x00201800 (16 x 0x80, see aarch64/src/vectors.rs)
scaffolding only: no VPU model in this image yet
qemu -kernel check passed
```

The script builds, flattens the ELF to a raw image, boots it, and asserts on
QEMU's exit status. It exits 2 — the same convention as `scripts/qemu-check.sh`
— when this QEMU has no `raspi4b`, so a toolchain gap reads as a skip and not as
a regression.

## How the image reports a pass

`cargo test`, `scripts/boot-check.sh` and the golden transcript do not run
inside a bare-metal image; [#32](https://github.com/valtzu/rpi-virt-fw/issues/32)
calls that the epic's real open risk. The image reports out through **ARM
semihosting** instead: `SYS_EXIT_EXTENDED` (`HLT #0xF000` with `0x20` in `x0`
and `ADP_Stopped_ApplicationExit` plus an exit code in the parameter block), so
QEMU's own process exit status carries the verdict.

| image exit status | meaning |
| --- | --- |
| 0 | ran, and every self-check passed |
| 1 | a self-check failed — not at EL2, or loaded at the wrong address |
| 3 | an exception was taken; the dump is on the console |
| 4 | a Rust `panic!` reached the panic handler |

2 is deliberately never used by the image: `scripts/qemu-kernel.sh` reserves it
for "this QEMU has no `raspi4b`", which CI turns into a warning.

This replaced grepping stdout for `CurrentEL   EL2`. A grep cannot tell
"reported EL1" from "printed nothing", it does not scale past a banner, and it
read a hang and a crash as the same thing — the script's `timeout` firing is now
a failure in its own right, because the image ends by exiting rather than by
parking.

The mechanism needs `-semihosting-config enable=on,target=native` on the QEMU
command line, which the script passes. Verified working on `raspi4b` under QEMU
10.2.1: the exit code propagates unchanged to the host shell. Without the flag,
`HLT #0xF000` is an unallocated instruction — the handler recognises that it is
the semihosting call itself that trapped, says so once and parks, rather than
recursing.

## Exception vectors

`VBAR_EL2` is installed by the entry stub before the `.bss` clear, so every
instruction after the first three is covered. Left at its reset value of 0, a
fault vectors into QEMU's boot stub at physical `0x200`, which is not a handler,
so the machine re-takes the same exception forever and prints nothing at all —
which is exactly how the `0x200000` load-address bug below presented, visible
only under `-d int`.

The table is sixteen 0x80-byte slots in its own 2 KiB-aligned output section
(`VBAR_EL2` ignores bits 10:0, so `link.ld` `ASSERT`s the alignment rather than
letting a misplaced table become a wrong-address jump on the first fault). Each
slot records which of the sixteen fired and branches to one common path, so the
`0x400` group — where stage 4's stage-2 aborts arrive — is a branch in
`rvf_exception` rather than a rewrite.

A handler must not itself be able to fault, so it has its own stack
(`__exc_stack_top`, separate from the boot stack, because the faulting `SP` may
*be* the fault), saves the general registers into a fixed `.bss` frame reached
with PC-relative `adrp`, and reports using `uart::puts`/`puthex` only — never
`core::fmt`, which dispatches through absolute vtable pointers and is a
plausible thing to have faulted in the first place.

`scripts/qemu-kernel.sh --fault` builds with the `fault` feature, which branches
to an unmapped address on purpose, and requires image status 3:

```console
$ scripts/qemu-kernel.sh --fault
...
fault feature: branching to an unmapped address on purpose

rpi-virt-fw: EXCEPTION: Current EL, SP_ELx: Synchronous
  ESR_EL2     0x86000000   EC 0x21 = instruction abort without a change of EL
  ELR_EL2     0x8000000000000000   (the faulting instruction)
  FAR_EL2     0x8000000000000000   (the faulting address)
  SPSR_EL2    0x00000000600003c9
  ISS         0x0000000      address size fault
  x0  0x0000000000000000   x1  0x0000000000203ea1
  ...
  x8  0x8000000000000000   x9  0x000000000000beef
  ...
  SP          0x0000000000214150   (at the fault, not the handler's)
qemu -kernel fault check passed (the vector table caught it)
```

CI runs both, so the path that only ever executes when something has already
gone wrong is not discovered to be broken at that moment.

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

CI builds it in the fast `tests` job of `.github/workflows/boot-log.yml`, and
the `aarch64` job boots it for real — both the normal run and the `--fault` one.

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
| `aarch64/src/boot.rs` | the 64-byte arm64 Image header and the entry stub (CPU park, `VBAR_EL2`, stack, `.bss` clear, placement evidence) |
| `aarch64/src/main.rs` | `rvf_main`, the banner, the self-checks, and the `#[panic_handler]` |
| `aarch64/src/vectors.rs` | the EL2 vector table and the fault report |
| `aarch64/src/semihost.rs` | `SYS_EXIT_EXTENDED`, and the exit statuses the script asserts on |
| `aarch64/src/uart.rs` | PL011 transmit at `0xFE20_1000`, register names shared with `src/periph/uart_pl011.rs`; `puthex`/`putdec` for the `core::fmt`-free fault path |
| `aarch64/link.ld` | load address, section order, `.bss`, the two stacks, `_image_size` |
| `aarch64/build.rs` | passes the linker script by absolute path |

## What stage 3 needs from this

* An allocator and a RAM window. The image reserves only its own `.bss` and a
  64 KiB stack; the model's memory has to come from somewhere deliberate —
  physical RAM above the image, not a `Vec`, because QEMU devices DMA straight
  into machine memory (there is no SMMU by default).
* Blobs via `-initrd`. `-pflash` is not available on `raspi4b`; `-initrd` places
  a bundle in RAM and reports its address in the device tree QEMU passes in `x0`
  (`0x100` on this machine). Nothing in the image parses the device tree yet.
* A way to report a pass, which is now here: call `semihost::exit` with
  `EXIT_OK` when the milestones the model was supposed to reach were reached,
  and a non-zero status otherwise. `scripts/qemu-kernel.sh` already asserts on
  it, and CI already fails on it. The hosted binary stays the primary test path
  — `cargo test` and the golden transcript still live there — but the image is
  no longer a thing that can only be judged by eye.
* A fault handler that is already installed. Anything stage 3 puts in the image
  gets `ESR_EL2`/`ELR_EL2`/`FAR_EL2`/`SPSR_EL2` and the general registers on the
  console when it goes wrong, and a distinct exit status, instead of a silent
  spin in QEMU's boot stub.
