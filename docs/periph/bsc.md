<!-- generated from specs/bsc.toml by `cargo run -- spec-docs --update` – do not edit -->

# `bsc` – BSC (I²C master): instance 0 with nothing attached, and the instance the board PMICs and GPIO expander sit on

- Bus: `vpu` (VPU bus address)
- Base: `0x7E205000`
- `PMIC` copy: `0x7E205E00`
- Size: `0x20`
- Interrupts: VPU source 117 · GIC id 149 (`GIC_SPI 117`)

A transfer takes its time on the wire at `core_clock / DIV`, 500 MHz core clock: `TA` stays set until the bytes have gone, then `DONE` (and `ERR` on an unacknowledged address) latch. Instance 0 is on GPIO 0/1, 28/29 or 44/45 (`specs/gpio.toml`), and only the first pair is the 40-pin header: a HAT's ID EEPROM answers while the pins are there and not otherwise. The `0x7E205E00` instance has pins of its own and is not muxed.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC chapter
- decompile (high): start4's I²C driver `FUN_0ecf0ed0` picks the base from the bus id: 0 -> `0x7E205000`, 8 -> `0x7E205E00`, else `0x7E803000 + id * 0x1000`
- trace (high): late in the boot start4 probes `0x52` on instance 0 for a HAT EEPROM; unmapped, `S` read 0 and the poll never ended
- trace (high): pinned start4 on instance 0, each probe in a session of its own at `DIV` `0x1388` with I2C0 muxed to GPIO 44/45 (ALT1) and released after: `camera_auto_detect` reads `0x10` reg `0x0000`, `0x36` reg `0x300A`, `0x1A` reg `0x0016`, 32 bytes from `0x40`, `0x1A` reg `0x303E`, `0x1A` reg `0x0016` and `0x1A` reg `0x3254` (twice each); after each `DISPLAY_DSI_PORT not defined` it reads `0x45` reg `0x80`, then reg `0x01`, whether or not `display_auto_detect` is set; later, unless `force_eeprom_read=0`, `0x50-0x53` in a session each on GPIO 0/1 (ALT0), up to ten queued reads of 4 bytes from reg `0x0000` per address — _checked by booting with `camera_auto_detect` and `display_auto_detect` removed from `config.txt` in turn, and with `force_eeprom_read=0` added_

`PMIC` copy:

Bus 8: the PMICs at `0x1B` / `0x1E` and the FXL6408 at `0x43`.

- decompile (high): `FUN_0ecf0ed0`: bus id 8; `pmic_init` `0x3ED4DAA8` uses it

Interrupts (VPU source 117 · GIC id 149 (`GIC_SPI 117`)):

One line for every I²C master on the chip; `PACTL_CS` bits 8 to 15 say which of them is pending. start4 polls `S.DONE` rather than taking it.

- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102: VC peripheral IRQ 53 is the OR of all I²C masters, VPU source 117; §6.2.4 Figure 6 puts I2C0 on `PACTL_CS` bit 8
- linux (high): `firmware/bcm2711-rpi-4-b.dtb`: `/soc/i2c@7e205000`, the masters at `0x7e205600`..`0x7e205c00` and `/soc/i2c@7e804000` all carry `interrupts = <0x0 0x75 0x4>`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`C`](#c) | rw | 32 | 2, best high |
| `0x004` | [`S`](#s) | rw | 32 | 1, best high |
| `0x008` | [`DLEN`](#dlen) | rw | 32 | 1, best high |
| `0x00C` | [`A`](#a) | rw | 32 | 1, best high |
| `0x010` | [`FIFO`](#fifo) | rw | 32 | 1, best high |
| `0x014` | [`DIV`](#div) | rw | 32 | 4, best high |
| `0x018` | [`DEL`](#del) | rw | 32 | 1, best high |
| `0x01C` | [`CLKT`](#clkt) | rw | 32 | 2, best high |

## `C`

Offset `0x000` · access `rw` · 32 bits

Control. start4 reads a register in one of two ways. Either it sets `ST` for the write, then sets `DLEN` and `ST | READ` for the read before it puts the register bytes in the FIFO, so the read follows as a repeated start (sensor probes, the expander's id). Or it writes the register, waits for `DONE`, then starts the read (the display probe). Every read ends with `C` written back as read, `S` cleared, the FIFO cleared twice and `C` <- 0.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `READ` | rw | Read transfer. |
| 5:4 | `CLEAR` | w | Clear the FIFO. |
| 7 | `ST` | w | Start a transfer. |
| 15 | `I2CEN` | rw | Controller on. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `C`
- trace (high): pinned start4: queued read at `0x3ECF0FD8` / `0x3ECF1150` with the FIFO writes at `0x3ECF115A` after it; write-then-read at `0x3ECF0FD8`, FIFO at `0x3ECF1046`, then `0x3ECF1100`; the common end at `0x3ECF2F92`, `0x3ECF2FFC`, `0x3ECF1826` / `0x3ECF185A`, `0x3ECF3028`

`READ` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `C.READ`

`CLEAR` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `C.CLEAR`

`ST` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `C.ST`

`I2CEN` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `C.I2CEN`

## `S`

Offset `0x004` · access `rw` · 32 bits

Status; `DONE`, `ERR` and `CLKT` are write-1-to-clear.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `TA` | r | Transfer active. |
| 1 | `DONE` | w1c | Transfer done. |
| 2 | `TXW` | r | FIFO needs writing. Kept clear. |
| 3 | `RXR` | r | FIFO needs reading. Kept clear. |
| 4 | `TXD` | r | FIFO can accept data. |
| 5 | `RXD` | r | FIFO contains data. |
| 6 | `TXE` | r | FIFO empty. |
| 7 | `RXF` | r | FIFO full. |
| 8 | `ERR` | w1c | Address not acknowledged. |
| 9 | `CLKT` | w1c | Clock-stretch timeout. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `S`

`TA` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `S.TA`

`DONE` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `S.DONE`

`TXW` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `S.TXW`

`RXR` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `S.RXR`

`TXD` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `S.TXD`

`RXD` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `S.RXD`

`TXE` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `S.TXE`

`RXF` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `S.RXF`

`ERR` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `S.ERR`

`CLKT` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `S.CLKT`

## `DLEN`

Offset `0x008` · access `rw` · 32 bits

Transfer length; reads back 0 once every byte has moved.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `DLEN`

## `A`

Offset `0x00C` · access `rw` · 32 bits

7-bit slave address.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `A`

## `FIFO`

Offset `0x010` · access `rw` · 32 bits

Data FIFO, 16 bytes each way.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `FIFO`

## `DIV`

Offset `0x014` · access `rw` · 32 bits

Clock divisor. start4 programs 5000 (100 kHz) for its PMIC sessions and 2500 for its FXL6408 sessions on the PMIC bus, 2500 for its probe sweep, 540 for HDMI DDC. Each session starts with `DIV`, then `DEL`, then `CLKT`. The bootloader aims at 100 kHz from the core clock as it reckons it: 540 while the core runs from the 54 MHz crystal, 5000 once its PLLs are started (bit 9 of `0x7E50_0220` set), with `DEL` at an eighth and a half of the divider (`0x0043010E`, `0x027109C4`) and `CLKT` `0x100` written between them.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `DIV`
- measured (high): `vcgencmd measure_clock core` on a Raspberry Pi 4B d03115: 500000992 Hz
- decompile (high): bootsys `0x8000346E`: `DIV = rate_mhz * 1e6 / 100000` from the frequency query `0x800026F6`, `DEL = max(DIV >> 3, 1) << 16 | max(DIV >> 1, 1)`; the pinned bootloader in the model, as a Raspberry Pi 4B d03115, makes 32 transfers at 540, then 27 in bootsys and all of bootmain's at 5000
- trace (high): pinned start4 on `0x7E205E00`: `0x9C4` at `0x3ECF2E56` before its FXL6408 transfers, `0x1388` before the ones to `0x1B` / `0x1E`

## `DEL`

Offset `0x018` · access `rw` · 32 bits

Data delay. Stored, otherwise ignored. start4 pairs it with `DIV`: `0x9C0271` with 2500, `0x13804E2` with 5000.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `DEL`

## `CLKT`

Offset `0x01C` · access `rw` · 32 bits

Clock-stretch timeout. Stored, otherwise ignored. start4 writes the session's timeout (`0x100`, or `0x200` for the display probe) when it opens a session and before every transfer, and `0x100` again after it closes one.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: `CLKT`
- trace (high): pinned start4: `0x100` at `0x3ECF2DD6` (session open, transfer start) and `0x3ECF2DFE` (after the close at `0x3ECF2ECC`)
