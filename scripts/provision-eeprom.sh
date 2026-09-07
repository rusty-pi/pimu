#!/usr/bin/env bash
# Write the "self-update timestamp" trailer into a pieeprom image, the way the
# bootloader does after applying an EEPROM update. A provisioned image boots
# straight through — `SELF-UPDATE ... skip` — instead of self-updating and
# rebooting on the first run under `recon`.
#
# The trailer is the last 8 bytes of the image:
#     [len-8 .. len-4)  ~timestamp   (u32 LE)
#     [len-4 .. len)     timestamp   (u32 LE)
#
# Usage:
#   scripts/provision-eeprom.sh <pieeprom.upd> [<pieeprom.sig> | <unix-ts>] [<out>]
#
# With no timestamp arg it looks for `<pieeprom.upd dir>/pieeprom.sig` and reads
# its `ts:` line. Default output: <pieeprom>-provisioned.bin
set -euo pipefail

src="${1:?usage: provision-eeprom.sh <pieeprom.upd> [<sig>|<ts>] [<out>]}"
tsarg="${2:-}"
out="${3:-${src%.*}-provisioned.bin}"

if [[ -z "$tsarg" ]]; then
  tsarg="$(dirname "$src")/pieeprom.sig"
fi

if [[ "$tsarg" =~ ^[0-9]+$ ]]; then
  ts="$tsarg"
elif [[ -f "$tsarg" ]]; then
  ts="$(grep -oE 'ts:[[:space:]]*[0-9]+' "$tsarg" | grep -oE '[0-9]+')"
  [[ -n "$ts" ]] || { echo "no 'ts:' line in $tsarg" >&2; exit 1; }
else
  echo "second arg must be a unix timestamp or a pieeprom.sig file" >&2
  exit 1
fi

size=$(stat -c %s "$src")
cp "$src" "$out"

# ~ts and ts, little-endian, as the last 8 bytes.
notts=$(( (~ts) & 0xffffffff ))
printf "$(printf '\\x%02x' \
  $(( notts        & 0xff )) $(( (notts >> 8)  & 0xff )) $(( (notts >> 16) & 0xff )) $(( (notts >> 24) & 0xff )) \
  $((  ts          & 0xff )) $(( ( ts    >> 8) & 0xff )) $(( ( ts    >> 16) & 0xff )) $(( ( ts    >> 24) & 0xff )) )" \
  | dd of="$out" bs=1 seek=$((size - 8)) count=8 conv=notrunc status=none

echo "provisioned $out  (ts=$ts, trailer @ $(printf '%#x' $((size-8))))"
