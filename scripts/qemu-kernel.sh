#!/usr/bin/env bash
# Build the bare-metal aarch64 frontend and boot it under QEMU as a `-kernel`.
#
#   scripts/qemu-kernel.sh [--fault] [extra qemu args...]
#
# This is stage 2 of #32: the image is scaffolding — it prints a banner, checks
# its own exception level and exits. What it proves is the environment the later
# stages need: an image of ours that QEMU's `raspi4b` accepts as an arm64 Image,
# loads where we linked it, and enters at EL2 (stage 4 installs stage-2
# translation, which only exists at EL2).
#
# `raspi4b` and not `virt`: RAM at physical 0 is the BCM2711 map the VideoCore
# firmware produces addresses for. `virt` starts RAM at 0x40000000 with flash at
# the bottom, so every firmware-produced address would be wrong.
#
# How a pass is reported: the image calls ARM semihosting `SYS_EXIT_EXTENDED`,
# so QEMU's own process exit status *is* the guest's verdict. That is why the
# assertion below is on a status and not on `grep 'CurrentEL   EL2'` — a grep
# cannot tell "reported EL1" from "printed nothing", it scales no further than
# a banner, and it reads a hang and a crash as the same thing. `cargo test`,
# `boot-check.sh` and the golden transcript do not run inside a bare-metal
# image; this is the substitute, and #32 calls it the epic's open risk.
#
# Exit status of this script:
#   0  the image ran and reported success
#   1  the image reported a failure, hung, or QEMU itself failed
#   2  this QEMU has no `raspi4b` machine — a toolchain gap, which CI warns on
#
# Exit status of the *image* (aarch64/src/semihost.rs, kept in sync):
#   0  ok        1  self-check failed        3  exception taken    4  panicked
#
# `--fault` builds the image with the `fault` feature, which branches to an
# unmapped address on purpose right after the banner, and asserts that the EL2
# vector table caught it (image status 3). A handler nobody has ever run is a
# handler that does not work.
#
# Env: QEMU (default qemu-system-aarch64), RVF_KERNEL_WALL (seconds the guest is
# allowed to run before it is considered hung, default 20), RVF_PROFILE (cargo
# profile, default release).
set -uo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
qemu="${QEMU:-qemu-system-aarch64}"
wall="${RVF_KERNEL_WALL:-20}"
profile="${RVF_PROFILE:-release}"
target=aarch64-unknown-none
elf="$here/target/$target/$profile/vc4-to-aarch64"
img="$here/target/$target/$profile/vc4-to-aarch64.img"

# Image exit statuses, mirroring aarch64/src/semihost.rs.
IMAGE_OK=0
IMAGE_SELF_CHECK=1
IMAGE_FAULT=3
IMAGE_PANIC=4

features=()
want=$IMAGE_OK
if [ "${1:-}" = "--fault" ]; then
  shift
  features=(--features fault)
  want=$IMAGE_FAULT
fi

# --- build --------------------------------------------------------------------
# The bare-metal crate is a workspace member kept out of `default-members`, so it
# needs the explicit --target: a plain `cargo build` at the root never builds it.
if ! rustup target list --installed 2>/dev/null | grep -qx "$target"; then
  echo "installing rust target $target" >&2
  rustup target add "$target" || exit 1
fi
cargo build --manifest-path "$here/aarch64/Cargo.toml" --target "$target" \
  ${profile:+--profile "$profile"} ${features[@]+"${features[@]}"} || exit 1

# --- flatten ------------------------------------------------------------------
# QEMU would happily load the ELF, but then it takes the entry point from the
# ELF header and the arm64 Image header is never looked at — which is precisely
# the thing this stage has to get right. Boot the flat image.
# `rust-objcopy` links against the toolchain's own libLLVM, which is not on the
# loader path — on a GitHub runner it exists and then dies with
# "libLLVM.so.22.1-rust-1.98.1-stable: cannot open shared object file".
# Exporting the sysroot lib directory fixes that and is inert for the others.
sysroot="$(rustc --print sysroot)"
export LD_LIBRARY_PATH="$sysroot/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# Existing on PATH is not the same as working, so probe each candidate rather
# than taking the first one `command -v` admits to.
objcopy=""
for cand in \
  llvm-objcopy \
  rust-objcopy \
  aarch64-linux-gnu-objcopy \
  "$sysroot/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/rust-objcopy"
do
  if "$cand" --version >/dev/null 2>&1; then objcopy="$cand"; break; fi
done
if [ -z "$objcopy" ]; then
  echo "MISSING: no working objcopy (rustup component add llvm-tools, or" >&2
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

# Semihosting is how the image reports its verdict, so it is not optional: with
# it disabled the image's `HLT #0xF000` is an unallocated instruction and the
# run ends in the vector table instead of in an exit status. `target=native`
# means QEMU services the call itself rather than forwarding it to a gdb stub.
semihosting=(-semihosting-config enable=on,target=native)

# stdin closed: the image never reads, and a blocked read would hang the run.
timeout --signal=TERM "$wall" \
  "$qemu" -M raspi4b -kernel "$img" -display none -serial stdio \
  "${semihosting[@]}" "$@" </dev/null 2>&1
status=$?

# 124 is `timeout` firing. It used to be the expected end — the image parked in
# `wfe` forever — and is now a hang: the image exits of its own accord.
if [ "$status" -eq 124 ]; then
  echo "FAILED: the image did not exit within ${wall}s (hung; see the output above)" >&2
  exit 1
fi

if [ "$status" -eq "$want" ]; then
  case "$want" in
    "$IMAGE_FAULT") echo "qemu -kernel fault check passed (the vector table caught it)" ;;
    *) echo "qemu -kernel check passed" ;;
  esac
  exit 0
fi

# Anything else: name it, because the status is the whole diagnosis now.
case "$status" in
  "$IMAGE_OK") echo "FAILED: expected image status $want, but it reported success" >&2 ;;
  "$IMAGE_SELF_CHECK") echo "FAILED: the image's own self-check failed (see the output above)" >&2 ;;
  "$IMAGE_FAULT") echo "FAILED: the image took an exception (see the EXCEPTION dump above)" >&2 ;;
  "$IMAGE_PANIC") echo "FAILED: the image panicked (see the PANIC line above)" >&2 ;;
  *) echo "FAILED: qemu exit status $status (qemu itself, not the image)" >&2 ;;
esac
exit 1
