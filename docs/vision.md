# Vision & roadmap

Where this is heading, beyond the current milestone work in
[`boot-chain.md`](boot-chain.md). Nothing here is committed API — it's the
target we're aiming the design at.

---

## 1. One command, all the blobs — done

The target was a single command that takes the firmware the way a real Pi
does, with no `--entry` / `--exc-vbase` / `--patch`. That is `boot` now (#57):

```
rpi-virt-fw boot --eeprom pieeprom.bin --sd sdcard.img
```

- The boot ROM stage verifies and stages the bootcode, and the EEPROM
  bootloader works out load addresses, entry points and the SDRAM alias itself,
  as on the hardware.
- `fixup4.dat` is applied by the firmware, not by the bench.
- The output is the serial console and one `result:` line (#55); `-v` adds the
  run report, and any option can come from a JSON or TOML file (#47).

---

## 2. Disk-image mode — done

It turned out not to need a separate mode: the model runs the real
second-stage bootloader, and that is what reads the EEPROM config
(`BOOT_ORDER`, …), walks the partition table, mounts the FAT boot partition and
loads `start4.elf`, `fixup4.dat`, `config.txt`, the overlays and the DTB out of
the image. So a change in that logic – a firmware bump, a `config.txt` edit, a
partition-layout change – shows up end to end.

The media are `--sd <img>` and `--usb <img>`, both read on demand (#54), and
`--netboot <dir>` for TFTP and HTTP boot off a built-in network peer (#38).

---

## 3. The whole machine: the ARM is ours too

Since #40 the bench is a whole-machine Pi 4 emulator, Bochs-style: the four
Cortex-A72 cores are interpreted in the same process as the two VPU cores and
kept in lock-step with them, so a run stays deterministic and a golden
transcript stays meaningful.

This replaced the earlier plan, which was to keep executing `start4.elf` next to
QEMU's `raspi4b` and bridge the ARM property mailbox between the two. The
thing [`rpi-mkosi` #37][37] cares about – `rpi-fw-crypto`, `/dev/vcio`, the
OTP-backed key derivation – is a live conversation between a booted Linux and a
still-running VideoCore, and the QEMU route fell short of carrying it:

- The mailbox bridge was the hard part. `raspi4b` has no PCI bus (so no
  `ivshmem`) and no virtio-mmio (so no vhost-user), which left a patched QEMU
  or a guest-side shim. With our own ARM, `0x7E00_B880` is an in-process device
  between the A72 and `start4`'s mailbox task.
- Without a working mailbox Linux stopped at `Waiting for root device`
  (`raspberrypi-exp-gpio`, `regulator-sd-io-1v8` and the SD controller all
  depend on the firmware).
- `raspi4b` caps guest RAM at 2 GiB, models no HDMI/VC4/PCIe/GENET, can't start
  from `-bios`, and rewrites parts of the device tree it is given, so Linux
  would not have seen what the firmware produced.
- Most of the ARM-side device surface was modelled already, because start4
  touches it: EMMC2, GENET, PCIe and the VL805, the RNG, DMA, the PL011. And
  AArch64 is documented, which VC4 is not – the CPU was the easier half.

### Where it stands

`boot --arm` releases the cores when `arm_loader` writes the ARM control block,
at PC 0 in EL3 like the SoC. The firmware's own armstub drops them to EL2, and
Linux boots off the SD card's ext4 root to a shell on the serial console
(`testdata/boot/linux-boot.toml`, run by CI). From that shell Raspberry Pi's
`rpi-fw-crypto` asks start4 for `GET_CRYPTO_HMAC_SHA256` through
`/dev/vcio_crypto` and gets the same digest as the firmware-only boot. UEFI
(edk2) and a systemd-boot + UKI image boot as well, from SD or USB, if slowly.

### What is left

- **Snapshot and restore at `arm_loader`** (#50), so a Linux boot doesn't re-run
  the firmware every time.
- **A debugger** – breakpoints, watchpoints, a gdb stub for both kinds of core –
  to replace most of the `RVF_*` probes in `docs/diagnostics.md`.
- **Fast-forwarding the VPU's idle loop** once Linux is up.
- **Always modelling the ARM** (#52): a firmware-only boot then ends in a kernel
  that halts, instead of the `--arm` switch.

The line between the two sides is the SoC's own now – the mailbox, the
doorbells and shared DRAM – all inside one process.

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
