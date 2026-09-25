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
`PIMU_TRAP*`, `PIMU_PROF*`, `PIMU_HEARTBEAT`, `PIMU_WATCH`, `PIMU_TCB`, and the log
channels of the run loop, the VPU core and the DMA window — are checked on
every instruction, so a normal build (CI's included) compiles them out. Set on
such a build the variables are reported and ignored, and `--trace*` and those
channels are refused. A `diag` build also takes every step through every check
of the run loop, instead of skipping the ones that cannot act
(`Emulator::fast_steps`), so the switches see each instruction. It also records
start4's boot-progress tags (stores to `0x?EC0_2000`), which `boot` prints
after the run.
`PIMU_LIVE_CONSOLE` is not a diagnostic and works everywhere, as do the device
log channels, which only fire on rare device events.

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

## Asking the firmware a question after it has booted

`start4.elf` does not stop at `arm_loader` — it leaves a `mbox_read` task
running and answers the property interface for the ARM it just released. A
kernel that parks the ARM never asks, so `--mbox-property` stands in for it:

```bash
boot firmware/pieeprom.bin --eeprom --sd firmware/sd-halt.img \
  --mbox-property 0x00000001,0x00030090
```

It builds the request buffer, posts the doorbell, resumes the VPU until the
answer comes back, and decodes the reply tag by tag — including whether the
firmware marked each tag as handled at all.

The model also decodes every property reply as the firmware posts it, whoever
asked, and the run report lists the results under `--- property replies ---`.
For each tag it shows how often the firmware set its handled mark and how often
it did not, and the first word of the value buffer as the latest reply left it.
Unmarked does not always mean ignored: `SET_GPIO_STATE` and `SET_GPIO_CONFIG`
come back without the mark but with their status, `0`, in the value, which is
what Linux's `gpio-raspberrypi-exp` checks. In a Linux boot the section covers
every request Linux makes, so a value Linux never checks can still be pinned:
`linux.toml` does this for `NOTIFY_XHCI_RESET`. The report prints before
an `--mbox-property` exchange runs, so for those requests read the exchange's
own decode.

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
and the task never wakes. `PIMU_TRACE_MMIO=0x7e00b880-0x7e00b9c0` shows that
failure directly — the ISR reading `0x7e00b9bc <- 0x00000001` and writing the
same value straight back, forever.

Each tag's value buffer is sized the way `rpifwcrypto.c` sizes it, since that
is the Linux-side client of the same interface. A size can be overridden per
tag with `<tag>:<bytes>`, and the flag can be repeated to make **several**
exchanges against one booted firmware — which is the only way to read
`GET_CRYPTO_LAST_ERROR`, because it reports the error left behind by the
*previous* request:

```bash
boot firmware/pieeprom.bin --eeprom --sd firmware/sd-halt.img \
  --mbox-property 0x00030090 --mbox-property 0x0003008e
```

When the buffer-level code is not `0x80000000`, the reply is also dumped as raw
words. That matters because the tag-by-tag decode walks by the sizes it staged,
so it is exactly what cannot be trusted when the sizes are in question. Every
rejected reply is logged the same way on the `mbox` channel, whoever asked:

```
boot firmware/pieeprom.bin --eeprom --sd firmware/sd-uefi.img --log mbox
  mbox: property reply error at 0xf8a76000: 00000022 80000001 00010003 0000000a ...
```

which is the declared total, the response code, the tag, its value-buffer size
and what the firmware's walk ran into — enough to see a client's layout is
wrong without a second run. The report's per-tag lines carry an `errors` count
for the same reason: a tag that is always in a rejected buffer is where to look.

### Replaying somebody else's request

`--mbox-property` lays a request out the way a well-behaved client does: every
value buffer word-aligned, an end marker, slack past it. A client whose
`sizeof` is wrong does not, and the difference decides whether the firmware
takes the request at all — an unaligned declared total, an end tag at an odd
offset, or stale bytes past the total the client only zeroed up to. `--mbox-raw`
posts an exact byte image instead, so such a buffer can be replayed:

```bash
boot firmware/pieeprom.bin --eeprom --sd firmware/sd-halt.img \
  --mbox-raw 2200000000000000030001000a000000000000000000000000000000000000000000
```

A `GET_BOARD_MAC_ADDRESS` request of a real UEFI build, declaring 34 bytes with
a 10-byte value buffer: the firmware fills the MAC, then walks on to offset 32,
past the end tag at 30, and answers `0x80000001` if anything there is not zero.

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
| `0x00010001` `GET_BOARD_MODEL` | `0`. |
| `0x00010002` `GET_BOARD_REVISION` | The revision code with the memory the ARM got, which on the default board is the `d03115` its fuses say. |
| `0x00010003` `GET_BOARD_MAC_ADDRESS` | Six bytes: OTP row 65 most significant byte first, then the top two bytes of row 64. |
| `0x00010004` `GET_BOARD_SERIAL` | The serial (OTP row 28), then `0x10000000`. |
| `0x00010005` `GET_ARM_MEMORY` | `0`, `0x3b400000` with `fixup4.dat` on the card; `0`, `0x08000000` without it, when `arm_loader` also says 128MB. |
| `0x00010006` `GET_VC_MEMORY` | `0x3b400000`, `0x04c00000` with `fixup4.dat`. |
| `0x00020001` `GET_POWER_STATE` | `1` for device 0, the SD card; `0` for devices 3 and 9. Ask with `0x00020001:8=0`. |
| `0x00020002` `GET_TIMING` | `1` for device 0. |
| `0x00030003` `GET_VOLTAGE` | Microvolts. Id 1 is the core voltage the firmware asked for (`1036000`, where `pmic_core` holds setpoint `0x68`); ids 2 to 4 are `1100000`; id 0 answers `0x80000000`. Ask with `0x00030003:8=1`. |
| `0x00030005` `GET_MAX_VOLTAGE` | Id 1: `970000`, the calibrated core voltage without the ARM's share; id 2: `1100000`. |
| `0x00030008` `GET_MIN_VOLTAGE` | Id 1: `880000`; id 2: `1100000`. |
| `0x00030006` `GET_TEMPERATURE` | Millidegrees, whatever the id: `43779` from the model's count of 752. The scaled count is divided rather than shifted, so it rounds towards zero. |
| `0x0003000a` `GET_MAX_TEMPERATURE` | `85000`. |
| `0x0003000b` `GET_STC` | `0`, then the system timer's low word. |
| `0x00030021` `GET_CUSTOMER_OTP` | The start and the count, then that many rows from row 36. A start of 8 or more comes back as `0x80000000`. Ask with `0x00030021:16=0.2`. |
| `0x00030047` `GET_CLOCK_RATE_MEASURED` | `0` for clocks 3 and 4. |
| `0x00030048` `NOTIFY_REBOOT` | Answered, with no value. |
| `0x00030064` `GET_REBOOT_FLAGS` | `0`. |
| `0x00030066` | Not handled. |
| `0x00050001` `GET_COMMAND_LINE` | The last 256 bytes of `/chosen/bootargs`, without the terminator. |
| `0x00060001` `GET_DMA_CHANNELS` | `0x37f5`. |

### The display tags, and what `--display` changes

Every display tag answers zero on a headless boot, which is the default and what
the reference board does. `boot --display` puts a monitor on HDMI0, and
then the firmware has a display to describe:

| tag | headless | `--display` |
|---|---|---|
| `0x00040013` `GET_NUM_DISPLAYS` | `0` | `1` |
| `0x00040003` `GET_PHYSICAL_WH` | `0`, `0` | `0x280`, `0x1e0` (640 x 480) |
| `0x00040004` `GET_VIRTUAL_WH` | `0`, `0` | `0x280`, `0x1e0` |
| `0x00040005` `GET_DEPTH` | `0` | `0x20` |
| `0x00040008` `GET_PITCH` | `0` | `0xa00` (2560 = 640 x 4) |
| `0x00030020` `GET_EDID_BLOCK` | fails | the attached blob, block 0 |

The geometry comes from the EDID's detailed timing, so `--display-edid` with a
blob of another mode moves all of it.

`0x00040001` `ALLOCATE_BUFFER` needs the whole sequence in front of it and an
8-byte tag buffer; asked on its own it answers a size of 0 and looks broken:

```
boot --display --mbox-property \
  '0x00048003:8=640.480,0x00048004:8=640.480,0x00048005:4=32,0x00048006:4=1,\
   0x00048007:4=1,0x00048009:8=0.0,0x00040001:8=4096,0x00040008:4=0'
```

answers `0xfeabc000 0x0012c000` — a framebuffer at that bus address, `640 x 480 x
4` bytes of it. Without `--display` the same sequence answers a size of 0, and so
does a real headless board: `vcmailbox 0x00040001 8 4 16` on a
Raspberry Pi 4B d03115 fails with `ioctl_set_msg failed:-1`, so the refusal is
the firmware's and not the model's.

What this does **not** reach is the HDMI state-machine clock, so nothing writes
`USBR +0x2C`. The encoder is being programmed — 96 accesses to the
`hdmi0` core window against 5 headless, the `phy` range at `0x7EF00F00` among
them — so it is a near miss rather than an untouched path. Tried without effect:
a card with no KMS overlay and no `disable_fw_kms_setup`, `hdmi_force_hotplug` /
`hdmi_group` / `hdmi_mode` / `max_framebuffers`, and a 256-byte EDID whose CEA
extension carries an HDMI VSDB so the sink is HDMI rather than DVI.

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

### `vcgencmd` over VCHIQ

The property mailbox is not the only channel the firmware answers on. `vcgencmd`
talks to the `GCMD` service over **VCHIQ**, a shared slot area in coherent DRAM
with a doorbell either way, and none of it goes through `/dev/vcio`. `--gencmd`
stands in for the client:

```bash
boot out/start4.elf --sd firmware/sd-halt.img \
  --gencmd commands --gencmd measure_temp --gencmd get_throttled
```

Each flag is one command line, and all of them go over one connection:

```
--- VCHIQ gencmd (slot area at 0x10100000) ---
  master up: slots 2..32, tx_pos 0
  CONNECT acknowledged
  GCMD open on firmware port 1, peer version 1
  commands  ->  status 0, 114 bytes
      commands="commands, version, measure_temp, ..."
  measure_temp  ->  status 0, 16 bytes
      temp=43.7'C
  GCMD closed; doorbell 0 rung 6 times
```

The harness (`src/cli/vchiq.rs`) is a whole ARM side of VCHIQ, not a wrapper
around the kernel's: it lays out a slot area of its own, hands it over with
`VCHIQ_INIT` (`0x00048010`), and then sends `CONNECT`, `OPEN`, a `DATA` message
per command and `CLOSE`. It has to be, because the kernel's `bcm2835_vchiq`
never *connects* on its own — `vchiq_probe` hands over the slot area and stops,
and the thread that would send `CONNECT` is only created once something has
connected, which in a real system is a userspace client opening `/dev/vchiq`.

Run it with the ARM parked, for the reason `--mbox-property` gives: the
hand-over is a property request, and a live kernel's `bcm2835-mbox` takes the
reply to *our* buffer as the reply to whatever it had outstanding.

The doorbells are `specs/bell.toml`: four words at `0x7E00_B840` with the VPU's
view `0x100` above, bells 0 and 1 raising the ARM (`GIC_SPI 34` is doorbell 0,
which is VCHIQ's) and bells 2 and 3 the VPU. `doorbell 0 rung N times` in the
report above is the firmware's side of the wake actually working: a peer that
frames its answer but never rings is a `vcgencmd` that hangs with the answer
sitting in the slot.
