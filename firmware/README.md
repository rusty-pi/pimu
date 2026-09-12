# firmware/

Boot blobs live here but are **not committed** — run:

```
./scripts/fetch-firmware.sh
```

It downloads, at pinned versions:

| file                 | from                          | role |
|----------------------|-------------------------------|------|
| `pieeprom.bin`       | `raspberrypi/rpi-eeprom`      | SPI-flash bootloader image (runs on the VPU; brings up SDRAM, picks boot media) |
| `recovery.bin`       | `raspberrypi/rpi-eeprom`      | EEPROM recovery/flasher (VPU) |
| `start4.elf`         | `raspberrypi/firmware`        | Pi 4 GPU firmware (VPU); loads the ARM kernel |
| `fixup4.dat`         | `raspberrypi/firmware`        | SDRAM split patch data applied to `start4.elf` |
| `vl805-*.bin`        | `raspberrypi/rpi-eeprom`      | VL805 USB controller firmware (flashed by the bootloader) |
| `busybox-aarch64`    | Debian `busybox-static` (arm64) | the userland `scripts/make-sd.sh` puts on the card's ext4 root partition; the `.deb` is checked against a pinned sha256 |
| `arm64-userland/`    | Raspberry Pi `raspi-utils` + Debian trixie (arm64) | `rpi-fw-crypto` and the shared libraries it loads, unpacked from pinned `.deb`s kept in `debs/`; `make-sd.sh` copies the binary and exactly those libraries onto the root partition |

`firmware.sha256` records the exact bytes fetched.
