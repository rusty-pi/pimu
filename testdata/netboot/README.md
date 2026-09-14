# Network-boot test key

`test-signing-key.pem` is an RSA-2048 key made for this repository's tests and
nothing else. It is public on purpose: it protects nothing and must never be
used to sign anything a real board runs.

HTTP boot only accepts a signed `boot.img`. `scripts/make-netboot.sh` signs the
test image with this key (the `rsa2048:` line of `boot.sig`, as
`rpi-eeprom-digest -k` writes it), and writes the public half in the format
`rpi-eeprom-config --pubkey` puts in the EEPROM's `pubkey.bin` slot.
`boot --eeprom-pubkey` (scenario key `eeprom_pubkey`) installs it in the
model's copy of the EEPROM, so the bootloader verifies the image against it —
the same steps as Raspberry Pi's signed-boot flow, minus the OTP.

Committing a fixed key keeps the boot deterministic: the bootloader prints the
signature it verifies, so a key generated per run would change the golden
transcript every time.
