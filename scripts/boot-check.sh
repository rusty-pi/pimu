#!/usr/bin/env bash
# Run the simulated boot and check it against the boot scenario. This is what
# CI runs; run it locally to reproduce a CI failure.
#
#   scripts/boot-check.sh [--update] [--scenario=<toml>] [boot.log]
#
# Assumes `cargo build --release` and an SD image built by scripts/make-sd.sh.
#
# What is checked, and where it is written down, both live in the scenario —
# `testdata/boot/firmware-boot.toml` unless `--scenario` names another (the
# Linux boot to a shell is `testdata/boot/linux-boot.toml`):
#
#   * the golden console transcript (`testdata/boot/golden/`), diffed line by
#     line so a change shows up in place — including output that moved or a
#     value that shifted, which a grep cannot see;
#   * the milestones, each naming the invariant it guards and the commit or
#     issue that made it pass.
#
# This script only *runs* the boot: the workload (which EEPROM image, which SD
# card, what wall budget) comes out of the scenario via `boot-check --plan`, so
# it is described in exactly one place. Set RVF_BOOT_WALL to override the
# budget. `--update` re-records the golden from this run; read the diff in the
# commit before you believe it.
set -uo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
bin="$here/target/release/rpi-virt-fw"
scenario="$here/testdata/boot/firmware-boot.toml"

update=()
log="$here/boot.log"
for arg in "$@"; do
  case "$arg" in
    --update) update=(--update) ;;
    --scenario=*) scenario="${arg#--scenario=}" ;;
    -*) echo "usage: $0 [--update] [--scenario=<toml>] [boot.log]" >&2; exit 2 ;;
    *) log="$arg" ;;
  esac
done
# The UART bytes on their own, with none of the run report interleaved: this is
# what the golden transcript is made of. `$log` keeps the combined stream, which
# is what the milestones and the CI artifact want.
console="$log.console"

# One boot, two sets of assertions. The wall clock has little headroom — two
# concurrent boots miss `arm_loader` — so the run happens exactly once here and
# both checks read what it left behind.
mapfile -t plan < <("$bin" boot-check "$scenario" --plan --console "$console")
if [ "${#plan[@]}" -lt 2 ]; then
  echo "boot-check --plan produced nothing; is $bin built?" >&2
  exit 1
fi
wall="${plan[0]#wall=}"
args=("${plan[@]:1}")

# Stream the UART console (incl. `MESS:` lines) as it is produced.
export RVF_LIVE_CONSOLE=1

# No instruction cap: the wall clock is what ends the run.
timeout --signal=INT "$(( wall + 40 ))" "$bin" "${args[@]}" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
echo "recon exit status: $status"
# 124 = hit the wall clock; that is expected, not a failure.
if [ "$status" -ne 0 ] && [ "$status" -ne 124 ]; then
  exit "$status"
fi

"$bin" boot-check "$scenario" --log "$log" --console "$console" "${update[@]+"${update[@]}"}"
