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

`firmware.sha256` records the exact bytes fetched.
