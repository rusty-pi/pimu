#!/usr/bin/env bash
# Build target/release/rpi-virt-fw with profile-guided optimisation (#48).
#
#   scripts/pgo-build.sh
#
# An instrumented build runs the firmware boot and the first part of the Linux
# boot, `llvm-profdata` merges what it counted, and the release build is done
# again with that profile. Needs the llvm-tools rustup component (added here
# if it is missing) and what scripts/boot-check.sh needs: the firmware blobs
# and both SD images scripts/make-sd.sh builds (firmware/sd.img, and
# firmware/sd-halt.img with KERNEL=halt).
#
# The profile only steers code layout and inlining. The guest runs exactly the
# same instructions either way, which boot-check's retired counts show. A plain
# `cargo build --release` afterwards rebuilds without it.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
cd "$here"
work="$here/target/pgo"
# The Linux training run stops at this console line, 44 ms into the kernel:
# all four cores up, interrupts and the timer running. Stopping on the guest's
# progress rather than on wall time gives every machine the same profile (a
# 120 s budget never got a slower instrumented build past the firmware).
# Training all the way to the shell took 40% longer on a Pi 4 and gained
# nothing. RVF_PGO_LINUX_UNTIL overrides it.
linux_until="${RVF_PGO_LINUX_UNTIL:-Switched to clocksource arch_sys_counter}"

rustup component add llvm-tools >/dev/null 2>&1 || true
profdata="$(find "$(rustc --print sysroot)" -name llvm-profdata -type f | head -n1)"
if [ -z "$profdata" ]; then
  echo "llvm-profdata not found; try: rustup component add llvm-tools" >&2
  exit 1
fi

rm -rf "$work"
mkdir -p "$work/raw"
RUSTFLAGS="-Cprofile-generate=$work/raw" cargo build --release --target-dir "$work/build"
instr="$work/build/release/rpi-virt-fw"

# A boot scenario's workload as boot-check.sh runs it, minus what it types into
# the console, plus any extra arguments (a later `--until` wins). The typed
# input has to go: `--until` only searches what the console prints after the
# last scripted line went in.
train() {
  local scenario="$1"
  shift
  local plan args=() i
  mapfile -t plan < <("$instr" boot-check "$scenario" --plan --console "$work/console")
  for ((i = 1; i < ${#plan[@]}; i++)); do
    if [ "${plan[i]}" = --send-after ]; then
      i=$((i + 2))
      continue
    fi
    args+=("${plan[i]}")
  done
  echo "training on $scenario" >&2
  RVF_LIVE_CONSOLE=0 "$instr" "${args[@]}" "$@" > "$work/$(basename "$scenario" .toml).log" 2>&1 || true
}
train testdata/boot/firmware-boot.toml
train testdata/boot/linux-boot.toml --until "$linux_until"

"$profdata" merge -o "$work/merged.profdata" "$work/raw"
RUSTFLAGS="-Cprofile-use=$work/merged.profdata" cargo build --release
