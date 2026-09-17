<!-- generated from specs/emmc2.toml by `cargo run -- spec-docs --update` – do not edit -->

# `emmc2` – EMMC2: the SD host controller (Arasan SDHCI 3.00) the bootloader, start4 and Linux boot from

- Bus: `vpu` (VPU bus address)
- Base: `0x7E340000`
- Size: `0x1000`

Command engine wired to a modelled card, with PIO, SDMA and ADMA2 (32-bit) data paths. Every register is a 32-bit word here; narrow accesses pick their lane.

Sources:

- standard (high): SD Host Controller Simplified Specification 3.00, section 2 (register map)
- datasheet (high): BCM2835 ARM Peripherals, EMMC chapter: the same Arasan layout at the older block
- linux (high): `mmc@7e340000` driven by `sdhci-iproc`; `mmc0: SDHCI controller on fe340000.mmc using ADMA` on the reference board

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`SDMA_ADDR`](#sdma_addr) | rw | 32 | 1, best high |
| `0x004` | [`BLOCK_SIZE_COUNT`](#block_size_count) | rw | 32 | 1, best high |
| `0x008` | [`ARGUMENT`](#argument) | rw | 32 | 1, best high |
| `0x00C` | [`CMD_XFER`](#cmd_xfer) | rw | 32 | 1, best high |
| `0x010` | [`RESPONSE0`](#response0) | r | 32 | 1, best high |
| `0x014` | [`RESPONSE1`](#response1) | r | 32 | 1, best high |
| `0x018` | [`RESPONSE2`](#response2) | r | 32 | 1, best high |
| `0x01C` | [`RESPONSE3`](#response3) | r | 32 | 1, best high |
| `0x020` | [`BUFFER_DATA`](#buffer_data) | rw | 32 | 2, best high |
| `0x024` | [`PRESENT_STATE`](#present_state) | r | 32 | 2, best high |
| `0x028` | [`HOST_CONTROL`](#host_control) | rw | 32 | 1, best high |
| `0x02C` | [`CLOCK_CONTROL`](#clock_control) | rw | 32 | 3, best high |
| `0x030` | [`INT_STATUS`](#int_status) | w1c | 32 | 2, best high |
| `0x034` | [`INT_STATUS_EN`](#int_status_en) | rw | 32 | 1, best high |
| `0x038` | [`INT_SIGNAL_EN`](#int_signal_en) | rw | 32 | 1, best high |
| `0x03C` | [`HOST_CONTROL2`](#host_control2) | rw | 32 | 1, best high |
| `0x040` | [`CAPABILITIES_0`](#capabilities_0) | r | 32 | 1, best high |
| `0x044` | [`CAPABILITIES_1`](#capabilities_1) | r | 32 | 1, best high |
| `0x048` | [`MAX_CURRENT`](#max_current) | r | 32 | 1, best high |
| `0x054` | [`ADMA_ERROR`](#adma_error) | r | 32 | 1, best high |
| `0x058` | [`ADMA_ADDR`](#adma_addr) | rw | 32 | 1, best high |
| `0x0FC` | [`CONTROLLER_VERSION`](#controller_version) | r | 32 | 2, best high |
| `0x100` | [`REG_100`](#reg_100) | rw | 32 | 2, best high |
| `0x154` | [`REG_154`](#reg_154) | rw | 32 | 1, best high |

## `SDMA_ADDR`

Offset `0x000` · access `rw` · 32 bits

SDMA system address (or argument 2). Writing it resumes a paused SDMA.

Sources:

- standard (high): SDHCI 3.00, 2.2.1

## `BLOCK_SIZE_COUNT`

Offset `0x004` · access `rw` · 32 bits

Block size, SDMA buffer boundary and block count.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 11:0 | `BLOCK_SIZE` | rw | Transfer block size in bytes. |
| 14:12 | `SDMA_BOUNDARY` | rw | SDMA buffer boundary: `4 KiB << n`. |
| 31:16 | `BLOCK_COUNT` | rw | Blocks to transfer. |

Sources:

- standard (high): SDHCI 3.00, 2.2.2 / 2.2.3

`BLOCK_SIZE` sources:

- standard (high): SDHCI 3.00, 2.2.2

`SDMA_BOUNDARY` sources:

- standard (high): SDHCI 3.00, 2.2.2

`BLOCK_COUNT` sources:

- standard (high): SDHCI 3.00, 2.2.3

## `ARGUMENT`

Offset `0x008` · access `rw` · 32 bits

Command argument.

Sources:

- standard (high): SDHCI 3.00, 2.2.4

## `CMD_XFER`

Offset `0x00C` · access `rw` · 32 bits

Transfer mode (low half) and command (high half). Writing the high half issues the command.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `DMA` | rw | Transfer by DMA. |
| 1 | `BLOCK_COUNT_EN` | rw | `BLOCK_COUNT` limits the transfer. |
| 3:2 | `AUTO_CMD` | rw | 1 = auto CMD12 after the data, 2 = auto CMD23 before it. |
| 4 | `READ` | rw | Card to host. |
| 5 | `MULTI` | rw | Multiple blocks. |

Sources:

- standard (high): SDHCI 3.00, 2.2.5 / 2.2.6

`DMA` sources:

- standard (high): SDHCI 3.00, 2.2.5

`BLOCK_COUNT_EN` sources:

- standard (high): SDHCI 3.00, 2.2.5

`AUTO_CMD` sources:

- standard (high): SDHCI 3.00, 2.2.5

`READ` sources:

- standard (high): SDHCI 3.00, 2.2.5

`MULTI` sources:

- standard (high): SDHCI 3.00, 2.2.5

## `RESPONSE0`

Offset `0x010` · access `r` · 32 bits

Response bits 31:0 (39:8 of a long response).

Sources:

- standard (high): SDHCI 3.00, 2.2.7

## `RESPONSE1`

Offset `0x014` · access `r` · 32 bits

Response bits 63:32.

Sources:

- standard (high): SDHCI 3.00, 2.2.7

## `RESPONSE2`

Offset `0x018` · access `r` · 32 bits

Response bits 95:64.

Sources:

- standard (high): SDHCI 3.00, 2.2.7

## `RESPONSE3`

Offset `0x01C` · access `r` · 32 bits

Response bits 127:96.

Sources:

- standard (high): SDHCI 3.00, 2.2.7

## `BUFFER_DATA`

Offset `0x020` · access `rw` · 32 bits

PIO buffer data port, least significant byte first.

Sources:

- standard (high): SDHCI 3.00, 2.2.8
- decompile (high): the bootloader and start4 read `start4.elf` and the kernel through it (CMD17 / CMD18 + CMD12)

## `PRESENT_STATE`

Offset `0x024` · access `r` · 32 bits · reset `0x1FFF0000`

Idle value: card inserted and stable, card-detect and write-protect pins high (writable), CMD and DAT[7:0] high.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 10 | `BUF_WRITE_EN` | r | The buffer has room for a PIO write. |
| 11 | `BUF_READ_EN` | r | The buffer holds data for a PIO read. It is a level, not a latch: it drops after the last word of a block and comes back once the next block has arrived. Both stock stages read `PRESENT_STATE` before every `BUFFER_DATA` read and read the word only while this bit is set. `BUF_READ_RDY` in `INT_STATUS` stays latched from the first block on, so it cannot pace later blocks of a `CMD18`. The model fills the buffer at once, so a driver that paces on the latch works in the model but would read later blocks too early on silicon. |
| 23:20 | `DAT_LINES` | r | DAT[3:0] line levels. The card holds them low during the CMD11 1.8 V switch. |
| 24 | `CMD_LINE` | r | CMD line level. |

Sources:

- standard (high): SDHCI 3.00, 2.2.9
- measured (high): `/dev/mem` read of `0xfe340024` on a Pi 4B rev 1.5 with Linux idle; start4 prints `status: 0x1fff0000` in `examples-on-real-hardware/sd-card-boot.log`

`BUF_WRITE_EN` sources:

- standard (high): SDHCI 3.00, 2.2.9

`BUF_READ_EN` sources:

- standard (high): SDHCI 3.00, 2.2.9 and 2.2.17
- trace (high): bootloader: `PRESENT_STATE` at `0x000815DA`, then `BUFFER_DATA` at `0x000815C8`, per word; start4: `0x3EC51F5E`, then `0x3ED6A71C`

`DAT_LINES` sources:

- standard (high): SDHCI 3.00, 2.2.9

`CMD_LINE` sources:

- standard (high): SDHCI 3.00, 2.2.9

## `HOST_CONTROL`

Offset `0x028` · access `rw` · 32 bits

Host control 1, power, block-gap and wakeup control.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 4:3 | `DMA_SELECT` | rw | 0 = SDMA, 2 = 32-bit ADMA2. |
| 8 | `BUS_POWER` | rw | SD bus power. Turning it off takes VDD from the card. |
| 23 | `FIXED` | r | Reads 1 whatever is written. |

Sources:

- standard (high): SDHCI 3.00, 2.2.10..2.2.13

`DMA_SELECT` sources:

- standard (high): SDHCI 3.00, 2.2.10

`BUS_POWER` sources:

- standard (high): SDHCI 3.00, 2.2.11

`FIXED` sources:

- measured (high): `0x00800000` with Linux running; the real bootloader prints `CTL0: 0x00800f00` right after writing `0x00000f00` (`sd-card-boot.log`)

## `CLOCK_CONTROL`

Offset `0x02C` · access `rw` · 32 bits

Clock control (15:0), data timeout (19:16) and the self-clearing software resets (26:24). Before it releases the ARM, start4 leaves the host reset twice over: after its last read it sets `SRST_ALL` and then writes 0 (clock off); after clearing `INT_SIGNAL_EN` and `INT_STATUS_EN` and writing all ones to `INT_STATUS`, it sets `SRST_ALL`, then `SRST_CMD` and `SRST_DATA` together, polling each until it clears.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `INTERNAL_EN` | rw | Internal clock on. |
| 1 | `STABLE` | r | Internal clock stable; immediately, in the model. |
| 2 | `SD_EN` | rw | SD clock to the card on. |
| 24 | `SRST_ALL` | rw | Reset everything. |
| 25 | `SRST_CMD` | rw | Reset the command circuit. |
| 26 | `SRST_DATA` | rw | Reset the data circuit. |

Sources:

- standard (high): SDHCI 3.00, 2.2.14..2.2.16
- trace (high): pinned start4, host write helper `0x3EC52C2E` and read helper `0x3EC51F5E`: `0x010E0207`, 0 after the console handover; then `INT_SIGNAL_EN`, `INT_STATUS_EN` <- 0, `INT_STATUS` <- `0xFFFFFFFF`, `0x01000000`, `0x06000000` once the SD power pin lookup has failed
- measured (high): the real board prints `arasan_emmc_set_clock ... C1: 0x000e0047` (`sd-card-boot.log`)

`INTERNAL_EN` sources:

- standard (high): SDHCI 3.00, 2.2.14

`STABLE` sources:

- standard (high): SDHCI 3.00, 2.2.14

`SD_EN` sources:

- standard (high): SDHCI 3.00, 2.2.14

`SRST_ALL` sources:

- standard (high): SDHCI 3.00, 2.2.16

`SRST_CMD` sources:

- standard (high): SDHCI 3.00, 2.2.16

`SRST_DATA` sources:

- standard (high): SDHCI 3.00, 2.2.16

## `INT_STATUS`

Offset `0x030` · access `w1c` · 32 bits

Normal (15:0) and error (31:16) interrupt status, gated by `INT_STATUS_EN`. The bootloader writes `0x31` before each command, then `0x1` once a command without data completes, or `0x21` (`CMD_COMPLETE | BUF_READ_RDY`) once a data command does. start4 writes all-ones before each command, and `0x1` after an R1b command (`CMD7`) before it waits for `XFER_COMPLETE`.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `CMD_COMPLETE` | w1c | Command complete. |
| 1 | `XFER_COMPLETE` | w1c | Transfer complete. |
| 2 | `BLOCK_GAP` | w1c | Block gap event. |
| 3 | `DMA` | w1c | SDMA reached a buffer boundary, or an ADMA2 descriptor asked for an interrupt. |
| 4 | `BUF_WRITE_RDY` | w1c | Buffer write ready. |
| 5 | `BUF_READ_RDY` | w1c | Buffer read ready. |
| 15 | `ERROR` | r | Set while any error bit is. |
| 16 | `ERR_CMD_TIMEOUT` | w1c | Command timeout. |
| 25 | `ERR_ADMA` | w1c | ADMA error. |

Sources:

- standard (high): SDHCI 3.00, 2.2.17 / 2.2.18
- trace (high): bootloader register-write helper `0x00081DC2`: 39 × `0x31`, 25 × `0x1`, 14 × `0x21`; start4 `0x3EC52C2E`: 104 × `0xFFFFFFFF`, 4 × `0x1`

`CMD_COMPLETE` sources:

- standard (high): SDHCI 3.00, 2.2.17

`XFER_COMPLETE` sources:

- standard (high): SDHCI 3.00, 2.2.17

`BLOCK_GAP` sources:

- standard (high): SDHCI 3.00, 2.2.17

`DMA` sources:

- standard (high): SDHCI 3.00, 2.2.17

`BUF_WRITE_RDY` sources:

- standard (high): SDHCI 3.00, 2.2.17

`BUF_READ_RDY` sources:

- standard (high): SDHCI 3.00, 2.2.17

`ERROR` sources:

- standard (high): SDHCI 3.00, 2.2.17

`ERR_CMD_TIMEOUT` sources:

- standard (high): SDHCI 3.00, 2.2.18

`ERR_ADMA` sources:

- standard (high): SDHCI 3.00, 2.2.18

## `INT_STATUS_EN`

Offset `0x034` · access `rw` · 32 bits

Which interrupt conditions latch into `INT_STATUS`.

Sources:

- standard (high): SDHCI 3.00, 2.2.19 / 2.2.20

## `INT_SIGNAL_EN`

Offset `0x038` · access `rw` · 32 bits

Which `INT_STATUS` bits drive the interrupt line (INTID 158).

Sources:

- standard (high): SDHCI 3.00, 2.2.21 / 2.2.22

## `HOST_CONTROL2`

Offset `0x03C` · access `rw` · 32 bits

Auto CMD error status (15:0, read-only) and host control 2 (31:16).

| Bits | Field | Access | Notes |
|---|---|---|---|
| 15:0 | `AUTO_CMD_ERROR` | r | Auto CMD error status. |
| 19 | `SIGNAL_1V8` | rw | 1.8 V signalling. |
| 22 | `EXEC_TUNING` | rw | Execute tuning. |
| 23 | `TUNED_CLK` | rw | Sampling clock tuned. |

Sources:

- standard (high): SDHCI 3.00, 2.2.23 / 2.2.24

`AUTO_CMD_ERROR` sources:

- standard (high): SDHCI 3.00, 2.2.23

`SIGNAL_1V8` sources:

- standard (high): SDHCI 3.00, 2.2.24

`EXEC_TUNING` sources:

- standard (high): SDHCI 3.00, 2.2.24

`TUNED_CLK` sources:

- standard (high): SDHCI 3.00, 2.2.24

## `CAPABILITIES_0`

Offset `0x040` · access `r` · 32 bits · reset `0x45EE6432`

Timeout clock 50 kHz, base clock 100 MHz, 2048-byte blocks, 8-bit bus, ADMA2, high speed, SDMA, suspend/resume, 3.3 V and 1.8 V, no 64-bit system bus, embedded slot.

Sources:

- measured (high): `/dev/mem` read of `0xfe340040` on a Pi 4B rev 1.5, controller idle

## `CAPABILITIES_1`

Offset `0x044` · access `r` · 32 bits · reset `0xA525`

SDR50 and DDR50 (no SDR104), driver type C, re-tuning after 16 s in mode 3, SDR50 needs tuning.

Sources:

- measured (high): `/dev/mem` read of `0xfe340044` on a Pi 4B rev 1.5

## `MAX_CURRENT`

Offset `0x048` · access `r` · 32 bits · reset `0x80008`

32 mA at 3.3 V and at 1.8 V.

Sources:

- measured (high): `/dev/mem` read of `0xfe340048` on a Pi 4B rev 1.5

## `ADMA_ERROR`

Offset `0x054` · access `r` · 32 bits

ADMA error status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 1:0 | `STATE` | r | Engine state at the error: 1 = fetching a descriptor, 3 = transferring. |
| 2 | `LEN_MISMATCH` | r | Descriptor lengths and block count disagree. |

Sources:

- standard (high): SDHCI 3.00, 2.2.29

`STATE` sources:

- standard (high): SDHCI 3.00, 2.2.29

`LEN_MISMATCH` sources:

- standard (high): SDHCI 3.00, 2.2.29

## `ADMA_ADDR`

Offset `0x058` · access `rw` · 32 bits

ADMA2 descriptor table address.

Sources:

- standard (high): SDHCI 3.00, 2.2.30

## `CONTROLLER_VERSION`

Offset `0x0FC` · access `r` · 32 bits · reset `0x10020000`

Slot interrupt status (low half) and version: vendor `0x10`, SDHCI 3.00.

Sources:

- measured (high): `/dev/mem` read of `0xfe3400fc` on a Pi 4B rev 1.5
- standard (high): SDHCI 3.00, 2.2.32 / 2.2.33

## `REG_100`

Offset `0x100` · access `rw` · 32 bits

Past the SDHCI registers. start4 clears bit 30 and sets bit 31 in the board clock set-up it runs once `config.txt` is read, right after `REG_154`, and goes on reading the card afterwards. Those writes depend on board feature bits (the helper at `0x3EC64902` tests one bit of a feature word). Meaning unknown.

Sources:

- trace (high): pinned start4 reads it at `0x3ED4A1DE` and writes `0x80000000` at `0x3ED4A1E6`; just before: GPIO `+0xD0` <- 1, `REG_154` <- 1, and CM `EMMC2DIV` <- `0x7800`
- decompile (high): start4 `0x3ED4A1DE..0x3ED4A1E6`: `bitclear 30`, `bitset 31`; the path runs when feature bits `0x19` and `0x1C` are clear

## `REG_154`

Offset `0x154` · access `rw` · 32 bits

Past the SDHCI registers. start4 writes 1 here in its board clock set-up, between setting GPIO `+0xD0` bit 0 and `REG_100`. Meaning unknown.

Sources:

- trace (high): pinned start4 writes `0x00000001` at `0x3ED4A1D8`
