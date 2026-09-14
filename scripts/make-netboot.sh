#!/usr/bin/env bash
# Build the network-boot root the built-in peer serves (`boot --netboot <dir>`,
# src/net/peer.rs) from the SD image scripts/make-sd.sh built:
#
#   <out>/                       the SD boot partition's files, for TFTP boot
#   <out>/net_install/boot.img   the same files as a FAT image, for HTTP boot
#   <out>/net_install/boot.sig   its SHA-256 and RSA signature, in the
#                                rpi-eeprom-digest format
#   <out>-pubkey.bin             the signing key's public half as
#                                `rpi-eeprom-config --pubkey` stores it, for
#                                `boot --eeprom-pubkey`
#
# `net_install` is the bootloader's default HTTP_PATH. The peer answers every
# DNS query with its own address, so any HTTP_HOST reaches it.
#
# HTTP boot only takes a signed boot.img: with the hash alone the bootloader
# says "Invalid RSA signature size 0 bytes", and with a signature but the
# pinned EEPROM's all-zero pubkey.bin "rsa-verify fail". The key is
# testdata/netboot/test-signing-key.pem (test-only, see the README there);
# NETBOOT_KEY overrides it.
#
# No root / loop devices: mtools reads the SD image and writes boot.img.
set -euo pipefail
# mtools stamps directory entries with this instead of the build time, so
# boot.img, its hash and its signature are the same on every build.
export SOURCE_DATE_EPOCH=315532800   # 1980-01-01, the FAT epoch
# Sorted file lists, whatever the builder's locale.
export LC_ALL=C

here="$(cd "$(dirname "$0")/.." && pwd)"
sd="${1:-$here/firmware/sd.img}"
out="${2:-$here/firmware/netboot}"
key="${NETBOOT_KEY:-$here/testdata/netboot/test-signing-key.pem}"
part_offset=$(( 2048 * 512 ))   # scripts/make-sd.sh: one partition at 1 MiB

echo "building $out from $sd"
rm -rf "$out"
mkdir -p "$out/net_install"
mcopy -s -n -i "$sd@@${part_offset}" '::*' "$out/"

# A fixed volume serial (-N) so two builds of the same files give the same
# image, and the same hash in boot.sig.
img="$out/net_install/boot.img"
truncate -s 16M "$img"
mformat -i "$img" -N 52564642 -v RPIBOOT ::
# mformat puts its own version in the boot sector's OEM name (`MTOO4049`); a
# fixed one keeps the image independent of the mtools that built it.
printf 'RPIVIRT ' | dd of="$img" bs=1 seek=3 conv=notrunc status=none
# One file at a time in sorted order (`mcopy -s` would follow readdir order),
# so the clusters are allocated the same way on every machine.
( cd "$out" && find . -mindepth 1 -path ./net_install -prune -o -print | sort ) |
  while read -r p; do
    p="${p#./}"
    if [[ -d "$out/$p" ]]; then
      mmd -i "$img" "::$p"
    else
      mcopy -i "$img" "$out/$p" "::$p"
    fi
  done

# rpi-eeprom-digest: the SHA-256 on the first line, the timestamp, then the
# RSA-2048 PKCS#1 v1.5 signature over SHA-256 as hex (`openssl dgst -sign`).
sig="$(mktemp)"
trap 'rm -f "$sig"' EXIT
openssl dgst -sha256 -sign "$key" -out "$sig" "$img"
{
  sha256sum "$img" | cut -d' ' -f1
  echo "ts: 1785847093"
  echo "rsa2048: $(od -An -v -tx1 "$sig" | tr -d ' \n')"
} >"$out/net_install/boot.sig"

# The public half as rpi-eeprom-config's pemtobin writes it into the EEPROM:
# modulus then public exponent, little-endian, 256 + 8 bytes.
modulus="$(openssl rsa -in "$key" -noout -modulus | cut -d= -f2)"
exponent="$(openssl rsa -in "$key" -noout -text | sed -n 's/^publicExponent: \([0-9]*\).*/\1/p')"
python3 -c '
import sys
n, e = int(sys.argv[1], 16), int(sys.argv[2])
assert n.bit_length() == 2048, "RSA-2048 only"
open(sys.argv[3], "wb").write(n.to_bytes(256, "little") + e.to_bytes(8, "little"))
' "$modulus" "$exponent" "$out-pubkey.bin"

echo "done:"
ls -la "$out" "$out/net_install" "$out-pubkey.bin"
