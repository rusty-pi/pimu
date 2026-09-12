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
# Debian's static aarch64 busybox: the userland on the SD card's root
# filesystem (scripts/make-sd.sh, #40 milestone 5). Checked against the hash.
BUSYBOX_DEB="${BUSYBOX_DEB:-busybox-static_1.35.0-4+deb12u1+b1_arm64.deb}"
BUSYBOX_SHA256="${BUSYBOX_SHA256:-732c9135564fc71337e0e05fb4da4d11e6c28c1834bce3e405e575afef2a52f5}"
# rpi-fw-crypto, Raspberry Pi's own command-line client of start4's crypto
# service (raspberrypi/utils rpifwcrypto, packaged in raspi-utils), and the
# shared libraries it loads, from Debian trixie — the Linux side of #40
# milestone 4. make-sd.sh puts them on the root filesystem. `<url> <sha256>`
# each; a Debian point release drops superseded versions from the pool, so a
# 404 here means bumping these to the current ones.
rpi="https://archive.raspberrypi.org/debian/pool/main"
deb="https://deb.debian.org/debian/pool/main"
USERLAND_DEBS=(
	"$rpi/r/raspi-utils/rpifwcrypto_20260626-1_arm64.deb 6a52e3eea71c1d46cca47531d068df73a6af7bc5e74744055572aaee8c03a339"
	"$rpi/r/raspi-utils/librpifwcrypto0_20260626-1_arm64.deb 519e8e289168890506e8c93ffbf0956caacabc17443eb67ab5aee1452cee30fd"
	"$deb/g/glibc/libc6_2.41-12+deb13u4_arm64.deb 8784eda966b189c777a384dac5ce009e8fc9b52d006926c5a013e7fa8aa688cc"
	"$deb/g/gnutls28/libgnutls30t64_3.8.9-3+deb13u4_arm64.deb 337ef41ab360015d051d6a07f85f8aa6706b35712e3dea23157eba9c902196e2"
	"$deb/libu/libunistring/libunistring5_1.3-2_arm64.deb 4847467a0e47039837895e88f5a99a4a86a3d8266be40c3c896fc7c10d552dc7"
	"$deb/libt/libtasn1-6/libtasn1-6_4.20.0-2+deb13u1_arm64.deb e421da949cd26245f24c594265d6a01900b5e060e80793f68b977f7bfaa009ef"
	"$deb/p/p11-kit/libp11-kit0_0.25.5-3_arm64.deb 78bfc746e68a5217e845f488f69100ed89d10c8886fbd3ffce937acb6a7d88fb"
	"$deb/libf/libffi/libffi8_3.4.8-2_arm64.deb d84a783b818f2386627604e64b793eb4ac5bb9ea1ba321a194a1c9c82fe09a01"
	"$deb/n/nettle/libnettle8t64_3.10.1-1_arm64.deb 16107ce5a7b522da021fa8aba42d42af9c5e90ddabfcda3710aff6ea95808ee0"
	"$deb/n/nettle/libhogweed6t64_3.10.1-1_arm64.deb cb92d5a51c4fd6c7b7cbb62aaa60c7af830d0bea5b410fb1f47ee685e26944d1"
	"$deb/libi/libidn2/libidn2-0_2.3.8-2_arm64.deb d2e5cef812f15db1eeb35f0a193158ee102a9e94284036c0e293521f2761d2d7"
	"$deb/g/gmp/libgmp10_6.3.0+dfsg-3_arm64.deb a27bbc27f119161ea9702c8dd66f54131cdf0d2ca73000f50ea91ef2fdfef0fb"
)
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

echo "debian busybox-static ($BUSYBOX_DEB)"
queue "https://deb.debian.org/debian/pool/main/b/busybox/$BUSYBOX_DEB" "busybox-static_arm64.deb"

echo "rpi-fw-crypto and its libraries (${#USERLAND_DEBS[@]} packages)"
mkdir -p "$dest/debs"
for entry in "${USERLAND_DEBS[@]}"; do
	url="${entry% *}"
	queue "$url" "debs/$(basename "$url")"
done

echo
curl -fSL --retry 3 --parallel --parallel-max 8 "${jobs[@]}"

"$here/scripts/make-dt-blob.py" "$dest/dt-blob.dts" "$dest/dt-blob.bin"

echo "$BUSYBOX_SHA256  $dest/busybox-static_arm64.deb" | sha256sum --quiet -c -
dpkg-deb --fsys-tarfile "$dest/busybox-static_arm64.deb" | tar -xO ./bin/busybox >"$dest/busybox-aarch64"
chmod +x "$dest/busybox-aarch64"

# Unpacked whole into one tree; make-sd.sh picks out the binary and the
# libraries it actually loads.
rm -rf "$dest/arm64-userland"
mkdir -p "$dest/arm64-userland"
for entry in "${USERLAND_DEBS[@]}"; do
	url="${entry% *}" sum="${entry##* }"
	file="$dest/debs/$(basename "$url")"
	echo "$sum  $file" | sha256sum --quiet -c -
	dpkg-deb -x "$file" "$dest/arm64-userland"
done

echo
echo "sha256:"
( cd "$dest" && sha256sum ./*.elf ./*.dat ./*.bin | tee firmware.sha256 )

echo
echo "done -> $dest"
