# Diagnostics

Every wall in this repo was found with one of these. They are environment
variables because they are reconnaissance tools, not configuration: none of them
changes what the firmware sees.

That is the rule they are held to. **A knob that changes what the firmware
observes is a shim**, needs an issue and a deletion plan, and several were
deleted for exactly that reason (`RVF_PMIC_EVENT`, `RVF_TICK_CALL`,
`RVF_DEFER_SLOT3`, `RVF_TICK_SLOT`, `RVF_TICK_CORE1`, `RVF_PMIC_HACK`,
`RVF_PROBE`, and before them `RVF_MBOX_KICK`, `RVF_GPIOMAN_SHIM`,
`RVF_SCHED_TICK`, `RVF_MCSYNC_RPC`). A knob that only changes what *we* print is
a diagnostic and belongs here.

The two exceptions, which describe the *board* rather than the firmware, are
`RVF_PCIE_DEVICE` and `RVF_BOOT_WALL`.

---

## The recipe that works

Most investigations follow the same shape: find where the boot stops, arm a
trace just before it, then watch the state it depends on.

```bash
# 1. Where did it stop?  The run report's `end` and `final pc` say.
recon firmware/pieeprom.bin --eeprom --sd firmware/sd.img

# 2. Arm the instruction trace when the boot first reaches that pc.
RVF_TRACE_ON_PC=0x3ec568f8 RVF_TRACE_CAP=4000 recon … 2> trace.log

# 3. Trap the call sites you suspect, with registers.
RVF_TRAP=0x3ecc5190,0x3ec568f8 recon … 2> traps.log

# 4. Watch the memory the firmware is branching on.
RVF_WATCH=0x3ef6b04c recon … 2> writes.log
```

`RVF_TRACE_ON_PC` exists because the console-substring trigger cannot reach code
that runs *after* the firmware stops printing — which is where a wedged boot
usually is. It is what cracked the `arm_loader` OTP gate.

Write logs to a file and grep afterwards. Piping a long run through `head` kills
it with `SIGPIPE`.

---

## Tracing

| Variable | Effect |
|---|---|
| `RVF_TRACE_ON_PC=<hex>` | Arm the instruction trace when core 0 first reaches this address. |
| `RVF_TRACE_ON_CONSOLE=<text>` | Arm it when this substring appears on the console. Useless after the firmware goes quiet — use `RVF_TRACE_ON_PC`. |
| `RVF_TRACE_CAP=<n>` | Stop tracing after `n` instructions (default 300000). |
| `RVF_TRACE_CF=1` | Trace only control flow — branches and calls, not every instruction. |
| `RVF_TRACE_MMIO=1` | Log every MMIO access with the PC that made it. |
| `RVF_TRACE_MMIO=<lo>-<hi>` | The same, restricted to an address range. This is what made enumerating `0x7D5D_0000` practical (#1). |
| `RVF_MMIO_FROM=<hex>` | Start the MMIO trace when core 0 reaches this address. |

## Traps and watchpoints

| Variable | Effect |
|---|---|
| `RVF_TRAP=<hex>[,<hex>…]` | Print registers every time core 0 reaches one of these addresses. |
| `RVF_TRAP_FROM=<n>` | Ignore traps until `n` instructions have retired. |
| `RVF_TRAP_MAX=<n>` | Stop printing after `n` hits. |
| `RVF_WATCH=<hex>[,<hex>…]` | Log every store to these word-aligned addresses, tagged with the PC. The way to find who fills a structure. |

## Profiling

| Variable | Effect |
|---|---|
| `RVF_PROF=1` | Bucket the core-0 PC into 256-byte slots and dump the hottest on exit. Finds the loop a stalled boot is spinning in. |
| `RVF_PROF_THREAD=<hex>` | The same, attributed per ThreadX thread. Takes the address of the firmware's current-thread pointer (`_tx_thread_current_ptr`) — only the firmware knows where that lives, so it is a parameter rather than a constant baked into the model. |
| `RVF_HEARTBEAT=<n>` | Print progress every `n` instructions, for runs that look hung. |

## Subsystem logs

All of these are `=1`.

| Variable | What it prints |
|---|---|
| `RVF_DBG_TICK` | ThreadX tick delivery and skips. |
| `RVF_DBG_VEC` | Interrupt vectoring: slot, vector base, handler. |
| `RVF_DBG_IRQEN` / `RVF_DBG_IRQTBL` | Interrupt enables; the firmware's interrupt table. |
| `RVF_DBG_SWIRQ` | Software-posted interrupts via CoreCtl. |
| `RVF_DBG_TCB` | ThreadX thread control blocks — who is suspended and on what. |
| `RVF_DBG_SLEEP` | `sleep` instructions and what woke the core. |
| `RVF_DBG_CMP` | Every system-timer compare arm. |
| `RVF_DBG_DMA` | Every DMA4 control block executed. |
| `RVF_DBG_DERAIL` | Execution derailing into unmapped or zeroed memory. |
| `RVF_DBG_SPI` | SPI0 transactions against the EEPROM flash. |
| `RVF_DBG_PMIC` | DA9090 PMIC register traffic. |
| `RVF_DBG_OTP` | Every OTP row the firmware reads, and what it got. |
| `RVF_DBG_XHCI` | xHCI rings, TRBs and port state. |
| `RVF_DBG_MBOX` | Every word across the ARM↔VideoCore property mailbox, both directions. |
| `RVF_DBG_PCIE` | Every change of the VL805's interrupt as the root complex sees it: INTA, or the MSI block's status and mask. Also every write to the inbound window `RC_BAR2`, and every endpoint DMA access that falls outside it (and so reaches no memory). |
| `RVF_DBG_ARM_EXC` | With `--arm`: every synchronous exception an ARM core takes (not `svc`), with the `ESR`/`FAR` its handler sees. |
| `RVF_BOOTARGS="<args>"` | With `--arm`: more kernel arguments after the harness's own (`initcall_debug` to time every initcall, `nokaslr` for addresses that match `System.map`). |

## Output and fixtures

| Variable | Effect |
|---|---|
| `RVF_LIVE_CONSOLE=1` | Stream the UART console as it is produced instead of buffering it. `scripts/boot-check.sh` sets this. |
| `RVF_DUMP_FLASH=<path>` | Write the EEPROM flash image out after the run, including any self-update the firmware applied. |
| `RVF_BOOT_WALL=<seconds>` | Overrides the boot scenario's `wall_secs` (default 330) for `scripts/boot-check.sh`. Raise it when other work is competing for the CPU — two concurrent boot runs will miss `arm_loader` on time. |
| `RVF_PCIE_DEVICE=0` | Unsolder the VL805 from the modelled board. Describes the hardware, not the firmware: a real Pi 4B always has one, so it is attached by default. |

---

## Asking the firmware a question after it has booted

`start4.elf` does not stop at `arm_loader` — it leaves a `mbox_read` task
running and answers the property interface for the ARM it just released. This
bench has no ARM, so `--mbox-property` stands in for one:

```bash
recon firmware/pieeprom.bin --eeprom --sd firmware/sd.img \
  --mbox-property 0x00000001,0x00030090
```

It builds the request buffer, posts the doorbell, resumes the VPU until the
answer comes back, and decodes the reply tag by tag — including whether the
firmware marked each tag as handled at all.

The wake is worth understanding before debugging it, because it is four things
in series and any of them failing looks the same from outside:

1. the request word is queued on MAIL1 (`0x7E00_B9A0`),
2. bit 2 appears in MAIL1's pending word (`0x7E00_B94C`),
3. interrupt source 94 is raised and delivered, entering `0x3EC58302`,
4. bit 4 — the "MAIL1 has data" *interrupt-pending* flag — is set in the config
   word at `0x7E00_B9BC`, which is what makes the ISR release the receive lock
   `gp+243076` that the `mbox_read` task is parked on.

Step 4 is the one that is easy to get wrong: model the config word as only the
enables the firmware wrote and the ISR runs on every step, finds nothing to do,
and the task never wakes. `RVF_TRACE_MMIO=0x7e00b880-0x7e00b9c0` shows that
failure directly — the ISR reading `0x7e00b9bc <- 0x00000001` and writing the
same value straight back, forever.

Each tag's value buffer is sized the way `rpifwcrypto.c` sizes it, since that
is the Linux-side client of the same interface. A size can be overridden per
tag with `<tag>:<bytes>`, and the flag can be repeated to make **several**
exchanges against one booted firmware — which is the only way to read
`GET_CRYPTO_LAST_ERROR`, because it reports the error left behind by the
*previous* request:

```bash
recon firmware/pieeprom.bin --eeprom --sd firmware/sd.img \
  --mbox-property 0x00030090 --mbox-property 0x0003008e
```

When the buffer-level code is not `0x80000000`, the reply is also dumped as raw
words. That matters because the tag-by-tag decode walks by the sizes it staged,
so it is exactly what cannot be trusted when the sizes are in question.

What the firmware answers today:

| Tag | Answer |
|---|---|
| `0x00000001` `GET_FIRMWARE_REVISION` | `0x6a7a16af` — the build timestamp of the pinned `start4.elf`. |
| `0x0003008f` `GET_CRYPTO_NUM_OTP_KEYS` | `0x00000001` — one key slot. The crypto service is up. |
| `0x0003008e` `GET_CRYPTO_LAST_ERROR` | `0` after a tag that worked, `3` `RPI_FW_CRYPTO_KEY_NOT_FOUND` after one that did not. Reports the *previous* request, so it needs its own exchange. |
| `0x00030090` `GET_CRYPTO_KEY_STATUS` | Depends on the `key_id` in the request — see below. Ask with `0x00030090=1`. |
| `0x0003009c` `GET_CRYPTO_KEY_USAGE` | `0` for key 1, `RPI_FW_CRYPTO_KEY_USAGE_UNDEFINED`. |
| `0x00030092` `GET_CRYPTO_HMAC_SHA256` | status `0`, length `0x20`, and a real HMAC. Ask with `0x00030092=<flags>.<key_id>.<len>.<message words>`. |
| `0x00030095` `GET_CRYPTO_GEN_ECDSA_KEY` | `0x80000000` for `key_id` 0, for the same reason. |

A failing crypto handler is fatal for the whole request: the tag itself is
marked answered, but the buffer-level code becomes `0x80000001` and the walk
stops, so every tag *after* it goes unanswered for that reason alone. Put the
crypto tag last, or in its own exchange, before reading anything into an
unanswered tag. Five copies of `0x00000001` in one buffer all answer and the
code stays `0x80000000`, so there is no tag-count or buffer-size limit behind
this.

A crypto tag that answers `0x80000000` with `LAST_ERROR` 3 is usually being
asked for `key_id` **0**. Key ids are **1-based**: `NUM_OTP_KEYS` of 1 means the
part has one key and it is key *1*. Ask with `0x00030090=1` and it answers
`0x00000001` (`TYPE_DEVICE_PRIVATE_KEY`) with `LAST_ERROR` 0; 0, 2 and 3 all
give `KEY_NOT_FOUND`. The reference board behaves identically when asked the
same way over `/dev/vcio` — which is worth knowing before reading a
`KEY_NOT_FOUND` as an unprovisioned part, because it looks exactly like one.

With the right key id the whole service runs: `GET_CRYPTO_HMAC_SHA256` returns
status 0, length `0x20`, and a real HMAC computed by `start4.elf`'s own mbedTLS
from the OTP key. The scenario pins that digest.

That board's key rows are fused, which `vcgencmd otp_dump` will not show you —
it prints rows 56-63 as `00000000`, hiding them the way it hides rows 19-26,
while Linux's `nvmem_priv0` reads back 32 non-zero bytes. Checking blankness
through `nvmem_priv0`/`nvmem_cust0` is the reliable way, and reporting it as a
boolean is the *only* way that respects the "never commit an OTP dump" rule in
`CLAUDE.md`. `src/periph/configotp.rs` models rows 56-63 as fused for the same
reason: firmware that reads 0 from a row concludes the fuse is unprogrammed.

The `nvmem_cust_rw`, `nvmem_mac_rw` and `nvmem_priv_rw` `dtparam`s open those
regions for *write* from Linux. OTP writes are one-way, so nothing here needs
them — read access is enough to check what is fused.
