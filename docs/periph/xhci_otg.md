<!-- generated from specs/xhci_otg.toml by `cargo run -- spec-docs --update` – do not edit -->

# `xhci_otg` – The BCM2711's own xHCI controller: the USB-C port as a USB 2.0 host, which the bootloader boots from as `BCM-USB-MSD` and Linux uses with `otg_mode=1`

- Bus: `vpu` (VPU bus address)
- Base: `0x7E9C0000`
- Size: `0x100000`
- Interrupts: GIC id 208 (`GIC_SPI 176`)

One USB2 root port, and the same ring engine as the xHCI behind the VL805 (`src/periph/xhci.rs`), so the register layout is the standard one and only the capability values differ: one port, one interrupter, no streams and no scratchpad buffers. Its DMA reaches memory by CPU-physical address — `/scb dma-ranges` is the identity over 16 GB — and those addresses are 64 bits wide: the firmware builds its rings in the low gigabyte, Linux wherever it allocated them, above 4 GB on a board with that much DRAM. `--otg <img>` plugs a mass-storage device into the port; with nothing plugged in the port reads empty and powered, as the other root ports do. The DWC2 core at `0x7E980000` (`specs/dwc2.toml`) is the *other* controller on the same socket: the two are alternatives, and `otg_mode` picks between them.

Sources:

- linux (high): `bcm2711-rpi-4-b.dtb` (raspberrypi/firmware): `/scb/xhci@7e9c0000`, compatible `generic-xhci`, `reg <0x0 0x7e9c0000 0x0 0x00100000>`, `interrupts <0x0 0xb0 0x4>`, `status = "disabled"` — the firmware enables the node when `config.txt` says `otg_mode=1`, and leaves `usb@7e980000` (the DWC2 core) disabled instead
- standard (high): eXtensible Host Controller Interface 1.1, section 5: the register layout at the base
- trace (high): `boot --eeprom firmware/pieeprom.bin --boot-order 0x5 -v`: after `Boot mode: BCM-USB-MSD (05) order 0` the bootloader reads `0x7E9C0000` `+0x00`..`+0x1C` — the capability registers — and nothing else; with the block on the catch-all stub it read zeroes and printed `xHC0 ver: 0 HCS: 00000000 00000000 00000000 HCC: 00000000`, `xHC0 ports 0 slots 0 intrs 0` and `USB xHC init failed`. The `dwc2` log channel is empty for that whole run: this boot mode never touches the DWC2 core
- decompile (medium): `start4.elf` has `bfs_xhci_init` beside `bfs_xhci_init_pcie`, the `otg_mode` config key and the `CLK_USBXHCI` clock name: a second, non-PCIe xHCI to bring up

Interrupts (GIC id 208 (`GIC_SPI 176`)):

Level-triggered, so the line follows interrupter 0's `IMAN.IP` (GIC id 208 — Linux reports 32 higher than the device tree's number). The VPU has no source of its own for this block: the bootloader polls `USBSTS` and the event ring, as it does behind the VL805.

- linux (high): `bcm2711-rpi-4-b.dtb` `/scb/xhci@7e9c0000`: `interrupts <0x0 0xb0 0x4>`
- datasheet (high): BCM2711 ARM Peripherals, §6.2.5 Table 103: the ETH_PCIe level-2 controller's IRQ 48 is `USB0_XHCI_0`, which the device tree numbers `128 + 48` — 176 — _Confirms the note above that the VPU has no source of its own for this block: Table 102 gives the 57 ETH_PCIe lines one ORed source, 122._

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CAPLENGTH`](#caplength) | r | 8 | 1, best medium |
| `0x002` | [`HCIVERSION`](#hciversion) | r | 16 | 1, best medium |
| `0x004` | [`HCSPARAMS1`](#hcsparams1) | r | 32 | 1, best medium |
| `0x008` | [`HCSPARAMS2`](#hcsparams2) | r | 32 | 1, best medium |
| `0x00C` | [`HCSPARAMS3`](#hcsparams3) | r | 32 | 1, best high |
| `0x010` | [`HCCPARAMS1`](#hccparams1) | r | 32 | 1, best medium |
| `0x014` | [`DBOFF`](#dboff) | r | 32 | 1, best medium |
| `0x018` | [`RTSOFF`](#rtsoff) | r | 32 | 1, best medium |
| `0x01C` | [`HCCPARAMS2`](#hccparams2) | r | 32 | 1, best high |
| `0x020` | [`USBCMD`](#usbcmd) | rw | 32 | 1, best high |
| `0x024` | [`USBSTS`](#usbsts) | rw | 32 | 1, best high |
| `0x028` | [`PAGESIZE`](#pagesize) | r | 32 | 1, best high |
| `0x034` | [`DNCTRL`](#dnctrl) | rw | 32 | 1, best high |
| `0x038` | [`CRCR_LO`](#crcr_lo) | rw | 32 | 1, best high |
| `0x03C` | [`CRCR_HI`](#crcr_hi) | rw | 32 | 1, best high |
| `0x050` | [`DCBAAP_LO`](#dcbaap_lo) | rw | 32 | 1, best high |
| `0x054` | [`DCBAAP_HI`](#dcbaap_hi) | rw | 32 | 1, best high |
| `0x058` | [`CONFIG`](#config) | rw | 32 | 1, best high |
| `0x0A0` | [`USBLEGSUP`](#usblegsup) | r | 32 | 1, best high |
| `0x0B0` | [`SUPPORTED_USB2`](#supported_usb2) | r | 32 | 1, best high |
| `0x0B4` | [`SUPPORTED_USB2_NAME`](#supported_usb2_name) | r | 32 | 1, best high |
| `0x0B8` | [`SUPPORTED_USB2_PORTS`](#supported_usb2_ports) | r | 32 | 1, best high |
| `0x100`–`0x180` (33 × 0x4) | [`DOORBELL`](#doorbell) | rw | 32 | 1, best high |
| `0x200` | [`MFINDEX`](#mfindex) | r | 32 | 1, best high |
| `0x220` | [`IMAN`](#iman) | rw | 32 | 1, best high |
| `0x224` | [`IMOD`](#imod) | rw | 32 | 1, best high |
| `0x228` | [`ERSTSZ`](#erstsz) | rw | 32 | 1, best high |
| `0x230` | [`ERSTBA_LO`](#erstba_lo) | rw | 32 | 1, best high |
| `0x234` | [`ERSTBA_HI`](#erstba_hi) | rw | 32 | 1, best high |
| `0x238` | [`ERDP_LO`](#erdp_lo) | rw | 32 | 1, best high |
| `0x23C` | [`ERDP_HI`](#erdp_hi) | rw | 32 | 1, best high |
| `0x420` | [`PORTSC`](#portsc) | rw | 32 | 2, best high |
| `0x424` | [`PORTPMSC`](#portpmsc) | rw | 32 | 1, best high |
| `0x428` | [`PORTLI`](#portli) | r | 32 | 1, best high |
| `0x42C` | [`PORTHLPMC`](#porthlpmc) | rw | 32 | 1, best high |

## `CAPLENGTH`

Offset `0x000` · access `r` · 8 bits · reset `0x20`

Where the operational registers start.

Sources:

- inferred (medium): the standard layout, the same one the VL805's controller uses (`specs/xhci.toml`); the model shares one ring engine between the two, and a driver finds the operational registers through this value whatever it is

## `HCIVERSION`

Offset `0x002` · access `r` · 16 bits · reset `0x100`

xHCI 1.0, the version the bootloader prints as `xHC0 ver: 256`.

Sources:

- inferred (medium): every xHCI on this board reports 1.0; the bootloader refuses a controller whose capability word reads 0

## `HCSPARAMS1`

Offset `0x004` · access `r` · 32 bits · reset `0x1000120`

`MaxSlots` 32, `MaxIntrs` 1, `MaxPorts` 1 — the one USB2 port on the USB-C socket.

Sources:

- inferred (medium): the socket has one port and the model uses one interrupter; 32 slots is what the VL805's controller reports and what the bootloader's slot array is sized for

## `HCSPARAMS2`

Offset `0x008` · access `r` · 32 bits · reset `0x31`

`IST` 1, `ERSTMax` 3 (8 segments), no scratchpad buffers.

Sources:

- inferred (medium): the model's engine needs no scratchpad (`src/periph/xhci.rs`), so the honest value is zero rather than the VL805's 31

## `HCSPARAMS3`

Offset `0x00C` · access `r` · 32 bits · reset `0x0`

U1 / U2 exit latencies: none, this is a USB 2.0 controller.

Sources:

- standard (high): xHCI 1.1, 5.3.4: the latencies describe SuperSpeed link states, which a USB2-only controller has none of

## `HCCPARAMS1`

Offset `0x010` · access `r` · 32 bits · reset `0x280029`

`AC64`, 32-byte contexts, `PPC`, `LHRC`, no streams, `xECP` `0x28` (base + `0xA0`).

Sources:

- inferred (medium): what the shared ring engine implements: 64-bit pointers, 32-byte contexts, port power control, a light host controller reset, and no stream support

## `DBOFF`

Offset `0x014` · access `r` · 32 bits · reset `0x100`

Doorbell array offset.

Sources:

- inferred (medium): the standard layout, as `specs/xhci.toml`

## `RTSOFF`

Offset `0x018` · access `r` · 32 bits · reset `0x200`

Runtime register offset.

Sources:

- inferred (medium): the standard layout, as `specs/xhci.toml`

## `HCCPARAMS2`

Offset `0x01C` · access `r` · 32 bits · reset `0x0`

No xHCI 1.1 capabilities.

Sources:

- standard (high): xHCI 1.1, 5.3.9: every bit is a 1.1 feature the engine does not implement

## `USBCMD`

Offset `0x020` · access `rw` · 32 bits

Run / stop and resets, exactly as behind the VL805: the bootloader sets `HCRST` and polls it clear, then `HSEE`, `INTE` and `RS`.

Sources:

- standard (high): xHCI 1.1, 5.4.1 `USBCMD`

## `USBSTS`

Offset `0x024` · access `rw` · 32 bits

Status; the event and change bits are write-1-to-clear. `HCH` follows the run bit.

Sources:

- standard (high): xHCI 1.1, 5.4.2 `USBSTS`

## `PAGESIZE`

Offset `0x028` · access `r` · 32 bits · reset `0x1`

4 KiB pages.

Sources:

- standard (high): xHCI 1.1, 5.4.3 `PAGESIZE`

## `DNCTRL`

Offset `0x034` · access `rw` · 32 bits

Device notification control. Stored.

Sources:

- standard (high): xHCI 1.1, 5.4.4 `DNCTRL`

## `CRCR_LO`

Offset `0x038` · access `rw` · 32 bits

Command ring pointer and consumer cycle; the pointer reads as 0, only Command Ring Running is visible.

Sources:

- standard (high): xHCI 1.1, 5.4.5 `CRCR`

## `CRCR_HI`

Offset `0x03C` · access `rw` · 32 bits

Command ring pointer, high word; reads 0.

Sources:

- standard (high): xHCI 1.1, 5.4.5 `CRCR`

## `DCBAAP_LO`

Offset `0x050` · access `rw` · 32 bits

Device context base address array pointer.

Sources:

- standard (high): xHCI 1.1, 5.4.6 `DCBAAP`

## `DCBAAP_HI`

Offset `0x054` · access `rw` · 32 bits

`DCBAAP`, high word.

Sources:

- standard (high): xHCI 1.1, 5.4.6 `DCBAAP`

## `CONFIG`

Offset `0x058` · access `rw` · 32 bits

Slots enabled. Stored.

Sources:

- standard (high): xHCI 1.1, 5.4.7 `CONFIG`

## `USBLEGSUP`

Offset `0x0A0` · access `r` · 32 bits · reset `0x401`

Extended capability: USB legacy support, next at `+0x10` dwords (`0xB0`).

Sources:

- standard (high): xHCI 1.1, 7.1: capability id 1, with the next pointer in bits 15:8

## `SUPPORTED_USB2`

Offset `0x0B0` · access `r` · 32 bits · reset `0x2000002`

Supported protocol: USB 2.0, and the last capability in the list — there is no SuperSpeed half here.

Sources:

- standard (high): xHCI 1.1, 7.2: capability id 2, revision 2.0, next pointer 0

## `SUPPORTED_USB2_NAME`

Offset `0x0B4` · access `r` · 32 bits · reset `0x20425355`

`USB `.

Sources:

- standard (high): xHCI 1.1, 7.2: the name string of a supported-protocol capability

## `SUPPORTED_USB2_PORTS`

Offset `0x0B8` · access `r` · 32 bits · reset `0x101`

Port offset 1, count 1.

Sources:

- standard (high): xHCI 1.1, 7.2, with the one root port of `HCSPARAMS1`

## `DOORBELL`

Offset `0x100`, 33 elements 0x4 apart · access `rw` · 32 bits

Doorbell 0 is the command ring, doorbell n slot n; a write runs the ring to completion before it returns.

Sources:

- standard (high): xHCI 1.1, 5.6 Doorbell registers

## `MFINDEX`

Offset `0x200` · access `r` · 32 bits

Microframe index.

Sources:

- standard (high): xHCI 1.1, 5.5.1 `MFINDEX`

## `IMAN`

Offset `0x220` · access `rw` · 32 bits

Interrupter management. One interrupter, whose `IP` drives GIC SPI 176.

Sources:

- standard (high): xHCI 1.1, 5.5.2.1 `IMAN`

## `IMOD`

Offset `0x224` · access `rw` · 32 bits

Interrupter moderation. Stored.

Sources:

- standard (high): xHCI 1.1, 5.5.2.2 `IMOD`

## `ERSTSZ`

Offset `0x228` · access `rw` · 32 bits

Event ring segment table size.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.1 `ERSTSZ`

## `ERSTBA_LO`

Offset `0x230` · access `rw` · 32 bits

Event ring segment table base.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.2 `ERSTBA`

## `ERSTBA_HI`

Offset `0x234` · access `rw` · 32 bits

`ERSTBA`, high word.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.2 `ERSTBA`

## `ERDP_LO`

Offset `0x238` · access `rw` · 32 bits

Event ring dequeue pointer; `EHB` is write-1-to-clear.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.3 `ERDP`

## `ERDP_HI`

Offset `0x23C` · access `rw` · 32 bits

`ERDP`, high word.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.3 `ERDP`

## `PORTSC`

Offset `0x420` · access `rw` · 32 bits

The one root port. Empty but powered reads `0x2A0` with `DR` set, as the VL805's USB2 port does; a device attached reads `0x400202E1` until the host resets the port, and `0x40000E03` once it is enabled at high speed.

Sources:

- standard (high): xHCI 1.1, 5.4.8 `PORTSC`
- measured (high): the same words measured on the VL805's USB2 root port, Raspberry Pi 4B d03115: `0x400202e1` just connected, `0x40000e03` enumerated, `0x000002a0` empty

## `PORTPMSC`

Offset `0x424` · access `rw` · 32 bits

Port power management. Stored.

Sources:

- standard (high): xHCI 1.1, 5.4.9 `PORTPMSC`

## `PORTLI`

Offset `0x428` · access `r` · 32 bits

Port link info.

Sources:

- standard (high): xHCI 1.1, 5.4.10 `PORTLI`

## `PORTHLPMC`

Offset `0x42C` · access `rw` · 32 bits

Port hardware LPM control. Stored.

Sources:

- standard (high): xHCI 1.1, 5.4.11 `PORTHLPMC`
