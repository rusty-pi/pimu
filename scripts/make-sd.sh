#!/usr/bin/env bash
# Build a bootable SD-card image for the model: MBR, a FAT32 boot partition
# holding the real Raspberry Pi 4 firmware (start4.elf + fixup4.dat), a config
# and the device tree, and an ext4 root filesystem with busybox for Linux.
# Consumed by `pimu boot --sd <img>`.
#
# No root / loop devices — sfdisk writes the partition table, mtools and
# `mke2fs -d` write the filesystems at byte offsets.
set -euo pipefail
# mtools stamps directory entries with this instead of the build time.
export SOURCE_DATE_EPOCH=315532800   # 1980-01-01, the FAT epoch
# mtools writes that time in the local timezone: without this a builder east
# of UTC stamps 02:00, and the card's bytes differ from CI's.
export TZ=UTC
# Globs sort by the locale's collation, and the order files are copied in is
# the order of their directory entries. An en_US host put vc4-kms-v3d.dtbo
# before vc4-kms-v3d-pi4.dtbo and the C-locale CI runner the other way round,
# so the firmware found the pi4 overlay one entry later here: 180 us of
# modelled time, enough to reorder two kernel lines in the Linux golden.
export LC_ALL=C

here="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$here/firmware/sd.img}"
fw="$here/firmware"
cache="${RPI_MKOSI_CACHE:-$HOME/src/rpi-mkosi/cache}"

# The boot partition's geometry is in the golden transcript (the bootloader
# prints its FAT cluster count), and the card's size is in it too (the CSD),
# so neither changes casually.
size_mb=256              # MBR + boot partition
root_mb=32               # the root filesystem after it
part_start=2048          # sectors (1 MiB alignment)
sector=512

# `OTG=1` adds `otg_mode=1` to config.txt: the firmware then gives Linux the
# BCM2711's own xHCI on the USB-C port (`xhci@7e9c0000`) instead of the DWC2
# core, which is what `boot --otg <img>` plugs a stick into. A card that
# the firmware *booted* from that port gets the node either way.
#
# `START4=start4cd` (the cut-down firmware) or `START4=start4db` (the
# debug build) puts that variant on the card next to the full pair, with the
# config.txt line that makes the bootloader pick it, as on a real card.
start4="${START4:-start4}"
case "$start4" in
  start4)   select= ;;
  start4cd) select=gpu_mem=16 ;;
  start4db) select=start_debug=1 ;;
  *) echo "unknown START4=$start4 (start4, start4cd or start4db)" >&2; exit 1 ;;
esac
fixup4="${start4/start/fixup}"
if [[ "$start4" != start4 ]]; then
  for f in "$start4.elf" "$fixup4.dat"; do
    [[ -f "$fw/$f" ]] || { echo "missing $fw/$f (run fetch-firmware.sh)" >&2; exit 1; }
  done
fi

echo "building $out ($(( size_mb + root_mb )) MiB: FAT32 boot, ext4 root)"
rm -f "$out"
truncate -s "$(( size_mb + root_mb ))M" "$out"

# --- partition table: FAT32 LBA boot (0x0c), then Linux (0x83) -----------------
root_start=$(( size_mb * 1024 * 1024 / sector ))
sfdisk --quiet --label dos "$out" <<EOF
label-id: 0x5250494d
${part_start},$(( root_start - part_start )),c,*
${root_start},,83
EOF

part_offset=$(( part_start * sector ))
part_bytes=$(( size_mb * 1024 * 1024 - part_offset ))

# --- filesystem --------------------------------------------------------------
mformat -i "$out@@${part_offset}" -F -v RPIBOOT -N 5250494d -T $(( part_bytes / sector )) ::

copy() {
  local src="$1" dst="$2"
  if [[ -f "$src" ]]; then
    mcopy -i "$out@@${part_offset}" -o "$src" "::${dst}"
    echo "  + $dst  ($(stat -Lc %s "$src") bytes)"
  else
    echo "  ! missing $src (skipped)" >&2
  fi
}

# The config the reference boot log was captured with (a stock Raspberry Pi OS
# config.txt with the UART console enabled). The dtparam/dtoverlay lines are
# what produce the `dtparam: spi=on` / `Loaded overlay '...'` lines in a
# start4 log from a Raspberry Pi 4B d03115 — a bare three-line config skips that
# whole phase, so the model has nothing to match against.
#
# `WIRELESS=1` drops the two `disable-` overlays, which is what a card
# straight off the imager has. The board then looks quite different:
# the base device tree keeps `serial0 = &uart1`, so the console is the
# mini-UART and not the PL011, and the WiFi SDIO host and the Bluetooth
# modem's UART are both live.
wireless="${WIRELESS:-0}"
# `BRCMFMAC=1` is the card that can bring the WiFi chip up: the kernel modules
# `brcmfmac` needs and the CYW43455's own firmware on the root filesystem
# (scripts/fetch-firmware.sh puts them under firmware/wifi/), plus the busybox
# applets that load them and the `modules.dep` the kernel's own
# `request_module` needs. It implies `WIRELESS=1`: with `dtoverlay=disable-wifi`
# on the card the `mmcnr@7e300000` node is off and there is no SDIO card for
# the driver to find at all.
brcmfmac="${BRCMFMAC:-0}"
if [[ "$brcmfmac" == 1 ]]; then wireless=1; fi
# `EEPROM_SPI=1` remaps SPI0 onto the bootloader EEPROM's own pins, which is how
# `rpi-eeprom-update` reads and writes the flash from Linux with no recovery.bin
# and no power cycle (#161). `spi-gpio40-45` puts the master on GPIO 40..42 and
# its three chip selects on 43..45 as plain outputs, and `audremap` is what
# moves PWM audio off 40/41 so the overlay can have them. /dev/spidev0.0 then
# reaches the flash; without the pair it reaches the header pins, where
# `flashrom --flash-name` answers `No EEPROM/flash device found`.
eeprom_spi="${EEPROM_SPI:-0}"
# Which tty the kernel's `console=serial0` ends up being, and so where the
# shell goes: `serial0` is the PL011 with Bluetooth disabled and the
# mini-UART with it enabled.
if [[ "$wireless" == 1 ]]; then console_tty=ttyS0; else console_tty=ttyAMA0; fi
tmpcfg="$(mktemp)"
{
  cat <<'EOF'
enable_uart=1
uart_2ndstage=1

dtparam=spi=on
dtparam=audio=off
EOF
  if [[ "$eeprom_spi" == 1 ]]; then
    cat <<'EOF'

dtoverlay=audremap
dtoverlay=spi-gpio40-45
EOF
  fi
  if [[ "$wireless" != 1 ]]; then
    cat <<'EOF'

dtoverlay=disable-bt
dtoverlay=disable-wifi
EOF
  fi
  cat <<'EOF'

camera_auto_detect=1
display_auto_detect=1
auto_initramfs=1

dtoverlay=vc4-kms-v3d
max_framebuffers=2
disable_fw_kms_setup=1

arm_64bit=1
disable_overscan=1
arm_boost=1
EOF
} >"$tmpcfg"
if [[ -n "$select" ]]; then echo "$select" >>"$tmpcfg"; fi
if [[ "${OTG:-0}" == 1 ]]; then echo "otg_mode=1" >>"$tmpcfg"; fi
mcopy -i "$out@@${part_offset}" -o "$tmpcfg" ::config.txt
echo "  + config.txt"
rm -f "$tmpcfg"

copy "$fw/start4.elf"                 start4.elf
copy "$fw/fixup4.dat"                 fixup4.dat
if [[ "$start4" != start4 ]]; then
  copy "$fw/$start4.elf"              "$start4.elf"
  copy "$fw/$fixup4.dat"              "$fixup4.dat"
fi

# The ARM device tree (used by start4 late, at the ARM handoff). Prefer a
# repo-local copy; fall back to the rpi-mkosi cache (flat or firmware/ subdir).
for dtb in "$fw/bcm2711-rpi-4-b.dtb" "$cache/bcm2711-rpi-4-b.dtb" \
           "$cache/firmware/bcm2711-rpi-4-b.dtb"; do
  [[ -f "$dtb" ]] && { copy "$dtb" bcm2711-rpi-4-b.dtb; break; }
done

# gpioman pin config. Without it the firmware retries gpioman forever in the
# model (built-in dt-blob fallback isn't reproduced).
if [[ -f "$fw/dt-blob.dts" && ! -f "$fw/dt-blob.bin" ]]; then
  "$here/scripts/make-dt-blob.py" "$fw/dt-blob.dts" "$fw/dt-blob.bin"
fi
copy "$fw/dt-blob.bin"                dt-blob.bin

# Kernel + overlays, so the boot has something to hand off to. The reference log
# loads these right after the HDMI bring-up.
#
# `KERNEL=halt` puts a kernel8.img there that parks the ARM instead: an arm64
# Image header, then `msr daifset, #0xf; wfi; b .-4`. That is the card for the
# boots that end at the handover: the ARM is always modelled, and this
# gives it nothing to do, so the run ends where the firmware goes quiet.
if [[ "${KERNEL:-linux}" == halt ]]; then
  tmpk="$(mktemp)"
  {
    printf '\x10\x00\x00\x14\x00\x00\x00\x00'   # b 0x40; code1
    printf '\x00%.0s' $(seq 8)                   # text_offset 0
    printf '\x4c\x00\x00\x00\x00\x00\x00\x00'   # image_size
    printf '\x0a\x00\x00\x00\x00\x00\x00\x00'   # flags: LE, 4K pages, anywhere
    printf '\x00%.0s' $(seq 24)                  # res2..res4
    printf 'ARM\x64\x00\x00\x00\x00'            # magic, res5
    printf '\xdf\x4f\x03\xd5'                   # msr daifset, #0xf
    printf '\x7f\x20\x03\xd5'                   # wfi
    printf '\xff\xff\xff\x17'                   # b .-4
  } >"$tmpk"
  copy "$tmpk" kernel8.img
  rm -f "$tmpk"
else
  copy "$fw/kernel8.img"              kernel8.img
fi

tmpcmd="$(mktemp)"
echo "console=serial0,115200 console=tty1 root=/dev/mmcblk0p2 rootfstype=ext4 fsck.repair=yes rootwait" >"$tmpcmd"
mcopy -i "$out@@${part_offset}" -o "$tmpcmd" ::cmdline.txt
echo "  + cmdline.txt"
rm -f "$tmpcmd"

if [[ -d "$fw/overlays" ]]; then
  mmd -i "$out@@${part_offset}" ::overlays 2>/dev/null || true
  for ovl in "$fw/overlays"/*; do
    [[ -f "$ovl" ]] || continue
    copy "$ovl" "overlays/$(basename "$ovl")"
  done
fi

# --- root filesystem: busybox init and a shell on the serial console -----------
# The kernel mounts it read-only (no `rw` on the command line), so a run never
# changes the image. Fixed UUID, hash seed and timestamps keep the image — and
# what Linux prints about it — the same on every build.
rootfs="$(mktemp -d)"
mkdir -p "$rootfs"/{bin,sbin,etc/init.d,proc,sys,dev,tmp,root}
if [[ -f "$fw/busybox-aarch64" ]]; then
  cp "$fw/busybox-aarch64" "$rootfs/bin/busybox"
  for applet in sh ash cat cut date df dmesg echo env false free grep head hostname \
                id kill ln ls mkdir mount mv ps pwd readlink rm sed sleep sort stty \
                sync tail test top touch tr true umount uname uptime vi wc; do
    ln -s busybox "$rootfs/bin/$applet"
  done
  # Loading the WiFi modules and looking at what they register needs four more
  # applets; they are left off the other cards so those stay byte-identical.
  if [[ "$brcmfmac" == 1 ]]; then
    for applet in insmod lsmod rmmod ip; do
      ln -s busybox "$rootfs/bin/$applet"
    done
  fi
  # And the EEPROM card needs `insmod` for spidev, plus `devmem` and `printf`
  # to work the master's registers by hand: nothing in busybox speaks
  # `SPI_IOC_MESSAGE`, so a full-duplex flash command is driven straight
  # through the registers (#161).
  if [[ "$eeprom_spi" == 1 ]]; then
    for applet in insmod devmem printf; do
      [[ -e "$rootfs/bin/$applet" ]] || ln -s busybox "$rootfs/bin/$applet"
    done
  fi
  ln -s ../bin/busybox "$rootfs/sbin/init"
  for applet in halt poweroff reboot; do
    ln -s ../bin/busybox "$rootfs/sbin/$applet"
  done
  echo "  + p2: busybox ($(stat -Lc %s "$fw/busybox-aarch64") bytes)"
else
  echo "  ! missing $fw/busybox-aarch64 (root filesystem left without init; run fetch-firmware.sh)" >&2
fi
# rpi-fw-crypto, to ask start4's crypto service for an HMAC from Linux
# userspace, with exactly the shared libraries it loads.
userland="$fw/arm64-userland"
if [[ -f "$userland/usr/bin/rpi-fw-crypto" ]]; then
  mkdir -p "$rootfs"/{lib,usr/bin,usr/lib/aarch64-linux-gnu}
  cp "$userland/usr/bin/rpi-fw-crypto" "$rootfs/usr/bin/"
  for so in ld-linux-aarch64.so.1 libc.so.6 librpifwcrypto.so.0 libgnutls.so.30 \
            libp11-kit.so.0 libffi.so.8 libidn2.so.0 libunistring.so.5 libtasn1.so.6 \
            libnettle.so.8 libhogweed.so.6 libgmp.so.10; do
    cp -L "$userland/usr/lib/aarch64-linux-gnu/$so" "$rootfs/usr/lib/aarch64-linux-gnu/$so"
  done
  ln -s ../usr/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1 "$rootfs/lib/ld-linux-aarch64.so.1"
  echo "  + p2: rpi-fw-crypto and its libraries"
else
  echo "  ! missing $userland (no rpi-fw-crypto on the card; run fetch-firmware.sh)" >&2
fi
# The WiFi driver and the chip's own firmware, laid out as /lib/modules and
# /lib/firmware. `brcmfmac` downloads brcmfmac43455-sdio.bin into the chip over
# SDIO at probe time and the kernel's filesystem firmware loader finds it
# under /lib/firmware, so both have to be on the root filesystem.
if [[ "$brcmfmac" == 1 ]]; then
  if [[ -d "$fw/wifi/lib" ]]; then
    mkdir -p "$rootfs/lib"
    cp -a "$fw/wifi/lib/." "$rootfs/lib/"
    # The modules ship as .ko.xz. Unpack them here rather than on the card:
    # busybox's insmod need not have seamless xz built in, and a plain
    # `insmod foo.ko` is one less thing between the scenario and the driver.
    find "$rootfs/lib/modules" -name '*.ko.xz' -exec xz -d {} +
    # `brcmfmac` asks the kernel for its vendor half by name —
    # `brcmf_fwvid_attach` does `request_module("brcmfmac-cyw")` and fails the
    # whole attach when that does not come back — so the card needs a
    # `/sbin/modprobe` for the kernel to run and a `modules.dep` for it to
    # read. Each line is a module and what it depends on, paths relative to
    # /lib/modules/<release>; the dependencies come out of the modules' own
    # `depends=` fields rather than a list kept by hand here.
    ln -s ../bin/busybox "$rootfs/sbin/modprobe"
    for moddir in "$rootfs/lib/modules"/*/; do
      declare -A modpath=()
      while IFS= read -r ko; do
        modpath["$(basename "$ko" .ko)"]="${ko#"$moddir"}"
      done < <(find "$moddir" -name '*.ko' | sort)
      : >"$moddir/modules.dep"
      for name in "${!modpath[@]}"; do
        deps=""
        for dep in $(tr '\0' '\n' <"$moddir${modpath[$name]}" | sed -n 's/^depends=//p' |
                     head -1 | tr ',' ' '); do
          if [[ -n "${modpath[$dep]:-}" ]]; then deps+=" ${modpath[$dep]}"; fi
        done
        echo "${modpath[$name]}:$deps" >>"$moddir/modules.dep"
      done
      LC_ALL=C sort -o "$moddir/modules.dep" "$moddir/modules.dep"
    done
    echo "  + p2: $(find "$rootfs/lib/modules" -name '*.ko' | wc -l) kernel modules and the CYW43455's firmware"
  else
    echo "  ! missing $fw/wifi (no WiFi driver on the card; run fetch-firmware.sh)" >&2
  fi
  # `iw`, the only way to ask the driver what it found: busybox has no applet
  # for nl80211, so without this the card can bring `wlan0` up and learn
  # nothing more about it. libc and the loader are already here for
  # rpi-fw-crypto; these two are what `iw` adds.
  if [[ -x "$userland/usr/sbin/iw" ]]; then
    mkdir -p "$rootfs"/{usr/sbin,usr/lib/aarch64-linux-gnu}
    cp "$userland/usr/sbin/iw" "$rootfs/usr/sbin/"
    for so in libnl-3.so.200 libnl-genl-3.so.200; do
      cp -L "$userland/usr/lib/aarch64-linux-gnu/$so" "$rootfs/usr/lib/aarch64-linux-gnu/$so"
    done
    echo "  + p2: iw ($(stat -Lc %s "$userland/usr/sbin/iw") bytes) and libnl"
  else
    echo "  ! missing $userland/usr/sbin/iw (nothing on the card can read nl80211; run fetch-firmware.sh)" >&2
  fi
fi
# `spi-bcm2835` and `spidev`, so that /dev/spidev0.0 exists: both are modules in
# the stock kernel, and with `dtoverlay=spi-gpio40-45` the bus behind them is the
# bootloader EEPROM's (#161). Left where the scenario insmods them from.
if [[ "$eeprom_spi" == 1 ]]; then
  if [[ -d "$fw/spi/lib" ]]; then
    mkdir -p "$rootfs/lib"
    cp -a "$fw/spi/lib/." "$rootfs/lib/"
    find "$rootfs/lib/modules" -name '*.ko.xz' -exec xz -d {} +
    echo "  + p2: the SPI master's driver and spidev"
  else
    echo "  ! missing $fw/spi (no SPI modules on the card; run fetch-firmware.sh)" >&2
  fi
fi
# The firmware's command line ends in `console=tty1`, which makes the
# framebuffer /dev/console; name the serial port instead.
cat >"$rootfs/etc/inittab" <<EOF
${console_tty}::sysinit:/etc/init.d/rcS
${console_tty}::respawn:-/bin/sh
::ctrlaltdel:/sbin/reboot
EOF
cat >"$rootfs/etc/init.d/rcS" <<'EOF'
#!/bin/sh
mount -t proc proc /proc
mount -t sysfs sysfs /sys
echo "pimu: userland up, $(uname -sr), $(grep -c ^processor /proc/cpuinfo) CPUs"
EOF
chmod -R u=rwX,go=rX "$rootfs"
chmod 755 "$rootfs/etc/init.d/rcS"
[[ -f "$rootfs/bin/busybox" ]] && chmod 755 "$rootfs/bin/busybox"
find "$rootfs" -exec touch -h -d @0 {} +
# No `orphan_file` (e2fsprogs 1.47's default): with it a read-only mount logs an
# orphan cleanup even on a clean filesystem.
E2FSPROGS_FAKE_TIME=1 mke2fs -q -t ext4 -O ^orphan_file -L rootfs \
  -U 7b3f1c8e-0d2a-4c5e-9f61-2a4b6c8d0e1f -E root_owner=0:0,hash_seed=7b3f1c8e-0d2a-4c5e-9f61-2a4b6c8d0e1f,offset=$(( root_start * sector )) \
  -d "$rootfs" "$out" "$(( root_mb * 1024 ))k"
# `mke2fs -d` copies the builder's uid/gid and gives every inode a random
# generation number; make everything root's, generation 0.
( cd "$rootfs" && { echo /; echo /lost+found; find . -mindepth 1 | sed 's|^\.||'; } | while read -r p; do
    echo "sif \"$p\" uid 0"
    echo "sif \"$p\" gid 0"
    echo "sif \"$p\" generation 0"
    echo "sif \"$p\" ctime 0"
  done ) | E2FSPROGS_FAKE_TIME=1 debugfs -w -f - "$out?offset=$(( root_start * sector ))" >/dev/null 2>&1
rm -rf "$rootfs"
echo "  + p2: ext4 root filesystem (${root_mb} MiB)"

echo "done. contents:"
mdir -i "$out@@${part_offset}" ::
