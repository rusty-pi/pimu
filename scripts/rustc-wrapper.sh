#!/bin/sh
# Cargo runs every rustc through this (.cargo/config.toml). Outside CI it keeps
# the compiler on the first 80% of the CPUs, so a build leaves the rest free
# and the desktop stays responsive. RVF_BUILD_CPU_PERCENT changes the share
# (100 turns the limit off); under CI the build gets every CPU.
if [ -z "${CI:-}" ] && command -v taskset >/dev/null 2>&1; then
  total=$(nproc)
  share=$((total * ${RVF_BUILD_CPU_PERCENT:-80} / 100))
  [ "$share" -ge 1 ] || share=1
  # Set on this shell and inherited through exec, so a failed taskset costs
  # the limit, never the build.
  if [ "$share" -lt "$total" ]; then
    taskset -cp "0-$((share - 1))" $$ >/dev/null 2>&1 || :
  fi
fi
exec "$@"
