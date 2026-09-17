# Agent instructions

- Keep changes focused on the requested firmware-modeling or workflow task.
- Read the relevant code and existing documentation before editing; preserve
  repository conventions and avoid unrelated cleanup.
- Run the smallest relevant tests or build checks before finishing.
- Do not commit secrets or generated firmware blobs.
- **Name real hardware by board type, never by hostname.** When a doc,
  comment, test, commit or issue cites a measurement or a test run on a real
  board, write the model and revision code — `Raspberry Pi 4B d03115`, not
  `rpi-dev`. A hostname only means something on one person's network; the
  revision code (`Revision` in `/proc/cpuinfo`) says which model, PCB revision
  and RAM size the value came from.
- **Use backticks for code in `specs/*.toml` text.** `summary`, `notes`, `ref`
  and `note` are copied as they are into the Markdown under `docs/periph/`, so
  mark register and field names, addresses, function names, log lines, commands
  and paths as Markdown inline code — not with single quotes, not as bare text:

  ```toml
  ref = "`dmesg` on a Raspberry Pi 4B d03115: `hcc params 0x002841eb hci version 0x100`"
  ```
- **A new or moved spec means editing `docs/board-sheet.svg`.** The board sheet
  is drawn by hand — no generator produces a schematic — so `tests/specs.rs`
  and `tests/board_sheet.rs` are what keep it honest: every block has to be
  named in a `data-block`, every `parent` drawn as a `data-edge`, and an
  address written on a part has to be the one its spec gives. When a spec
  gains a block, a `parent`, a copy or a new base, draw it and rerun
  `cargo test --test board_sheet`. Adding the name to an existing part's
  `data-block` list is fine for a block with nothing of its own to show.
- **Never let CI or anything in `scripts/` depend on real hardware.** The
  reference boards are ad-hoc and only sometimes reachable, and CI runs in the
  cloud. Measured values belong baked into the model with a source comment
  saying which board they came from — that is what `src/periph/avs.rs`,
  `pvt.rs` and `xhci.rs` do. Anything that has to *reach* a board at build or
  test time is a broken build waiting to happen. Benchmarking on a board is
  fine, but it is load-sensitive: check `/proc/loadavg` and skip rather than
  report a number taken under contention.
- **Never commit an OTP dump.** `vcgencmd otp_dump` on a real board
  includes device-unique and secret material — the board serial, the customer
  key hash, and the private key the `rpi-machine-id` / LUKS derivation depends
  on. Quoting one or two named rows whose value is genuinely needed as ground
  truth is fine when they are blank or non-secret (the codec licence rows 45/46
  are `00000000`, and `src/periph/vce.rs` cites them); dumping the table is not.
  The same goes for anything else read out of OTP or the crypto FIFO.
- Do not add `Co-authored-by:` trailers to commits.
- For boot-model changes, validate behavior with the available firmware
  scenarios and document any remaining shims or known limitations.
