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
image: target/aarch64-unknown-none/release/vc4-to-aarch64.img (276720 bytes)

rpi-virt-fw: aarch64 frontend (issue #32 stage 3)
  CurrentEL   EL2
  MPIDR_EL1   0x0000000080000000
  DTB (x0)    0x0000000000000100
  image       0x00200000 .. 0x00257a20 (0x57a20 bytes)
  bss         0x002438f0 .. 0x00243a20   stack top 0x00253a20
  UART0       0xfe201000 (PL011)
  VBAR_EL2    0x00235800 (16 x 0x80, see aarch64/src/vectors.rs)
  heap        0x20000000 .. 0x80000000 (1536 MiB, see aarch64/src/heap.rs)
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

## Stage 3: the model in the image

```bash
scripts/qemu-kernel.sh --boot            # builds the bundle from firmware/
scripts/qemu-kernel.sh --boot my.bundle  # or bring your own
```

It reaches the ARM hand-off:

```console
  heap        0x20000000 .. 0x80000000 (1536 MiB, see aarch64/src/heap.rs)
  CNTFRQ_EL0  62500000 Hz (the max_wall clock)
  initrd      0x08000000 .. 0x19080040 (279040 KiB)
  blob        pieeprom.bin     524288 bytes
  blob        sd.img           285212672 bytes

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
heap        1039 MiB live, 1039 MiB peak of 1536 MiB

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

`raspi4b` has 2 GiB of RAM at physical 0 and no MMU on, so every claim on it is
by convention. Bottom-up:

| range | who |
| --- | --- |
| `0x0` .. `~0x1000` | QEMU's own boot stub, written at `loader_start` |
| `0x8_0000` | where the firmware puts the ARM kernel — kept free for stage 4 to `ERET` into |
| `0x20_0000` .. `_end` | this image, stacks included |
| `0x800_0000` .. | the `-initrd` bundle: QEMU places it at `loader_start + MIN(ram_size / 2, 128 MiB)` |
| `0x2000_0000` .. `0x8000_0000` | **the heap**, 1.5 GiB |

The model asks for 1 GiB of it. That is not a comfortable round number but the
exact size of the address space it can reach: `Machine::fold_ram_addr` masks
every RAM address with `0x3FFF_FFFF` to collapse the four VC4 cache aliases onto
one backing store, so anything above 1 GiB is unreachable. (The hosted
`recon --eeprom` default of 2048 MiB is slack, and `recon --ram-mb 1024` reaches
`arm_loader` exactly as the default does — checked before relying on it.)

512 MiB is where the heap starts because the bundle carries the 272 MiB SD
image, so the initrd runs to about 400 MiB. That is checked and not assumed: the
image reads the initrd extent out of the boot arguments and fails with a named
error if it reaches into the window, because a bigger bundle would otherwise
present as the firmware reading corrupted blob bytes a long way downstream.

The allocator is a bump pointer with LIFO rollback — freeing the most recent
block un-bumps it, growing it in place extends it, which is what keeps the
console `Vec` from copying itself as it doubles. It does not recycle anything
else, so the run reports its high-water mark at the end and the margin is a
number rather than a hope. `alloc_zeroed` skips zeroing memory that has never
been handed out, since QEMU's guest RAM is an anonymous mapping and comes up
zero; the image samples the window at startup so that assumption is tested
rather than trusted.

This matters beyond convenience. #32 depends on the model's RAM *being* machine
physical memory: QEMU's devices DMA into it with no SMMU in the way, so once
stage 4 lets Linux program a DMA engine, an address the firmware handed out has
to be one QEMU's devices can actually write.

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
| `aarch64/src/boot.rs` | the 64-byte arm64 Image header and the entry stub (CPU park, `VBAR_EL2`, stack, `.bss` clear, placement evidence) |
| `aarch64/src/main.rs` | `rvf_main`, the banner, the self-checks, the frontend seams, and the `#[panic_handler]` |
| `aarch64/src/model.rs` | stage 3: build the machine from the bundle, run it, tick off milestones |
| `aarch64/src/heap.rs` | the `#[global_allocator]` and the RAM window it owns |
| `aarch64/src/clock.rs` | `CNTPCT_EL0`/`CNTFRQ_EL0` as the run loop's wall clock |
| `aarch64/src/initrd.rs` | where the bootloader put `-initrd`, from a device tree or an ATAG list |
| `aarch64/src/bundle.rs` | the `RVFB` container format and the borrowed-in-place `BlockDevice` over it |
| `aarch64/src/vectors.rs` | the EL2 vector table and the fault report |
| `aarch64/src/semihost.rs` | `SYS_EXIT_EXTENDED`, and the exit statuses the script asserts on |
| `aarch64/src/uart.rs` | PL011 transmit at `0xFE20_1000`, register names shared with `src/periph/uart_pl011.rs`; `puthex`/`putdec` for the `core::fmt`-free fault path |
| `aarch64/link.ld` | load address, section order, `.bss`, the two stacks, `_image_size` |
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
* **RAM the guest and the models agree about.** The model's RAM is a heap block
  at `0x2000_0000`-something, while the firmware believes it starts at 0. Stage 2
  translation can map a guest IPA of 0 onto wherever the block landed, but device
  DMA bypasses stage 2 and uses physical addresses — so the first peripheral the
  guest programs to write into "RAM" is where that indirection stops being free.
  The long-run answer is a heap window whose base the model's `Ram` is
  constructed at, rather than one it is merely allocated from.
* **An SDHCI driver**, so the SD image comes through `-drive if=sd` and
  `block::BlockDevice` instead of `-initrd`, and the EEPROM's self-update
  persists to the file the way it does on real SPI flash.
* **A fault handler that is already installed**, and now covers a billion
  interpreted instructions rather than a banner: anything that goes wrong gets
  `ESR_EL2`/`ELR_EL2`/`FAR_EL2`/`SPSR_EL2` and the general registers on the
  console, plus a distinct exit status.
