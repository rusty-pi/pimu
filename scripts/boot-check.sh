#!/usr/bin/env bash
# Run the simulated boot and check it against the milestones it is known to
# reach. This is what CI runs; run it locally to reproduce a CI failure.
#
#   scripts/boot-check.sh [boot.log]
#
# Assumes `cargo build --release` and an SD image built by scripts/make-sd.sh.
# Set RVF_BOOT_WALL to change the wall-clock budget (default 330 s; the runners
# are roughly 1.6x slower than a dev box, where the last milestone lands around
# 90 s). The budget went 290 -> 330 s so the run reaches the codec licence
# check, which is what exercises the VCE guard below.
set -uo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
log="${1:-$here/boot.log}"
wall="${RVF_BOOT_WALL:-330}"
bin="$here/target/release/rpi-virt-fw"

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
# The PCIe link trains and the bus scan finds the VL805 that is soldered to
# every Pi 4B (#18). Same identity the real board prints, sd-card-boot.log:27.
want 'PCIe endpoint enumeration'           'PCIe scan 00001106:00003483'
# The bootloader reads the VL805's xHCI capability registers. It reaches them
# through 40-bit DMA4 transfers into the PCIe outbound window, not a load, so
# this line is the end-to-end proof of that path (src/periph/pcie.rs). The
# values are byte-identical to the real board, sd-card-boot.log:29-32.
want 'xHCI capability registers'           'xHC0 ver: 256 HCS: 05000420 fc000031 00e70004 HCC: 002841eb'
want 'xHCI port and slot counts'           'xHC0 ports 5 slots 32 intrs 4'
must_not 'PCIe link never trained'         'PCIe timeout'
must_not 'xHC bring-up failed'             'USB xHC init failed'
# The xHCI ring engine enumerates the VIA Labs hub that is soldered to root
# port 1 of every Pi 4B: port connect, reset, Enable Slot / Address Device, and
# GET_DESCRIPTOR over the control ring. All four lines are byte-identical to
# the real board, sd-card-boot.log:36-39 (#18 stage 3).
want 'USB2 root port reports the hub'      'USB2\[1\] 400202e1 connected'
want 'root hub port init'                  'USB2 root HUB port 1 init'
want 'hub enumerated over the control ring' 'DEV \[01:00\] 2.16 000000:01 class 9 VID 2109 PID 3431'
want 'hub driver bound'                    'HUB init \[01:00\] 2.16 000000:01'
# The pre-handover XHCI-STOP: EINT | PCD, an event posted and a port change
# seen. A board with nothing on the bus prints `USBSTS 0`; the reference board
# prints 18 (sd-card-boot.log:73) because of the hub above.
want 'xHCI stopped with events pending'    'USBSTS 18'
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
# Bounded, not a retry storm: posting into HDMI1's event-flags group used to
# re-trigger its EDID fetch on every pass (71bd94a).
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
# The console baud rate is recomputed for the ARM once the VPU is done with it
# (vc4-boot.log 62-65). Getting here needs the ASB bridge handshake modelled:
# start4 gates the H264 power domain on the way, and with `0x7E00_A000`
# unmapped the bridge never acknowledged the stop request (`src/periph/asb.rs`).
want 'final baud rate'                     'Set PL011 baud rate to 103448'
# Exactly one "Set", two "done" — that asymmetry is real, not a modelling gap
# (#20). The clock notifier calls the PL011 callback `0x3EC799BC` twice in
# phase 0 and twice in phase 2. Phase 0 prints, then clears the "console
# usable" flag `[gp+5176]` and writes `UARTCR = 0`; the console writer
# `0x3ED85E9C` drops every byte while that flag is 0, so the second phase-0
# line never reaches the wire. Phase 2 restores the flag before it prints, so
# both "done" lines do. `vc4-boot.log` shows two "Set" lines because it is a
# `vcdbg` dump of the firmware's message ring (it keeps logging past
# `arm_loader`, when the UART already belongs to Linux), not a serial capture.
# The genuine UART captures agree with the model: sd-card-boot-perfect.log
# lines 137-139 are one "Set" and two "done".
count_eq 'baud rate change starts'         'Set PL011 baud rate' 1
count_eq 'baud rate change completes'      'Baud rate change done' 2
# The last thing before `arm_loader`: `codec_enabled` runs its licence-key
# check on the VCE and waits on interrupt source 68. With the block unmapped
# that wait timed out and start4 printed this instead (`src/periph/vce.rs`).
must_not 'VCE launch never completed'      'VCE taking >1s to run'
# The board-identity check `arm_loader` gates the ARM launch on reads OTP rows
# 19..26; with them blank it failed and start4 blinked LED error code 4-4
# ("unsupported board type") forever (`src/periph/configotp.rs`).
want 'SD card power handover'              'pin SDCARD_CONTROL_POWER not defined'
want 'watchdog stopped'                    'Watchdog stopped'
# The goal: the VPU hands the board over to the ARM, with the same split of a
# 1 GB board's memory the reference reports (vc4-boot.log 68).
want 'ARM handover'                        'arm_loader: Starting ARM with 948MB'
# The point of the bench (#5, rpi-mkosi#37). Before releasing the ARM,
# `arm_loader` patches `/chosen` and publishes `rpi-machine-id` at `0x3EC568F8`
# — the string a `rpi-mkosi` image turns into its root-LUKS passphrase. `recon`
# reads the patched blob back out of DRAM at the address the firmware logged
# and prints the identity properties, so a firmware bump that changes the
# derivation fails right here instead of on deployed cards.
#
# Pinning the value is safe: every OTP row it derives from is invented in
# `src/periph/configotp.rs` (see the OTP rule in CLAUDE.md), so this is the
# model's own identity and not any real board's. `rpi-serial64` is the same
# two rows unhashed, which is what makes the pair a useful regression: if the
# serial still matches but the machine id does not, the *derivation* moved.
want 'patched device tree read back'       'device tree handed to the ARM'
want 'chosen serial published'             '/chosen/rpi-serial64 .*"fa1e00231aa2bb31"'
want 'rpi-machine-id published'            '/chosen/rpi-machine-id .*"2928640898f6b5035da98885da0ac498"'
# And the derivation itself, not just its output (#22). The EEPROM bootloader —
# not `start4` — computes the identity as `SHA-256(otp[28] | otp[35] | otp[30] |
# otp[64] | otp[65])` truncated to 16 bytes, each row a little-endian word, and
# stages it at `BVER + 0x8c` for `start4` to hex-encode. `src/identity.rs`
# recomputes it from the modelled fuses, so this line distinguishes "the fuses
# changed" from "the algorithm changed" — the latter is what would silently move
# a deployed card's root-LUKS passphrase (rpi-mkosi#37).
want 'machine-id derivation reproduced'    'matches the value the firmware published'
must_not 'derivation no longer predicted'  'MISMATCH: the EEPROM bootloader'
# With the ARM running the VPU keeps polling the LPDDR4 MR4 temperature code
# once a second and rescales the DRAM refresh interval by 1 << (3 - code); the
# reference board reports code 2 and the interval doubles (vc4-boot.log 69,
# `sdram: sdram refresh 1562->3124 (2)`). That line never reaches a UART — the
# console belongs to Linux by then and only the firmware's internal message
# ring has it — so the check is on the controller state instead
# (`src/periph/sdc.rs` models the mode-register port at `0x7E00_109C`).
want 'SDRAM refresh rescaled after handover' 'refresh interval 658 -> 1562 -> 3124'
# No nop-slides at all: the register file must survive preemptive context
# switches (8d7c27a) and no callback may be null.
must_not 'derailed into a nop-slide'       '\[derail\]'

# `recon` now stops on an instruction the decoder does not implement rather
# than stepping over it, so a skip can only come from the remaining recon
# leniencies (`bkpt` padding, `sleep`, an unhandled `swi`) — never from an
# unknown opcode. Nothing in a clean boot should need even those.
skipped="$(sed -n 's/^retired .*(skipped \([0-9]*\).*/\1/p' "$log" | tail -1)"
if [ -z "$skipped" ]; then
  echo "MISSING: could not read the skipped-instruction count from the report" >&2
  fail=1
elif [ "$skipped" -ne 0 ]; then
  echo "COUNT: skipped instructions is $skipped, want 0" >&2
  fail=1
fi
# The wall clock is the fallback stop, not the primary one: an unknown
# instruction must be reported as a fault, with its pc and raw bytes.
must_not 'stopped on an unimplemented instruction' 'Unimplemented'

if [ "$fail" -ne 0 ]; then
  echo "boot check FAILED" >&2
  exit 1
fi
echo "boot check passed"
