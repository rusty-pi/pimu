#!/usr/bin/env bash
#
# Fetch the Raspberry Pi VideoCore boot blobs this bench targets into ./firmware/.
# Blobs are never committed (see .gitignore); this script is the reproducible way
# to get them.
#
# Versions are pinned below for reproducibility. Override on the command line:
#   FIRMWARE_REF=<git sha/tag> EEPROM_DATE=YYYY-MM-DD ./scripts/fetch-firmware.sh
#
set -euo pipefail

# --- pinned versions ---------------------------------------------------------
# raspberrypi/firmware: tag providing start4.elf + fixup4.dat
FIRMWARE_REF="${FIRMWARE_REF:-1.20260824}"
# raspberrypi/rpi-eeprom: channel + dated image for pieeprom.bin
EEPROM_REF="${EEPROM_REF:-master}"
EEPROM_CHANNEL="${EEPROM_CHANNEL:-latest}"
EEPROM_DATE="${EEPROM_DATE:-2026-08-04}"
EEPROM_VL805="${EEPROM_VL805:-000138c0}"
# ---------------------------------------------------------------------------

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$here/firmware"
mkdir -p "$dest"

raw="https://raw.githubusercontent.com"

# Every blob is queued here and fetched in one parallel curl run at the end.
jobs=()
queue() {
	local url="$1" out="$2"
	echo "  $out  <-  $url"
	jobs+=(-o "$dest/$out" "$url")
}

echo "raspberrypi/firmware @ $FIRMWARE_REF"
queue "$raw/raspberrypi/firmware/$FIRMWARE_REF/boot/start4.elf" "start4.elf"
queue "$raw/raspberrypi/firmware/$FIRMWARE_REF/boot/fixup4.dat" "fixup4.dat"
# Debug build: identical function, far more verbose UART logging (MESS: lines).
# Embed it in the boot medium with `START4=start4db make-sd.sh` to see where
# start4 gets stuck.
queue "$raw/raspberrypi/firmware/$FIRMWARE_REF/boot/start4db.elf" "start4db.elf"
queue "$raw/raspberrypi/firmware/$FIRMWARE_REF/boot/fixup4db.dat" "fixup4db.dat"

# Kernel, device tree and the overlays the reference boot log loads. Without
# these the boot has nothing to hand off to and stops after the HDMI bring-up;
# with them it can reproduce examples-on-real-hardware/vc4-boot.log from
# 'dtparam:' onwards. (initramfs8 is generated per-install, not shipped here —
# auto_initramfs simply finds nothing, which is fine.)
queue "$raw/raspberrypi/firmware/$FIRMWARE_REF/boot/kernel8.img" "kernel8.img"
queue "$raw/raspberrypi/firmware/$FIRMWARE_REF/boot/bcm2711-rpi-4-b.dtb" "bcm2711-rpi-4-b.dtb"
mkdir -p "$dest/overlays"
for ovl in overlay_map.dtb disable-bt.dtbo disable-wifi.dtbo vc4-kms-v3d.dtbo \
           vc4-kms-v3d-pi4.dtbo; do
	queue "$raw/raspberrypi/firmware/$FIRMWARE_REF/boot/overlays/$ovl" "overlays/$ovl"
done

# dt-blob source (gpioman pin config). Compiled to dt-blob.bin by
# scripts/make-dt-blob.py and placed on the SD by scripts/make-sd.sh.
queue "$raw/raspberrypi/firmware/$FIRMWARE_REF/extra/dt-blob.dts" "dt-blob.dts"

echo "raspberrypi/rpi-eeprom @ $EEPROM_REF ($EEPROM_CHANNEL/$EEPROM_DATE)"
queue "$raw/raspberrypi/rpi-eeprom/$EEPROM_REF/firmware-2711/$EEPROM_CHANNEL/pieeprom-$EEPROM_DATE.bin" "pieeprom.bin"
queue "$raw/raspberrypi/rpi-eeprom/$EEPROM_REF/firmware-2711/$EEPROM_CHANNEL/recovery.bin" "recovery.bin"
queue "$raw/raspberrypi/rpi-eeprom/$EEPROM_REF/firmware-2711/$EEPROM_CHANNEL/vl805-$EEPROM_VL805.bin" "vl805-$EEPROM_VL805.bin"

echo
curl -fSL --retry 3 --parallel --parallel-max 8 "${jobs[@]}"

"$here/scripts/make-dt-blob.py" "$dest/dt-blob.dts" "$dest/dt-blob.bin"

echo
echo "sha256:"
( cd "$dest" && sha256sum ./*.elf ./*.dat ./*.bin | tee firmware.sha256 )

echo
echo "done -> $dest"
