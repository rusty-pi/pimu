# Getting the patched device tree out

`arm_loader` patches the identity properties of `/chosen` — `rpi-serial64`,
`rpi-boardrev-ext`, `rpi-sdram-size-gbit` — into the device tree just before it
releases the ARM. They are what to compare across a firmware bump.

`boot -v` prints them on every run that gets that far:

```text
--- device tree handed to the ARM ---
  at 0x2eff1e00  totalsize 0xe1b5  version 17
  /chosen/rpi-serial64           "fa1e00231aa2bb31"
  ...
```

and `--dump-fdt <path>` writes the blob itself, so two firmware versions can be
compared byte for byte:

```bash
cargo run --release -- boot --eeprom firmware/pieeprom.bin --sd firmware/sd-halt.img \
  --max-wall 200 --dump-fdt old.dtb
# …bump firmware/, rebuild the SD image, run again into new.dtb…
diff <(fdtdump old.dtb) <(fdtdump new.dtb)
```

The blob is the one the armstub hands the ARM (its `dtb_ptr32` word; the
firmware's own `Device tree loaded to 0x… (size 0x…)` log line when the ARM was
never released) and its FDT header is validated before anything is written, so
no address is hard-coded and the flag keeps working across firmware versions,
the cut-down `start4cd.elf` included, which logs nothing after it starts. With a
kernel that parks the ARM nothing overwrites the tree afterwards, so reading it
out at the end of the run is safe.
