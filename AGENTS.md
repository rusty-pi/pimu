# Agent instructions

- Keep changes focused on the requested firmware-modeling or workflow task.
- Read the relevant code and existing documentation before editing; preserve
  repository conventions and avoid unrelated cleanup.
- Run the smallest relevant tests or build checks before finishing.
- Do not commit secrets or generated firmware blobs.
- **Never commit an OTP dump.** `vcgencmd otp_dump` on the reference board
  includes device-unique and secret material — the board serial, the customer
  key hash, and the private key the `rpi-machine-id` / LUKS derivation depends
  on. Quoting one or two named rows whose value is genuinely needed as ground
  truth is fine when they are blank or non-secret (the codec licence rows 45/46
  are `00000000`, and `src/periph/vce.rs` cites them); dumping the table is not.
  The same goes for anything else read out of OTP or the crypto FIFO.
- Do not add `Co-authored-by:` trailers to commits.
- For boot-model changes, validate behavior with the available firmware
  scenarios and document any remaining shims or known limitations.
