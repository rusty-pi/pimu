# Getting the patched device tree out

`arm_loader` patches `/chosen` — `rpi-machine-id`, `rpi-serial64`,
`rpi-boardrev-ext`, `rpi-sdram-size-gbit` — into the device tree just before it
releases the ARM. Those are the values `rpi-mkosi`
[#37](https://github.com/valtzu/rpi-mkosi/issues/37) needs to compare across a
firmware bump, because `rpi-machine-id` feeds the root LUKS passphrase.

`boot -v` prints them on every run that gets that far:

```text
--- device tree handed to the ARM ---
  at 0x2eff1e00  totalsize 0xe1b5  version 17
  /chosen/rpi-serial64           "fa1e00231aa2bb31"
  /chosen/rpi-machine-id         "ed96a9bc626d9d0869ce37ee4aea025d"
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

## Where `rpi-machine-id` comes from

`arm_loader` does not compute it itself. `0x3ECC5190` first looks for the `BVER`
block in the handoff table the EEPROM bootloader left behind and, if it is
there, hex-encodes the 16 bytes at `BVER+0x8c`; only with no such block does it
fall back to hashing OTP itself — `SHA-256(otp[28] ‖ otp[35] ‖ otp[30])`
truncated to 16 bytes, the three rows being the serial low word, the serial high
word and the revision code. A real boot always has the block, and so does this
bench (traced), so the value is `pieeprom.bin`'s and `start4.elf` only publishes
it at `0x3EC568F8`.

The identity behind it is this bench's own, not any real board's: every OTP row
involved is invented in `src/periph/configotp.rs`. See the OTP rule in
[`../CLAUDE.md`](../CLAUDE.md) for why a real board's must never be committed.
