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

Open <http://localhost:8000/>, plug in the power (the plug under the board, or
**Power on**).

## The page

The board is a picture whose parts take what is plugged into them. Click one:

| Part | What it takes |
|---|---|
| microSD | The official firmware files from GitHub, the files of any GitHub directory, a folder from your computer, or a disk image (local file or URL). `config.txt` lines and the kernel command line. |
| EEPROM | The bootloader image (official, a URL, a file), the boot order as a list of steps, every bootloader setting as a form with drop-downs where the value is one of a few, signed-boot options. |
| USB 3.0 | A stick: a disk image, local or by URL, and its size. |
| USB-C | The power, and a stick on the OTG port, optionally behind a dock. |
| Ethernet | The built-in network peer (DHCP, DNS, TFTP, HTTP) with files to serve, and a Wake-on-LAN packet. |
| HDMI 0 | A monitor, with its EDID. |
| GPIO | A HAT's ID EEPROM. |
| BCM2711 | The silicon (B0 or C0), the memory, a board revision code, fuses to program, jitter and faults, and the checks of the `coherency` and `alignment` logs. |

The cores in the SoC light up as they start, sleep and run. The **Log** tab
records the channels of `pimu boot --log`, as `jsonl`, and filters them per
channel and by text; **Inspect** shows the device tree the firmware handed to the
ARM, the bootloader configuration, the fuses, and downloads the flash as the
firmware left it. Whatever is downloaded shows its progress under the board.

Parts are locked while the board is powered: the machine is built at power-on.

## Disk images are never loaded whole

An image is read as the guest asks for blocks, in 1 MiB pieces, like a `--sd
<url>` of the command line: a local file through `FileReaderSync`, a URL through
ranged requests (so the server has to answer `Range` and allow it cross-origin,
and tell the length). What the guest writes stays in memory. `xz`-compressed
images have to be decompressed first.

## How it is built

- Everything that runs WebAssembly runs in a Web Worker (`web/worker.js`), a
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

## Not done

HTTP- and TFTP-served directories from a URL, `--gencmd` and `--mbox-property`,
and an orderly reboot are command-line only. There is no picture: the HDMI output
is not rendered.
