<!-- generated from specs/xhci_otg.toml by `cargo run -- spec-docs --update` – do not edit -->

# `xhci_otg` – The BCM2711's own xHCI controller: the USB-C port as a USB 2.0 host, which the bootloader boots from as `BCM-USB-MSD` and Linux uses with `otg_mode=1`

- Bus: `vpu` (VPU bus address)
- Base: `0x7E9C0000`
- Size: `0x100000`
- Interrupts: GIC id 208 (`GIC_SPI 176`)

One USB2 root port, and the same ring engine as the xHCI behind the VL805 (`src/periph/xhci.rs`), so the register layout is the standard one and only the capability values differ: one port, one interrupter, 64 slots, no streams and one scratchpad buffer, with the doorbells at `DBOFF` `0x480` and the runtime registers at `RTSOFF` `0x440` rather than the VL805's `0x100` and `0x200`. Its DMA reaches memory by CPU-physical address — `/scb dma-ranges` is the identity over 16 GB — and those addresses are 64 bits wide: the firmware builds its rings in the low gigabyte, Linux wherever it allocated them, above 4 GB on a board with that much DRAM. `--otg <img>` plugs a mass-storage device into the port; with nothing plugged in the port reads empty and powered, as the other root ports do. The DWC2 core at `0x7E980000` (`specs/dwc2.toml`) is the *other* controller on the same socket: the two are alternatives, and `otg_mode` picks between them.

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
| `0x000` | [`CAPLENGTH`](#caplength) | r | 8 | 1, best high |
| `0x002` | [`HCIVERSION`](#hciversion) | r | 16 | 1, best high |
| `0x004` | [`HCSPARAMS1`](#hcsparams1) | r | 32 | 1, best high |
| `0x008` | [`HCSPARAMS2`](#hcsparams2) | r | 32 | 1, best high |
| `0x00C` | [`HCSPARAMS3`](#hcsparams3) | r | 32 | 2, best high |
| `0x010` | [`HCCPARAMS1`](#hccparams1) | r | 32 | 1, best high |
| `0x014` | [`DBOFF`](#dboff) | r | 32 | 1, best high |
| `0x018` | [`RTSOFF`](#rtsoff) | r | 32 | 1, best high |
| `0x01C` | [`HCCPARAMS2`](#hccparams2) | r | 32 | 2, best high |
| `0x020` | [`USBCMD`](#usbcmd) | rw | 32 | 1, best high |
| `0x024` | [`USBSTS`](#usbsts) | rw | 32 | 1, best high |
| `0x028` | [`PAGESIZE`](#pagesize) | r | 32 | 1, best high |
| `0x034` | [`DNCTRL`](#dnctrl) | rw | 32 | 1, best high |
| `0x038` | [`CRCR_LO`](#crcr_lo) | rw | 32 | 1, best high |
| `0x03C` | [`CRCR_HI`](#crcr_hi) | rw | 32 | 1, best high |
| `0x050` | [`DCBAAP_LO`](#dcbaap_lo) | rw | 32 | 1, best high |
| `0x054` | [`DCBAAP_HI`](#dcbaap_hi) | rw | 32 | 1, best high |
| `0x058` | [`CONFIG`](#config) | rw | 32 | 1, best high |
| `0x880` | [`USBLEGSUP`](#usblegsup) | r | 32 | 2, best high |
| `0x890` | [`SUPPORTED_USB2`](#supported_usb2) | r | 32 | 2, best high |
| `0x894` | [`SUPPORTED_USB2_NAME`](#supported_usb2_name) | r | 32 | 2, best high |
| `0x898` | [`SUPPORTED_USB2_PORTS`](#supported_usb2_ports) | r | 32 | 2, best high |
| `0x480`–`0x580` (65 × 0x4) | [`DOORBELL`](#doorbell) | rw | 32 | 2, best high |
| `0x440` | [`MFINDEX`](#mfindex) | r | 32 | 2, best high |
| `0x460` | [`IMAN`](#iman) | rw | 32 | 2, best high |
| `0x464` | [`IMOD`](#imod) | rw | 32 | 2, best high |
| `0x468` | [`ERSTSZ`](#erstsz) | rw | 32 | 2, best high |
| `0x470` | [`ERSTBA_LO`](#erstba_lo) | rw | 32 | 2, best high |
| `0x474` | [`ERSTBA_HI`](#erstba_hi) | rw | 32 | 2, best high |
| `0x478` | [`ERDP_LO`](#erdp_lo) | rw | 32 | 2, best high |
| `0x47C` | [`ERDP_HI`](#erdp_hi) | rw | 32 | 2, best high |
| `0x420` | [`PORTSC`](#portsc) | rw | 32 | 2, best high |
| `0x424` | [`PORTPMSC`](#portpmsc) | rw | 32 | 1, best high |
| `0x428` | [`PORTLI`](#portli) | r | 32 | 1, best high |
| `0x42C` | [`PORTHLPMC`](#porthlpmc) | rw | 32 | 1, best high |

## `CAPLENGTH`

Offset `0x000` · access `r` · 8 bits · reset `0x20`

Where the operational registers start.

Sources:

- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: word `+0x00` reads `0x01100020`, `CAPLENGTH` `0x20` and `HCIVERSION` `0x0110`

## `HCIVERSION`

Offset `0x002` · access `r` · 16 bits · reset `0x110`

xHCI 1.1, the version the bootloader prints as `xHC0 ver: 272`.

Sources:

- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: word `+0x00` reads `0x01100020`: `CAPLENGTH` `0x20`, `HCIVERSION` `0x0110`

## `HCSPARAMS1`

Offset `0x004` · access `r` · 32 bits · reset `0x1000140`

`MaxSlots` 64, `MaxIntrs` 1, `MaxPorts` 1 — the one USB2 port on the USB-C socket.

Sources:

- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `0x01000140`

## `HCSPARAMS2`

Offset `0x008` · access `r` · 32 bits · reset `0xC0000F1`

`IST` 1, `ERSTMax` 15, and one scratchpad buffer (`Max_Scratchpad_Bufs_Lo` 1): a driver has to give the controller that page.

Sources:

- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `0x0C0000F1`

## `HCSPARAMS3`

Offset `0x00C` · access `r` · 32 bits · reset `0x7FF000A`

U1 device exit latency 10 us, U2 `0x7FF`.

Sources:

- standard (high): xHCI 1.1, 5.3.4: the latencies describe SuperSpeed link states, which a USB2-only controller has none of
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `0x07FF000A`

## `HCCPARAMS1`

Offset `0x010` · access `r` · 32 bits · reset `0x220FE65`

`AC64`, 32-byte contexts, `PPC`, `LHRC`, `xECP` `0x220` (base + `0x880`).

Sources:

- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `0x0220FE65`

## `DBOFF`

Offset `0x014` · access `r` · 32 bits · reset `0x480`

Doorbell array offset.

Sources:

- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `0x00000480`

## `RTSOFF`

Offset `0x018` · access `r` · 32 bits · reset `0x440`

Runtime register offset.

Sources:

- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `0x00000440`

## `HCCPARAMS2`

Offset `0x01C` · access `r` · 32 bits · reset `0x2F`

Extended capability parameters.

Sources:

- standard (high): xHCI 1.1, 5.3.9: every bit is a 1.1 feature the engine does not implement
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `0x0000002F`

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

Offset `0x880` · access `r` · 32 bits · reset `0x401`

Extended capability: USB legacy support, next at `+0x10` dwords (`0xB0`).

Sources:

- standard (high): xHCI 1.1, 7.1: capability id 1, with the next pointer in bits 15:8
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: the first extended capability, at `xECP` * 4 = `0x880`, reads `0x00000401`

## `SUPPORTED_USB2`

Offset `0x890` · access `r` · 32 bits · reset `0x2000002`

Supported protocol: USB 2.0, and the last capability in the list — there is no SuperSpeed half here.

Sources:

- standard (high): xHCI 1.1, 7.2: capability id 2, revision 2.0, next pointer 0
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: the second and last extended capability, at `0x890`, reads `0x02000002`

## `SUPPORTED_USB2_NAME`

Offset `0x894` · access `r` · 32 bits · reset `0x20425355`

`USB `.

Sources:

- standard (high): xHCI 1.1, 7.2: the name string of a supported-protocol capability
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `0x20425355`, the string `USB `

## `SUPPORTED_USB2_PORTS`

Offset `0x898` · access `r` · 32 bits · reset `0x180101`

Port offset 1, count 1, and the protocol slot type and speed-ID count the silicon reports in the upper half.

Sources:

- standard (high): xHCI 1.1, 7.2, with the one root port of `HCSPARAMS1`
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `0x00180101`

## `DOORBELL`

Offset `0x480`, 65 elements 0x4 apart · access `rw` · 32 bits

Doorbell 0 is the command ring, doorbell n slot n; a write runs the ring to completion before it returns.

Sources:

- standard (high): xHCI 1.1, 5.6 Doorbell registers
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `DBOFF` reads `0x480`, and 64 slots make 65 doorbells

## `MFINDEX`

Offset `0x440` · access `r` · 32 bits

Microframe index.

Sources:

- standard (high): xHCI 1.1, 5.5.1 `MFINDEX`
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `RTSOFF` reads `0x440`, and `MFINDEX` is its first register

## `IMAN`

Offset `0x460` · access `rw` · 32 bits

Interrupter management. One interrupter, whose `IP` drives GIC SPI 176.

Sources:

- standard (high): xHCI 1.1, 5.5.2.1 `IMAN`
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `RTSOFF` `0x440` plus the interrupter set at `+0x20`

## `IMOD`

Offset `0x464` · access `rw` · 32 bits

Interrupter moderation. Stored.

Sources:

- standard (high): xHCI 1.1, 5.5.2.2 `IMOD`
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `RTSOFF` `0x440` plus `+0x24`

## `ERSTSZ`

Offset `0x468` · access `rw` · 32 bits

Event ring segment table size.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.1 `ERSTSZ`
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `RTSOFF` `0x440` plus `+0x28`

## `ERSTBA_LO`

Offset `0x470` · access `rw` · 32 bits

Event ring segment table base.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.2 `ERSTBA`
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `RTSOFF` `0x440` plus `+0x30`

## `ERSTBA_HI`

Offset `0x474` · access `rw` · 32 bits

`ERSTBA`, high word.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.2 `ERSTBA`
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `RTSOFF` `0x440` plus `+0x34`

## `ERDP_LO`

Offset `0x478` · access `rw` · 32 bits

Event ring dequeue pointer; `EHB` is write-1-to-clear.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.3 `ERDP`
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `RTSOFF` `0x440` plus `+0x38`

## `ERDP_HI`

Offset `0x47C` · access `rw` · 32 bits

`ERDP`, high word.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.3 `ERDP`
- measured (high): Raspberry Pi 4B d03115, 32-bit `/dev/mem` read of `0xFE9C0000` with the pinned `start4.elf` running and `otg_mode=1` set, so the controller is powered: `RTSOFF` `0x440` plus `+0x3C`

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
