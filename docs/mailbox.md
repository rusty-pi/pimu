# The firmware's own interfaces

`start4.elf` does not stop at `arm_loader`: it answers the property mailbox and
the `GCMD` service over VCHIQ for the ARM it just released. This page is how to
ask it something, and what it answers today. The reconnaissance tools that find
a wall are in [`diagnostics.md`](diagnostics.md).

## Asking a property

The firmware leaves a `mbox_read` task running and answers the property
interface. A kernel that parks the ARM never asks, so `--mbox-property` stands
in for it:

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
`linux.yaml` does this for `NOTIFY_XHCI_RESET`. The report prints before
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

## Replaying somebody else's request

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
| `0x00030048` `NOTIFY_REBOOT` | Answered, with no value. It sets a flag, and on a 4B rev 1.5 clears bit 0 of PMIC `0x1B` register `0x05` (a read-modify-write). It does not touch the GPIO expander. |
| `0x00038041` `SET_GPIO_STATE` | For `SD_PWR_ON` (GPIO 134) <- 0 after a `NOTIFY_REBOOT`, start4 power-cycles the card: pin low, 2 ms, `BT_ON` and `WL_ON` low, `SIO_1V8_SEL` low, 5 ms, `SD_PWR_ON` high again, all before it answers. A Raspberry Pi 4B d03115 does the same: `GET_GPIO_STATE` 134 reads 1 after the request. |
| `0x00030064` `GET_REBOOT_FLAGS` | `0`. |
| `0x00030066` | Not handled. |
| `0x00050001` `GET_COMMAND_LINE` | The last 256 bytes of `/chosen/bootargs`, without the terminator. |
| `0x00060001` `GET_DMA_CHANNELS` | `0x37f5`. |

## The display tags, and what `--display` changes

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
`CLAUDE.md`. `src/periph/configotp.rs` leaves those rows blank, because it models
a board as it leaves the factory and only an owner running `rpi-otp-private-key`
fuses them; the scenario provisions a key of its own with `otp_row`, because
firmware that reads 0 from a row concludes the fuse is unprogrammed.

The `nvmem_cust_rw`, `nvmem_mac_rw` and `nvmem_priv_rw` `dtparam`s open those
regions for *write* from Linux. OTP writes are one-way, so nothing here needs
them — read access is enough to check what is fused.

## `vcgencmd` over VCHIQ

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
