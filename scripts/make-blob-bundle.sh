#!/usr/bin/env bash
# Pack firmware blobs into the one file the bare-metal image can be handed.
#
#   scripts/make-blob-bundle.sh <out.bundle> <file> [<file>...]
#
# `raspi4b` under QEMU takes exactly one blob of ours: `-initrd`. `-pflash` is
# refused, a second `-drive if=sd` is refused, and there is no virtio transport
# on the machine. So the blobs the model needs — `pieeprom.bin` and `sd.img`,
# the same two `recon --eeprom --sd` opens — travel inside a container, and the
# image reads them in place out of RAM (aarch64/src/bundle.rs, which documents
# the format and is the only other place that knows it).
#
#   0  u8[4]  "RVFB"
#   4  u32    version = 1
#   8  u32    entry count
#  12  u32    reserved
#  16  entry[count]: u8[16] name (NUL-padded), u32 offset, u32 length
#      ... bodies, in entry order, in the order given on the command line
#
# All little-endian. Names are the basename, which is what the image looks up.
#
# This is a stopgap with a known end: the SD image belongs behind QEMU's own SD
# controller (`-drive if=sd`), reached a sector at a time through
# `block::BlockDevice`, which is also what makes the EEPROM's self-update
# persist. Until the image has an SDHCI driver, `-initrd` carries it.
set -euo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <out.bundle> <file> [<file>...]" >&2
  exit 2
fi

out="$1"; shift

# Little-endian u32 as four printf escapes. `printf '\x00'` emits a real NUL in
# bash's builtin, which is what makes a binary header possible in a shell at
# all; `command printf` on some systems does not, so do not "fix" this by
# hoisting it.
u32() {
  local v=$1
  printf "$(printf '\\x%02x\\x%02x\\x%02x\\x%02x' \
    $((v & 255)) $(((v >> 8) & 255)) $(((v >> 16) & 255)) $(((v >> 24) & 255)))"
}

header_len=16
entry_len=24
name_len=16
count=$#

# Offsets are absolute from the start of the bundle, so the table has to be
# sized before any of it is written.
off=$((header_len + count * entry_len))
names=()
sizes=()
offsets=()
for f in "$@"; do
  [ -r "$f" ] || { echo "$0: cannot read $f" >&2; exit 1; }
  n="$(basename "$f")"
  if [ "${#n}" -gt "$name_len" ]; then
    echo "$0: '$n' is longer than the $name_len-byte name field" >&2
    exit 1
  fi
  s="$(stat -Lc %s "$f")"
  names+=("$n")
  sizes+=("$s")
  offsets+=("$off")
  off=$((off + s))
done

{
  printf 'RVFB'
  u32 1
  u32 "$count"
  u32 0
  for i in "${!names[@]}"; do
    # `printf %-16s` pads with spaces, not NULs, so pad by hand.
    printf '%s' "${names[$i]}"
    pad=$((name_len - ${#names[$i]}))
    while [ "$pad" -gt 0 ]; do printf '\x00'; pad=$((pad - 1)); done
    u32 "${offsets[$i]}"
    u32 "${sizes[$i]}"
  done
  cat "$@"
} > "$out"

echo "bundle: $out ($(stat -Lc %s "$out") bytes, $count blob(s))"
for i in "${!names[@]}"; do
  printf '  %-16s %10s bytes @ %s\n' "${names[$i]}" "${sizes[$i]}" "${offsets[$i]}"
done
