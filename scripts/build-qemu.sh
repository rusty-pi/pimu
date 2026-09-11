#!/usr/bin/env bash
# Build the QEMU that runs the VideoCore: upstream at a pinned tag plus the
# patches in qemu/patches/, linked against this crate's static library.
#
#   scripts/build-qemu.sh [qemu-source-dir]
#
# With no argument the sources are cloned into qemu/src (gitignored) at
# $QEMU_TAG and the patches applied on a branch named rvf-videocore. With an
# argument, that checkout is used as-is: it is expected to already carry the
# patches (a working tree you are hacking on). The build goes to qemu/build
# ($QEMU_BUILD) and the result is qemu/build/qemu-system-aarch64.
#
# The model is found through pkg-config: this script writes
# target/release/rpi-virt-fw.pc pointing at the freshly built
# librpi_virt_fw.a and include/rvf.h, and puts that directory on
# PKG_CONFIG_PATH for QEMU's configure. QEMU's own dependencies (glib, pixman,
# libfdt, meson, ninja, ...) are yours to provide — on Debian/Ubuntu:
#
#   sudo apt-get build-dep qemu-system-arm     # or, minimally:
#   sudo apt-get install build-essential meson ninja-build libglib2.0-dev \
#        libpixman-1-dev libfdt-dev libslirp-dev python3-venv
#
# Extra configure arguments go in $QEMU_CONFIGURE_EXTRA (e.g. --meson=... or
# --extra-cflags=... for a non-standard toolchain).
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
tag="${QEMU_TAG:-v10.2.1}"
src="${1:-$here/qemu/src}"
build="${QEMU_BUILD:-$here/qemu/build}"
jobs="${JOBS:-$(nproc)}"
patches=("$here"/qemu/patches/*.patch)

# --- 1. the model, as a static library with a pkg-config file --------------
echo "== rpi-virt-fw: building librpi_virt_fw.a"
(cd "$here" && cargo build --release --lib)
# The C runtime libraries Rust's std needs when linked statically. Printed as
# a `note:` on stderr by rustc; nothing else emits that string.
static_libs="$(cd "$here" && cargo rustc --release --lib -- --print native-static-libs 2>&1 \
  | sed -n 's/.*native-static-libs: //p' | tail -1)"
pc="$here/target/release/rpi-virt-fw.pc"
cat >"$pc" <<PC
prefix=$here
libdir=\${prefix}/target/release
includedir=\${prefix}/include

Name: rpi-virt-fw
Description: Raspberry Pi 4 VideoCore firmware model
Version: $(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)
Libs: -L\${libdir} -lrpi_virt_fw $static_libs
Cflags: -I\${includedir}
PC
echo "   wrote $pc"

# --- 2. sources -------------------------------------------------------------
if [ ! -d "$src/.git" ]; then
  echo "== cloning QEMU $tag into $src"
  git clone --depth 1 --branch "$tag" https://gitlab.com/qemu-project/qemu.git "$src"
  (
    cd "$src"
    git checkout -q -b rvf-videocore
    echo "== applying ${#patches[@]} patch(es)"
    git -c user.name=rpi-virt-fw -c user.email=build@rpi-virt-fw am -q "${patches[@]}"
  )
else
  echo "== using QEMU sources in $src ($(git -C "$src" describe --always --tags 2>/dev/null))"
  echo "   (expected to carry qemu/patches already)"
fi

# --- 3. configure + build ---------------------------------------------------
mkdir -p "$build"
cd "$build"
if [ ! -f config-host.h ] || [ "${QEMU_RECONFIGURE:-0}" = 1 ]; then
  echo "== configuring"
  # shellcheck disable=SC2086
  PKG_CONFIG_PATH="$here/target/release${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" \
    "$src/configure" --target-list=aarch64-softmmu --disable-docs \
      --disable-user --disable-gtk --disable-sdl --disable-vnc \
      ${QEMU_CONFIGURE_EXTRA:-}
fi
if ! grep -qE '^#define CONFIG_RVF( 1)?$' config-host.h; then
  echo "configure did not find rpi-virt-fw.pc — CONFIG_RVF is off, the machine would have no videocore= option" >&2
  exit 1
fi
echo "== building (-j$jobs)"
ninja -j"$jobs" qemu-system-aarch64
echo "== $build/qemu-system-aarch64"
"$build/qemu-system-aarch64" -machine raspi4b,help 2>/dev/null | grep -E '^  videocore' || {
  echo "the built QEMU has no videocore machine property" >&2; exit 1; }
