# Test data

Two kinds of scenario live here, plus everything they are checked against.

| Directory | What is in it |
|---|---|
| `scenarios/` | In-process scenarios: a hand-assembled VPU payload each, a millisecond each, run by `cargo test` and `rpi-virt-fw run`. |
| `golden/` | Their golden transcripts. |
| `boot/` | The real boots: one file per medium and per firmware variant, run by `rpi-virt-fw boot-check` and by CI. Minutes each, and they need firmware blobs that are never committed. |
| `boot/golden/` | Their golden transcripts and retired instruction counts. |
| `netboot/` | The test-only signing key HTTP boot verifies against ([README](netboot/README.md)). |
| `arm/` | Fixtures for the ARM tests. |

## The boot scenarios

Each file in `boot/` describes one run — which EEPROM image, which boot medium,
what wall budget, what to type into the console — and everything asserted about
it.

| Scenario | Boot | Card |
|---|---|---|
| `firmware.toml` | SD card, through to `arm_loader` | `sd-halt.img` |
| `usb-boot.toml` | USB mass storage on the VL805 (`BOOT_ORDER` 0x4), no SD card | `sd-halt.img` |
| `otg-boot.toml` | USB mass storage on the USB-C port (`BOOT_ORDER` 0x5), no SD card | `sd-halt.img` |
| `tftp-boot.toml` | Network boot over TFTP | `firmware/netboot/` |
| `http-boot.toml` | HTTP boot of a signed `boot.img` ramdisk | `firmware/netboot/` |
| `b0-stepping.toml` | SD card on a B0 Pi 4B rev 1.2 ([#77](https://github.com/valtzu/rpi-virt-fw/issues/77)) | `sd-halt.img` |
| `firmware-cd.toml` | SD card with the cut-down `start4cd.elf` ([#105](https://github.com/valtzu/rpi-virt-fw/issues/105)) | `sd-halt-start4cd.img` |
| `uefi.toml` | SD card, on into the RPi4 UEFI firmware and its boot menu | `sd-uefi.img` |
| `linux.toml` | SD card, on into Linux: a busybox shell, then a few commands typed into it | `sd.img` |
| `linux-bt.toml` | The stock-config boot, Bluetooth and WiFi enabled, so Linux's console is the mini-UART | `sd-wireless.img` |
| `linux-wifi.toml` | The same, with `brcmfmac` loaded by hand against the modelled CYW43455 | `sd-brcmfmac.img` |

CI runs every one of them, in parallel, on each push and PR to `main`.
[`docs/running.md`](../docs/running.md) has the `make-sd.sh` command for each
card; `boot-check <scenario> --plan` prints the flags a scenario boots with.

### What a boot is checked against

- the **golden transcript** in `boot/golden/`, the whole console diffed line by
  line. This is what catches output that *moved* or a value that shifted — a
  change no grep sees, because the line still matches somewhere. The firmware's
  own timestamps are stripped first: they are cycle-derived and reproduce
  exactly, but any change to what an instruction costs shifts all of them at
  once, which would bury the one line that did change.
- the **milestones**, substring assertions each carrying the reason it exists:
  the commit or issue that made it pass. This is what a raw diff cannot say —
  which invariant broke.
- the **retired counts** beside the golden (`boot/golden/<name>.retired.toml`):
  how many instructions each VPU and ARM core ran
  ([#85](https://github.com/valtzu/rpi-virt-fw/issues/85)). They reproduce
  exactly from one machine to the next, so a change that makes a boot run
  differently without printing anything different fails here instead of going
  unnoticed. A run the wall clock cut off (`end TimeLimit`) fails outright: its
  counts only say how fast the host was.

All three are checked against a single boot; the wall clock has little headroom,
so nothing here runs the firmware twice.

### Running one

```bash
cargo run --release -- boot-check testdata/boot/firmware.toml
cargo run --release -- boot-check testdata/boot/linux.toml
cargo run --release -- boot-check testdata/boot/firmware.toml --update          # re-record the golden and counts
cargo run --release -- boot-check testdata/boot/firmware.toml --max-wall 600    # slower, busier machine
cargo run --release -- boot-check testdata/boot/usb-boot.toml --from boot-usb-boot.log  # an earlier run's pair
```

The combined output goes to `boot-<scenario>.log` (`--output` names another
file) and the console to `<log>.console` beside it; the pair is what `--from`
checks again without booting. The name carries the scenario, so two checks
running at once do not overwrite each other's evidence. CI keeps neither: a
failed boot prints its log into the job log.

After an intentional change, `--update` and then read the golden and counts diff
in the commit: it is the change, spelled out. A change that only moves the
counts still needs the re-record, and the diff then documents that it did.

## The in-process scenarios

`scenarios/*.toml` run a hand-assembled VPU payload (`src/harness/payloads.rs`)
against a small machine and diff the console with `golden/`. `cargo test` runs
them through `tests/scenarios.rs`; `rpi-virt-fw run-all -v` runs them with their
transcripts.

```toml
name = "hello-vpu"
description = "Hand-assembled payload prints a line on the mini-UART, then swi."

[payload]
kind = "builtin"          # builtin | elf | raw
source = "hello"          # builtin name, or path for elf/raw
load_addr = 0x00010000

[machine]
ram_mb = 16
console = "mini-uart"     # pl011 | mini-uart | pins

[run]
max_steps = 10_000
unimpl = "fault"          # fault | skip  (skip = reconnaissance mode)

[golden]
path = "../golden/hello-vpu.txt"
```
