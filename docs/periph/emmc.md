<!-- generated from specs/emmc.toml by `cargo run -- spec-docs --update` – do not edit -->

# `emmc` – The legacy EMMC controller (Arasan SDHCI): the Pi 4's WiFi SDIO host, and the host 2020-era bootcode reads the SD card through

- Bus: `vpu` (VPU bus address)
- Base: `0x7E300000`
- Size: `0x100`

EMMC2's command engine (src/periph/emmc2.rs) with nothing on the bus: the software resets self-clear, the internal clock reports stable once enabled, CMD0 completes and every command that expects a response times out. 2020-era bootcode routes the SD slot here (GPIO block +0xD0, bit 1) and boots from SD through this host; the model does not follow that mux, so the card stays on EMMC2 and such bootcode reports "Failed to open device: 'sdcard'" and tries its next boot mode. The interrupt output is GIC SPI 126, the line EMMC2 drives too.

Sources:

- datasheet (high): BCM2835 ARM Peripherals, EMMC chapter: this controller and its register map, at the same bus address
- linux (high): bcm2711-rpi-4-b.dtb (raspberrypi/firmware): mmcnr@7e300000, compatible brcm,bcm2835-mmc / brcm,bcm2835-sdhci, reg <0x7e300000 0x100>, interrupts <GIC_SPI 0x7e>, with wifi@1 (brcm,bcm4329-fmac) on it — _The SD image's disable-wifi overlay turns the node off, so Linux never touches the block in the model's boots._
- trace (high): pieeprom-2020-09-03 bootcode, RVF_TRACE_MMIO=7e300000-7e341000: its whole SD init runs on this block and EMMC2 is never touched; it writes 0x2 to 0x7E2000D0 first (0x8000f60a). The 2026 bootcode and start4 never touch this block.
- measured (high): UART logs of the 2020 bootloaders, which print this host's HOST_CONTROL and PRESENT_STATE on every clock change: raspberrypi/rpi-eeprom#139, #227, #242 (no card in the slot), #282 (booting from the SD card) — _Other people's boards, not the reference board._

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`SDMA_ADDR`](#sdma_addr) | rw | 32 | 2, best high |
| `0x004` | [`BLOCK_SIZE_COUNT`](#block_size_count) | rw | 32 | 2, best high |
| `0x008` | [`ARGUMENT`](#argument) | rw | 32 | 2, best high |
| `0x00C` | [`CMD_XFER`](#cmd_xfer) | rw | 32 | 2, best high |
| `0x010` | [`RESPONSE0`](#response0) | r | 32 | 1, best high |
| `0x014` | [`RESPONSE1`](#response1) | r | 32 | 1, best high |
| `0x018` | [`RESPONSE2`](#response2) | r | 32 | 1, best high |
| `0x01C` | [`RESPONSE3`](#response3) | r | 32 | 1, best high |
| `0x020` | [`BUFFER_DATA`](#buffer_data) | rw | 32 | 2, best high |
| `0x024` | [`PRESENT_STATE`](#present_state) | r | 32 | 2, best high |
| `0x028` | [`HOST_CONTROL`](#host_control) | rw | 32 | 2, best high |
| `0x02C` | [`CLOCK_CONTROL`](#clock_control) | rw | 32 | 2, best high |
| `0x030` | [`INT_STATUS`](#int_status) | w1c | 32 | 1, best high |
| `0x034` | [`INT_STATUS_EN`](#int_status_en) | rw | 32 | 1, best high |
| `0x038` | [`INT_SIGNAL_EN`](#int_signal_en) | rw | 32 | 2, best high |
| `0x03C` | [`HOST_CONTROL2`](#host_control2) | rw | 32 | 1, best high |
| `0x040` | [`CAPABILITIES_0`](#capabilities_0) | r | 32 | 1, best high |
| `0x044` | [`CAPABILITIES_1`](#capabilities_1) | r | 32 | 1, best high |
| `0x048` | [`MAX_CURRENT`](#max_current) | r | 32 | 1, best high |
| `0x0FC` | [`CONTROLLER_VERSION`](#controller_version) | r | 32 | 2, best high |

## `SDMA_ADDR`

Offset `0x000` · access `rw` · 32 bits

ARG2 in the BCM2835 datasheet: the argument auto-CMD23 sends.

Sources:

- standard (high): SDHCI 3.00, 2.2.1
- datasheet (high): BCM2835 ARM Peripherals, EMMC: ARG2

## `BLOCK_SIZE_COUNT`

Offset `0x004` · access `rw` · 32 bits

BLKSIZECNT: block size and block count.

Sources:

- standard (high): SDHCI 3.00, 2.2.2 / 2.2.3
- datasheet (high): BCM2835 ARM Peripherals, EMMC: BLKSIZECNT

## `ARGUMENT`

Offset `0x008` · access `rw` · 32 bits

ARG1: the command argument.

Sources:

- standard (high): SDHCI 3.00, 2.2.4
- datasheet (high): BCM2835 ARM Peripherals, EMMC: ARG1

## `CMD_XFER`

Offset `0x00C` · access `rw` · 32 bits

CMDTM: transfer mode (low half) and command (high half). Writing the high half issues the command.

Sources:

- standard (high): SDHCI 3.00, 2.2.5 / 2.2.6
- datasheet (high): BCM2835 ARM Peripherals, EMMC: CMDTM

## `RESPONSE0`

Offset `0x010` · access `r` · 32 bits

RESP0: response bits 31:0 (39:8 of a long response).

Sources:

- standard (high): SDHCI 3.00, 2.2.7

## `RESPONSE1`

Offset `0x014` · access `r` · 32 bits

RESP1: response bits 63:32.

Sources:

- standard (high): SDHCI 3.00, 2.2.7

## `RESPONSE2`

Offset `0x018` · access `r` · 32 bits

RESP2: response bits 95:64.

Sources:

- standard (high): SDHCI 3.00, 2.2.7

## `RESPONSE3`

Offset `0x01C` · access `r` · 32 bits

RESP3: response bits 127:96.

Sources:

- standard (high): SDHCI 3.00, 2.2.7

## `BUFFER_DATA`

Offset `0x020` · access `rw` · 32 bits

DATA: the PIO buffer data port.

Sources:

- standard (high): SDHCI 3.00, 2.2.8
- datasheet (high): BCM2835 ARM Peripherals, EMMC: DATA

## `PRESENT_STATE`

Offset `0x024` · access `r` · 32 bits · reset `0x1FFF0000`

STATUS. Idle: card inserted and stable, CMD and DAT[7:0] high, with or without a card in the slot. After a command times out the real host keeps CMD_INHIBIT (bit 0) set until a command-line reset, so the bootcode's failure message says status 1fff0001; the model lets it go at once and says 1fff0000.

Sources:

- standard (high): SDHCI 3.00, 2.2.9
- measured (high): raspberrypi/rpi-eeprom#227, bootloader 2020-09-03, no card: 'status: 0x1fff0000' on every 'SD HOST:' line, then "Failed to open device: 'sdcard' (cmd 371a0010 status 1fff0001)"

## `HOST_CONTROL`

Offset `0x028` · access `rw` · 32 bits

CONTROL0: host control 1, power, block-gap and wakeup control. Reads back what was written: unlike EMMC2's, no bit reads 1 regardless.

Sources:

- standard (high): SDHCI 3.00, 2.2.10..2.2.13
- measured (high): raspberrypi/rpi-eeprom#139, #227, #282: the 2020 bootloaders print 'CTL0: 0x00000000' after the reset and 'CTL0: 0x00000f00' once bus power and voltage are set, where EMMC2 reads 0x00800f00

## `CLOCK_CONTROL`

Offset `0x02C` · access `rw` · 32 bits

CONTROL1: clock control (15:0), data timeout (19:16) and the self-clearing software resets (26:24). 2020-era bootcode waits here twice: for its software reset to clear, and for the internal clock to report stable once enabled. On the catch-all stub neither happened, and that boot hung before its first line of output (#64).

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `INTERNAL_EN` | rw | CLK_INTLEN: internal clock on. |
| 1 | `STABLE` | r | CLK_STABLE: internal clock stable; immediately, in the model. |
| 2 | `SD_EN` | rw | CLK_EN: SD clock to the card on. |
| 24 | `SRST_ALL` | rw | SRST_HC: reset everything. |
| 25 | `SRST_CMD` | rw | Reset the command circuit. |
| 26 | `SRST_DATA` | rw | Reset the data circuit. |

Sources:

- standard (high): SDHCI 3.00, 2.2.14..2.2.16
- decompile (high): pieeprom-2020-09-03 bootcode: 0x80000ed6 'ld r0, [r3+0x2c]; and r0, r8; beq done', polled for 100 ms of the system timer after writing SRST 0x07000000; 0x80000a40 'ld r0, [r5+0x2c]; btest r0, #1; beq back' after writing 0x000ee201, with no timeout

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

INTERRUPT: normal (15:0) and error (31:16) status, gated by INT_STATUS_EN. With nothing on the bus, a command that expects a response ends in ERR_CMD_TIMEOUT.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `CMD_COMPLETE` | w1c | CMD_DONE: command complete. |
| 15 | `ERROR` | r | ERR: set while any error bit is. |
| 16 | `ERR_CMD_TIMEOUT` | w1c | CTO_ERR: command timeout. |

Sources:

- standard (high): SDHCI 3.00, 2.2.17 / 2.2.18

`CMD_COMPLETE` sources:

- standard (high): SDHCI 3.00, 2.2.17

`ERROR` sources:

- standard (high): SDHCI 3.00, 2.2.17

`ERR_CMD_TIMEOUT` sources:

- standard (high): SDHCI 3.00, 2.2.18

## `INT_STATUS_EN`

Offset `0x034` · access `rw` · 32 bits

IRPT_MASK: which interrupt conditions latch into INT_STATUS.

Sources:

- standard (high): SDHCI 3.00, 2.2.19 / 2.2.20

## `INT_SIGNAL_EN`

Offset `0x038` · access `rw` · 32 bits

IRPT_EN: which INT_STATUS bits drive the interrupt line, GIC SPI 126, shared with EMMC2.

Sources:

- standard (high): SDHCI 3.00, 2.2.21 / 2.2.22
- linux (high): bcm2711-rpi-4-b.dtb: interrupts <GIC_SPI 0x7e IRQ_TYPE_LEVEL_HIGH> on both mmcnr@7e300000 and mmc@7e340000

## `HOST_CONTROL2`

Offset `0x03C` · access `rw` · 32 bits

CONTROL2: auto-CMD error status (15:0, read-only) and host control 2 (31:16).

Sources:

- standard (high): SDHCI 3.00, 2.2.23 / 2.2.24

## `CAPABILITIES_0`

Offset `0x040` · access `r` · 32 bits

Not measured on this block, and not in the BCM2835 datasheet; Linux supplies this host's capabilities itself rather than read them. The model reads 0.

Sources:

- linux (high): drivers/mmc/host/sdhci-iproc.c: bcm2835_data for brcm,bcm2835-sdhci has its own .caps / .caps1 and .missing_caps = true

## `CAPABILITIES_1`

Offset `0x044` · access `r` · 32 bits

Not measured on this block; see CAPABILITIES_0. The model reads 0.

Sources:

- linux (high): drivers/mmc/host/sdhci-iproc.c: bcm2835_data, .missing_caps = true

## `MAX_CURRENT`

Offset `0x048` · access `r` · 32 bits

Not measured on this block. The model reads 0.

Sources:

- standard (high): SDHCI 3.00, 2.2.27

## `CONTROLLER_VERSION`

Offset `0x0FC` · access `r` · 32 bits

SLOTISR_VER: slot interrupt status (low half), vendor and SDHCI version. Not measured on this block. The model reads 0.

Sources:

- datasheet (high): BCM2835 ARM Peripherals, EMMC: SLOTISR_VER
- standard (high): SDHCI 3.00, 2.2.32 / 2.2.33
