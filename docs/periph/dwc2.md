<!-- generated from specs/dwc2.toml by `cargo run -- spec-docs --update` – do not edit -->

# `dwc2` – DesignWare USB 2.0 OTG controller (the USB-C port): the reset start4 runs when USB power comes on, and a host port with nothing plugged in

- Bus: `vpu` (VPU bus address)
- Base: `0x7E980000`
- Size: `0x10000`

GRSTCTL's reset and flush bits complete as soon as they are written and AHBIDLE always reads 1. The id and hardware-configuration words answer the measured values. The core is a host unless GUSBCFG forces device mode, a host channel asked to halt halts at once, and the root port reports nothing attached. Everything else is plain storage, and a core soft reset does not return it to its reset values.

Sources:

- linux (high): bcm283x.dtsi usb@7e980000 (brcm,bcm2835-usb, the dwc2 driver), reg size 0x10000; bcm2711.dtsi keeps the node
- decompile (high): SET_POWER_STATE USB handler 0x3ED89520..0x3ED89802, BCM2711 branch: core soft reset and FIFO flushes, then GUSBCFG with HCFG / HFIR, or DCFG / DCTL
- standard (high): edk2-platforms Platform/RaspberryPi/Drivers/DwUsbHostDxe (in the rpi-mkosi image's RPI_EFI.fd): DwCoreInit / DwHcInit / DwHcGetRootHubPortStatus

## Register map

| Offset | Name | Access | Width | Sources |
|---|---|---|---|---|
| `0x008` | [`GAHBCFG`](#gahbcfg) | rw | 32 | 2, best high |
| `0x00C` | [`GUSBCFG`](#gusbcfg) | rw | 32 | 3, best high |
| `0x010` | [`GRSTCTL`](#grstctl) | rw | 32 | 3, best high |
| `0x014` | [`GINTSTS`](#gintsts) | w1c | 32 | 2, best high |
| `0x040` | [`GSNPSID`](#gsnpsid) | r | 32 | 2, best high |
| `0x044` | [`GHWCFG1`](#ghwcfg1) | r | 32 | 1, best high |
| `0x048` | [`GHWCFG2`](#ghwcfg2) | r | 32 | 1, best high |
| `0x04C` | [`GHWCFG3`](#ghwcfg3) | r | 32 | 1, best high |
| `0x050` | [`GHWCFG4`](#ghwcfg4) | r | 32 | 1, best high |
| `0x400` | [`HCFG`](#hcfg) | rw | 32 | 2, best high |
| `0x404` | [`HFIR`](#hfir) | rw | 32 | 2, best high |
| `0x440` | [`HPRT0`](#hprt0) | rw | 32 | 2, best high |
| `0x500`–`0x5E0` (8 × 0x20) | [`HCCHAR`](#hcchar) | rw | 32 | 3, best high |
| `0x508`–`0x5E8` (8 × 0x20) | [`HCINT`](#hcint) | w1c | 32 | 2, best high |
| `0x800` | [`DCFG`](#dcfg) | rw | 32 | 2, best high |
| `0x804` | [`DCTL`](#dctl) | rw | 32 | 2, best high |
| `0x808` | [`DSTS`](#dsts) | r | 32 | 1, best high |

## `GAHBCFG`

Offset `0x008` · access `rw` · 32 bits

AHB configuration. Plain storage.

Sources:

- standard (high): DWC2 global register at 0x008; Linux drivers/usb/dwc2/hw.h GAHBCFG
- decompile (high): 0x3ED895D8 writes 1 on the device-mode path

## `GUSBCFG`

Offset `0x00C` · access `rw` · 32 bits

USB configuration. Stored; FORCEDEVMODE decides GINTSTS.CURMOD.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 30 | `FORCEDEVMODE` | rw | Force device mode. |

Sources:

- standard (high): DWC2 global register at 0x00C; Linux drivers/usb/dwc2/hw.h GUSBCFG
- decompile (high): 0x3ED895D6 writes 0x40402700 (device mode), 0x3ED897CC writes 0x20402700 (host mode)
- measured (high): Raspberry Pi 4B d03115 after vcmailbox SET_POWER_STATE(USB, on|wait): 0x20402700, the host-mode value start4 writes

`FORCEDEVMODE` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GUSBCFG_FORCEDEVMODE

## `GRSTCTL`

Offset `0x010` · access `rw` · 32 bits

Reset control. start4 writes one reset or flush bit at a time and spins until it clears, with no timeout.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `CSFTRST` | rw | Core soft reset. Completes at once in the model. |
| 1 | `HSFTRST` | rw | HCLK soft reset. Completes at once. |
| 2 | `FRMCNTRRST` | rw | Host frame counter reset. Completes at once. |
| 3 | `INTKNQFLSH` | rw | IN token queue flush. Completes at once. |
| 4 | `RXFFLSH` | rw | RX FIFO flush. Completes at once. |
| 5 | `TXFFLSH` | rw | TX FIFO flush. Completes at once. |
| 10:6 | `TXFNUM` | rw | Which TX FIFO TXFFLSH flushes; 0x10 is all of them. Stored. |
| 31 | `AHBIDLE` | r | AHB master idle. Always 1: nothing is ever in flight. |

Sources:

- standard (high): DWC2 global register at 0x010; Linux drivers/usb/dwc2/hw.h GRSTCTL
- decompile (high): 0x3ED8958A..0x3ED895B2: write 1, 2, 0x420 and 0x10 in turn, spinning on bits 0, 1, 5 and 4
- measured (high): Raspberry Pi 4B d03115, 32-bit /dev/mem read of 0xFE980010 after vcmailbox SET_POWER_STATE(USB, on|wait): 0x80000000

`CSFTRST` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GRSTCTL_CSFTRST

`HSFTRST` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GRSTCTL_HSFTRST

`FRMCNTRRST` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GRSTCTL_FRMCNTRRST

`INTKNQFLSH` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GRSTCTL_IN_TKNQ_FLSH

`RXFFLSH` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GRSTCTL_RXFFLSH

`TXFFLSH` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GRSTCTL_TXFFLSH

`TXFNUM` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GRSTCTL_TXFNUM_MASK

`AHBIDLE` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GRSTCTL_AHBIDLE; dwc2_core_reset and DwUsbHostDxe's DwCoreReset wait for it before a soft reset
- measured (high): Raspberry Pi 4B d03115: GRSTCTL 0x80000000 with USB powered

## `GINTSTS`

Offset `0x014` · access `w1c` · 32 bits

Core interrupt status. The levels are derived. Of the latched bits only the suspend pair is modelled: in device mode, connected, with no host on the port the bus is idle, so the core reports a suspend (#68). Nothing else on the port raises anything.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `CURMOD` | r | 1 in host mode. The ID pin reads as an A-device (GOTGCTL.CONIDSTS 0 on the reference board), so the core is a host unless GUSBCFG.FORCEDEVMODE is set. |
| 5 | `NPTXFEMP` | r | Non-periodic TX FIFO empty. Always 1: nothing is ever queued. |
| 10 | `ERLYSUSP` | w1c | Early suspend: 3 ms of idle bus in device mode. Set with USBSUSP when the core connects as a device with no host. |
| 11 | `USBSUSP` | w1c | USB suspend. The boot ROM's device-mode poll (0x60001bb0) takes it as 'no host', resets its USB state and gives up on rpiboot (#68). |
| 26 | `PTXFEMP` | r | Periodic TX FIFO empty. Always 1. |

Sources:

- standard (high): DWC2 global register at 0x014; Linux drivers/usb/dwc2/hw.h GINTSTS
- measured (high): Raspberry Pi 4B d03115 after vcmailbox SET_POWER_STATE(USB, on|wait): 0x5400002B (CURMOD, NPTXFEMP and PTXFEMP, plus latched MODEMIS, SOF, CONIDSTSCHNG and SESSREQINT)

`CURMOD` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GINTSTS_CURMODE_HOST; DwUsbHostDxe's DwHcInit powers the root port only when it is set
- measured (high): Raspberry Pi 4B d03115: 1 with GUSBCFG 0x20402700; GOTGCTL read 0x001C0000

`NPTXFEMP` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GINTSTS_NPTXFEMP

`ERLYSUSP` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GINTSTS_ERLYSUSP

`USBSUSP` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GINTSTS_USBSUSP
- decompile (medium): C0 boot ROM 0x60001bea: btest GINTSTS, 11

`PTXFEMP` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GINTSTS_PTXFEMP

## `GSNPSID`

Offset `0x040` · access `r` · 32 bits · reset `0x4F54280A`

Core id: OTG, release 2.80a. Linux's dwc2 refuses a core whose id lacks the 0x4F54 prefix.

Sources:

- measured (high): Raspberry Pi 4B d03115, 32-bit /dev/mem read of 0xFE980040 after vcmailbox SET_POWER_STATE(USB, on|wait): 0x4F54280A
- linux (high): drivers/usb/dwc2/params.c dwc2_get_hwparams checks GSNPSID against DWC2_OTG_ID (0x4F540000)

## `GHWCFG1`

Offset `0x044` · access `r` · 32 bits · reset `0x0`

Hardware configuration 1: endpoint directions.

Sources:

- measured (high): Raspberry Pi 4B d03115, /dev/mem read of 0xFE980044 with USB powered: 0x00000000

## `GHWCFG2`

Offset `0x048` · access `r` · 32 bits · reset `0x228DDD50`

Hardware configuration 2. DwUsbHostDxe sizes its channel loops from NUM_HOST_CHAN; 0 reads as one channel.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 17:14 | `NUM_HOST_CHAN` | r | Host channels minus one: 7, so eight channels. |

Sources:

- measured (high): Raspberry Pi 4B d03115, /dev/mem read of 0xFE980048 with USB powered: 0x228DDD50

`NUM_HOST_CHAN` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h GHWCFG2_NUM_HOST_CHAN_MASK; edk2 DwcHw.h DWC2_HWCFG2_NUM_HOST_CHAN_MASK

## `GHWCFG3`

Offset `0x04C` · access `r` · 32 bits · reset `0xFF000E8`

Hardware configuration 3: FIFO depth and transfer-size widths.

Sources:

- measured (high): Raspberry Pi 4B d03115, /dev/mem read of 0xFE98004C with USB powered: 0x0FF000E8

## `GHWCFG4`

Offset `0x050` · access `r` · 32 bits · reset `0x1FF00020`

Hardware configuration 4.

Sources:

- measured (high): Raspberry Pi 4B d03115, /dev/mem read of 0xFE980050 with USB powered: 0x1FF00020

## `HCFG`

Offset `0x400` · access `rw` · 32 bits

Host configuration. Plain storage.

Sources:

- standard (high): DWC2 host register at 0x400; Linux drivers/usb/dwc2/hw.h HCFG
- decompile (high): 0x3ED897DE writes 1 on the host-mode path

## `HFIR`

Offset `0x404` · access `rw` · 32 bits

Host frame interval. Plain storage.

Sources:

- standard (high): DWC2 host register at 0x404; Linux drivers/usb/dwc2/hw.h HFIR
- decompile (high): 0x3ED897E2 writes 48000 on the host-mode path

## `HPRT0`

Offset `0x440` · access `rw` · 32 bits

Root port control and status. Nothing is plugged in: the status and change bits read 0, the control bits are stored.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `PRTCONNSTS` | r | A device is attached. Always 0. |
| 1 | `PRTCONNDET` | w1c | Connect detected. Never set. |
| 2 | `PRTENA` | w1c | Port enabled. Only the core sets it, after a reset of an attached device; writing 1 disables the port. Always 0. |
| 3 | `PRTENCHNG` | w1c | Enable changed. Never set. |
| 4 | `PRTOVRCURRACT` | r | Over-current. Always 0. |
| 5 | `PRTOVRCURRCHNG` | w1c | Over-current changed. Never set. |
| 8 | `PRTRST` | rw | Port reset. Stored. |
| 11:10 | `PRTLNSTS` | r | Line state. 0 with nothing attached. |
| 12 | `PRTPWR` | rw | Port power. Stored. |
| 18:17 | `PRTSPD` | r | Speed of the attached device. 0 with nothing attached. |

Sources:

- standard (high): DWC2 host register at 0x440; Linux drivers/usb/dwc2/hw.h HPRT0; edk2 DwcHw.h DWC2_HPRT0_*
- measured (high): Raspberry Pi 4B d03115, /dev/mem read of 0xFE980440 with USB powered and nothing on the USB-C data lines: 0x00000000

`PRTCONNSTS` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HPRT0_CONNSTS

`PRTCONNDET` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HPRT0_CONNDET

`PRTENA` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HPRT0_ENA

`PRTENCHNG` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HPRT0_ENACHG

`PRTOVRCURRACT` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HPRT0_OVRCURRACT

`PRTOVRCURRCHNG` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HPRT0_OVRCURRCHG

`PRTRST` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HPRT0_RST

`PRTLNSTS` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HPRT0_LNSTS_MASK

`PRTPWR` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HPRT0_PWR

`PRTSPD` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HPRT0_SPD_MASK

## `HCCHAR`

Offset `0x500`, 8 elements 0x20 apart · access `rw` · 32 bits

Host channel characteristics, one per channel. Writing CHENA and CHDIS together halts the channel at once: both clear and HCINT.CHHLTD sets, since no channel ever has a transfer in flight. A transfer start (CHENA alone) is stored and goes nowhere.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 30 | `CHDIS` | rw | Channel disable request. |
| 31 | `CHENA` | rw | Channel enable. The core clears it when the channel halts. |

Sources:

- standard (high): DWC2 host channel registers at 0x500 + 0x20 * n; Linux drivers/usb/dwc2/hw.h HCCHAR; dwc2_core_host_init halts every channel and waits for CHENA to clear
- standard (high): DwUsbHostDxe DwHcInit: writes CHENA | CHDIS to each channel and waits up to DW_HC_RESET_TIMEOUT_MS (10 s) for CHENA to clear
- measured (high): Raspberry Pi 4B d03115, /dev/mem read of 0xFE980500 with USB powered: 0x00000000

`CHDIS` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HCCHAR_CHDIS; edk2 DwcHw.h DWC2_HCCHAR_CHDIS

`CHENA` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HCCHAR_CHENA; edk2 DwcHw.h DWC2_HCCHAR_CHEN

## `HCINT`

Offset `0x508`, 8 elements 0x20 apart · access `w1c` · 32 bits

Host channel interrupt status, one per channel.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 1 | `CHHLTD` | w1c | The channel halted. |

Sources:

- standard (high): DWC2 host channel registers at 0x508 + 0x20 * n; Linux drivers/usb/dwc2/hw.h HCINT
- measured (high): Raspberry Pi 4B d03115, /dev/mem read of 0xFE980508 with USB powered: 0x00000000

`CHHLTD` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h HCINTMSK_CHHLTD

## `DCFG`

Offset `0x800` · access `rw` · 32 bits

Device configuration. Plain storage.

Sources:

- standard (high): DWC2 device register at 0x800; Linux drivers/usb/dwc2/hw.h DCFG
- decompile (high): 0x3ED895C6 writes 0x00200200

## `DCTL`

Offset `0x804` · access `rw` · 32 bits

Device control. Storage, except that SFTDISCON decides whether a device-mode core is on the bus and the global NAK set/clear bits act on their status bits at once.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 2 | `GNPINNAKSTS` | r | Global non-periodic IN NAK in effect: set by SGNPINNAK, cleared by CGNPINNAK, at once. |
| 3 | `GOUTNAKSTS` | r | Global OUT NAK in effect: set by SGOUTNAK, cleared by CGOUTNAK, at once. The boot ROM spins on it after a suspend (0x60001bfc, #68). |
| 7 | `SGNPINNAK` | w | Set global non-periodic IN NAK. Write-only. |
| 8 | `CGNPINNAK` | w | Clear global non-periodic IN NAK. Write-only. |
| 9 | `SGOUTNAK` | w | Set global OUT NAK. Write-only. |
| 10 | `CGOUTNAK` | w | Clear global OUT NAK. Write-only. |
| 1 | `SFTDISCON` | rw | Soft disconnect. Reads 0 out of reset: the boot ROM never writes DCTL before it waits for a host, so a core that reset with it set could not be rpiboot'ed. |

Sources:

- standard (high): DWC2 device register at 0x804; Linux drivers/usb/dwc2/hw.h DCTL
- decompile (high): 0x3ED895C0..0x3ED895C4 set bit 1 (soft disconnect)

`GNPINNAKSTS` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h DCTL_GNPINNAKSTS

`GOUTNAKSTS` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h DCTL_GOUTNAKSTS

`SGNPINNAK` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h DCTL_SGNPINNAK

`CGNPINNAK` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h DCTL_CGNPINNAK

`SGOUTNAK` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h DCTL_SGOUTNAK

`CGOUTNAK` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h DCTL_CGOUTNAK

`SFTDISCON` sources:

- inferred (medium): C0 boot ROM: GUSBCFG 0x40402700 and DCFG, then straight into the GINTSTS poll

## `DSTS`

Offset `0x808` · access `r` · 32 bits

Device status. SUSPSTS follows the suspend a device-mode core reports with no host; the rest reads 0.

| Bits | Field | Access | Notes |
|---|---|---|---|
| 0 | `SUSPSTS` | r | The bus is suspended. |

Sources:

- standard (high): DWC2 device register at 0x808; Linux drivers/usb/dwc2/hw.h DSTS

`SUSPSTS` sources:

- standard (high): Linux drivers/usb/dwc2/hw.h DSTS_SUSPSTS
