# Vision & roadmap

Where this is heading, beyond the current milestone work in
[`boot-chain.md`](boot-chain.md). Nothing here is committed API — it's the
target we're aiming the design at.

---

## 1. One command, all the blobs

Today the entry points are `boot` / `run` with a pile of flags and manual
`--patch` arguments. The target is a **single command** that takes the firmware
set the way a real Pi consumes it:

```
rpi-virt-fw boot \
    --eeprom   pieeprom.bin \
    --firmware start4.elf \
    --fixup    fixup4.dat \
    [--config  config.txt] \
    [--dtb     bcm2711-rpi-4-b.dtb]
```

- No `--entry` / `--exc-vbase` / `--patch`: the boot-ROM approximation and the
  EEPROM loader figure out load addresses, entry points and the SDRAM alias
  themselves, exactly as the hardware does.
- `fixup4.dat` is *applied*, not ignored — it sets the GPU/CPU memory split and
  patches `start4.elf` in place.
- Output is the serial transcript (+ any captured device-tree; a
  `--features diag` build adds start4's boot-progress tags), suitable for
  `diff` against a golden or against another firmware version.

This is the shape the regression bench and the `/chosen` diff tool both plug
into.

---

## 2. Disk-image mode — "as if the EEPROM were already flashed"

Longer term, drop the requirement to hand over `start4.elf` / `fixup4.dat`
separately. Give the emulator only what a provisioned device has:

```
rpi-virt-fw boot \
    --eeprom pieeprom.bin \
    --disk   sdcard.img          # or a raw block device / qcow2
```

The emulator then does what the real second-stage bootloader does:

1. Parse the EEPROM config (`BOOT_ORDER`, `BOOT_PARTITION`, `TRYBOOT_*`, …).
2. Walk the MBR/GPT partition table on the image.
3. Mount the boot partition (FAT32), honour `os_prefix` / `[partition]` /
   `autoboot.txt` / `tryboot.txt` selection logic.
4. Read `start4.elf`, `fixup4.dat`, `config.txt`, overlays, the DTB — straight
   out of the image.
5. Continue the boot from there.

At that point the emulator *is* the firmware: the same media-selection and
config-parsing logic the Pi runs, so a change in that logic (a firmware bump, a
`config.txt` edit, a partition-layout change) is observable end to end.

Netboot (`BOOT_ORDER` TFTP/HTTP) is the same idea with a mock network backend
instead of a disk image — lower priority.

---

## 3. Reaching Linux — keep the VideoCore running alongside QEMU

We reach Linux now: the boot runs to `arm_loader: Starting ARM with 948MB`, and
`boot --dump-fdt` yields a device tree a stock
`qemu-system-aarch64 -M raspi4b -m 2G -kernel kernel8.img -dtb handoff.dtb`
boots the real kernel from, as far as `Waiting for root device`.

Fully emulating the ARM side is still not the point. But a *static* hand-off is
not enough either: the thing [`rpi-mkosi` #37][37] cares about — `rpi-fw-crypto`,
`/dev/vcio`, the OTP-backed key derivation — is a **live** conversation between a
booted Linux and a still-running VideoCore. So the target is: **our model keeps
executing `start4.elf` alongside QEMU and services the ARM property mailbox for
real.**

### What is settled

- **Guest DRAM can be shared, with stock QEMU.**
  `-object memory-backend-file,id=pcram,size=2G,mem-path=…,share=on` plus
  `-machine memory-backend=pcram` works on `raspi4b`; a second process sees guest
  writes live and can write back. Property buffers arrive as `0xC000_0000 | phys`
  (`/soc` carries `dma-ranges = <0xc0000000 0x0 0x0 0x40000000>`), which is the
  uncached SDRAM alias the model already implements — so a shared mapping needs
  no address translation at all.
- **The hand-off is a file copy, not a subsystem.** `boot --dram-map` reports
  39 MiB non-zero in 24 regions at `arm_loader`: the armstub and spin table at
  `0x0..0x1b000`, the kernel at `0x200000`, the patched DTB at `0x2eff1e00`, and
  `start4`'s own image around `0x3ebe4000`. Nothing above `0x4000_0000`. The
  firmware's image has to stay live regardless — `/reserved-memory/nvram@0` and
  `nvram@1`, which Linux reads as `rpi-bootloader-config` and
  `rpi-bootloader-public-key`, point straight into VPU DRAM.
- **`-bios` does not work on `raspi4b`.** The image is copied to `0x80000`, but
  CPU0 resets to PC = 0 in EL3 secure and QEMU writes nothing at 0, so the guest
  executes zeros. The "build a BIOS image" shape is dropped. That reset state is
  exactly right for a restore, though — it is where `armstub8` starts on
  hardware — and `-device loader,file=…,addr=…` (with `cpu-num=` for the reset
  PC) restores memory and entry point with no patched QEMU.
- **The device tree is accepted.** Our `--dump-fdt` blob boots Linux 6.18 to the
  same point as the stock `bcm2711-rpi-4-b.dtb`, with `Attached to firmware` and
  `mailbox enabled`.

### What is not

- **The mailbox bridge.** `0x7E00_B880` is in the memory map but unmodelled, and
  nothing forwards its MMIO out of QEMU. `raspi4b` has no PCI bus (so no
  `ivshmem`) and no virtio-mmio (so no vhost-user), which leaves either a small
  custom QEMU device over a socket — shipping a patched QEMU — or a guest-side
  driver shim, the way `rpi-mkosi`'s own `rpi-fw-mock` already does it, with our
  model behind it instead of hardcoded constants.
- **Without a working mailbox Linux stops at `Waiting for root device`.** The
  device tree alone is not enough: `raspberrypi-exp-gpio` fails
  `GET_GPIO_CONFIG`, `regulator-sd-io-1v8` fails to probe, and the SD/eMMC
  controller defers forever. Firmware clocks, power domains, cpufreq, thermal
  and `vcio` go with it.
- **`raspi4b` caps guest RAM at 2 GiB** and models no HDMI/VC4/PCIe/GENET, so a
  device tree carrying `dtoverlay=vc4-kms-v3d` aborts the guest in
  `brcmstb_l2_intc_probe` (the HDMI L2 interrupt controller at `0x7ef00100`).
  QEMU also rewrites parts of a supplied DTB on the way in, so what Linux sees
  is not byte-identical to what the firmware produced.

### Order of work

**Model the ARM mailbox in our own machine first**, and prove `start4`'s mailbox
task — it exists, the blob says `Creating mailbox reading task ...` — answers
`GET_FIRMWARE_REVISION` and then `GET_CRYPTO_HMAC_SHA256`, with no ARM anywhere
in the picture. That single step answers #37 in CI in one `boot` run. Only then
is it worth choosing a QEMU transport, because the transport is a deployment
detail on top of a proven mailbox rather than the thing the idea is gated on.

Either way: **we own everything the VideoCore does; QEMU owns the ARM.** The line
is the mailbox, not the reset vector.

[37]: https://github.com/valtzu/rpi-mkosi/issues/37

---

## Why bother (the concrete driver)

[`rpi-mkosi` #37](https://github.com/valtzu/rpi-mkosi/issues/37): firmware and
EEPROM are bumped on a schedule via automated PRs. A bump that changes how
`start4.elf` derives `rpi-machine-id` (or how the crypto mailbox responds)
silently changes the derived root-LUKS passphrase and bricks the update. CI
needs to run old-vs-new firmware through an identical model and diff:

- `/proc/device-tree/chosen/*` (rendered from the DTB the firmware hands Linux)
- the `rpi-fw-crypto` HMAC mailbox responses for a fixed mock OTP

See [`references.md`](references.md) for the prior-art survey — nothing
off-the-shelf executes these blobs.
