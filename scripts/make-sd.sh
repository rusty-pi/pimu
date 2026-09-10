#!/usr/bin/env bash
# Build a bootable SD-card image for the model: MBR + one FAT32 boot partition
# holding the real Raspberry Pi 4 firmware (start4.elf + fixup4.dat), a config
# and the device tree. Consumed by `rpi-virt-fw recon --sd <img>`.
#
# No root / loop devices — sfdisk writes the partition table, mtools writes the
# filesystem at a byte offset.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$here/firmware/sd.img}"
fw="$here/firmware"
cache="${RPI_MKOSI_CACHE:-$HOME/src/rpi-mkosi/cache}"

size_mb=256
part_start=2048          # sectors (1 MiB alignment)
sector=512

echo "building $out (${size_mb} MiB, FAT32 boot partition)"
rm -f "$out"
truncate -s "${size_mb}M" "$out"

# --- partition table: one 0x0c (FAT32 LBA) partition filling the image ---------
sfdisk --quiet --label dos "$out" <<EOF
${part_start},,c,*
EOF

part_offset=$(( part_start * sector ))
part_bytes=$(( size_mb * 1024 * 1024 - part_offset ))

# --- filesystem --------------------------------------------------------------
mformat -i "$out@@${part_offset}" -F -v RPIBOOT -T $(( part_bytes / sector )) ::

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

echo "done. contents:"
mdir -i "$out@@${part_offset}" ::
