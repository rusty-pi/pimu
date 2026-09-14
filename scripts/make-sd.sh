#!/usr/bin/env bash
# Build a bootable SD-card image for the model: MBR, a FAT32 boot partition
# holding the real Raspberry Pi 4 firmware (start4.elf + fixup4.dat), a config
# and the device tree, and an ext4 root filesystem with busybox for Linux.
# Consumed by `rpi-virt-fw boot --sd <img>`.
#
# No root / loop devices — sfdisk writes the partition table, mtools and
# `mke2fs -d` write the filesystems at byte offsets.
set -euo pipefail
# mtools stamps directory entries with this instead of the build time.
export SOURCE_DATE_EPOCH=315532800   # 1980-01-01, the FAT epoch
# Globs sort by the locale's collation, and the order files are copied in is
# the order of their directory entries. An en_US host put vc4-kms-v3d.dtbo
# before vc4-kms-v3d-pi4.dtbo and the C-locale CI runner the other way round,
# so the firmware found the pi4 overlay one entry later here: 180 us of
# modelled time, enough to reorder two kernel lines in the Linux golden.
export LC_ALL=C

here="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$here/firmware/sd.img}"
fw="$here/firmware"
cache="${RPI_MKOSI_CACHE:-$HOME/src/rpi-mkosi/cache}"

# The boot partition's geometry is in the golden transcript (the bootloader
# prints its FAT cluster count), and the card's size is in it too (the CSD),
# so neither changes casually.
size_mb=256              # MBR + boot partition
root_mb=32               # the root filesystem after it
part_start=2048          # sectors (1 MiB alignment)
sector=512

echo "building $out ($(( size_mb + root_mb )) MiB: FAT32 boot, ext4 root)"
rm -f "$out"
truncate -s "$(( size_mb + root_mb ))M" "$out"

# --- partition table: FAT32 LBA boot (0x0c), then Linux (0x83) -----------------
root_start=$(( size_mb * 1024 * 1024 / sector ))
sfdisk --quiet --label dos "$out" <<EOF
label-id: 0x5250494d
${part_start},$(( root_start - part_start )),c,*
${root_start},,83
EOF

part_offset=$(( part_start * sector ))
part_bytes=$(( size_mb * 1024 * 1024 - part_offset ))

# --- filesystem --------------------------------------------------------------
mformat -i "$out@@${part_offset}" -F -v RPIBOOT -N 5250494d -T $(( part_bytes / sector )) ::

copy() {
  local src="$1" dst="$2"
  if [[ -f "$src" ]]; then
    mcopy -i "$out@@${part_offset}" -o "$src" "::${dst}"
    echo "  + $dst  ($(stat -Lc %s "$src") bytes)"
  else
    echo "  ! missing $src (skipped)" >&2
  fi
}

# The config the reference boot log was captured with (a stock Raspberry Pi OS
# config.txt with the UART console enabled). The dtparam/dtoverlay lines are
# what produce the `dtparam: spi=on` / `Loaded overlay '...'` lines in
# examples-on-real-hardware/vc4-boot.log — a bare three-line config skips that
# whole phase, so the model has nothing to match against.
tmpcfg="$(mktemp)"
cat >"$tmpcfg" <<'EOF'
enable_uart=1
uart_2ndstage=1

dtparam=spi=on
dtparam=audio=off

dtoverlay=disable-bt
dtoverlay=disable-wifi

camera_auto_detect=1
display_auto_detect=1
auto_initramfs=1

dtoverlay=vc4-kms-v3d
max_framebuffers=2
disable_fw_kms_setup=1

arm_64bit=1
disable_overscan=1
arm_boost=1
EOF
mcopy -i "$out@@${part_offset}" -o "$tmpcfg" ::config.txt
echo "  + config.txt"
rm -f "$tmpcfg"

copy "$fw/start4.elf"                 start4.elf
copy "$fw/fixup4.dat"                 fixup4.dat

# The ARM device tree (used by start4 late, at the ARM handoff). Prefer a
# repo-local copy; fall back to the rpi-mkosi cache (flat or firmware/ subdir).
for dtb in "$fw/bcm2711-rpi-4-b.dtb" "$cache/bcm2711-rpi-4-b.dtb" \
           "$cache/firmware/bcm2711-rpi-4-b.dtb"; do
  [[ -f "$dtb" ]] && { copy "$dtb" bcm2711-rpi-4-b.dtb; break; }
done

# gpioman pin config. Without it the firmware retries gpioman forever in the
# model (built-in dt-blob fallback isn't reproduced).
if [[ -f "$fw/dt-blob.dts" && ! -f "$fw/dt-blob.bin" ]]; then
  "$here/scripts/make-dt-blob.py" "$fw/dt-blob.dts" "$fw/dt-blob.bin"
fi
copy "$fw/dt-blob.bin"                dt-blob.bin

# Kernel + overlays, so the boot has something to hand off to. The reference log
# loads these right after the HDMI bring-up.
copy "$fw/kernel8.img"                kernel8.img

tmpcmd="$(mktemp)"
echo "console=serial0,115200 console=tty1 root=/dev/mmcblk0p2 rootfstype=ext4 fsck.repair=yes rootwait" >"$tmpcmd"
mcopy -i "$out@@${part_offset}" -o "$tmpcmd" ::cmdline.txt
echo "  + cmdline.txt"
rm -f "$tmpcmd"

if [[ -d "$fw/overlays" ]]; then
  mmd -i "$out@@${part_offset}" ::overlays 2>/dev/null || true
  for ovl in "$fw/overlays"/*; do
    [[ -f "$ovl" ]] || continue
    copy "$ovl" "overlays/$(basename "$ovl")"
  done
fi

# --- root filesystem: busybox init and a shell on the serial console -----------
# The kernel mounts it read-only (no `rw` on the command line), so a run never
# changes the image. Fixed UUID, hash seed and timestamps keep the image — and
# what Linux prints about it — the same on every build.
rootfs="$(mktemp -d)"
mkdir -p "$rootfs"/{bin,sbin,etc/init.d,proc,sys,dev,tmp,root}
if [[ -f "$fw/busybox-aarch64" ]]; then
  cp "$fw/busybox-aarch64" "$rootfs/bin/busybox"
  for applet in sh ash cat cut date df dmesg echo env false free grep head hostname \
                id kill ln ls mkdir mount mv ps pwd readlink rm sed sleep sort stty \
                sync tail test top touch tr true umount uname uptime vi wc; do
    ln -s busybox "$rootfs/bin/$applet"
  done
  ln -s ../bin/busybox "$rootfs/sbin/init"
  for applet in halt poweroff reboot; do
    ln -s ../bin/busybox "$rootfs/sbin/$applet"
  done
  echo "  + p2: busybox ($(stat -Lc %s "$fw/busybox-aarch64") bytes)"
else
  echo "  ! missing $fw/busybox-aarch64 (root filesystem left without init; run fetch-firmware.sh)" >&2
fi
# rpi-fw-crypto, to ask start4's crypto service for an HMAC from Linux
# userspace (#40 milestone 4), with exactly the shared libraries it loads.
userland="$fw/arm64-userland"
if [[ -f "$userland/usr/bin/rpi-fw-crypto" ]]; then
  mkdir -p "$rootfs"/{lib,usr/bin,usr/lib/aarch64-linux-gnu}
  cp "$userland/usr/bin/rpi-fw-crypto" "$rootfs/usr/bin/"
  for so in ld-linux-aarch64.so.1 libc.so.6 librpifwcrypto.so.0 libgnutls.so.30 \
            libp11-kit.so.0 libffi.so.8 libidn2.so.0 libunistring.so.5 libtasn1.so.6 \
            libnettle.so.8 libhogweed.so.6 libgmp.so.10; do
    cp -L "$userland/usr/lib/aarch64-linux-gnu/$so" "$rootfs/usr/lib/aarch64-linux-gnu/$so"
  done
  ln -s ../usr/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1 "$rootfs/lib/ld-linux-aarch64.so.1"
  echo "  + p2: rpi-fw-crypto and its libraries"
else
  echo "  ! missing $userland (no rpi-fw-crypto on the card; run fetch-firmware.sh)" >&2
fi
# The firmware's command line ends in `console=tty1`, which makes the
# framebuffer /dev/console; name the serial port instead.
cat >"$rootfs/etc/inittab" <<'EOF'
ttyAMA0::sysinit:/etc/init.d/rcS
ttyAMA0::respawn:-/bin/sh
::ctrlaltdel:/sbin/reboot
EOF
cat >"$rootfs/etc/init.d/rcS" <<'EOF'
#!/bin/sh
mount -t proc proc /proc
mount -t sysfs sysfs /sys
echo "rpi-virt-fw: userland up, $(uname -sr), $(grep -c ^processor /proc/cpuinfo) CPUs"
EOF
chmod -R u=rwX,go=rX "$rootfs"
chmod 755 "$rootfs/etc/init.d/rcS"
[[ -f "$rootfs/bin/busybox" ]] && chmod 755 "$rootfs/bin/busybox"
find "$rootfs" -exec touch -h -d @0 {} +
# No `orphan_file` (e2fsprogs 1.47's default): with it a read-only mount logs an
# orphan cleanup even on a clean filesystem.
E2FSPROGS_FAKE_TIME=1 mke2fs -q -t ext4 -O ^orphan_file -L rootfs \
  -U 7b3f1c8e-0d2a-4c5e-9f61-2a4b6c8d0e1f -E root_owner=0:0,hash_seed=7b3f1c8e-0d2a-4c5e-9f61-2a4b6c8d0e1f,offset=$(( root_start * sector )) \
  -d "$rootfs" "$out" "$(( root_mb * 1024 ))k"
# `mke2fs -d` copies the builder's uid/gid and gives every inode a random
# generation number; make everything root's, generation 0.
( cd "$rootfs" && { echo /; echo /lost+found; find . -mindepth 1 | sed 's|^\.||'; } | while read -r p; do
    echo "sif \"$p\" uid 0"
    echo "sif \"$p\" gid 0"
    echo "sif \"$p\" generation 0"
    echo "sif \"$p\" ctime 0"
  done ) | E2FSPROGS_FAKE_TIME=1 debugfs -w -f - "$out?offset=$(( root_start * sector ))" >/dev/null 2>&1
rm -rf "$rootfs"
echo "  + p2: ext4 root filesystem (${root_mb} MiB)"

echo "done. contents:"
mdir -i "$out@@${part_offset}" ::
