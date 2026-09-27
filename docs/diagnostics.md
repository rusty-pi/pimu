# Diagnostics

Every wall in this repo was found with one of these: the `--log` channels, which
say what a subsystem did, and the `PIMU_*` environment variables, which trace,
trap and profile. They are reconnaissance tools, not configuration: none of
them changes what the firmware sees.

That is the rule they are held to. **A knob that changes what the firmware
observes is a shim**, needs an issue and a deletion plan, and several were
deleted for exactly that reason (`PIMU_PMIC_EVENT`, `PIMU_TICK_CALL`,
`PIMU_DEFER_SLOT3`, `PIMU_TICK_SLOT`, `PIMU_TICK_CORE1`, `PIMU_PMIC_HACK`,
`PIMU_PROBE`, and before them `PIMU_MBOX_KICK`, `PIMU_GPIOMAN_SHIM`,
`PIMU_SCHED_TICK`, `PIMU_MCSYNC_RPC`). A knob that only changes what *we* print is
a diagnostic and belongs here.

The one exception, which describes the *board* rather than the firmware, is
`PIMU_PCIE_DEVICE`.

**Build with the `diag` feature to use most of them:**

```bash
cargo build --release --features diag
```

The run-loop switches — tracing (`PIMU_TRACE_*`, `PIMU_MMIO_FROM`, `--trace*`),
`PIMU_TRAP*`, `PIMU_PROF*`, `PIMU_HEARTBEAT`, `PIMU_WATCH`, `PIMU_TCB`, and the
log channels of the run loop, the VPU core and the DMA window — are checked on
every instruction, so a normal build (CI's included) compiles them out; set on
such a build they are reported and ignored. [`building.md`](building.md) says
what else that build changes. `PIMU_LIVE_CONSOLE` and the device log channels
work everywhere.

---

## The recipe that works

Most investigations follow the same shape: find where the boot stops, arm a
trace just before it, then watch the state it depends on.

```bash
# 1. Where did it stop?  The run report's `end` and `final pc` say.
boot firmware/pieeprom.bin --eeprom --sd firmware/sd.img

# 1b. What did it branch through on the way, and what did it poke that
#     nothing models?  Neither is in the report by default.
boot … --control-transfers --stub-log

# 2. Arm the instruction trace when the boot first reaches that pc.
PIMU_TRACE_ON_PC=0x3ec568f8 PIMU_TRACE_CAP=4000 boot … 2> trace.log

# 3. Trap the call sites you suspect, with registers.
PIMU_TRAP=0x3ecc5190,0x3ec568f8 boot … 2> traps.log

# 4. Watch the memory the firmware is branching on.
PIMU_WATCH=0x3ef6b04c boot … 2> writes.log

# 5. Ask the devices it talks to what they saw.
boot … --log pcie,xhci 2> devices.log
```

`PIMU_TRACE_ON_PC` exists because the console-substring trigger cannot reach code
that runs *after* the firmware stops printing — which is where a wedged boot
usually is. It is what cracked the `arm_loader` OTP gate.

Write logs to a file and grep afterwards. Piping a long run through `head` kills
it with `SIGPIPE`.

---

## Tracing

| Variable | Effect |
|---|---|
| `PIMU_TRACE_ON_PC=<hex>` | Arm the instruction trace when core 0 first reaches this address. |
| `PIMU_TRACE_ON_CONSOLE=<text>` | Arm it when this substring appears on the console. Useless after the firmware goes quiet — use `PIMU_TRACE_ON_PC`. |
| `PIMU_TRACE_CAP=<n>` | Stop tracing after `n` instructions (default 300000). |
| `PIMU_TRACE_CF=1` | Trace only control flow — branches and calls, not every instruction. |
| `PIMU_TRACE_MMIO=1` | Log every MMIO access with the PC that made it. |
| `PIMU_TRACE_MMIO=<lo>-<hi>` | The same, restricted to an address range. This is what made enumerating `0x7D5D_0000` practical. |
| `PIMU_MMIO_FROM=<hex>` | Start the MMIO trace when core 0 reaches this address. |

## Traps and watchpoints

| Variable | Effect |
|---|---|
| `PIMU_TRAP=<hex>[,<hex>…]` | Print registers every time core 0 reaches one of these addresses. |
| `PIMU_TRAP_FROM=<n>` | Ignore traps until `n` instructions have retired. |
| `PIMU_TRAP_MAX=<n>` | Stop printing after `n` hits. |
| `PIMU_WATCH=<hex>[,<hex>…]` | Log every store to these word-aligned addresses, tagged with the PC. The way to find who fills a structure. |
| `PIMU_TCB=<hex>[,<hex>…]` | At exit, decode these ThreadX thread control blocks: where each thread is parked and what it is waiting on, with a rough backtrace. |

## Profiling

| Variable | Effect |
|---|---|
| `PIMU_PROF=1` | Bucket the core-0 PC into 256-byte slots and dump the hottest on exit. Finds the loop a stalled boot is spinning in. |
| `PIMU_PROF_THREAD=<hex>` | The same, attributed per ThreadX thread. Takes the address of the firmware's current-thread pointer (`_tx_thread_current_ptr`) — only the firmware knows where that lives, so it is a parameter rather than a constant baked into the model. |
| `PIMU_ARM_PROF=<us>` | From model time `<us>` on (`1` for the whole run), count every ARM step by core, EL and 256-byte PC bucket, and list the hottest in the run report — and at every reset, for the boot that ended. Asleep cores are not stepped, so they do not show; the passes a parked core skips count at the loop's PCs. |
| `PIMU_ARM_BLOCKS=1` | Count the straight-line runs the ARM cores execute — the instructions from one control-flow transfer's destination to the next — and how often each is re-entered, keyed by physical PC and EL. The report gives the mean run length, the share of executed instructions in runs of at least *n* instructions and in runs entered at least *n* times, and the hottest runs. This is what says whether translating a block at a time could pay. Run it with `PIMU_NO_PARK=1 PIMU_NO_SHA_SKIP=1`: a parked core's skipped passes and a natively hashed SHA-256 block are never stepped, so otherwise the counts miss the hottest loops. Takes the cores off the burst path, so it is slower than a plain run. |
| `PIMU_HEARTBEAT=<n>` | Print progress every `n` instructions, for runs that look hung. |

## Log channels

`boot --log [text:|jsonl:]<channel>[,<channel>...]` turns channels on. It can
be repeated,
and the lines go to stderr unless `--log-file <path>` says otherwise. The
channels share one output, so the lines come out in the order things happened,
each stamped with the model time — the system timer's, in seconds, the way the
firmware and Linux stamp their own logs:

```text
   2.959261 pcie: inbound window Some((0, 200000000))
   4.950839 pcie: endpoint irq true: intx false msi status 0x1 mask 0xffffffff
   6.653907 cmp: #1 C0 <- 0x65aee3 now=6653907 delta=10000
```

`jsonl:` gives one JSON object a line instead, with the time in µs:
`{"us":2959261,"channel":"pcie","msg":"..."}`. `io` keeps its own fields there
(`dev`, `op`, `lba`, `blocks`, `files`, ...). A reset builds a new machine, so
the time starts from 0 again, as after a reboot.

```bash
boot … --log pcie,xhci 2> usb.log
boot … --log jsonl:io --log-file io.jsonl
boot … --quiet --log jsonl:io 2>&1 >/dev/null | jq .
```

`--quiet` keeps the
serial console off stdout, for a run whose log is the point; `--console-log`
still records it.

| Channel | What it prints |
|---|---|
| `io` | What crossed the peripherals apart from the console: SD card and USB stick block runs with the files they belong to, OTP rows read and programmed, and what the network peer did — see [what the machine read and wrote](#what-the-machine-read-and-wrote). |
| `arm-exc` | Every synchronous exception an ARM core takes (not `svc`), with the `ESR`/`FAR` its handler sees, and for an external abort the physical address nothing answered at. |
| `cmp` | Every system-timer compare arm. |
| `dwc2` | The DWC2 USB OTG controller (`0x7E98_0000`): every write, and every read that differs from the previous read of the same register, so a poll shows once. |
| `emmc` | The SD host controllers, EMMC2 and the legacy EMMC: every command and its response, every block read, every register access. |
| `expander` | FXL6408 GPIO expander register traffic. |
| `gpio` | The GPIO block: every pin whose function, level or termination changes, named the way the board wires it — `42 (STATUS_LED_G_CLK) input -> output, high` for the activity LED, `40 (PWM0_MISO) input -> ALT4` for a flash session. |
| `irqen` | Interrupt enables, decoded back into the `enable_irq_source(src, prio)` calls that wrote them. |
| `mbox` | Every word across the ARM↔VideoCore property mailbox, both directions, and the first tag of each property request Linux posts. |
| `otp` | Every OTP row the firmware reads, and what it got; every row it programs, before and after; commands the model does not know. |
| `pcie` | Every change of the VL805's interrupt as the root complex sees it: INTA, or the MSI block's status and mask. Also every write to the inbound window `RC_BAR2`, and every endpoint DMA access that falls outside it (and so reaches no memory). |
| `pmic` | DA9090 PMIC register traffic. |
| `spi` | SPI0 transactions against the EEPROM flash. |
| `uart` | The two console UARTs: every register write apart from the data the console prints, every move of the mini-UART's interrupt line, and which block GPIO 14/15 carry — `the serial header is on the mini-UART (GPIO 14/15 ALT5)` when the firmware hands the pins over to Linux's `ttyS0`. |
| `xhci` | xHCI rings, TRBs and port state. Both controllers: the VL805's, and the BCM2711's own on the USB-C port, whose lines carry an `otg` tag. |

These need a `diag` build:

| Channel | What it prints |
|---|---|
| `derail` | Execution derailing out of start4's code, into unmapped or zeroed memory. |
| `dma` | Every DMA control block executed, and every access to the legacy DMA controller window. |
| `ff` | Every jump of the system timer through a firmware busy-wait. |
| `irqtbl` | At exit, the firmware's per-source interrupt handler table next to its vector table. |
| `sleep` | `sleep` instructions and what woke the core. |
| `swirq` | Software-posted interrupts via CoreCtl. |
| `tick` | ThreadX tick delivery and skips, device interrupts vectored, and an `rti` that returns outside start4's code. |
| `vec` | Interrupt vectoring: slot, vector base, handler. |

Each channel used to be an `PIMU_DBG_<NAME>=1` variable (the eMMC one
`EMMC_DBG`), and the I/O log was `--io-log`. `boot` warns about a variable
that is still set and names the channel that replaced it.

---

## What the machine read and wrote

`--log io` writes what crossed the peripherals to stderr (or to `--log-file
<path>`), apart from the console: block runs on the SD card and the USB stick
with the files they belong to, the OTP rows the firmware read and programmed,
and what the network peer did. The file names come
from the bench reading the image's partition table and FAT itself, so the
firmware stays a black box:

```text
   0.000944 io: otp  read  row 28  = 0x1aa2bb31  serial number
   6.270753 io: sd   read  lba 0x0+2  (partition table)
   6.271470 io: sd   read  lba 0x800+2  p1:(boot sector)
   6.278862 io: sd   read  lba 0x1014+5  p1:/config.txt, p1:/start4.elf
   6.291327 io: sd   read  lba 0x101c+4489  p1:/start4.elf, p1:/fixup4.dat
   6.393364 io: sd   read  lba 0x21a8+9  p1:/fixup4.dat, p1:/bcm2711-rpi-4-b.dtb
```

An OTP line ends with what the row is for, after Raspberry Pi's
[OTP register list](https://github.com/raspberrypi/documentation/blob/ecd7a8129d4f2cb908d6cbd6ea5a994e0091285d/documentation/asciidoc/computers/raspberry-pi/otp-bits.adoc),
and with `(blank)` when none of its fuses are programmed. A row the firmware programs shows as
`io: otp  write row <n> = <new>  <meaning> (was <old>)`, and what the network
peer did as `io: net  dhcp: ...`.

Programming works the way start4 drives the OTP block, key sequence first, and a
fuse only ever goes from 0 to 1. To watch it, program
two words of customer OTP (rows 36 and 37) the way
`vcmailbox 0x00038021 16 16 0 2 ...` does on a Pi, and read them back, on the
halt-kernel card (`KERNEL=halt scripts/make-sd.sh firmware/sd-halt.img`):

```bash
boot --eeprom firmware/pieeprom.bin --sd firmware/sd-halt.img --log io \
  --mbox-property 0x00038021:16=0.2.0x11111111.0x22222222 \
  --mbox-property 0x00030021:16=0.2
```

The fuses a run programmed are kept across runs with `--otp` — see
[`running.md`](running.md).

## Output and fixtures

| Variable | Effect |
|---|---|
| `PIMU_LIVE_CONSOLE=0` | Buffer the UART console and print it with the run report, instead of streaming it as it is produced (the default). |
| `PIMU_BOOTARGS="<args>"` | More kernel arguments after the harness's own (`initcall_debug` to time every initcall, `nokaslr` for addresses that match `System.map`). |
| `PIMU_SLOW_LOOP=1` | Take every step through every check of the run loop, as a `diag` build does, instead of skipping the checks that cannot act (`Emulator::fast_steps`). A run must come out the same either way; this is how to check that it does. |
| `PIMU_NO_PARK=1` | Execute every pass of a busy-wait loop instead of parking the core in it (`arm/mod.rs`, "Busy-wait loops"). The same check for the ARM side: a run must come out the same either way. Works in every build. |
| `PIMU_NO_BURST=1` | Take a core that is the only one running through the whole cycle loop, one instruction at a time, instead of stepping it in bursts (`arm/mod.rs`, "Time and scheduling"). Another same-either-way check. Works in every build. |
| `PIMU_NO_STRAIGHT=1` | Step a burst one instruction at a time instead of running each straight-line stretch of it off one page (`arm/mod.rs`, "Straight-line runs"). Another same-either-way check. Works in every build. |
| `PIMU_NO_SHA_SKIP=1` | Run a SHA-256 block loop block by block instead of hashing the blocks in the middle of a slice natively (`arm/mod.rs`, "SHA-256 loops"). Same either way, cycle count included. Works in every build. |
| `PIMU_DUMP_FLASH=<path>` | Write the EEPROM flash image out after the run, including any self-update the firmware applied. |
| `PIMU_DUMP_RAM=<path>` | Write SDRAM out after every boot, as `<path>.<n>` for boot `n`, before a reset replaces it. A kernel that dies before its console comes up still has its log buffer in there. Works in every build. |
| `PIMU_PCIE_DEVICE=0` | Unsolder the VL805 from the modelled board. Describes the hardware, not the firmware: a real Pi 4B always has one, so it is attached by default. |

---

## What the devices hold when the run ends

The golden transcript says what the firmware printed and the retired counts say
how much it ran. Neither sees a value a driver wrote into a register and never
mentioned, and some of those are the whole point of the boot. `-v` prints them
under `--- device state ---`:

```
--- device state ---
  genet   MAC 02:00:5e:00:53:01  tx on  rx on  promisc on
  bt      chip 02:00:5e:00:53:02, as it came up
          device tree 02:00:5e:aa:f9:ab on /soc/serial@7e201000/bluetooth
  cyw43455 MAC 02:00:5e:00:57:01, from the card's nvram
```

A client that asks the firmware for the board's MAC address and does not check
the answer — or checks it, fails, and carries on — programs `00:00:00:00:00:00`
into the GENET and boots to exactly the same console bytes. `uefi.toml`
pins that line for the RPi4 UEFI firmware, which is a second, independent
client of the property interface.

The Bluetooth address is two values, and they are not the same one:

* what the modem answers `Read_BD_ADDR` with, which is the address the chip
  came up with until a host writes another one into it with
  `BCM_WRITE_BD_ADDR` (`btbcm_set_bdaddr`), when the line says `written by the
  host` instead;
* what the firmware derived and published as `local-bd-address` on the
  `brcm,bcm43438-bt` node it left enabled, and which node that was.

The property holds the address least significant octet first — the kernel reads
it straight into a `bdaddr_t` (`hci_dev_get_bd_addr_from_property()`) — so the
six bytes `ab f9 aa 5e 00 02` are `02:00:5e:aa:f9:ab`, and the report decodes
them rather than printing the raw property. A Pi 4B's tree carries one such
node under each UART and the firmware enables the one the `config.txt` overlays
leave the modem on, so the node is picked by its `status`: with
`dtoverlay=disable-bt` both are disabled and the line says so, rather than
reporting the all-zero address the node under the mini-UART carries.

`linux-bt.toml` pins both, because neither ever reaches the console.

The WiFi chip's address is a third one again, from a third place, and the line
says which of three it is:

* `as it came up` — `02:00:5e:00:53:03`, the address the model's chip has of
  its own, standing in for the unique one a real 43455 has fused into it;
* `from the card's nvram` — the `macaddr=` line of the nvram the driver
  downloaded into the chip, which is what a card carrying
  `brcmfmac43455-sdio.txt` gets;
* `written by the host` — an address the driver set with `cur_etheraddr`,
  which it does only for one the platform handed it.

`ip link` prints the address but not the source, and on a boot that never
loads `brcmfmac` it prints nothing at all — the chip still answers, and this
line still says what it would have answered. `linux-wifi.toml` pins it.

A device belongs in this section once it holds a value worth diffing between
two firmware versions.

---

## The firmware's own interfaces

`--mbox-property`, `--mbox-raw` and `--gencmd` ask the booted firmware a
question through the property mailbox or over VCHIQ, and the run report decodes
every property reply the firmware posts. [`mailbox.md`](mailbox.md) covers
those, and what each tag answers today.
