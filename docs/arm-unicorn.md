# Running Linux against the live firmware, in-process

The simulated boot ends with `arm_loader: Starting ARM with 948MB` and the
firmware parked in its ThreadX idle loop, still answering the property mailbox
(#23). What is missing is the thing on the other side: an ARM that runs the
kernel `start4.elf` just loaded and talks to the firmware the way Linux does.

This adds one, in the same process as the VPU model: a Unicorn 2 (QEMU TCG under
the hood) aarch64 core sharing the model's `Machine`.

## Why in-process

The alternative, #32, runs the emulator as a bare-metal `-kernel` inside QEMU's
`raspi4b` and traps Linux's MMIO at EL2 (PR #33 takes it to the ARM hand-off).
Its advantage is reusing QEMU's peripheral models; its cost is sharing a
physical address space with the guest — a `no_std` port, relocating the image
out of the guest's RAM, and a model RAM at physical 0 that the compiler treats
as null.

In-process has none of that. The guest's "physical" RAM is our buffer, mapped
wherever Unicorn likes, the hosted tooling (`boot-check`, the golden
transcript, `cargo test`) keeps working unchanged, and runs stay deterministic.
The price is that nothing is passed through: every peripheral Linux touches is
ours to model, starting with the interrupt controller and the timer.

## Shape

* **RAM** — `mem_map_ptr` over the model's `Ram`, so a store from either core is
  visible to the other with no copy. Nothing may hold a reference into RAM across
  an `emu_start` slice.
* **Peripherals** — the ARM sees them at `0xFE00_0000`, the VPU at
  `0x7E00_0000`. One `mmio_map` over the ARM window translates and dispatches
  into the existing `Bus`, so the PL011, the mailbox, eMMC2 and the rest answer
  both cores from the same state.
* **ARM-local** — the GIC-400 at `0xFF84_0000` and the ARM local block at
  `0xFF80_0000` have no VPU-side counterpart: `src/periph/gic.rs` and
  `src/periph/armlocal.rs`, mapped only on the ARM side.
* **Entry** — the firmware's own armstub at physical 0, in EL3, as the SoC
  does it; the stub enters the kernel at EL2 with `x0` = the dtb.
* **Exceptions** — Unicorn takes *no* exception into the guest (it hands each
  one to a hook and drops it), and it has no API to raise an IRQ line. So every
  exception entry is done by hand — `hvc`, `svc`, `brk` and UNDEFINED now, IRQs
  between slices for milestone 2: `ELR`/`SPSR`/`ESR` saved, `PC` set to the
  vector.
* **Scheduling** — `emu_start(.., count)` runs a fixed number of ARM
  instructions, interleaved with VPU steps, so a run is reproducible.
* **Opt-in** — behind the `arm` cargo feature (`recon --arm`), so the default
  build, the firmware-only tool and CI's existing jobs do not grow a C
  dependency.

`src/arm.rs` carries the details and the evidence for each of these.

## Milestones

1. The kernel executes and its first `earlycon` line comes out of our PL011.
2. GIC and generic timer interrupts delivered — the scheduler runs.
3. A mailbox request from Linux reaches the live firmware and is answered.
4. Root filesystem mounted from the modelled SD card, userspace reached.
