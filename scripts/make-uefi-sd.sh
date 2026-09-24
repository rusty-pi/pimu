#!/usr/bin/env bash
# Build an SD-card image that boots the RPi4 UEFI firmware (pftf/RPi4's
# RPI_EFI.fd, fetched by scripts/fetch-firmware.sh) as the armstub: MBR, one
# FAT32 partition with the Raspberry Pi firmware, the device tree and the
# overlays, and a config.txt that hands the ARM to UEFI instead of a kernel.
# Consumed by `pimu boot --sd <img>`, and by testdata/boot/uefi.toml.
#
# There is no root filesystem and no kernel: the run this card is for ends at
# the UEFI boot menu, which is already past the point where UEFI's drivers have
# asked the firmware for everything they need.
#
# No root / loop devices — sfdisk writes the partition table and mtools writes
# the filesystem at a byte offset.
set -euo pipefail
# mtools stamps directory entries with this instead of the build time, and
# writes it in the local timezone.
export SOURCE_DATE_EPOCH=315532800   # 1980-01-01, the FAT epoch
export TZ=UTC
# The order files are copied in is the order of their directory entries, and a
# glob sorts by the locale's collation: without this a builder east of C puts
# the overlays in a different order than CI does.
export LC_ALL=C

here="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$here/firmware/sd-uefi.img}"
fw="$here/firmware"

size_mb=256
part_start=2048          # sectors (1 MiB alignment)
sector=512

for f in RPI_EFI.fd start4.elf fixup4.dat; do
  [[ -f "$fw/$f" ]] || { echo "missing $fw/$f (run fetch-firmware.sh)" >&2; exit 1; }
done

echo "building $out ($size_mb MiB: FAT32 boot, UEFI armstub)"
rm -f "$out"
truncate -s "${size_mb}M" "$out"

sfdisk --quiet --label dos "$out" <<EOF
label-id: 0x5250494d
${part_start},,c,*
EOF

part_offset=$(( part_start * sector ))
part_bytes=$(( size_mb * 1024 * 1024 - part_offset ))
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

# pftf/RPi4's own config.txt, with two lines that matter here:
#
#   * `dtoverlay=disable-bt`, without which the PL011 stays wired to the
#     Bluetooth side and UEFI prints nothing at all on the serial console;
#   * `armstub=RPI_EFI.fd`, which is what makes the firmware hand the ARM to
#     UEFI. The firmware still looks for a kernel and says it found none.
tmpcfg="$(mktemp)"
cat >"$tmpcfg" <<'EOF'
arm_64bit=1
arm_boost=0
enable_gic=1
armstub=RPI_EFI.fd
disable_commandline_tags=2
disable_overscan=1
device_tree_address=0x3e0000
device_tree_end=0x400000
dtparam=audio=off
dtoverlay=disable-bt
dtoverlay=disable-wifi
dtoverlay=upstream-pi4
disable_splash=1
boot_delay=0
enable_uart=1
uart_2ndstage=1
EOF
mcopy -i "$out@@${part_offset}" -o "$tmpcfg" ::config.txt
echo "  + config.txt"
rm -f "$tmpcfg"

copy "$fw/RPI_EFI.fd"                 RPI_EFI.fd
copy "$fw/start4.elf"                 start4.elf
copy "$fw/fixup4.dat"                 fixup4.dat
copy "$fw/bcm2711-rpi-4-b.dtb"        bcm2711-rpi-4-b.dtb

# gpioman pin config. Without it the firmware retries gpioman forever in the
# model (built-in dt-blob fallback isn't reproduced).
if [[ -f "$fw/dt-blob.dts" && ! -f "$fw/dt-blob.bin" ]]; then
  "$here/scripts/make-dt-blob.py" "$fw/dt-blob.dts" "$fw/dt-blob.bin"
fi
copy "$fw/dt-blob.bin"                dt-blob.bin

if [[ -d "$fw/overlays" ]]; then
  mmd -i "$out@@${part_offset}" ::overlays 2>/dev/null || true
  for ovl in "$fw/overlays"/*; do
    [[ -f "$ovl" ]] || continue
    copy "$ovl" "overlays/$(basename "$ovl")"
  done
fi

echo "done -> $out"
