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
# pftf/RPi4: the RPi4 UEFI firmware, as a release zip holding RPI_EFI.fd.
# scripts/make-uefi-sd.sh puts it on a card as the armstub, which is what the
# uefi scenario boots: a second, independent client of the firmware's
# property interface, and the one that reads the board's MAC out of it.
UEFI_REF="${UEFI_REF:-v1.53}"
UEFI_SHA256="${UEFI_SHA256:-ca9973e2a7a546b3df871cfb7382e656829114b6dfa424f40dc67cc90a217d88}"
# RPi-Distro/firmware-nonfree: the CYW43455's *own* firmware, its regulatory
# (CLM) blob and the Pi 4B nvram. The chip runs that firmware out of its own
# SRAM; `brcmfmac` downloads it over SDIO at probe time, so the card has to
# carry it for the WiFi chip to get past enumeration.
#
# The paths have moved between layouts. On the `trixie` branch the blobs live
# under `debian/added-firmware/`, and the names `brcmfmac` asks for are
# symlinks there:
#
#   brcm/brcmfmac43455-sdio.raspberrypi,4-model-b.bin -> ../cypress/cyfmac43455-sdio.bin
#   brcm/brcmfmac43455-sdio.raspberrypi,4-model-b.txt -> brcmfmac43455-sdio.txt
#
# `cyfmac43455-sdio.bin` is not a file in the tree at all: the package's
# update-alternatives picks between `-standard` (priority 50) and `-minimal`
# (10), so a Raspberry Pi OS install runs the standard build. A raw fetch gets
# a symlink's *text*, not its target, so the targets are named here and
# installed under the names the driver asks for.
NONFREE_REF="${NONFREE_REF:-3bab0f823f5b53150b76aab77093adef6655b920}"
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
	# `iw`, to ask the WiFi driver what it found, and the two libnl libraries
	# it links. Everything else it needs — libc and the loader — is already
	# here for rpi-fw-crypto. `BRCMFMAC=1 make-sd.sh` puts it on the card.
	"$deb/i/iw/iw_6.9-1+b1_arm64.deb dcacc4fb0a002b313effb1f5bb81adf14aca83eed9043bc97acf9e8b437c3d93"
	"$deb/libn/libnl3/libnl-3-200_3.7.0-2_arm64.deb 0489548a052d64b1acf7f61b0f08fd2da954b6fd6b9c729a4e740d7d39652d00"
	"$deb/libn/libnl3/libnl-genl-3-200_3.7.0-2_arm64.deb 5455099e4ae9a013a44bce245678b50c84b93f9983cd3005f49e6a94d14e30ee"
)
# Everything the WiFi bring-up needs, as `<path under firmware/wifi/> <sha256>`
# with `KVER` standing in for the kernel version read out of kernel8.img. The
# modules come from FIRMWARE_REF and the chip firmware from NONFREE_REF, so
# overriding either ref means re-taking these — the check prints the hash it
# got for exactly that.
WIFI_SHA256=(
	"lib/modules/KVER/kernel/net/rfkill/rfkill.ko.xz c8d38d608ac4bdd620e416188e4bf0aead164272ba4fe76f2ebb122069f38f76"
	"lib/modules/KVER/kernel/net/wireless/cfg80211.ko.xz b659e3080a706631312b06cf6b0cf583f8c034c92374ffdd28264b02272e5b7c"
	"lib/modules/KVER/kernel/drivers/net/wireless/broadcom/brcm80211/brcmutil/brcmutil.ko.xz 8609f1fe7a5e8c1765d67739eaacf8ae7365a8418522dcf81881a10425fd0fcc"
	"lib/modules/KVER/kernel/drivers/net/wireless/broadcom/brcm80211/brcmfmac/brcmfmac.ko.xz 102d77619c7f1bd5392afe9e49568211612534b7baac552ebf8423e71b32ad17"
	"lib/modules/KVER/kernel/drivers/net/wireless/broadcom/brcm80211/brcmfmac/cyw/brcmfmac-cyw.ko.xz ed6e36bd7b962c0f78abb8bcdee5d3f41010b64f846a48d84399d15d6e321c0a"
	"lib/firmware/brcm/brcmfmac43455-sdio.bin d608f866582519c0a28d86db43040f4f1b98dd1d153e72e9752586546b4a36c3"
	"lib/firmware/brcm/brcmfmac43455-sdio.clm_blob 9823842cae9fb9a5dd1e5fb31f595516ec7deee341354bef30bb3026eee29cc1"
	"lib/firmware/brcm/brcmfmac43455-sdio.txt ca709be81a78bdb6932936374f39943acbd7af07fae6151011127599a3ce9e3d"
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
# Cut-down build (`gpu_mem=16`): no camera or codecs, and silent on the UART
# once started. `START4=start4cd make-sd.sh` makes the card for firmware-cd.toml.
queue "$raw/raspberrypi/firmware/$FIRMWARE_REF/boot/start4cd.elf" "start4cd.elf"
queue "$raw/raspberrypi/firmware/$FIRMWARE_REF/boot/fixup4cd.dat" "fixup4cd.dat"

# Kernel, device tree and the overlays the reference boot log loads. Without
# these the boot has nothing to hand off to and stops after the HDMI bring-up;
# with them it can reproduce a start4 log from a Raspberry Pi 4B d03115 from
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

echo "pftf/RPi4 @ $UEFI_REF"
queue "https://github.com/pftf/RPi4/releases/download/$UEFI_REF/RPi4_UEFI_Firmware_$UEFI_REF.zip" "RPi4_UEFI_Firmware.zip"

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

# --- the WiFi driver, and the WiFi chip's own firmware -----------------------
# `brcmfmac` ships as a module, not built into the stock kernel, so a card that
# is to bring the CYW43455 up has to carry it — together with cfg80211,
# brcmutil and rfkill, which it needs. Which kernel's modules those are is not
# a guess: the version comes out of the kernel8.img just fetched (the `-v8+`
# build), so it always matches whatever FIRMWARE_REF is pinned above.
kver="${MODULES_KVER:-$(gzip -dc "$dest/kernel8.img" 2>/dev/null |
	LC_ALL=C grep -ao 'Linux version [0-9][^ ]*' | sed -n '1s/Linux version //p' || true)}"
[[ -n "$kver" ]] || {
	echo "no kernel version string in $dest/kernel8.img (set MODULES_KVER)" >&2
	exit 1
}

wifi="$dest/wifi"
rm -rf "$wifi"
mods="lib/modules/$kver/kernel"
brcm80211="$mods/drivers/net/wireless/broadcom/brcm80211"
jobs=()

echo
echo "raspberrypi/firmware @ $FIRMWARE_REF: $kver modules for the WiFi chip"
fwmod="$raw/raspberrypi/firmware/$FIRMWARE_REF/modules/$kver/kernel"
queue "$fwmod/net/rfkill/rfkill.ko.xz" "wifi/$mods/net/rfkill/rfkill.ko.xz"
queue "$fwmod/net/wireless/cfg80211.ko.xz" "wifi/$mods/net/wireless/cfg80211.ko.xz"
queue "$fwmod/drivers/net/wireless/broadcom/brcm80211/brcmutil/brcmutil.ko.xz" \
	"wifi/$brcm80211/brcmutil/brcmutil.ko.xz"
queue "$fwmod/drivers/net/wireless/broadcom/brcm80211/brcmfmac/brcmfmac.ko.xz" \
	"wifi/$brcm80211/brcmfmac/brcmfmac.ko.xz"
# The per-vendor half of the split driver. `brcmfmac` asks for it by
# `request_module("brcmfmac-cyw")` once it knows what chip it is talking to,
# so it is on the card before it is reachable.
queue "$fwmod/drivers/net/wireless/broadcom/brcm80211/brcmfmac/cyw/brcmfmac-cyw.ko.xz" \
	"wifi/$brcm80211/brcmfmac/cyw/brcmfmac-cyw.ko.xz"

echo "RPi-Distro/firmware-nonfree @ $NONFREE_REF"
nonfree="$raw/RPi-Distro/firmware-nonfree/$NONFREE_REF/debian/added-firmware"
queue "$nonfree/cypress/cyfmac43455-sdio-standard.bin" "wifi/lib/firmware/brcm/brcmfmac43455-sdio.bin"
queue "$nonfree/cypress/cyfmac43455-sdio.clm_blob" "wifi/lib/firmware/brcm/brcmfmac43455-sdio.clm_blob"
queue "$nonfree/brcm/brcmfmac43455-sdio.txt" "wifi/lib/firmware/brcm/brcmfmac43455-sdio.txt"

echo
curl -fSL --retry 3 --parallel --parallel-max 8 --create-dirs "${jobs[@]}"

# `brcmfmac` asks for `<name>.<board compatible>.txt` before the plain name,
# and upstream makes the per-board one a symlink to it. Both here.
cp "$wifi/lib/firmware/brcm/brcmfmac43455-sdio.txt" \
	"$wifi/lib/firmware/brcm/brcmfmac43455-sdio.raspberrypi,4-model-b.txt"

for entry in "${WIFI_SHA256[@]}"; do
	path="${entry% *}" sum="${entry##* }"
	file="$wifi/${path//KVER/$kver}"
	echo "$sum  $file" | sha256sum --quiet -c - || {
		echo "  got $(sha256sum "$file" | cut -d' ' -f1)" >&2
		echo "  (a FIRMWARE_REF or NONFREE_REF override means re-taking WIFI_SHA256)" >&2
		exit 1
	}
done

echo "$UEFI_SHA256  $dest/RPi4_UEFI_Firmware.zip" | sha256sum --quiet -c -
# One file out of the zip, without needing unzip on the runner.
python3 -c 'import sys, zipfile
with zipfile.ZipFile(sys.argv[1]) as z, open(sys.argv[2], "wb") as out:
    out.write(z.read("RPI_EFI.fd"))' "$dest/RPi4_UEFI_Firmware.zip" "$dest/RPI_EFI.fd"

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
( cd "$dest" && sha256sum ./*.elf ./*.dat ./*.bin ./RPI_EFI.fd | tee firmware.sha256 )

echo
echo "done -> $dest"
