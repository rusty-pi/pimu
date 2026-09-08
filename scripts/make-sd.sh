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
    echo "  + $dst  ($(stat -c %s "$src") bytes)"
  else
    echo "  ! missing $src (skipped)" >&2
  fi
}

# Minimal config: just enough to route the debug console to the PL011 and keep
# the second-stage chatter on. No armstub / UEFI blob yet — the milestone is
# "bootloader loads start4.elf", not a full UEFI bring-up.
tmpcfg="$(mktemp)"
cat >"$tmpcfg" <<'EOF'
enable_uart=1
uart_2ndstage=1
arm_64bit=1
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

echo "done. contents:"
mdir -i "$out@@${part_offset}" ::
