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
| `0x004` | [`BLOCK_SIZE_COUNT`](#block_size_count) | rw | 32 | 2, best high |
| `0x008` | [`ARGUMENT`](#argument) | rw | 32 | 3, best high |
| `0x00C` | [`CMD_XFER`](#cmd_xfer) | rw | 32 | 2, best high |
| `0x010` | [`RESPONSE0`](#response0) | r | 32 | 1, best high |
| `0x014` | [`RESPONSE1`](#response1) | r | 32 | 1, best high |
| `0x018` | [`RESPONSE2`](#response2) | r | 32 | 1, best high |
| `0x01C` | [`RESPONSE3`](#response3) | r | 32 | 1, best high |
| `0x020` | [`BUFFER_DATA`](#buffer_data) | rw | 32 | 2, best high |
| `0x024` | [`PRESENT_STATE`](#present_state) | r | 32 | 2, best high |
| `0x028` | [`HOST_CONTROL`](#host_control) | rw | 32 | 1, best high |
| `0x02C` | [`CLOCK_CONTROL`](#clock_control) | rw | 32 | 3, best high |
| `0x030` | [`INT_STATUS`](#int_status) | w1c | 32 | 2, best high |
| `0x034` | [`INT_STATUS_EN`](#int_status_en) | rw | 32 | 3, best high |
| `0x038` | [`INT_SIGNAL_EN`](#int_signal_en) | rw | 32 | 2, best high |
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

Block size, SDMA buffer boundary and block count. Neither stock stage uses the count: before a card register read they write the register's length alone (8 for the SCR, `0x40` for a SWITCH_FUNC status). start4 writes `0x200` back after each such read; the bootloader instead writes `0x200` three times over before every `CMD18`.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 11:0 | `BLOCK_SIZE` | rw | Transfer block size in bytes. |
| 14:12 | `SDMA_BOUNDARY` | rw | SDMA buffer boundary: `4 KiB << n`. |
| 31:16 | `BLOCK_COUNT` | rw | Blocks to transfer. |

Sources:

- standard (high): SDHCI 3.00, 2.2.2 / 2.2.3
- trace (high): start4 writes it at `0x3EC5270A`: 8, `0x200`, `0x40`, `0x200`, `0x40`, `0x200` around ACMD51 and the two CMD6 reads; the bootloader writes 8 and `0x40` once each, then `0x200` three times before each of its `CMD18`s

`BLOCK_SIZE` sources:

- standard (high): SDHCI 3.00, 2.2.2

`SDMA_BOUNDARY` sources:

- standard (high): SDHCI 3.00, 2.2.2

`BLOCK_COUNT` sources:

- standard (high): SDHCI 3.00, 2.2.3

## `ARGUMENT`

Offset `0x008` · access `rw` · 32 bits

Command argument. The two stock stages identify a card differently. The bootloader sends ACMD41 with `0x00100000` (3.2-3.3 V), plus bit 30 once CMD8 has been answered, then after CMD2, CMD3, CMD9 and CMD7 reads the SCR (ACMD51, with the card's RCA as argument) and asks SWITCH_FUNC in check mode for high speed only (`0x00000001`); it goes to 50 MHz and sends ACMD6 (`2`) without switching the card. start4 sends CMD8 with `0x155`, ACMD41 with `0x40200000`, then CMD2, CMD3, CMD9, CMD7, CMD13, ACMD42 (`0`), ACMD6 (`2`), ACMD51 (`0`), CMD6 `0x00FFFFF1` (check), CMD6 `0x80FFFFF1` (switch) and CMD16 (`0x200`).

Sources:

- standard (high): SDHCI 3.00, 2.2.4
- trace (high): `boot --log emmc` of the pinned EEPROM and card: the command and argument sequence of each stage; start4 writes arguments at `0x3EC52358`
- decompile (high): bootmain `0xACD74..0xACDAA`: ACMD41 argument `0x100000`, or-ed with `1 << 30` unless the `SDV1` flag is set; `0xACD32`: CMD1 argument `0x40100000` for eMMC

## `CMD_XFER`

Offset `0x00C` · access `rw` · 32 bits

Transfer mode (low half) and command (high half). Writing the high half issues the command. The bootloader reads every run of sectors, one or many, with an open-ended `CMD18` (`0x123A0030`) and ends it with CMD12 as R1b (`0x0C1B0030`, the multi-block read mode kept). start4 reads a single sector with `CMD17` (`0x113A0010`) and a longer run with `CMD18`, ended by CMD12 with mode `0x10` (`0x0C1B0010`) and followed by CMD13. Neither sets `BLOCK_COUNT_EN` or `AUTO_CMD`.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `DMA` | rw | Transfer by DMA. |
| 1 | `BLOCK_COUNT_EN` | rw | `BLOCK_COUNT` limits the transfer. |
| 3:2 | `AUTO_CMD` | rw | 1 = auto CMD12 after the data, 2 = auto CMD23 before it. |
| 4 | `READ` | rw | Card to host. |
| 5 | `MULTI` | rw | Multiple blocks. |

Sources:

- standard (high): SDHCI 3.00, 2.2.5 / 2.2.6
- trace (high): start4 writes commands at `0x3EC52394`: 7 × `0x113A0010`, 16 × `0x123A0030`, 16 × `0x0C1B0010`, 17 × `0x0D1A0010` up to its first `config.txt` read; the bootloader's are all `0x123A0030` / `0x0C1B0030`

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
| 0 | `CMD_INHIBIT` | r | A command is in progress. The model completes commands at once, so it never shows this bit. |
| 1 | `DAT_INHIBIT` | r | A data transfer is in progress: set from the data command until its transfer completes, or until a reset of the data side. A driver ending an open-ended read must not wait for it before CMD12. Before CMD0, CMD12 and CMD13, start4 waits only for `CMD_INHIBIT`; before any other command, for both bits, up to 4 s. The bootloader does not wait on either. The model sets this bit while a PIO or DMA transfer has data left, including the read-ahead of an open-ended `CMD18`. |
| 10 | `BUF_WRITE_EN` | r | The buffer has room for a PIO write. |
| 11 | `BUF_READ_EN` | r | The buffer holds data for a PIO read. It is a level, not a latch: it drops after the last word of a block and comes back once the next block has arrived. Both stock stages read `PRESENT_STATE` before every `BUFFER_DATA` read and read the word only while this bit is set. `BUF_READ_RDY` in `INT_STATUS` latches again as each block arrives, but stays set until the driver clears it, so a driver that never clears it cannot pace the later blocks of a `CMD18` by it. The model drops this bit after every block of a read except the last. The next block arrives on the second read of `PRESENT_STATE` or `INT_STATUS` after that, or 21 µs later if nothing polls (a 512-byte block on a 4-bit bus at the 50 MHz both stock stages clock the card at). A `BUFFER_DATA` read before then returns 0 and uses up no data, and `--log emmc` reports it as a guest bug. The first block of a transfer is there as soon as the command completes. |
| 23:20 | `DAT_LINES` | r | DAT[3:0] line levels. The card holds them low during the CMD11 1.8 V switch. |
| 24 | `CMD_LINE` | r | CMD line level. |

Sources:

- standard (high): SDHCI 3.00, 2.2.9
- measured (high): `/dev/mem` read of `0xfe340024` on a Pi 4B rev 1.5 with Linux idle; start4 prints `status: 0x1fff0000` in `examples-on-real-hardware/sd-card-boot.log`

`CMD_INHIBIT` sources:

- standard (high): SDHCI 3.00, 2.2.9

`DAT_INHIBIT` sources:

- standard (high): SDHCI 3.00, 2.2.9 (and 3.7.1, which leaves the check out for an abort command)
- decompile (high): start4 command routine `0x3EC52084`: the busy-wait `0x3EC52BAE` gets mask 1 for codes 1024 (CMD0), 13 and 2060 (CMD12) and mask 3 otherwise, with a 4 000 000 us timeout; before CMD0 it waits up to 2000 us for mask 1, then resets `SRST_CMD` if no error is pending (`0x3EC520CA`)

`BUF_WRITE_EN` sources:

- standard (high): SDHCI 3.00, 2.2.9

`BUF_READ_EN` sources:

- standard (high): SDHCI 3.00, 2.2.9 and 2.2.17
- trace (high): bootloader: `PRESENT_STATE` at `0x000815DA`, then `BUFFER_DATA` at `0x000815C8`, per word; start4: `0x3EC51F5E`, then `0x3ED6A71C`
- trace (high): `boot --log emmc` on the firmware boot: after each block both stock stages read `PRESENT_STATE` as `0x1fff0002` once, then `0x1fff0802`, and read on; no `BUFFER_DATA` read falls in a gap

`DAT_LINES` sources:

- standard (high): SDHCI 3.00, 2.2.9

`CMD_LINE` sources:

- standard (high): SDHCI 3.00, 2.2.9

## `HOST_CONTROL`

Offset `0x028` · access `rw` · 32 bits

Host control 1, power, block-gap and wakeup control.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 1 | `DATA_WIDTH` | rw | 4-bit bus. start4 sets it right after ACMD6; the bootloader after its 50 MHz clock change. |
| 2 | `HIGH_SPEED` | rw | High-speed timing. Both stock stages set it in the same write sequence as their 50 MHz clock: `CLOCK_CONTROL` <- 0, this bit, then the new divider. start4 does it after reading the MBR, with the card already switched; the bootloader after only asking the card. |
| 4:3 | `DMA_SELECT` | rw | 0 = SDMA, 2 = 32-bit ADMA2. |
| 8 | `BUS_POWER` | rw | SD bus power. Turning it off takes VDD from the card. |
| 16 | `STOP_AT_GAP` | rw | Stop at block gap request. start4 writes host control before every command: with this bit set before the CMD12 that ends a `CMD18`, clear before any other. Not modelled: the model ends the transfer on CMD12 either way. |
| 23 | `FIXED` | r | Reads 1 whatever is written. |

Sources:

- standard (high): SDHCI 3.00, 2.2.10..2.2.13

`DATA_WIDTH` sources:

- standard (high): SDHCI 3.00, 2.2.10
- trace (high): start4 writes `0x00800F02` at `0x3EC5275A` after ACMD6

`HIGH_SPEED` sources:

- standard (high): SDHCI 3.00, 2.2.10
- trace (high): start4 writes `0x00800F06` at `0x3EC527F6` between `CLOCK_CONTROL` <- 0 (`0x3EC527A0`) and <- `0x000E0201` (`0x3EC52814`); the bootloader prints `CTL0: 0x00800f04` with its 50 MHz `SD HOST` line

`DMA_SELECT` sources:

- standard (high): SDHCI 3.00, 2.2.10

`BUS_POWER` sources:

- standard (high): SDHCI 3.00, 2.2.11

`STOP_AT_GAP` sources:

- standard (high): SDHCI 3.00, 2.2.12
- trace (high): start4 writes `0x00810F06` at `0x3EC521AA` before each of its 16 CMD12s, and host control without the bit at `0x3EC521C4` before its other commands

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
| 26 | `SRST_DATA` | rw | Reset the data circuit. Both stock stages set it after the CMD12 that ends an open-ended read, polling until it clears, rather than waiting for the stop's busy end: the bootloader right after issuing CMD12 and again once it completes, start4 once it completes. |

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
- trace (high): start4 writes `0x040E0207` at `0x3EC52026` once per CMD12 (16 up to its first `config.txt` read); the bootloader writes it twice per CMD12, before and after it reads `INT_STATUS`

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
| 5 | `BUF_READ_RDY` | w1c | Buffer read ready. Latches when a PIO read block arrives in the buffer, every block of a multi-block read included, and stays set until cleared. edk2's `ArasanMmcHostDxe` clears it before it reads each block and then waits for it again, polling this register only; the model counts those polls toward the next block's arrival as it does `PRESENT_STATE` reads. |
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
- standard (high): edk2-platforms `Platform/RaspberryPi/Drivers/ArasanMmcHostDxe/ArasanMmcHostDxe.c` `MMCReadBlockData`: `MMCHS_INT_STAT` polled for `BRR`, cleared, then 512 bytes from `MMCHS_DATA`, per block

`ERROR` sources:

- standard (high): SDHCI 3.00, 2.2.17

`ERR_CMD_TIMEOUT` sources:

- standard (high): SDHCI 3.00, 2.2.18

`ERR_ADMA` sources:

- standard (high): SDHCI 3.00, 2.2.18

## `INT_STATUS_EN`

Offset `0x034` · access `rw` · 32 bits

Which interrupt conditions latch into `INT_STATUS`. The EEPROM bootloader writes `0x007E0037`, which leaves `ERR_CMD_TIMEOUT` out: with no card nothing latches, and it gives each command 100 ms before it gives up. Its probe is CMD0, CMD8 (`0x1AA`); on no answer it logs `EMMC` and tries CMD1 (`0x40100000`), resets the host (`SRST_ALL`, about 200 ms before the clock is set again, one `SD HOST` line) and tries CMD0 and CMD1 again, then once more CMD0 and CMD8, logs `SDV1` and sends CMD55, whose timeout ends the open (`SD CMD: 0x371a0010 (55) 0x0 0x1fff0000`, `Failed to open device`). Each reset is logged `SD retry N oc M`, 10 ms before it. After a failed open it resets `SRST_CMD` and `SRST_DAT`, then `SRST_ALL`, and with `SD_BOOT_MAX_RETRIES=1` opens once more about 300 ms later.

Sources:

- standard (high): SDHCI 3.00, 2.2.19 / 2.2.20
- trace (high): `boot` of the pinned EEPROM with no `--sd`, `--log emmc`: `W [0x34] <- 0x007e0037`, then CMD0, CMD8 and 14478 reads of `INT_STATUS` returning 0 over 100 ms; the command order and the host resets as described, and `R [0x0c]`, `R [0x08]`, `R [0x24]` for the `SD CMD` line
- decompile (medium): bootmain probe state machine `0xACB74` (`EMMC` / `SDV1` flags at `+64` / `+68`) and its retry `0xAC640` (`SD retry %d oc %d`, a 10 000 us wait, then the host reset op) — _event-driven code; the order above is what the trace shows_

## `INT_SIGNAL_EN`

Offset `0x038` · access `rw` · 32 bits

Which `INT_STATUS` bits drive the interrupt line (INTID 158). start4 writes it before every command: 1 (`CMD_COMPLETE`) for a command with a data phase or an R1b response, 0 for any other. The bootloader does not touch it per command.

Sources:

- standard (high): SDHCI 3.00, 2.2.21 / 2.2.22
- trace (high): start4 writes it at `0x3EC52386`: 43 × 1 (CMD7, CMD12, CMD17, CMD18, ACMD51, both CMD6) and 32 × 0 up to its first `config.txt` read

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
