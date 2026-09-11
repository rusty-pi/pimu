#!/usr/bin/env bash
# Build a bootable SD-card image for the model: MBR + a FAT32 boot partition
# holding the real Raspberry Pi 4 firmware (start4.elf + fixup4.dat), a config
# and the device tree, plus a raw partition holding the EEPROM image.
# Consumed by `rpi-virt-fw recon --sd <img>`.
#
# No root / loop devices — sfdisk writes the partition table, mtools writes the
# filesystem at a byte offset, dd writes the EEPROM partition.
#
# Why the EEPROM is on here at all: on hardware it is a separate SPI NOR chip,
# and it is writable — `pieeprom.upd` self-updates during boot. The bare-metal
# frontend (#32) has nowhere else to put it. QEMU's `raspi4b` refuses every
# other medium (`-pflash`, `if=sd,index=1`, every virtio transport), so the one
# drive it does take carries both. That costs no fidelity: the firmware reaches
# the EEPROM through `src/periph/spi0.rs` and never learns where those bytes are
# kept, exactly as it never learns where the SD image is kept. With
# `-drive if=sd,file=sd.img,format=raw` QEMU persists the writes back to the
# file, so a self-update survives across runs the way a real flash burn does.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$here/firmware/sd.img}"
fw="$here/firmware"
cache="${RPI_MKOSI_CACHE:-$HOME/src/rpi-mkosi/cache}"

boot_mb=256
part_start=2048          # sectors (1 MiB alignment)
sector=512

# The EEPROM partition is sized for the part the model claims to be: `RDID` in
# spi0.rs answers a Winbond W25Q128, 16 MiB. The `pieeprom.bin` image is only
# 512 KiB of that; the rest is erased flash, so the partition is filled with
# 0xFF and the tail reads back the way unused address space on a real chip
# does.
#
# It is *appended*, not carved out of the boot partition. The firmware prints
# the partition table and the FAT geometry it derives from it
# (`MBR: ... type: 0x0c`, `FAT32 clusters N`), so shrinking partition 1 to make
# room would move `clusters`, `fat-sectors` and the cluster count in the golden
# boot transcript — a change to the workload dressed up as a change to the
# layout. Growing the image instead leaves partition 1 byte-for-byte what it
# was, and the only line that moves is the previously-empty MBR slot 2 now
# describing a partition.
eeprom_mb=16
eeprom_sectors=$(( eeprom_mb * 1024 * 1024 / sector ))
boot_sectors=$(( boot_mb * 1024 * 1024 / sector - part_start ))
eeprom_start=$(( part_start + boot_sectors ))
size_mb=$(( boot_mb + eeprom_mb ))

echo "building $out (${size_mb} MiB: ${boot_mb} MiB FAT32 boot + ${eeprom_mb} MiB EEPROM)"
rm -f "$out"
truncate -s "${size_mb}M" "$out"

# --- partition table ----------------------------------------------------------
# 1: 0x0c FAT32 LBA, bootable — what the firmware boots from.
# 2: 0xda "non-FS data" — a raw blob, no filesystem. Nothing should try to
#    mount or probe it; the model reaches it by LBA through `block::Window`.
sfdisk --quiet --label dos "$out" <<EOF
${part_start},${boot_sectors},c,*
${eeprom_start},${eeprom_sectors},da
EOF

part_offset=$(( part_start * sector ))
part_bytes=$(( boot_sectors * sector ))

# --- EEPROM partition ---------------------------------------------------------
# Erased flash first (all 0xFF), then the image over the front of it.
eeprom_img=""
for cand in "$fw/pieeprom.bin" "$fw/pieeprom.upd"; do
  [[ -f "$cand" ]] && { eeprom_img="$cand"; break; }
done
# The zero source is bounded by `head` rather than by dd's `count`: an
# unbounded /dev/zero leaves `tr` writing into a closed pipe, and `pipefail`
# turns that SIGPIPE into a failed script.
head -c "$(( eeprom_sectors * sector ))" /dev/zero | tr '\000' '\377' \
  | dd of="$out" bs="$sector" seek="$eeprom_start" conv=notrunc status=none
if [[ -n "$eeprom_img" ]]; then
  dd if="$eeprom_img" of="$out" bs="$sector" seek="$eeprom_start" \
     conv=notrunc status=none
  echo "  + eeprom p2 @ LBA $eeprom_start  ($(stat -Lc %s "$eeprom_img") bytes of ${eeprom_mb} MiB)"
else
  echo "  ! no pieeprom.bin/.upd in $fw — EEPROM partition left erased" >&2
fi

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
