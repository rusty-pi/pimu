#!/usr/bin/env bash
# Boot the device tree our firmware hands the ARM under a stock QEMU and check
# that Linux accepts it. This is what CI runs; run it locally to reproduce a CI
# failure.
#
#   scripts/qemu-check.sh [workdir]
#
# Assumes `cargo build --release` and an SD image built by scripts/make-sd.sh.
# Everything this produces lands in <workdir> (default firmware/qemu-check,
# which .gitignore already covers): the patched SD image, the extracted kernel,
# handoff.dtb, and the two logs.
#
# What this proves, and what it does not:
#
#   * It proves Linux *boots* the tree `arm_loader` publishes — that the nodes,
#     the overlays merged into it and the `/chosen` patches are well-formed
#     enough for the kernel to bring up the machine on them. A firmware bump
#     that emits a tree Linux chokes on fails here, which the transcript check
#     in scripts/boot-check.sh cannot see.
#   * It does NOT prove the tree reaches Linux unchanged. QEMU rewrites parts of
#     the blob on the way in and says so — `warning: bcm2711 dtc:
#     brcm,bcm2711-pcie/rng200/thermal/genet-v5 has been disabled!`. The check
#     for "unchanged" is a `--dump-fdt` diff between two firmware versions, not
#     this job.
#
# Set RVF_BOOT_WALL to change the simulated boot's wall-clock budget (default
# 330 s, same as scripts/boot-check.sh: the runners are roughly 1.6x slower than
# a dev box, where the device tree lands around 90 s). RVF_QEMU_WALL bounds the
# QEMU boot itself (default 90 s); it is a guest kernel booting to a wait, so it
# never exits on its own and the timeout is the normal way this ends.
set -uo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
work="${1:-$here/firmware/qemu-check}"
wall="${RVF_BOOT_WALL:-330}"
qemu_wall="${RVF_QEMU_WALL:-90}"
bin="$here/target/release/rpi-virt-fw"
qemu="${QEMU:-qemu-system-aarch64}"

# The FAT32 boot partition starts at sector 2048 — see scripts/make-sd.sh, which
# is what wrote this image.
part_offset=$(( 2048 * 512 ))

# Fail loudly rather than passing vacuously: a missing QEMU must not look like a
# clean run. On Debian/Ubuntu the package is `qemu-system-arm`.
if ! command -v "$qemu" >/dev/null 2>&1; then
  echo "MISSING: $qemu is not installed (Debian/Ubuntu: apt-get install qemu-system-arm)" >&2
  exit 1
fi
# The machine has to exist in *this* QEMU. `raspi4b` arrived in 7.1 (it was
# `raspi4` before that), and a build without it fails with a bare "unsupported
# machine type" that says nothing about which machines it does have. Exit 2
# rather than 1 so a caller can tell "this QEMU is too old" apart from "the
# device tree is bad" — CI reports the former as a skipped check, because a
# toolchain gap is not a regression in the firmware model.
if ! "$qemu" -machine help 2>/dev/null | grep -q '^raspi4b '; then
  echo "SKIP: $qemu has no raspi4b machine ($("$qemu" --version | head -1))." >&2
  echo "      Available raspi machines:" >&2
  "$qemu" -machine help 2>/dev/null | grep -E '^raspi' >&2 || echo "      (none)" >&2
  exit 2
fi
for f in "$bin" "$here/firmware/pieeprom.bin" "$here/firmware/sd.img"; do
  if [ ! -f "$f" ]; then
    echo "MISSING: $f (run scripts/fetch-firmware.sh, scripts/make-sd.sh, cargo build --release)" >&2
    exit 1
  fi
done

rm -rf "$work"
mkdir -p "$work"
img="$work/sd.img"
cp "$here/firmware/sd.img" "$img"

# --- config.txt without KMS ---------------------------------------------------
# A tree carrying `dtoverlay=vc4-kms-v3d` panics the guest on the way up:
#
#   Internal error: synchronous external abort ... brcmstb_l2_intc_probe
#
# That is the HDMI L2 interrupt controller at 0x7ef00100, which `raspi4b` does
# not model — a QEMU gap, not a firmware one. We pin a KMS-free config for this
# job rather than asserting the abort, because an expected-panic assertion would
# swallow every *other* external abort too: any real regression in the tree
# would look exactly like the one we tolerate. With KMS out of the way the guest
# runs far enough to assert forward progress, which is the whole point.
#
# The rest of the file is the SD image's own config.txt (scripts/make-sd.sh), so
# the dtparam/dtoverlay merging this exercises is still the real thing.
cfg="$work/config.txt"
cat >"$cfg" <<'EOF'
enable_uart=1
uart_2ndstage=1

dtparam=spi=on
dtparam=audio=off

dtoverlay=disable-bt
dtoverlay=disable-wifi

camera_auto_detect=1
display_auto_detect=1
auto_initramfs=1

arm_64bit=1
disable_overscan=1
arm_boost=1
EOF
mcopy -i "$img@@${part_offset}" -o "$cfg" ::config.txt

# The kernel comes out of the SD image the firmware just booted from, not from a
# separate download: QEMU has to be handed the same `kernel8.img` the simulated
# `arm_loader` loaded, or the pair proves nothing.
mcopy -i "$img@@${part_offset}" -n ::kernel8.img "$work/kernel8.img"

# --- the simulated boot, for its device tree ----------------------------------
dtb="$work/handoff.dtb"
boot_log="$work/boot.log"
export RVF_LIVE_CONSOLE=1
timeout --signal=INT "$(( wall + 10 ))" \
  "$bin" recon "$here/firmware/pieeprom.bin" \
    --eeprom --sd "$img" \
    --max-wall "$wall" \
    --dump-fdt "$dtb" \
  >"$boot_log" 2>&1
status=$?
echo "recon exit status: $status"
# 124 = hit the wall clock; that is expected, not a failure.
if [ "$status" -ne 0 ] && [ "$status" -ne 124 ]; then
  tail -40 "$boot_log" >&2
  exit "$status"
fi
if [ ! -s "$dtb" ]; then
  echo "MISSING: $dtb — the boot never reached 'Device tree loaded to'" >&2
  tail -40 "$boot_log" >&2
  exit 1
fi
echo "device tree: $dtb ($(stat -Lc %s "$dtb") bytes)"

# --- boot it ------------------------------------------------------------------
# No disk: the guest is expected to stop at `Waiting for root device` (see
# below), so there is nothing for it to find. `-display none` keeps it headless;
# stdin is closed so a guest that does reach a shell cannot block the run.
qemu_log="$work/qemu.log"
timeout --signal=TERM "$qemu_wall" \
  "$qemu" -M raspi4b -m 2G \
    -kernel "$work/kernel8.img" \
    -dtb "$dtb" \
    -display none -serial stdio \
    </dev/null >"$qemu_log" 2>&1
qstatus=$?
echo "qemu exit status: $qstatus"
# 124 = the timeout fired, which is the normal end: the guest is parked in the
# root-device wait and never exits.
if [ "$qstatus" -ne 0 ] && [ "$qstatus" -ne 124 ]; then
  tail -40 "$qemu_log" >&2
  exit "$qstatus"
fi

fail=0
# want <what it proves> <pattern>
want() {
  local why="$1" pat="$2"
  if ! grep -q "$pat" "$qemu_log"; then
    echo "MISSING: $pat  ($why)" >&2
    fail=1
  fi
}
must_not() {
  local why="$1" pat="$2"
  if grep -q "$pat" "$qemu_log"; then
    echo "PRESENT: $pat  ($why)" >&2
    fail=1
  fi
}

# The kernel read the board identity out of our tree's root node — so the blob
# parsed, and it parsed as a Pi 4B and not as something generic.
want 'board identity from our /model'      'Machine model: Raspberry Pi 4 Model B'
# All four cores came up off the tree's /cpus, enable-method and spin-table
# release addresses included. A malformed /cpus leaves the guest single-core.
want 'all four CPUs brought up'            'SMP: Total of 4 processors activated'
# The mailbox node at fe00b880 is the one Linux reaches the firmware through
# (docs/vision.md §3). It probing is what makes the rest of the Pi-specific
# stack — clocks, power domains, vcio — reachable at all.
want 'mailbox node probed'                 'bcm2835-mbox fe00b880.mailbox: mailbox enabled'
want 'firmware node bound'                 'raspberrypi-firmware soc:firmware: Attached to firmware'
# `/chosen/bootargs` — the command line the firmware assembled from cmdline.txt
# plus its own additions — reached the kernel intact.
want 'kernel command line from /chosen'    'Kernel command line: .*console=ttyAMA0\|Kernel command line: .*console=serial0'
# The far end: the guest got through every probe that the tree drives and is
# parked waiting for a root filesystem. That is the expected result *until #23
# lands*: the SD/eMMC controller needs a working property mailbox, and until our
# model services one the card never appears. When #23 lands this assertion is
# the one to change — the guest should get past the wait and mount a root.
want 'reached the root-device wait'        'Waiting for root device'
# The guest must not have died on the way. The KMS/L2-intc abort above is the
# known instance of this; it is fenced off by the config, so any abort now is a
# genuine regression in the tree we publish.
must_not 'guest took a synchronous abort'  'Internal error: synchronous external abort'
must_not 'guest panicked'                  'Kernel panic'
# A tree QEMU could not parse at all never gets as far as the asserts above, but
# it is worth naming the failure when it happens.
must_not 'QEMU rejected the device tree'   'cannot load device tree'

if [ "$fail" -ne 0 ]; then
  echo "qemu check FAILED (see $qemu_log)" >&2
  exit 1
fi
echo "qemu check passed"
