#!/usr/bin/env bash
# Run the simulated boot and check it against the boot scenario. This is what
# CI runs; run it locally to reproduce a CI failure.
#
#   scripts/boot-check.sh [--update] [--scenario=<toml>] [boot.log]
#
# Needs `cargo build --release` and the boot media the scenario names: an SD
# image built by scripts/make-sd.sh (with `KERNEL=halt` for the boots that end
# at the handover, firmware/sd-halt.img), and for the network boots the root
# scripts/make-netboot.sh builds from that. A missing one stops the run before
# it starts, with the command that makes it.
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

# The binary is never rebuilt here: CI hands over the one its build job made.
if [ ! -x "$bin" ]; then
  echo "$bin is not built: cargo build --release" >&2
  exit 1
fi
# Only a warning, and not in CI, where file times say nothing about the build.
if [ -z "${CI:-}" ] && [ -n "$(find "$here/src" "$here/build.rs" "$here/Cargo.toml" \
    "$here/Cargo.lock" -newer "$bin" -print -quit)" ]; then
  echo "warning: $bin is older than the sources; cargo build --release to test them" >&2
fi

# One boot, two sets of assertions. The wall clock has little headroom — two
# concurrent boots miss `arm_loader` — so the run happens exactly once here and
# both checks read what it left behind.
mapfile -t plan < <("$bin" boot-check "$scenario" --plan --console "$console")
if [ "${#plan[@]}" -lt 2 ]; then
  # `--plan` said why: most often a boot medium that is not built yet.
  echo "boot-check --plan failed; nothing was run" >&2
  exit 1
fi
wall="${plan[0]#wall=}"
args=("${plan[@]:1}")

# Never check a console an earlier run left behind: a boot that cannot start
# writes none, and the check would diff the stale one instead.
rm -f "$console"

# Stream the UART console (incl. `MESS:` lines) as it is produced.
export RVF_LIVE_CONSOLE=1

# No instruction cap: the wall clock is what ends the run.
timeout --signal=INT "$(( wall + 40 ))" "$bin" "${args[@]}" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
echo "boot exit status: $status"
# 124 = hit the wall clock; that is expected, not a failure. 1 = the boot did
# not get where it was meant to, and the check below says which milestone.
if [ "$status" -ne 0 ] && [ "$status" -ne 1 ] && [ "$status" -ne 124 ]; then
  exit "$status"
fi
# ...or `boot` itself failed, before it ran or part-way through, and then it
# writes no console: its `error:` line above is the whole story.
if [ ! -e "$console" ]; then
  echo "the boot wrote no console ($console): see the error above; nothing to check" >&2
  exit "$(( status == 0 ? 1 : status ))"
fi

"$bin" boot-check "$scenario" --log "$log" --console "$console" "${update[@]+"${update[@]}"}"
