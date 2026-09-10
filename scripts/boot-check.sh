#!/usr/bin/env bash
# Run the simulated boot and check it against the milestones it is known to
# reach. This is what CI runs; run it locally to reproduce a CI failure.
#
#   scripts/boot-check.sh [boot.log]
#
# Assumes `cargo build --release` and an SD image built by scripts/make-sd.sh.
# Set RVF_BOOT_WALL to change the wall-clock budget (default 290 s; the runners
# are roughly 1.6x slower than a dev box, where the last milestone lands around
# 90 s).
set -uo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
log="${1:-$here/boot.log}"
wall="${RVF_BOOT_WALL:-290}"
bin="$here/target/release/rpi-virt-fw"

# Model the SR interrupt-enable bit and run the real ThreadX periodic tick
# through vector-table entry 64, so the scheduler actually preempts (#7). This
# is the only environment variable the boot needs — every shim is retired.
export RVF_SCHED_TICK=1
# Stream the UART console (incl. `MESS:` lines) as it is produced.
export RVF_LIVE_CONSOLE=1

# No instruction cap: the wall clock is what ends the run.
timeout --signal=INT "$(( wall + 10 ))" \
  "$bin" recon "$here/firmware/pieeprom.bin" \
    --eeprom --sd "$here/firmware/sd.img" \
    --max-wall "$wall" \
  2>&1 | tee "$log"
status=${PIPESTATUS[0]}
echo "recon exit status: $status"
# 124 = hit the wall clock; that is expected, not a failure.
if [ "$status" -ne 0 ] && [ "$status" -ne 124 ]; then
  exit "$status"
fi

fail=0
# want <what it proves> <pattern>
want() {
  local why="$1" pat="$2"
  if ! grep -q "$pat" "$log"; then
    echo "MISSING: $pat  ($why)" >&2
    fail=1
  fi
}
count_eq() {
  local why="$1" pat="$2" n="$3" got
  got="$(grep -c "$pat" "$log")"
  if [ "$got" -ne "$n" ]; then
    echo "COUNT: '$pat' is $got, want $n  ($why)" >&2
    fail=1
  fi
}
count_le() {
  local why="$1" pat="$2" n="$3" got
  got="$(grep -c "$pat" "$log")"
  if [ "$got" -gt "$n" ]; then
    echo "COUNT: '$pat' is $got, want <= $n  ($why)" >&2
    fail=1
  fi
}
must_not() {
  local why="$1" pat="$2"
  if grep -q "$pat" "$log"; then
    echo "PRESENT: $pat  ($why)" >&2
    fail=1
  fi
}

# The reset cause the bootloader reports; the real board latches HADWRF
# (1bc5fa1), matching sd-card-boot.log line 4.
want 'PM reset-status register'            'PM_RSTS 00000020'
# start4 got past the DMA transfer completion and emitted its first MESS log.
want 'first MESS log'                      'MESS:.*arasan_emmc_open'
# gpioman resolves its pin names from the real dt-blob, so the firmware itself
# emits the two "not defined" lines the reference has (vc4-boot.log 16-17).
count_eq 'gpioman pin lookup'              'pin DISPLAY_DSI_PORT not defined' 2
# gpioman configures its pin providers for real (#2): the confzilla schema pool
# has to survive its DMA relocation.
must_not 'gpioman registration regressed (#2)' 'gpioman: configuration attempt'
# The HDMI phase gives up on EDID once per controller, the way a board with no
# monitor does (vc4-boot.log 20-31). Needs SCALER_DISPID (#13).
want 'HDMI0 EDID give-up'                  'HDMI0:EDID giving up on reading EDID block 0'
want 'HDMI1 EDID give-up'                  'HDMI1:EDID giving up on reading EDID block 0'
# Bounded, not a retry storm (guard for the RVF_PMIC_EVENT window bug, 71bd94a).
count_le 'EDID retry storm'                'EDID giving up' 16
want 'hdmi_get_state deprecation'          'hdmi: HDMI:hdmi_get_state is deprecated'
want 'HDMI0 pixel-clock limit'             'HDMI0: hdmi_pixel_freq_limit'
want 'HDMI1 pixel-clock limit'             'HDMI1: hdmi_pixel_freq_limit'
# Past the second config.txt read into the restart-logging phase (line 19).
want 'restart logging'                     '\*\*\* Restart logging'
# The file-loading phase, which needed the hardware RNG modelled (e012fad):
# device tree, overlays, kernel command line, kernel (lines 32 onward).
want 'ARM device tree load'                "Loaded 'bcm2711-rpi-4-b.dtb'"
want 'config.txt dtparam'                  'dtparam: spi=on'
want 'overlay load'                        "Loaded overlay 'disable-bt'"
want 'kernel command line'                 "Read command line from file 'cmdline.txt'"
want 'kernel load'                         "Loaded 'kernel8.img'"
want 'device tree relocation'              'Device tree loaded to'
# No nop-slides at all: the register file must survive preemptive context
# switches (8d7c27a) and no callback may be null.
must_not 'derailed into a nop-slide'       '\[derail\]'

if [ "$fail" -ne 0 ]; then
  echo "boot check FAILED" >&2
  exit 1
fi
echo "boot check passed"
