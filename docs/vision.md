# Vision & roadmap

Where this is heading, beyond the current milestone work in
[`boot-chain.md`](boot-chain.md). Nothing here is committed API — it's the
target we're aiming the design at.

---

## 1. One command, all the blobs

Today the entry points are `recon` / `run` with a pile of flags and manual
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
- Output is the serial transcript (+ the boot-progress phases, + any captured
  device-tree), suitable for `diff` against a golden or against another
  firmware version.

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

## 3. If we ever reach Linux — hand off to QEMU

Fully emulating the ARM side (kernel, userspace, USB, networking, …) is not the
point of this project and would be a poor use of effort. QEMU already does it
well.

The vision is to make our firmware emulator a **drop-in BIOS/firmware for
QEMU's `raspi4b` (or `virt`) machine**: run the real VideoCore boot chain in our
model, and at the ARM hand-off, transfer the resulting machine state — DRAM
contents, the loaded kernel, the fixed-up device tree, PLL/clock state, the ARM
stub — into QEMU and let it take over from reset.

Two plausible integration shapes:

```
# a) our emulator produces a firmware image QEMU loads as -bios
rpi-virt-fw build-bios --eeprom pieeprom.bin --disk sdcard.img -o rpi-fw.bin
qemu-system-aarch64 -M raspi4b -bios rpi-fw.bin -drive file=sdcard.img,...

# b) our emulator runs first, snapshots at hand-off, resumes under QEMU
rpi-virt-fw boot ... --handoff-snapshot state.tar
qemu-system-aarch64 -M raspi4b -incoming-rpi-handoff state.tar ...
```

(b) is closer to how the hardware actually works (the VPU keeps running
alongside the ARM cores, servicing the mailbox); it would need a small
paravirtual channel for the VideoCore mailbox / property interface so a booted
Linux can still talk to "the firmware". (a) is simpler and probably enough for
CI-style "does it boot" checks.

Either way: **we own everything up to the ARM cores leaving reset; QEMU owns
everything after.** The firmware crypto / mailbox / device-tree behaviour —
the part `rpi-mkosi` #37 cares about — is entirely on our side of that line.

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
