<!-- generated from specs/spi0.toml by `cargo run -- spec-docs --update` – do not edit -->

# `spi0` – SPI0 master, with the serial-NOR flash the EEPROM bootloader was loaded from

- Bus: `vpu` (VPU bus address)
- Base: `0x7E204000`
- Size: `0x18`
- Interrupts: VPU source 118 · GIC id 150 (`GIC_SPI 118`)

Driven in polled single-byte mode. The pads are GPIO 40..43 and only ALT4 puts them on the flash, so a transfer with the pins elsewhere reads MISO idle-high (`specs/gpio.toml`). Every shift is instantaneous; the flash answers `READ`, `FAST_READ`, `RDID`, `RDSR`, `WREN` / `WRDI`, `SE` and `PP`. start4 reads the flash once, before it touches the SD card: it saves the functions of GPIO 40..43, puts them on ALT4, reads every section header of the image (a 28-byte full-duplex `READ`: command, three address bytes, the 24-byte header; at most 33 sections, stopping at a bad magic or at 512 KiB), then the data of the first `pubkey.bin` and `bootconf.txt` (data length + 4 bytes in one transfer), and gives the pins their functions back. Each transfer's buffer is also what goes out after the address, so the header reads send the previous header, and the first one whatever was on the stack. The two EEPROM stages instead switch GPIO 43, 40, 41 and 42 to ALT4, in that order, for every flash session. Afterwards they clear `CS` and `CLK` and put the four pins back to inputs in the same order. GPIO 42 is also the activity LED, so they then drive it to the state they last set it to, which makes it an output again. After an error code has been flashed, that state is off.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI chapter
- decompile (high): EEPROM bootloader: wait `TXD`, write FIFO, wait `RXD`, read FIFO per byte; start4's EEPROM scanner `0x3ED77E00`
- decompile (high): start4 platform init `0x3ED4947A` (BCM2711 only): GPIO 40..43 saved and set to ALT4 through the GPIO driver, `bootloader_eeprom_find_files` `0x3EC649BC` (header walk `0x3EC64ED0`, file read `0x3EC655B0`), pins restored
- trace (high): start4: `GPFSEL4` `0x40` -> `0x6DB` at `0x3ECC9562` (pins 40..43 to ALT4), 27 header reads and reads of 512 and 79 bytes from the stock image, `GPFSEL4` back to `0x40`, then the first log line
- decompile (high): bootloader: session end `0xAEDDC` (`CS`, `CLK` <- 0, pins 43 and 40..42 to function 0), then `0xA937A`, which drives the LED from the state `0xA9534` records; the error flash `0xA817A` ends with `0xA9534(0)`
- trace (high): bootcode and bootloader: `GPFSEL4` `0x40`, `0x640`, `0x643`, `0x65B`, `0x6DB` into a session; `0xDB`, `0xD8`, `0xC0`, `0` after it, then `GPSET1` (or `GPCLR1`) <- `0x400` and `GPFSEL4` <- `0x40`

Interrupts (VPU source 118 · GIC id 150 (`GIC_SPI 118`)):

One line for every SPI master on the chip: SPI0 here and SPI3 to SPI6 in the `0x7E204600`..`0x7E204C00` block all raise it, and `PACTL_CS` bits 0 to 6 say which. The EEPROM bootloader polls `CS.DONE` instead of taking it.

- datasheet (high): BCM2711 ARM Peripherals, §6.2.4 Table 102: VC peripheral IRQ 54 is the OR of all SPI masters, VPU source 118; §6.2.4 Figure 6 puts SPI0 on `PACTL_CS` bit 0
- linux (high): `firmware/bcm2711-rpi-4-b.dtb`: `/soc/spi@7e204000` and the four masters at `0x7e204600`..`0x7e204c00` all carry `interrupts = <0x0 0x76 0x4>`

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CS`](#cs) | rw | 32 | 1, best high |
| `0x004` | [`FIFO`](#fifo) | rw | 32 | 1, best high |
| `0x008` | [`CLK`](#clk) | rw | 32 | 3, best high |
| `0x00C` | [`DLEN`](#dlen) | rw | 32 | 1, best high |
| `0x010` | [`LTOH`](#ltoh) | rw | 32 | 1, best high |
| `0x014` | [`DC`](#dc) | rw | 32 | 1, best high |

## `CS`

Offset `0x000` · access `rw` · 32 bits

Control and status.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 1:0 | `CS` | rw | Chip select. |
| 4 | `CLEAR_TX` | w | Clear the TX FIFO. |
| 5 | `CLEAR_RX` | w | Clear the RX FIFO. |
| 7 | `TA` | rw | Transfer active; the whole command runs with it set. start4 writes `CS = 0` (its mode bits for the flash), sets `TA` with a read-modify-write, and clears it the same way once the transfer is done. |
| 16 | `DONE` | r | Nothing left to shift. start4's transfer waits for it, with no timeout, once it has written and read back every byte, then clears `TA`. |
| 17 | `RXD` | r | RX FIFO holds data. |
| 18 | `TXD` | r | TX FIFO has room. |
| 19 | `RXR` | r | RX FIFO needs reading. |
| 20 | `RXF` | r | RX FIFO full. |

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `CS`

`CS` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `CS`

`CLEAR_TX` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `CS.CLEAR`

`CLEAR_RX` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `CS.CLEAR`

`TA` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `CS.TA`

`DONE` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `CS.DONE`
- decompile (high): `0x3ED77E00`: `do {} while ((CS & 0x10000) == 0)` after its byte loop, read at `0x3ED77EE8`

`RXD` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `CS.RXD`

`TXD` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `CS.TXD`

`RXR` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `CS.RXR`

`RXF` sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `CS.RXF`

## `FIFO`

Offset `0x004` · access `rw` · 32 bits

Write shifts a byte out, read takes the byte shifted in.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `FIFO`

## `CLK`

Offset `0x008` · access `rw` · 32 bits

Clock divider. start4 writes 63 before every transfer: `max(ceil(source / 8 MHz), 2)`, its flash configuration asking for 8 MHz from a source of about 500 MHz.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `CLK`
- decompile (high): `0x3ED77E00`: 64-bit `ceil(rate / speed)` then `max(.., 2)`; speed 8000000 from the configuration at `DAT_0edfe1c8`
- trace (high): start4 `0x3ED77E74` writes `0x3F`

## `DLEN`

Offset `0x00C` · access `rw` · 32 bits

DMA data length.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `DLEN`

## `LTOH`

Offset `0x010` · access `rw` · 32 bits

LoSSI output hold delay.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `LTOH`

## `DC`

Offset `0x014` · access `rw` · 32 bits

DMA DREQ controls.

Sources:

- datasheet (high): BCM2711 ARM Peripherals, SPI: `DC`
