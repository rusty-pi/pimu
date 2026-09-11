#!/usr/bin/env bash
# Boot a Pi 4 image under the VideoCore-carrying QEMU: the real firmware runs
# the VPU side, QEMU runs the ARM side, both on the same RAM, SD card and
# serial port.
#
#   scripts/qemu-boot.sh [sd-image] [extra qemu args...]
#
# Defaults: firmware/pieeprom.bin as the EEPROM and firmware/sd.img as the
# card (scripts/make-sd.sh); the QEMU is qemu/build/qemu-system-aarch64 from
# scripts/build-qemu.sh, overridable with $QEMU. The card is opened
# read-write by QEMU's SD controller, so pass a copy of anything you care
# about.
#
# The firmware log and the kernel console both arrive on stdio: they are the
# same PL011 on the board, and here too.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
qemu="${QEMU:-$here/qemu/build/qemu-system-aarch64}"
eeprom="${RVF_EEPROM:-$here/firmware/pieeprom.bin}"
sd="${1:-$here/firmware/sd.img}"
shift $(( $# > 0 ? 1 : 0 ))

for f in "$qemu" "$eeprom" "$sd"; do
  [ -e "$f" ] || { echo "MISSING: $f" >&2; exit 1; }
done

exec "$qemu" \
  -machine "raspi4b,videocore=$eeprom,videocore-sd=$sd" \
  -drive "if=sd,file=$sd,format=raw" \
  -display none -serial stdio \
  "$@"
