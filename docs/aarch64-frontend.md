# The bare-metal aarch64 frontend

`aarch64/` builds `vc4-to-aarch64`, an image `qemu-system-aarch64 -machine
raspi4b -kernel` boots. It is stages 2 and 3 of [#32](https://github.com/valtzu/rpi-virt-fw/issues/32),
whose end goal is to run Linux against the *live* firmware model: the emulator
stays resident at EL2, installs stage-2 translation, and services the guest's
mailbox traffic from the same models the hosted `recon` binary drives.

The image has two modes, and which one it is in depends on whether it was given
firmware to boot.

**Without `-initrd` it is the stage-2 environment check.** It brings up a stack,
installs EL2 exception vectors, clears `.bss`, prints a banner over the PL011,
checks its own exception level and exits. It takes under a second, which is why
CI still runs it on every push.

```console
$ scripts/qemu-kernel.sh
image: target/aarch64-unknown-none/release/vc4-to-aarch64.img (277936 bytes)

rpi-virt-fw: aarch64 frontend (issue #32 stage 3)
  CurrentEL   EL2
  MPIDR_EL1   0x0000000080000000
  DTB (x0)    0x0000000000000100
  image       0x40000000 .. 0x40057ee0 (0x57ee0 bytes)
  bss         0x40043db0 .. 0x40043ee0   stack top 0x40053ee0
  model RAM   0x00000000 .. 0x40000000 (1024 MiB, identity — see heap.rs)
  UART0       0xfe201000 (PL011)
  VBAR_EL2    0x40035800 (16 x 0x80, see aarch64/src/vectors.rs)
  heap        0x40200000 .. 0x80000000 (1022 MiB, see aarch64/src/heap.rs)
  CNTFRQ_EL0  62500000 Hz (the max_wall clock)
no firmware bundle: no ATAG_INITRD2 in the boot arguments (pass -initrd)
stage 2 only: pass -initrd <bundle> (scripts/make-blob-bundle.sh) to boot the model
qemu -kernel check passed
```

**With `--boot` it runs the VPU model** over `pieeprom.bin` and `sd.img` — the
bare-metal spelling of `recon firmware/pieeprom.bin --eeprom --sd
firmware/sd.img`, with the same crate, the same models and the same run loop.
See [Stage 3](#stage-3-the-model-in-the-image) below.

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
| 1 | a self-check failed — not at EL2, not running at the link address, a secondary CPU still in the model's RAM, a bad bundle, or a boot that stopped short |
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

`VBAR_EL2` is installed by the entry stub as the first thing it does after
relocating, so everything but the copy loop itself is covered — and the copy
loop is a dozen instructions that touch only memory the bootloader handed us.
Left at its reset value of 0, a
fault vectors into QEMU's boot stub at physical `0x200`, which is not a handler,
so the machine re-takes the same exception forever and prints nothing at all —
which is exactly how the load-address bug described below presented, visible
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
  x0  0x0000000000000000   x1  0x000000004003d27f
  ...
  x8  0x8000000000000000   x9  0x000000000000beef
  ...
  SP          0x0000000040053f20   (at the fault, not the handler's)
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

## Where the image is loaded, and where it runs

This is the one thing here that is easy to get wrong and hard to debug, so it
is worth stating plainly. The image is **loaded at `0x20_0000` and runs at
`0x4000_0000`**, and it copies itself between the two in its first dozen
instructions.

The header carries a `text_offset` (where the image wants to sit, relative to
the base of RAM) and an `image_size` (how much memory to reserve there, `.bss`
included). QEMU's `load_aarch64_image()` in `hw/arm/boot.c` only looks at
`text_offset` when `image_size` is non-zero — and then adds 2 MiB if the
requested offset would collide with the boot stub QEMU writes at the bottom of
RAM. So an image asking for offset 0 is loaded at `0x20_0000`, not `0x8_0000`.

The first cut of this frontend was linked for `0x8_0000` with `text_offset = 0`
and failed exactly there: the entry stub and everything reachable by PC-relative
code ran fine at the wrong address, and the first absolute pointer — a
`core::fmt` vtable — branched into unmapped RAM. The symptom is
`Taking exception 1 [Undefined Instruction] ... ELR 0x810f0` under `-d int`, at
an address that is real in the binary and zero in the machine.

### Why it does not simply stay there

Because the model's RAM is physical `0x0` .. `0x4000_0000` now (see below), and
`0x20_0000` is inside it.

### Why it is not simply *loaded* at `0x4000_0000`

`text_offset` is honoured, so asking to be loaded at 1 GiB does work on its
own — verified. What breaks is `-initrd`, which is how the firmware blobs get
in. QEMU places the initrd at `MAX(loader_start + MIN(ram_size/2, 128 MiB),
end-of-kernel-image)` and refuses it if it does not fit under `ram_size` — and
on `raspi4b` that `ram_size` is **960 MiB, not the machine's 2 GiB**:
`raspi_machine_init()` passes `machine->ram_size - vcram_size` to
`setup_boot()`, and the ARM's share is 1 GiB less the 64 MiB VideoCore split.

Measured on QEMU 10.2.1 by patching `text_offset` in the built image and
booting it with a 1 MiB `-initrd`:

| `text_offset` | result |
| --- | --- |
| `0x3800_0000` | ok |
| `0x3BF0_0000` | `could not load initrd` (the blob would cross `0x3C00_0000`) |
| `0x3BFB_0000` | `not enough space after kernel to load initrd` (start ≥ `0x3C00_0000`) |
| `0x4000_0000` | `not enough space after kernel to load initrd` |

So the image has to be loaded low, below anything the initrd needs, and get
itself out of the model's way afterwards.

### How the copy works

`.text.boot` is position-independent up to the branch that ends it. The stub
compares where it is running (`adr`, PC-relative) against where it was linked
(a literal the linker filled in), copies `_load_size` bytes — everything up to
the NOLOAD `.bss`, both bounds 16-byte aligned — to the link address, does
`dsb; ic iallu; dsb; isb` because it has just written the instructions it is
about to execute, and `br`s to an absolute literal pointing into the copy. Only
then does it install `VBAR_EL2`, set `SP` and clear `.bss`: the vector table,
both stacks and every absolute pointer in the image are at the link address,
and nothing that depends on them runs before the branch.

`rvf_main` still receives both values and still refuses to run if they differ,
so a copy that did not happen is a named failure rather than the first
`core::fmt` vtable branching into unmapped RAM. `link.ld` `ASSERT`s that the
loaded image cannot reach the address it is copied to, and that the whole image
fits in the 2 MiB reserved below the heap.

`0x8_0000` is left free throughout: it is where the VideoCore firmware puts the
ARM kernel that stage 4 `ERET`s into.

## The secondary CPUs

Left alone, three of `raspi4b`'s four cores would spend the whole run inside
what is now the model's RAM. That has to be dealt with before the model writes
a byte of it.

Under `-kernel`, `hw/arm/raspi.c` installs `write_smpboot64()` as the board's
`write_secondary_boot` hook and starts cores 1-3 at `smp_loader_start`. Read
back from inside the image on QEMU 10.2.1, physical `0x300` holds exactly that
stub — `mov x5, #0xd8` (`d2801b05`), `mrs x6, mpidr_el1`, `and x6, x6, #3`,
`wfe`, `ldr x4, [x5, x6, lsl #3]`, `cbz x4, spin`, … `br x4` — and the release
table at `0xd8` reads all zeroes. They never reach `_start`; the image's own
`mpidr` guard has never fired on this machine.

Left alone, the model's first write to low RAM turns that stub into whatever
the firmware stored there, and three cores start executing model RAM as
instructions with `VBAR_EL2` still at its reset value of 0. They would not
merely spin: a stray store from one of them lands in the model's RAM, and the
retired-instruction count stops matching the hosted run for reasons nothing in
the transcript would explain.

`aarch64/src/smp.rs` writes the address of a `wfi` park loop *inside the
relocated image* into each release slot, `sev`s, and then waits for each core
to mark its own byte on arrival. The waiting is the point — it turns "they are
probably out of the way" into evidence, printed as

```text
  secondaries 3/3 parked at 0x400000dc (QEMU's spin table at 0xd8)
```

before the model starts. A machine whose stub is not the one above fails the
run rather than guessing, because there is no honest way to overwrite a
gigabyte while not knowing where the other cores are.

## Stage 3: the model in the image

```bash
scripts/qemu-kernel.sh --boot            # builds the bundle from firmware/
scripts/qemu-kernel.sh --boot my.bundle  # or bring your own
```

It reaches the ARM hand-off:

```console
  model RAM   0x00000000 .. 0x40000000 (1024 MiB, identity — see heap.rs)
  heap        0x40200000 .. 0x80000000 (1022 MiB, see aarch64/src/heap.rs)
  CNTFRQ_EL0  62500000 Hz (the max_wall clock)
  initrd      0x08000000 .. 0x19080040 (279040 KiB)
  bundle      copied to 0x40200000 (279040 KiB, out of the model's RAM)
  blob        pieeprom.bin     524288 bytes
  blob        sd.img           285212672 bytes
  secondaries 3/3 parked at 0x400000dc (QEMU's spin table at 0xd8)

--- boot 1 : entry 0x80000200, 1024 MiB model RAM, wall 3580 s ---

  0.00 RPi: BOOTSYS release VERSION:0a7ef05f DATE: 2026/08/04 TIME: 12:38:13
  ...
MESS:00:00:21.562712:0: arm_loader: Starting ARM with 948MB

--- end Stuck { pc: 0x3ec40014, silent_us: 60001330, retired: 68773536 }  pc 0x3ec40014  retired 991737098  skipped 0  console 8215 bytes ---

milestones:
  [ok] EEPROM bootloader running
      "PM_RSTS 00000020"
  [ok] PCIe up, VL805 found
      "PCIe scan 00001106:00003483"
  [ok] start4.elf loaded and logging
      "MESS:"
  [ok] config.txt parsed, second-stage log
      "*** Restart logging"
  [ok] ARM hand-off — the whole boot
      "arm_loader: Starting ARM with 948MB"
heap        287 MiB live, 287 MiB peak of 1022 MiB

the VPU model reached the ARM hand-off bare-metal
qemu -kernel check passed
```

`retired 991737098` is the same instruction count the hosted `recon
--ram-mb 1024` reports for this boot, to the instruction, and the 8215 console
bytes diff clean against the hosted `boot.log.console` from the same blobs. The
run takes about 35 minutes on a 12-core dev box against 55 seconds hosted — the
interpreter is itself running under TCG — which is why `--boot` raises the
wall budget to an hour and why the hosted path stays the one CI blocks on.

The image runs the same `Emulator` over the same `Machine` as the hosted
`recon`, from the same crate — `aarch64/` depends on `rpi-virt-fw` with
`default-features = false`, which is the `no_std` + `alloc` build from stage 1.
`aarch64/src/model.rs` is the whole of the difference, and it is short: build
the payload from the EEPROM image, attach the SD card, set `RunLimits`, run,
tick off milestones.

Four things `src/` deliberately leaves to a frontend, and where each one is:

| what | where | why it matters |
| --- | --- | --- |
| a `#[global_allocator]` | `heap.rs` | `Machine::new` is a `vec![0; ram_bytes]`; without a heap nothing allocates at all |
| a microsecond clock | `clock.rs` | with no source installed `time::Stopwatch` reads zero forever and `RunLimits::max_wall` silently never trips |
| the diagnostic and console sinks | `main.rs` | `diag_eprintln!` and the modelled UART are dropped on the floor until a frontend takes them |
| a `DiagConfig` | `model.rs` | `from_env` is hosted-only; `DiagConfig::quiet()` is the bare-metal equivalent |

### The memory map

`raspi4b` has 2 GiB of RAM at physical 0, will not take `-m` (*"Invalid RAM
size, should be 2 GiB"*), and hands us no memory map — the MMU is off and every
claim on RAM is by convention. The convention, bottom-up:

| range | size | who |
| --- | --- | --- |
| `0x0000_0000` .. `0x4000_0000` | 1 GiB | **the model's RAM**, at the address the firmware believes RAM starts at |
| `0x4000_0000` .. `0x4020_0000` | 2 MiB | this image after it relocates itself, `.bss` and both stacks included |
| `0x4020_0000` .. `0x8000_0000` | 1022 MiB | **the heap** |

and transiently, before the model claims the first gigabyte — all of it inside
that gigabyte, and all of it consumed by the time it does:

| range | who |
| --- | --- |
| `0x0` .. `0x1000` | QEMU's boot stub, the ATAG list at `0x100`, the secondary-CPU release table at `0xd8` and its stub at `0x300` |
| `0x8_0000` | where the firmware puts the ARM kernel — kept free for stage 4 to `ERET` into |
| `0x20_0000` .. | the image as QEMU loaded it, dead once the entry stub has copied it up |
| `0x800_0000` .. | the `-initrd` bundle: QEMU places it at `loader_start + MIN(ram_size / 2, 128 MiB)` |

**The model's RAM is physical memory, identity, no translation.** Stage 4 puts
Linux at EL1 under stage-2 translation, which could map a guest IPA of 0 onto a
block of ours wherever it landed — but device DMA cannot be mapped. There is no
SMMU here, so a QEMU device Linux programs writes *physical* addresses, and an
address the firmware handed out is only writable if the model's backing store
is at that physical address. Identity is the only arrangement in which the
guest's IPA, the model's address and the machine's physical address are the
same number. Doing it now rather than during stage 4 is deliberate: the
alternative breaks at the first DMA-capable peripheral the guest programs,
which is a long way from anything that would point at the cause.

**1 GiB** is not a comfortable round number but the exact size of the address
space the model can reach: `Machine::fold_ram_addr` masks every RAM address
with `0x3FFF_FFFF` to collapse the four VC4 cache aliases onto one backing
store, so anything above 1 GiB is unreachable. (The hosted `recon --eeprom`
default of 2048 MiB is slack, and `recon --ram-mb 1024` reaches `arm_loader`
exactly as the default does — checked before relying on it.)

Which leaves the rest of the arithmetic: 2 GiB total, less the model's first
gigabyte, is 1 GiB for us. The image needs 360 KiB and is given 2 MiB,
`ASSERT`ed in `link.ld` against `heap::HEAP_BASE`. The heap gets the remaining
1022 MiB, and its largest tenant is a 272 MiB copy of the bundle.

**The bundle has to be copied.** QEMU puts `-initrd` at 128 MiB, inside the
model's gigabyte, and `BundleBlocks` serves SD sectors straight out of those
bytes for the whole boot — so they have to survive it. There is nowhere QEMU
could have put it that was out of the way: it places the blob after the kernel
image and will not go above the 960 MiB ARM split. So `main.rs` copies the
blob into the heap before the model starts, which is 272 MiB of memcpy once
against a boot that runs for half an hour. What the image still checks, rather
than assumes, is that the blob did not land on *the image itself* — a bigger
bundle moves that closer with no warning, and the overlap would present as the
firmware reading corrupted blob bytes a long way downstream.

The allocator is a bump pointer with LIFO rollback — freeing the most recent
block un-bumps it, growing it in place extends it, which is what keeps the
console `Vec` from copying itself as it doubles. It does not recycle anything
else, so the run reports its high-water mark at the end and the margin is a
number rather than a hope. `alloc_zeroed` skips zeroing memory that has never
been handed out, since QEMU's guest RAM is an anonymous mapping and comes up
zero; the image samples the window at startup so that assumption is tested
rather than trusted. The model's gigabyte gets no such shortcut — the
bootloader's leavings are all over it — so `Ram::over_region` zeroes it
outright, which is what makes a bare-metal boot retire the same instructions as
a hosted one.

### Physical zero is not expressible in safe Rust

The model's RAM starts at address 0, and that is where the language stops
helping: a Rust reference may not be null, and `ptr::copy_nonoverlapping` and
friends require a non-null pointer too. To the compiler, naming address 0 is a
promise that the code is unreachable — and it acts on it.

It acted on it twice while this was written. `slice::from_raw_parts_mut(0,
1 GiB)`, and then `write_bytes(0 as *mut u8, ..)`, each let LLVM propagate the
non-null precondition backwards until the entire VPU model was dead code:
`.text` fell from 276 KiB to 40 KiB and the image printed its banner, skipped
the boot it exists to run, and exited 0.

Two things follow, and they are the reason `src/mem.rs` looks the way it does:

* `Ram` holds a raw pointer and a length, not a slice, and hands out no
  references into the region — `read_into` copies out. The hosted case keeps a
  `Vec` in an `owner` field purely so the allocation is still freed when the
  `Machine` is dropped; `Box::leak`ing it to make both cases `&'static mut
  [u8]` was rejected because it turns every `Machine` into a permanent 1-2 GiB
  leak, which a five-reboot `recon` run and the regression harness both notice.
* `heap::model_ram()` launders the address through an empty `asm!` with an
  `inout` operand — the identity function the optimiser cannot see through —
  so that nothing downstream ever has a pointer it can prove is null. The
  address being 0 is a fact about the machine, not a fact the compiler is
  entitled to reason from.

### Getting the blobs in

`raspi4b` takes exactly one blob of ours. `-pflash` is refused, a second
`-drive if=sd` is refused, and the machine has no virtio transport. So
`scripts/make-blob-bundle.sh` packs `pieeprom.bin` and `sd.img` into one
`RVFB` container, `-initrd` carries it, and `aarch64/src/bundle.rs` reads the
blobs **in place** — `BundleBlocks` serves SD sectors straight out of the initrd
rather than copying a 272 MiB image into a heap that is already mostly the
model's RAM.

Finding it is the one genuinely surprising part. QEMU only builds a device tree
when the machine supplies one or `-dtb` names one, and `raspi4b` does neither —
so it falls back to `set_kernel_args()`, which writes an **ATAG list** at
`0x100` and passes that in `x0`. Verified on QEMU 10.2.1: `ATAG_CORE`,
`ATAG_MEM`, `ATAG_INITRD2` with the blob's address and length, and no
`d00dfeed` anywhere. `aarch64/src/initrd.rs` reads a device tree when there is
one (through `rpi_virt_fw::fdt`, the crate's own parser — it is already
`no_std`) and the ATAG list when there is not, so the image is not tied to this
machine.

This is a stopgap with a known end: the SD image belongs behind QEMU's own SD
controller, reached a sector at a time through `block::BlockDevice`, which is
what that trait exists for and what makes the EEPROM's self-update persist to a
file. Until the image has an SDHCI driver, `-initrd` carries it.

### What it asserts

Not the golden transcript. `cargo test` and `scripts/boot-check.sh` stay hosted
— 46 milestones, a 141-line transcript diffed line by line — and they remain the
primary test path. The image checks an ordered list of five console landmarks
(`aarch64/src/model.rs`), which answers the one question stage 3 asks: does the
firmware get as far here as it does there? When it does not, the ticked list
says where it stopped instead of leaving a wall of UART output to read.

### Budget

A full boot retires 991,737,098 VPU instructions and the interpreter is itself
running under TCG here, so `--boot` raises the default `RVF_KERNEL_WALL` to
3600 seconds — measured at about 35 minutes, with headroom for a slower
machine. The image carries its own budget too, baked in by `build.rs`
from the same variable and set 20 s lower, because a guest cannot ask QEMU how
long `timeout` will give it — and a wedged boot that stops itself prints a
report, while one that gets killed does not.

## Layout

| file | what it is |
| --- | --- |
| `aarch64/src/boot.rs` | the 64-byte arm64 Image header and the entry stub (relocation, `VBAR_EL2`, stack, `.bss` clear, placement evidence, the secondary-CPU park) |
| `aarch64/src/main.rs` | `rvf_main`, the banner, the self-checks, the frontend seams, and the `#[panic_handler]` |
| `aarch64/src/model.rs` | stage 3: build the machine from the bundle, run it, tick off milestones |
| `aarch64/src/heap.rs` | the `#[global_allocator]` and the RAM window it owns |
| `aarch64/src/clock.rs` | `CNTPCT_EL0`/`CNTFRQ_EL0` as the run loop's wall clock |
| `aarch64/src/initrd.rs` | where the bootloader put `-initrd`, from a device tree or an ATAG list |
| `aarch64/src/bundle.rs` | the `RVFB` container format and the borrowed-in-place `BlockDevice` over it |
| `aarch64/src/smp.rs` | moving cores 1-3 out of the model's RAM, and checking that they went |
| `aarch64/src/vectors.rs` | the EL2 vector table and the fault report |
| `aarch64/src/semihost.rs` | `SYS_EXIT_EXTENDED`, and the exit statuses the script asserts on |
| `aarch64/src/uart.rs` | PL011 transmit at `0xFE20_1000`, register names shared with `src/periph/uart_pl011.rs`; `puthex`/`putdec` for the `core::fmt`-free fault path |
| `aarch64/link.ld` | load and link addresses (and why they differ), section order, `.bss`, the two stacks, `_image_size`/`_load_size` |
| `aarch64/build.rs` | passes the linker script by absolute path, bakes in the wall budget |
| `scripts/make-blob-bundle.sh` | packs the firmware blobs into the one file `-initrd` can carry |

## Where the EEPROM lives

On hardware the EEPROM is a separate SPI NOR chip, and it is writable:
`pieeprom.upd` rewrites it mid-boot. The hosted tool can take the image from a
file and, with `RVF_DUMP_FLASH`, drop the modified bytes somewhere afterwards.
Bare-metal there is no file and nowhere to drop anything, and `raspi4b` accepts
no second medium to model the chip with:

| tried | result |
| --- | --- |
| `-drive if=sd,index=1` | `machine type does not support if=sd,bus=0,unit=1` |
| `-device virtio-blk-pci` | `No 'PCI' bus found` |
| `-device virtio-blk-device` | `No 'virtio-bus' bus found` |
| `-pflash` | `machine type does not support if=pflash,bus=0,unit=0` |
| `-device usb-storage` | accepted, but consuming it needs an xHCI or DWC2 host driver plus USB MSC |

So the EEPROM gets a partition on the one drive QEMU does take, and both live on
`-drive if=sd,file=sd.img,format=raw`. QEMU writes through to the file, so a
self-update survives across runs the way a flash burn does.

`scripts/make-sd.sh` writes:

| # | start LBA | sectors | type | contents |
| --- | --- | --- | --- | --- |
| 1 | 2048 | 522240 | `0x0c` FAT32 LBA, bootable | `start4.elf`, `fixup4.dat`, `config.txt`, `dt-blob.bin`, the dtb, the kernel, `overlays/` |
| 2 | 524288 | 32768 | `0xda` non-FS data | the raw EEPROM image, padded to 16 MiB with `0xFF` |

Partition 2 is *appended*; partition 1 is byte-for-byte what it was before it
existed. That is deliberate. The firmware prints the partition table and the FAT
geometry it derives from it, so carving the EEPROM out of the boot partition
would have moved `FAT32 clusters`, `fat-sectors` and the cluster count in the
golden boot transcript — a change to the workload disguised as a change to the
layout. Growing the image instead moves exactly two golden lines, both of them
the firmware describing the medium rather than behaving differently:

```text
-CSD: 400e005a05b5900001ff000002600000     # C_SIZE 0x1ff = 256 MiB
+CSD: 400e005a05b59000021f000002600000     # C_SIZE 0x21f = 272 MiB
-[t] MBR: 0x00000000,       0 type: 0x00   # slot 2 was empty
+[t] MBR: 0x00080000,   32768 type: 0xda   # slot 2 is the EEPROM
```

(SDHC encodes capacity as `C_SIZE = bytes / 512 KiB - 1`, so 511 → 543.) All 46
milestones and the published `rpi-machine-id` are unchanged.

The size is the part the model claims to be: `RDID` in `src/periph/spi0.rs`
answers a Winbond W25Q128, 16 MiB. `pieeprom.bin` is 512 KiB of that; the rest
reads back as erased flash, which is what the unused address space of a real
chip does.

### How it reaches `Spi0`

`block::Window` is the whole adapter — a `BlockDevice` over a `BlockDevice`,
addressed from zero, refusing any access past its end so a runaway EEPROM write
cannot reach the FAT partition the firmware is about to boot from.

`Spi0` is not a block device and does not become one: serial NOR is byte
addressed, with 4 KiB sector erase and bit-clearing page program, and the
working copy stays a `Vec<u8>`. What the backing store adds is durability. The
image is read out of it once (`attach_flash_medium`), and `flush_flash()`
writes back the sectors that actually differ when the `dirty` flag says the
firmware erased or programmed something. `RVF_DUMP_FLASH` becomes the
degenerate case of the same idea rather than a second mechanism.

Hosted, the same path is available with `--eeprom-part <n>`:

```console
$ rpi-virt-fw recon --eeprom-part 2 --sd firmware/sd.img
eeprom     firmware/sd.img partition 2 @ LBA 524288 (32768 blocks)
```

It reports an image byte-identical to `recon firmware/pieeprom.bin --eeprom`.
The file-based form is untouched and stays the primary one: it is how every
scenario and `scripts/boot-check.sh` run, with no SD image and no QEMU involved.

**Not yet exercised end to end.** The write-back is covered by tests in
`src/block.rs` and `tests/peripherals.rs`, but no *modelled* boot has reached a
self-update to trigger it: the bootloader's `find_files` reports
`[sdcard] pieeprom.upd not found` even with the file on the FAT and placed in
the first directory sector, so the burn never starts. That is a firmware-model
gap on the lookup side, not on this one, and a boot that never writes leaves the
partition byte-identical — verified across full runs.

## What stage 4 needs from this

* **Stage-2 translation tables**, which is the actual work: unmap nothing at
  first, `ERET` into the kernel the firmware loaded at `0x8_0000`, and check
  that Linux boots exactly as it does under plain `raspi4b`. Everything below is
  already in place for it.
* ~~**RAM the guest and the models agree about.**~~ Done: the model's RAM *is*
  physical `0x0` .. `0x4000_0000`, so a guest IPA, a model address and a machine
  physical address are the same number and device DMA needs nothing from us. See
  [the memory map](#the-memory-map).
* **A way back for the secondary CPUs.** Linux brings up the Pi's cores through
  the spin table (`enable-method = "spin-table"`, `cpu-release-addr = <0xd8>`),
  and `0xd8` is now a location *inside the model's RAM* that the firmware also
  writes. Cores 1-3 are parked in a `wfi` loop in this image and deliberately
  poll nothing, so the guest's releases will have to be noticed by the resident
  — a trapped write, or the model reporting the store — rather than by a core
  watching model memory and launching on whatever the firmware happened to put
  there. `aarch64/src/smp.rs` has the detail.
* **An SDHCI driver**, so the SD image comes through `-drive if=sd` and
  `block::BlockDevice` instead of `-initrd`, and the EEPROM's self-update
  persists to the file the way it does on real SPI flash.
* **A fault handler that is already installed**, and now covers a billion
  interpreted instructions rather than a banner: anything that goes wrong gets
  `ESR_EL2`/`ELR_EL2`/`FAR_EL2`/`SPSR_EL2` and the general registers on the
  console, plus a distinct exit status.
