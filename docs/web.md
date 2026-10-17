# pimu in the browser

The library builds for `wasm32-unknown-unknown`, and `web/` is a page that boots
a Raspberry Pi 4 in a tab: the real `start4.elf` on the VPU, then the Linux kernel
on the four Cortex-A72 cores. It is the same deterministic, lock-stepped run as
`pimu boot`, so it is just as slow: a few minutes on a desktop CPU to the kernel's
last message (see [the README](../README.md)).

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version "$(scripts/build-wasm.sh --version)" --locked
scripts/build-wasm.sh
python3 -m http.server -d web
```

Every release is also deployed to GitHub Pages (`.github/workflows/pages.yml`),
at <https://rusty-pi.github.io/pimu/> once Pages is set to deploy from Actions.

Open <http://localhost:8000/>, plug in the power (the plug under the board, or
**Power on**).

## The page

The board is a block diagram in the style of [`board-sheet.svg`](board-sheet.svg): the SoC in the middle with its controllers on the edges, and the parts that plug into them around it. A wire is dashed while nothing is plugged in and solid once something is. Click a part:

| Part | What it takes |
|---|---|
| Power | The switch that powers the board on and off. |
| microSD | The official firmware files from GitHub, the files of any GitHub directory, a folder from your computer, a disk image (local file or URL), or no card at all. `config.txt` lines and the kernel command line. |
| EEPROM | The bootloader image (official, a URL, a file), the boot order as a list of steps, every bootloader setting as a form with drop-downs where the value is one of a few, signed-boot options. |
| USB 3.0 | A stick: a disk image, local or by URL, and its size. |
| USB-C | The power, and a stick on the OTG port, optionally behind a dock. |
| Ethernet | The built-in network peer (DHCP, DNS, TFTP, HTTP) as a boot server over TFTP, HTTP or both, each with its own files, and a Wake-on-LAN packet. |
| HDMI 0 | A monitor, with its EDID. |
| GPIO | A HAT's ID EEPROM. |
| BCM2711 | The silicon (B0 or C0), the memory, a board revision code, fuses to program, jitter and faults, and the checks of the `coherency` and `alignment` logs. |

The six cores in the SoC are tiles of one size that light up as they start, sleep and run, and the line under the board counts the VPU and ARM instructions retired. The **Log** tab
records the channels of `pimu boot --log`, as `jsonl`, and filters them per
channel and by text; **Inspect** shows the device tree the firmware handed to the
ARM, the bootloader configuration, the fuses, and downloads the flash as the
firmware left it. Whatever is downloaded shows its progress under the board.

Parts are locked while the board is powered: the machine is built at power-on.

## Network boot

The Ethernet panel boots the board from the built-in peer. TFTP and HTTP are
separate options, and each serves the official firmware, the files of a GitHub
directory, or a folder from your computer. Choose **No card** in the microSD
panel to boot from the network alone. Unless the boot order is set in the EEPROM
panel, the network comes first (TFTP, then HTTP), then the card and the USB
sticks, and the `config.txt` and `cmdline.txt` lines of the microSD panel are
applied to the served files as well.

- **TFTP** serves the files one by one, as `BOOT_ORDER` digit `2` fetches them.
- **HTTP** (digit `7`) only boots a signed image, so the page builds a FAT16
  `net_install/boot.img` from the files in memory (`src/bootimg.rs`), signs it
  with an RSA-2048 key that WebCrypto makes for that power-on (`web/signing.mjs`),
  serves it with its `boot.sig`, and puts the public key in the EEPROM's
  `pubkey.bin` slot. `HTTP_HOST=netboot.test` and `HTTP_PORT=80` are added to
  the bootloader settings unless they are set in the EEPROM panel. The key never
  leaves the page, and a public key picked for secure boot is not used while HTTP
  boot is on. The bootloader checks the image's SHA-256 in emulated VideoCore
  code, so it takes longer than TFTP: about a minute with the official firmware
  in Chrome, against twenty seconds for TFTP.

## Disk images are never loaded whole

An image is read as the guest asks for blocks, in 1 MiB pieces, like a `--sd
<url>` of the command line: a local file through `FileReaderSync`, a URL through
ranged requests (so the server has to answer `Range` and allow it cross-origin,
and tell the length). What the guest writes stays in memory. `xz`-compressed
images have to be decompressed first.

## How it is built

- Everything that runs WebAssembly runs in a Web Worker (`web/worker.mjs`), a
  slice of 100 ms of wall time at a time, so the page never waits for the guest.
  A fresh worker per power-on: a WebAssembly memory never shrinks.
- `src/web/session.rs` is the browser's `run_boot`, built on plain Rust that
  also builds and tests on the host; `src/web/bindings.rs` is the
  `wasm-bindgen` surface and exists for `wasm32` only. The edits to the EEPROM
  image and the card, the device-tree lookup and the verdict on a run are shared
  with the command line (`src/boot.rs`).
- Disk images come in through `periph::disk::ByteSource`.
- `Instant` comes from `web-time`, which reads `performance.now()`.

## Differences from the command line

- **At most 2 GiB.** The default 8 GiB board does not fit a 32-bit address space,
  and a tab will not grant much more than 1 GiB reliably, so the default is 1 GiB.
- **No host files, processes or sockets.** `--net passt`, the `xz` and `curl`
  helpers and a terminal on stdin are not part of the browser build.
- **No VPU ELF payload, `--maskrom`, `--patch` or scenarios.** A page boots an
  EEPROM image.
- **The log channels are chosen at power-on.** After a reset the model clock
  starts from 0 again, as in the command line; a `bench` line marks it.

## Tests

The page is tested in a browser with the emulation mocked, so the suite is quick
and needs neither the Rust toolchain nor the WebAssembly build:

```bash
cd web
npm ci
npx playwright install chromium     # or PW_CHANNEL=chrome to use an installed Chrome
npm test                            # the settings form's logic, then the page
node tests/coverage-report.mjs      # which functions of the page no test ran
```

- `tests/fixtures.mjs` replaces `Worker` with a stand-in that records what the page
  sends and lets a test say what the worker answers, so a test can make the board
  start, print, log, fault or answer for the inspect tab without running anything.
- `tests/worker.spec.mjs` runs the real `worker.mjs` against a stub of the
  WebAssembly module (`tests/stub-pimu.mjs`): downloads and their progress, local
  files, ranged disk images, errors, pause, keys, the inspect answers.
- `tests/layout.spec.mjs` holds the page still: opening a part, switching a tab,
  a download or a run moves nothing, on a wide screen and on a phone.
- CI (`.github/workflows/web-ui.yml`) fails when a function of the page's scripts
  is run by no test.

## Not done

HTTP- and TFTP-served directories from a URL, `--gencmd` and `--mbox-property`,
and an orderly reboot are command-line only. There is no picture: the HDMI output
is not rendered.
