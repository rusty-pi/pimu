#!/usr/bin/env bash
# Build target/release/pimu with profile-guided optimisation.
#
#   scripts/pgo-build.sh [--profile-only]
#
# `--profile-only` stops once the profile is merged, leaving it at
# target/pgo/merged.profdata for a caller that does its own final build — the
# release workflow builds two targets from the one profile.
#
# An instrumented build runs the firmware boot and the first part of the Linux
# boot, `llvm-profdata` merges what it counted, and the release build is done
# again with that profile. Needs the llvm-tools rustup component (added here
# if it is missing) and what `boot-check` needs: the firmware blobs
# and both SD images scripts/make-sd.sh builds (firmware/sd.img, and
# firmware/sd-halt.img with KERNEL=halt).
#
# The profile only steers code layout and inlining. The guest runs exactly the
# same instructions either way, which boot-check's retired counts show. A plain
# `cargo build --release` afterwards rebuilds without it.
set -euo pipefail

profile_only=
case "${1:-}" in
  --profile-only) profile_only=1 ;;
  "") ;;
  *) echo "usage: $0 [--profile-only]" >&2; exit 2 ;;
esac

here="$(cd "$(dirname "$0")/.." && pwd)"
cd "$here"
work="$here/target/pgo"
# The Linux training run stops at this console line, 44 ms into the kernel:
# all four cores up, interrupts and the timer running. Stopping on the guest's
# progress rather than on wall time gives every machine the same profile (a
# 120 s budget never got a slower instrumented build past the firmware).
# Training all the way to the shell took 40% longer on a Pi 4 and gained
# nothing. PIMU_PGO_LINUX_UNTIL overrides it.
linux_until="${PIMU_PGO_LINUX_UNTIL:-Switched to clocksource arch_sys_counter}"

rustup component add llvm-tools >/dev/null 2>&1 || true
profdata="$(find "$(rustc --print sysroot)" -name llvm-profdata -type f | head -n1)"
if [ -z "$profdata" ]; then
  echo "llvm-profdata not found; try: rustup component add llvm-tools" >&2
  exit 1
fi

rm -rf "$work"
mkdir -p "$work/raw"
RUSTFLAGS="-Cprofile-generate=$work/raw" cargo build --release --target-dir "$work/build"
instr="$work/build/release/pimu"

# A boot scenario's workload as boot-check runs it, minus what it types into
# the console, plus any extra arguments (a later `--until` wins). The typed
# input has to go: `--until` only searches what the console prints after the
# last scripted line went in.
train() {
  local scenario="$1"
  shift
  local plan args=() i
  mapfile -t plan < <("$instr" boot-check "$scenario" --plan --output "$work/boot")
  # `--plan` names any boot medium that is not built yet, on stderr.
  if [ "${#plan[@]}" -lt 2 ]; then
    echo "no plan for $scenario; build what it says is missing" >&2
    exit 1
  fi
  for ((i = 1; i < ${#plan[@]}; i++)); do
    if [ "${plan[i]}" = --send-after ]; then
      i=$((i + 2))
      continue
    fi
    args+=("${plan[i]}")
  done
  # A boot that dies in its first instruction leaves a profile that says
  # nothing and a release that is slower than it looks, so the run has to
  # say how it ended and stop the build when it ended badly. Both runs are
  # meant to succeed: an EEPROM boot is `ok` once the firmware starts the ARM,
  # and an `--until` run once the console prints the line.
  local log="$work/$(basename "$scenario" .yaml).log" start=$SECONDS status=0
  echo "training on $scenario" >&2
  # `--speed max`: the profile must count the run loop's own work, not host sleep,
  # and a paced training run would take the guest's real time to collect it. The
  # plan says so too; this does not depend on that.
  PIMU_LIVE_CONSOLE=0 "$instr" "${args[@]}" --speed max "$@" > "$log" 2>&1 || status=$?
  local secs=$((SECONDS - start)) result
  result="$(grep '^result:' "$log" | tail -n1)"
  echo "  ${secs}s — ${result:-no result line}" >&2
  if [ "$status" -ne 0 ]; then
    echo "training on $scenario failed (exit $status), last 20 lines of $log:" >&2
    tail -n 20 "$log" >&2
    exit 1
  fi
}
train testdata/boot/firmware.yaml
train testdata/boot/linux.yaml --until "$linux_until"

"$profdata" merge -o "$work/merged.profdata" "$work/raw"
echo "merged $(find "$work/raw" -name '*.profraw' | wc -l) profraw files into" \
  "$(du -h "$work/merged.profdata" | cut -f1) of profile" >&2
if [ -n "$profile_only" ]; then
  echo "$work/merged.profdata"
  exit 0
fi
RUSTFLAGS="-Cprofile-use=$work/merged.profdata" cargo build --release
