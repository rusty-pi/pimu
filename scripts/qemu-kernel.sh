#!/usr/bin/env bash
# Build the bare-metal aarch64 frontend and boot it under QEMU as a `-kernel`.
#
#   scripts/qemu-kernel.sh [extra qemu args...]
#
# This is stage 2 of #32: the image is scaffolding — it prints a banner and its
# exception level, and parks. What it proves is the environment the later stages
# need: an image of ours that QEMU's `raspi4b` accepts as an arm64 Image, loads
# where we linked it, and enters at EL2 (stage 4 installs stage-2 translation,
# which only exists at EL2).
#
# `raspi4b` and not `virt`: RAM at physical 0 is the BCM2711 map the VideoCore
# firmware produces addresses for. `virt` starts RAM at 0x40000000 with flash at
# the bottom, so every firmware-produced address would be wrong.
#
# Env: QEMU (default qemu-system-aarch64), RVF_KERNEL_WALL (seconds the guest is
# allowed to run, default 10 — the image parks and never exits, so the timeout
# is the normal way this ends), RVF_PROFILE (cargo profile, default release).
set -uo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
qemu="${QEMU:-qemu-system-aarch64}"
wall="${RVF_KERNEL_WALL:-10}"
profile="${RVF_PROFILE:-release}"
target=aarch64-unknown-none
elf="$here/target/$target/$profile/vc4-to-aarch64"
img="$here/target/$target/$profile/vc4-to-aarch64.img"

# --- build --------------------------------------------------------------------
# The bare-metal crate is a workspace member kept out of `default-members`, so it
# needs the explicit --target: a plain `cargo build` at the root never builds it.
if ! rustup target list --installed 2>/dev/null | grep -qx "$target"; then
  echo "installing rust target $target" >&2
  rustup target add "$target" || exit 1
fi
cargo build --manifest-path "$here/aarch64/Cargo.toml" --target "$target" \
  ${profile:+--profile "$profile"} || exit 1

# --- flatten ------------------------------------------------------------------
# QEMU would happily load the ELF, but then it takes the entry point from the
# ELF header and the arm64 Image header is never looked at — which is precisely
# the thing this stage has to get right. Boot the flat image.
objcopy=""
for cand in llvm-objcopy rust-objcopy aarch64-linux-gnu-objcopy; do
  if command -v "$cand" >/dev/null 2>&1; then objcopy="$cand"; break; fi
done
# rust-objcopy ships with the llvm-tools rustup component and is not on PATH.
if [ -z "$objcopy" ]; then
  cand="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/rust-objcopy"
  [ -x "$cand" ] && objcopy="$cand"
fi
if [ -z "$objcopy" ]; then
  echo "MISSING: no usable objcopy (rustup component add llvm-tools, or" >&2
  echo "         apt-get install binutils-aarch64-linux-gnu)" >&2
  exit 1
fi
"$objcopy" -O binary "$elf" "$img" || exit 1

# The header is 64 bytes with magic "ARM\x64" at offset 56; without it QEMU
# treats the file as a raw blob and the load address is anyone's guess.
magic=$(od -An -tx1 -j56 -N4 "$img" | tr -d ' \n')
if [ "$magic" != "41524d64" ]; then
  echo "BAD: arm64 Image magic at offset 56 is $magic, expected 41524d64" >&2
  exit 1
fi
echo "image: $img ($(stat -Lc %s "$img") bytes)"

# --- run ----------------------------------------------------------------------
if ! command -v "$qemu" >/dev/null 2>&1; then
  echo "MISSING: $qemu is not installed (Debian/Ubuntu: apt-get install qemu-system-arm)" >&2
  exit 1
fi
# Exit 2 for "this QEMU is too old", the same convention scripts/qemu-check.sh
# uses, so CI can report a toolchain gap as a warning rather than a regression.
if ! "$qemu" -machine help 2>/dev/null | grep -q '^raspi4b '; then
  echo "SKIP: $qemu has no raspi4b machine ($("$qemu" --version | head -1))." >&2
  exit 2
fi

log="$(mktemp)"
trap 'rm -f "$log"' EXIT
# stdin closed: the image never reads, and a blocked read would hang the run.
timeout --signal=TERM "$wall" \
  "$qemu" -M raspi4b -kernel "$img" -display none -serial stdio "$@" \
  </dev/null 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
# 124 = the timeout fired, which is the expected end: the image parks in `wfe`.
if [ "$status" -ne 0 ] && [ "$status" -ne 124 ]; then
  echo "qemu exit status: $status" >&2
  exit "$status"
fi

# The one assertion worth making here. EL2 is what the rest of #32 is built on,
# and "no output at all" is what a mis-sized or mis-placed image looks like.
if ! grep -q 'CurrentEL   EL2' "$log"; then
  echo "FAILED: the image did not report EL2 (see the output above)" >&2
  exit 1
fi
echo "qemu -kernel check passed"
