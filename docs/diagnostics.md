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
| `RVF_PROF_THREAD=1` | The same, attributed per ThreadX thread. |
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
| `RVF_DBG_RESUME` / `RVF_DBG_MAINSUS` | Thread resumes; every suspend of the main boot thread. |
| `RVF_DBG_EVGET` / `RVF_DBG_EVSET` / `RVF_DBG_FF` | ThreadX event-flags gets, sets, and the flag words. |
| `RVF_DBG_SLEEP` | `sleep` instructions and what woke the core. |
| `RVF_DBG_CMP` | Every system-timer compare arm. |
| `RVF_DBG_DMA` | Every DMA4 control block executed. |
| `RVF_DBG_DERAIL` | Execution derailing into unmapped or zeroed memory. |
| `RVF_DBG_SPI` | SPI0 transactions against the EEPROM flash. |
| `RVF_DBG_PMIC` | DA9090 PMIC register traffic. |
| `RVF_DBG_OTP` | Every OTP row the firmware reads, and what it got. |
| `RVF_DBG_XHCI` | xHCI rings, TRBs and port state. |
| `RVF_CZ_LOG=<n>` | confzilla / dt-blob schema matching, at verbosity `n`. |

## Output and fixtures

| Variable | Effect |
|---|---|
| `RVF_LIVE_CONSOLE=1` | Stream the UART console as it is produced instead of buffering it. `scripts/boot-check.sh` sets this. |
| `RVF_DUMP_FLASH=<path>` | Write the EEPROM flash image out after the run, including any self-update the firmware applied. |
| `RVF_BOOT_WALL=<seconds>` | Overrides the boot scenario's `wall_secs` (default 330) for `scripts/boot-check.sh`. Raise it when other work is competing for the CPU — two concurrent boot runs will miss `arm_loader` on time. |
| `RVF_PCIE_DEVICE=0` | Unsolder the VL805 from the modelled board. Describes the hardware, not the firmware: a real Pi 4B always has one, so it is attached by default. |
