<!-- generated from specs/xhci.toml by `cargo run -- spec-docs --update` – do not edit -->

# `xhci` – xHCI register block behind the VL805's BAR0: capability, operational, runtime and doorbell registers

- Bus: `pci` (offset in the PCI function)
- Base: `0x00000000`
- Size: `0x1000`

Offsets are into BAR0 (4 KiB). The VPU reaches it only by 40-bit DMA through the PCIe outbound window, the ARM through that window at 0x6_0000_0000. Five root ports: port 1 USB2, ports 2..5 USB3; a VIA hub sits on port 1 on every board.

Sources:

- standard (high): eXtensible Host Controller Interface 1.1, section 5
- measured (high): capability registers and PORTSC read on a Raspberry Pi 4B d03115 through /dev/mem (docs/usb-xhci.md section 2); dmesg 'hcc params 0x002841eb hci version 0x100'
- measured (high): examples-on-real-hardware/sd-card-boot.log: 'xHC0 ver: 256 HCS: 05000420 fc000031 00e70004 HCC: 002841eb'

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x000` | [`CAPLENGTH`](#caplength) | r | 8 | 1, best high |
| `0x002` | [`HCIVERSION`](#hciversion) | r | 16 | 1, best high |
| `0x004` | [`HCSPARAMS1`](#hcsparams1) | r | 32 | 1, best high |
| `0x008` | [`HCSPARAMS2`](#hcsparams2) | r | 32 | 1, best high |
| `0x00C` | [`HCSPARAMS3`](#hcsparams3) | r | 32 | 1, best high |
| `0x010` | [`HCCPARAMS1`](#hccparams1) | r | 32 | 1, best high |
| `0x014` | [`DBOFF`](#dboff) | r | 32 | 1, best high |
| `0x018` | [`RTSOFF`](#rtsoff) | r | 32 | 1, best high |
| `0x01C` | [`HCCPARAMS2`](#hccparams2) | r | 32 | 1, best high |
| `0x020` | [`USBCMD`](#usbcmd) | rw | 32 | 1, best high |
| `0x024` | [`USBSTS`](#usbsts) | rw | 32 | 2, best high |
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
| `0x0D0` | [`SUPPORTED_USB3`](#supported_usb3) | r | 32 | 1, best high |
| `0x0D4` | [`SUPPORTED_USB3_NAME`](#supported_usb3_name) | r | 32 | 1, best high |
| `0x0D8` | [`SUPPORTED_USB3_PORTS`](#supported_usb3_ports) | r | 32 | 1, best high |
| `0x100`–`0x180` (33 × 0x4) | [`DOORBELL`](#doorbell) | rw | 32 | 1, best high |
| `0x200` | [`MFINDEX`](#mfindex) | r | 32 | 1, best high |
| `0x220`–`0x280` (4 × 0x20) | [`IMAN`](#iman) | rw | 32 | 1, best high |
| `0x224`–`0x284` (4 × 0x20) | [`IMOD`](#imod) | rw | 32 | 1, best high |
| `0x228`–`0x288` (4 × 0x20) | [`ERSTSZ`](#erstsz) | rw | 32 | 1, best high |
| `0x230`–`0x290` (4 × 0x20) | [`ERSTBA_LO`](#erstba_lo) | rw | 32 | 1, best high |
| `0x234`–`0x294` (4 × 0x20) | [`ERSTBA_HI`](#erstba_hi) | rw | 32 | 1, best high |
| `0x238`–`0x298` (4 × 0x20) | [`ERDP_LO`](#erdp_lo) | rw | 32 | 1, best high |
| `0x23C`–`0x29C` (4 × 0x20) | [`ERDP_HI`](#erdp_hi) | rw | 32 | 1, best high |
| `0x300` | [`DEBUG_CAP`](#debug_cap) | r | 32 | 1, best high |
| `0x420`–`0x460` (5 × 0x10) | [`PORTSC`](#portsc) | rw | 32 | 2, best high |
| `0x424`–`0x464` (5 × 0x10) | [`PORTPMSC`](#portpmsc) | rw | 32 | 1, best high |
| `0x428`–`0x468` (5 × 0x10) | [`PORTLI`](#portli) | r | 32 | 1, best high |
| `0x42C`–`0x46C` (5 × 0x10) | [`PORTHLPMC`](#porthlpmc) | rw | 32 | 1, best high |

## `CAPLENGTH`

Offset `0x000` · access `r` · 8 bits · reset `0x20`

Where the operational registers start.

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem

## `HCIVERSION`

Offset `0x002` · access `r` · 16 bits · reset `0x100`

xHCI 1.0.

Sources:

- measured (high): Raspberry Pi 4B d03115 dmesg: hci version 0x100

## `HCSPARAMS1`

Offset `0x004` · access `r` · 32 bits · reset `0x5000420`

MaxSlots 32, MaxIntrs 4, MaxPorts 5.

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem; sd-card-boot.log 'xHC0 ports 5 slots 32 intrs 4'

## `HCSPARAMS2`

Offset `0x008` · access `r` · 32 bits · reset `0xFC000031`

IST 1, ERSTMax 3, MaxScratchpad 31.

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem

## `HCSPARAMS3`

Offset `0x00C` · access `r` · 32 bits · reset `0xE70004`

Exit latencies.

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem

## `HCCPARAMS1`

Offset `0x010` · access `r` · 32 bits · reset `0x2841EB`

AC64, 32-byte contexts, PPC, xECP 0x28 (BAR0 + 0xA0).

Sources:

- measured (high): Raspberry Pi 4B d03115 dmesg: hcc params 0x002841eb

## `DBOFF`

Offset `0x014` · access `r` · 32 bits · reset `0x100`

Doorbell array offset.

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem

## `RTSOFF`

Offset `0x018` · access `r` · 32 bits · reset `0x200`

Runtime register offset.

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem

## `HCCPARAMS2`

Offset `0x01C` · access `r` · 32 bits · reset `0x0`

No extended capabilities of version 1.1.

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem

## `USBCMD`

Offset `0x020` · access `rw` · 32 bits

Run / stop and resets.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `RS` | rw | Run. |
| 1 | `HCRST` | rw | Host controller reset: everything back to power-on. |
| 2 | `INTE` | rw | Interrupter enable. |
| 7 | `LHCRST` | rw | Light host controller reset. |

Sources:

- standard (high): xHCI 1.1, 5.4.1 USBCMD

`RS` sources:

- standard (high): xHCI 1.1, 5.4.1

`HCRST` sources:

- standard (high): xHCI 1.1, 5.4.1

`INTE` sources:

- standard (high): xHCI 1.1, 5.4.1

`LHCRST` sources:

- standard (high): xHCI 1.1, 5.4.1

## `USBSTS`

Offset `0x024` · access `rw` · 32 bits

Status; the event and change bits are write-1-to-clear. The bring-up's stop path writes all-ones here.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `HCH` | r | Halted. |
| 2 | `HSE` | w1c | Host system error. |
| 3 | `EINT` | w1c | Event interrupt. |
| 4 | `PCD` | w1c | Port change detected. |
| 10 | `SRE` | w1c | Save / restore error. |

Sources:

- standard (high): xHCI 1.1, 5.4.2 USBSTS
- measured (high): reference log pre-handover XHCI-STOP prints 'USBSTS 18' (EINT | PCD) because of the hub

`HCH` sources:

- standard (high): xHCI 1.1, 5.4.2

`HSE` sources:

- standard (high): xHCI 1.1, 5.4.2

`EINT` sources:

- standard (high): xHCI 1.1, 5.4.2

`PCD` sources:

- standard (high): xHCI 1.1, 5.4.2

`SRE` sources:

- standard (high): xHCI 1.1, 5.4.2

## `PAGESIZE`

Offset `0x028` · access `r` · 32 bits · reset `0x1`

4 KiB pages.

Sources:

- standard (high): xHCI 1.1, 5.4.3 PAGESIZE

## `DNCTRL`

Offset `0x034` · access `rw` · 32 bits

Device notification control. Stored.

Sources:

- standard (high): xHCI 1.1, 5.4.4 DNCTRL

## `CRCR_LO`

Offset `0x038` · access `rw` · 32 bits

Command ring pointer and consumer cycle; the pointer reads as 0, only Command Ring Running is visible.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3 | `CRR` | r | Command ring running. |

Sources:

- standard (high): xHCI 1.1, 5.4.5 CRCR

`CRR` sources:

- standard (high): xHCI 1.1, 5.4.5

## `CRCR_HI`

Offset `0x03C` · access `rw` · 32 bits

Command ring pointer, high word; reads 0.

Sources:

- standard (high): xHCI 1.1, 5.4.5 CRCR

## `DCBAAP_LO`

Offset `0x050` · access `rw` · 32 bits

Device context base address array pointer.

Sources:

- standard (high): xHCI 1.1, 5.4.6 DCBAAP

## `DCBAAP_HI`

Offset `0x054` · access `rw` · 32 bits

DCBAAP, high word.

Sources:

- standard (high): xHCI 1.1, 5.4.6 DCBAAP

## `CONFIG`

Offset `0x058` · access `rw` · 32 bits

Slots enabled. Stored.

Sources:

- standard (high): xHCI 1.1, 5.4.7 CONFIG

## `USBLEGSUP`

Offset `0x0A0` · access `r` · 32 bits · reset `0x401`

Extended capability: USB legacy support, next at +0x10 dwords (0xB0).

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem

## `SUPPORTED_USB2`

Offset `0x0B0` · access `r` · 32 bits · reset `0x2000802`

Supported protocol: USB 2.0, next at 0xD0.

Sources:

- measured (high): Raspberry Pi 4B d03115: id=2 'USB ' rev 2.0 portoff=1 count=1

## `SUPPORTED_USB2_NAME`

Offset `0x0B4` · access `r` · 32 bits · reset `0x20425355`

'USB '.

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem

## `SUPPORTED_USB2_PORTS`

Offset `0x0B8` · access `r` · 32 bits · reset `0x101`

Port offset 1, count 1.

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem

## `SUPPORTED_USB3`

Offset `0x0D0` · access `r` · 32 bits · reset `0x3008C02`

Supported protocol: USB 3.0, next at 0x300.

Sources:

- measured (high): Raspberry Pi 4B d03115: id=2 'USB ' rev 3.0 portoff=2 count=4

## `SUPPORTED_USB3_NAME`

Offset `0x0D4` · access `r` · 32 bits · reset `0x20425355`

'USB '.

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem

## `SUPPORTED_USB3_PORTS`

Offset `0x0D8` · access `r` · 32 bits · reset `0x402`

Port offset 2, count 4.

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem

## `DOORBELL`

Offset `0x100`, 33 elements 0x4 apart · access `rw` · 32 bits

Doorbell 0 is the command ring, doorbell n slot n; a write runs the ring to completion before it returns.

Sources:

- standard (high): xHCI 1.1, 5.6 Doorbell registers

## `MFINDEX`

Offset `0x200` · access `r` · 32 bits

Microframe index.

Sources:

- standard (high): xHCI 1.1, 5.5.1 MFINDEX

## `IMAN`

Offset `0x220`, 4 elements 0x20 apart · access `rw` · 32 bits

Interrupter management. Only interrupter 0 is used.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `IP` | w1c | Interrupt pending; only the controller sets it. |
| 1 | `IE` | rw | Interrupt enable. |

Sources:

- standard (high): xHCI 1.1, 5.5.2.1 IMAN

`IP` sources:

- standard (high): xHCI 1.1, 5.5.2.1

`IE` sources:

- standard (high): xHCI 1.1, 5.5.2.1

## `IMOD`

Offset `0x224`, 4 elements 0x20 apart · access `rw` · 32 bits

Interrupter moderation. Stored.

Sources:

- standard (high): xHCI 1.1, 5.5.2.2 IMOD

## `ERSTSZ`

Offset `0x228`, 4 elements 0x20 apart · access `rw` · 32 bits

Event ring segment table size.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.1 ERSTSZ

## `ERSTBA_LO`

Offset `0x230`, 4 elements 0x20 apart · access `rw` · 32 bits

Event ring segment table base.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.2 ERSTBA

## `ERSTBA_HI`

Offset `0x234`, 4 elements 0x20 apart · access `rw` · 32 bits

ERSTBA, high word.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.2 ERSTBA

## `ERDP_LO`

Offset `0x238`, 4 elements 0x20 apart · access `rw` · 32 bits

Event ring dequeue pointer.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 3 | `EHB` | w1c | Event handler busy; the driver clears it when done. |

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.3 ERDP

`EHB` sources:

- standard (high): xHCI 1.1, 5.5.2.3.3

## `ERDP_HI`

Offset `0x23C`, 4 elements 0x20 apart · access `rw` · 32 bits

ERDP, high word.

Sources:

- standard (high): xHCI 1.1, 5.5.2.3.3 ERDP

## `DEBUG_CAP`

Offset `0x300` · access `r` · 32 bits · reset `0xA`

Extended capability 10 (debug capability), the last in the list.

Sources:

- measured (high): Raspberry Pi 4B d03115 /dev/mem

## `PORTSC`

Offset `0x420`, 5 elements 0x10 apart · access `rw` · 32 bits

Port status and control. Empty but powered is 0x2A0; the USB2 port adds DR.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `CCS` | r | Device connected. |
| 1 | `PED` | rw | Port enabled; writing 1 disables it. |
| 4 | `PR` | rw | Port reset. |
| 8:5 | `PLS` | rw | Link state: 0 U0, 5 RxDetect, 7 Polling. |
| 9 | `PP` | rw | Port power. |
| 13:10 | `SPEED` | r | Port speed id. |
| 17 | `CSC` | w1c | Connect status change. |
| 18 | `PEC` | w1c | Port enabled change. |
| 19 | `WRC` | w1c | Warm reset change. |
| 20 | `OCC` | w1c | Over-current change. |
| 21 | `PRC` | w1c | Port reset change. |
| 22 | `PLC` | w1c | Link state change. |
| 23 | `CEC` | w1c | Config error change. |
| 30 | `DR` | r | Device removable; set on the USB2 root port. |
| 31 | `WPR` | rw | Warm port reset. |

Sources:

- standard (high): xHCI 1.1, 5.4.8 PORTSC
- measured (high): Raspberry Pi 4B d03115, moving a stick between sockets: 0x400202e1 USB2 just connected, 0x40000e03 USB2 enumerated, 0x00021203 USB3 SuperSpeed, 0x000002a0 empty

`CCS` sources:

- standard (high): xHCI 1.1, 5.4.8

`PED` sources:

- standard (high): xHCI 1.1, 5.4.8

`PR` sources:

- standard (high): xHCI 1.1, 5.4.8

`PLS` sources:

- standard (high): xHCI 1.1, 5.4.8

`PP` sources:

- standard (high): xHCI 1.1, 5.4.8

`SPEED` sources:

- standard (high): xHCI 1.1, 5.4.8

`CSC` sources:

- standard (high): xHCI 1.1, 5.4.8

`PEC` sources:

- standard (high): xHCI 1.1, 5.4.8

`WRC` sources:

- standard (high): xHCI 1.1, 5.4.8

`OCC` sources:

- standard (high): xHCI 1.1, 5.4.8

`PRC` sources:

- standard (high): xHCI 1.1, 5.4.8

`PLC` sources:

- standard (high): xHCI 1.1, 5.4.8

`CEC` sources:

- standard (high): xHCI 1.1, 5.4.8

`DR` sources:

- measured (high): top bit of the measured 0x400202e1

`WPR` sources:

- standard (high): xHCI 1.1, 5.4.8

## `PORTPMSC`

Offset `0x424`, 5 elements 0x10 apart · access `rw` · 32 bits

Port power management. Stored.

Sources:

- standard (high): xHCI 1.1, 5.4.9 PORTPMSC

## `PORTLI`

Offset `0x428`, 5 elements 0x10 apart · access `r` · 32 bits

Port link info.

Sources:

- standard (high): xHCI 1.1, 5.4.10 PORTLI

## `PORTHLPMC`

Offset `0x42C`, 5 elements 0x10 apart · access `rw` · 32 bits

Port hardware LPM control. Stored.

Sources:

- standard (high): xHCI 1.1, 5.4.11 PORTHLPMC
