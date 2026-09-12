<!-- generated from specs/bsc.toml by `cargo run -- spec-docs --update` – do not edit -->

# `bsc` – BSC (I²C master): instance 0 with nothing attached, and the instance the board PMICs and GPIO expander sit on

- Bus: `vpu` (VPU bus address)
- Base: `0x7E205000`
- `PMIC` copy: `0x7E205E00`
- Size: `0x20`

A transfer takes its time on the wire at core_clock / DIV, 500 MHz core clock: TA stays set until the bytes have gone, then DONE (and ERR on an unacknowledged address) latch.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC chapter
- decompile (high): start4's I²C driver FUN_0ecf0ed0 picks the base from the bus id: 0 -> 0x7E205000, 8 -> 0x7E205E00, else 0x7E803000 + id * 0x1000
- trace (high): late in the boot start4 probes 0x52 on instance 0 for a HAT EEPROM; unmapped, S read 0 and the poll never ended

`PMIC` copy:

Bus 8: the PMICs at 0x1B / 0x1E and the FXL6408 at 0x43.

- decompile (high): FUN_0ecf0ed0: bus id 8; pmic_init 0x3ED4DAA8 uses it

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`C`](#c) | rw | 32 | 1, best high |
| `0x004` | [`S`](#s) | rw | 32 | 1, best high |
| `0x008` | [`DLEN`](#dlen) | rw | 32 | 1, best high |
| `0x00C` | [`A`](#a) | rw | 32 | 1, best high |
| `0x010` | [`FIFO`](#fifo) | rw | 32 | 1, best high |
| `0x014` | [`DIV`](#div) | rw | 32 | 2, best high |
| `0x018` | [`DEL`](#del) | rw | 32 | 1, best high |
| `0x01C` | [`CLKT`](#clkt) | rw | 32 | 1, best high |

## `C`

Offset `0x000` · access `rw` · 32 bits

Control.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `READ` | rw | Read transfer. |
| 5:4 | `CLEAR` | w | Clear the FIFO. |
| 7 | `ST` | w | Start a transfer. |
| 15 | `I2CEN` | rw | Controller on. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: C

`READ` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: C.READ

`CLEAR` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: C.CLEAR

`ST` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: C.ST

`I2CEN` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: C.I2CEN

## `S`

Offset `0x004` · access `rw` · 32 bits

Status; DONE, ERR and CLKT are write-1-to-clear.

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

- datasheet (high): BCM2711 ARM Peripherals, BSC: S

`TA` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: S.TA

`DONE` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: S.DONE

`TXW` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: S.TXW

`RXR` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: S.RXR

`TXD` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: S.TXD

`RXD` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: S.RXD

`TXE` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: S.TXE

`RXF` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: S.RXF

`ERR` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: S.ERR

`CLKT` sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: S.CLKT

## `DLEN`

Offset `0x008` · access `rw` · 32 bits

Transfer length; reads back 0 once every byte has moved.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: DLEN

## `A`

Offset `0x00C` · access `rw` · 32 bits

7-bit slave address.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: A

## `FIFO`

Offset `0x010` · access `rw` · 32 bits

Data FIFO, 16 bytes each way.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: FIFO

## `DIV`

Offset `0x014` · access `rw` · 32 bits

Clock divisor. start4 programs 5000 (100 kHz) for the PMIC bus, 2500 for its probe sweep, 540 for HDMI DDC.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: DIV
- measured (high): vcgencmd measure_clock core on rpi-dev: 500000992 Hz

## `DEL`

Offset `0x018` · access `rw` · 32 bits

Data delay. Stored, otherwise ignored.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: DEL

## `CLKT`

Offset `0x01C` · access `rw` · 32 bits

Clock-stretch timeout. Stored, otherwise ignored.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, BSC: CLKT
