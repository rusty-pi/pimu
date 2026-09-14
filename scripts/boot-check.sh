#!/usr/bin/env bash
# Run the simulated boot and check it against the boot scenario. This is what
# CI runs; run it locally to reproduce a CI failure.
#
#   scripts/boot-check.sh [--update] [--scenario=<toml>] [boot.log]
#
# Assumes `cargo build --release` and the boot media the scenario names: an SD
# image built by scripts/make-sd.sh, and for the network boots the root
# scripts/make-netboot.sh builds from it.
#
# What is checked, and where it is written down, both live in the scenario —
# `testdata/boot/firmware-boot.toml` (the SD boot) unless `--scenario` names
# another: the same files booted from USB, TFTP and HTTP (`usb-boot.toml`,
# `tftp-boot.toml`, `http-boot.toml`), or the Linux boot to a shell
# (`linux-boot.toml`), all in `testdata/boot/`:
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
while [ $# -gt 0 ]; do
  case "$1" in
    --update) update=(--update) ;;
    --scenario=*) scenario="${1#--scenario=}" ;;
    --scenario) scenario="${2:?--scenario needs a file}"; shift ;;
    -*) echo "usage: $0 [--update] [--scenario=<toml>] [boot.log]" >&2; exit 2 ;;
    *) log="$1" ;;
  esac
  shift
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
echo "boot exit status: $status"
# 124 = hit the wall clock; that is expected, not a failure.
if [ "$status" -ne 0 ] && [ "$status" -ne 124 ]; then
  exit "$status"
fi

"$bin" boot-check "$scenario" --log "$log" --console "$console" "${update[@]+"${update[@]}"}"
