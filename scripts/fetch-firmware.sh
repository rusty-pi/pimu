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

fetch() {
	local url="$1" out="$2"
	echo "  $out  <-  $url"
	curl -fSL --retry 3 -o "$dest/$out" "$url"
}

echo "raspberrypi/firmware @ $FIRMWARE_REF"
fetch "$raw/raspberrypi/firmware/$FIRMWARE_REF/boot/start4.elf" "start4.elf"
fetch "$raw/raspberrypi/firmware/$FIRMWARE_REF/boot/fixup4.dat" "fixup4.dat"

echo "raspberrypi/rpi-eeprom @ $EEPROM_REF ($EEPROM_CHANNEL/$EEPROM_DATE)"
fetch "$raw/raspberrypi/rpi-eeprom/$EEPROM_REF/firmware-2711/$EEPROM_CHANNEL/pieeprom-$EEPROM_DATE.bin" "pieeprom.bin"
fetch "$raw/raspberrypi/rpi-eeprom/$EEPROM_REF/firmware-2711/$EEPROM_CHANNEL/recovery.bin" "recovery.bin"
fetch "$raw/raspberrypi/rpi-eeprom/$EEPROM_REF/firmware-2711/$EEPROM_CHANNEL/vl805-$EEPROM_VL805.bin" "vl805-$EEPROM_VL805.bin"

echo
echo "sha256:"
( cd "$dest" && sha256sum ./*.elf ./*.dat ./*.bin | tee firmware.sha256 )

echo
echo "done -> $dest"
