# The VideoCore inside QEMU

`qemu/patches/` teaches QEMU's `raspi4b` machine about the BCM2711's second
processor. With them, and this crate linked in as a library:

```bash
scripts/build-qemu.sh                 # clone v10.2.1, patch, build qemu/build/qemu-system-aarch64
scripts/qemu-boot.sh path/to/sd.img   # or, by hand:
qemu/build/qemu-system-aarch64 \
  -machine raspi4b,videocore=firmware/pieeprom.bin,videocore-sd=sd.img \
  -drive if=sd,file=sd.img,format=raw -display none -serial stdio
```

boots the way the board does. There is no `-kernel` and no `-dtb`: the real
`pieeprom.bin` runs on the modelled VPU over the guest's RAM, reads
`config.txt`, `start4.elf`, the kernel, the device tree and the overlays off
the card, places them in memory, and releases the four Cortex-A72s out of
reset at physical 0 in EL3 — into `armstub8`, or into `RPI_EFI.fd`'s TF-A,
whatever the card carries. From then on the firmware stays resident and
answers the ARM's mailbox requests from the same models `recon` drives.

## Why in-process

The alternative was a hosted `rpi-virt-fw` process beside a stock QEMU,
talking over shared memory. Stock QEMU has no way to forward MMIO out of
`raspi4b` (no PCI, so no ivshmem or vfio-user; no dynamic sysbus; TCG plugins
cannot intercept), so a QEMU patch was needed either way. Once it is, putting
the model *inside* is strictly better:

* an ARM access to a firmware-side peripheral is a function call, and the
  firmware's access to a QEMU-side one (`address_space_rw` on the VideoCore
  bus) is another — no ring, no doorbell, no second process to orchestrate;
* RAM is shared by pointer (`memory_region_get_ram_ptr`); the model's `Ram`
  sits on QEMU's guest memory, so the kernel the firmware loads is the kernel
  the ARM executes with no copy;
* peripherals the two processors share on the board can be one device here.
  UART0 already is: the firmware drives QEMU's PL011, so the firmware log and
  the kernel console interleave on one `-serial` exactly as on a real cable;
* the ARM release is `arm_set_cpu_on()` — the PSCI primitive — not a QMP dance;
* interrupts are `qemu_set_irq` on the GIC.

## How it fits into QEMU

One patch, small on purpose:

| file | what |
| --- | --- |
| `hw/misc/rvf_videocore.c` | the device: owns the model, runs it on a thread, exposes the ARM's mailbox aperture and its GIC line |
| `hw/arm/bcm2838_peripherals.c` | creates the device; maps its `0x40`-byte mailbox window over the `bcm2835-mbox`/`bcm2835-property` mocks at `0xfe00b880` |
| `hw/arm/bcm2838.c` | wires the mailbox interrupt to GIC SPI 33 |
| `hw/arm/raspi.c`, `hw/arm/raspi4b.c` | the `videocore=` / `videocore-sd=` machine properties; with them, no boot stub is written and every core starts powered off |
| `meson.build`, `hw/misc/meson.build` | `dependency('rpi-virt-fw', method: 'pkg-config')` → `CONFIG_RVF` |

Without `videocore=` the machine is untouched. Without the pkg-config file
the patch compiles to nothing (`CONFIG_RVF` off), which is what
`scripts/build-qemu.sh` refuses to accept.

**Locking.** The model is not thread-safe. The VPU thread takes the BQL for
each slice of instructions it runs (`slice=`, 2000 by default, ~0.1 ms), and
the ARM's MMIO into the model already runs under the BQL on a vCPU thread —
so the two are mutually exclusive with no second lock, and the model may
reach QEMU devices and raise interrupts from inside a slice. Between slices
the thread yields the lock; after the ARM is released it also paces itself
against `QEMU_CLOCK_VIRTUAL`, so VPU time and ARM time stay within a
millisecond of each other.

**The C ABI** is `include/rvf.h` ↔ `src/capi.rs`, kept in step by hand.
`rvf_vc_new` builds a `Machine` over caller-owned RAM (`Ram::over_raw`),
`rvf_vc_run` is `Emulator::run` in slices, `rvf_vc_mmio_read/write` are the
ARM's accesses, `rvf_vc_add_foreign` sends a VC address range to the host's
`mmio_read/mmio_write` callbacks instead of the model's own peripheral.

## Where it stands

- [x] the model builds as a static library with a C header
- [x] QEMU patch: device, machine properties, mailbox window, GIC line, ARM release
- [ ] first boot under QEMU — the firmware to `arm_loader`, the ARM into the stub
- [ ] Linux's `/dev/vcio` `GET_FIRMWARE_REVISION` answered by the live firmware
- [ ] the eMMC as one device: the firmware driving QEMU's SDHCI instead of
      its own card model, so the card the kernel writes is the card the
      firmware reads (`videocore-sd=` goes away)
- [ ] more shared devices where QEMU has one: the system timer, GPIO, DMA
- [ ] more firmware-side windows where QEMU has nothing: OTP (so
      `rpi-machine-id` is the firmware's), thermal, RNG, PCIe/VL805 (#30)
- [ ] a board reset when the firmware asks the PM for one

Known shims, to be removed in that order: the ARM release is detected on the
`arm_loader: Starting ARM` log line rather than on the register write that
does it on the hardware (the release goes through the firmware's power
manager, whose register write has not been pinned yet); the card is read
twice (QEMU's SDHCI for the ARM, the model's `Emmc2` for the VPU).
