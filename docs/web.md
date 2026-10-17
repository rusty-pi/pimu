# pimu in the browser

The library builds for `wasm32-unknown-unknown`, and `web/` is a page that boots
a Raspberry Pi 4 in a tab: the real `start4.elf` on the VPU, then the Linux kernel
on the four Cortex-A72 cores. It is the same deterministic, lock-stepped run as
`pimu boot`, so it is just as slow: a few minutes on a desktop CPU to the kernel's last
message (see [the README](../README.md)).

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version "$(scripts/build-wasm.sh --version)" --locked
scripts/build-wasm.sh
python3 -m http.server -d web
```

Open <http://localhost:8000/> and press Boot. The page downloads the firmware, the
device tree, the kernel and the EEPROM image from GitHub when it is pressed
(about 13 MB, none of it in this repository), and `web/worker.js` runs the
machine in a Web Worker, `Pi.run(100)` — 100 ms of wall time — at a time, so the page
keeps answering keys in between.

## What differs from the command line

- **1 GiB board.** The board is a Raspberry Pi 4B `a03114`. The default 8 GiB one
  does not fit a 32-bit address space, and a browser tab will not grant much
  more than 1 GiB in any case.
- **No host files, processes or sockets.** The boot partition is built in memory
  from the bytes the page hands over (`PiBuilder.add_file`). `--net passt`, the
  `xz` and `curl` helpers and a terminal on stdin are not part of the browser
  build: the stream backend is `#[cfg(unix)]`, and serial input arrives through
  `HostInput::channel` instead of a reader thread.
- **Time.** `Instant` comes from `web-time`, which reads `performance.now()`.
- **Logs.** `PiBuilder.log("arm-exc,mbox")` sends those `--log` channels to the
  browser console.

## Limits

Only the boot-partition path is wired up: no disk image, USB, network or reset
loop (a firmware self-update reset ends the run with `Reset`). The kernel has no
root filesystem here, so the boot ends in the same panic as the first example in
the README.
